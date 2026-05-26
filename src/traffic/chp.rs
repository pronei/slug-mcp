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
use serde::{Deserialize, Serialize};

use crate::util::FuzzyMatcher;

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

/// Fetch + parse the full CHP feed, return Santa Cruz County incidents only.
pub async fn fetch_sc_incidents(http: &reqwest::Client) -> Result<Vec<Incident>> {
    let resp = http
        .get(CHP_FEED_URL)
        .send()
        .await
        .context("GET CHP sa.xml")?;
    if !resp.status().is_success() {
        anyhow::bail!("CHP feed returned HTTP {}", resp.status());
    }
    let body = resp.text().await.context("reading CHP body")?;
    parse_sc_incidents(&body)
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

/// Filter an incident list to entries on a given route (e.g. "17", "9", "1").
/// Matches an SR/HWY/US designator followed by the route number in either the
/// `location` or `location_desc` field, case-insensitively.
pub fn filter_by_route<'a>(incidents: &'a [Incident], route: &str) -> Vec<&'a Incident> {
    let route = route.trim().trim_start_matches(|c: char| {
        c.eq_ignore_ascii_case(&'h') || c.eq_ignore_ascii_case(&'w') || c == 'y' || c == ' '
    });
    // The route number must not be followed by another digit, otherwise "1"
    // matches "SR17" and "US101". `regex` has no lookahead, so we assert the
    // boundary with a trailing non-digit (or end-of-string) instead.
    let pattern = format!(
        r"(?i)\b(?:sr|hwy|us)[\s-]*{}(?:[^0-9]|$)",
        regex::escape(route)
    );
    let re = match regex::Regex::new(&pattern) {
        Ok(re) => re,
        Err(_) => return Vec::new(),
    };
    incidents
        .iter()
        .filter(|i| re.is_match(&format!("{} {}", i.location, i.location_desc)))
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
            assert!(
                is_sc_county_area(&i.area),
                "unexpected area: {}",
                i.area
            );
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
    fn filter_by_route_does_not_match_longer_routes() {
        // Regression: "SR1" is a prefix of "SR17"/"US101", so a naive substring
        // match surfaced Hwy 17 incidents when asked about Hwy 1.
        let mk = |loc: &str| Incident {
            id: loc.to_string(),
            log_time: String::new(),
            log_type: String::new(),
            location: loc.to_string(),
            location_desc: String::new(),
            area: "Santa Cruz".to_string(),
            latlon: String::new(),
        };
        let incidents = vec![
            mk("Sr1 S / Buena Vista"),
            mk("Sr17 S / Spanish Oaks"),
            mk("US101 N / Foo"),
        ];

        let r1 = filter_by_route(&incidents, "1");
        assert_eq!(r1.len(), 1, "route 1 should match only Sr1");
        assert_eq!(r1[0].location, "Sr1 S / Buena Vista");

        let r17 = filter_by_route(&incidents, "17");
        assert_eq!(r17.len(), 1);
        assert_eq!(r17[0].location, "Sr17 S / Spanish Oaks");

        let r101 = filter_by_route(&incidents, "101");
        assert_eq!(r101.len(), 1);
        assert_eq!(r101[0].location, "US101 N / Foo");
    }
}
