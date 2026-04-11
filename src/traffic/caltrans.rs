//! Caltrans District 5 Lane Closure System (LCS) JSON feed.
//!
//! Source: <https://cwwp2.dot.ca.gov/data/d5/lcs/lcsStatusD05.json>
//!
//! Returns all active lane closures in Caltrans District 5 (Monterey, San
//! Luis Obispo, Santa Barbara, Santa Cruz, San Benito counties). We filter
//! client-side to Santa Cruz County entries and optionally by route.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const D5_LCS_URL: &str = "https://cwwp2.dot.ca.gov/data/d5/lcs/lcsStatusD05.json";

// ───── upstream JSON types ─────

#[derive(Debug, Deserialize, Serialize)]
pub struct LcsFeed {
    #[serde(default)]
    pub data: Vec<LcsEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct LcsEntry {
    pub lcs: Lcs,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Lcs {
    #[serde(default)]
    pub index: String,
    #[serde(default)]
    pub location: LcsLocation,
    #[serde(default)]
    pub closure: LcsClosure,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct LcsLocation {
    #[serde(rename = "travelFlowDirection", default)]
    pub direction: String,
    #[serde(default)]
    pub begin: LocationPoint,
    #[serde(default)]
    pub end: LocationPoint,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct LocationPoint {
    #[serde(rename = "beginDistrict", default)]
    #[serde(alias = "endDistrict")]
    pub district: String,
    #[serde(rename = "beginLocationName", default)]
    #[serde(alias = "endLocationName")]
    pub location_name: String,
    #[serde(rename = "beginFreeFormDescription", default)]
    #[serde(alias = "endFreeFormDescription")]
    pub free_form: String,
    #[serde(rename = "beginNearbyPlace", default)]
    #[serde(alias = "endNearbyPlace")]
    pub nearby_place: String,
    #[serde(rename = "beginCounty", default)]
    #[serde(alias = "endCounty")]
    pub county: String,
    #[serde(rename = "beginRoute", default)]
    #[serde(alias = "endRoute")]
    pub route: String,
    #[serde(rename = "beginLatitude", default)]
    #[serde(alias = "endLatitude")]
    pub latitude: String,
    #[serde(rename = "beginLongitude", default)]
    #[serde(alias = "endLongitude")]
    pub longitude: String,
    #[serde(rename = "beginPostmile", default)]
    #[serde(alias = "endPostmile")]
    pub postmile: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct LcsClosure {
    #[serde(rename = "closureID", default)]
    pub closure_id: String,
    #[serde(rename = "closureTimestamp", default)]
    pub timestamp: ClosureTimestamp,
    #[serde(rename = "typeOfClosure", default)]
    pub type_of_closure: String,
    #[serde(rename = "typeOfWork", default)]
    pub type_of_work: String,
    #[serde(rename = "estimatedDelay", default)]
    pub estimated_delay: String,
    #[serde(rename = "lanesClosed", default)]
    pub lanes_closed: String,
    #[serde(rename = "totalExistingLanes", default)]
    pub total_lanes: String,
    #[serde(rename = "facility", default)]
    pub facility: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct ClosureTimestamp {
    #[serde(rename = "closureStartDate", default)]
    pub start_date: String,
    #[serde(rename = "closureStartTime", default)]
    pub start_time: String,
    #[serde(rename = "closureEndDate", default)]
    pub end_date: String,
    #[serde(rename = "closureEndTime", default)]
    pub end_time: String,
    #[serde(rename = "isClosureEndIndefinite", default)]
    pub end_indefinite: String,
}

// ───── normalized closure for display ─────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaneClosure {
    pub closure_id: String,
    pub route: String,
    pub direction: String,
    pub county: String,
    pub nearby_place: String,
    pub location_name: String,
    pub free_form: String,
    pub type_of_closure: String,
    pub type_of_work: String,
    pub lanes_closed: String,
    pub total_lanes: String,
    pub estimated_delay: String,
    pub facility: String,
    pub start_date: String,
    pub start_time: String,
    pub end_date: String,
    pub end_time: String,
    pub end_indefinite: bool,
}

impl From<LcsEntry> for LaneClosure {
    fn from(e: LcsEntry) -> Self {
        let lcs = e.lcs;
        LaneClosure {
            closure_id: lcs.closure.closure_id,
            route: lcs.location.begin.route,
            direction: lcs.location.direction,
            county: lcs.location.begin.county,
            nearby_place: lcs.location.begin.nearby_place,
            location_name: lcs.location.begin.location_name,
            free_form: lcs.location.begin.free_form,
            type_of_closure: lcs.closure.type_of_closure,
            type_of_work: lcs.closure.type_of_work,
            lanes_closed: lcs.closure.lanes_closed,
            total_lanes: lcs.closure.total_lanes,
            estimated_delay: lcs.closure.estimated_delay,
            facility: lcs.closure.facility,
            start_date: lcs.closure.timestamp.start_date,
            start_time: lcs.closure.timestamp.start_time,
            end_date: lcs.closure.timestamp.end_date,
            end_time: lcs.closure.timestamp.end_time,
            end_indefinite: lcs
                .closure
                .timestamp
                .end_indefinite
                .eq_ignore_ascii_case("true"),
        }
    }
}

// ───── public API ─────

pub async fn fetch_sc_closures(http: &reqwest::Client) -> Result<Vec<LaneClosure>> {
    let resp = http
        .get(D5_LCS_URL)
        .send()
        .await
        .context("GET Caltrans CWWP2 D5 LCS")?;
    if !resp.status().is_success() {
        anyhow::bail!("Caltrans LCS returned HTTP {}", resp.status());
    }
    let body = resp
        .text()
        .await
        .context("reading Caltrans LCS body")?;
    parse_sc_closures(&body)
}

pub fn parse_sc_closures(body: &str) -> Result<Vec<LaneClosure>> {
    let feed: LcsFeed = serde_json::from_str(body).context("parsing Caltrans LCS JSON")?;
    let mut out: Vec<LaneClosure> = feed
        .data
        .into_iter()
        .map(LaneClosure::from)
        .filter(|c| c.county.eq_ignore_ascii_case("Santa Cruz"))
        .collect();
    // Sort by route then start date for stable output
    out.sort_by(|a, b| {
        a.route
            .cmp(&b.route)
            .then_with(|| a.start_date.cmp(&b.start_date))
    });
    Ok(out)
}

/// Filter closures to those on a specific route. Accepts "1", "9", "17",
/// "SR-1", "Sr1", "101", "US-101", etc.
pub fn filter_by_route<'a>(closures: &'a [LaneClosure], route: &str) -> Vec<&'a LaneClosure> {
    let r = route.trim().to_uppercase();
    // Normalize candidate matches
    let bare = r
        .trim_start_matches("SR-")
        .trim_start_matches("SR")
        .trim_start_matches("US-")
        .trim_start_matches("US")
        .trim_start_matches("HWY ")
        .trim_start_matches("HWY")
        .trim();
    closures
        .iter()
        .filter(|c| {
            let route_up = c.route.to_uppercase();
            route_up == format!("SR-{}", bare)
                || route_up == format!("US-{}", bare)
                || route_up == format!("SR{}", bare)
                || route_up == format!("US{}", bare)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("fixtures/caltrans_d5_sample.json");

    #[test]
    fn parse_sc_closures_from_fixture() {
        let closures = parse_sc_closures(FIXTURE).unwrap();
        assert!(!closures.is_empty(), "expected at least 1 closure");
        for c in &closures {
            assert_eq!(c.county, "Santa Cruz");
            assert!(!c.route.is_empty(), "route should be present");
        }
    }

    #[test]
    fn filter_by_route_sr1() {
        let closures = parse_sc_closures(FIXTURE).unwrap();
        let sr1 = filter_by_route(&closures, "1");
        // Fixture samples are all SR-1 entries
        assert!(!sr1.is_empty(), "expected SR-1 closures in fixture");
        for c in &sr1 {
            assert_eq!(c.route, "SR-1");
        }
    }

    #[test]
    fn filter_by_route_sr17_none() {
        let closures = parse_sc_closures(FIXTURE).unwrap();
        let sr17 = filter_by_route(&closures, "17");
        // Fixture is SR-1 only, so SR-17 should yield nothing
        assert!(sr17.is_empty());
    }
}
