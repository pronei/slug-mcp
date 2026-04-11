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
    resp.json::<MarineResponse>()
        .await
        .context("parsing Open-Meteo marine response")
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
    resp.json::<ForecastResponse>()
        .await
        .context("parsing Open-Meteo forecast response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_marine_current() {
        let json = r#"{
            "latitude": 36.96,
            "longitude": -122.04,
            "current": {
                "time": "2026-04-10T17:00",
                "wave_height": 1.32,
                "wave_direction": 225.0,
                "wave_period": 10.1,
                "swell_wave_height": 0.78,
                "swell_wave_period": 13.7
            }
        }"#;
        let parsed: MarineResponse = serde_json::from_str(json).unwrap();
        let current = parsed.current.unwrap();
        assert_eq!(current.wave_height, Some(1.32));
        assert_eq!(current.swell_wave_period, Some(13.7));
    }

    #[test]
    fn parse_forecast_current_wind() {
        let json = r#"{
            "current": {
                "time": "2026-04-10T17:00",
                "temperature_2m": 62.3,
                "wind_speed_10m": 9.1,
                "wind_direction_10m": 280.0,
                "wind_gusts_10m": 14.2
            }
        }"#;
        let parsed: ForecastResponse = serde_json::from_str(json).unwrap();
        let current = parsed.current.unwrap();
        assert_eq!(current.wind_speed_10m, Some(9.1));
        assert_eq!(current.wind_direction_10m, Some(280.0));
    }
}
