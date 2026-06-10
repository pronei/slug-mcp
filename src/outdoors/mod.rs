//! Outdoor points of interest via OpenStreetMap Overpass API.
//!
//! Searches for trails, peaks, viewpoints, water/restroom facilities, and
//! parking areas around a given location (default: Santa Cruz).

use std::fmt::Write;
use std::sync::Arc;

use anyhow::{bail, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cache::CacheStore;

// ─── Defaults ───

const DEFAULT_LAT: f64 = 36.9741;
const DEFAULT_LON: f64 = -122.0308;
const DEFAULT_RADIUS: u32 = 5000;
const DEFAULT_LIMIT: u32 = 20;
const MAX_RADIUS: u32 = 25_000;
const MAX_LIMIT: u32 = 50;
const CACHE_TTL: u64 = 86_400; // 24 hours

const OVERPASS_URL: &str = "https://overpass-api.de/api/interpreter";

// ─── Request ───

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OutdoorsRequest {
    /// Category to search: "trails", "peaks", "viewpoints", "water_restrooms", "parking".
    pub category: String,
    /// Latitude (default 36.9741, Santa Cruz).
    pub lat: Option<f64>,
    /// Longitude (default -122.0308, Santa Cruz).
    pub lon: Option<f64>,
    /// Search radius in meters (default 5000, max 25000).
    pub radius_m: Option<u32>,
    /// Max results (default 20, max 50).
    pub limit: Option<u32>,
}

// ─── Overpass response types ───

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OverpassResponse {
    elements: Vec<OverpassElement>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OverpassElement {
    #[serde(rename = "type")]
    osm_type: String,
    id: i64,
    #[serde(default)]
    lat: Option<f64>,
    #[serde(default)]
    lon: Option<f64>,
    #[serde(default)]
    center: Option<OverpassCenter>,
    #[serde(default)]
    tags: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OverpassCenter {
    lat: f64,
    lon: f64,
}

// ─── Parsed feature ───

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Feature {
    name: Option<String>,
    category: String,
    lat: f64,
    lon: f64,
    elevation: Option<String>,
    extra_tags: Vec<(String, String)>,
}

// ─── Service ───

pub struct OutdoorsService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl OutdoorsService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    pub async fn search(&self, req: &OutdoorsRequest) -> Result<String> {
        let category = req.category.trim().to_lowercase();
        validate_category(&category)?;

        let lat = req.lat.unwrap_or(DEFAULT_LAT);
        let lon = req.lon.unwrap_or(DEFAULT_LON);

        if let Some(r) = req.radius_m {
            if r > MAX_RADIUS {
                bail!("radius_m must be at most {} meters", MAX_RADIUS);
            }
        }
        let radius = req.radius_m.unwrap_or(DEFAULT_RADIUS).min(MAX_RADIUS);
        let limit = req.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);

        let cache_key = format!(
            "outdoors:{}:{:.3}:{:.3}:{}:{}",
            category, lat, lon, radius, limit
        );

        let http = self.http.clone();
        let cat = category.clone();
        let features = match self
            .cache
            .get_or_fetch::<Vec<Feature>, _, _>(&cache_key, CACHE_TTL, || async move {
                fetch_and_parse(&http, &cat, lat, lon, radius, limit).await
            })
            .await
        {
            Ok(f) => f,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("busy") || msg.contains("rate-limited") || msg.contains("timed out")
                {
                    return Ok(format!(
                        "Overpass API is temporarily unavailable. Please try again in a minute.\n\
                         _(details: {})_",
                        msg
                    ));
                }
                return Err(e);
            }
        };

        Ok(format_output(&category, &features, lat, lon, radius, limit))
    }
}

// ─── Validation ───

const VALID_CATEGORIES: &[&str] = &["trails", "peaks", "viewpoints", "water_restrooms", "parking"];

fn validate_category(category: &str) -> Result<()> {
    if VALID_CATEGORIES.contains(&category) {
        Ok(())
    } else {
        bail!(
            "Unknown category '{}'. Valid categories: {}",
            category,
            VALID_CATEGORIES.join(", ")
        )
    }
}

// ─── Query templates ───

fn build_query(category: &str, lat: f64, lon: f64, radius: u32, limit: u32) -> String {
    match category {
        "trails" => format!(
            "[out:json][timeout:15];\
             (way[\"highway\"~\"path|footway\"](around:{radius},{lat},{lon});\
             relation[\"route\"=\"hiking\"](around:{radius},{lat},{lon}););\
             out tags center {limit};",
            radius = radius,
            lat = lat,
            lon = lon,
            limit = limit,
        ),
        "peaks" => format!(
            "[out:json][timeout:15];\
             node[\"natural\"=\"peak\"](around:{radius},{lat},{lon});\
             out tags {limit};",
            radius = radius,
            lat = lat,
            lon = lon,
            limit = limit,
        ),
        "viewpoints" => format!(
            "[out:json][timeout:15];\
             node[\"tourism\"=\"viewpoint\"](around:{radius},{lat},{lon});\
             out tags {limit};",
            radius = radius,
            lat = lat,
            lon = lon,
            limit = limit,
        ),
        "water_restrooms" => format!(
            "[out:json][timeout:15];\
             (node[\"amenity\"=\"drinking_water\"](around:{radius},{lat},{lon});\
             node[\"amenity\"=\"toilets\"](around:{radius},{lat},{lon}););\
             out tags {limit};",
            radius = radius,
            lat = lat,
            lon = lon,
            limit = limit,
        ),
        "parking" => format!(
            "[out:json][timeout:15];\
             (node[\"amenity\"=\"parking\"](around:{radius},{lat},{lon});\
             way[\"amenity\"=\"parking\"](around:{radius},{lat},{lon}););\
             out tags center {limit};",
            radius = radius,
            lat = lat,
            lon = lon,
            limit = limit,
        ),
        _ => unreachable!("category already validated"),
    }
}

// ─── Fetch + parse ───

async fn fetch_and_parse(
    http: &reqwest::Client,
    category: &str,
    lat: f64,
    lon: f64,
    radius: u32,
    limit: u32,
) -> Result<Vec<Feature>> {
    let query = build_query(category, lat, lon, radius, limit);

    let resp = http
        .post(OVERPASS_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!("data={}", urlencoding::encode(&query)))
        .send()
        .await?;

    if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        bail!("Overpass API is busy (rate-limited). Please try again in a minute.");
    }

    let body = resp.text().await?;

    // Overpass sometimes returns 200 with an error message in the body
    if body.contains("runtime error:") || body.contains("Dispatcher_Client") {
        bail!("Overpass API server is busy or timed out. Try again shortly or reduce the search radius.");
    }

    let parsed: OverpassResponse = serde_json::from_str(&body)?;

    let mut features: Vec<Feature> = parsed
        .elements
        .iter()
        .filter_map(|el| element_to_feature(el, category))
        .collect();

    // Sort by distance from query point
    features.sort_by(|a, b| {
        let da = haversine_km(lat, lon, a.lat, a.lon);
        let db = haversine_km(lat, lon, b.lat, b.lon);
        da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
    });

    // Respect the limit (Overpass may return fewer, but never more after `out ... N`)
    features.truncate(limit as usize);

    Ok(features)
}

fn element_to_feature(el: &OverpassElement, category: &str) -> Option<Feature> {
    // Resolve coordinates: nodes have top-level lat/lon; ways have center
    let (lat, lon) = if let (Some(lat), Some(lon)) = (el.lat, el.lon) {
        (lat, lon)
    } else if let Some(center) = &el.center {
        (center.lat, center.lon)
    } else {
        return None;
    };

    let tags = el.tags.as_ref();

    let name = tags
        .and_then(|t| t.get("name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let elevation = tags
        .and_then(|t| t.get("ele"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let feature_category = match category {
        "trails" => {
            let highway = tags
                .and_then(|t| t.get("highway"))
                .and_then(|v| v.as_str())
                .unwrap_or("path");
            highway.to_string()
        }
        "peaks" => "peak".to_string(),
        "viewpoints" => "viewpoint".to_string(),
        "water_restrooms" => {
            let amenity = tags
                .and_then(|t| t.get("amenity"))
                .and_then(|v| v.as_str())
                .unwrap_or("facility");
            match amenity {
                "drinking_water" => "drinking water".to_string(),
                "toilets" => "restroom".to_string(),
                other => other.to_string(),
            }
        }
        "parking" => "parking".to_string(),
        _ => category.to_string(),
    };

    // Collect other interesting tags
    let skip_tags = ["name", "ele", "highway", "amenity", "natural", "tourism", "type"];
    let extra_tags: Vec<(String, String)> = tags
        .map(|t| {
            t.iter()
                .filter(|(k, _)| !skip_tags.contains(&k.as_str()))
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    Some(Feature {
        name,
        category: feature_category,
        lat,
        lon,
        elevation,
        extra_tags,
    })
}

// ─── Output formatting ───

fn format_output(
    category: &str,
    features: &[Feature],
    query_lat: f64,
    query_lon: f64,
    radius: u32,
    limit: u32,
) -> String {
    let mut out = String::new();

    let title = match category {
        "trails" => "Trails",
        "peaks" => "Peaks",
        "viewpoints" => "Viewpoints",
        "water_restrooms" => "Water & Restrooms",
        "parking" => "Parking",
        _ => "Results",
    };

    let radius_km = radius as f64 / 1000.0;

    let _ = writeln!(
        out,
        "# {} near Santa Cruz ({:.3}, {:.3})\n",
        title, query_lat, query_lon
    );

    if features.is_empty() {
        let _ = writeln!(
            out,
            "No {} found within {:.1} km of the search point.",
            title.to_lowercase(),
            radius_km
        );
    } else {
        let shown = features.len().min(limit as usize);
        let _ = writeln!(
            out,
            "_Showing {} results within {:.1} km_\n",
            shown, radius_km
        );

        for (i, feature) in features.iter().take(limit as usize).enumerate() {
            let dist = haversine_km(query_lat, query_lon, feature.lat, feature.lon);
            let bearing = bearing_label(query_lat, query_lon, feature.lat, feature.lon);

            let name_str = feature
                .name
                .as_deref()
                .unwrap_or("(unnamed)");

            let mut line = format!("{}. **{}**", i + 1, name_str);

            // Elevation for peaks
            if let Some(ele) = &feature.elevation {
                let _ = write!(line, " ({} m)", ele);
            }

            let _ = write!(line, " \u{2014} {:.1} km {}", dist, bearing);

            // Category detail for trails (footway/path) and water_restrooms
            match category {
                "trails" => {
                    let _ = write!(line, " · {}", feature.category);
                }
                "water_restrooms" => {
                    let _ = write!(line, " · {}", feature.category);
                }
                _ => {}
            }

            // Extra tags of interest
            let interesting: Vec<String> = feature
                .extra_tags
                .iter()
                .filter(|(k, _)| {
                    matches!(
                        k.as_str(),
                        "surface" | "access" | "fee" | "capacity" | "wheelchair" | "operator"
                            | "opening_hours" | "description" | "route"
                    )
                })
                .map(|(k, v)| format!("{}: {}", k, v))
                .collect();
            if !interesting.is_empty() {
                let _ = write!(line, " · {}", interesting.join(", "));
            }

            let _ = writeln!(out, "{}", line);
        }
    }

    let now = crate::util::now_pacific();
    let _ = write!(
        out,
        "\n_Source: OpenStreetMap via Overpass API. Data (c) OSM contributors. Last updated: {}_\n",
        now.format("%-I:%M %p")
    );

    out
}

// ─── Geo helpers ───

/// Haversine distance in kilometers between two lat/lon points.
fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const R: f64 = 6371.0; // Earth radius in km

    let d_lat = (lat2 - lat1).to_radians();
    let d_lon = (lon2 - lon1).to_radians();
    let lat1_r = lat1.to_radians();
    let lat2_r = lat2.to_radians();

    let a = (d_lat / 2.0).sin().powi(2)
        + lat1_r.cos() * lat2_r.cos() * (d_lon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();

    R * c
}

/// 8-point compass bearing from (lat1, lon1) to (lat2, lon2).
fn bearing_label(from_lat: f64, from_lon: f64, to_lat: f64, to_lon: f64) -> &'static str {
    let from_lat_r = from_lat.to_radians();
    let to_lat_r = to_lat.to_radians();
    let d_lon_r = (to_lon - from_lon).to_radians();

    let x = d_lon_r.sin() * to_lat_r.cos();
    let y = from_lat_r.cos() * to_lat_r.sin()
        - from_lat_r.sin() * to_lat_r.cos() * d_lon_r.cos();

    let bearing_deg = x.atan2(y).to_degrees();
    // Normalize to 0..360
    let bearing = ((bearing_deg % 360.0) + 360.0) % 360.0;

    // 8-point compass: each sector is 45 degrees, centered on the cardinal
    if bearing < 22.5 || bearing >= 337.5 {
        "N"
    } else if bearing < 67.5 {
        "NE"
    } else if bearing < 112.5 {
        "E"
    } else if bearing < 157.5 {
        "SE"
    } else if bearing < 202.5 {
        "S"
    } else if bearing < 247.5 {
        "SW"
    } else if bearing < 292.5 {
        "W"
    } else {
        "NW"
    }
}

// ─── Tests ───

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haversine_known_distance() {
        // Santa Cruz city hall to San Francisco city hall: ~96 km
        let sc_lat = 36.974;
        let sc_lon = -122.031;
        let sf_lat = 37.779;
        let sf_lon = -122.419;

        let dist = haversine_km(sc_lat, sc_lon, sf_lat, sf_lon);
        assert!(
            (dist - 96.0).abs() < 5.0,
            "expected ~96 km, got {:.1} km",
            dist
        );

        // Same point should be 0
        assert!(haversine_km(0.0, 0.0, 0.0, 0.0) < 0.001);
    }

    #[test]
    fn bearing_label_directions() {
        let lat = 36.974;
        let lon = -122.031;

        // Due north
        assert_eq!(bearing_label(lat, lon, lat + 1.0, lon), "N");
        // Due south
        assert_eq!(bearing_label(lat, lon, lat - 1.0, lon), "S");
        // Due east
        assert_eq!(bearing_label(lat, lon, lat, lon + 1.0), "E");
        // Due west
        assert_eq!(bearing_label(lat, lon, lat, lon - 1.0), "W");
        // Northeast
        assert_eq!(bearing_label(lat, lon, lat + 1.0, lon + 1.0), "NE");
        // Southeast
        assert_eq!(bearing_label(lat, lon, lat - 1.0, lon + 1.0), "SE");
        // Southwest
        assert_eq!(bearing_label(lat, lon, lat - 1.0, lon - 1.0), "SW");
        // Northwest
        assert_eq!(bearing_label(lat, lon, lat + 1.0, lon - 1.0), "NW");
    }

    #[test]
    fn parse_overpass_elements() {
        let json = r#"{
            "elements": [
                {
                    "type": "node",
                    "id": 358761319,
                    "lat": 37.001,
                    "lon": -122.05,
                    "tags": {
                        "name": "Bald Mountain",
                        "ele": "395",
                        "natural": "peak"
                    }
                },
                {
                    "type": "way",
                    "id": 123456789,
                    "center": { "lat": 36.98, "lon": -122.04 },
                    "tags": {
                        "name": "West Cliff Path",
                        "highway": "footway",
                        "surface": "paved"
                    }
                },
                {
                    "type": "node",
                    "id": 999,
                    "tags": { "natural": "peak" }
                }
            ]
        }"#;

        let resp: OverpassResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.elements.len(), 3);

        // Parse as peaks
        let peak = element_to_feature(&resp.elements[0], "peaks").unwrap();
        assert_eq!(peak.name.as_deref(), Some("Bald Mountain"));
        assert_eq!(peak.elevation.as_deref(), Some("395"));
        assert!((peak.lat - 37.001).abs() < 0.0001);

        // Parse as trail (way with center)
        let trail = element_to_feature(&resp.elements[1], "trails").unwrap();
        assert_eq!(trail.name.as_deref(), Some("West Cliff Path"));
        assert_eq!(trail.category, "footway");
        assert!((trail.lat - 36.98).abs() < 0.0001);

        // Third element has no lat/lon and no center — should be None
        let missing = element_to_feature(&resp.elements[2], "peaks");
        assert!(missing.is_none());
    }

    #[test]
    fn format_output_peaks() {
        let features = vec![
            Feature {
                name: Some("Bald Mountain".to_string()),
                category: "peak".to_string(),
                lat: 37.05,
                lon: -122.08,
                elevation: Some("395".to_string()),
                extra_tags: vec![],
            },
            Feature {
                name: Some("La Corona".to_string()),
                category: "peak".to_string(),
                lat: 37.0,
                lon: -122.03,
                elevation: Some("137".to_string()),
                extra_tags: vec![],
            },
        ];

        let output = format_output("peaks", &features, 36.974, -122.031, 20000, 20);

        assert!(output.contains("# Peaks near Santa Cruz"));
        assert!(output.contains("**Bald Mountain** (395 m)"));
        assert!(output.contains("**La Corona** (137 m)"));
        assert!(output.contains("Showing 2 results"));
        assert!(output.contains("OpenStreetMap via Overpass API"));
    }

    #[test]
    fn invalid_category_rejected() {
        let result = validate_category("swimming_pools");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Unknown category"));
        assert!(err_msg.contains("swimming_pools"));

        // Valid categories should pass
        for cat in VALID_CATEGORIES {
            assert!(validate_category(cat).is_ok());
        }
    }
}
