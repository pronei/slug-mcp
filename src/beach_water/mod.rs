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
    // Absent on error envelopes (success=false + `error` object instead).
    result: Option<CkanResult>,
    #[serde(default)]
    error: Option<serde_json::Value>,
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
    #[serde(rename = "Result", default, deserialize_with = "de_number_as_string")]
    result: Option<String>,
    #[serde(rename = "Unit")]
    unit: Option<String>,
    #[serde(
        rename = "30DayGeoMean",
        default,
        deserialize_with = "de_number_as_string"
    )]
    geo_mean_30d: Option<String>,
    #[serde(
        rename = "TargetLatitude",
        default,
        deserialize_with = "de_number_as_string"
    )]
    latitude: Option<String>,
    #[serde(
        rename = "TargetLongitude",
        default,
        deserialize_with = "de_number_as_string"
    )]
    longitude: Option<String>,
}

/// The datastore schema declares these columns `numeric`; datastore_search_sql
/// stringifies them today but CKAN's plain datastore_search returns JSON
/// numbers — accept both so an upstream serialization flip can't break parsing.
fn de_number_as_string<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(s) => Ok(Some(s)),
        serde_json::Value::Number(n) => Ok(Some(n.to_string())),
        other => Err(serde::de::Error::custom(format!(
            "expected string or number, got {}",
            other
        ))),
    }
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
///
/// data.ca.gov names analytes "Coliform, Total" / "Coliform, Fecal" while the
/// AB411 text says "Total Coliform" — match on word presence, fecal before
/// total so a fecal-coliform row can never hit the total-coliform branch.
fn check_threshold(analyte: &str, value: f64) -> (&'static str, bool) {
    let lower = analyte.to_lowercase();
    let has = |w: &str| lower.contains(w);
    if has("coliform") && has("fecal") {
        (
            "<400 single",
            value > Ab411Thresholds::FECAL_COLIFORM_SINGLE,
        )
    } else if has("coliform") && has("total") {
        (
            "<10,000 single",
            value > Ab411Thresholds::TOTAL_COLIFORM_SINGLE,
        )
    } else if has("enterococcus") {
        ("<104 single", value > Ab411Thresholds::ENTEROCOCCUS_SINGLE)
    } else if has("e. coli") || has("e.coli") || has("escherichia") {
        ("<235 single", value > Ab411Thresholds::E_COLI_SINGLE)
    } else {
        ("—", false)
    }
}

/// Check a 30-day geometric mean against the AB411 geo-mean threshold.
/// Returns `(threshold_description, exceeds_threshold)`.
fn check_geo_threshold(analyte: &str, value: f64) -> (&'static str, bool) {
    let lower = analyte.to_lowercase();
    let has = |w: &str| lower.contains(w);
    if has("coliform") && has("fecal") {
        ("<200", value > Ab411Thresholds::FECAL_COLIFORM_GEO)
    } else if has("coliform") && has("total") {
        ("<1,000", value > Ab411Thresholds::TOTAL_COLIFORM_GEO)
    } else if has("enterococcus") {
        ("<35", value > Ab411Thresholds::ENTEROCOCCUS_GEO)
    } else if has("e. coli") || has("e.coli") || has("escherichia") {
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
        // Escape for a SQL string literal embedded in the CKAN datastore_search_sql
        // query: double single-quotes, and neutralize LIKE wildcards so user
        // input can't widen the match or break out of the literal.
        let escaped = beach
            .replace('\'', "''")
            .replace('%', "\\%")
            .replace('_', "\\_");
        write!(
            sql,
            r#" AND LOWER("StationName") LIKE '%{}%'"#,
            escaped.to_lowercase()
        )
        .unwrap();
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
    parse_ckan_body(&body)
}

fn parse_ckan_body(body: &str) -> Result<Vec<CkanRecord>> {
    let ckan: CkanResponse = serde_json::from_str(body).context("parsing CKAN JSON response")?;

    if !ckan.success {
        let detail = match &ckan.error {
            Some(e) => e
                .get("message")
                .and_then(|m| m.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| e.to_string()),
            None => "no error detail provided".to_string(),
        };
        anyhow::bail!("CKAN API error: {}", detail);
    }

    Ok(ckan.result.map(|r| r.records).unwrap_or_default())
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
        let code = recs.first().map(|r| r.station_code.as_str()).unwrap_or("—");
        let date_display = if latest_date.is_empty() {
            "Unknown".to_string()
        } else {
            format_sample_date(latest_date)
        };

        writeln!(out, "## {} ({})", name, code).unwrap();
        writeln!(out, "**Latest sample**: {}", date_display).unwrap();
        writeln!(out, "| Analyte | Result | Threshold | Status |").unwrap();
        writeln!(out, "|---|---|---|---|").unwrap();

        for rec in recs {
            let result_str = rec.result.as_deref().unwrap_or("—");
            let unit = rec.unit.as_deref().unwrap_or("");

            // Single-sample result row. Analytes with no AB411 threshold get
            // "—" rather than a misleading "Pass".
            match result_str.parse::<f64>() {
                Ok(value) => {
                    let (threshold, exceeds) = check_threshold(&rec.analyte, value);
                    if threshold == "—" {
                        writeln!(out, "| {} | {} {} | — | — |", rec.analyte, result_str, unit)
                            .unwrap();
                    } else {
                        let status = if exceeds { "Exceeds" } else { "Pass" };
                        let icon = if exceeds {
                            "\u{26a0}\u{fe0f}"
                        } else {
                            "\u{2705}"
                        };
                        writeln!(
                            out,
                            "| {} | {} {} | {} | {} {} |",
                            rec.analyte, result_str, unit, threshold, icon, status
                        )
                        .unwrap();
                    }
                }
                Err(_) => {
                    writeln!(out, "| {} | {} {} | — | — |", rec.analyte, result_str, unit).unwrap();
                }
            }

            // 30-day geo mean row (if present and parseable)
            if let Some(geo_str) = rec.geo_mean_30d.as_deref()
                && let Ok(geo_val) = geo_str.parse::<f64>()
            {
                let (threshold, exceeds) = check_geo_threshold(&rec.analyte, geo_val);
                if threshold == "—" {
                    writeln!(
                        out,
                        "| 30-day geo mean ({}) | {} | — | — |",
                        rec.analyte, geo_str
                    )
                    .unwrap();
                } else {
                    let status = if exceeds { "Exceeds" } else { "Pass" };
                    let icon = if exceeds {
                        "\u{26a0}\u{fe0f}"
                    } else {
                        "\u{2705}"
                    };
                    writeln!(
                        out,
                        "| 30-day geo mean ({}) | {} | {} | {} {} |",
                        rec.analyte, geo_str, threshold, icon, status
                    )
                    .unwrap();
                }
            }
        }

        out.push('\n');
    }

    writeln!(
        out,
        "_Beach water quality samples are collected weekly by Santa Cruz County \
         Environmental Health. Results are typically 1-7 days old._"
    )
    .unwrap();
    let now = crate::util::now_pacific();
    writeln!(
        out,
        "_Source: CA State Water Board BeachWatch via data.ca.gov. Last updated: {}_",
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
        assert_eq!(clean_station_name("Some Beach, Santa Cruz"), "Some Beach");
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
        let records = parsed.result.unwrap().records;
        assert_eq!(records.len(), 2);

        let rec = &records[0];
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
        let rec2 = &records[1];
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
        assert_eq!(format_sample_date("2022-05-16T00:00:00"), "May 16, 2022");
        assert_eq!(format_sample_date("2023-12-01"), "December 1, 2023");
        // Unparseable falls back to raw
        assert_eq!(format_sample_date("garbage"), "garbage");
    }

    const CKAN_FIXTURE: &str = include_str!("fixtures/ckan_beach_water.json");

    #[test]
    fn live_analyte_names_match_thresholds() {
        // data.ca.gov names analytes "Coliform, Total" / "Coliform, Fecal"
        // (word order flipped vs the AB411 names) — matching must be
        // order-insensitive or the threshold check silently never fires.
        let (desc, exceeds) = check_threshold("Coliform, Total", 15_000.0);
        assert_eq!(desc, "<10,000 single");
        assert!(exceeds);

        let (desc, exceeds) = check_threshold("Coliform, Fecal", 500.0);
        assert_eq!(desc, "<400 single");
        assert!(exceeds);

        let (desc, exceeds) = check_geo_threshold("Coliform, Total", 1_500.0);
        assert_eq!(desc, "<1,000");
        assert!(exceeds);

        let (desc, exceeds) = check_geo_threshold("Coliform, Fecal", 250.0);
        assert_eq!(desc, "<200");
        assert!(exceeds);

        // Under-limit live names still pass.
        let (_, exceeds) = check_threshold("Coliform, Total", 650.0);
        assert!(!exceeds);
    }

    #[test]
    fn unknown_analyte_renders_no_status() {
        let records = vec![CkanRecord {
            station_name: "O490-Cowell Beach, Santa Cruz".to_string(),
            station_code: "O490".to_string(),
            sample_date: Some("2026-06-29T00:00:00".to_string()),
            analyte: "Mystery Bacteria".to_string(),
            result: Some("99999".to_string()),
            unit: Some("MPN/100 mL".to_string()),
            geo_mean_30d: Some("88888".to_string()),
            latitude: None,
            longitude: None,
        }];
        let out = format_output(&records, false);
        // An analyte with no AB411 threshold must not claim "Pass" no matter
        // how large the value is.
        assert!(!out.contains("Pass"), "unexpected Pass status:\n{}", out);
        assert!(!out.contains("Exceeds"));
        assert!(out.contains("| Mystery Bacteria | 99999 MPN/100 mL | — | — |"));
        assert!(out.contains("| 30-day geo mean (Mystery Bacteria) | 88888 | — | — |"));
    }

    #[test]
    fn parse_live_fixture() {
        let records = parse_ckan_body(CKAN_FIXTURE).unwrap();
        assert_eq!(records.len(), 5);
        assert_eq!(records[0].analyte, "Coliform, Total");
        assert_eq!(records[0].result.as_deref(), Some("10.0"));
        assert_eq!(records[0].unit.as_deref(), Some("MPN/100 mL"));
        // Null-heavy record: all optional fields None.
        let nullish = &records[4];
        assert_eq!(nullish.station_code, "O490");
        assert!(nullish.result.is_none());
        assert!(nullish.unit.is_none());
        assert!(nullish.geo_mean_30d.is_none());
        assert!(nullish.latitude.is_none());
        assert!(nullish.longitude.is_none());

        // The live-shape records render with real threshold checks.
        let out = format_output(&records, false);
        assert!(out.contains("## Rio del Mar Beach (O110)"));
        assert!(out.contains("Pass"));
        // Null result renders the em-dash placeholder, not a status.
        assert!(out.contains("| Enterococcus | —  | — | — |"));
    }

    #[test]
    fn ckan_error_envelope_surfaces_error() {
        // Real CKAN failures return success=false with an `error` object and
        // NO `result` key — the parser must surface the CKAN error, not a
        // confusing serde "missing field" message.
        let body = r#"{
            "help": "https://data.ca.gov/api/3/action/help_show?name=datastore_search_sql",
            "success": false,
            "error": {
                "__type": "Validation Error",
                "info": { "orig": ["syntax error at or near \"FROM\""] }
            }
        }"#;
        let err = parse_ckan_body(body).unwrap_err().to_string();
        assert!(err.contains("CKAN"), "got: {err}");
        assert!(err.contains("Validation Error"), "got: {err}");
    }

    #[test]
    fn numeric_fields_accept_json_numbers() {
        // The datastore schema declares Result/30DayGeoMean/TargetLatitude/
        // TargetLongitude as `numeric`; datastore_search_sql stringifies them
        // today but the plain datastore_search endpoint returns JSON numbers —
        // tolerate both.
        let body = r#"{
            "success": true,
            "result": {
                "records": [{
                    "StationName": "O110-Rio del Mar Beach, Santa Cruz",
                    "StationCode": "O110",
                    "SampleDate": "2026-06-22T00:00:00",
                    "Analyte": "Coliform, Total",
                    "Result": 650.0,
                    "Unit": "MPN/100 mL",
                    "30DayGeoMean": 58.626,
                    "TargetLatitude": 36.9688,
                    "TargetLongitude": -121.906
                }]
            }
        }"#;
        let records = parse_ckan_body(body).unwrap();
        assert_eq!(records.len(), 1);
        let rec = &records[0];
        assert_eq!(
            rec.result.as_deref().unwrap().parse::<f64>().unwrap(),
            650.0
        );
        assert_eq!(rec.geo_mean_30d.as_deref(), Some("58.626"));
        assert_eq!(
            rec.latitude.as_deref().unwrap().parse::<f64>().unwrap(),
            36.9688
        );
    }

    #[test]
    fn empty_records_parse_ok() {
        let body = r#"{"success": true, "result": {"records": []}}"#;
        let records = parse_ckan_body(body).unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn truncated_json_errors_gracefully() {
        let cut = &CKAN_FIXTURE[..CKAN_FIXTURE.len() / 2];
        let err = parse_ckan_body(cut).unwrap_err().to_string();
        assert!(err.contains("parsing CKAN JSON response"));
    }
}
