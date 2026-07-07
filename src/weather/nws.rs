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

    // Live captures 2026-07-07 for 36.9741,-122.0308.
    const POINTS_FIXTURE: &str = include_str!("fixtures/points.json");
    const FORECAST_FIXTURE: &str = include_str!("fixtures/forecast.json");
    const ALERTS_FIXTURE: &str = include_str!("fixtures/alerts.json");

    #[test]
    fn parse_points_fixture() {
        let parsed: PointResponse = serde_json::from_str(POINTS_FIXTURE).unwrap();
        assert_eq!(parsed.properties.grid_id, "MTR");
        assert_eq!(parsed.properties.grid_x, 91);
        assert_eq!(parsed.properties.grid_y, 67);
        assert_eq!(
            parsed.properties.forecast,
            "https://api.weather.gov/gridpoints/MTR/91,67/forecast"
        );
        assert!(
            parsed
                .properties
                .forecast_hourly
                .as_deref()
                .unwrap()
                .ends_with("/hourly")
        );
    }

    #[test]
    fn parse_forecast_fixture() {
        let parsed: ForecastResponse = serde_json::from_str(FORECAST_FIXTURE).unwrap();
        let periods = &parsed.properties.periods;
        assert_eq!(periods.len(), 2);
        assert_eq!(periods[0].name, "Today");
        assert_eq!(periods[0].temperature, 74);
        assert_eq!(periods[0].temperature_unit, "F");
        assert_eq!(periods[0].wind_speed, "2 to 12 mph");
        assert_eq!(periods[0].wind_direction, "WNW");
        assert_eq!(periods[0].short_forecast, "Mostly Sunny");
        assert!(periods[0].is_daytime);
        assert_eq!(
            periods[0]
                .probability_of_precipitation
                .as_ref()
                .unwrap()
                .value,
            Some(0)
        );
        assert_eq!(periods[1].name, "Tonight");
        assert_eq!(periods[1].temperature, 55);
        assert!(!periods[1].is_daytime);
    }

    #[test]
    fn parse_alerts_fixture() {
        let parsed: AlertsResponse = serde_json::from_str(ALERTS_FIXTURE).unwrap();
        assert_eq!(parsed.features.len(), 1);
        let props = &parsed.features[0].properties;
        assert_eq!(props.event, "Beach Hazards Statement");
        assert_eq!(props.severity, "Moderate");
        assert_eq!(props.urgency, "Expected");
        assert!(props.headline.contains("Beach Hazards Statement"));
        assert!(props.instruction.is_some());
        assert!(!props.expires.is_empty());
    }

    #[test]
    fn parse_problem_json_as_forecast_errs() {
        // NWS error bodies are RFC 7807 problem+json; the parse must fail
        // cleanly so the service can degrade to its friendly message.
        let body = r#"{
            "correlationId": "abc123",
            "title": "Unexpected Problem",
            "type": "https://api.weather.gov/problems/UnexpectedProblem",
            "status": 500,
            "detail": "An unexpected problem has occurred."
        }"#;
        assert!(serde_json::from_str::<ForecastResponse>(body).is_err());
        assert!(serde_json::from_str::<PointResponse>(body).is_err());
    }

    #[test]
    fn parse_forecast_truncated_errs() {
        let cut = &FORECAST_FIXTURE[..FORECAST_FIXTURE.len() / 2];
        assert!(serde_json::from_str::<ForecastResponse>(cut).is_err());
    }

    #[test]
    fn parse_forecast_missing_periods_errs() {
        assert!(serde_json::from_str::<ForecastResponse>(r#"{"properties": {}}"#).is_err());
    }

    #[test]
    fn parse_forecast_temperature_as_string_errs() {
        // NWS documents temperature as an integer; drift to string should fail
        // parse (service degrades) rather than render garbage.
        let json = r#"{"properties": {"periods": [{
            "name": "Today",
            "startTime": "2026-07-07T06:00:00-07:00",
            "endTime": "2026-07-07T18:00:00-07:00",
            "isDaytime": true,
            "temperature": "74",
            "temperatureUnit": "F"
        }]}}"#;
        assert!(serde_json::from_str::<ForecastResponse>(json).is_err());
    }

    #[test]
    fn parse_forecast_empty_periods_ok() {
        let parsed: ForecastResponse =
            serde_json::from_str(r#"{"properties": {"periods": []}}"#).unwrap();
        assert!(parsed.properties.periods.is_empty());
    }

    #[test]
    fn parse_forecast_null_pop_value_ok() {
        let json = r#"{
            "name": "Tonight",
            "startTime": "2026-07-07T18:00:00-07:00",
            "endTime": "2026-07-08T06:00:00-07:00",
            "isDaytime": false,
            "temperature": 55,
            "temperatureUnit": "F",
            "probabilityOfPrecipitation": {"unitCode": "wmoUnit:percent", "value": null}
        }"#;
        let parsed: ForecastPeriod = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.probability_of_precipitation.unwrap().value, None);
    }

    #[test]
    fn parse_alert_minimal_properties_defaults() {
        let json = r#"{"features": [{"properties": {"event": "Test Alert"}}]}"#;
        let parsed: AlertsResponse = serde_json::from_str(json).unwrap();
        let props = &parsed.features[0].properties;
        assert_eq!(props.event, "Test Alert");
        assert!(props.severity.is_empty());
        assert!(props.instruction.is_none());
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
        assert_eq!(parsed.probability_of_precipitation.unwrap().value, Some(20));
    }

    #[test]
    fn parse_alerts_empty() {
        let json = r#"{"features": []}"#;
        let parsed: AlertsResponse = serde_json::from_str(json).unwrap();
        assert!(parsed.features.is_empty());
    }
}
