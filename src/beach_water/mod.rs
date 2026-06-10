//! Beach bacteria monitoring results from California's data.ca.gov CKAN
//! DataStore API. Evaluates sample results against AB411 bacterial standards.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::Arc;

use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cache::CacheStore;

const RESOURCE_ID: &str = "15a63495-8d9f-4a49-b43a-3092ef3106b9";
const CKAN_BASE: &str = "https://data.ca.gov/api/3/action/datastore_search_sql";

// ─── Request ───

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BeachWaterQualityRequest {
    /// Beach name filter (e.g. "Cowell", "Capitola", "Natural Bridges").
    /// Case-insensitive partial match. If omitted, returns all monitored
    /// Santa Cruz County beaches.
    pub beach: Option<String>,
    /// Days back to search (default 30, max 90). Shows the most recent sample
    /// per station within this window.
    pub days: Option<u32>,
}

// ─── Service ───

pub struct BeachWaterService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl BeachWaterService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    pub async fn get_beach_water_quality(
        &self,
        beach: Option<&str>,
        days: Option<u32>,
    ) -> Result<String> {
        let days = days.unwrap_or(30).clamp(1, 90);
        let beach_lower = beach.map(|b| b.to_lowercase());

        // Cache key: "all" when no beach filter
        let beach_key = beach_lower.as_deref().unwrap_or("all");
        let cache_key = format!("beach_water:{}:{}", beach_key, days);

        let http = self.http.clone();
        let beach_filter = beach_lower.clone();
        let records = self
            .cache
            .get_or_fetch::<Vec<CkanRecord>, _, _>(&cache_key, 43200, move || async move {
                fetch_records(&http, beach_filter.as_deref(), days).await
            })
            .await?;

        Ok(format_output(&records, beach.is_some()))
    }
}

// ─── CKAN API types ───

#[derive(Debug, Deserialize, Serialize, Clone)]
struct CkanResponse {
    success: bool,
    result: CkanResult,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct CkanResult {
    records: Vec<CkanRecord>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct CkanRecord {
    #[serde(rename = "StationName")]
    station_name: String,
    #[serde(rename = "StationCode")]
    station_code: String,
    #[serde(rename = "SampleDate")]
    sample_date: Option<String>,
    #[serde(rename = "Analyte")]
    analyte: String,
    #[serde(rename = "Result")]
    result: Option<String>,
    #[serde(rename = "Unit")]
    unit: Option<String>,
    #[serde(rename = "30DayGeoMean", default)]
    geo_mean_30d: Option<String>,
    #[serde(rename = "TargetLatitude", default)]
    latitude: Option<String>,
    #[serde(rename = "TargetLongitude", default)]
    longitude: Option<String>,
}

// ─── AB411 thresholds ───

struct Ab411Thresholds;

impl Ab411Thresholds {
    const TOTAL_COLIFORM_SINGLE: f64 = 10_000.0;
    const TOTAL_COLIFORM_GEO: f64 = 1_000.0;
    const FECAL_COLIFORM_SINGLE: f64 = 400.0;
    const FECAL_COLIFORM_GEO: f64 = 200.0;
    const ENTEROCOCCUS_SINGLE: f64 = 104.0;
    const ENTEROCOCCUS_GEO: f64 = 35.0;
    const E_COLI_SINGLE: f64 = 235.0;
    const E_COLI_GEO: f64 = 126.0;
}

/// Check a single-sample result against the AB411 threshold for the given analyte.
/// Returns `(threshold_description, exceeds_threshold)`.
fn check_threshold(analyte: &str, value: f64) -> (&'static str, bool) {
    let lower = analyte.to_lowercase();
    if lower.contains("total coliform") {
        ("<10,000 single", value > Ab411Thresholds::TOTAL_COLIFORM_SINGLE)
    } else if lower.contains("fecal coliform") {
        ("<400 single", value > Ab411Thresholds::FECAL_COLIFORM_SINGLE)
    } else if lower.contains("enterococcus") {
        ("<104 single", value > Ab411Thresholds::ENTEROCOCCUS_SINGLE)
    } else if lower.contains("e. coli") || lower.contains("e.coli") || lower.contains("escherichia")
    {
        ("<235 single", value > Ab411Thresholds::E_COLI_SINGLE)
    } else {
        ("—", false)
    }
}

/// Check a 30-day geometric mean against the AB411 geo-mean threshold.
/// Returns `(threshold_description, exceeds_threshold)`.
fn check_geo_threshold(analyte: &str, value: f64) -> (&'static str, bool) {
    let lower = analyte.to_lowercase();
    if lower.contains("total coliform") {
        ("<1,000", value > Ab411Thresholds::TOTAL_COLIFORM_GEO)
    } else if lower.contains("fecal coliform") {
        ("<200", value > Ab411Thresholds::FECAL_COLIFORM_GEO)
    } else if lower.contains("enterococcus") {
        ("<35", value > Ab411Thresholds::ENTEROCOCCUS_GEO)
    } else if lower.contains("e. coli") || lower.contains("e.coli") || lower.contains("escherichia")
    {
        ("<126", value > Ab411Thresholds::E_COLI_GEO)
    } else {
        ("—", false)
    }
}

// ─── Station name cleanup ───

/// Strip the station code prefix and ", Santa Cruz" suffix from display names.
/// `"O490-Cowell Beach, Santa Cruz"` -> `"Cowell Beach"`
fn clean_station_name(raw: &str) -> &str {
    let name = raw.split('-').nth(1).unwrap_or(raw).trim();
    name.strip_suffix(", Santa Cruz").unwrap_or(name)
}

// ─── API fetch ───

async fn fetch_records(
    http: &reqwest::Client,
    beach_filter: Option<&str>,
    days: u32,
) -> Result<Vec<CkanRecord>> {
    let now = crate::util::now_pacific();
    let start_date = (now - chrono::Duration::days(i64::from(days)))
        .format("%Y-%m-%d")
        .to_string();

    let mut sql = format!(
        r#"SELECT "StationName", "StationCode", "SampleDate", "Analyte", "Result", "Unit", "30DayGeoMean", "TargetLatitude", "TargetLongitude" FROM "{}" WHERE "StationCode" LIKE 'O%' AND "SampleDate" >= '{}'"#,
        RESOURCE_ID, start_date
    );

    if let Some(beach) = beach_filter {
        write!(sql, r#" AND LOWER("StationName") LIKE '%{}%'"#, beach).unwrap();
    }

    write!(
        sql,
        r#" ORDER BY "StationName", "SampleDate" DESC, "Analyte" LIMIT 500"#
    )
    .unwrap();

    let encoded_sql = urlencoding::encode(&sql);
    let url = format!("{}?sql={}", CKAN_BASE, encoded_sql);

    let resp = http
        .get(&url)
        .send()
        .await
        .context("CKAN HTTP request failed")?;

    if !resp.status().is_success() {
        anyhow::bail!("data.ca.gov returned HTTP {}", resp.status());
    }

    let body = resp.text().await.context("reading CKAN response body")?;
    let ckan: CkanResponse =
        serde_json::from_str(&body).context("parsing CKAN JSON response")?;

    if !ckan.success {
        anyhow::bail!("CKAN API returned success=false");
    }

    Ok(ckan.result.records)
}

// ─── Formatting ───

/// Format a human-readable date from the CKAN ISO timestamp.
/// `"2022-05-16T00:00:00"` -> `"May 16, 2022"`
fn format_sample_date(raw: &str) -> String {
    // Try parsing the full ISO datetime first, then fall back to date-only
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S") {
        return dt.format("%B %-d, %Y").to_string();
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return d.format("%B %-d, %Y").to_string();
    }
    raw.to_string()
}

fn format_output(records: &[CkanRecord], filtered: bool) -> String {
    if records.is_empty() {
        let mut out = String::from("# Beach Water Quality — Santa Cruz County\n\n");
        write!(
            out,
            "No recent samples found. The monitoring data may not have been updated recently, \
             or the beach filter may not match any stations.\n\n\
             Try broadening the search window with `days: 90` or omitting the beach filter.\n"
        )
        .unwrap();
        return out;
    }

    // Group records by station
    let mut stations: BTreeMap<String, Vec<&CkanRecord>> = BTreeMap::new();
    for record in records {
        stations
            .entry(record.station_name.clone())
            .or_default()
            .push(record);
    }

    // For each station, find the most recent sample date and keep only those records
    let mut latest_by_station: BTreeMap<String, (String, Vec<&CkanRecord>)> = BTreeMap::new();
    for (station, recs) in &stations {
        let latest_date = recs
            .iter()
            .filter_map(|r| r.sample_date.as_deref())
            .max()
            .unwrap_or("")
            .to_string();

        let latest_records: Vec<&CkanRecord> = recs
            .iter()
            .filter(|r| r.sample_date.as_deref() == Some(latest_date.as_str()))
            .copied()
            .collect();

        latest_by_station.insert(station.clone(), (latest_date, latest_records));
    }

    let station_count = latest_by_station.len();
    let mut out = String::from("# Beach Water Quality — Santa Cruz County\n\n");

    if filtered {
        write!(
            out,
            "_Showing latest samples from {} matching station(s). \
             Samples collected weekly; results are 1-7 days old._\n\n",
            station_count
        )
        .unwrap();
    } else {
        write!(
            out,
            "_Showing latest samples from {} monitored beaches. \
             Samples collected weekly; results are 1-7 days old._\n\n",
            station_count
        )
        .unwrap();
    }

    for (station_raw, (latest_date, recs)) in &latest_by_station {
        let name = clean_station_name(station_raw);
        let code = recs
            .first()
            .map(|r| r.station_code.as_str())
            .unwrap_or("—");
        let date_display = if latest_date.is_empty() {
            "Unknown".to_string()
        } else {
            format_sample_date(latest_date)
        };

        write!(out, "## {} ({})\n", name, code).unwrap();
        write!(out, "**Latest sample**: {}\n", date_display).unwrap();
        write!(out, "| Analyte | Result | Threshold | Status |\n").unwrap();
        write!(out, "|---|---|---|---|\n").unwrap();

        for rec in recs {
            let result_str = rec.result.as_deref().unwrap_or("—");
            let unit = rec.unit.as_deref().unwrap_or("");

            // Single-sample result row
            if let Ok(value) = result_str.parse::<f64>() {
                let (threshold, exceeds) = check_threshold(&rec.analyte, value);
                let status = if exceeds { "Exceeds" } else { "Pass" };
                let icon = if exceeds { "\u{26a0}\u{fe0f}" } else { "\u{2705}" };
                write!(
                    out,
                    "| {} | {} {} | {} | {} {} |\n",
                    rec.analyte, result_str, unit, threshold, icon, status
                )
                .unwrap();
            } else {
                write!(
                    out,
                    "| {} | {} {} | — | — |\n",
                    rec.analyte, result_str, unit
                )
                .unwrap();
            }

            // 30-day geo mean row (if present and parseable)
            if let Some(geo_str) = rec.geo_mean_30d.as_deref() {
                if let Ok(geo_val) = geo_str.parse::<f64>() {
                    let (threshold, exceeds) = check_geo_threshold(&rec.analyte, geo_val);
                    let status = if exceeds { "Exceeds" } else { "Pass" };
                    let icon = if exceeds { "\u{26a0}\u{fe0f}" } else { "\u{2705}" };
                    write!(
                        out,
                        "| 30-day geo mean ({}) | {} | {} | {} {} |\n",
                        rec.analyte, geo_str, threshold, icon, status
                    )
                    .unwrap();
                }
            }
        }

        out.push('\n');
    }

    write!(
        out,
        "_Beach water quality samples are collected weekly by Santa Cruz County \
         Environmental Health. Results are typically 1-7 days old._\n"
    )
    .unwrap();
    let now = crate::util::now_pacific();
    write!(
        out,
        "_Source: CA State Water Board BeachWatch via data.ca.gov. Last updated: {}_\n",
        now.format("%-I:%M %p")
    )
    .unwrap();

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_station_name_strips() {
        assert_eq!(
            clean_station_name("O490-Cowell Beach, Santa Cruz"),
            "Cowell Beach"
        );
        assert_eq!(
            clean_station_name("O235-Capitola City Beach, Santa Cruz"),
            "Capitola City Beach"
        );
        // No prefix
        assert_eq!(
            clean_station_name("Some Beach, Santa Cruz"),
            "Some Beach"
        );
        // No suffix
        assert_eq!(clean_station_name("O100-My Beach"), "My Beach");
        // Neither prefix nor suffix
        assert_eq!(clean_station_name("Plain Name"), "Plain Name");
    }

    #[test]
    fn check_threshold_pass_fail() {
        // Total Coliform — under limit
        let (desc, exceeds) = check_threshold("Total Coliform", 220.0);
        assert_eq!(desc, "<10,000 single");
        assert!(!exceeds);

        // Total Coliform — over limit
        let (_, exceeds) = check_threshold("Total Coliform", 15_000.0);
        assert!(exceeds);

        // Fecal Coliform — pass
        let (desc, exceeds) = check_threshold("Fecal Coliform", 100.0);
        assert_eq!(desc, "<400 single");
        assert!(!exceeds);

        // Fecal Coliform — fail
        let (_, exceeds) = check_threshold("Fecal Coliform", 500.0);
        assert!(exceeds);

        // Enterococcus — pass
        let (desc, exceeds) = check_threshold("Enterococcus", 28.0);
        assert_eq!(desc, "<104 single");
        assert!(!exceeds);

        // Enterococcus — fail
        let (_, exceeds) = check_threshold("Enterococcus", 156.0);
        assert!(exceeds);

        // E. coli — pass
        let (desc, exceeds) = check_threshold("E. coli", 100.0);
        assert_eq!(desc, "<235 single");
        assert!(!exceeds);

        // E. coli — fail
        let (_, exceeds) = check_threshold("E. coli", 300.0);
        assert!(exceeds);

        // Geo mean thresholds
        let (desc, exceeds) = check_geo_threshold("Enterococcus", 22.4);
        assert_eq!(desc, "<35");
        assert!(!exceeds);

        let (_, exceeds) = check_geo_threshold("Enterococcus", 50.0);
        assert!(exceeds);

        let (desc, exceeds) = check_geo_threshold("Total Coliform", 500.0);
        assert_eq!(desc, "<1,000");
        assert!(!exceeds);

        let (_, exceeds) = check_geo_threshold("Total Coliform", 1_500.0);
        assert!(exceeds);

        // Unknown analyte
        let (desc, exceeds) = check_threshold("Mystery Bacteria", 999.0);
        assert_eq!(desc, "—");
        assert!(!exceeds);
    }

    #[test]
    fn parse_ckan_response() {
        let json = r#"{
            "success": true,
            "result": {
                "records": [
                    {
                        "StationName": "O490-Cowell Beach, Santa Cruz",
                        "StationCode": "O490",
                        "SampleDate": "2022-05-16T00:00:00",
                        "Analyte": "Total Coliform",
                        "Result": "220",
                        "Unit": "CFU/100mL",
                        "30DayGeoMean": "287.3",
                        "TargetLatitude": "36.9618",
                        "TargetLongitude": "-122.023"
                    },
                    {
                        "StationName": "O235-Capitola City Beach, Santa Cruz",
                        "StationCode": "O235",
                        "SampleDate": "2022-05-16T00:00:00",
                        "Analyte": "Enterococcus",
                        "Result": "156",
                        "Unit": "CFU/100mL",
                        "30DayGeoMean": null,
                        "TargetLatitude": "36.9722",
                        "TargetLongitude": "-121.9533"
                    }
                ],
                "sql": "SELECT ..."
            }
        }"#;

        let parsed: CkanResponse = serde_json::from_str(json).unwrap();
        assert!(parsed.success);
        assert_eq!(parsed.result.records.len(), 2);

        let rec = &parsed.result.records[0];
        assert_eq!(rec.station_name, "O490-Cowell Beach, Santa Cruz");
        assert_eq!(rec.station_code, "O490");
        assert_eq!(rec.sample_date.as_deref(), Some("2022-05-16T00:00:00"));
        assert_eq!(rec.analyte, "Total Coliform");
        assert_eq!(rec.result.as_deref(), Some("220"));
        assert_eq!(rec.unit.as_deref(), Some("CFU/100mL"));
        assert_eq!(rec.geo_mean_30d.as_deref(), Some("287.3"));
        assert_eq!(rec.latitude.as_deref(), Some("36.9618"));
        assert_eq!(rec.longitude.as_deref(), Some("-122.023"));

        // Null geo mean
        let rec2 = &parsed.result.records[1];
        assert!(rec2.geo_mean_30d.is_none());
    }

    #[test]
    fn format_output_renders() {
        let records = vec![
            CkanRecord {
                station_name: "O490-Cowell Beach, Santa Cruz".to_string(),
                station_code: "O490".to_string(),
                sample_date: Some("2022-05-16T00:00:00".to_string()),
                analyte: "Total Coliform".to_string(),
                result: Some("220".to_string()),
                unit: Some("CFU/100mL".to_string()),
                geo_mean_30d: Some("287.3".to_string()),
                latitude: Some("36.9618".to_string()),
                longitude: Some("-122.023".to_string()),
            },
            CkanRecord {
                station_name: "O490-Cowell Beach, Santa Cruz".to_string(),
                station_code: "O490".to_string(),
                sample_date: Some("2022-05-16T00:00:00".to_string()),
                analyte: "Enterococcus".to_string(),
                result: Some("28".to_string()),
                unit: Some("CFU/100mL".to_string()),
                geo_mean_30d: Some("22.4".to_string()),
                latitude: Some("36.9618".to_string()),
                longitude: Some("-122.023".to_string()),
            },
            CkanRecord {
                station_name: "O235-Capitola City Beach, Santa Cruz".to_string(),
                station_code: "O235".to_string(),
                sample_date: Some("2022-05-16T00:00:00".to_string()),
                analyte: "Enterococcus".to_string(),
                result: Some("156".to_string()),
                unit: Some("CFU/100mL".to_string()),
                geo_mean_30d: None,
                latitude: Some("36.9722".to_string()),
                longitude: Some("-121.9533".to_string()),
            },
        ];

        let output = format_output(&records, false);

        // Header
        assert!(output.contains("# Beach Water Quality"));
        assert!(output.contains("2 monitored beaches"));
        // Cowell Beach station
        assert!(output.contains("## Cowell Beach (O490)"));
        assert!(output.contains("May 16, 2022"));
        // Total Coliform row — pass
        assert!(output.contains("Total Coliform"));
        assert!(output.contains("220 CFU/100mL"));
        assert!(output.contains("Pass"));
        // Geo mean row for Total Coliform
        assert!(output.contains("30-day geo mean (Total Coliform)"));
        assert!(output.contains("287.3"));
        // Enterococcus single — pass at Cowell
        assert!(output.contains("28 CFU/100mL"));
        // Geo mean for Enterococcus — pass
        assert!(output.contains("30-day geo mean (Enterococcus)"));
        assert!(output.contains("22.4"));
        // Capitola station
        assert!(output.contains("## Capitola City Beach (O235)"));
        // Enterococcus exceeds at Capitola
        assert!(output.contains("156 CFU/100mL"));
        assert!(output.contains("Exceeds"));
        // Footer
        assert!(output.contains("data.ca.gov"));
    }

    #[test]
    fn empty_results_message() {
        let output = format_output(&[], false);
        assert!(output.contains("No recent samples found"));
        assert!(output.contains("broadening the search window"));
    }

    #[test]
    fn format_sample_date_parses() {
        assert_eq!(
            format_sample_date("2022-05-16T00:00:00"),
            "May 16, 2022"
        );
        assert_eq!(format_sample_date("2023-12-01"), "December 1, 2023");
        // Unparseable falls back to raw
        assert_eq!(format_sample_date("garbage"), "garbage");
    }
}
