//! Air quality forecast via Open-Meteo (PM2.5, PM10, pollen).
//!
//! Complements the `air_quality` module (EPA AirNow, current AQI) by providing
//! hourly forecasts from the free Open-Meteo air quality API.

use std::fmt::Write;
use std::sync::Arc;

use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cache::CacheStore;
use crate::util::now_pacific;

const DEFAULT_LAT: f64 = 36.9741;
const DEFAULT_LON: f64 = -122.0308;
const DEFAULT_DAYS: u32 = 2;
const CACHE_TTL: u64 = 1800;

// ───── request ─────

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AirForecastRequest {
    /// Latitude (default 36.9741, Santa Cruz).
    pub lat: Option<f64>,
    /// Longitude (default -122.0308, Santa Cruz).
    pub lon: Option<f64>,
    /// Forecast days (1-5, default 2).
    pub days: Option<u32>,
}

// ───── service ─────

pub struct AirForecastService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl AirForecastService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    pub async fn get_air_quality_forecast(
        &self,
        lat: Option<f64>,
        lon: Option<f64>,
        days: Option<u32>,
    ) -> Result<String> {
        let lat = lat.unwrap_or(DEFAULT_LAT);
        let lon = lon.unwrap_or(DEFAULT_LON);
        let days = days.unwrap_or(DEFAULT_DAYS).clamp(1, 5);

        let key = format!("air_forecast:{:.3}:{:.3}:{}", lat, lon, days);
        let http = self.http.clone();

        let resp = self
            .cache
            .get_or_fetch::<AirQualityResponse, _, _>(&key, CACHE_TTL, move || async move {
                fetch_air_quality(&http, lat, lon, days).await
            })
            .await?;

        Ok(format_output(&resp))
    }
}

// ───── API fetch ─────

async fn fetch_air_quality(
    http: &reqwest::Client,
    lat: f64,
    lon: f64,
    days: u32,
) -> Result<AirQualityResponse> {
    let url = format!(
        "https://air-quality-api.open-meteo.com/v1/air-quality\
         ?latitude={lat}&longitude={lon}\
         &current=us_aqi,pm2_5,pm10,alder_pollen,birch_pollen,grass_pollen,mugwort_pollen,olive_pollen,ragweed_pollen\
         &hourly=us_aqi,pm2_5,pm10,alder_pollen,birch_pollen,grass_pollen,mugwort_pollen,olive_pollen,ragweed_pollen\
         &timezone=America%2FLos_Angeles\
         &forecast_days={days}"
    );

    let resp = http
        .get(&url)
        .send()
        .await
        .context("Open-Meteo air quality HTTP request failed")?;

    if !resp.status().is_success() {
        anyhow::bail!("Open-Meteo air quality returned HTTP {}", resp.status());
    }

    resp.json::<AirQualityResponse>()
        .await
        .context("parsing Open-Meteo air quality JSON")
}

// ───── serde types ─────

#[derive(Debug, Deserialize, Serialize, Clone)]
struct AirQualityResponse {
    latitude: f64,
    longitude: f64,
    #[serde(default)]
    current: Option<AirQualityCurrent>,
    #[serde(default)]
    hourly: Option<AirQualityHourly>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct AirQualityCurrent {
    time: String,
    us_aqi: Option<f64>,
    pm2_5: Option<f64>,
    pm10: Option<f64>,
    alder_pollen: Option<f64>,
    birch_pollen: Option<f64>,
    grass_pollen: Option<f64>,
    mugwort_pollen: Option<f64>,
    olive_pollen: Option<f64>,
    ragweed_pollen: Option<f64>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct AirQualityHourly {
    time: Vec<String>,
    #[serde(default)]
    us_aqi: Vec<Option<f64>>,
    #[serde(default)]
    pm2_5: Vec<Option<f64>>,
    #[serde(default)]
    pm10: Vec<Option<f64>>,
    #[serde(default)]
    alder_pollen: Vec<Option<f64>>,
    #[serde(default)]
    birch_pollen: Vec<Option<f64>>,
    #[serde(default)]
    grass_pollen: Vec<Option<f64>>,
    #[serde(default)]
    mugwort_pollen: Vec<Option<f64>>,
    #[serde(default)]
    olive_pollen: Vec<Option<f64>>,
    #[serde(default)]
    ragweed_pollen: Vec<Option<f64>>,
}

// ───── helpers ─────

fn aqi_category(aqi: f64) -> (&'static str, &'static str) {
    match aqi as u32 {
        0..=50 => ("Good", "\u{1f7e2}"),
        51..=100 => ("Moderate", "\u{1f7e1}"),
        101..=150 => ("Unhealthy for Sensitive Groups", "\u{1f7e0}"),
        151..=200 => ("Unhealthy", "\u{1f534}"),
        201..=300 => ("Very Unhealthy", "\u{1f7e3}"),
        _ => ("Hazardous", "\u{26ab}"),
    }
}

/// Format an hourly time string like "2026-04-27T14:00" into "2 PM".
fn format_hour(time_str: &str) -> String {
    chrono::NaiveDateTime::parse_from_str(time_str, "%Y-%m-%dT%H:%M")
        .map(|dt| dt.format("%-I %p").to_string())
        .unwrap_or_else(|_| time_str.to_string())
}

fn fmt_val(v: Option<f64>) -> String {
    v.map(|x| format!("{:.1}", x))
        .unwrap_or_else(|| "\u{2014}".to_string())
}

/// Returns true if any pollen value in the hourly data is non-null.
fn has_pollen_data(hourly: &AirQualityHourly) -> bool {
    let vecs: [&[Option<f64>]; 6] = [
        &hourly.alder_pollen,
        &hourly.birch_pollen,
        &hourly.grass_pollen,
        &hourly.mugwort_pollen,
        &hourly.olive_pollen,
        &hourly.ragweed_pollen,
    ];
    vecs.iter().any(|v| v.iter().any(|x| x.is_some()))
}

/// Returns true if any current pollen value is non-null.
fn has_current_pollen(current: &AirQualityCurrent) -> bool {
    [
        current.alder_pollen,
        current.birch_pollen,
        current.grass_pollen,
        current.mugwort_pollen,
        current.olive_pollen,
        current.ragweed_pollen,
    ]
    .iter()
    .any(|v| v.is_some())
}

// ───── formatting ─────

fn format_output(resp: &AirQualityResponse) -> String {
    let mut out = String::new();
    writeln!(out, "# Air Quality Forecast \u{2014} Santa Cruz\n").unwrap();

    // Current conditions
    if let Some(current) = &resp.current {
        writeln!(out, "## Current Conditions").unwrap();
        if let Some(aqi) = current.us_aqi {
            let (cat, icon) = aqi_category(aqi);
            writeln!(out, "- {} **US AQI**: {} ({})", icon, aqi as u32, cat).unwrap();
        }
        if let Some(pm25) = current.pm2_5 {
            writeln!(out, "- **PM2.5**: {:.1} \u{03bc}g/m\u{00b3}", pm25).unwrap();
        }
        if let Some(pm10) = current.pm10 {
            writeln!(out, "- **PM10**: {:.1} \u{03bc}g/m\u{00b3}", pm10).unwrap();
        }
        writeln!(out).unwrap();
    }

    // Hourly forecast
    if let Some(hourly) = &resp.hourly {
        let now_hour = now_pacific().format("%Y-%m-%dT%H:00").to_string();
        let start = hourly
            .time
            .iter()
            .position(|t| t.as_str() >= now_hour.as_str())
            .unwrap_or(0);

        writeln!(out, "## Hourly Forecast").unwrap();
        writeln!(out, "| Time | AQI | PM2.5 | PM10 |").unwrap();
        writeln!(out, "|---|---|---|---|").unwrap();

        for i in start..(start + 12).min(hourly.time.len()) {
            let time_label = hourly
                .time
                .get(i)
                .map(|s| format_hour(s))
                .unwrap_or_else(|| "\u{2014}".to_string());

            let aqi_cell = hourly
                .us_aqi
                .get(i)
                .copied()
                .flatten()
                .map(|v| {
                    let (_, icon) = aqi_category(v);
                    format!("{} {}", icon, v as u32)
                })
                .unwrap_or_else(|| "\u{2014}".to_string());

            let pm25 = fmt_val(hourly.pm2_5.get(i).copied().flatten());
            let pm10 = fmt_val(hourly.pm10.get(i).copied().flatten());

            writeln!(out, "| {} | {} | {} | {} |", time_label, aqi_cell, pm25, pm10).unwrap();
        }
        writeln!(out).unwrap();

        // Pollen (only if data exists)
        let show_pollen = has_pollen_data(hourly)
            || resp.current.as_ref().map_or(false, |c| has_current_pollen(c));

        if show_pollen {
            writeln!(out, "## Pollen Forecast").unwrap();
            writeln!(
                out,
                "| Time | Grass | Birch | Alder | Ragweed | Olive | Mugwort |"
            )
            .unwrap();
            writeln!(out, "|---|---|---|---|---|---|---|").unwrap();

            for i in start..(start + 12).min(hourly.time.len()) {
                let time_label = hourly
                    .time
                    .get(i)
                    .map(|s| format_hour(s))
                    .unwrap_or_else(|| "\u{2014}".to_string());

                let grass = fmt_val(hourly.grass_pollen.get(i).copied().flatten());
                let birch = fmt_val(hourly.birch_pollen.get(i).copied().flatten());
                let alder = fmt_val(hourly.alder_pollen.get(i).copied().flatten());
                let ragweed = fmt_val(hourly.ragweed_pollen.get(i).copied().flatten());
                let olive = fmt_val(hourly.olive_pollen.get(i).copied().flatten());
                let mugwort = fmt_val(hourly.mugwort_pollen.get(i).copied().flatten());

                writeln!(
                    out,
                    "| {} | {} | {} | {} | {} | {} | {} |",
                    time_label, grass, birch, alder, ragweed, olive, mugwort
                )
                .unwrap();
            }
            writeln!(out).unwrap();
        }
    }

    let now = now_pacific();
    writeln!(
        out,
        "_Source: Open-Meteo Air Quality API. Last updated: {}_",
        now.format("%-I:%M %p")
    )
    .unwrap();

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aqi_category_thresholds() {
        assert_eq!(aqi_category(0.0), ("Good", "\u{1f7e2}"));
        assert_eq!(aqi_category(50.0), ("Good", "\u{1f7e2}"));
        assert_eq!(aqi_category(51.0), ("Moderate", "\u{1f7e1}"));
        assert_eq!(aqi_category(100.0), ("Moderate", "\u{1f7e1}"));
        assert_eq!(
            aqi_category(101.0),
            ("Unhealthy for Sensitive Groups", "\u{1f7e0}")
        );
        assert_eq!(
            aqi_category(150.0),
            ("Unhealthy for Sensitive Groups", "\u{1f7e0}")
        );
        assert_eq!(aqi_category(151.0), ("Unhealthy", "\u{1f534}"));
        assert_eq!(aqi_category(200.0), ("Unhealthy", "\u{1f534}"));
        assert_eq!(aqi_category(201.0), ("Very Unhealthy", "\u{1f7e3}"));
        assert_eq!(aqi_category(300.0), ("Very Unhealthy", "\u{1f7e3}"));
        assert_eq!(aqi_category(301.0), ("Hazardous", "\u{26ab}"));
        assert_eq!(aqi_category(500.0), ("Hazardous", "\u{26ab}"));
    }

    #[test]
    fn parse_response_with_null_pollen() {
        let json = r#"{
            "latitude": 37.0,
            "longitude": -122.0,
            "current": {
                "time": "2026-04-27T14:00",
                "us_aqi": 62,
                "pm2_5": 12.4,
                "pm10": 17.8,
                "alder_pollen": null,
                "birch_pollen": null,
                "grass_pollen": null,
                "mugwort_pollen": null,
                "olive_pollen": null,
                "ragweed_pollen": null
            },
            "hourly": {
                "time": ["2026-04-27T14:00", "2026-04-27T15:00"],
                "us_aqi": [62, 58],
                "pm2_5": [12.4, 11.2],
                "pm10": [17.8, 16.5],
                "alder_pollen": [null, null],
                "birch_pollen": [null, null],
                "grass_pollen": [null, null],
                "mugwort_pollen": [null, null],
                "olive_pollen": [null, null],
                "ragweed_pollen": [null, null]
            }
        }"#;

        let resp: AirQualityResponse = serde_json::from_str(json).unwrap();
        assert!(resp.current.is_some());
        let current = resp.current.as_ref().unwrap();
        assert_eq!(current.us_aqi, Some(62.0));
        assert_eq!(current.pm2_5, Some(12.4));
        assert!(current.alder_pollen.is_none());
        assert!(current.birch_pollen.is_none());

        let hourly = resp.hourly.as_ref().unwrap();
        assert_eq!(hourly.time.len(), 2);
        assert_eq!(hourly.us_aqi.len(), 2);
        assert!(hourly.grass_pollen.iter().all(|v| v.is_none()));
    }

    #[test]
    fn format_output_renders() {
        let resp = AirQualityResponse {
            latitude: 37.0,
            longitude: -122.0,
            current: Some(AirQualityCurrent {
                time: "2026-04-27T14:00".to_string(),
                us_aqi: Some(62.0),
                pm2_5: Some(12.4),
                pm10: Some(17.8),
                alder_pollen: None,
                birch_pollen: None,
                grass_pollen: None,
                mugwort_pollen: None,
                olive_pollen: None,
                ragweed_pollen: None,
            }),
            hourly: Some(AirQualityHourly {
                time: vec![
                    "2026-04-27T14:00".to_string(),
                    "2026-04-27T15:00".to_string(),
                    "2026-04-27T16:00".to_string(),
                ],
                us_aqi: vec![Some(62.0), Some(58.0), Some(45.0)],
                pm2_5: vec![Some(12.4), Some(11.2), Some(9.8)],
                pm10: vec![Some(17.8), Some(16.5), Some(14.1)],
                alder_pollen: vec![None, None, None],
                birch_pollen: vec![None, None, None],
                grass_pollen: vec![None, None, None],
                mugwort_pollen: vec![None, None, None],
                olive_pollen: vec![None, None, None],
                ragweed_pollen: vec![None, None, None],
            }),
        };

        let out = format_output(&resp);
        assert!(out.contains("# Air Quality Forecast"));
        assert!(out.contains("## Current Conditions"));
        assert!(out.contains("62"));
        assert!(out.contains("Moderate"));
        assert!(out.contains("PM2.5"));
        assert!(out.contains("PM10"));
        assert!(out.contains("## Hourly Forecast"));
        assert!(out.contains("Open-Meteo Air Quality API"));
    }

    #[test]
    fn pollen_hidden_when_null() {
        let resp = AirQualityResponse {
            latitude: 37.0,
            longitude: -122.0,
            current: Some(AirQualityCurrent {
                time: "2026-04-27T14:00".to_string(),
                us_aqi: Some(50.0),
                pm2_5: Some(10.0),
                pm10: Some(15.0),
                alder_pollen: None,
                birch_pollen: None,
                grass_pollen: None,
                mugwort_pollen: None,
                olive_pollen: None,
                ragweed_pollen: None,
            }),
            hourly: Some(AirQualityHourly {
                time: vec!["2026-04-27T14:00".to_string()],
                us_aqi: vec![Some(50.0)],
                pm2_5: vec![Some(10.0)],
                pm10: vec![Some(15.0)],
                alder_pollen: vec![None],
                birch_pollen: vec![None],
                grass_pollen: vec![None],
                mugwort_pollen: vec![None],
                olive_pollen: vec![None],
                ragweed_pollen: vec![None],
            }),
        };

        let out = format_output(&resp);
        assert!(
            !out.contains("Pollen"),
            "Pollen section should be omitted when all values are null"
        );
    }

    #[test]
    fn pollen_shown_when_data_present() {
        let resp = AirQualityResponse {
            latitude: 37.0,
            longitude: -122.0,
            current: None,
            hourly: Some(AirQualityHourly {
                time: vec!["2026-04-27T14:00".to_string()],
                us_aqi: vec![Some(50.0)],
                pm2_5: vec![Some(10.0)],
                pm10: vec![Some(15.0)],
                alder_pollen: vec![None],
                birch_pollen: vec![None],
                grass_pollen: vec![Some(15.0)],
                mugwort_pollen: vec![None],
                olive_pollen: vec![Some(8.0)],
                ragweed_pollen: vec![None],
            }),
        };

        let out = format_output(&resp);
        assert!(
            out.contains("## Pollen Forecast"),
            "Pollen section should appear when data is present"
        );
        assert!(out.contains("15.0"));
        assert!(out.contains("8.0"));
    }

    #[test]
    fn format_hour_display() {
        assert_eq!(format_hour("2026-04-27T14:00"), "2 PM");
        assert_eq!(format_hour("2026-04-27T00:00"), "12 AM");
        assert_eq!(format_hour("2026-04-27T09:00"), "9 AM");
        assert_eq!(format_hour("bad-input"), "bad-input");
    }
}
