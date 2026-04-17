//! NOAA CO-OPS tide predictions.
//!
//! Uses the CO-OPS Data API (`api.tidesandcurrents.noaa.gov`, no auth, free).
//! Default station is 9413450 Monterey, CA — the closest official tide station
//! to Santa Cruz (the SC Wharf station was decommissioned). Other useful
//! stations nearby: 9414575 Coyote Creek / 9414290 San Francisco / 9413450
//! Monterey.
//!
//! Docs: <https://api.tidesandcurrents.noaa.gov/api/prod/>

use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{Duration, Local, NaiveDate, NaiveDateTime};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cache::CacheStore;

const COOPS_BASE: &str = "https://api.tidesandcurrents.noaa.gov/api/prod/datagetter";
const DEFAULT_STATION: &str = "9413450"; // Monterey
const DEFAULT_STATION_NAME: &str = "Monterey, CA";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TidesRequest {
    /// NOAA CO-OPS station ID. Defaults to 9413450 (Monterey, CA) — closest
    /// official station to Santa Cruz. Find others at
    /// https://tidesandcurrents.noaa.gov/
    pub station: Option<String>,
    /// Days ahead to fetch (1-7). Default 3.
    pub days: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TidePrediction {
    /// Local time string "YYYY-MM-DD HH:MM"
    pub time: String,
    /// Water level in feet above MLLW
    pub height_ft: f64,
    /// "H" or "L"
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TideBundle {
    station: String,
    station_name: String,
    predictions: Vec<TidePrediction>,
}

pub struct TidesService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl TidesService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    pub async fn get_tides(&self, station: Option<&str>, days: Option<u32>) -> Result<String> {
        let station = station.unwrap_or(DEFAULT_STATION).to_string();
        let days = days.unwrap_or(3).clamp(1, 7);

        let cache_key = format!("tides:{}:{}", station, days);
        let http = self.http.clone();
        let station_for_fetch = station.clone();
        let bundle = self
            .cache
            .get_or_fetch::<TideBundle, _, _>(&cache_key, 3600, move || async move {
                let preds = fetch_predictions(&http, &station_for_fetch, days).await?;
                let name = if station_for_fetch == DEFAULT_STATION {
                    DEFAULT_STATION_NAME.to_string()
                } else {
                    format!("station {}", station_for_fetch)
                };
                Ok(TideBundle {
                    station: station_for_fetch,
                    station_name: name,
                    predictions: preds,
                })
            })
            .await?;

        Ok(format_tides(&bundle, days))
    }
}

async fn fetch_predictions(
    http: &reqwest::Client,
    station: &str,
    days: u32,
) -> Result<Vec<TidePrediction>> {
    let today = Local::now().date_naive();
    let end = today + Duration::days(days as i64 - 1);
    let begin_date = today.format("%Y%m%d").to_string();
    let end_date = end.format("%Y%m%d").to_string();

    let url = format!(
        "{}?product=predictions&application=slug-mcp&begin_date={}&end_date={}\
         &datum=MLLW&station={}&time_zone=lst_ldt&units=english&interval=hilo&format=json",
        COOPS_BASE, begin_date, end_date, station
    );

    let resp = http
        .get(&url)
        .send()
        .await
        .context("CO-OPS HTTP request failed")?;
    if !resp.status().is_success() {
        anyhow::bail!("CO-OPS returned HTTP {}", resp.status());
    }

    #[derive(Deserialize)]
    struct CoopsResponse {
        #[serde(default)]
        predictions: Vec<CoopsPrediction>,
        #[serde(default)]
        error: Option<CoopsError>,
    }
    #[derive(Deserialize)]
    struct CoopsPrediction {
        t: String,
        v: String,
        #[serde(rename = "type")]
        ty: String,
    }
    #[derive(Deserialize)]
    struct CoopsError {
        message: String,
    }

    let body: CoopsResponse = resp.json().await.context("parsing CO-OPS JSON")?;
    if let Some(err) = body.error {
        anyhow::bail!("CO-OPS error: {}", err.message);
    }
    let out = body
        .predictions
        .into_iter()
        .filter_map(|p| {
            let v = p.v.parse::<f64>().ok()?;
            Some(TidePrediction {
                time: p.t,
                height_ft: v,
                kind: p.ty,
            })
        })
        .collect();
    Ok(out)
}

fn format_tides(bundle: &TideBundle, days: u32) -> String {
    let mut out = format!(
        "# Tide predictions — {} ({})\n\n",
        bundle.station_name, bundle.station
    );
    let day_word = if days == 1 { "day" } else { "days" };
    out.push_str(&format!(
        "Next {} {} of high/low tides. Heights in feet above MLLW (Mean Lower Low Water).\n\n",
        days, day_word
    ));

    if bundle.predictions.is_empty() {
        out.push_str("_No predictions returned._\n");
        return out;
    }

    let mut current_date: Option<NaiveDate> = None;
    for p in &bundle.predictions {
        let parsed = NaiveDateTime::parse_from_str(&p.time, "%Y-%m-%d %H:%M").ok();
        let date = parsed.map(|d| d.date());
        if date != current_date {
            if current_date.is_some() {
                out.push('\n');
            }
            if let Some(d) = date {
                out.push_str(&format!("## {}\n", d.format("%A, %B %-d")));
                out.push_str("| Time | Type | Height |\n");
                out.push_str("|---|---|---|\n");
            }
            current_date = date;
        }
        let time_str = parsed
            .map(|d| d.format("%-I:%M %p").to_string())
            .unwrap_or_else(|| p.time.clone());
        let kind_label = match p.kind.as_str() {
            "H" => "High",
            "L" => "Low",
            other => other,
        };
        out.push_str(&format!(
            "| {} | {} | {:.2} ft |\n",
            time_str, kind_label, p.height_ft
        ));
    }

    out.push_str(&format!(
        "\n_Source: NOAA CO-OPS. Last updated: {}_\n",
        Local::now().format("%-I:%M %p")
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_tides_groups_by_date() {
        let bundle = TideBundle {
            station: "9413450".to_string(),
            station_name: "Monterey, CA".to_string(),
            predictions: vec![
                TidePrediction {
                    time: "2026-04-17 03:12".to_string(),
                    height_ft: 4.45,
                    kind: "H".to_string(),
                },
                TidePrediction {
                    time: "2026-04-17 09:34".to_string(),
                    height_ft: 0.87,
                    kind: "L".to_string(),
                },
                TidePrediction {
                    time: "2026-04-18 04:00".to_string(),
                    height_ft: 4.6,
                    kind: "H".to_string(),
                },
            ],
        };
        let out = format_tides(&bundle, 2);
        assert!(out.contains("Monterey, CA"));
        assert!(out.contains("High"));
        assert!(out.contains("Low"));
        assert!(out.contains("4.45 ft"));
        // should have two date sections
        assert_eq!(out.matches("## ").count(), 2);
    }
}
