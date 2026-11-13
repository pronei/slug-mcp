//! Santa Cruz BCycle dock availability from the public GBFS feed (no key).
//!
//! UCSC ended its BCycle contract, so the campus docks are being removed by
//! the end of 2026 while the city system continues. No station is listed by
//! hand: campus views key off the "UCSC" name prefix, and once the feed stops
//! carrying those stations they fall back to the nearest city docks.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, TimeZone};
use chrono_tz::Tz;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};

use crate::cache::CacheStore;
use crate::util::{FuzzyMatcher, InvalidInput, haversine_miles, now_pacific, reject_empty_catalog};

// GBFS 2.1. The unversioned discovery URL still serves 1.1, which encodes the
// station flags as 0/1; `flag` accepts both, so either version parses.
const INFO_URL: &str = "https://gbfs.bcycle.com/2/bcycle_santacruz/station_information.json";
const STATUS_URL: &str = "https://gbfs.bcycle.com/2/bcycle_santacruz/station_status.json";
const INFO_TTL_SECS: u64 = 86_400;
// The feed declares `ttl: 0`, but CloudFront serves it with max-age=60, so
// polling faster only returns the same snapshot.
const STATUS_TTL_SECS: u64 = 60;
const DEFAULT_NEAR_LIMIT: usize = 10;
const MAX_LIMIT: usize = 100;
const SUGGESTION_COUNT: usize = 3;
const STALE_FEED_MINUTES: i64 = 15;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BikeShareRequest {
    /// Part of a station name, case-insensitive; every word must appear (e.g. "Science & Engineering Library", "Quarry Plaza", "Pacific & Cooper", "Capitola"). If no station name matches, the text is treated as a place and the nearest stations are listed instead.
    pub station: Option<String>,
    /// List stations by straight-line distance from a place: a campus or Santa Cruz landmark ("Science Hill", "Quarry Plaza", "McHenry", "main entrance", "downtown", "Metro Center", "Boardwalk", "Wharf", "Seabright", "Capitola"), a station name, or "lat,lon" (e.g. "36.9741,-122.0308").
    pub near: Option<String>,
    /// Only UCSC campus stations (names starting "UCSC"). Defaults to true when neither `station` nor `near` is given. UCSC ended its BCycle contract and the campus docks are being removed by the end of 2026; once they are gone, the nearest remaining city stations are listed instead.
    pub campus_only: Option<bool>,
    /// "bike" to list only stations with a bike ready to rent, or "dock" to list only stations with an open dock to return one.
    pub need: Option<String>,
    /// Maximum number of stations to list (default 10 when sorting by distance, otherwise every match; max 100).
    pub limit: Option<u32>,
}

pub struct BikeShareService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl BikeShareService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    pub async fn get_bike_share(&self, req: &BikeShareRequest) -> Result<String> {
        let query = Query::from_request(req)?;
        let http = &self.http;
        let (info, status) = tokio::join!(
            self.cache
                .get_or_fetch("bike_share:stations", INFO_TTL_SECS, || async {
                    parse_info(&fetch(http, INFO_URL).await?)
                }),
            self.cache
                .get_or_fetch("bike_share:status", STATUS_TTL_SECS, || async {
                    parse_status(&fetch(http, STATUS_URL).await?)
                }),
        );
        let (info, status) = match (info, status) {
            (Ok(info), Ok(status)) => (info, status),
            (Err(e), _) | (_, Err(e)) => {
                tracing::warn!("BCycle feed unavailable: {e:#}");
                return Ok(format!(
                    "Santa Cruz BCycle's live station feed is unavailable right now ({e}). \
                     Try again in a minute, or check the BCycle app."
                ));
            }
        };
        render(
            &join(&info, &status.stations),
            status.last_updated,
            &query,
            now_pacific(),
        )
    }
}

// ───── feed ─────

#[derive(Debug, Clone, Deserialize)]
struct StationInfo {
    station_id: String,
    name: String,
    lat: f64,
    lon: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct StationStatus {
    station_id: String,
    num_bikes_available: u32,
    num_docks_available: u32,
    #[serde(deserialize_with = "flag")]
    is_installed: bool,
    #[serde(deserialize_with = "flag")]
    is_renting: bool,
    #[serde(deserialize_with = "flag")]
    is_returning: bool,
    // BCycle extension, not GBFS; the 2.1 spec's `vehicle_types_available` is absent.
    #[serde(default)]
    num_bikes_available_types: Option<BikeTypes>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct BikeTypes {
    #[serde(default)]
    classic: u32,
    #[serde(default)]
    smart: u32,
    #[serde(default)]
    electric: u32,
}

#[derive(Debug, Clone)]
struct StatusSnapshot {
    last_updated: i64,
    stations: Vec<StationStatus>,
}

#[derive(Deserialize)]
struct Feed<T> {
    last_updated: i64,
    data: FeedData<T>,
}

#[derive(Deserialize)]
struct FeedData<T> {
    stations: Vec<T>,
}

fn flag<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Flag {
        Bool(bool),
        Int(i64),
    }
    Ok(match Flag::deserialize(d)? {
        Flag::Bool(b) => b,
        Flag::Int(n) => n != 0,
    })
}

async fn fetch(http: &reqwest::Client, url: &str) -> Result<String> {
    let resp = http
        .get(url)
        .send()
        .await
        .context("BCycle request failed")?;
    if !resp.status().is_success() {
        anyhow::bail!("BCycle returned HTTP {}", resp.status());
    }
    resp.text().await.context("reading BCycle response")
}

fn parse_feed<T: DeserializeOwned>(body: &str, feed: &str) -> Result<(i64, Vec<T>)> {
    let value: serde_json::Value =
        serde_json::from_str(body).with_context(|| format!("parsing BCycle {feed} JSON"))?;
    // BCycle answers unknown systems with `{"error":404,"error_description":"404 Not Found"}`.
    if let Some(err) = value.get("error") {
        let detail = value
            .get("error_description")
            .and_then(|d| d.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| err.to_string());
        anyhow::bail!("BCycle {feed} feed error: {detail}");
    }
    let parsed: Feed<T> =
        serde_json::from_value(value).with_context(|| format!("parsing BCycle {feed} JSON"))?;
    Ok((parsed.last_updated, parsed.data.stations))
}

fn parse_info(body: &str) -> Result<Vec<StationInfo>> {
    let (_, stations) = parse_feed(body, "station_information")?;
    reject_empty_catalog(stations, "BCycle station_information")
}

fn parse_status(body: &str) -> Result<StatusSnapshot> {
    let (last_updated, stations) = parse_feed(body, "station_status")?;
    Ok(StatusSnapshot {
        last_updated,
        stations,
    })
}

// ───── stations ─────

#[derive(Debug, Clone)]
struct Station {
    name: String,
    lat: f64,
    lon: f64,
    status: Option<StationStatus>,
}

impl Station {
    fn is_campus(&self) -> bool {
        self.name.to_ascii_lowercase().starts_with("ucsc")
    }

    fn rentable(&self) -> bool {
        self.status
            .as_ref()
            .is_some_and(|s| s.is_installed && s.is_renting && s.num_bikes_available > 0)
    }

    fn returnable(&self) -> bool {
        self.status
            .as_ref()
            .is_some_and(|s| s.is_installed && s.is_returning && s.num_docks_available > 0)
    }

    fn meets(&self, need: Option<Need>) -> bool {
        match need {
            None => true,
            Some(Need::Bike) => self.rentable(),
            Some(Need::Dock) => self.returnable(),
        }
    }

    fn miles_to(&self, p: &Place) -> f64 {
        haversine_miles(p.lat, p.lon, self.lat, self.lon)
    }
}

/// Status entries without station information are dropped: there is no name
/// or location to show for them.
fn join(info: &[StationInfo], status: &[StationStatus]) -> Vec<Station> {
    let by_id: HashMap<&str, &StationStatus> =
        status.iter().map(|s| (s.station_id.as_str(), s)).collect();
    info.iter()
        .map(|i| Station {
            name: i.name.trim().to_string(),
            lat: i.lat,
            lon: i.lon,
            status: by_id.get(i.station_id.as_str()).map(|&s| s.clone()),
        })
        .collect()
}

/// Lowercase, `&` spelled "and", apostrophes dropped, other punctuation
/// spaced out. The feed itself mixes "Bay & King" with "Bay and West Cliff".
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str(" and "),
            '\'' | '\u{2019}' => {}
            c if c.is_alphanumeric() => out.extend(c.to_lowercase()),
            _ => out.push(' '),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn name_matches(query_norm: &str, name: &str) -> bool {
    let name = normalize(name);
    let mut words = query_norm.split(' ').filter(|w| !w.is_empty()).peekable();
    words.peek().is_some() && words.all(|w| name.contains(w))
}

// ───── places ─────

#[derive(Debug, Clone)]
struct Place {
    label: String,
    lat: f64,
    lon: f64,
    campus: bool,
}

struct Landmark {
    name: &'static str,
    lat: f64,
    lon: f64,
    campus: bool,
    /// Normalized; matched as whole words against the query, longest wins.
    aliases: &'static [&'static str],
}

impl Landmark {
    fn place(&self) -> Place {
        Place {
            label: self.name.to_string(),
            lat: self.lat,
            lon: self.lon,
            campus: self.campus,
        }
    }
}

const CAMPUS_CORE: Landmark = Landmark {
    name: "UCSC campus core (Quarry Plaza)",
    lat: 36.9973,
    lon: -122.0556,
    campus: true,
    aliases: &["ucsc", "uc santa cruz", "campus", "quarry plaza", "quarry"],
};

// Fixed coordinates, so campus places still resolve after their docks leave
// the feed.
const LANDMARKS: &[Landmark] = &[
    CAMPUS_CORE,
    Landmark {
        name: "UCSC main entrance (Bay & High)",
        lat: 36.9771,
        lon: -122.0534,
        campus: true,
        aliases: &[
            "main entrance",
            "campus entrance",
            "base of campus",
            "lower campus",
            "bay and high",
        ],
    },
    Landmark {
        name: "Science Hill",
        lat: 36.9997,
        lon: -122.0613,
        campus: true,
        aliases: &["science hill", "baskin", "physical sciences"],
    },
    Landmark {
        name: "Science & Engineering Library",
        lat: 36.9989,
        lon: -122.0603,
        campus: true,
        aliases: &[
            "science and engineering library",
            "s and e library",
            "science library",
            "sci lib",
        ],
    },
    Landmark {
        name: "McHenry Library",
        lat: 36.9957,
        lon: -122.0592,
        campus: true,
        aliases: &["mchenry"],
    },
    Landmark {
        name: "East Field House",
        lat: 36.9951,
        lon: -122.0540,
        campus: true,
        aliases: &["east field", "field house", "opers", "east gym"],
    },
    Landmark {
        name: "Cowell & Stevenson colleges",
        lat: 36.9973,
        lon: -122.0532,
        campus: true,
        aliases: &["cowell", "stevenson"],
    },
    Landmark {
        name: "Crown & Merrill colleges",
        lat: 36.9999,
        lon: -122.0538,
        campus: true,
        aliases: &["crown", "merrill"],
    },
    Landmark {
        name: "College Nine & John R. Lewis College",
        lat: 37.0017,
        lon: -122.0578,
        campus: true,
        aliases: &[
            "college nine",
            "college 9",
            "college ten",
            "college 10",
            "john r lewis",
            "john lewis",
        ],
    },
    Landmark {
        name: "Porter College",
        lat: 36.9948,
        lon: -122.0655,
        campus: true,
        aliases: &["porter"],
    },
    Landmark {
        name: "Kresge College",
        lat: 36.9983,
        lon: -122.0666,
        campus: true,
        aliases: &["kresge"],
    },
    Landmark {
        name: "Oakes & Rachel Carson colleges",
        lat: 36.9911,
        lon: -122.0640,
        campus: true,
        aliases: &[
            "oakes",
            "rachel carson",
            "carson",
            "college eight",
            "college 8",
        ],
    },
    Landmark {
        name: "UCSC Coastal Science Campus",
        lat: 36.9512,
        lon: -122.0652,
        campus: true,
        aliases: &[
            "coastal science",
            "coastal campus",
            "seymour",
            "marine discovery",
            "long marine",
        ],
    },
    Landmark {
        name: "Westside Research Park",
        lat: 36.9570,
        lon: -122.0584,
        campus: true,
        aliases: &["westside research park"],
    },
    Landmark {
        name: "downtown Santa Cruz (Pacific Ave)",
        lat: 36.9747,
        lon: -122.0262,
        campus: false,
        aliases: &["downtown", "pacific avenue", "pacific ave"],
    },
    Landmark {
        name: "Santa Cruz Metro Center",
        lat: 36.9714,
        lon: -122.0256,
        campus: false,
        aliases: &[
            "metro center",
            "pacific station",
            "bus station",
            "transit center",
        ],
    },
    Landmark {
        name: "Santa Cruz Beach Boardwalk",
        lat: 36.9644,
        lon: -122.0177,
        campus: false,
        aliases: &["boardwalk", "main beach"],
    },
    Landmark {
        name: "Santa Cruz Wharf",
        lat: 36.9605,
        lon: -122.0210,
        campus: false,
        aliases: &["wharf"],
    },
    Landmark {
        name: "West Cliff lighthouse",
        lat: 36.9514,
        lon: -122.0268,
        campus: false,
        aliases: &["lighthouse", "west cliff", "steamer lane"],
    },
    Landmark {
        name: "Natural Bridges",
        lat: 36.9512,
        lon: -122.0573,
        campus: false,
        aliases: &["natural bridges"],
    },
    Landmark {
        name: "Seabright",
        lat: 36.9665,
        lon: -122.0085,
        campus: false,
        aliases: &["seabright"],
    },
    Landmark {
        name: "Santa Cruz Harbor",
        lat: 36.9660,
        lon: -122.0015,
        campus: false,
        aliases: &["harbor", "harbour"],
    },
    Landmark {
        name: "Capitola Village",
        lat: 36.9722,
        lon: -121.9520,
        campus: false,
        aliases: &["capitola"],
    },
];

const PLACE_EXAMPLES: &str = "Science Hill, Quarry Plaza, McHenry, main entrance, downtown, \
                              Metro Center, Boardwalk, Wharf, Seabright, Capitola";

fn find_landmark(query_norm: &str) -> Option<&'static Landmark> {
    LANDMARKS
        .iter()
        .flat_map(|l| l.aliases.iter().map(move |a| (l, *a)))
        .filter(|(_, alias)| {
            FuzzyMatcher::new([*alias])
                .word_boundary()
                .matches(query_norm)
        })
        .max_by_key(|(_, alias)| alias.len())
        .map(|(l, _)| l)
}

fn parse_lat_lon(s: &str) -> Option<(f64, f64)> {
    let parts: Vec<&str> = s
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .collect();
    let [lat, lon] = parts.as_slice() else {
        return None;
    };
    let (lat, lon) = (lat.parse::<f64>().ok()?, lon.parse::<f64>().ok()?);
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
}

/// A coordinate pair or a known landmark.
fn resolve_landmark(text: &str) -> Option<Place> {
    if let Some((lat, lon)) = parse_lat_lon(text) {
        return Some(Place {
            label: format!("{lat:.4}, {lon:.4}"),
            lat,
            lon,
            campus: false,
        });
    }
    find_landmark(&normalize(text)).map(Landmark::place)
}

/// A landmark, else the centre of the stations whose names match.
fn resolve_place(text: &str, stations: &[Station]) -> Option<Place> {
    resolve_landmark(text).or_else(|| {
        let norm = normalize(text);
        let hits: Vec<&Station> = stations
            .iter()
            .filter(|s| name_matches(&norm, &s.name))
            .collect();
        let label = match hits.as_slice() {
            [only] => only.name.clone(),
            _ => format!("\"{text}\""),
        };
        centroid(&hits, label)
    })
}

fn centroid(stations: &[&Station], label: String) -> Option<Place> {
    if stations.is_empty() {
        return None;
    }
    let n = stations.len() as f64;
    Some(Place {
        label,
        lat: stations.iter().map(|s| s.lat).sum::<f64>() / n,
        lon: stations.iter().map(|s| s.lon).sum::<f64>() / n,
        campus: stations.iter().all(|s| s.is_campus()),
    })
}

// ───── query ─────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Need {
    Bike,
    Dock,
}

fn parse_need(s: Option<&str>) -> Result<Option<Need>> {
    let Some(s) = s.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    match s.to_ascii_lowercase().as_str() {
        "bike" | "bikes" | "e-bike" | "e-bikes" | "ebike" | "ebikes" | "rent" => {
            Ok(Some(Need::Bike))
        }
        "dock" | "docks" | "open dock" | "return" => Ok(Some(Need::Dock)),
        _ => Err(InvalidInput::err(format!(
            "need must be \"bike\" (a bike to rent) or \"dock\" (an open dock to return one); got \"{s}\""
        ))),
    }
}

#[derive(Debug, Default)]
struct Query<'a> {
    station: Option<&'a str>,
    near: Option<&'a str>,
    campus_only: Option<bool>,
    need: Option<Need>,
    limit: Option<usize>,
}

impl<'a> Query<'a> {
    fn from_request(req: &'a BikeShareRequest) -> Result<Self> {
        let text = |s: &'a Option<String>| s.as_deref().map(str::trim).filter(|s| !s.is_empty());
        Ok(Self {
            station: text(&req.station),
            near: text(&req.near),
            campus_only: req.campus_only,
            need: parse_need(req.need.as_deref())?,
            limit: req.limit.map(|n| n as usize),
        })
    }
}

// ───── formatting ─────

const CAMPUS_LEAVING: &str = "UCSC ended its BCycle contract; the campus docks are being removed \
                              by the end of 2026. City stations stay.";
const CAMPUS_GONE: &str = "UCSC's campus docks were removed at the end of 2026 when its BCycle \
                           contract ended; the city system continues.";

fn render(stations: &[Station], feed_updated: i64, q: &Query, now: DateTime<Tz>) -> Result<String> {
    let campus_left = stations.iter().any(Station::is_campus);
    let mut origin = match q.near {
        Some(text) => Some(resolve_place(text, stations).ok_or_else(|| {
            InvalidInput::err(format!(
                "Couldn't place \"{text}\". Use a landmark ({PLACE_EXAMPLES}), \
                 a station name, or \"lat,lon\" (e.g. \"36.9741,-122.0308\")."
            ))
        })?),
        None => None,
    };
    let campus_only = q
        .campus_only
        .unwrap_or(q.station.is_none() && q.near.is_none());
    if campus_only && !campus_left {
        origin.get_or_insert_with(|| CAMPUS_CORE.place());
    }
    let campus_scope = campus_only && campus_left;
    let pool: Vec<&Station> = stations
        .iter()
        .filter(|s| !campus_scope || s.is_campus())
        .collect();

    let mut notes: Vec<String> = Vec::new();
    let mut matched = false;
    let mut listed = match q.station {
        Some(text) => {
            let norm = normalize(text);
            let hits: Vec<&Station> = pool
                .iter()
                .copied()
                .filter(|s| name_matches(&norm, &s.name))
                .collect();
            if !hits.is_empty() {
                matched = true;
                hits
            } else {
                let Some(place) = resolve_landmark(text) else {
                    return Ok(no_match(text, campus_left));
                };
                let from = origin.get_or_insert(place);
                notes.push(format!(
                    "No station name contains \"{text}\", so these are the stations nearest {}.",
                    from.label
                ));
                pool
            }
        }
        None => pool,
    };

    let title = match (q.station, &origin) {
        (Some(text), _) if matched => format!("stations matching \"{text}\""),
        (_, Some(o)) => format!("stations nearest {}", o.label),
        _ if campus_scope => "UCSC campus stations".to_string(),
        _ => "all stations".to_string(),
    };

    match &origin {
        Some(o) => listed.sort_by(|a, b| a.miles_to(o).total_cmp(&b.miles_to(o))),
        None => listed.sort_by(|a, b| a.name.cmp(&b.name)),
    }
    let eligible: Vec<&Station> = listed.iter().copied().filter(|s| s.meets(q.need)).collect();
    let limit = q
        .limit
        .unwrap_or(if origin.is_some() {
            DEFAULT_NEAR_LIMIT
        } else {
            MAX_LIMIT
        })
        .clamp(1, MAX_LIMIT);
    let shown = &eligible[..eligible.len().min(limit)];

    let wants_campus = campus_only || origin.as_ref().is_some_and(|o| o.campus);
    if !campus_left && wants_campus {
        notes.insert(0, CAMPUS_GONE.to_string());
    } else if campus_left && shown.iter().any(|s| s.is_campus()) {
        notes.insert(0, CAMPUS_LEAVING.to_string());
    }
    let age_min = (now.timestamp() - feed_updated) / 60;
    if age_min > STALE_FEED_MINUTES {
        notes.insert(
            0,
            format!(
                "⚠️ BCycle's feed hasn't updated since {} ({age_min} min ago); counts may be out of date.",
                pacific_stamp(feed_updated)
            ),
        );
    }

    let mut out = format!("# Santa Cruz BCycle: {title}\n\n");
    for note in &notes {
        out.push_str(&format!("> {note}\n\n"));
    }
    if shown.is_empty() {
        out.push_str(&match q.need {
            Some(need) if !listed.is_empty() => format!(
                "None of these {} stations has {} right now.\n",
                listed.len(),
                need_phrase(need)
            ),
            _ => "No stations to show.\n".to_string(),
        });
    }
    for s in shown {
        out.push_str(&station_line(s, origin.as_ref()));
    }
    if eligible.len() > shown.len() {
        out.push_str(&format!(
            "- …and {} more (raise `limit` to see them)\n",
            eligible.len() - shown.len()
        ));
    }

    if matched {
        let from = origin.clone().or_else(|| centroid(&listed, String::new()));
        if let Some(from) = from {
            if q.need != Some(Need::Dock) && !listed.iter().any(|s| s.rentable()) {
                out.push_str("\n**No bikes to rent there right now.** Nearest with a bike:\n");
                for s in nearest(stations, &listed, &from, Station::rentable) {
                    out.push_str(&station_line(s, Some(&from)));
                }
            }
            if q.need != Some(Need::Bike) && !listed.iter().any(|s| s.returnable()) {
                out.push_str("\n**No open docks there right now.** Nearest with an open dock:\n");
                for s in nearest(stations, &listed, &from, Station::returnable) {
                    out.push_str(&station_line(s, Some(&from)));
                }
            }
        }
    }

    let live = stations.iter().filter(|s| s.status.is_some()).count();
    let ready: u32 = stations
        .iter()
        .filter(|s| s.rentable())
        .filter_map(|s| s.status.as_ref().map(|st| st.num_bikes_available))
        .sum();
    out.push_str(&format!(
        "\n_Live from Santa Cruz BCycle, updated {}. System-wide: {live} stations, {ready} bikes \
         ready to rent. Unlock and return through the BCycle app; plans and pricing at \
         santacruz.bcycle.com._\n",
        pacific_stamp(feed_updated)
    ));
    Ok(out)
}

fn nearest<'a>(
    stations: &'a [Station],
    exclude: &[&Station],
    from: &Place,
    keep: fn(&Station) -> bool,
) -> Vec<&'a Station> {
    let mut hits: Vec<&Station> = stations
        .iter()
        .filter(|s| keep(s) && !exclude.iter().any(|e| std::ptr::eq(*e, *s)))
        .collect();
    hits.sort_by(|a, b| a.miles_to(from).total_cmp(&b.miles_to(from)));
    hits.truncate(SUGGESTION_COUNT);
    hits
}

fn no_match(text: &str, campus_left: bool) -> String {
    let mut out = format!(
        "No Santa Cruz BCycle station name contains \"{text}\". Try one distinctive word from \
         the name (e.g. \"Cooper\", \"Capitola\", \"Library\"), or pass `near` with a place to \
         list the closest stations."
    );
    if !campus_left {
        out.push_str(&format!(" {CAMPUS_GONE}"));
    }
    out.push('\n');
    out
}

fn need_phrase(need: Need) -> &'static str {
    match need {
        Need::Bike => "a bike to rent",
        Need::Dock => "an open dock",
    }
}

fn station_line(s: &Station, from: Option<&Place>) -> String {
    let dist = from
        .map(|p| format!(" ({:.1} mi)", s.miles_to(p)))
        .unwrap_or_default();
    let detail = match &s.status {
        None => "no live status (the station may have been removed)".to_string(),
        Some(st) if !st.is_installed => "⛔ offline (not installed)".to_string(),
        Some(st) => {
            let counts = format!(
                "{}, {}",
                bikes_phrase(st),
                count(st.num_docks_available, "open dock")
            );
            match (st.is_renting, st.is_returning) {
                (true, true) => counts,
                (false, false) => format!("⛔ not renting or accepting returns ({counts})"),
                (false, true) => format!("⚠️ not renting, returns OK ({counts})"),
                (true, false) => format!("⚠️ not accepting returns ({counts})"),
            }
        }
    };
    format!("- **{}**{dist}: {detail}\n", s.name)
}

fn bikes_phrase(st: &StationStatus) -> String {
    match st.num_bikes_available_types {
        Some(t) if t.classic + t.smart + t.electric == st.num_bikes_available => {
            let electric = count(t.electric, "e-bike");
            match t.classic + t.smart {
                0 => electric,
                pedal => format!("{electric} + {}", count(pedal, "pedal bike")),
            }
        }
        _ => count(st.num_bikes_available, "bike"),
    }
}

fn count(n: u32, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

fn pacific_stamp(epoch: i64) -> String {
    match chrono::Utc.timestamp_opt(epoch, 0).single() {
        Some(t) if epoch > 0 => t
            .with_timezone(&chrono_tz::US::Pacific)
            .format("%-I:%M %p %Z on %a %b %-d")
            .to_string(),
        _ => "at an unknown time".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO_FIXTURE: &str = include_str!("fixtures/station_information.json");
    const STATUS_FIXTURE: &str = include_str!("fixtures/station_status.json");
    // `last_updated` of the status fixture: 2026-11-13 3:39 AM PST.
    const FEED_TIME: i64 = 1_794_569_946;

    fn fixture() -> Vec<Station> {
        let info = parse_info(INFO_FIXTURE).unwrap();
        let status = parse_status(STATUS_FIXTURE).unwrap();
        join(&info, &status.stations)
    }

    fn city_only() -> Vec<Station> {
        fixture().into_iter().filter(|s| !s.is_campus()).collect()
    }

    fn at(epoch: i64) -> DateTime<Tz> {
        chrono::Utc
            .timestamp_opt(epoch, 0)
            .unwrap()
            .with_timezone(&chrono_tz::US::Pacific)
    }

    fn run(stations: &[Station], q: Query) -> String {
        render(stations, FEED_TIME, &q, at(FEED_TIME + 60)).unwrap()
    }

    fn station_lines(out: &str) -> Vec<&str> {
        out.lines().filter(|l| l.starts_with("- **")).collect()
    }

    fn line_for<'a>(out: &'a str, name: &str) -> &'a str {
        out.lines()
            .find(|l| l.starts_with(&format!("- **{name}**")))
            .unwrap_or_else(|| panic!("no line for {name} in:\n{out}"))
    }

    fn miles(line: &str) -> f64 {
        let start = line.find("** (").expect("distance") + 4;
        let end = start + line[start..].find(" mi)").unwrap();
        line[start..end].parse().unwrap()
    }

    fn status_of<'a>(stations: &'a mut [Station], name: &str) -> &'a mut StationStatus {
        stations
            .iter_mut()
            .find(|s| s.name == name)
            .and_then(|s| s.status.as_mut())
            .unwrap()
    }

    #[test]
    fn info_fixture_parses() {
        let info = parse_info(INFO_FIXTURE).unwrap();
        assert_eq!(info.len(), 18);
        let sel = info
            .iter()
            .find(|s| s.name == "UCSC - Science & Engineering Library")
            .unwrap();
        assert_eq!(sel.station_id, "bcycle_santacruz_7434");
        assert!((sel.lat - 36.9989).abs() < 1e-3 && (sel.lon + 122.0603).abs() < 1e-3);
        assert_eq!(
            info.iter().filter(|s| s.name.starts_with("UCSC")).count(),
            8
        );
    }

    #[test]
    fn status_fixture_parses_counts_types_and_flags() {
        let status = parse_status(STATUS_FIXTURE).unwrap();
        assert_eq!(status.last_updated, FEED_TIME);
        assert_eq!(status.stations.len(), 18);
        let sel = &status.stations[0];
        assert_eq!(sel.station_id, "bcycle_santacruz_7434");
        assert_eq!((sel.num_bikes_available, sel.num_docks_available), (18, 0));
        assert_eq!(sel.num_bikes_available_types.unwrap().electric, 18);
        let city_hall = status
            .stations
            .iter()
            .find(|s| s.station_id == "bcycle_santacruz_7485")
            .unwrap();
        assert!(city_hall.is_installed && !city_hall.is_renting && !city_hall.is_returning);
    }

    // The unversioned discovery URL serves GBFS 1.1, which uses 0/1 for flags.
    #[test]
    fn gbfs_1_1_integer_flags_parse() {
        let body = r#"{"last_updated":1794569917,"ttl":0,"version":"1.1","data":{"stations":[
            {"station_id":"bcycle_santacruz_7485","num_bikes_available":0,"num_docks_available":1,
             "is_installed":1,"is_renting":0,"is_returning":0,"last_reported":1794569917,
             "num_bikes_available_types":{"classic":0,"smart":0,"electric":0}}]}}"#;
        let s = &parse_status(body).unwrap().stations[0];
        assert!(s.is_installed && !s.is_renting && !s.is_returning);
    }

    #[test]
    fn join_keeps_info_without_status_and_drops_status_without_info() {
        let info = parse_info(INFO_FIXTURE).unwrap();
        let mut status = parse_status(STATUS_FIXTURE).unwrap().stations;
        status.retain(|s| s.station_id != "bcycle_santacruz_7462"); // Hahn
        let mut orphan = status[0].clone();
        orphan.station_id = "bcycle_santacruz_99999".to_string();
        status.push(orphan);

        let stations = join(&info, &status);
        assert_eq!(stations.len(), info.len());
        let hahn = stations.iter().find(|s| s.name == "UCSC - Hahn").unwrap();
        assert!(hahn.status.is_none());

        let out = run(&stations, Query::default());
        assert!(
            line_for(&out, "UCSC - Hahn").contains("no live status"),
            "{out}"
        );
        assert!(out.contains("System-wide: 17 stations"), "{out}");
    }

    #[test]
    fn name_matching_ignores_case_ampersands_and_apostrophes() {
        let m = |q: &str, name: &str| name_matches(&normalize(q), name);
        assert!(m(
            "science & engineering library",
            "UCSC - Science & Engineering Library"
        ));
        assert!(m("S&E Library", "UCSC - Science & Engineering Library"));
        assert!(!m("S&E Library", "UCSC - McHenry Library South"));
        assert!(m("pacific & second", "Santa Cruz - Pacific and Second"));
        assert!(m("days market", "Santa Cruz - Day's Market"));
        assert!(m("college 9", "UCSC - College 9-John R. Lewis College"));
        assert!(m(
            "lewis college 9",
            "UCSC - College 9-John R. Lewis College"
        ));
        assert!(!m("college 10", "UCSC - College 9-John R. Lewis College"));
        assert!(!m(" - ", "UCSC - Hahn"));
    }

    #[test]
    fn station_query_lists_every_match() {
        let out = run(
            &fixture(),
            Query {
                station: Some("pacific"),
                ..Default::default()
            },
        );
        assert!(out.starts_with("# Santa Cruz BCycle: stations matching \"pacific\""));
        let names: Vec<&str> = station_lines(&out);
        assert_eq!(names.len(), 2, "{out}");
        assert!(names[0].contains("Pacific & Cooper"));
        assert!(names[1].contains("Pacific and Second"));
    }

    #[test]
    fn default_lists_campus_stations_alphabetically_with_the_removal_notice() {
        let out = run(&fixture(), Query::default());
        assert!(
            out.starts_with("# Santa Cruz BCycle: UCSC campus stations"),
            "{out}"
        );
        assert!(out.contains(CAMPUS_LEAVING), "{out}");
        let lines = station_lines(&out);
        assert_eq!(lines.len(), 8, "{out}");
        assert!(lines.iter().all(|l| l.starts_with("- **UCSC - ")));
        assert!(lines[0].starts_with("- **UCSC - Baskin Engineering**"));
        assert!(
            lines[7].starts_with(
                "- **UCSC - Science & Engineering Library**: 18 e-bikes, 0 open docks"
            )
        );
        assert!(
            out.contains("updated 3:39 AM PST on Fri Nov 13. System-wide: 18 stations, 90 bikes ready to rent."),
            "{out}"
        );
        assert!(!out.contains("hasn't updated"), "{out}");
    }

    #[test]
    fn near_downtown_sorts_by_distance_and_applies_the_default_limit() {
        let out = run(
            &fixture(),
            Query {
                near: Some("downtown"),
                ..Default::default()
            },
        );
        assert!(
            out.starts_with(
                "# Santa Cruz BCycle: stations nearest downtown Santa Cruz (Pacific Ave)"
            ),
            "{out}"
        );
        let lines = station_lines(&out);
        assert_eq!(lines.len(), DEFAULT_NEAR_LIMIT, "{out}");
        assert!(
            lines[0].starts_with("- **Santa Cruz - Pacific & Cooper** (0.0 mi)"),
            "{out}"
        );
        let d: Vec<f64> = lines.iter().map(|l| miles(l)).collect();
        assert!(d.windows(2).all(|w| w[0] <= w[1]), "{d:?}");
        assert!(out.contains("- …and 8 more"), "{out}");
        assert!(
            lines[..8]
                .iter()
                .all(|l| l.starts_with("- **Santa Cruz - ")),
            "city docks come before campus: {out}"
        );
    }

    #[test]
    fn near_accepts_lat_lon_station_names_and_an_explicit_limit() {
        let stations = fixture();
        let out = run(
            &stations,
            Query {
                near: Some("36.9989, -122.0603"),
                limit: Some(3),
                ..Default::default()
            },
        );
        let lines = station_lines(&out);
        assert_eq!(lines.len(), 3, "{out}");
        assert!(lines[0].contains("Science & Engineering Library"), "{out}");
        assert!(out.contains("- …and 15 more"), "{out}");

        let out = run(
            &stations,
            Query {
                near: Some("Depot Park"),
                limit: Some(1),
                ..Default::default()
            },
        );
        assert!(
            out.contains("stations nearest Santa Cruz - Depot Park"),
            "{out}"
        );
        assert!(
            station_lines(&out)[0].contains("Depot Park** (0.0 mi)"),
            "{out}"
        );
    }

    #[test]
    fn landmarks_match_whole_words_and_prefer_the_longest_alias() {
        let name = |q: &str| find_landmark(&normalize(q)).map(|l| l.name);
        assert_eq!(
            name("downtown Santa Cruz"),
            Some("downtown Santa Cruz (Pacific Ave)")
        );
        assert_eq!(name("UCSC East Field House"), Some("East Field House"));
        assert_eq!(name("UCSC"), Some("UCSC campus core (Quarry Plaza)"));
        assert_eq!(name("S&E Library"), Some("Science & Engineering Library"));
        assert_eq!(name("Natural Bridges State Beach"), Some("Natural Bridges"));
        assert_eq!(name("harborview"), None);
        assert_eq!(name("Hahn"), None);
    }

    #[test]
    fn lat_lon_parsing_is_strict() {
        assert_eq!(parse_lat_lon("36.97,-122.03"), Some((36.97, -122.03)));
        assert_eq!(parse_lat_lon("36.97 -122.03"), Some((36.97, -122.03)));
        assert_eq!(parse_lat_lon("136.97,-122.03"), None);
        assert_eq!(parse_lat_lon("36.97"), None);
        assert_eq!(parse_lat_lon("NaN,1"), None);
        assert_eq!(parse_lat_lon("Bay & High"), None);
    }

    #[test]
    fn unknown_near_is_a_caller_error() {
        let err = render(
            &fixture(),
            FEED_TIME,
            &Query {
                near: Some("Timbuktu"),
                ..Default::default()
            },
            at(FEED_TIME),
        )
        .unwrap_err();
        assert!(err.downcast_ref::<InvalidInput>().is_some(), "{err}");
        assert!(err.to_string().contains("Science Hill"), "{err}");
    }

    #[test]
    fn bad_need_is_a_caller_error() {
        let err = parse_need(Some("scooter")).unwrap_err();
        assert!(err.downcast_ref::<InvalidInput>().is_some(), "{err}");
        assert_eq!(parse_need(Some(" Bike ")).unwrap(), Some(Need::Bike));
        assert_eq!(parse_need(Some("return")).unwrap(), Some(Need::Dock));
        assert_eq!(parse_need(Some("")).unwrap(), None);
    }

    #[test]
    fn offline_stations_are_flagged() {
        let mut stations = fixture();
        status_of(&mut stations, "Santa Cruz - Front & River").is_installed = false;
        status_of(&mut stations, "Santa Cruz - Depot Park").is_renting = false;
        status_of(&mut stations, "Santa Cruz - Bay & High").is_returning = false;
        let out = run(
            &stations,
            Query {
                campus_only: Some(false),
                ..Default::default()
            },
        );
        assert!(
            line_for(&out, "Santa Cruz - City Hall")
                .contains("⛔ not renting or accepting returns (0 e-bikes, 1 open dock)"),
            "{out}"
        );
        assert!(
            line_for(&out, "Santa Cruz - Front & River").contains("⛔ offline (not installed)")
        );
        assert!(line_for(&out, "Santa Cruz - Depot Park").contains("⚠️ not renting, returns OK"));
        assert!(line_for(&out, "Santa Cruz - Bay & High").contains("⚠️ not accepting returns"));
        assert!(
            line_for(&out, "UCSC - Quarry Plaza").ends_with(": 5 e-bikes, 10 open docks"),
            "{out}"
        );
    }

    #[test]
    fn need_filters_out_stations_that_cannot_serve() {
        let stations = fixture();
        let bikes = run(
            &stations,
            Query {
                near: Some("downtown"),
                need: Some(Need::Bike),
                limit: Some(100),
                ..Default::default()
            },
        );
        for gone in [
            "Santa Cruz - City Hall",
            "Pacific and Second",
            "Boardwalk Ticket Booth",
            "UCSC - Hahn",
        ] {
            assert!(!bikes.contains(gone), "{gone} listed: {bikes}");
        }
        assert_eq!(station_lines(&bikes).len(), 14, "{bikes}");

        let docks = run(
            &stations,
            Query {
                near: Some("downtown"),
                need: Some(Need::Dock),
                limit: Some(100),
                ..Default::default()
            },
        );
        assert!(!docks.contains("Science & Engineering Library"), "{docks}");
        assert!(!docks.contains("Pacific and Second"), "{docks}");
        assert!(docks.contains("Boardwalk Ticket Booth"), "{docks}");
    }

    #[test]
    fn empty_station_suggests_the_nearest_with_a_bike() {
        let out = run(
            &fixture(),
            Query {
                station: Some("hahn"),
                ..Default::default()
            },
        );
        assert!(line_for(&out, "UCSC - Hahn").contains("0 e-bikes, 2 open docks"));
        let (_, suggestions) = out
            .split_once("**No bikes to rent there right now.**")
            .expect(&out);
        let lines = station_lines(suggestions);
        assert_eq!(lines.len(), SUGGESTION_COUNT, "{out}");
        assert!(lines.iter().all(|l| !l.contains("Hahn")));
        assert!(lines.iter().all(|l| miles(l) < 0.5), "{out}");
        assert!(!out.contains("No open docks"), "{out}");
    }

    #[test]
    fn full_station_suggests_the_nearest_open_dock() {
        let out = run(
            &fixture(),
            Query {
                station: Some("Science & Engineering Library"),
                ..Default::default()
            },
        );
        assert!(!out.contains("No bikes to rent"), "{out}");
        let (_, suggestions) = out
            .split_once("**No open docks there right now.**")
            .expect(&out);
        assert!(
            station_lines(suggestions)[0]
                .starts_with("- **UCSC - Perk Coffee-Physical Sciences** (0.1 mi)"),
            "{out}"
        );
    }

    #[test]
    fn need_with_no_eligible_match_says_so_and_suggests() {
        let out = run(
            &fixture(),
            Query {
                station: Some("Pacific and Second"),
                need: Some(Need::Bike),
                ..Default::default()
            },
        );
        assert!(
            out.contains("None of these 1 stations has a bike to rent right now."),
            "{out}"
        );
        assert!(
            out.contains("**No bikes to rent there right now.**"),
            "{out}"
        );
        assert!(!out.contains("No open docks"), "{out}");
    }

    #[test]
    fn unmatched_station_name_that_is_a_place_lists_the_nearest() {
        let out = run(
            &fixture(),
            Query {
                station: Some("Boardwalk entrance"),
                ..Default::default()
            },
        );
        assert!(
            out.contains("No station name contains \"Boardwalk entrance\", so these are the stations nearest Santa Cruz Beach Boardwalk."),
            "{out}"
        );
        assert!(
            station_lines(&out)[0].contains("Boardwalk Ticket Booth"),
            "{out}"
        );
    }

    #[test]
    fn unmatched_station_name_is_plain_text_while_campus_docks_remain() {
        let out = run(
            &fixture(),
            Query {
                station: Some("zzz"),
                ..Default::default()
            },
        );
        assert!(out.starts_with("No Santa Cruz BCycle station name contains \"zzz\""));
        assert!(!out.contains("removed"), "{out}");
    }

    #[test]
    fn no_campus_stations_left_points_to_the_nearest_city_docks() {
        let stations = city_only();
        for q in [
            Query::default(),
            Query {
                campus_only: Some(true),
                ..Default::default()
            },
        ] {
            let out = run(&stations, q);
            assert!(
                out.starts_with(
                    "# Santa Cruz BCycle: stations nearest UCSC campus core (Quarry Plaza)"
                ),
                "{out}"
            );
            assert!(out.contains(CAMPUS_GONE), "{out}");
            assert!(!out.contains(CAMPUS_LEAVING), "{out}");
            assert!(
                station_lines(&out)[0].starts_with("- **Santa Cruz - Bay & High** (1.4 mi)"),
                "{out}"
            );
        }
    }

    #[test]
    fn removed_campus_station_name_falls_back_to_its_landmark() {
        let out = run(
            &city_only(),
            Query {
                station: Some("UCSC - Science & Engineering Library"),
                ..Default::default()
            },
        );
        assert!(out.contains(CAMPUS_GONE), "{out}");
        assert!(
            out.contains("stations nearest Science & Engineering Library"),
            "{out}"
        );
        assert!(station_lines(&out)[0].contains("Bay & High"), "{out}");

        let out = run(
            &city_only(),
            Query {
                near: Some("Science Hill"),
                ..Default::default()
            },
        );
        assert!(out.contains(CAMPUS_GONE), "{out}");

        let out = run(
            &city_only(),
            Query {
                station: Some("Hahn"),
                ..Default::default()
            },
        );
        assert!(out.starts_with("No Santa Cruz BCycle station name contains \"Hahn\""));
        assert!(out.contains(CAMPUS_GONE), "{out}");
    }

    #[test]
    fn city_queries_do_not_mention_the_campus_once_it_is_gone() {
        let out = run(
            &city_only(),
            Query {
                near: Some("downtown"),
                ..Default::default()
            },
        );
        assert!(!out.contains("UCSC"), "{out}");
    }

    #[test]
    fn stale_feed_is_flagged_in_pacific_time() {
        let out = render(
            &fixture(),
            FEED_TIME,
            &Query::default(),
            at(FEED_TIME + 3600),
        )
        .unwrap();
        assert!(
            out.contains(
                "> ⚠️ BCycle's feed hasn't updated since 3:39 AM PST on Fri Nov 13 (60 min ago)"
            ),
            "{out}"
        );
    }

    #[test]
    fn bike_counts_fall_back_when_types_disagree() {
        let mut stations = fixture();
        let st = status_of(&mut stations, "UCSC - Quarry Plaza");
        st.num_bikes_available_types = Some(BikeTypes {
            classic: 2,
            smart: 0,
            electric: 3,
        });
        assert_eq!(bikes_phrase(st), "3 e-bikes + 2 pedal bikes");
        st.num_bikes_available_types = None;
        assert_eq!(bikes_phrase(st), "5 bikes");
        st.num_bikes_available_types = Some(BikeTypes {
            classic: 0,
            smart: 0,
            electric: 1,
        });
        assert_eq!(bikes_phrase(st), "5 bikes");
    }

    #[test]
    fn truncated_json_is_err_not_panic() {
        let err = parse_status(&STATUS_FIXTURE[..STATUS_FIXTURE.len() / 2]).unwrap_err();
        assert!(
            format!("{err:#}").contains("parsing BCycle station_status JSON"),
            "{err:#}"
        );
        let err = parse_info("<html>502 Bad Gateway</html>").unwrap_err();
        assert!(format!("{err:#}").contains("parsing BCycle station_information JSON"));
    }

    #[test]
    fn wrong_shape_is_err() {
        let err = parse_status(r#"{"last_updated":1,"data":{"stations":[{"station_id":"x"}]}}"#)
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("parsing BCycle station_status JSON"),
            "{err:#}"
        );
    }

    // What gbfs.bcycle.com returns for an unknown system (with HTTP 404).
    #[test]
    fn gbfs_error_body_is_err() {
        let err = parse_status(r#"{"error":404,"error_description":"404 Not Found"}"#).unwrap_err();
        assert_eq!(
            err.to_string(),
            "BCycle station_status feed error: 404 Not Found"
        );
    }

    #[test]
    fn empty_station_information_is_err() {
        let body =
            r#"{"last_updated":1794566461,"ttl":3600,"version":"2.1","data":{"stations":[]}}"#;
        assert!(parse_info(body).is_err());
    }
}
