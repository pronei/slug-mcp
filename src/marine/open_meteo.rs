//! Open-Meteo API client.
//!
//! License: Open-Meteo data is free for non-commercial use under the terms at
//! <https://open-meteo.com/en/terms>. This server is a UCSC student project,
//! which qualifies. If the project ever becomes commercial, review the terms
//! and migrate to a paid tier.
//!
//! No auth required. Two endpoints:
//! - Marine forecast: <https://marine-api.open-meteo.com/v1/marine>
//! - Atmospheric forecast: <https://api.open-meteo.com/v1/forecast>

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const MARINE_BASE: &str = "https://marine-api.open-meteo.com/v1/marine";
pub const FORECAST_BASE: &str = "https://api.open-meteo.com/v1/forecast";

/// Open-Meteo signals errors as `{"error":true,"reason":"..."}` — sometimes
/// with HTTP 200 — which would otherwise parse as an empty/invalid response.
fn open_meteo_error_reason(body: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Envelope {
        error: bool,
        #[serde(default)]
        reason: Option<String>,
    }
    match serde_json::from_str::<Envelope>(body) {
        Ok(e) if e.error => Some(e.reason.unwrap_or_else(|| "no reason given".to_string())),
        _ => None,
    }
}

// ───── marine ─────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MarineResponse {
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default)]
    pub current_units: Option<serde_json::Value>,
    #[serde(default)]
    pub current: Option<MarineCurrent>,
    #[serde(default)]
    pub hourly_units: Option<serde_json::Value>,
    #[serde(default)]
    pub hourly: Option<MarineHourly>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MarineCurrent {
    pub time: String,
    #[serde(default)]
    pub wave_height: Option<f64>,
    #[serde(default)]
    pub wave_direction: Option<f64>,
    #[serde(default)]
    pub wave_period: Option<f64>,
    #[serde(default)]
    pub swell_wave_height: Option<f64>,
    #[serde(default)]
    pub swell_wave_direction: Option<f64>,
    #[serde(default)]
    pub swell_wave_period: Option<f64>,
    #[serde(default)]
    pub wind_wave_height: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MarineHourly {
    pub time: Vec<String>,
    #[serde(default)]
    pub wave_height: Vec<Option<f64>>,
    #[serde(default)]
    pub wave_direction: Vec<Option<f64>>,
    #[serde(default)]
    pub wave_period: Vec<Option<f64>>,
    #[serde(default)]
    pub swell_wave_height: Vec<Option<f64>>,
    #[serde(default)]
    pub swell_wave_direction: Vec<Option<f64>>,
    #[serde(default)]
    pub swell_wave_period: Vec<Option<f64>>,
    #[serde(default)]
    pub wind_wave_height: Vec<Option<f64>>,
}

fn parse_marine_body(body: &str) -> Result<MarineResponse> {
    if let Some(reason) = open_meteo_error_reason(body) {
        anyhow::bail!("Open-Meteo marine error: {}", reason);
    }
    serde_json::from_str(body).context("parsing Open-Meteo marine response")
}

pub async fn get_marine(http: &reqwest::Client, lat: f64, lon: f64) -> Result<MarineResponse> {
    let url = format!(
        "{base}?latitude={lat:.4}&longitude={lon:.4}\
         &current=wave_height,wave_direction,wave_period,swell_wave_height,swell_wave_direction,swell_wave_period,wind_wave_height\
         &hourly=wave_height,wave_direction,wave_period,swell_wave_height,swell_wave_direction,swell_wave_period,wind_wave_height\
         &timezone=America%2FLos_Angeles&forecast_days=2",
        base = MARINE_BASE,
        lat = lat,
        lon = lon,
    );
    let resp = http
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {}", url))?;
    if !resp.status().is_success() {
        anyhow::bail!("Open-Meteo marine returned HTTP {}", resp.status());
    }
    let body = resp
        .text()
        .await
        .context("reading Open-Meteo marine response")?;
    parse_marine_body(&body)
}

// ───── atmospheric forecast (wind, temp) ─────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ForecastResponse {
    #[serde(default)]
    pub current: Option<ForecastCurrent>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ForecastCurrent {
    pub time: String,
    #[serde(default)]
    pub temperature_2m: Option<f64>,
    #[serde(default)]
    pub wind_speed_10m: Option<f64>,
    #[serde(default)]
    pub wind_direction_10m: Option<f64>,
    #[serde(default)]
    pub wind_gusts_10m: Option<f64>,
}

fn parse_forecast_body(body: &str) -> Result<ForecastResponse> {
    if let Some(reason) = open_meteo_error_reason(body) {
        anyhow::bail!("Open-Meteo forecast error: {}", reason);
    }
    serde_json::from_str(body).context("parsing Open-Meteo forecast response")
}

pub async fn get_forecast(http: &reqwest::Client, lat: f64, lon: f64) -> Result<ForecastResponse> {
    let url = format!(
        "{base}?latitude={lat:.4}&longitude={lon:.4}\
         &current=temperature_2m,wind_speed_10m,wind_direction_10m,wind_gusts_10m\
         &wind_speed_unit=mph&temperature_unit=fahrenheit&timezone=America%2FLos_Angeles",
        base = FORECAST_BASE,
        lat = lat,
        lon = lon,
    );
    let resp = http
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {}", url))?;
    if !resp.status().is_success() {
        anyhow::bail!("Open-Meteo forecast returned HTTP {}", resp.status());
    }
    let body = resp
        .text()
        .await
        .context("reading Open-Meteo forecast response")?;
    parse_forecast_body(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARINE_FIXTURE: &str = include_str!("fixtures/marine_steamer_lane.json");
    const FORECAST_FIXTURE: &str = include_str!("fixtures/forecast_current.json");

    #[test]
    fn parse_marine_fixture() {
        let parsed = parse_marine_body(MARINE_FIXTURE).unwrap();
        assert!((parsed.latitude - 36.95).abs() < 0.5);
        let current = parsed.current.expect("current block");
        assert!(current.wave_height.is_some());
        assert!(current.swell_wave_period.is_some());
        assert!(current.wind_wave_height.is_some());
        let hourly = parsed.hourly.expect("hourly block");
        assert_eq!(hourly.time.len(), 6);
        assert_eq!(hourly.wave_height.len(), 6);
        assert!(hourly.wave_height[0].is_some());
        assert_eq!(hourly.swell_wave_direction.len(), 6);
    }

    #[test]
    fn parse_forecast_fixture() {
        let parsed = parse_forecast_body(FORECAST_FIXTURE).unwrap();
        let current = parsed.current.expect("current block");
        assert!(current.temperature_2m.is_some());
        assert!(current.wind_speed_10m.is_some());
        assert!(current.wind_direction_10m.is_some());
        assert!(current.wind_gusts_10m.is_some());
    }

    // Open-Meteo can return its error envelope with HTTP 200 — the reason
    // must reach the user instead of a bare serde error.
    #[test]
    fn marine_error_envelope_surfaces_reason() {
        let body = r#"{"error":true,"reason":"Latitude must be in range of -90 to 90"}"#;
        let err = parse_marine_body(body).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("Latitude must be in range"), "got: {}", msg);
    }

    #[test]
    fn forecast_error_envelope_surfaces_reason() {
        let body = r#"{"error":true,"reason":"Cannot initialize WeatherVariable"}"#;
        let err = parse_forecast_body(body).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("Cannot initialize"), "got: {}", msg);
    }

    #[test]
    fn marine_error_envelope_without_reason() {
        let body = r#"{"error":true}"#;
        let err = parse_marine_body(body).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("Open-Meteo"), "got: {}", msg);
    }

    #[test]
    fn marine_empty_hourly_arrays_parse_ok() {
        let body = r#"{
            "latitude": 36.96, "longitude": -122.04,
            "current": null,
            "hourly": {
                "time": [], "wave_height": [], "wave_direction": [],
                "wave_period": [], "swell_wave_height": [],
                "swell_wave_direction": [], "swell_wave_period": [],
                "wind_wave_height": []
            }
        }"#;
        let parsed = parse_marine_body(body).unwrap();
        assert!(parsed.current.is_none());
        assert!(parsed.hourly.unwrap().time.is_empty());
    }

    #[test]
    fn marine_truncated_json_is_err_not_panic() {
        let body = r#"{"latitude":36.96,"longitude":-122.04,"current":{"time":"2026-"#;
        let err = parse_marine_body(body).unwrap_err();
        assert!(format!("{:#}", err).contains("parsing Open-Meteo marine"));
    }

    #[test]
    fn forecast_malformed_json_is_err_not_panic() {
        let err = parse_forecast_body("<html>Bad Gateway</html>").unwrap_err();
        assert!(format!("{:#}", err).contains("parsing Open-Meteo forecast"));
    }

    // Schema drift: value arrays dropped from the hourly block entirely.
    #[test]
    fn marine_hourly_missing_value_arrays_defaults_empty() {
        let body = r#"{
            "latitude": 36.96, "longitude": -122.04,
            "hourly": { "time": ["2026-04-10T00:00", "2026-04-10T01:00"] }
        }"#;
        let parsed = parse_marine_body(body).unwrap();
        let hourly = parsed.hourly.unwrap();
        assert_eq!(hourly.time.len(), 2);
        assert!(hourly.wave_height.is_empty());
    }

    // Schema drift: a number field turning into a string is a parse error,
    // not a panic, and keeps the endpoint context.
    #[test]
    fn marine_string_wave_height_is_err_not_panic() {
        let body = r#"{
            "latitude": 36.96, "longitude": -122.04,
            "current": { "time": "2026-04-10T17:00", "wave_height": "1.32" }
        }"#;
        let err = parse_marine_body(body).unwrap_err();
        assert!(format!("{:#}", err).contains("parsing Open-Meteo marine"));
    }
}
