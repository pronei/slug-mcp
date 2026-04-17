//! USGS NWIS real-time stream & water-quality data.
//!
//! Uses the USGS Instantaneous Values (IV) Web Service (no auth, free).
//! Default site is **11160500** — San Lorenzo River at Big Trees — the SC
//! County reference gauge. Other SC-area gauges: 11159200 (Pajaro R. at
//! Chittenden), 11160900 (Soquel Creek at Soquel).
//!
//! Docs: <https://waterservices.usgs.gov/docs/instantaneous-values/>

use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Local;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cache::CacheStore;

const DEFAULT_SITE: &str = "11160500"; // San Lorenzo R. at Big Trees
const DEFAULT_PARAMS: &str = "00060,00065,00010"; // discharge, gage height, water temp

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StreamConditionsRequest {
    /// USGS site ID (8-digit). Defaults to 11160500 (San Lorenzo River at
    /// Big Trees). Find sites at https://waterdata.usgs.gov/nwis
    pub site: Option<String>,
    /// Comma-separated USGS parameter codes. Defaults to
    /// 00060 (discharge cfs) + 00065 (gage height ft) + 00010 (water temp C).
    pub parameters: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamReading {
    pub parameter_code: String,
    pub parameter_name: String,
    pub unit: String,
    pub value: f64,
    pub timestamp: String,
    pub site_name: String,
    pub site_id: String,
}

pub struct UsgsWaterService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl UsgsWaterService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    pub async fn get_stream_conditions(
        &self,
        site: Option<&str>,
        parameters: Option<&str>,
    ) -> Result<String> {
        let site = site.unwrap_or(DEFAULT_SITE).to_string();
        let params = parameters.unwrap_or(DEFAULT_PARAMS).to_string();

        let cache_key = format!("usgs:iv:{}:{}", site, params);
        let http = self.http.clone();
        let site_for_fetch = site.clone();
        let params_for_fetch = params.clone();
        let readings = self
            .cache
            .get_or_fetch::<Vec<StreamReading>, _, _>(&cache_key, 600, move || async move {
                fetch_iv(&http, &site_for_fetch, &params_for_fetch).await
            })
            .await?;

        Ok(format_readings(&site, &readings))
    }
}

async fn fetch_iv(
    http: &reqwest::Client,
    site: &str,
    parameters: &str,
) -> Result<Vec<StreamReading>> {
    let url = format!(
        "https://waterservices.usgs.gov/nwis/iv/?sites={}&parameterCd={}&format=json",
        site, parameters
    );
    let resp = http
        .get(&url)
        .send()
        .await
        .context("USGS IV HTTP request failed")?;
    if !resp.status().is_success() {
        anyhow::bail!("USGS returned HTTP {}", resp.status());
    }
    let body: IvResponse = resp.json().await.context("parsing USGS JSON")?;
    parse_iv(&body)
}

// ─── USGS response model ───
#[derive(Deserialize)]
struct IvResponse {
    value: IvValue,
}
#[derive(Deserialize)]
struct IvValue {
    #[serde(default)]
    #[serde(rename = "timeSeries")]
    time_series: Vec<TimeSeries>,
}
#[derive(Deserialize)]
struct TimeSeries {
    #[serde(rename = "sourceInfo")]
    source_info: SourceInfo,
    variable: Variable,
    #[serde(default)]
    values: Vec<ValueBlock>,
}
#[derive(Deserialize)]
struct SourceInfo {
    #[serde(rename = "siteName")]
    site_name: String,
    #[serde(rename = "siteCode")]
    #[serde(default)]
    site_code: Vec<SiteCode>,
}
#[derive(Deserialize)]
struct SiteCode {
    value: String,
}
#[derive(Deserialize)]
struct Variable {
    #[serde(rename = "variableCode")]
    #[serde(default)]
    variable_code: Vec<VariableCode>,
    #[serde(rename = "variableName")]
    variable_name: String,
    unit: Unit,
}
#[derive(Deserialize)]
struct VariableCode {
    value: String,
}
#[derive(Deserialize)]
struct Unit {
    #[serde(rename = "unitCode")]
    unit_code: String,
}
#[derive(Deserialize)]
struct ValueBlock {
    #[serde(default)]
    value: Vec<Reading>,
}
#[derive(Deserialize)]
struct Reading {
    value: String,
    #[serde(rename = "dateTime")]
    date_time: String,
}

fn parse_iv(body: &IvResponse) -> Result<Vec<StreamReading>> {
    let mut out = Vec::new();
    for ts in &body.value.time_series {
        let site_id = ts
            .source_info
            .site_code
            .first()
            .map(|s| s.value.clone())
            .unwrap_or_default();
        let param_code = ts
            .variable
            .variable_code
            .first()
            .map(|v| v.value.clone())
            .unwrap_or_default();
        for vb in &ts.values {
            if let Some(latest) = vb.value.last() {
                if let Ok(v) = latest.value.parse::<f64>() {
                    // USGS emits -999999 for missing
                    if v < -99999.0 {
                        continue;
                    }
                    out.push(StreamReading {
                        parameter_code: param_code.clone(),
                        parameter_name: ts.variable.variable_name.clone(),
                        unit: ts.variable.unit.unit_code.clone(),
                        value: v,
                        timestamp: latest.date_time.clone(),
                        site_name: ts.source_info.site_name.clone(),
                        site_id: site_id.clone(),
                    });
                }
            }
        }
    }
    if out.is_empty() {
        anyhow::bail!("USGS returned no usable readings (site may be offline)");
    }
    Ok(out)
}

fn format_readings(site: &str, readings: &[StreamReading]) -> String {
    let name = readings
        .first()
        .map(|r| r.site_name.as_str())
        .unwrap_or("USGS site");
    let mut out = format!("# Stream conditions — {} (USGS {})\n\n", name, site);

    // readings may share a site but have different parameters; group by param
    for r in readings {
        let value_str = match r.parameter_code.as_str() {
            "00010" => format!(
                "{:.1}°F ({:.1}°C)",
                r.value * 9.0 / 5.0 + 32.0,
                r.value
            ),
            "00060" => format!("{:.0} cfs", r.value),
            "00065" => format!("{:.2} ft", r.value),
            _ => format!("{:.2} {}", r.value, r.unit),
        };
        out.push_str(&format!(
            "- **{}** (`{}`): {}\n  _{}_\n",
            simplify_param_name(&r.parameter_name),
            r.parameter_code,
            value_str,
            r.timestamp
        ));
    }

    out.push_str(&format!(
        "\n_Source: USGS NWIS Instantaneous Values. Last updated: {}_\n",
        Local::now().format("%-I:%M %p")
    ));
    out
}

fn simplify_param_name(raw: &str) -> String {
    // USGS names have a trailing ", °C" or similar. Keep the first comma-segment.
    raw.split(',').next().unwrap_or(raw).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_iv_sample() {
        let json = r#"
        {
          "value": {
            "timeSeries": [
              {
                "sourceInfo": {
                  "siteName": "San Lorenzo R AT Big Trees CA",
                  "siteCode": [{"value": "11160500"}]
                },
                "variable": {
                  "variableCode": [{"value": "00060"}],
                  "variableName": "Streamflow, ft&#179;/s",
                  "unit": {"unitCode": "ft3/s"}
                },
                "values": [
                  {"value": [
                    {"value": "45", "dateTime": "2026-04-17T08:00:00-07:00"},
                    {"value": "47", "dateTime": "2026-04-17T08:15:00-07:00"}
                  ]}
                ]
              },
              {
                "sourceInfo": {
                  "siteName": "San Lorenzo R AT Big Trees CA",
                  "siteCode": [{"value": "11160500"}]
                },
                "variable": {
                  "variableCode": [{"value": "00010"}],
                  "variableName": "Temperature, water, °C",
                  "unit": {"unitCode": "deg C"}
                },
                "values": [
                  {"value": [
                    {"value": "12.5", "dateTime": "2026-04-17T08:15:00-07:00"}
                  ]}
                ]
              }
            ]
          }
        }
        "#;
        let body: IvResponse = serde_json::from_str(json).unwrap();
        let parsed = parse_iv(&body).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].parameter_code, "00060");
        assert!((parsed[0].value - 47.0).abs() < 0.001);
        assert_eq!(parsed[1].parameter_code, "00010");
        assert!((parsed[1].value - 12.5).abs() < 0.001);
    }

    #[test]
    fn parse_iv_skips_missing_sentinel() {
        let json = r#"
        {
          "value": {
            "timeSeries": [
              {
                "sourceInfo": {"siteName": "X", "siteCode": [{"value": "1"}]},
                "variable": {
                  "variableCode": [{"value": "00060"}],
                  "variableName": "Streamflow",
                  "unit": {"unitCode": "ft3/s"}
                },
                "values": [{"value": [{"value": "-999999", "dateTime": "2026-04-17T08:00:00-07:00"}]}]
              }
            ]
          }
        }
        "#;
        let body: IvResponse = serde_json::from_str(json).unwrap();
        assert!(parse_iv(&body).is_err());
    }

    #[test]
    fn format_readings_renders() {
        let r = StreamReading {
            parameter_code: "00060".to_string(),
            parameter_name: "Streamflow, ft3/s".to_string(),
            unit: "ft3/s".to_string(),
            value: 47.2,
            timestamp: "2026-04-17T08:15:00-07:00".to_string(),
            site_name: "San Lorenzo R AT Big Trees CA".to_string(),
            site_id: "11160500".to_string(),
        };
        let out = format_readings("11160500", &[r]);
        assert!(out.contains("San Lorenzo"));
        assert!(out.contains("47 cfs"));
    }
}
