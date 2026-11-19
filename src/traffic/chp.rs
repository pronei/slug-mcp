//! CHP CAD incident feed.
//!
//! Pulls the statewide CHP communications center log XML at
//! <https://media.chp.ca.gov/sa_xml/sa.xml>. The feed is an undocumented but
//! long-stable dump of all active CHP dispatch logs. We filter to
//! `Center ID = "GGHB"` > `Dispatch ID = "MYCC"` (Monterey Comm Center, which
//! covers both Monterey and Santa Cruz counties) and then post-filter by
//! `Area` field to keep Santa Cruz County entries.
//!
//! Note: CHP XML text nodes are LITERALLY wrapped in `"..."` quote characters,
//! e.g. `<LogTime>"Apr 10 2026  7:56AM"</LogTime>`. We strip those at the
//! accessor level.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use regex::Regex;
use serde::{Deserialize, Serialize};

use super::caltrans::route_number;
use crate::util::{FuzzyMatcher, truncate};

pub const CHP_FEED_URL: &str = "https://media.chp.ca.gov/sa_xml/sa.xml";
pub const CENTER_GGHB: &str = "GGHB";
pub const DISPATCH_MYCC: &str = "MYCC";

/// Santa Cruz County areas that appear as the `<Area>` field in MYCC logs.
/// Case-insensitive substring match.
pub const SC_COUNTY_AREAS: &[&str] = &[
    "Santa Cruz",
    "Scotts Valley",
    "Capitola",
    "Watsonville",
    "Soquel",
    "Aptos",
    "Ben Lomond",
    "Felton",
    "Boulder Creek",
    "Corralitos",
    "Freedom",
];

// ───── quick-xml deserialization types ─────

#[derive(Debug, Deserialize, Serialize)]
pub struct StateFeed {
    #[serde(rename = "Center", default)]
    pub centers: Vec<Center>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Center {
    #[serde(rename = "@ID")]
    pub id: String,
    #[serde(rename = "Dispatch", default)]
    pub dispatches: Vec<Dispatch>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Dispatch {
    #[serde(rename = "@ID")]
    pub id: String,
    #[serde(rename = "Log", default)]
    pub logs: Vec<Log>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Log {
    #[serde(rename = "@ID")]
    pub id: String,
    #[serde(rename = "LogTime", default)]
    pub log_time: String,
    #[serde(rename = "LogType", default)]
    pub log_type: String,
    #[serde(rename = "Location", default)]
    pub location: String,
    #[serde(rename = "LocationDesc", default)]
    pub location_desc: String,
    #[serde(rename = "Area", default)]
    pub area: String,
    #[serde(rename = "LATLON", default)]
    pub latlon: String,
}

// ───── public API ─────

/// A CHP incident normalized for display — with stripped quote characters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Incident {
    pub id: String,
    pub log_time: String,
    pub log_type: String,
    pub location: String,
    pub location_desc: String,
    pub area: String,
    pub latlon: String,
}

impl From<Log> for Incident {
    fn from(log: Log) -> Self {
        Incident {
            id: log.id,
            log_time: strip_quotes(&log.log_time),
            log_type: strip_quotes(&log.log_type),
            location: strip_quotes(&log.location),
            location_desc: strip_quotes(&log.location_desc),
            area: strip_quotes(&log.area),
            latlon: strip_quotes(&log.latlon),
        }
    }
}

/// What CHP answered with, kept so a bad copy can be described and aged.
#[derive(Debug, Clone)]
pub struct FeedMeta {
    pub status: u16,
    pub content_type: Option<String>,
    pub last_modified: Option<DateTime<Utc>>,
    /// The response's `Date` header.
    pub served_at: Option<DateTime<Utc>>,
}

/// Santa Cruz County incidents from one good copy of the feed.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub incidents: Vec<Incident>,
    pub as_of: DateTime<Tz>,
}

/// A 2xx answer that isn't a usable copy of the feed.
#[derive(Debug)]
pub struct BadCopy(pub String);

impl std::fmt::Display for BadCopy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BadCopy {}

// A live copy is rewritten every few seconds. On 2026-11-19 one CHP server kept
// handing out a file ten hours old, so a copy older than this isn't current.
const MAX_FEED_AGE_MINUTES: i64 = 15;

impl FeedMeta {
    fn describe(&self, body: &str) -> String {
        let mut out = format!(
            "HTTP {} {}, {} bytes",
            self.status,
            self.content_type.as_deref().unwrap_or("(no content type)"),
            body.len()
        );
        if let Some(modified) = self.last_modified {
            let pacific = modified.with_timezone(&chrono_tz::US::Pacific);
            out.push_str(&format!(
                ", last modified {}",
                pacific.format("%b %-d %-I:%M %p %Z")
            ));
        }
        out
    }
}

fn parse_http_date(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc2822(value.trim())
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn head_snippet(body: &str) -> String {
    let start: String = body.chars().take(400).collect();
    truncate(&collapse_ws(&start), 120)
}

fn tail_snippet(body: &str) -> String {
    let mut end: Vec<char> = body.chars().rev().take(400).collect();
    end.reverse();
    let collapsed: Vec<char> = collapse_ws(&end.into_iter().collect::<String>())
        .chars()
        .collect();
    match collapsed.len().checked_sub(80) {
        Some(skip) if skip > 0 => format!("...{}", collapsed[skip..].iter().collect::<String>()),
        _ => collapsed.into_iter().collect(),
    }
}

/// Fetch + parse the full CHP feed, return Santa Cruz County incidents only.
pub async fn fetch_sc_incidents(http: &reqwest::Client) -> Result<Snapshot> {
    let resp = http
        .get(CHP_FEED_URL)
        .send()
        .await
        .context("GET CHP sa.xml")?;
    if !resp.status().is_success() {
        anyhow::bail!("CHP feed returned HTTP {}", resp.status());
    }
    let header = |name| resp.headers().get(name).and_then(|v| v.to_str().ok());
    let meta = FeedMeta {
        status: resp.status().as_u16(),
        content_type: header(reqwest::header::CONTENT_TYPE).map(str::to_string),
        last_modified: header(reqwest::header::LAST_MODIFIED).and_then(parse_http_date),
        served_at: header(reqwest::header::DATE).and_then(parse_http_date),
    };
    let body = resp.text().await.context("reading CHP body")?;
    Ok(check_feed(&meta, &body, Utc::now())?)
}

/// Turn one answer from CHP into incidents, or say what was wrong with it.
pub fn check_feed(
    meta: &FeedMeta,
    body: &str,
    fetched_at: DateTime<Utc>,
) -> Result<Snapshot, BadCopy> {
    let what = meta.describe(body);
    let trimmed = body.trim();
    let looks_like_feed = trimmed
        .chars()
        .take(300)
        .collect::<String>()
        .contains("<State");
    let not_the_feed = || {
        BadCopy(format!(
            "not the incident feed: {what}, starts \"{}\"",
            head_snippet(trimmed)
        ))
    };

    let incidents = match parse_sc_incidents(body) {
        Ok(incidents) => incidents,
        Err(_) if trimmed.is_empty() => return Err(BadCopy(format!("empty body: {what}"))),
        Err(_) if !looks_like_feed => return Err(not_the_feed()),
        Err(_) if !trimmed.ends_with("</State>") => {
            return Err(BadCopy(format!(
                "cut-off copy: {what}, ends \"{}\"",
                tail_snippet(trimmed)
            )));
        }
        Err(e) => {
            return Err(BadCopy(format!(
                "XML error ({}): {what}, starts \"{}\", ends \"{}\"",
                e.root_cause(),
                head_snippet(trimmed),
                tail_snippet(trimmed)
            )));
        }
    };

    // quick-xml doesn't check the root element's name, so a well-formed page
    // that isn't the feed parses to zero incidents.
    if !body.contains("<Center") {
        return Err(if looks_like_feed {
            BadCopy(format!(
                "no dispatch centers: {what}, starts \"{}\"",
                head_snippet(trimmed)
            ))
        } else {
            not_the_feed()
        });
    }

    // Both times are CHP's own, so this host's clock doesn't come into it.
    if let (Some(modified), Some(served)) = (meta.last_modified, meta.served_at) {
        let age = served - modified;
        if age > chrono::Duration::minutes(MAX_FEED_AGE_MINUTES) {
            let age = match (age.num_hours(), age.num_minutes() % 60) {
                (0, minutes) => format!("{minutes} min"),
                (hours, minutes) => format!("{hours} h {minutes} min"),
            };
            return Err(BadCopy(format!(
                "stale copy: {what}, {age} before the response"
            )));
        }
    }

    Ok(Snapshot {
        incidents,
        as_of: meta
            .last_modified
            .unwrap_or(fetched_at)
            .with_timezone(&chrono_tz::US::Pacific),
    })
}

/// Parse a CHP XML body and return only Santa Cruz County incidents.
pub fn parse_sc_incidents(body: &str) -> Result<Vec<Incident>> {
    let feed: StateFeed = quick_xml::de::from_str(body).context("parsing CHP XML")?;

    let mut out = Vec::new();
    for center in feed.centers {
        if center.id.trim() != CENTER_GGHB {
            continue;
        }
        for dispatch in center.dispatches {
            if dispatch.id.trim() != DISPATCH_MYCC {
                continue;
            }
            for log in dispatch.logs {
                let incident: Incident = log.into();
                if is_sc_county_area(&incident.area) {
                    out.push(incident);
                }
            }
        }
    }

    Ok(out)
}

/// Filter an incident list to entries on a given route. Takes the spellings
/// the lane-closure filter does ("17", "SR-17", "Hwy 17", "Highway 17") and
/// checks both `location` and `location_desc`.
pub fn filter_by_route<'a>(incidents: &'a [Incident], route: &str) -> Vec<&'a Incident> {
    let number = route_number(route);
    if number.is_empty() {
        return Vec::new();
    }
    // CHP writes "Sr17 N / Summit", "Us101 N / Abbott", "13000 Highway 9".
    // The closing \b keeps route 1 from matching Sr17 and Us101.
    let pattern = format!(
        r"(?i)\b(?:SR|US|HWY|HIGHWAY)[- ]*{}\b",
        regex::escape(&number)
    );
    let Ok(on_route) = Regex::new(&pattern) else {
        return Vec::new();
    };
    incidents
        .iter()
        .filter(|i| on_route.is_match(&i.location) || on_route.is_match(&i.location_desc))
        .collect()
}

fn is_sc_county_area(area: &str) -> bool {
    FuzzyMatcher::new(SC_COUNTY_AREAS.iter().copied())
        .case_insensitive()
        .matches(area)
}

/// Strip literal leading/trailing `"` characters that CHP wraps values in.
/// Example: `"Apr 10 2026  7:56AM"` → `Apr 10 2026  7:56AM`.
pub fn strip_quotes(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("fixtures/chp_mycc_sample.xml");

    #[test]
    fn parse_returns_only_sc_incidents() {
        let incidents = parse_sc_incidents(FIXTURE).unwrap();
        // The fixture has mixed Monterey + Santa Cruz + Hollister Gilroy areas.
        // We should only return Santa Cruz ones.
        assert!(
            !incidents.is_empty(),
            "expected at least 1 Santa Cruz incident"
        );
        for i in &incidents {
            assert!(is_sc_county_area(&i.area), "unexpected area: {}", i.area);
        }
    }

    #[test]
    fn strips_literal_quotes() {
        assert_eq!(strip_quotes("\"hello\""), "hello");
        assert_eq!(strip_quotes("  \"world\"  "), "world");
        assert_eq!(strip_quotes("no quotes"), "no quotes");
        assert_eq!(strip_quotes("\"\""), "");
        assert_eq!(strip_quotes("\""), "\""); // unbalanced — left alone
    }

    #[test]
    fn incident_from_log_strips_quotes() {
        let log = Log {
            id: "TEST".to_string(),
            log_time: "\"Apr 10 2026  7:56AM\"".to_string(),
            log_type: "\"1125-Traffic Hazard\"".to_string(),
            location: "\"Sr17 N / Summit\"".to_string(),
            location_desc: "\"NB 17 AT THE SUMMIT\"".to_string(),
            area: "\"Santa Cruz\"".to_string(),
            latlon: "\"37143411:121984839\"".to_string(),
        };
        let incident: Incident = log.into();
        assert_eq!(incident.log_time, "Apr 10 2026  7:56AM");
        assert_eq!(incident.area, "Santa Cruz");
        assert_eq!(incident.location, "Sr17 N / Summit");
    }

    #[test]
    fn sc_county_area_matching() {
        assert!(is_sc_county_area("Santa Cruz"));
        assert!(is_sc_county_area("santa cruz"));
        assert!(is_sc_county_area("Watsonville"));
        assert!(is_sc_county_area("Scotts Valley"));
        assert!(!is_sc_county_area("Monterey"));
        assert!(!is_sc_county_area("Hollister Gilroy"));
    }

    #[test]
    fn filter_by_route_17() {
        let incidents = parse_sc_incidents(FIXTURE).unwrap();
        // The fixture's SR17 entry has Area=Santa Cruz, so it should survive the SC filter.
        let sr17 = filter_by_route(&incidents, "17");
        assert!(
            sr17.iter().any(|i| i.location.contains("Sr17")
                || i.location.contains("SR17")
                || i.location.contains("SR-17")),
            "expected to find the Sr17 Summit entry"
        );
    }

    #[test]
    fn unclosed_tag_xml_errors_gracefully() {
        // Truncated mid-download: unclosed <Log> element.
        let cut = &FIXTURE[..FIXTURE.len() / 2];
        let err = parse_sc_incidents(cut).unwrap_err().to_string();
        assert!(err.contains("parsing CHP XML"), "got: {err}");
    }

    #[test]
    fn feed_without_gghb_center_is_empty() {
        // Other comm centers only — no GGHB/MYCC means no SC incidents,
        // not an error.
        let xml = r#"<?xml version="1.0" ?>
<State>
<Center ID = "SAHB">
<Dispatch ID = "SACC">
<Log ID = "260707SA0015">
    <LogTime>"Jul  7 2026 12:18AM"</LogTime>
    <LogType>"FIRE-Report of Fire"</LogType>
    <Location>"Hillsdale Blvd"</Location>
    <LocationDesc>""</LocationDesc>
    <Area>"North Sac"</Area>
    <LATLON>"38672378:121356592"</LATLON>
</Log>
</Dispatch>
</Center>
</State>"#;
        let incidents = parse_sc_incidents(xml).unwrap();
        assert!(incidents.is_empty());
    }

    #[test]
    fn empty_dispatch_is_empty_not_error() {
        // MYCC present but with no Log children — `default` on `logs` kicks in.
        let xml = r#"<?xml version="1.0" ?>
<State>
<Center ID = "GGHB">
<Dispatch ID = "MYCC">
</Dispatch>
</Center>
</State>"#;
        let incidents = parse_sc_incidents(xml).unwrap();
        assert!(incidents.is_empty());
    }

    #[test]
    fn log_missing_optional_elements_defaults_empty() {
        // CHP occasionally omits detail elements — every text field defaults
        // to "" instead of failing the whole feed.
        let xml = r#"<?xml version="1.0" ?>
<State>
<Center ID = "GGHB">
<Dispatch ID = "MYCC">
<Log ID = "260707MY0001">
    <LogType>"1183-Trfc Collision-Unkn Inj"</LogType>
    <Area>"Santa Cruz"</Area>
</Log>
</Dispatch>
</Center>
</State>"#;
        let incidents = parse_sc_incidents(xml).unwrap();
        assert_eq!(incidents.len(), 1);
        assert_eq!(incidents[0].log_type, "1183-Trfc Collision-Unkn Inj");
        assert_eq!(incidents[0].log_time, "");
        assert_eq!(incidents[0].location, "");
        assert_eq!(incidents[0].latlon, "");
    }

    // ───── filter_by_route ─────

    fn at(location: &str, desc: &str) -> Incident {
        Incident {
            id: "261119MY0001".to_string(),
            log_time: "Nov 19 2026  9:08AM".to_string(),
            log_type: "1125-Traffic Hazard".to_string(),
            location: location.to_string(),
            location_desc: desc.to_string(),
            area: "Santa Cruz".to_string(),
            latlon: String::new(),
        }
    }

    fn found(incidents: &[Incident], route: &str) -> Vec<String> {
        filter_by_route(incidents, route)
            .iter()
            .map(|i| i.location.clone())
            .collect()
    }

    #[test]
    fn route_filter_accepts_the_spellings_the_lane_closure_filter_does() {
        let incidents = [
            at("Sr17 N / Summit", "NB 17 AT THE SUMMIT"),
            at("SR17 S / SUMMIT", "NB AND SB 17"),
            at("Sr1 N / Soquel Dr Ofr", "NB OFR"),
        ];
        for q in [
            "17",
            " 17 ",
            "SR-17",
            "sr17",
            "Sr 17",
            "Hwy 17",
            "HWY 17",
            "hwy17",
            "Highway 17",
            "Route 17",
            "CA-17",
        ] {
            assert_eq!(
                found(&incidents, q),
                ["Sr17 N / Summit", "SR17 S / SUMMIT"],
                "query {q:?}"
            );
        }
    }

    #[test]
    fn route_filter_matches_the_whole_number_only() {
        let incidents = [
            at("Sr1 N / Soquel Dr Ofr", ""),
            at("Sr17 N / Summit", ""),
            at("Us101 N / Abbott", ""),
            at("SR129 / Carlton Rd", ""),
        ];
        assert_eq!(found(&incidents, "1"), ["Sr1 N / Soquel Dr Ofr"]);
        assert_eq!(found(&incidents, "101"), ["Us101 N / Abbott"]);
        assert_eq!(found(&incidents, "129"), ["SR129 / Carlton Rd"]);
    }

    #[test]
    fn route_filter_reads_street_addresses_and_descriptions() {
        let incidents = [
            at("13000 Highway 9", ""),
            at("Graham Hill Rd / Lockwood Ln", "JNO HWY  17"),
            at("17 Mile Dr", ""),
        ];
        assert_eq!(found(&incidents, "9"), ["13000 Highway 9"]);
        // "17 Mile Dr" names no route; the other is on 17 by its description.
        assert_eq!(found(&incidents, "17"), ["Graham Hill Rd / Lockwood Ln"]);
    }

    #[test]
    fn route_filter_with_no_number_matches_nothing() {
        let incidents = [at("Sr17 N / Summit", ""), at("Us101 N / Abbott", "")];
        for q in ["", "  ", "SR", "Highway"] {
            assert!(found(&incidents, q).is_empty(), "query {q:?}");
        }
    }

    // ───── check_feed: what CHP answered with ─────

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// CHP's headers on a live copy, 2026-11-19: the file is seconds old.
    fn live_meta() -> FeedMeta {
        FeedMeta {
            status: 200,
            content_type: Some("text/xml".to_string()),
            last_modified: Some(utc("2026-11-19T18:19:09Z")),
            served_at: Some(utc("2026-11-19T18:19:13Z")),
        }
    }

    /// The copy one CHP server kept serving that day, frozen at 07:57:17 GMT.
    fn frozen_meta() -> FeedMeta {
        FeedMeta {
            last_modified: Some(utc("2026-11-19T07:57:17Z")),
            served_at: Some(utc("2026-11-19T17:26:04Z")),
            ..live_meta()
        }
    }

    fn bad_copy(meta: &FeedMeta, body: &str) -> String {
        check_feed(meta, body, utc("2026-11-19T17:26:05Z"))
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn cut_off_copy_is_named_with_its_size_age_and_last_bytes() {
        // The real broken copy stopped mid-tag inside a <details> block.
        let cut = format!(
            "{}<details>\r\n\t\t\t<DetailTime>\"Nov 18 2026 12:16PM\"</D",
            &FIXTURE[..FIXTURE.find("</Log>").unwrap()]
        );
        let err = bad_copy(&frozen_meta(), &cut);
        assert!(err.contains("cut-off copy"), "{err}");
        assert!(err.contains("HTTP 200 text/xml"), "{err}");
        assert!(err.contains(&format!("{} bytes", cut.len())), "{err}");
        assert!(err.contains("last modified Nov 18 11:57 PM PST"), "{err}");
        assert!(
            err.contains("<DetailTime>\"Nov 18 2026 12:16PM\"</D"),
            "{err}"
        );
    }

    #[test]
    fn web_page_is_reported_as_not_the_feed() {
        // What a firewall in front of CHP would answer with: a 200 and a page.
        let page = "<html><head><title>Request Rejected</title></head><body>The requested URL was rejected. Please consult with your administrator.<br><br>Your support ID is: 1234567890</body></html>";
        let meta = FeedMeta {
            content_type: Some("text/html; charset=utf-8".to_string()),
            ..live_meta()
        };
        let err = bad_copy(&meta, page);
        assert!(err.contains("not the incident feed"), "{err}");
        assert!(err.contains("HTTP 200 text/html"), "{err}");
        assert!(err.contains("<title>Request Rejected</title>"), "{err}");
    }

    #[test]
    fn well_formed_page_is_not_zero_incidents() {
        // quick-xml doesn't check the root's name, so XHTML deserializes cleanly.
        let page = "<html><body><p>Scheduled maintenance</p></body></html>";
        let err = bad_copy(&live_meta(), page);
        assert!(err.contains("not the incident feed"), "{err}");
    }

    #[test]
    fn feed_with_no_centers_is_an_error() {
        let err = bad_copy(&live_meta(), "<?xml version=\"1.0\" ?>\n<State>\n</State>");
        assert!(err.contains("no dispatch centers"), "{err}");
    }

    #[test]
    fn empty_body_is_named() {
        for body in ["", " \r\n"] {
            let err = bad_copy(&live_meta(), body);
            assert!(err.contains("empty body"), "{body:?}: {err}");
        }
    }

    #[test]
    fn complete_but_frozen_copy_is_stale_not_current() {
        let err = bad_copy(&frozen_meta(), FIXTURE);
        assert!(err.contains("stale copy"), "{err}");
        assert!(err.contains("9 h 28 min"), "{err}");
    }

    #[test]
    fn copy_a_few_minutes_old_is_accepted_and_one_past_the_limit_is_not() {
        let served = utc("2026-11-19T18:19:13Z");
        let aged = |minutes| FeedMeta {
            last_modified: Some(served - chrono::Duration::minutes(minutes)),
            served_at: Some(served),
            ..live_meta()
        };
        assert!(check_feed(&aged(14), FIXTURE, served).is_ok());
        assert!(check_feed(&aged(16), FIXTURE, served).is_err());
    }

    #[test]
    fn malformed_but_complete_copy_quotes_the_parser() {
        let broken = FIXTURE.replacen("</LogTime>", "</LogTim>", 1);
        let err = bad_copy(&live_meta(), &broken);
        assert!(err.contains("XML error"), "{err}");
        assert!(err.contains("LogTim"), "{err}");
    }

    #[test]
    fn snippets_do_not_split_multibyte_characters() {
        let body = format!(
            "<?xml version=\"1.0\" ?><State><Center ID = \"GGHB\">{}",
            "é".repeat(500)
        );
        let err = bad_copy(&live_meta(), &body);
        assert!(err.contains("cut-off copy"), "{err}");
    }

    #[test]
    fn live_copy_gives_incidents_dated_by_the_feed() {
        let snapshot = check_feed(&live_meta(), FIXTURE, utc("2026-11-19T18:19:40Z")).unwrap();
        assert!(!snapshot.incidents.is_empty());
        // 18:19:09 GMT is 10:19:09 AM PST. The fetch time isn't used.
        assert_eq!(
            snapshot.as_of.format("%-I:%M:%S %p %Z").to_string(),
            "10:19:09 AM PST"
        );
    }

    #[test]
    fn copy_without_date_headers_is_accepted_and_dated_by_the_fetch() {
        let meta = FeedMeta {
            status: 200,
            content_type: None,
            last_modified: None,
            served_at: None,
        };
        let snapshot = check_feed(&meta, FIXTURE, utc("2026-11-19T18:19:40Z")).unwrap();
        assert_eq!(
            snapshot.as_of.format("%-I:%M:%S %p").to_string(),
            "10:19:40 AM"
        );
    }

    #[test]
    fn http_dates_parse_in_the_form_chp_sends() {
        assert_eq!(
            parse_http_date("Thu, 19 Nov 2026 07:57:17 GMT"),
            Some(utc("2026-11-19T07:57:17Z"))
        );
        assert_eq!(parse_http_date("yesterday"), None);
    }
}
