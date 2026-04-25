//! CDIP / NDBC wave buoy spectral summary.
//!
//! CDIP (Scripps Coastal Data Information Program) operates Monterey-area
//! waveriders whose data is mirrored in NDBC's `.spec` format. The `.spec`
//! endpoint separates swell and wind-wave components (height, period,
//! direction) along with a steepness category — far more useful for surf &
//! wave research than the combined `Hs` in the plain realtime2 feed.
//!
//! Defaults to Monterey-area CDIP stations:
//! - **46114** (CDIP 158 – Pt. Sur Offshore)
//! - **46236** (CDIP 185 – Monterey Canyon Inner)
//! - **46042** (NOAA – Monterey — included for context as a 3m discus)
//!
//! Feed: <https://www.ndbc.noaa.gov/data/realtime2/{STATION}.spec>

use std::sync::Arc;

use anyhow::{Context, Result};
use crate::util::now_pacific;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cache::CacheStore;

pub const DEFAULT_STATIONS: &[(&str, &str)] = &[
    ("46114", "Pt. Sur Offshore (CDIP 158)"),
    ("46236", "Monterey Canyon Inner (CDIP 185)"),
    ("46042", "Monterey (NOAA 46042)"),
];

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaveBuoyRequest {
    /// Comma-separated NDBC station IDs (CDIP stations are NDBC-mirrored).
    /// If omitted, queries default Monterey-area waveriders
    /// (46114 / 46236 / 46042) and compares them.
    pub stations: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SpectralObservation {
    pub timestamp_utc: String,
    pub significant_height_m: Option<f64>,
    pub swell_height_m: Option<f64>,
    pub swell_period_s: Option<f64>,
    pub swell_direction: Option<String>, // compass label like "WNW"
    pub wind_wave_height_m: Option<f64>,
    pub wind_wave_period_s: Option<f64>,
    pub wind_wave_direction: Option<String>,
    pub steepness: Option<String>,
    pub average_period_s: Option<f64>,
    pub mean_wave_direction_deg: Option<f64>,
}

pub struct WaveBuoyService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl WaveBuoyService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    pub async fn get_wave_data(&self, stations: Option<&str>) -> Result<String> {
        let station_list: Vec<(String, String)> = if let Some(s) = stations {
            s.split(',')
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty())
                .map(|id| {
                    let label = DEFAULT_STATIONS
                        .iter()
                        .find(|(s, _)| *s == id)
                        .map(|(_, name)| name.to_string())
                        .unwrap_or_else(|| format!("Station {}", id));
                    (id, label)
                })
                .collect()
        } else {
            DEFAULT_STATIONS
                .iter()
                .map(|(s, n)| (s.to_string(), n.to_string()))
                .collect()
        };

        if station_list.is_empty() {
            anyhow::bail!("no stations to query");
        }

        let futures = station_list.iter().map(|(id, label)| {
            let http = self.http.clone();
            let cache = self.cache.clone();
            let id = id.clone();
            let label = label.clone();
            async move {
                let key = format!("wave_buoy:spec:{}", id);
                let id_for_fetch = id.clone();
                let result = cache
                    .get_or_fetch::<SpectralObservation, _, _>(&key, 1800, move || async move {
                        fetch_latest_spec(&http, &id_for_fetch).await
                    })
                    .await;
                (id, label, result)
            }
        });
        let results = futures_util::future::join_all(futures).await;
        Ok(format_results(&results))
    }
}

async fn fetch_latest_spec(
    http: &reqwest::Client,
    station: &str,
) -> Result<SpectralObservation> {
    let url = format!("https://www.ndbc.noaa.gov/data/realtime2/{}.spec", station);
    let resp = http
        .get(&url)
        .send()
        .await
        .context("NDBC spec HTTP request failed")?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!(
            "no .spec feed for station '{}'. Waverider / CDIP mirrored \
             stations with swell components only. See \
             https://www.ndbc.noaa.gov/",
            station
        );
    }
    if !resp.status().is_success() {
        anyhow::bail!("NDBC returned HTTP {}", resp.status());
    }
    let body = resp.text().await.context("reading NDBC spec body")?;
    parse_spec(&body)
}

/// Parse the `.spec` realtime file. Columns (v2):
/// `#YY MM DD hh mm WVHT SwH SwP WWH WWP SwD WWD STEEPNESS APD MWD`
fn parse_spec(body: &str) -> Result<SpectralObservation> {
    let mut lines = body.lines();
    let header = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("empty .spec body"))?;
    if !header.starts_with('#') {
        anyhow::bail!("unexpected .spec header: {}", header);
    }
    let cols: Vec<&str> = header[1..].split_whitespace().collect();
    let _units = lines.next();

    let idx = |name: &str| cols.iter().position(|c| c.eq_ignore_ascii_case(name));
    let i_yy = idx("YY").or_else(|| idx("YYYY"));
    let i_mm = idx("MM");
    let i_dd = idx("DD");
    let i_hh = idx("hh");
    let i_mn = idx("mm");
    let i_wvht = idx("WVHT");
    let i_swh = idx("SwH");
    let i_swp = idx("SwP");
    let i_wwh = idx("WWH");
    let i_wwp = idx("WWP");
    let i_swd = idx("SwD");
    let i_wwd = idx("WWD");
    let i_steep = idx("STEEPNESS");
    let i_apd = idx("APD");
    let i_mwd = idx("MWD");

    for line in lines {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let get = |i: Option<usize>| i.and_then(|j| fields.get(j).copied()).unwrap_or("");
        let parse_f = |i: Option<usize>| {
            let s = get(i);
            if s == "MM" || s.is_empty() {
                None
            } else {
                s.parse::<f64>().ok()
            }
        };
        let parse_str = |i: Option<usize>| {
            let s = get(i);
            if s.is_empty() || s == "MM" {
                None
            } else {
                Some(s.to_string())
            }
        };

        let (yy, mm, dd, hh, mn) = (get(i_yy), get(i_mm), get(i_dd), get(i_hh), get(i_mn));
        let ts = if !yy.is_empty() && !mm.is_empty() && !dd.is_empty() {
            format!("{}-{}-{} {}:{} UTC", yy, mm, dd, hh, mn)
        } else {
            "unknown".to_string()
        };

        // Return the first data row (newest)
        return Ok(SpectralObservation {
            timestamp_utc: ts,
            significant_height_m: parse_f(i_wvht),
            swell_height_m: parse_f(i_swh),
            swell_period_s: parse_f(i_swp),
            swell_direction: parse_str(i_swd),
            wind_wave_height_m: parse_f(i_wwh),
            wind_wave_period_s: parse_f(i_wwp),
            wind_wave_direction: parse_str(i_wwd),
            steepness: parse_str(i_steep),
            average_period_s: parse_f(i_apd),
            mean_wave_direction_deg: parse_f(i_mwd),
        });
    }

    anyhow::bail!("NDBC .spec feed had no observations")
}

fn m_to_ft(m: f64) -> f64 {
    m * 3.28084
}

fn format_results(results: &[(String, String, Result<SpectralObservation>)]) -> String {
    let mut out = String::from("# Wave Buoy — Spectral Summary\n\n");
    out.push_str(
        "Swell vs wind-wave breakdown from NDBC/CDIP waveriders. Swell is the \
         long-period energy (good for surf), wind-wave is the local chop.\n\n",
    );

    for (id, label, res) in results {
        out.push_str(&format!("## {} — `{}`\n", label, id));
        match res {
            Ok(obs) => {
                out.push_str(&format!("_{}_\n\n", obs.timestamp_utc));
                if let Some(hs) = obs.significant_height_m {
                    out.push_str(&format!(
                        "- **Significant wave height (Hs)**: {:.1} ft ({:.1} m)\n",
                        m_to_ft(hs),
                        hs
                    ));
                }
                if let Some(sh) = obs.swell_height_m {
                    let per = obs
                        .swell_period_s
                        .map(|p| format!(" · period {:.1} s", p))
                        .unwrap_or_default();
                    let dir = obs
                        .swell_direction
                        .as_deref()
                        .map(|d| format!(" · from {}", d))
                        .unwrap_or_default();
                    out.push_str(&format!(
                        "- **Swell**: {:.1} ft ({:.1} m){}{}\n",
                        m_to_ft(sh),
                        sh,
                        per,
                        dir
                    ));
                }
                if let Some(wwh) = obs.wind_wave_height_m {
                    let per = obs
                        .wind_wave_period_s
                        .map(|p| format!(" · period {:.1} s", p))
                        .unwrap_or_default();
                    let dir = obs
                        .wind_wave_direction
                        .as_deref()
                        .map(|d| format!(" · from {}", d))
                        .unwrap_or_default();
                    out.push_str(&format!(
                        "- **Wind wave**: {:.1} ft ({:.1} m){}{}\n",
                        m_to_ft(wwh),
                        wwh,
                        per,
                        dir
                    ));
                }
                if let Some(s) = &obs.steepness {
                    out.push_str(&format!("- **Steepness**: {}\n", s.to_lowercase()));
                }
                if let Some(apd) = obs.average_period_s {
                    out.push_str(&format!("- **Average wave period**: {:.1} s\n", apd));
                }
                out.push('\n');
            }
            Err(e) => {
                out.push_str(&format!("  ⚠ unavailable: {}\n\n", e));
            }
        }
    }

    out.push_str(&format!(
        "_Source: NDBC .spec feeds (CDIP-owned stations mirrored). Last updated: {}_\n",
        now_pacific().format("%-I:%M %p")
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_spec_sample() {
        let body = "#YY  MM DD hh mm WVHT  SwH  SwP  WWH  WWP SwD WWD  STEEPNESS  APD MWD\n\
                    #yr  mo dy hr mn    m    m  sec    m  sec  -   -          -  sec degT\n\
                    2026 04 17 15 50  1.6  1.2 11.1  0.8  5.4 WNW W    AVERAGE   6.5 295\n";
        let obs = parse_spec(body).unwrap();
        assert!((obs.significant_height_m.unwrap() - 1.6).abs() < 0.001);
        assert!((obs.swell_height_m.unwrap() - 1.2).abs() < 0.001);
        assert_eq!(obs.swell_direction.as_deref(), Some("WNW"));
        assert_eq!(obs.steepness.as_deref(), Some("AVERAGE"));
        assert_eq!(obs.mean_wave_direction_deg, Some(295.0));
    }

    #[test]
    fn parse_spec_missing_returns_none() {
        let body = "#YY  MM DD hh mm WVHT  SwH  SwP  WWH  WWP SwD WWD  STEEPNESS  APD MWD\n\
                    #yr  mo dy hr mn    m    m  sec    m  sec  -   -          -  sec degT\n\
                    2026 04 17 15 50   MM   MM   MM   MM   MM  MM  MM         MM   MM  MM\n";
        let obs = parse_spec(body).unwrap();
        assert!(obs.significant_height_m.is_none());
        assert!(obs.swell_direction.is_none());
    }

    #[test]
    fn format_results_renders_known_station() {
        let obs = SpectralObservation {
            timestamp_utc: "2026-04-17 15:50 UTC".to_string(),
            significant_height_m: Some(1.6),
            swell_height_m: Some(1.2),
            swell_period_s: Some(11.1),
            swell_direction: Some("WNW".to_string()),
            ..Default::default()
        };
        let out = format_results(&[(
            "46114".to_string(),
            "Pt. Sur Offshore (CDIP 158)".to_string(),
            Ok(obs),
        )]);
        assert!(out.contains("Pt. Sur Offshore"));
        assert!(out.contains("Swell"));
        assert!(out.contains("WNW"));
    }
}
