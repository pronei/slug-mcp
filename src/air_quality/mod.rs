//! AirNow current AQI observations near a ZIP code.
//!
//! AirNow retired its `zipCode`/`latLong` observation services on 2026-10-01,
//! so the ZIP is geocoded via Open-Meteo and the bounding-box data service
//! (`/aq/data/?BBOX=…`) supplies per-monitor hourly AQI. Requires a free
//! `AIRNOW_API_KEY` (register at <https://docs.airnowapi.org/>). Graceful
//! degradation: if the key is absent, returns registration instructions
//! instead of erroring. Default ZIP is **95064** (UCSC).

use std::collections::HashMap;
use std::sync::Arc;

use crate::util::now_pacific;
use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cache::CacheStore;

const DEFAULT_ZIP: &str = "95064"; // UCSC
const DEFAULT_DISTANCE_MILES: u32 = 25;
const MAX_DISTANCE_MILES: u32 = 150;
const AIRNOW_DATA_URL: &str = "https://www.airnowapi.org/aq/data/";
const GEOCODE_URL: &str = "https://geocoding-api.open-meteo.com/v1/search";
// Monitors report on the hour but some lag by an hour or two; a window keeps
// a slow monitor in the answer instead of dropping it.
const LOOKBACK_HOURS: i64 = 3;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AirQualityRequest {
    /// US ZIP code. Defaults to 95064 (UCSC main campus).
    pub zip_code: Option<String>,
    /// Search radius in miles (default 25).
    pub distance_miles: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZipLocation {
    pub place: String,
    pub state: Option<String>,
    pub lat: f64,
    pub lon: f64,
    pub timezone: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AirReading {
    /// UTC hour of the observation, `YYYY-MM-DDTHH:MM`.
    pub utc: String,
    pub site_name: String,
    pub parameter_name: String,
    pub aqi: i32,
    pub category: String,
    pub distance_miles: f64,
}

pub struct AirQualityService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
    api_key: Option<String>,
}

impl AirQualityService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>, api_key: Option<String>) -> Self {
        Self {
            http,
            cache,
            api_key,
        }
    }

    pub async fn get_current(&self, zip: Option<&str>, distance: Option<u32>) -> Result<String> {
        let key = match &self.api_key {
            Some(k) if !k.is_empty() => k.clone(),
            _ => {
                return Ok("AirNow API key not configured.\n\
                     Get a free key at https://docs.airnowapi.org/ and set \
                     the `AIRNOW_API_KEY` environment variable."
                    .to_string());
            }
        };

        let zip = zip.unwrap_or(DEFAULT_ZIP).trim().to_string();
        // Reject anything but 5 digits before it reaches a query string.
        if zip.len() != 5 || !zip.chars().all(|c| c.is_ascii_digit()) {
            anyhow::bail!("invalid ZIP code '{}': expected 5 digits (e.g. 95064)", zip);
        }
        let distance = distance
            .unwrap_or(DEFAULT_DISTANCE_MILES)
            .clamp(1, MAX_DISTANCE_MILES);

        let http = self.http.clone();
        let zip_for_fetch = zip.clone();
        let location = self
            .cache
            .get_or_fetch::<Option<ZipLocation>, _, _>(
                &format!("air:zip:{zip}"),
                86_400,
                move || async move { geocode_zip(&http, &zip_for_fetch).await },
            )
            .await?;
        let Some(location) = location else {
            return Ok(format!(
                "Couldn't locate US ZIP code {zip}. Check the ZIP and try again.\n"
            ));
        };

        let cache_key = format!("air:current:{}:{}", zip, distance);
        let http = self.http.clone();
        let loc = location.clone();
        let readings = self
            .cache
            .get_or_fetch::<Vec<AirReading>, _, _>(&cache_key, 1800, move || async move {
                let body = fetch_airnow_bbox(&http, &key, &loc, distance).await?;
                let obs = parse_airnow_body(&body)?;
                Ok(nearest_per_parameter(&obs, &loc, f64::from(distance)))
            })
            .await?;

        Ok(format_readings(&zip, &location, distance, &readings))
    }
}

// ───── geocoding ─────

#[derive(Deserialize)]
struct GeocodeResponse {
    #[serde(default)]
    results: Vec<GeocodeResult>,
}

#[derive(Deserialize)]
struct GeocodeResult {
    name: String,
    latitude: f64,
    longitude: f64,
    admin1: Option<String>,
    timezone: Option<String>,
    #[serde(default)]
    postcodes: Vec<String>,
}

async fn geocode_zip(http: &reqwest::Client, zip: &str) -> Result<Option<ZipLocation>> {
    let resp = http
        .get(GEOCODE_URL)
        .query(&[
            ("name", zip),
            ("countryCode", "US"),
            ("count", "5"),
            ("format", "json"),
        ])
        .send()
        .await
        .context("ZIP geocoding request failed")?;
    if !resp.status().is_success() {
        anyhow::bail!("Open-Meteo geocoding returned HTTP {}", resp.status());
    }
    let body = resp.text().await.context("reading geocoding response")?;
    parse_geocode_body(&body, zip)
}

fn parse_geocode_body(body: &str, zip: &str) -> Result<Option<ZipLocation>> {
    let parsed: GeocodeResponse = serde_json::from_str(body).context("parsing geocoding JSON")?;
    let mut results = parsed.results;
    // A numeric search can also hit place names; prefer the one that lists the ZIP.
    let idx = results
        .iter()
        .position(|r| r.postcodes.iter().any(|p| p == zip))
        .unwrap_or(0);
    if results.is_empty() {
        return Ok(None);
    }
    let r = results.swap_remove(idx);
    Ok(Some(ZipLocation {
        place: r.name,
        state: r.admin1,
        lat: r.latitude,
        lon: r.longitude,
        timezone: r.timezone,
    }))
}

// ───── AirNow bounding-box data service ─────

/// `(min_lon, min_lat, max_lon, max_lat)` enclosing a circle of `miles` radius.
fn bounding_box(lat: f64, lon: f64, miles: f64) -> (f64, f64, f64, f64) {
    const MILES_PER_DEG_LAT: f64 = 69.0;
    let dlat = miles / MILES_PER_DEG_LAT;
    let dlon = miles / (MILES_PER_DEG_LAT * lat.to_radians().cos().max(0.01));
    (
        (lon - dlon).max(-180.0),
        (lat - dlat).max(-90.0),
        (lon + dlon).min(180.0),
        (lat + dlat).min(90.0),
    )
}

fn haversine_miles(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const EARTH_RADIUS_MILES: f64 = 3958.8;
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = (lat2 - lat1).to_radians();
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_MILES * a.sqrt().asin()
}

async fn fetch_airnow_bbox(
    http: &reqwest::Client,
    key: &str,
    loc: &ZipLocation,
    distance: u32,
) -> Result<String> {
    let (w, s, e, n) = bounding_box(loc.lat, loc.lon, f64::from(distance));
    let bbox = format!("{w:.4},{s:.4},{e:.4},{n:.4}");
    let now = chrono::Utc::now();
    let end = now.format("%Y-%m-%dT%H").to_string();
    let start = (now - chrono::Duration::hours(LOOKBACK_HOURS))
        .format("%Y-%m-%dT%H")
        .to_string();

    let resp = http
        .get(AIRNOW_DATA_URL)
        .query(&[
            ("startDate", start.as_str()),
            ("endDate", end.as_str()),
            ("parameters", "OZONE,PM25,PM10"),
            ("BBOX", bbox.as_str()),
            ("dataType", "A"),
            ("format", "application/json"),
            ("verbose", "1"),
            ("monitorType", "0"),
            ("includerawconcentrations", "0"),
            ("API_KEY", key),
        ])
        .send()
        .await
        .context("AirNow HTTP request failed")?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        anyhow::bail!("AirNow returned 401 — check AIRNOW_API_KEY");
    }
    if !resp.status().is_success() {
        anyhow::bail!("AirNow returned HTTP {}", resp.status());
    }
    resp.text().await.context("reading AirNow response")
}

#[derive(Debug, Clone, Deserialize)]
struct AirNowObs {
    #[serde(rename = "Latitude")]
    latitude: f64,
    #[serde(rename = "Longitude")]
    longitude: f64,
    #[serde(rename = "UTC")]
    utc: String,
    #[serde(rename = "Parameter")]
    parameter: String,
    #[serde(rename = "AQI")]
    aqi: i32,
    #[serde(rename = "Category")]
    category: Option<i32>,
    #[serde(rename = "SiteName", default)]
    site_name: Option<String>,
}

fn parse_airnow_body(body: &str) -> Result<Vec<AirNowObs>> {
    // AirNow reports key, parameter and quota problems with HTTP 200 and a
    // `{"WebServiceError":[{"Message":…}]}` object instead of the array.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body)
        && let Some(msg) = v
            .pointer("/WebServiceError/0/Message")
            .and_then(|m| m.as_str())
    {
        anyhow::bail!("AirNow error: {msg}");
    }
    serde_json::from_str(body).context("parsing AirNow JSON")
}

fn display_parameter(p: &str) -> &str {
    match p {
        "OZONE" => "O3",
        other => other,
    }
}

fn parameter_rank(p: &str) -> u8 {
    match p {
        "O3" => 0,
        "PM2.5" => 1,
        "PM10" => 2,
        _ => 3,
    }
}

fn category_name(category: Option<i32>, aqi: i32) -> String {
    let from_aqi = || match aqi {
        i if i < 0 => "Unknown",
        i if i <= 50 => "Good",
        i if i <= 100 => "Moderate",
        i if i <= 150 => "Unhealthy for Sensitive Groups",
        i if i <= 200 => "Unhealthy",
        i if i <= 300 => "Very Unhealthy",
        _ => "Hazardous",
    };
    match category {
        Some(1) => "Good",
        Some(2) => "Moderate",
        Some(3) => "Unhealthy for Sensitive Groups",
        Some(4) => "Unhealthy",
        Some(5) => "Very Unhealthy",
        Some(6) => "Hazardous",
        Some(7) => "Unavailable",
        _ => from_aqi(),
    }
    .to_string()
}

/// One reading per pollutant, keeping the shape of the retired ZIP service:
/// each monitor's latest valid hour, then the nearest monitor inside the
/// radius (the bounding box's corners lie outside it).
fn nearest_per_parameter(obs: &[AirNowObs], loc: &ZipLocation, radius: f64) -> Vec<AirReading> {
    let mut latest: HashMap<(String, String), AirReading> = HashMap::new();
    for o in obs {
        let distance_miles = haversine_miles(loc.lat, loc.lon, o.latitude, o.longitude);
        if distance_miles > radius {
            continue;
        }
        let site_name = o
            .site_name
            .clone()
            .unwrap_or_else(|| format!("monitor at {:.3}, {:.3}", o.latitude, o.longitude));
        let parameter_name = display_parameter(&o.parameter).to_string();
        let reading = AirReading {
            utc: o.utc.clone(),
            site_name: site_name.clone(),
            parameter_name: parameter_name.clone(),
            aqi: o.aqi,
            category: category_name(o.category, o.aqi),
            distance_miles,
        };
        let slot = latest.entry((site_name, parameter_name));
        match slot {
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(reading);
            }
            std::collections::hash_map::Entry::Occupied(mut cur) => {
                let c = cur.get();
                // A valid reading beats AirNow's -999 sentinel; then newer wins.
                if (reading.aqi >= 0, &reading.utc) > (c.aqi >= 0, &c.utc) {
                    cur.insert(reading);
                }
            }
        }
    }

    let mut best: HashMap<String, AirReading> = HashMap::new();
    for r in latest.into_values() {
        match best.get(&r.parameter_name) {
            Some(cur)
                if (cur.aqi < 0, cur.distance_miles, &cur.site_name)
                    <= (r.aqi < 0, r.distance_miles, &r.site_name) => {}
            _ => {
                best.insert(r.parameter_name.clone(), r);
            }
        }
    }
    let mut out: Vec<AirReading> = best.into_values().collect();
    out.sort_by(|a, b| {
        parameter_rank(&a.parameter_name)
            .cmp(&parameter_rank(&b.parameter_name))
            .then_with(|| a.parameter_name.cmp(&b.parameter_name))
    });
    out
}

// ───── formatting ─────

fn aqi_icon(aqi: i32) -> &'static str {
    match aqi {
        i if i <= 50 => "🟢",
        i if i <= 100 => "🟡",
        i if i <= 150 => "🟠",
        i if i <= 200 => "🔴",
        i if i <= 300 => "🟣",
        _ => "⚫",
    }
}

/// "YYYY-MM-DD at H:MM AM/PM ZZZ" in the ZIP's zone (UTC if unknown).
fn local_stamp(utc: &str, tz: Option<&str>) -> String {
    let Ok(naive) = chrono::NaiveDateTime::parse_from_str(utc, "%Y-%m-%dT%H:%M") else {
        return format!("{utc} UTC");
    };
    let at_utc = naive.and_utc();
    match tz.and_then(|t| t.parse::<chrono_tz::Tz>().ok()) {
        Some(zone) => at_utc
            .with_timezone(&zone)
            .format("%Y-%m-%d at %-I:%M %p %Z")
            .to_string(),
        None => at_utc.format("%Y-%m-%d at %-I:%M %p UTC").to_string(),
    }
}

fn place_label(loc: &ZipLocation) -> String {
    match &loc.state {
        Some(state) => format!("{}, {}", loc.place, state),
        None => loc.place.clone(),
    }
}

fn format_readings(zip: &str, loc: &ZipLocation, distance: u32, readings: &[AirReading]) -> String {
    if readings.is_empty() {
        return format!(
            "No AirNow monitors reported within {} mi of ZIP {} ({}) in the last few hours. \
             Station coverage may be sparse — try a larger `distance_miles`.\n",
            distance,
            zip,
            place_label(loc)
        );
    }

    let newest = readings
        .iter()
        .map(|r| r.utc.as_str())
        .max()
        .unwrap_or_default();
    let tz = loc.timezone.as_deref();
    let mut out = format!(
        "# Air quality — {} (ZIP {})\n\n\
         **Observed**: {}\n\n",
        place_label(loc),
        zip,
        local_stamp(newest, tz)
    );

    for r in readings {
        let site = format!("{}, {:.0} mi away", r.site_name, r.distance_miles);
        let stale = if r.utc != newest {
            format!(", as of {}", local_stamp(&r.utc, tz))
        } else {
            String::new()
        };
        // AirNow reports missing data as negative AQI (typically -999).
        if r.aqi < 0 {
            out.push_str(&format!(
                "- **{}**: no valid reading reported ({site}{stale})\n",
                r.parameter_name
            ));
            continue;
        }
        out.push_str(&format!(
            "- {} **{}**: AQI {} ({}) — {site}{stale}\n",
            aqi_icon(r.aqi),
            r.parameter_name,
            r.aqi,
            r.category
        ));
    }

    out.push_str(&format!(
        "\n_Source: EPA AirNow, nearest regulatory monitor within {} mi per pollutant. \
         Last updated: {}_\n",
        distance,
        now_pacific().format("%-I:%M %p")
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BBOX_FIXTURE: &str = include_str!("fixtures/airnow_bbox.json");
    const GEOCODE_FIXTURE: &str = include_str!("fixtures/geocode_95064.json");

    fn ucsc() -> ZipLocation {
        parse_geocode_body(GEOCODE_FIXTURE, "95064")
            .unwrap()
            .expect("fixture locates 95064")
    }

    fn obs(site: &str, param: &str, utc: &str, aqi: i32, lat: f64, lon: f64) -> AirNowObs {
        AirNowObs {
            latitude: lat,
            longitude: lon,
            utc: utc.to_string(),
            parameter: param.to_string(),
            aqi,
            category: None,
            site_name: Some(site.to_string()),
        }
    }

    #[test]
    fn aqi_icon_thresholds() {
        assert_eq!(aqi_icon(25), "🟢");
        assert_eq!(aqi_icon(75), "🟡");
        assert_eq!(aqi_icon(125), "🟠");
        assert_eq!(aqi_icon(175), "🔴");
        assert_eq!(aqi_icon(250), "🟣");
        assert_eq!(aqi_icon(400), "⚫");
    }

    #[test]
    fn geocode_fixture_locates_santa_cruz() {
        let loc = ucsc();
        assert_eq!(loc.place, "Santa Cruz");
        assert_eq!(loc.state.as_deref(), Some("California"));
        assert_eq!(loc.timezone.as_deref(), Some("America/Los_Angeles"));
        assert!((loc.lat - 36.97).abs() < 0.05, "{}", loc.lat);
    }

    #[test]
    fn geocode_prefers_result_listing_the_zip() {
        let body = r#"{"results":[
            {"name":"Wrongville","latitude":1.0,"longitude":2.0,"postcodes":["11111"]},
            {"name":"Rightville","latitude":3.0,"longitude":4.0,"admin1":"Montana",
             "timezone":"America/Denver","postcodes":["59718"]}
        ]}"#;
        let loc = parse_geocode_body(body, "59718").unwrap().unwrap();
        assert_eq!(loc.place, "Rightville");
    }

    #[test]
    fn geocode_without_results_is_none() {
        assert!(
            parse_geocode_body(r#"{"generationtime_ms":0.1}"#, "00601")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn bounding_box_widens_longitude_with_latitude() {
        let (w, s, e, n) = bounding_box(36.97, -122.03, 25.0);
        assert!((n - s - 50.0 / 69.0).abs() < 1e-9);
        // At ~37°N a degree of longitude is ~0.8 of a degree of latitude.
        assert!(e - w > n - s);
        assert!(w < -122.03 && e > -122.03);
    }

    #[test]
    fn haversine_known_distance() {
        // Santa Cruz → Salinas is about 30 miles as the crow flies.
        let d = haversine_miles(36.9741, -122.0308, 36.6943, -121.6233);
        assert!((28.0..31.0).contains(&d), "{d}");
    }

    #[test]
    fn live_bbox_fixture_picks_nearest_monitor_per_pollutant() {
        let loc = ucsc();
        let observations = parse_airnow_body(BBOX_FIXTURE).unwrap();
        assert_eq!(observations.len(), 27);
        let readings = nearest_per_parameter(&observations, &loc, 25.0);
        let names: Vec<&str> = readings.iter().map(|r| r.parameter_name.as_str()).collect();
        assert_eq!(names, ["O3", "PM2.5"]);
        assert!(readings.iter().all(|r| r.site_name == "Santa Cruz AMS"));
        assert!(readings.iter().all(|r| r.distance_miles < 5.0));
        assert!(readings.iter().all(|r| r.category == "Good"));

        let out = format_readings("95064", &loc, 25, &readings);
        assert!(
            out.contains("# Air quality — Santa Cruz, California (ZIP 95064)"),
            "{out}"
        );
        assert!(out.contains("**O3**: AQI"), "{out}");
        assert!(out.contains("Santa Cruz AMS, 2 mi away"), "{out}");
        assert!(out.contains("PST"), "{out}");
    }

    #[test]
    fn monitors_outside_the_radius_are_dropped() {
        let loc = ucsc();
        let observations = parse_airnow_body(BBOX_FIXTURE).unwrap();
        // Santa Cruz AMS sits ~2.5 mi from the geocoded point; nothing else is that close.
        let readings = nearest_per_parameter(&observations, &loc, 1.0);
        assert!(readings.is_empty(), "{readings:?}");
        let out = format_readings("95064", &loc, 1, &readings);
        assert!(
            out.contains("No AirNow monitors reported within 1 mi"),
            "{out}"
        );
    }

    #[test]
    fn latest_hour_per_monitor_wins() {
        let loc = ucsc();
        let o = vec![
            obs("A", "PM2.5", "2026-11-12T17:00", 10, 36.98, -122.0),
            obs("A", "PM2.5", "2026-11-12T19:00", 30, 36.98, -122.0),
            obs("A", "PM2.5", "2026-11-12T18:00", 20, 36.98, -122.0),
        ];
        let r = nearest_per_parameter(&o, &loc, 25.0);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].aqi, 30);
        assert_eq!(r[0].category, "Good");
    }

    #[test]
    fn valid_reading_beats_newer_sentinel_and_nearer_sentinel() {
        let loc = ucsc();
        let o = vec![
            obs("Near", "OZONE", "2026-11-12T19:00", -999, 36.975, -122.03),
            obs("Far", "OZONE", "2026-11-12T19:00", 40, 37.2, -122.0),
            obs("Far", "OZONE", "2026-11-12T20:00", -999, 37.2, -122.0),
        ];
        let r = nearest_per_parameter(&o, &loc, 25.0);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].site_name, "Far");
        assert_eq!(r[0].aqi, 40);
        assert_eq!(r[0].parameter_name, "O3");
    }

    // Missing readings carry -999 in Category as well as AQI (Hollister AMS
    // PM10, 2026-11-12); that row once failed the whole response.
    #[test]
    fn sentinel_category_parses() {
        let body = r#"[{"Latitude": 36.843425, "Longitude": -121.3621,
            "UTC": "2026-11-12T18:00", "Parameter": "PM10", "Unit": "UG/M3",
            "AQI": -999, "Category": -999, "SiteName": "Hollister AMS",
            "AgencyName": "Monterey Bay Unified APCD", "FullAQSCode": "060690002",
            "IntlAQSCode": "840060690002"}]"#;
        let obs = parse_airnow_body(body).unwrap();
        assert_eq!(obs[0].category, Some(-999));
        assert_eq!(category_name(obs[0].category, obs[0].aqi), "Unknown");
    }

    // AirNow uses -999 as a missing-data sentinel — it must not be presented
    // as a real (green, "Good") reading.
    #[test]
    fn negative_aqi_not_reported_as_good() {
        let loc = ucsc();
        let r = nearest_per_parameter(
            &[obs(
                "Only",
                "PM2.5",
                "2026-11-12T19:00",
                -999,
                36.98,
                -122.0,
            )],
            &loc,
            25.0,
        );
        let out = format_readings("95064", &loc, 25, &r);
        assert!(!out.contains("🟢"), "sentinel must not look Good: {}", out);
        assert!(!out.contains("-999"), "sentinel must not be shown: {}", out);
        assert!(out.contains("no valid reading"), "{out}");
    }

    #[test]
    fn lagging_monitor_is_marked_with_its_hour() {
        let loc = ucsc();
        let o = vec![
            obs("A", "OZONE", "2026-11-12T19:00", 30, 36.98, -122.0),
            obs("B", "PM2.5", "2026-11-12T17:00", 20, 36.98, -122.0),
        ];
        let out = format_readings("95064", &loc, 25, &nearest_per_parameter(&o, &loc, 25.0));
        assert!(
            out.contains("**Observed**: 2026-11-12 at 11:00 AM PST"),
            "{out}"
        );
        assert!(out.contains("as of 2026-11-12 at 9:00 AM PST"), "{out}");
    }

    #[test]
    fn category_numbers_and_aqi_fallback() {
        assert_eq!(
            category_name(Some(3), 120),
            "Unhealthy for Sensitive Groups"
        );
        assert_eq!(category_name(None, 75), "Moderate");
        assert_eq!(category_name(Some(9), 250), "Very Unhealthy");
    }

    #[test]
    fn summer_stamp_is_pdt_and_unknown_zone_is_utc() {
        assert_eq!(
            local_stamp("2026-07-07T19:00", Some("America/Los_Angeles")),
            "2026-07-07 at 12:00 PM PDT"
        );
        assert_eq!(
            local_stamp("2026-07-07T19:00", None),
            "2026-07-07 at 7:00 PM UTC"
        );
        assert_eq!(local_stamp("garbage", None), "garbage UTC");
    }

    #[test]
    fn zero_observations_parse_ok() {
        assert!(parse_airnow_body("[]").unwrap().is_empty());
    }

    #[test]
    fn truncated_json_is_err_not_panic() {
        let err = parse_airnow_body(&BBOX_FIXTURE[..BBOX_FIXTURE.len() / 2]).unwrap_err();
        assert!(format!("{:#}", err).contains("parsing AirNow"));
    }

    // AirNow reports auth/param problems as a JSON object, not an array.
    #[test]
    fn error_object_instead_of_array_is_err() {
        let body = r#"{"WebServiceError":[{"Message":"Invalid API key"}]}"#;
        let err = parse_airnow_body(body).unwrap_err();
        assert_eq!(err.to_string(), "AirNow error: Invalid API key");
    }
}
