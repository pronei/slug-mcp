//! NASA FIRMS satellite fire detections for Santa Cruz County.
//!
//! Pulls near-real-time VIIRS hot-spots via the FIRMS Area CSV API. The API
//! requires a free 32-character MAP_KEY (register at
//! <https://firms.modaps.eosdis.nasa.gov/api/area/>). Graceful degradation:
//! if `SLUG_MCP_FIRMS_KEY` isn't set, the tool returns registration
//! instructions instead of erroring.
//!
//! Docs: <https://firms.modaps.eosdis.nasa.gov/api/area/>

use std::sync::Arc;

use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cache::CacheStore;

/// Santa Cruz County bounding box: (west, south, east, north).
///
/// Loose-fit box covering the county + a small buffer so edge-of-county
/// detections (e.g., above Bonny Doon near the ridge) still show up.
pub const SC_BBOX: (f64, f64, f64, f64) = (-122.35, 36.85, -121.57, 37.30);

/// Downtown Santa Cruz — used for distance annotations.
const DOWNTOWN_LAT: f64 = 36.9741;
const DOWNTOWN_LON: f64 = -122.0308;

const FIRMS_BASE: &str = "https://firms.modaps.eosdis.nasa.gov/api/area/csv";
const SENSOR: &str = "VIIRS_SNPP_NRT";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FireDetectionsRequest {
    /// Days to query (1-5, per FIRMS API limits). Default: 1.
    pub days: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Detection {
    pub latitude: f64,
    pub longitude: f64,
    pub bright_ti4: f64,
    pub acq_date: String,
    pub acq_time: String,
    pub satellite: String,
    pub confidence: Confidence,
    pub frp: f64,
    pub daynight: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Low,
    Nominal,
    High,
    Unknown,
}

impl Confidence {
    fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "l" | "low" => Confidence::Low,
            "n" | "nominal" => Confidence::Nominal,
            "h" | "high" => Confidence::High,
            _ => Confidence::Unknown,
        }
    }

    fn icon(&self) -> &'static str {
        match self {
            Confidence::High => "🔥",
            Confidence::Nominal => "⚠",
            Confidence::Low => "•",
            Confidence::Unknown => "?",
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Confidence::High => "high",
            Confidence::Nominal => "nominal",
            Confidence::Low => "low",
            Confidence::Unknown => "unknown",
        }
    }
}

pub struct FireService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
    map_key: Option<String>,
}

impl FireService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>, map_key: Option<String>) -> Self {
        Self {
            http,
            cache,
            map_key,
        }
    }

    pub async fn get_detections(&self, days: u32) -> Result<String> {
        let days = days.clamp(1, 5);

        let map_key = match &self.map_key {
            Some(k) if !k.is_empty() => k.clone(),
            _ => {
                return Ok(
                    "NASA FIRMS map key not configured.\n\
                     Get a free key at https://firms.modaps.eosdis.nasa.gov/api/area/ and\n\
                     set the `SLUG_MCP_FIRMS_KEY` environment variable."
                        .to_string(),
                );
            }
        };

        let cache_key = format!("fire:firms:{}", days);
        let http = self.http.clone();
        let detections_result = self
            .cache
            .get_or_fetch::<Vec<Detection>, _, _>(&cache_key, 600, move || async move {
                fetch_detections(&http, &map_key, SC_BBOX, days).await
            })
            .await;

        match detections_result {
            Ok(detections) => Ok(format_detections(&detections, days)),
            Err(e) => {
                tracing::warn!("FIRMS fetch failed: {}", e);
                Ok(format!(
                    "⚠ NASA FIRMS temporarily unreachable. Try again in a minute.\n(details: {})",
                    e
                ))
            }
        }
    }
}

async fn fetch_detections(
    http: &reqwest::Client,
    map_key: &str,
    bbox: (f64, f64, f64, f64),
    days: u32,
) -> Result<Vec<Detection>> {
    let (w, s, e, n) = bbox;
    let url = format!(
        "{}/{}/{}/{:.4},{:.4},{:.4},{:.4}/{}",
        FIRMS_BASE, map_key, SENSOR, w, s, e, n, days
    );
    let resp = http
        .get(&url)
        .send()
        .await
        .context("FIRMS HTTP request failed")?;
    if !resp.status().is_success() {
        anyhow::bail!("FIRMS returned HTTP {}", resp.status());
    }
    let body = resp.text().await.context("reading FIRMS CSV body")?;

    // FIRMS returns an error message starting with "Invalid" in the body
    // rather than an HTTP error for quota/key problems — detect those first.
    let trimmed = body.trim_start();
    if trimmed.starts_with("Invalid") || trimmed.starts_with("Error") {
        anyhow::bail!("FIRMS error: {}", trimmed.lines().next().unwrap_or(""));
    }

    parse_csv(&body)
}

/// Parse the FIRMS VIIRS CSV response.
///
/// Column order for VIIRS_SNPP_NRT:
/// latitude, longitude, bright_ti4, scan, track, acq_date, acq_time,
/// satellite, instrument, confidence, version, bright_ti5, frp, daynight
fn parse_csv(body: &str) -> Result<Vec<Detection>> {
    let mut out = Vec::new();
    let mut lines = body.lines();

    // Read header row and find column indices (FIRMS sometimes shuffles columns
    // between products, so don't hardcode positions — look them up by name).
    let header = lines.next().unwrap_or("");
    let cols: Vec<&str> = header.split(',').map(str::trim).collect();

    let idx = |name: &str| cols.iter().position(|c| c.eq_ignore_ascii_case(name));
    let i_lat = idx("latitude");
    let i_lon = idx("longitude");
    let i_bti4 = idx("bright_ti4");
    let i_date = idx("acq_date");
    let i_time = idx("acq_time");
    let i_sat = idx("satellite");
    let i_conf = idx("confidence");
    let i_frp = idx("frp");
    let i_dn = idx("daynight");

    if i_lat.is_none() || i_lon.is_none() || i_date.is_none() {
        anyhow::bail!(
            "unexpected FIRMS CSV header (missing lat/lon/date): {}",
            header
        );
    }

    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();

        let get = |i: Option<usize>| i.and_then(|j| fields.get(j).copied()).unwrap_or("");
        let parse_f64 = |i: Option<usize>| get(i).parse::<f64>().ok();

        let latitude = match parse_f64(i_lat) {
            Some(v) => v,
            None => continue,
        };
        let longitude = match parse_f64(i_lon) {
            Some(v) => v,
            None => continue,
        };

        out.push(Detection {
            latitude,
            longitude,
            bright_ti4: parse_f64(i_bti4).unwrap_or(0.0),
            acq_date: get(i_date).to_string(),
            acq_time: get(i_time).to_string(),
            satellite: get(i_sat).to_string(),
            confidence: Confidence::from_str(get(i_conf)),
            frp: parse_f64(i_frp).unwrap_or(0.0),
            daynight: get(i_dn).to_string(),
        });
    }

    Ok(out)
}

fn haversine_miles(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 3958.8; // earth radius in miles
    let d_lat = (lat2 - lat1).to_radians();
    let d_lon = (lon2 - lon1).to_radians();
    let a = (d_lat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (d_lon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();
    r * c
}

fn format_detections(detections: &[Detection], days: u32) -> String {
    let day_word = if days == 1 { "day" } else { "days" };

    if detections.is_empty() {
        return format!(
            "# Santa Cruz County fire detections\n\n\
             No VIIRS_SNPP fire detections in the last {} {}.\n\n\
             _Source: NASA FIRMS. Last checked: {}_\n",
            days,
            day_word,
            chrono::Local::now().format("%-I:%M %p")
        );
    }

    // Sort by FRP (fire radiative power) descending — brightest first.
    let mut sorted: Vec<&Detection> = detections.iter().collect();
    sorted.sort_by(|a, b| b.frp.partial_cmp(&a.frp).unwrap_or(std::cmp::Ordering::Equal));

    let mut out = format!(
        "# Santa Cruz County fire detections ({} total, last {} {})\n\n",
        detections.len(),
        days,
        day_word
    );
    out.push_str(
        "Satellite hot-spots from NASA FIRMS VIIRS_SNPP_NRT (~375m resolution, ~60s latency).\n\
         Note: thermal anomalies include industrial sources (quarries, flares) as well as\n\
         wildfires — check official CAL FIRE before acting.\n\n",
    );

    for d in sorted.iter().take(25) {
        let dist =
            haversine_miles(DOWNTOWN_LAT, DOWNTOWN_LON, d.latitude, d.longitude);
        let daynight = if d.daynight == "D" {
            "day"
        } else if d.daynight == "N" {
            "night"
        } else {
            d.daynight.as_str()
        };
        out.push_str(&format!(
            "{} **{:.4}, {:.4}** ({:.1} mi from downtown)\n",
            d.confidence.icon(),
            d.latitude,
            d.longitude,
            dist
        ));
        out.push_str(&format!(
            "   {} at {} UTC · FRP {:.1} MW · {:.0} K · {} · confidence: {} · sat {}\n\n",
            d.acq_date,
            d.acq_time,
            d.frp,
            d.bright_ti4,
            daynight,
            d.confidence.label(),
            d.satellite
        ));
    }

    if sorted.len() > 25 {
        out.push_str(&format!("_...and {} more detections omitted._\n\n", sorted.len() - 25));
    }

    out.push_str(&format!(
        "_Source: NASA FIRMS. Last updated: {}_\n",
        chrono::Local::now().format("%-I:%M %p")
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_csv_sample() {
        let csv = "latitude,longitude,bright_ti4,scan,track,acq_date,acq_time,satellite,instrument,confidence,version,bright_ti5,frp,daynight\n\
                   37.1234,-122.0567,320.5,0.45,0.42,2026-04-09,1234,N,VIIRS,h,2.0NRT,290.3,12.5,D\n\
                   36.9456,-122.1234,305.1,0.50,0.48,2026-04-09,1240,N,VIIRS,n,2.0NRT,285.2,5.8,N\n";
        let parsed = parse_csv(csv).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].confidence, Confidence::High);
        assert_eq!(parsed[1].confidence, Confidence::Nominal);
        assert!((parsed[0].frp - 12.5).abs() < 0.001);
        assert_eq!(parsed[0].daynight, "D");
    }

    #[test]
    fn parse_csv_empty_body_ok() {
        // FIRMS returns just the header row when there are no detections
        let csv = "latitude,longitude,bright_ti4,scan,track,acq_date,acq_time,satellite,instrument,confidence,version,bright_ti5,frp,daynight\n";
        let parsed = parse_csv(csv).unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn parse_csv_missing_required_column_errors() {
        let csv = "foo,bar,baz\n1,2,3\n";
        assert!(parse_csv(csv).is_err());
    }

    #[test]
    fn confidence_from_letter() {
        assert_eq!(Confidence::from_str("h"), Confidence::High);
        assert_eq!(Confidence::from_str("n"), Confidence::Nominal);
        assert_eq!(Confidence::from_str("l"), Confidence::Low);
        assert_eq!(Confidence::from_str("x"), Confidence::Unknown);
    }

    #[test]
    fn haversine_downtown_to_self_zero() {
        let d = haversine_miles(DOWNTOWN_LAT, DOWNTOWN_LON, DOWNTOWN_LAT, DOWNTOWN_LON);
        assert!(d < 0.001);
    }
}
