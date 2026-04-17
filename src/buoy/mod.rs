//! NDBC (National Data Buoy Center) real-time observations.
//!
//! Fetches the last-45-days realtime2 text feed for a single buoy station and
//! renders the most recent observation. Default station is **46042** Monterey
//! Bay — a NOAA 3-meter discus moored offshore of Santa Cruz. Other useful
//! nearby stations: 46092 (MBARI M1 - Monterey), 46114 (PT. SUR) and 46236
//! (MONTEREY CANYON INNER).
//!
//! Feed: <https://www.ndbc.noaa.gov/data/realtime2/{STATION}.txt>
//! Format reference: <https://www.ndbc.noaa.gov/faq/measdes.shtml>

use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Local;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cache::CacheStore;
use crate::util::degrees_to_compass;

const DEFAULT_STATION: &str = "46042";
const DEFAULT_STATION_NAME: &str = "Monterey Bay (46042)";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BuoyRequest {
    /// NDBC station ID (e.g. "46042", "46092"). Defaults to 46042 Monterey Bay.
    pub station: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BuoyObservation {
    pub timestamp_utc: String,
    pub wind_dir_deg: Option<f64>,
    pub wind_speed_ms: Option<f64>,
    pub wind_gust_ms: Option<f64>,
    pub wave_height_m: Option<f64>,
    pub dominant_period_s: Option<f64>,
    pub mean_wave_period_s: Option<f64>,
    pub mean_wave_dir_deg: Option<f64>,
    pub pressure_hpa: Option<f64>,
    pub air_temp_c: Option<f64>,
    pub water_temp_c: Option<f64>,
    pub dew_point_c: Option<f64>,
    pub pressure_tendency_hpa: Option<f64>,
}

pub struct BuoyService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl BuoyService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    pub async fn get_observations(&self, station: Option<&str>) -> Result<String> {
        let station = station.unwrap_or(DEFAULT_STATION).to_string();

        let cache_key = format!("buoy:ndbc:{}", station);
        let http = self.http.clone();
        let station_for_fetch = station.clone();
        let obs = self
            .cache
            .get_or_fetch::<Vec<BuoyObservation>, _, _>(&cache_key, 600, move || async move {
                fetch_observations(&http, &station_for_fetch).await
            })
            .await?;

        let name = if station == DEFAULT_STATION {
            DEFAULT_STATION_NAME.to_string()
        } else {
            format!("NDBC station {}", station)
        };
        Ok(format_observations(&name, &obs))
    }
}

async fn fetch_observations(
    http: &reqwest::Client,
    station: &str,
) -> Result<Vec<BuoyObservation>> {
    let url = format!("https://www.ndbc.noaa.gov/data/realtime2/{}.txt", station);
    let resp = http
        .get(&url)
        .send()
        .await
        .context("NDBC HTTP request failed")?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!(
            "NDBC station '{}' not found (404). Check the station ID at https://www.ndbc.noaa.gov/",
            station
        );
    }
    if !resp.status().is_success() {
        anyhow::bail!("NDBC returned HTTP {}", resp.status());
    }
    let body = resp.text().await.context("reading NDBC body")?;
    parse_realtime2(&body)
}

/// Parse the realtime2 .txt format. Returns observations newest-first.
fn parse_realtime2(body: &str) -> Result<Vec<BuoyObservation>> {
    let mut lines = body.lines();
    // First line: header with column names prefixed by "#".
    let header = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("empty NDBC body"))?;
    if !header.starts_with('#') {
        anyhow::bail!("unexpected NDBC header: {}", header);
    }
    let cols: Vec<&str> = header[1..].split_whitespace().collect();
    // Skip the units line (also starts with #)
    let _units = lines.next();

    let idx = |name: &str| cols.iter().position(|c| c.eq_ignore_ascii_case(name));
    let i_yy = idx("YY").or_else(|| idx("YYYY"));
    let i_mm = idx("MM");
    let i_dd = idx("DD");
    let i_hh = idx("hh");
    let i_mn = idx("mm");
    let i_wdir = idx("WDIR");
    let i_wspd = idx("WSPD");
    let i_gst = idx("GST");
    let i_wvht = idx("WVHT");
    let i_dpd = idx("DPD");
    let i_apd = idx("APD");
    let i_mwd = idx("MWD");
    let i_pres = idx("PRES");
    let i_atmp = idx("ATMP");
    let i_wtmp = idx("WTMP");
    let i_dewp = idx("DEWP");
    let i_ptdy = idx("PTDY");

    let mut out = Vec::new();
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

        let (yy, mm, dd, hh, mn) = (get(i_yy), get(i_mm), get(i_dd), get(i_hh), get(i_mn));
        let timestamp_utc = if !yy.is_empty() && !mm.is_empty() && !dd.is_empty() {
            format!("{}-{}-{} {}:{} UTC", yy, mm, dd, hh, mn)
        } else {
            "unknown".to_string()
        };

        out.push(BuoyObservation {
            timestamp_utc,
            wind_dir_deg: parse_f(i_wdir),
            wind_speed_ms: parse_f(i_wspd),
            wind_gust_ms: parse_f(i_gst),
            wave_height_m: parse_f(i_wvht),
            dominant_period_s: parse_f(i_dpd),
            mean_wave_period_s: parse_f(i_apd),
            mean_wave_dir_deg: parse_f(i_mwd),
            pressure_hpa: parse_f(i_pres),
            air_temp_c: parse_f(i_atmp),
            water_temp_c: parse_f(i_wtmp),
            dew_point_c: parse_f(i_dewp),
            pressure_tendency_hpa: parse_f(i_ptdy),
        });
    }

    if out.is_empty() {
        anyhow::bail!("NDBC feed returned no observation rows");
    }
    Ok(out)
}

fn m_to_ft(m: f64) -> f64 {
    m * 3.28084
}
fn ms_to_mph(ms: f64) -> f64 {
    ms * 2.23694
}
fn c_to_f(c: f64) -> f64 {
    c * 9.0 / 5.0 + 32.0
}

fn format_observations(name: &str, obs: &[BuoyObservation]) -> String {
    let mut out = format!("# Buoy observations — {}\n\n", name);
    let latest = &obs[0];
    out.push_str(&format!("**Observed**: {}\n\n", latest.timestamp_utc));

    // Wind
    if let Some(spd) = latest.wind_speed_ms {
        let mph = ms_to_mph(spd);
        let gust = latest
            .wind_gust_ms
            .map(|g| format!(" (gusts {:.0} mph)", ms_to_mph(g)))
            .unwrap_or_default();
        let dir = latest
            .wind_dir_deg
            .map(|d| format!(" from {}° ({})", d as i32, degrees_to_compass(d)))
            .unwrap_or_default();
        out.push_str(&format!(
            "- **Wind**: {:.0} mph{}{}\n",
            mph, gust, dir
        ));
    }

    // Waves
    if let Some(wh) = latest.wave_height_m {
        let ft = m_to_ft(wh);
        let dpd = latest
            .dominant_period_s
            .map(|p| format!(" · dominant period {:.1} s", p))
            .unwrap_or_default();
        let apd = latest
            .mean_wave_period_s
            .map(|p| format!(" · mean period {:.1} s", p))
            .unwrap_or_default();
        let mwd = latest
            .mean_wave_dir_deg
            .map(|d| format!(" · {}° ({})", d as i32, degrees_to_compass(d)))
            .unwrap_or_default();
        out.push_str(&format!(
            "- **Significant wave height**: {:.1} ft ({:.1} m){}{}{}\n",
            ft, wh, dpd, apd, mwd
        ));
    }

    if let Some(wt) = latest.water_temp_c {
        out.push_str(&format!(
            "- **Water temperature**: {:.1}°F ({:.1}°C)\n",
            c_to_f(wt),
            wt
        ));
    }
    if let Some(at) = latest.air_temp_c {
        out.push_str(&format!(
            "- **Air temperature**: {:.1}°F ({:.1}°C)\n",
            c_to_f(at),
            at
        ));
    }
    if let Some(dew) = latest.dew_point_c {
        out.push_str(&format!(
            "- **Dew point**: {:.1}°F ({:.1}°C)\n",
            c_to_f(dew),
            dew
        ));
    }
    if let Some(p) = latest.pressure_hpa {
        let tendency = latest
            .pressure_tendency_hpa
            .map(|t| {
                let sign = if t > 0.0 { "+" } else { "" };
                format!(" ({}{:.1} hPa / 3h)", sign, t)
            })
            .unwrap_or_default();
        out.push_str(&format!("- **Pressure**: {:.1} hPa{}\n", p, tendency));
    }

    // 3h trend: find observation ~3 hours earlier
    if obs.len() > 1 {
        if let (Some(w_now), Some(older)) = (latest.water_temp_c, obs.get(18)) {
            if let Some(w_old) = older.water_temp_c {
                let delta = w_now - w_old;
                out.push_str(&format!(
                    "\n_Water temp trend over ~3h: {:+.1}°C ({:+.1}°F)_\n",
                    delta,
                    delta * 9.0 / 5.0
                ));
            }
        }
    }

    out.push_str(&format!(
        "\n_Source: NDBC realtime2. Last updated: {}_\n",
        Local::now().format("%-I:%M %p")
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_realtime2_sample() {
        let body = "#YY  MM DD hh mm WDIR WSPD GST  WVHT   DPD   APD MWD   PRES  ATMP  WTMP  DEWP  VIS PTDY  TIDE\n\
                    #yr  mo dy hr mn degT m/s  m/s     m   sec   sec degT   hPa  degC  degC  degC  nmi  hPa    ft\n\
                    2026 04 17 15 50 290  5.2  6.4   1.6   9.1   6.5 295  1018.2  14.1  13.5   9.2   MM  +0.2    MM\n\
                    2026 04 17 15 40 280  5.0  6.0   1.5   9.0   6.4 294  1018.1  14.0  13.4   9.1   MM   MM    MM\n";
        let parsed = parse_realtime2(body).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].wind_dir_deg, Some(290.0));
        assert!((parsed[0].wave_height_m.unwrap() - 1.6).abs() < 0.001);
        assert_eq!(parsed[0].dew_point_c, Some(9.2));
        assert_eq!(parsed[0].pressure_tendency_hpa, Some(0.2));
    }

    #[test]
    fn parse_realtime2_handles_missing() {
        let body = "#YY  MM DD hh mm WDIR WSPD GST  WVHT   DPD   APD MWD   PRES  ATMP  WTMP  DEWP  VIS PTDY  TIDE\n\
                    #yr  mo dy hr mn degT m/s  m/s     m   sec   sec degT   hPa  degC  degC  degC  nmi  hPa    ft\n\
                    2026 04 17 15 50  MM   MM   MM     MM    MM    MM  MM      MM    MM    MM    MM   MM   MM    MM\n";
        let parsed = parse_realtime2(body).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].wind_speed_ms.is_none());
        assert!(parsed[0].wave_height_m.is_none());
    }

    #[test]
    fn format_observations_renders() {
        let obs = vec![BuoyObservation {
            timestamp_utc: "2026-04-17 15:50 UTC".to_string(),
            wind_dir_deg: Some(290.0),
            wind_speed_ms: Some(5.2),
            wave_height_m: Some(1.6),
            water_temp_c: Some(13.5),
            ..Default::default()
        }];
        let out = format_observations("Monterey Bay (46042)", &obs);
        assert!(out.contains("Monterey Bay"));
        assert!(out.contains("Wind"));
        assert!(out.contains("Significant wave height"));
        assert!(out.contains("Water temperature"));
    }
}
