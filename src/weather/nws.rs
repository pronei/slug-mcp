//! NOAA National Weather Service (NWS) API client.
//!
//! Public, no-auth JSON API. NWS requires a non-empty `User-Agent` on every
//! request; the shared `reqwest::Client` in `main.rs` sets one.
//!
//! Docs: <https://www.weather.gov/documentation/services-web-api>

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const NWS_BASE: &str = "https://api.weather.gov";

/// Santa Cruz downtown as a default forecast point.
pub const SC_DEFAULT_LAT: f64 = 36.9741;
pub const SC_DEFAULT_LON: f64 = -122.0308;

/// Public forecast zones covering Santa Cruz city/county and the mountains.
///
/// CAZ529 = Northern Monterey Bay (coastal); CAZ512 = Santa Cruz Mountains.
/// Verified against `/points/` lookups for downtown Santa Cruz and Zayante.
/// These codes feed the `/alerts/active?zone=...` endpoint.
pub const ZONE_COASTAL: &str = "CAZ529";
pub const ZONE_MOUNTAINS: &str = "CAZ512";

// ───── /points/{lat},{lon} ─────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PointResponse {
    pub properties: PointProperties,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PointProperties {
    pub forecast: String,
    #[serde(rename = "forecastHourly")]
    pub forecast_hourly: Option<String>,
    #[serde(rename = "gridId")]
    pub grid_id: String,
    #[serde(rename = "gridX")]
    pub grid_x: u32,
    #[serde(rename = "gridY")]
    pub grid_y: u32,
}

/// Look up the forecast URL + gridpoint for a lat/lon.
pub async fn get_point(http: &reqwest::Client, lat: f64, lon: f64) -> Result<PointResponse> {
    let url = format!("{}/points/{:.4},{:.4}", NWS_BASE, lat, lon);
    let resp = http
        .get(&url)
        .header("Accept", "application/geo+json")
        .send()
        .await
        .with_context(|| format!("GET {}", url))?;
    if !resp.status().is_success() {
        anyhow::bail!("NWS /points returned HTTP {}", resp.status());
    }
    resp.json::<PointResponse>()
        .await
        .context("parsing NWS /points response")
}

// ───── forecast endpoint ─────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ForecastResponse {
    pub properties: ForecastProperties,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ForecastProperties {
    pub periods: Vec<ForecastPeriod>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ForecastPeriod {
    pub name: String,
    #[serde(rename = "startTime")]
    pub start_time: String,
    #[serde(rename = "endTime")]
    pub end_time: String,
    #[serde(rename = "isDaytime")]
    pub is_daytime: bool,
    pub temperature: i32,
    #[serde(rename = "temperatureUnit")]
    pub temperature_unit: String,
    #[serde(rename = "windSpeed", default)]
    pub wind_speed: String,
    #[serde(rename = "windDirection", default)]
    pub wind_direction: String,
    #[serde(rename = "shortForecast", default)]
    pub short_forecast: String,
    #[serde(rename = "detailedForecast", default)]
    pub detailed_forecast: String,
    #[serde(rename = "probabilityOfPrecipitation")]
    #[serde(default)]
    pub probability_of_precipitation: Option<ProbabilityValue>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProbabilityValue {
    pub value: Option<u32>,
}

pub async fn get_forecast(http: &reqwest::Client, forecast_url: &str) -> Result<ForecastResponse> {
    let resp = http
        .get(forecast_url)
        .header("Accept", "application/geo+json")
        .send()
        .await
        .with_context(|| format!("GET {}", forecast_url))?;
    if !resp.status().is_success() {
        anyhow::bail!("NWS forecast returned HTTP {}", resp.status());
    }
    resp.json::<ForecastResponse>()
        .await
        .context("parsing NWS forecast response")
}

// ───── alerts endpoint ─────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AlertsResponse {
    pub features: Vec<AlertFeature>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AlertFeature {
    pub properties: AlertProperties,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AlertProperties {
    #[serde(default)]
    pub event: String,
    #[serde(default)]
    pub severity: String,
    #[serde(default)]
    pub urgency: String,
    #[serde(default)]
    pub headline: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub instruction: Option<String>,
    #[serde(default)]
    pub effective: String,
    #[serde(default)]
    pub expires: String,
    #[serde(rename = "areaDesc", default)]
    pub area_desc: String,
}

pub async fn get_alerts_for_zone(http: &reqwest::Client, zone: &str) -> Result<AlertsResponse> {
    let url = format!("{}/alerts/active?zone={}", NWS_BASE, zone);
    let resp = http
        .get(&url)
        .header("Accept", "application/geo+json")
        .send()
        .await
        .with_context(|| format!("GET {}", url))?;
    if !resp.status().is_success() {
        anyhow::bail!("NWS /alerts returned HTTP {}", resp.status());
    }
    resp.json::<AlertsResponse>()
        .await
        .context("parsing NWS alerts response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_point_response() {
        let json = r#"{
            "properties": {
                "forecast": "https://api.weather.gov/gridpoints/MTR/90,99/forecast",
                "forecastHourly": "https://api.weather.gov/gridpoints/MTR/90,99/forecast/hourly",
                "gridId": "MTR",
                "gridX": 90,
                "gridY": 99
            }
        }"#;
        let parsed: PointResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.properties.grid_id, "MTR");
        assert_eq!(parsed.properties.grid_x, 90);
        assert!(parsed.properties.forecast.contains("/gridpoints/"));
    }

    #[test]
    fn parse_forecast_period() {
        let json = r#"{
            "name": "Tonight",
            "startTime": "2026-04-10T18:00:00-07:00",
            "endTime": "2026-04-11T06:00:00-07:00",
            "isDaytime": false,
            "temperature": 48,
            "temperatureUnit": "F",
            "windSpeed": "5 to 10 mph",
            "windDirection": "NW",
            "shortForecast": "Partly Cloudy",
            "detailedForecast": "Partly cloudy, low around 48.",
            "probabilityOfPrecipitation": { "value": 20 }
        }"#;
        let parsed: ForecastPeriod = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.name, "Tonight");
        assert_eq!(parsed.temperature, 48);
        assert_eq!(
            parsed.probability_of_precipitation.unwrap().value,
            Some(20)
        );
    }

    #[test]
    fn parse_alerts_empty() {
        let json = r#"{"features": []}"#;
        let parsed: AlertsResponse = serde_json::from_str(json).unwrap();
        assert!(parsed.features.is_empty());
    }
}
