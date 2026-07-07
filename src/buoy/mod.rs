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

use crate::util::now_pacific;
use anyhow::{Context, Result};
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

/// NDBC station IDs are short alphanumeric codes (typically 5 chars, e.g.
/// "46042"). Reject anything else before it reaches the URL path so a crafted
/// `station` can't traverse paths or inject extra request targets.
fn validate_station(station: &str) -> Result<()> {
    let ok = (4..=6).contains(&station.len()) && station.chars().all(|c| c.is_ascii_alphanumeric());
    if !ok {
        anyhow::bail!(
            "invalid NDBC station ID '{}': expected 4-6 alphanumeric characters (e.g. 46042)",
            station
        );
    }
    Ok(())
}

async fn fetch_observations(http: &reqwest::Client, station: &str) -> Result<Vec<BuoyObservation>> {
    validate_station(station)?;
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
    let mut obs = parse_realtime2(&body)?;
    // Only rows 0 (latest) and ~18 (the ~3h-earlier trend row) are ever read,
    // so cap the cached value at 24 rows instead of all ~2,500 45-day rows.
    obs.truncate(24);
    Ok(obs)
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
    // Month "MM" vs minute "mm" differ only by case — must match exactly here.
    let i_mm = cols.iter().position(|c| *c == "MM");
    let i_dd = idx("DD");
    let i_hh = idx("hh");
    let i_mn = cols.iter().position(|c| *c == "mm");
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
        out.push_str(&format!("- **Wind**: {:.0} mph{}{}\n", mph, gust, dir));
    }

    // Wave sensors report ~2/hour vs 10-min met rows, so the newest row often
    // has MM waves — use the newest row within the last hour that has them.
    if let Some(row) = obs.iter().take(6).find(|o| o.wave_height_m.is_some())
        && let Some(wh) = row.wave_height_m
    {
        let ft = m_to_ft(wh);
        let dpd = row
            .dominant_period_s
            .map(|p| format!(" · dominant period {:.1} s", p))
            .unwrap_or_default();
        let apd = row
            .mean_wave_period_s
            .map(|p| format!(" · mean period {:.1} s", p))
            .unwrap_or_default();
        let mwd = row
            .mean_wave_dir_deg
            .map(|d| format!(" · {}° ({})", d as i32, degrees_to_compass(d)))
            .unwrap_or_default();
        let when = if row.timestamp_utc != latest.timestamp_utc {
            format!(" (at {})", row.timestamp_utc)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "- **Significant wave height**: {:.1} ft ({:.1} m){}{}{}{}\n",
            ft, wh, dpd, apd, mwd, when
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
    if obs.len() > 1
        && let (Some(w_now), Some(older)) = (latest.water_temp_c, obs.get(18))
        && let Some(w_old) = older.water_temp_c
    {
        let delta = w_now - w_old;
        out.push_str(&format!(
            "\n_Water temp trend over ~3h: {:+.1}°C ({:+.1}°F)_\n",
            delta,
            delta * 9.0 / 5.0
        ));
    }

    out.push_str(&format!(
        "\n_Source: NDBC realtime2. Last updated: {}_\n",
        now_pacific().format("%-I:%M %p")
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Live capture 2026-07-07, station 46042.
    const FIXTURE: &str = include_str!("fixtures/46042.txt");

    #[test]
    fn parse_realtime2_fixture() {
        let parsed = parse_realtime2(FIXTURE).unwrap();
        assert_eq!(parsed.len(), 10);
        let latest = &parsed[0];
        assert_eq!(latest.timestamp_utc, "2026-07-07 13:30 UTC");
        assert_eq!(latest.wind_dir_deg, Some(330.0));
        assert_eq!(latest.wind_speed_ms, Some(7.0));
        assert_eq!(latest.wind_gust_ms, Some(9.0));
        // Wave sensors report ~2/hour, so the newest 10-min met row has MM waves.
        assert!(latest.wave_height_m.is_none());
        assert_eq!(latest.pressure_hpa, Some(1016.6));
        assert_eq!(latest.air_temp_c, Some(13.9));
        assert_eq!(latest.water_temp_c, Some(15.6));
        assert_eq!(latest.dew_point_c, Some(12.5));
        // 13:20 row carries the wave sample.
        assert!((parsed[1].wave_height_m.unwrap() - 1.7).abs() < 0.001);
        assert_eq!(parsed[1].dominant_period_s, Some(12.0));
        assert_eq!(parsed[1].mean_wave_dir_deg, Some(307.0));
        // "+0.0" pressure tendency parses despite the sign prefix.
        assert_eq!(parsed[3].pressure_tendency_hpa, Some(0.0));
    }

    #[test]
    fn parse_realtime2_empty_body_errs() {
        // format_observations indexes obs[0]; the parser refusing empty input is
        // the invariant that keeps that path panic-free.
        let err = parse_realtime2("").unwrap_err();
        assert!(err.to_string().contains("empty NDBC body"));
    }

    #[test]
    fn parse_realtime2_header_only_errs() {
        let body = "#YY  MM DD hh mm WDIR WSPD GST  WVHT   DPD   APD MWD   PRES  ATMP  WTMP  DEWP  VIS PTDY  TIDE\n";
        let err = parse_realtime2(body).unwrap_err();
        assert!(err.to_string().contains("no observation rows"));
    }

    #[test]
    fn parse_realtime2_header_and_units_only_errs() {
        let body = "#YY  MM DD hh mm WDIR WSPD GST  WVHT   DPD   APD MWD   PRES  ATMP  WTMP  DEWP  VIS PTDY  TIDE\n\
                    #yr  mo dy hr mn degT m/s  m/s     m   sec   sec degT   hPa  degC  degC  degC  nmi  hPa    ft\n";
        let err = parse_realtime2(body).unwrap_err();
        assert!(err.to_string().contains("no observation rows"));
    }

    #[test]
    fn parse_realtime2_html_page_errs() {
        let err = parse_realtime2("<html><body>404</body></html>").unwrap_err();
        assert!(err.to_string().contains("unexpected NDBC header"));
    }

    #[test]
    fn parse_realtime2_short_row_no_panic() {
        let body = "#YY  MM DD hh mm WDIR WSPD GST  WVHT   DPD   APD MWD   PRES  ATMP  WTMP  DEWP  VIS PTDY  TIDE\n\
                    #yr  mo dy hr mn degT m/s  m/s     m   sec   sec degT   hPa  degC  degC  degC  nmi  hPa    ft\n\
                    2026 07\n";
        let parsed = parse_realtime2(body).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].timestamp_utc, "unknown");
        assert!(parsed[0].wind_speed_ms.is_none());
        // Formatting a degenerate row must not panic either.
        let out = format_observations("test", &parsed);
        assert!(out.contains("unknown"));
    }

    #[test]
    fn parse_realtime2_truncated_download_no_panic() {
        // Simulate a download cut mid-row (rows are newest-first, so only the
        // oldest row is degenerate).
        let cut = &FIXTURE[..FIXTURE.len() - 40];
        let parsed = parse_realtime2(cut).unwrap();
        assert!(!parsed.is_empty());
        assert_eq!(parsed[0].timestamp_utc, "2026-07-07 13:30 UTC");
    }

    #[test]
    fn format_observations_falls_back_to_newest_wave_row() {
        // Newest row has MM waves (live cadence); wave line must come from the
        // newest row that has them, labeled with its own timestamp.
        let obs = parse_realtime2(FIXTURE).unwrap();
        let out = format_observations("Monterey Bay (46042)", &obs);
        assert!(
            out.contains("Significant wave height"),
            "wave line missing:\n{}",
            out
        );
        assert!(out.contains("(1.7 m)"), "wave height missing:\n{}", out);
        assert!(
            out.contains("13:20"),
            "wave-row timestamp missing:\n{}",
            out
        );
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
