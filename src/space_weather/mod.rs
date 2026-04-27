//! NOAA Space Weather Prediction Center — Kp index, solar wind, and storm scales.
//!
//! Fetches three SWPC JSON endpoints in parallel (Kp index, NOAA scales,
//! solar wind speed) and renders a markdown summary. No API key required.
//!
//! Docs: <https://www.swpc.noaa.gov/products>

use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Arc;

use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cache::CacheStore;

// ---------------------------------------------------------------------------
// API URLs
// ---------------------------------------------------------------------------

const KP_URL: &str = "https://services.swpc.noaa.gov/products/noaa-planetary-k-index.json";
const SCALES_URL: &str = "https://services.swpc.noaa.gov/products/noaa-scales.json";
const SOLAR_WIND_URL: &str = "https://services.swpc.noaa.gov/products/summary/solar-wind-speed.json";

// ---------------------------------------------------------------------------
// Request type (empty — space weather data is global)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpaceWeatherRequest {
    // No parameters — space weather data is global.
    // Included for macro compatibility.
}

// ---------------------------------------------------------------------------
// API response types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KpEntry {
    pub time_tag: String,
    #[serde(rename = "Kp", deserialize_with = "deserialize_f64_from_any")]
    pub kp: f64,
    #[serde(default, deserialize_with = "deserialize_option_i64_from_any")]
    pub a_running: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_option_i64_from_any")]
    pub station_count: Option<i64>,
}

/// Custom deserializer: accept both number and string representations of f64.
fn deserialize_f64_from_any<'de, D>(deserializer: D) -> std::result::Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumOrStr {
        Num(f64),
        Str(String),
    }
    match NumOrStr::deserialize(deserializer)? {
        NumOrStr::Num(n) => Ok(n),
        NumOrStr::Str(s) => s.parse::<f64>().map_err(serde::de::Error::custom),
    }
}

/// Custom deserializer for optional i64 that may arrive as number or string.
fn deserialize_option_i64_from_any<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumOrStr {
        Num(i64),
        Str(String),
        Null,
    }
    match Option::<NumOrStr>::deserialize(deserializer)? {
        None => Ok(None),
        Some(NumOrStr::Null) => Ok(None),
        Some(NumOrStr::Num(n)) => Ok(Some(n)),
        Some(NumOrStr::Str(s)) => s.parse::<i64>().map(Some).map_err(serde::de::Error::custom),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaleValue {
    #[serde(rename = "Scale")]
    pub scale: Option<String>,
    #[serde(rename = "Text")]
    pub text: Option<String>,
    #[serde(rename = "MinorProb")]
    pub minor_prob: Option<String>,
    #[serde(rename = "MajorProb")]
    pub major_prob: Option<String>,
    #[serde(rename = "Prob")]
    pub prob: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScalePeriod {
    #[serde(rename = "DateStamp")]
    pub date_stamp: Option<String>,
    #[serde(rename = "TimeStamp")]
    pub time_stamp: Option<String>,
    #[serde(rename = "R")]
    pub r: Option<ScaleValue>,
    #[serde(rename = "S")]
    pub s: Option<ScaleValue>,
    #[serde(rename = "G")]
    pub g: Option<ScaleValue>,
}

/// The scales response is a JSON object with string keys: "0" (current),
/// "1" (next period), "-1" (previous period).
pub type ScalesResponse = HashMap<String, ScalePeriod>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolarWindResponse {
    #[serde(rename = "WindSpeed")]
    #[serde(deserialize_with = "deserialize_f64_from_any")]
    pub wind_speed: f64,
    #[serde(rename = "TimeStamp")]
    pub time_stamp: Option<String>,
}

// ---------------------------------------------------------------------------
// Cached aggregate
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SpaceWeatherData {
    kp_entries: Option<Vec<KpEntry>>,
    scales: Option<ScalesResponse>,
    solar_wind: Option<SolarWindResponse>,
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

pub struct SpaceWeatherService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl SpaceWeatherService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    pub async fn get_summary(&self) -> Result<String> {
        let key = "space_weather:summary";
        let http = self.http.clone();
        let data = self
            .cache
            .get_or_fetch::<SpaceWeatherData, _, _>(key, 1800, move || async move {
                fetch_all(&http).await
            })
            .await?;

        Ok(format_output(&data))
    }
}

// ---------------------------------------------------------------------------
// Fetch helpers
// ---------------------------------------------------------------------------

async fn fetch_all(http: &reqwest::Client) -> Result<SpaceWeatherData> {
    let kp_fut = fetch_kp(http);
    let scales_fut = fetch_scales(http);
    let wind_fut = fetch_solar_wind(http);

    let (kp_res, scales_res, wind_res) =
        futures_util::future::join3(kp_fut, scales_fut, wind_fut).await;

    // Partial success: include whatever succeeded.
    let kp_entries = match kp_res {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!("SWPC Kp index fetch failed: {}", e);
            None
        }
    };
    let scales = match scales_res {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!("SWPC scales fetch failed: {}", e);
            None
        }
    };
    let solar_wind = match wind_res {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!("SWPC solar wind fetch failed: {}", e);
            None
        }
    };

    Ok(SpaceWeatherData {
        kp_entries,
        scales,
        solar_wind,
    })
}

async fn fetch_kp(http: &reqwest::Client) -> Result<Vec<KpEntry>> {
    let resp = http
        .get(KP_URL)
        .send()
        .await
        .context("SWPC Kp index HTTP request failed")?;
    if !resp.status().is_success() {
        anyhow::bail!("SWPC Kp index returned HTTP {}", resp.status());
    }
    let entries: Vec<KpEntry> = resp.json().await.context("parsing SWPC Kp index JSON")?;
    Ok(entries)
}

async fn fetch_scales(http: &reqwest::Client) -> Result<ScalesResponse> {
    let resp = http
        .get(SCALES_URL)
        .send()
        .await
        .context("SWPC scales HTTP request failed")?;
    if !resp.status().is_success() {
        anyhow::bail!("SWPC scales returned HTTP {}", resp.status());
    }
    let scales: ScalesResponse = resp.json().await.context("parsing SWPC scales JSON")?;
    Ok(scales)
}

async fn fetch_solar_wind(http: &reqwest::Client) -> Result<SolarWindResponse> {
    let resp = http
        .get(SOLAR_WIND_URL)
        .send()
        .await
        .context("SWPC solar wind HTTP request failed")?;
    if !resp.status().is_success() {
        anyhow::bail!("SWPC solar wind returned HTTP {}", resp.status());
    }
    let wind: SolarWindResponse =
        resp.json().await.context("parsing SWPC solar wind JSON")?;
    Ok(wind)
}

// ---------------------------------------------------------------------------
// Kp classification
// ---------------------------------------------------------------------------

fn kp_level(kp: f64) -> &'static str {
    match kp as u32 {
        0..=1 => "Quiet",
        2..=3 => "Unsettled",
        4 => "Active",
        5 => "Minor storm (G1)",
        6 => "Moderate storm (G2)",
        7 => "Strong storm (G3)",
        8 => "Severe storm (G4)",
        _ => "Extreme storm (G5)",
    }
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

fn format_scale_line(label: &str, code: &str, sv: Option<&ScaleValue>) -> String {
    match sv {
        Some(v) => {
            let scale_num = v
                .scale
                .as_deref()
                .unwrap_or("0");
            let text = v
                .text
                .as_deref()
                .unwrap_or("none");
            let text_cap = capitalize(text);

            let mut line = format!(
                "- **{} ({})**: {} ({}{})",
                label, code, text_cap, code, scale_num
            );

            if let Some(mp) = &v.minor_prob {
                let _ = write!(line, " · Minor prob: {}%", mp);
            }
            if let Some(mp) = &v.major_prob {
                let _ = write!(line, " · Major prob: {}%", mp);
            }
            if let Some(p) = &v.prob {
                let _ = write!(line, " · Prob: {}%", p);
            }
            line
        }
        None => format!("- **{} ({})**: unavailable", label, code),
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => {
            let upper: String = c.to_uppercase().collect();
            upper + chars.as_str()
        }
    }
}

fn format_output(data: &SpaceWeatherData) -> String {
    let mut out = String::from("# Space Weather Summary\n\n");

    // --- Current Conditions ---
    out.push_str("## Current Conditions\n");

    // Kp index
    if let Some(entries) = &data.kp_entries {
        if let Some(latest) = entries.last() {
            let stations = latest.station_count.unwrap_or(0);
            let _ = writeln!(
                out,
                "- **Kp index**: {:.1} ({}) · {} stations reporting",
                latest.kp,
                kp_level(latest.kp),
                stations
            );
        } else {
            out.push_str("- **Kp index**: no data\n");
        }
    } else {
        out.push_str("- **Kp index**: unavailable (fetch failed)\n");
    }

    // Solar wind
    if let Some(wind) = &data.solar_wind {
        let _ = writeln!(out, "- **Solar wind**: {:.0} km/s", wind.wind_speed);
    } else {
        out.push_str("- **Solar wind**: unavailable (fetch failed)\n");
    }

    // Scales (G, S, R)
    if let Some(scales) = &data.scales {
        if let Some(current) = scales.get("0") {
            let _ = writeln!(
                out,
                "{}",
                format_scale_line("Geomagnetic storms", "G", current.g.as_ref())
            );
            let _ = writeln!(
                out,
                "{}",
                format_scale_line("Solar radiation", "S", current.s.as_ref())
            );
            let _ = writeln!(
                out,
                "{}",
                format_scale_line("Radio blackouts", "R", current.r.as_ref())
            );
        } else {
            out.push_str("- **Storm scales**: no current-period data\n");
        }
    } else {
        out.push_str("- **Storm scales**: unavailable (fetch failed)\n");
    }

    // --- Kp trend table (last 24h = 8 entries) ---
    if let Some(entries) = &data.kp_entries {
        if entries.len() > 1 {
            out.push_str("\n## Kp Index — Last 24 Hours\n");
            out.push_str("| Time (UTC) | Kp | Level |\n");
            out.push_str("|---|---|---|\n");

            // Take the last 8 entries (24h of 3-hourly data), newest first.
            let start = entries.len().saturating_sub(8);
            let recent: Vec<&KpEntry> = entries[start..].iter().rev().collect();

            for entry in recent {
                // Parse time_tag for display: "2026-04-27T15:00:00" -> "Apr 27 15:00"
                let display_time = format_time_tag(&entry.time_tag);
                let _ = writeln!(
                    out,
                    "| {} | {:.2} | {} |",
                    display_time,
                    entry.kp,
                    kp_level(entry.kp)
                );
            }
        }
    }

    // --- Santa Cruz Notes ---
    out.push_str("\n## Santa Cruz Notes\n");
    if let Some(entries) = &data.kp_entries {
        if let Some(latest) = entries.last() {
            let _ = writeln!(
                out,
                "At latitude 37°N, aurora requires Kp >= 8 (rare). Current Kp: {:.1} — {}.",
                latest.kp,
                if latest.kp >= 8.0 {
                    "aurora may be visible!"
                } else {
                    "no aurora visible"
                }
            );
        } else {
            out.push_str("At latitude 37°N, aurora requires Kp >= 8 (rare). Kp data unavailable.\n");
        }
    } else {
        out.push_str("At latitude 37°N, aurora requires Kp >= 8 (rare). Kp data unavailable.\n");
    }

    let _ = writeln!(
        out,
        "\n_Source: NOAA Space Weather Prediction Center. Last updated: {}_",
        crate::util::now_pacific().format("%-I:%M %p")
    );
    out
}

/// Format a time_tag like "2026-04-27T15:00:00" into "Apr 27 15:00".
fn format_time_tag(time_tag: &str) -> String {
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(time_tag, "%Y-%m-%dT%H:%M:%S") {
        dt.format("%b %d %H:%M").to_string()
    } else if let Ok(dt) =
        chrono::NaiveDateTime::parse_from_str(time_tag, "%Y-%m-%dT%H:%M:%S%.f")
    {
        dt.format("%b %d %H:%M").to_string()
    } else {
        // Fallback: return the raw string
        time_tag.to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kp_level_thresholds() {
        assert_eq!(kp_level(0.0), "Quiet");
        assert_eq!(kp_level(0.5), "Quiet");
        assert_eq!(kp_level(1.0), "Quiet");
        assert_eq!(kp_level(1.99), "Quiet"); // truncates to 1
        assert_eq!(kp_level(2.0), "Unsettled");
        assert_eq!(kp_level(3.9), "Unsettled"); // truncates to 3
        assert_eq!(kp_level(4.0), "Active");
        assert_eq!(kp_level(4.99), "Active"); // truncates to 4
        assert_eq!(kp_level(5.0), "Minor storm (G1)");
        assert_eq!(kp_level(6.0), "Moderate storm (G2)");
        assert_eq!(kp_level(7.0), "Strong storm (G3)");
        assert_eq!(kp_level(8.0), "Severe storm (G4)");
        assert_eq!(kp_level(9.0), "Extreme storm (G5)");
        assert_eq!(kp_level(10.0), "Extreme storm (G5)");
    }

    #[test]
    fn parse_kp_index() {
        let json = r#"[
            {"time_tag": "2026-04-27T12:00:00", "Kp": 1.67, "a_running": 6, "station_count": 8},
            {"time_tag": "2026-04-27T15:00:00", "Kp": 2.33, "a_running": 9, "station_count": 7}
        ]"#;
        let entries: Vec<KpEntry> = serde_json::from_str(json).unwrap();
        assert_eq!(entries.len(), 2);
        assert!((entries[0].kp - 1.67).abs() < 0.001);
        assert_eq!(entries[0].station_count, Some(8));
        assert_eq!(entries[1].time_tag, "2026-04-27T15:00:00");
    }

    #[test]
    fn parse_kp_index_string_values() {
        // The real API may return Kp as a string
        let json = r#"[
            {"time_tag": "2026-04-27T12:00:00", "Kp": "1.67", "a_running": "6", "station_count": "8"}
        ]"#;
        let entries: Vec<KpEntry> = serde_json::from_str(json).unwrap();
        assert_eq!(entries.len(), 1);
        assert!((entries[0].kp - 1.67).abs() < 0.001);
        assert_eq!(entries[0].station_count, Some(8));
    }

    #[test]
    fn parse_scales() {
        let json = r#"{
            "0": {
                "DateStamp": "2026-04-27",
                "TimeStamp": "12:00:00",
                "R": {"Scale": "2", "Text": "moderate", "MinorProb": "70", "MajorProb": "25"},
                "S": {"Scale": null, "Text": null, "Prob": "15"},
                "G": {"Scale": "0", "Text": "none"}
            }
        }"#;
        let scales: ScalesResponse = serde_json::from_str(json).unwrap();
        let current = scales.get("0").unwrap();
        let r = current.r.as_ref().unwrap();
        assert_eq!(r.scale.as_deref(), Some("2"));
        assert_eq!(r.text.as_deref(), Some("moderate"));
        assert_eq!(r.minor_prob.as_deref(), Some("70"));
        assert_eq!(r.major_prob.as_deref(), Some("25"));

        let s = current.s.as_ref().unwrap();
        assert!(s.scale.is_none());
        assert!(s.text.is_none());
        assert_eq!(s.prob.as_deref(), Some("15"));

        let g = current.g.as_ref().unwrap();
        assert_eq!(g.scale.as_deref(), Some("0"));
        assert_eq!(g.text.as_deref(), Some("none"));
    }

    #[test]
    fn format_output_renders() {
        let data = SpaceWeatherData {
            kp_entries: Some(vec![
                KpEntry {
                    time_tag: "2026-04-27T06:00:00".to_string(),
                    kp: 2.0,
                    a_running: Some(7),
                    station_count: Some(8),
                },
                KpEntry {
                    time_tag: "2026-04-27T09:00:00".to_string(),
                    kp: 1.33,
                    a_running: Some(5),
                    station_count: Some(8),
                },
                KpEntry {
                    time_tag: "2026-04-27T12:00:00".to_string(),
                    kp: 1.67,
                    a_running: Some(6),
                    station_count: Some(8),
                },
                KpEntry {
                    time_tag: "2026-04-27T15:00:00".to_string(),
                    kp: 1.67,
                    a_running: Some(6),
                    station_count: Some(8),
                },
            ]),
            scales: Some({
                let mut m = HashMap::new();
                m.insert(
                    "0".to_string(),
                    ScalePeriod {
                        date_stamp: Some("2026-04-27".to_string()),
                        time_stamp: Some("12:00:00".to_string()),
                        r: Some(ScaleValue {
                            scale: Some("2".to_string()),
                            text: Some("moderate".to_string()),
                            minor_prob: Some("70".to_string()),
                            major_prob: Some("25".to_string()),
                            prob: None,
                        }),
                        s: Some(ScaleValue {
                            scale: None,
                            text: None,
                            minor_prob: None,
                            major_prob: None,
                            prob: Some("15".to_string()),
                        }),
                        g: Some(ScaleValue {
                            scale: Some("0".to_string()),
                            text: Some("none".to_string()),
                            minor_prob: None,
                            major_prob: None,
                            prob: None,
                        }),
                    },
                );
                m
            }),
            solar_wind: Some(SolarWindResponse {
                wind_speed: 448.0,
                time_stamp: Some("2026-04-27T20:59:00".to_string()),
            }),
        };

        let out = format_output(&data);

        // Header
        assert!(out.contains("# Space Weather Summary"));
        // Kp
        assert!(out.contains("Kp index"));
        assert!(out.contains("1.7"));
        assert!(out.contains("Quiet"));
        assert!(out.contains("8 stations"));
        // Solar wind
        assert!(out.contains("448 km/s"));
        // Scales
        assert!(out.contains("Geomagnetic storms"));
        assert!(out.contains("G0"));
        assert!(out.contains("None"));
        assert!(out.contains("Radio blackouts"));
        assert!(out.contains("R2"));
        assert!(out.contains("Moderate"));
        assert!(out.contains("Minor prob: 70%"));
        assert!(out.contains("Major prob: 25%"));
        // Trend table
        assert!(out.contains("Last 24 Hours"));
        assert!(out.contains("Apr 27 15:00"));
        assert!(out.contains("1.67"));
        // Santa Cruz note
        assert!(out.contains("no aurora visible"));
        // Source footer
        assert!(out.contains("Source: NOAA Space Weather Prediction Center"));
    }

    #[test]
    fn format_output_partial_failure() {
        // Only solar wind available; Kp and scales failed
        let data = SpaceWeatherData {
            kp_entries: None,
            scales: None,
            solar_wind: Some(SolarWindResponse {
                wind_speed: 350.0,
                time_stamp: None,
            }),
        };
        let out = format_output(&data);
        assert!(out.contains("unavailable (fetch failed)"));
        assert!(out.contains("350 km/s"));
    }

    #[test]
    fn format_time_tag_parsing() {
        assert_eq!(format_time_tag("2026-04-27T15:00:00"), "Apr 27 15:00");
        assert_eq!(format_time_tag("2026-01-01T00:00:00"), "Jan 01 00:00");
        // Fractional seconds fallback
        assert_eq!(format_time_tag("2026-04-27T15:00:00.000"), "Apr 27 15:00");
        // Unparseable — returns raw
        assert_eq!(format_time_tag("garbage"), "garbage");
    }

    #[test]
    fn capitalize_works() {
        assert_eq!(capitalize("moderate"), "Moderate");
        assert_eq!(capitalize("none"), "None");
        assert_eq!(capitalize(""), "");
        assert_eq!(capitalize("A"), "A");
    }
}
