//! AirNow current AQI observations by ZIP code.
//!
//! Pulls the EPA AirNow `zipCode/current` JSON endpoint. Requires a free
//! `AIRNOW_API_KEY` (register at <https://docs.airnowapi.org/>). Graceful
//! degradation: if the key is absent, returns registration instructions
//! instead of erroring. Default ZIP is **95064** (UCSC).
//!
//! Docs: <https://docs.airnowapi.org/aq101>

use std::sync::Arc;

use anyhow::{Context, Result};
use crate::util::now_pacific;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cache::CacheStore;

const DEFAULT_ZIP: &str = "95064"; // UCSC

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AirQualityRequest {
    /// US ZIP code. Defaults to 95064 (UCSC main campus).
    pub zip_code: Option<String>,
    /// Search radius in miles (default 25).
    pub distance_miles: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AirReading {
    pub date_observed: String,
    pub hour_observed: u32,
    pub local_time_zone: String,
    pub reporting_area: String,
    pub state: String,
    pub parameter_name: String,
    pub aqi: i32,
    pub category: String,
}

pub struct AirQualityService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
    api_key: Option<String>,
}

impl AirQualityService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>, api_key: Option<String>) -> Self {
        Self {
            http,
            cache,
            api_key,
        }
    }

    pub async fn get_current(
        &self,
        zip: Option<&str>,
        distance: Option<u32>,
    ) -> Result<String> {
        let key = match &self.api_key {
            Some(k) if !k.is_empty() => k.clone(),
            _ => {
                return Ok(
                    "AirNow API key not configured.\n\
                     Get a free key at https://docs.airnowapi.org/ and set \
                     the `AIRNOW_API_KEY` environment variable."
                        .to_string(),
                );
            }
        };

        let zip = zip.unwrap_or(DEFAULT_ZIP).to_string();
        let distance = distance.unwrap_or(25);

        let cache_key = format!("air:current:{}:{}", zip, distance);
        let http = self.http.clone();
        let zip_for_fetch = zip.clone();
        let readings = self
            .cache
            .get_or_fetch::<Vec<AirReading>, _, _>(&cache_key, 1800, move || async move {
                fetch_airnow(&http, &key, &zip_for_fetch, distance).await
            })
            .await?;

        Ok(format_readings(&zip, &readings))
    }
}

async fn fetch_airnow(
    http: &reqwest::Client,
    key: &str,
    zip: &str,
    distance: u32,
) -> Result<Vec<AirReading>> {
    let url = format!(
        "https://www.airnowapi.org/aq/observation/zipCode/current/?format=application/json\
         &zipCode={}&distance={}&API_KEY={}",
        zip, distance, key
    );
    let resp = http
        .get(&url)
        .send()
        .await
        .context("AirNow HTTP request failed")?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        anyhow::bail!("AirNow returned 401 — check AIRNOW_API_KEY");
    }
    if !resp.status().is_success() {
        anyhow::bail!("AirNow returned HTTP {}", resp.status());
    }
    let body: Vec<AirNowObs> = resp.json().await.context("parsing AirNow JSON")?;

    Ok(body
        .into_iter()
        .map(|o| AirReading {
            date_observed: o.date_observed.trim().to_string(),
            hour_observed: o.hour_observed,
            local_time_zone: o.local_time_zone,
            reporting_area: o.reporting_area,
            state: o.state_code,
            parameter_name: o.parameter_name,
            aqi: o.aqi,
            category: o
                .category
                .and_then(|c| c.name)
                .unwrap_or_else(|| "Unknown".to_string()),
        })
        .collect())
}

#[derive(Deserialize)]
struct AirNowObs {
    #[serde(rename = "DateObserved")]
    date_observed: String,
    #[serde(rename = "HourObserved")]
    hour_observed: u32,
    #[serde(rename = "LocalTimeZone")]
    local_time_zone: String,
    #[serde(rename = "ReportingArea")]
    reporting_area: String,
    #[serde(rename = "StateCode")]
    state_code: String,
    #[serde(rename = "ParameterName")]
    parameter_name: String,
    #[serde(rename = "AQI")]
    aqi: i32,
    #[serde(rename = "Category")]
    category: Option<AirNowCategory>,
}
#[derive(Deserialize)]
struct AirNowCategory {
    #[serde(rename = "Name")]
    name: Option<String>,
}

fn aqi_icon(aqi: i32) -> &'static str {
    match aqi {
        i if i <= 50 => "🟢",
        i if i <= 100 => "🟡",
        i if i <= 150 => "🟠",
        i if i <= 200 => "🔴",
        i if i <= 300 => "🟣",
        _ => "⚫",
    }
}

fn format_readings(zip: &str, readings: &[AirReading]) -> String {
    if readings.is_empty() {
        return format!(
            "No AirNow observations returned for ZIP {} (station coverage may be sparse).\n",
            zip
        );
    }

    let area = &readings[0].reporting_area;
    let state = &readings[0].state;
    let date = &readings[0].date_observed;
    let hour = readings[0].hour_observed;
    let tz = &readings[0].local_time_zone;
    let mut out = format!(
        "# Air quality — {}, {} (ZIP {})\n\n\
         **Observed**: {} at {:02}:00 {}\n\n",
        area, state, zip, date, hour, tz
    );

    for r in readings {
        out.push_str(&format!(
            "- {} **{}**: AQI {} ({})\n",
            aqi_icon(r.aqi),
            r.parameter_name,
            r.aqi,
            r.category
        ));
    }

    out.push_str(&format!(
        "\n_Source: EPA AirNow. Last updated: {}_\n",
        now_pacific().format("%-I:%M %p")
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aqi_icon_thresholds() {
        assert_eq!(aqi_icon(25), "🟢");
        assert_eq!(aqi_icon(75), "🟡");
        assert_eq!(aqi_icon(125), "🟠");
        assert_eq!(aqi_icon(175), "🔴");
        assert_eq!(aqi_icon(250), "🟣");
        assert_eq!(aqi_icon(400), "⚫");
    }

    #[test]
    fn format_readings_sample() {
        let readings = vec![
            AirReading {
                date_observed: "2026-04-17".to_string(),
                hour_observed: 9,
                local_time_zone: "PDT".to_string(),
                reporting_area: "Santa Cruz".to_string(),
                state: "CA".to_string(),
                parameter_name: "O3".to_string(),
                aqi: 42,
                category: "Good".to_string(),
            },
            AirReading {
                date_observed: "2026-04-17".to_string(),
                hour_observed: 9,
                local_time_zone: "PDT".to_string(),
                reporting_area: "Santa Cruz".to_string(),
                state: "CA".to_string(),
                parameter_name: "PM2.5".to_string(),
                aqi: 58,
                category: "Moderate".to_string(),
            },
        ];
        let out = format_readings("95064", &readings);
        assert!(out.contains("Santa Cruz"));
        assert!(out.contains("O3"));
        assert!(out.contains("PM2.5"));
        assert!(out.contains("Good"));
        assert!(out.contains("Moderate"));
    }

    #[test]
    fn format_empty() {
        let out = format_readings("00000", &[]);
        assert!(out.contains("No AirNow"));
    }
}
