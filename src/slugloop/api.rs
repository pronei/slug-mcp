/// SlugLoop API client for UCSC campus loop bus real-time data.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

pub const SLUGLOOP_BASE_URL: &str = "https://slugloop.tech";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// A single bus from the `/buses` endpoint.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bus {
    #[serde(default)]
    pub id: String,
    #[serde(default, alias = "lastLatitude")]
    pub lat: f64,
    #[serde(default, alias = "lastLongitude")]
    pub lon: f64,
    #[serde(default)]
    pub direction: String,
    #[serde(default)]
    pub heading: f64,
    #[serde(default)]
    #[allow(dead_code)]
    pub route: String,
}

/// ETA data for stops in one direction (CW or CCW).
/// Keys are stop names (lowercase), values are ETA in seconds (or null).
pub type StopEtas = HashMap<String, Option<f64>>;

/// Response from `/busEta` — contains CW and CCW stop ETA documents.
#[derive(Debug, Deserialize)]
pub struct BusEtaResponse {
    #[serde(default, alias = "CW", alias = "cw")]
    pub clockwise: Option<StopEtas>,
    #[serde(default, alias = "CCW", alias = "ccw")]
    pub counter_clockwise: Option<StopEtas>,
}

/// Fetch current bus locations from SlugLoop.
pub async fn fetch_buses(http: &reqwest::Client) -> Result<Vec<Bus>> {
    let resp = http
        .get(format!("{}/buses", SLUGLOOP_BASE_URL))
        .header("User-Agent", "slug-mcp/0.1")
        .header("Accept", "application/json")
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .context("failed to reach SlugLoop API")?
        .error_for_status()
        .context("SlugLoop /buses returned error status")?;

    let buses: Vec<Bus> = resp
        .json()
        .await
        .context("failed to parse SlugLoop /buses response")?;

    Ok(buses)
}

/// Fetch ETAs for all stops from SlugLoop.
pub async fn fetch_etas(http: &reqwest::Client) -> Result<BusEtaResponse> {
    let resp = http
        .get(format!("{}/busEta", SLUGLOOP_BASE_URL))
        .header("User-Agent", "slug-mcp/0.1")
        .header("Accept", "application/json")
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .context("failed to reach SlugLoop API")?
        .error_for_status()
        .context("SlugLoop /busEta returned error status")?;

    let etas: BusEtaResponse = resp
        .json()
        .await
        .context("failed to parse SlugLoop /busEta response")?;

    Ok(etas)
}
