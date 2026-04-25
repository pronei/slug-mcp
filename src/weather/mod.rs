//! NOAA NWS weather forecasts and active alerts for Santa Cruz.

pub mod nws;

use std::sync::Arc;

use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::cache::CacheStore;
use nws::{AlertsResponse, ForecastResponse, PointResponse};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WeatherForecastRequest {
    /// Number of forecast periods to return (1-14; NWS returns ~2 per day: morning + night). Default: 7.
    pub periods: Option<u32>,
}

pub struct WeatherService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl WeatherService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    /// Multi-day forecast for downtown Santa Cruz.
    pub async fn get_forecast(&self, periods: u32) -> Result<String> {
        let periods = periods.clamp(1, 14) as usize;

        let point = match self.load_point().await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("NWS /points fetch failed: {}", e);
                return Ok(format!(
                    "⚠ NOAA NWS temporarily unreachable. Try again in a minute.\n(details: {})",
                    e
                ));
            }
        };

        let forecast_key = format!(
            "weather:forecast:{}:{}:{}",
            point.properties.grid_id, point.properties.grid_x, point.properties.grid_y
        );
        let http = &self.http;
        let forecast_url = point.properties.forecast.clone();
        let forecast_result = self
            .cache
            .get_or_fetch::<ForecastResponse, _, _>(&forecast_key, 900, || async move {
                nws::get_forecast(http, &forecast_url).await
            })
            .await;

        match forecast_result {
            Ok(forecast) => Ok(format_forecast(&forecast, periods)),
            Err(e) => {
                tracing::warn!("NWS forecast fetch failed: {}", e);
                Ok(format!(
                    "⚠ NOAA NWS forecast temporarily unreachable. Try again in a minute.\n(details: {})",
                    e
                ))
            }
        }
    }

    /// Active NWS alerts for the coastal + mountain public zones covering Santa Cruz.
    pub async fn get_alerts(&self) -> Result<String> {
        let coastal = self.load_alerts_for_zone(nws::ZONE_COASTAL);
        let mountains = self.load_alerts_for_zone(nws::ZONE_MOUNTAINS);
        let (coastal_res, mountains_res) = futures_util::future::join(coastal, mountains).await;

        let coastal_alerts = coastal_res.unwrap_or_else(|e| {
            tracing::warn!("NWS coastal alerts fetch failed: {}", e);
            AlertsResponse { features: vec![] }
        });
        let mountain_alerts = mountains_res.unwrap_or_else(|e| {
            tracing::warn!("NWS mountain alerts fetch failed: {}", e);
            AlertsResponse { features: vec![] }
        });

        Ok(format_alerts(&coastal_alerts, &mountain_alerts))
    }

    async fn load_point(&self) -> Result<PointResponse> {
        let key = format!(
            "weather:points:{:.4},{:.4}",
            nws::SC_DEFAULT_LAT,
            nws::SC_DEFAULT_LON
        );
        let http = &self.http;
        self.cache
            .get_or_fetch::<PointResponse, _, _>(&key, 86_400, || async move {
                nws::get_point(http, nws::SC_DEFAULT_LAT, nws::SC_DEFAULT_LON).await
            })
            .await
    }

    async fn load_alerts_for_zone(&self, zone: &str) -> Result<AlertsResponse> {
        let key = format!("weather:alerts:{}", zone);
        let http = &self.http;
        let zone_owned = zone.to_string();
        self.cache
            .get_or_fetch::<AlertsResponse, _, _>(&key, 300, || async move {
                nws::get_alerts_for_zone(http, &zone_owned).await
            })
            .await
    }
}

fn format_forecast(forecast: &ForecastResponse, max_periods: usize) -> String {
    let mut out = String::from("# Santa Cruz Weather Forecast\n\n");

    if forecast.properties.periods.is_empty() {
        out.push_str("No forecast periods returned by NWS.\n");
        return out;
    }

    for period in forecast.properties.periods.iter().take(max_periods) {
        let pop = period
            .probability_of_precipitation
            .as_ref()
            .and_then(|p| p.value)
            .map(|v| format!(" · {}% precip", v))
            .unwrap_or_default();

        out.push_str(&format!(
            "**{}** — {}°{} · {} {}{}\n",
            period.name,
            period.temperature,
            period.temperature_unit,
            period.wind_direction,
            period.wind_speed,
            pop
        ));
        if !period.short_forecast.is_empty() {
            out.push_str(&format!("  {}\n", period.short_forecast));
        }
        if !period.detailed_forecast.is_empty() && period.detailed_forecast != period.short_forecast
        {
            out.push_str(&format!("  {}\n", period.detailed_forecast));
        }
        out.push('\n');
    }

    out.push_str(&format!(
        "_Source: NOAA NWS. Last updated: {}_\n",
        crate::util::now_pacific().format("%-I:%M %p")
    ));
    out
}

fn format_alerts(coastal: &AlertsResponse, mountains: &AlertsResponse) -> String {
    let total = coastal.features.len() + mountains.features.len();
    let mut out = format!("# NWS Active Alerts — Santa Cruz ({} total)\n\n", total);

    if total == 0 {
        out.push_str("No active weather alerts for Santa Cruz coastal or mountain zones.\n");
        let now = crate::util::now_pacific();
        out.push_str(&format!(
            "\n_Checked: {} · Zones: {} (coastal), {} (mountains)_\n",
            now.format("%-I:%M %p"),
            nws::ZONE_COASTAL,
            nws::ZONE_MOUNTAINS
        ));
        return out;
    }

    if !coastal.features.is_empty() {
        out.push_str(&format!(
            "## Coastal ({})\n\n",
            nws::ZONE_COASTAL
        ));
        for feature in &coastal.features {
            write_alert(&mut out, &feature.properties);
        }
    }

    if !mountains.features.is_empty() {
        out.push_str(&format!(
            "## Mountains ({})\n\n",
            nws::ZONE_MOUNTAINS
        ));
        for feature in &mountains.features {
            write_alert(&mut out, &feature.properties);
        }
    }

    let now = crate::util::now_pacific();
    out.push_str(&format!(
        "\n_Source: NOAA NWS. Last checked: {}_\n",
        now.format("%-I:%M %p")
    ));
    out
}

fn write_alert(out: &mut String, a: &nws::AlertProperties) {
    let severity = if a.severity.is_empty() {
        String::new()
    } else {
        format!(" [{}]", a.severity)
    };
    out.push_str(&format!("**{}**{}\n", a.event, severity));
    if !a.headline.is_empty() {
        out.push_str(&format!("{}\n", a.headline));
    }
    if !a.description.is_empty() {
        // Trim NWS descriptions to a few lines — they can be huge
        let trimmed: String = a.description.lines().take(4).collect::<Vec<_>>().join("\n");
        out.push_str(&format!("{}\n", trimmed));
    }
    if let Some(instruction) = &a.instruction {
        if !instruction.is_empty() {
            out.push_str(&format!("_Instructions:_ {}\n", instruction));
        }
    }
    if !a.expires.is_empty() {
        out.push_str(&format!("_Expires: {}_\n", a.expires));
    }
    out.push('\n');
}
