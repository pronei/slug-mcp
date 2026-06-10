//! USGS Earthquake Hazards API — recent seismic activity near Santa Cruz.
//!
//! Queries the USGS FDSNWS event endpoint for GeoJSON earthquake data within a
//! configurable radius of Santa Cruz (default 50 km). No API key required.
//!
//! Docs: <https://earthquake.usgs.gov/fdsnws/event/1/>

use std::fmt::Write;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::DateTime;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cache::CacheStore;

/// Default coordinates: downtown Santa Cruz.
const DEFAULT_LAT: f64 = 36.9741;
const DEFAULT_LON: f64 = -122.0308;

const USGS_BASE: &str = "https://earthquake.usgs.gov/fdsnws/event/1/query";

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EarthquakeRequest {
    /// Latitude (default 36.9741, Santa Cruz).
    pub lat: Option<f64>,
    /// Longitude (default -122.0308, Santa Cruz).
    pub lon: Option<f64>,
    /// Search radius in km (default 50, max 200).
    pub radius_km: Option<f64>,
    /// Minimum magnitude to include (default 1.0, range 0-9).
    pub min_magnitude: Option<f64>,
    /// Days back to search (default 7, max 30).
    pub days: Option<u32>,
    /// Max results (default 20, max 100).
    pub limit: Option<u32>,
}

// ---------------------------------------------------------------------------
// USGS GeoJSON response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize, Clone)]
struct UsgsResponse {
    metadata: UsgsMetadata,
    features: Vec<UsgsFeature>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct UsgsMetadata {
    count: u32,
    title: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct UsgsFeature {
    properties: UsgsProperties,
    geometry: UsgsGeometry,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct UsgsProperties {
    mag: Option<f64>,
    place: Option<String>,
    time: Option<i64>,
    #[serde(rename = "type")]
    event_type: Option<String>,
    title: Option<String>,
    felt: Option<u32>,
    sig: Option<u32>,
    alert: Option<String>,
    tsunami: Option<u32>,
    status: Option<String>,
    #[serde(rename = "magType")]
    mag_type: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct UsgsGeometry {
    coordinates: Vec<f64>, // [lon, lat, depth_km]
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

pub struct EarthquakeService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl EarthquakeService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    pub async fn get_earthquakes(
        &self,
        lat: f64,
        lon: f64,
        radius_km: f64,
        min_magnitude: f64,
        days: u32,
        limit: u32,
    ) -> Result<String> {
        let lat = if lat == 0.0 { DEFAULT_LAT } else { lat };
        let lon = if lon == 0.0 { DEFAULT_LON } else { lon };
        let radius_km = radius_km.clamp(1.0, 200.0);
        let min_magnitude = min_magnitude.clamp(0.0, 9.0);
        let days = days.clamp(1, 30);
        let limit = limit.clamp(1, 100);

        let cache_key = format!(
            "earthquake:{:.3}:{:.3}:{}:{}:{}:{}",
            lat, lon, radius_km, min_magnitude, days, limit
        );

        let http = self.http.clone();
        let result = self
            .cache
            .get_or_fetch::<UsgsResponse, _, _>(&cache_key, 300, move || async move {
                fetch_earthquakes(&http, lat, lon, radius_km, min_magnitude, days, limit).await
            })
            .await;

        match result {
            Ok(resp) => Ok(format_output(&resp, lat, lon, radius_km, min_magnitude, days)),
            Err(e) => {
                tracing::warn!("USGS earthquake fetch failed: {}", e);
                Ok(format!(
                    "USGS Earthquake API temporarily unreachable. Try again in a minute.\n(details: {})",
                    e
                ))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP fetch
// ---------------------------------------------------------------------------

async fn fetch_earthquakes(
    http: &reqwest::Client,
    lat: f64,
    lon: f64,
    radius_km: f64,
    min_magnitude: f64,
    days: u32,
    limit: u32,
) -> Result<UsgsResponse> {
    let now = crate::util::now_pacific();
    let start = now - chrono::Duration::days(days as i64);
    let start_str = start.format("%Y-%m-%dT%H:%M:%S").to_string();

    let url = format!(
        "{}?format=geojson&latitude={}&longitude={}&maxradiuskm={}&minmagnitude={}&starttime={}&limit={}&orderby=time",
        USGS_BASE, lat, lon, radius_km, min_magnitude, start_str, limit
    );

    let resp = http
        .get(&url)
        .send()
        .await
        .context("USGS HTTP request failed")?;

    let status = resp.status();
    if status == reqwest::StatusCode::BAD_REQUEST {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("USGS returned 400 Bad Request: {}", body);
    }
    if !status.is_success() {
        anyhow::bail!("USGS returned HTTP {}", status);
    }

    let body = resp
        .text()
        .await
        .context("reading USGS response body")?;

    serde_json::from_str::<UsgsResponse>(&body).context("parsing USGS GeoJSON response")
}

// ---------------------------------------------------------------------------
// Magnitude classification
// ---------------------------------------------------------------------------

fn mag_label(mag: f64) -> &'static str {
    if mag < 2.0 {
        "micro"
    } else if mag < 3.0 {
        "minor"
    } else if mag < 4.0 {
        "light"
    } else if mag < 5.0 {
        "moderate"
    } else if mag < 6.0 {
        "strong"
    } else if mag < 7.0 {
        "major"
    } else {
        "great"
    }
}

// ---------------------------------------------------------------------------
// Time formatting
// ---------------------------------------------------------------------------

fn format_epoch_pacific(millis: i64) -> String {
    let dt = DateTime::from_timestamp(millis / 1000, 0)
        .unwrap_or_default()
        .with_timezone(&chrono_tz::US::Pacific);
    dt.format("%b %-d %-I:%M %p").to_string()
}

// ---------------------------------------------------------------------------
// Output formatting
// ---------------------------------------------------------------------------

fn format_output(
    resp: &UsgsResponse,
    _lat: f64,
    _lon: f64,
    radius_km: f64,
    min_magnitude: f64,
    days: u32,
) -> String {
    let mut out = String::new();

    let _ = writeln!(
        out,
        "# Recent Earthquakes — Santa Cruz ({} km radius)\n",
        radius_km
    );

    let day_word = if days == 1 { "day" } else { "days" };
    let _ = writeln!(
        out,
        "_{} events above M{} in the last {} {}_\n",
        resp.metadata.count, min_magnitude, days, day_word
    );

    if resp.features.is_empty() {
        let _ = writeln!(
            out,
            "No earthquakes recorded in this area for the selected time period.\n"
        );
    } else {
        let _ = writeln!(out, "| Time | Mag | Depth | Location | Felt |");
        let _ = writeln!(out, "|---|---|---|---|---|");

        for feature in &resp.features {
            let props = &feature.properties;

            let time_str = props
                .time
                .map(format_epoch_pacific)
                .unwrap_or_else(|| "—".to_string());

            let mag = props.mag.unwrap_or(0.0);
            let label = mag_label(mag);
            let mag_str = if mag >= 2.5 {
                format!("**M{:.1}** ({})", mag, label)
            } else {
                format!("M{:.1} ({})", mag, label)
            };

            let depth = feature
                .geometry
                .coordinates
                .get(2)
                .copied()
                .unwrap_or(0.0);

            let location = props
                .place
                .as_deref()
                .unwrap_or("Unknown location");

            let felt_str = match props.felt {
                Some(n) if n > 0 => format!("{} reports", n),
                _ => "—".to_string(),
            };

            let _ = writeln!(
                out,
                "| {} | {} | {:.1} km | {} | {} |",
                time_str, mag_str, depth, location, felt_str
            );
        }
    }

    let _ = write!(
        out,
        "\n## Context\n\
         Santa Cruz sits near the San Andreas Fault. The 1989 Loma Prieta earthquake (M6.9) \
         epicenter was 15 km NE of Santa Cruz. Micro-earthquakes (M < 2.0) are common and \
         rarely felt.\n"
    );

    let now = crate::util::now_pacific();
    let _ = write!(
        out,
        "\n_Source: USGS Earthquake Hazards Program. Last updated: {}_\n",
        now.format("%-I:%M %p")
    );

    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mag_label_ranges() {
        assert_eq!(mag_label(0.5), "micro");
        assert_eq!(mag_label(1.9), "micro");
        assert_eq!(mag_label(2.0), "minor");
        assert_eq!(mag_label(2.9), "minor");
        assert_eq!(mag_label(3.0), "light");
        assert_eq!(mag_label(3.9), "light");
        assert_eq!(mag_label(4.0), "moderate");
        assert_eq!(mag_label(4.9), "moderate");
        assert_eq!(mag_label(5.0), "strong");
        assert_eq!(mag_label(5.9), "strong");
        assert_eq!(mag_label(6.0), "major");
        assert_eq!(mag_label(6.9), "major");
        assert_eq!(mag_label(7.0), "great");
        assert_eq!(mag_label(9.5), "great");
    }

    #[test]
    fn parse_usgs_response() {
        let json = r#"{
            "type": "FeatureCollection",
            "metadata": { "count": 2, "title": "test" },
            "features": [
                {
                    "type": "Feature",
                    "properties": {
                        "mag": 2.7,
                        "place": "16 km E of Seven Trees, CA",
                        "time": 1737729123000,
                        "type": "earthquake",
                        "title": "M 2.7 - 16 km E of Seven Trees, CA",
                        "felt": 49,
                        "sig": 112,
                        "alert": null,
                        "tsunami": 0,
                        "status": "reviewed",
                        "magType": "ml"
                    },
                    "geometry": {
                        "type": "Point",
                        "coordinates": [-121.7308, 37.2741, 6.7]
                    }
                },
                {
                    "type": "Feature",
                    "properties": {
                        "mag": 1.3,
                        "place": "10 km NNE of San Juan Bautista, CA",
                        "time": 1737642900000,
                        "type": "earthquake",
                        "title": "M 1.3 - 10 km NNE of San Juan Bautista, CA",
                        "felt": null,
                        "sig": 26,
                        "alert": null,
                        "tsunami": 0,
                        "status": "automatic",
                        "magType": "md"
                    },
                    "geometry": {
                        "type": "Point",
                        "coordinates": [-121.4894, 36.9122, 4.4]
                    }
                }
            ]
        }"#;

        let resp: UsgsResponse = serde_json::from_str(json).expect("should parse");
        assert_eq!(resp.metadata.count, 2);
        assert_eq!(resp.features.len(), 2);

        let first = &resp.features[0];
        assert_eq!(first.properties.mag, Some(2.7));
        assert_eq!(
            first.properties.place.as_deref(),
            Some("16 km E of Seven Trees, CA")
        );
        assert_eq!(first.properties.felt, Some(49));
        assert_eq!(first.properties.tsunami, Some(0));
        assert!((first.geometry.coordinates[2] - 6.7).abs() < 0.01);

        let second = &resp.features[1];
        assert_eq!(second.properties.mag, Some(1.3));
        assert_eq!(second.properties.felt, None);
        assert_eq!(
            second.properties.mag_type.as_deref(),
            Some("md")
        );
    }

    #[test]
    fn format_output_renders() {
        let resp = UsgsResponse {
            metadata: UsgsMetadata {
                count: 1,
                title: Some("test".to_string()),
            },
            features: vec![UsgsFeature {
                properties: UsgsProperties {
                    mag: Some(2.7),
                    place: Some("16 km E of Seven Trees, CA".to_string()),
                    time: Some(1737729123000),
                    event_type: Some("earthquake".to_string()),
                    title: Some("M 2.7 - 16 km E of Seven Trees, CA".to_string()),
                    felt: Some(49),
                    sig: Some(112),
                    alert: None,
                    tsunami: Some(0),
                    status: Some("reviewed".to_string()),
                    mag_type: Some("ml".to_string()),
                },
                geometry: UsgsGeometry {
                    coordinates: vec![-121.7308, 37.2741, 6.7],
                },
            }],
        };

        let output = format_output(&resp, DEFAULT_LAT, DEFAULT_LON, 50.0, 1.0, 7);

        assert!(output.contains("# Recent Earthquakes"));
        assert!(output.contains("50 km radius"));
        assert!(output.contains("1 events above M1"));
        assert!(output.contains("| Time | Mag | Depth | Location | Felt |"));
        assert!(output.contains("**M2.7** (minor)"));
        assert!(output.contains("6.7 km"));
        assert!(output.contains("16 km E of Seven Trees, CA"));
        assert!(output.contains("49 reports"));
        assert!(output.contains("## Context"));
        assert!(output.contains("San Andreas Fault"));
        assert!(output.contains("USGS Earthquake Hazards Program"));
    }

    #[test]
    fn format_epoch_pacific_converts() {
        // 1737729123000 ms = 2025-01-24 06:52:03 UTC
        // Pacific (PST = UTC-8) => Jan 23 10:52 PM
        let formatted = format_epoch_pacific(1737729123000);
        assert!(
            formatted.contains("Jan"),
            "expected month 'Jan', got: {}",
            formatted
        );
        assert!(
            formatted.contains("23") || formatted.contains("24"),
            "expected day 23 or 24, got: {}",
            formatted
        );
    }

    #[test]
    fn empty_features_message() {
        let resp = UsgsResponse {
            metadata: UsgsMetadata {
                count: 0,
                title: Some("test".to_string()),
            },
            features: vec![],
        };

        let output = format_output(&resp, DEFAULT_LAT, DEFAULT_LON, 50.0, 1.0, 7);

        assert!(output.contains("# Recent Earthquakes"));
        assert!(output.contains("0 events"));
        assert!(output.contains("No earthquakes recorded"));
        assert!(output.contains("## Context"));
        assert!(output.contains("USGS Earthquake Hazards Program"));
    }
}
