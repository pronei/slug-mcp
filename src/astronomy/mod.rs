//! Sun, moon & UV data for Santa Cruz (or custom coordinates).
//!
//! Combines:
//! - Open-Meteo forecast API (sunrise/sunset + UV index)
//! - sunrise-sunset.org API (civil, nautical, astronomical twilight)
//! - Computed moon phase from synodic period

use std::fmt::Write;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, NaiveDateTime};
use chrono_tz::Tz;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cache::CacheStore;

/// Default Santa Cruz coordinates.
const DEFAULT_LAT: f64 = 36.9741;
const DEFAULT_LON: f64 = -122.0308;

// ─── Request ───

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SunMoonRequest {
    /// Latitude (default 36.9741, Santa Cruz).
    pub lat: Option<f64>,
    /// Longitude (default -122.0308, Santa Cruz).
    pub lon: Option<f64>,
    /// Days of forecast (1-7, default 3).
    pub days: Option<u32>,
}

// ─── Service ───

pub struct AstronomyService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl AstronomyService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    pub async fn get_sun_moon(&self, req: &SunMoonRequest) -> Result<String> {
        let lat = req.lat.unwrap_or(DEFAULT_LAT);
        let lon = req.lon.unwrap_or(DEFAULT_LON);
        let days = req.days.unwrap_or(3).clamp(1, 7);

        // Fetch Open-Meteo and sunrise-sunset.org in parallel.
        let open_meteo_fut = self.load_open_meteo(lat, lon, days);
        let twilight_fut = self.load_twilight(lat, lon);
        let (open_meteo_res, twilight_res) =
            futures_util::future::join(open_meteo_fut, twilight_fut).await;

        let open_meteo = open_meteo_res.context("Open-Meteo forecast fetch failed")?;
        // Twilight is best-effort — graceful degradation if the API is down.
        let twilight = match twilight_res {
            Ok(t) => Some(t),
            Err(e) => {
                tracing::warn!("sunrise-sunset.org fetch failed: {}", e);
                None
            }
        };

        Ok(format_output(&open_meteo, twilight.as_ref(), lat, lon, days))
    }

    async fn load_open_meteo(&self, lat: f64, lon: f64, days: u32) -> Result<OpenMeteoResponse> {
        let key = format!("astro:openmeteo:{:.3}:{:.3}:{}", lat, lon, days);
        let http = self.http.clone();
        self.cache
            .get_or_fetch::<OpenMeteoResponse, _, _>(&key, 3600, move || async move {
                fetch_open_meteo(&http, lat, lon, days).await
            })
            .await
    }

    async fn load_twilight(&self, lat: f64, lon: f64) -> Result<TwilightData> {
        let key = format!("astro:twilight:{:.3}:{:.3}", lat, lon);
        let http = self.http.clone();
        self.cache
            .get_or_fetch::<TwilightData, _, _>(&key, 3600, move || async move {
                fetch_twilight(&http, lat, lon).await
            })
            .await
    }
}

// ─── Open-Meteo types ───

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenMeteoResponse {
    pub current: Option<OpenMeteoCurrent>,
    pub daily: Option<OpenMeteoDaily>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenMeteoCurrent {
    pub time: Option<String>,
    pub uv_index: Option<f64>,
    pub uv_index_clear_sky: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenMeteoDaily {
    pub time: Vec<String>,
    pub sunrise: Vec<String>,
    pub sunset: Vec<String>,
    pub uv_index_max: Vec<f64>,
    pub uv_index_clear_sky_max: Vec<f64>,
}

async fn fetch_open_meteo(
    http: &reqwest::Client,
    lat: f64,
    lon: f64,
    days: u32,
) -> Result<OpenMeteoResponse> {
    let url = format!(
        "https://api.open-meteo.com/v1/forecast\
         ?latitude={lat}&longitude={lon}\
         &daily=sunrise,sunset,uv_index_max,uv_index_clear_sky_max\
         &current=uv_index,uv_index_clear_sky\
         &timezone=America%2FLos_Angeles\
         &forecast_days={days}"
    );
    let resp = http
        .get(&url)
        .send()
        .await
        .context("Open-Meteo HTTP request failed")?;
    if !resp.status().is_success() {
        anyhow::bail!("Open-Meteo returned HTTP {}", resp.status());
    }
    resp.json::<OpenMeteoResponse>()
        .await
        .context("parsing Open-Meteo JSON")
}

// ─── sunrise-sunset.org types ───

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SunriseSunsetResponse {
    results: SunriseSunsetResults,
    status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SunriseSunsetResults {
    civil_twilight_begin: String,
    civil_twilight_end: String,
    nautical_twilight_begin: String,
    nautical_twilight_end: String,
    astronomical_twilight_begin: String,
    astronomical_twilight_end: String,
}

/// Parsed twilight times in Pacific.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwilightData {
    pub civil_begin: String,
    pub civil_end: String,
    pub nautical_begin: String,
    pub nautical_end: String,
    pub astro_begin: String,
    pub astro_end: String,
}

async fn fetch_twilight(http: &reqwest::Client, lat: f64, lon: f64) -> Result<TwilightData> {
    let url = format!(
        "https://api.sunrise-sunset.org/json?lat={lat}&lng={lon}&formatted=0"
    );
    let resp = http
        .get(&url)
        .send()
        .await
        .context("sunrise-sunset.org HTTP request failed")?;
    if !resp.status().is_success() {
        anyhow::bail!("sunrise-sunset.org returned HTTP {}", resp.status());
    }
    let body = resp
        .json::<SunriseSunsetResponse>()
        .await
        .context("parsing sunrise-sunset.org JSON")?;
    if body.status != "OK" {
        anyhow::bail!("sunrise-sunset.org status: {}", body.status);
    }
    let r = &body.results;
    Ok(TwilightData {
        civil_begin: utc_to_pacific_display(&r.civil_twilight_begin)?,
        civil_end: utc_to_pacific_display(&r.civil_twilight_end)?,
        nautical_begin: utc_to_pacific_display(&r.nautical_twilight_begin)?,
        nautical_end: utc_to_pacific_display(&r.nautical_twilight_end)?,
        astro_begin: utc_to_pacific_display(&r.astronomical_twilight_begin)?,
        astro_end: utc_to_pacific_display(&r.astronomical_twilight_end)?,
    })
}

/// Parse an RFC 3339 timestamp and format it in Pacific time as "h:MM AM/PM".
fn utc_to_pacific_display(rfc3339: &str) -> Result<String> {
    let dt = DateTime::parse_from_rfc3339(rfc3339)
        .with_context(|| format!("invalid RFC 3339: {}", rfc3339))?;
    let pacific: DateTime<Tz> = dt.with_timezone(&chrono_tz::US::Pacific);
    Ok(pacific.format("%-I:%M %p").to_string())
}

// ─── Moon phase ───

const SYNODIC_PERIOD: f64 = 29.530_588_53;
/// Known new moon: January 6, 2000, 12:14 UTC — Julian Day 2451550.1.
const NEW_MOON_EPOCH_JD: f64 = 2_451_550.1;

/// Compute moon phase for a given date.
///
/// Returns `(phase_fraction, phase_name, illumination_percent)`.
/// `phase_fraction` ranges from 0.0 (new moon) to 1.0 (next new moon).
fn moon_phase(date: NaiveDate) -> (f64, &'static str, u8) {
    // Julian Day at midnight UTC for the date.
    let epoch = NaiveDate::from_ymd_opt(1, 1, 1).unwrap();
    let days_from_ce = (date - epoch).num_days();
    let jd = days_from_ce as f64 + 1_721_425.5;
    let phase = ((jd - NEW_MOON_EPOCH_JD) / SYNODIC_PERIOD).rem_euclid(1.0);

    let name = match phase {
        p if p < 0.0625 => "New Moon",
        p if p < 0.1875 => "Waxing Crescent",
        p if p < 0.3125 => "First Quarter",
        p if p < 0.4375 => "Waxing Gibbous",
        p if p < 0.5625 => "Full Moon",
        p if p < 0.6875 => "Waning Gibbous",
        p if p < 0.8125 => "Last Quarter",
        p if p < 0.9375 => "Waning Crescent",
        _ => "New Moon",
    };

    // Illumination: 0% at new, 100% at full.
    let diff: f64 = 2.0 * phase - 1.0;
    let illum = ((1.0 - diff.abs()) * 100.0).round() as u8;

    (phase, name, illum)
}

fn moon_emoji(name: &str) -> &'static str {
    match name {
        "New Moon" => "🌑",
        "Waxing Crescent" => "🌒",
        "First Quarter" => "🌓",
        "Waxing Gibbous" => "🌔",
        "Full Moon" => "🌕",
        "Waning Gibbous" => "🌖",
        "Last Quarter" => "🌗",
        "Waning Crescent" => "🌘",
        _ => "",
    }
}

// ─── UV helpers ───

fn uv_category(uv: f64) -> &'static str {
    match uv as u32 {
        0..=2 => "Low",
        3..=5 => "Moderate",
        6..=7 => "High",
        8..=10 => "Very High",
        _ => "Extreme",
    }
}

// ─── Formatting ───

fn format_output(
    om: &OpenMeteoResponse,
    twilight: Option<&TwilightData>,
    _lat: f64,
    _lon: f64,
    _days: u32,
) -> String {
    let now = crate::util::now_pacific();
    let mut out = String::from("# Sun, Moon & UV — Santa Cruz\n\n");

    let daily = match &om.daily {
        Some(d) if !d.time.is_empty() => d,
        _ => {
            writeln!(out, "No forecast data available from Open-Meteo.").unwrap();
            writeln!(
                out,
                "\n_Source: Open-Meteo. Last updated: {}_",
                now.format("%-I:%M %p")
            )
            .unwrap();
            return out;
        }
    };

    // ── Today ──
    let today_date_str = &daily.time[0];
    if let Ok(today_date) = NaiveDate::parse_from_str(today_date_str, "%Y-%m-%d") {
        let day_label = today_date.format("%a, %b %-d").to_string();
        writeln!(out, "## Today — {}", day_label).unwrap();

        // Sunrise / sunset / day length
        let sunrise_str = format_local_time(&daily.sunrise[0]);
        let sunset_str = format_local_time(&daily.sunset[0]);
        let day_length = compute_day_length(&daily.sunrise[0], &daily.sunset[0]);

        write!(
            out,
            "- **Sunrise**: {} · **Sunset**: {}",
            sunrise_str, sunset_str
        )
        .unwrap();
        if let Some(dl) = day_length {
            writeln!(out, " · **Day length**: {}", dl).unwrap();
        } else {
            writeln!(out).unwrap();
        }

        // Twilight (only for today)
        if let Some(tw) = twilight {
            writeln!(
                out,
                "- **Civil twilight**: {} – {}",
                tw.civil_begin, tw.civil_end
            )
            .unwrap();
            writeln!(
                out,
                "- **Nautical twilight**: {} – {}",
                tw.nautical_begin, tw.nautical_end
            )
            .unwrap();
            writeln!(
                out,
                "- **Astronomical twilight**: {} – {}",
                tw.astro_begin, tw.astro_end
            )
            .unwrap();
        } else {
            writeln!(out, "- _Twilight times unavailable (sunrise-sunset.org unreachable)_")
                .unwrap();
        }

        // Moon for today
        let (_, moon_name, moon_illum) = moon_phase(today_date);
        writeln!(
            out,
            "- **Moon**: {} ({}%) {}",
            moon_name,
            moon_illum,
            moon_emoji(moon_name)
        )
        .unwrap();
    }

    // ── UV Index ──
    writeln!(out).unwrap();
    writeln!(out, "## UV Index").unwrap();

    if let Some(current) = &om.current {
        if let Some(uv) = current.uv_index {
            let cat = uv_category(uv);
            let clear_sky = current
                .uv_index_clear_sky
                .map(|cs| format!(" · Clear-sky: {:.1}", cs))
                .unwrap_or_default();
            writeln!(out, "- **Current**: {:.1} ({}){}", uv, cat, clear_sky).unwrap();
        }
    }

    if !daily.uv_index_max.is_empty() {
        let today_max = daily.uv_index_max[0];
        writeln!(
            out,
            "- **Today max**: {:.1} ({})",
            today_max,
            uv_category(today_max)
        )
        .unwrap();
    }
    if daily.uv_index_max.len() > 1 {
        let tomorrow_max = daily.uv_index_max[1];
        writeln!(
            out,
            "- **Tomorrow max**: {:.1} ({})",
            tomorrow_max,
            uv_category(tomorrow_max)
        )
        .unwrap();
    }

    // ── Remaining days ──
    for i in 1..daily.time.len() {
        let date_str = &daily.time[i];
        if let Ok(date) = NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
            let label = if i == 1 {
                format!("Tomorrow — {}", date.format("%a, %b %-d"))
            } else {
                date.format("%a, %b %-d").to_string()
            };
            writeln!(out).unwrap();
            writeln!(out, "## {}", label).unwrap();

            let sunrise_str = format_local_time(&daily.sunrise[i]);
            let sunset_str = format_local_time(&daily.sunset[i]);
            writeln!(
                out,
                "- **Sunrise**: {} · **Sunset**: {}",
                sunrise_str, sunset_str
            )
            .unwrap();

            let (_, moon_name, moon_illum) = moon_phase(date);
            writeln!(
                out,
                "- **Moon**: {} ({}%) {}",
                moon_name,
                moon_illum,
                moon_emoji(moon_name)
            )
            .unwrap();
        }
    }

    writeln!(out).unwrap();
    writeln!(
        out,
        "_Source: Open-Meteo + sunrise-sunset.org. Last updated: {}_",
        now.format("%-I:%M %p")
    )
    .unwrap();
    out
}

/// Format an Open-Meteo local time string ("2026-04-27T06:17") as "6:17 AM".
fn format_local_time(local: &str) -> String {
    NaiveDateTime::parse_from_str(local, "%Y-%m-%dT%H:%M")
        .map(|dt| dt.format("%-I:%M %p").to_string())
        .unwrap_or_else(|_| local.to_string())
}

/// Compute day length as "Xh Ym" from two local time strings.
fn compute_day_length(sunrise: &str, sunset: &str) -> Option<String> {
    let sr = NaiveDateTime::parse_from_str(sunrise, "%Y-%m-%dT%H:%M").ok()?;
    let ss = NaiveDateTime::parse_from_str(sunset, "%Y-%m-%dT%H:%M").ok()?;
    let diff = ss.signed_duration_since(sr);
    let total_minutes = diff.num_minutes();
    if total_minutes < 0 {
        return None;
    }
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;
    Some(format!("{}h {:02}m", hours, minutes))
}

// ─── Tests ───

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moon_phase_known_dates() {
        // January 29, 2025 — known new moon
        let date = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
        let (phase, name, illum) = moon_phase(date);
        assert!(
            phase < 0.07 || phase > 0.93,
            "expected near-new for 2025-01-29, got phase={:.3}",
            phase
        );
        assert!(
            name == "New Moon" || name == "Waning Crescent" || name == "Waxing Crescent",
            "got {}",
            name
        );
        assert!(illum < 15, "expected low illumination, got {}%", illum);

        // January 13, 2025 — known full moon (Wolf Moon)
        let date = NaiveDate::from_ymd_opt(2025, 1, 13).unwrap();
        let (phase, name, illum) = moon_phase(date);
        assert!(
            (phase - 0.5).abs() < 0.07,
            "expected near-full for 2025-01-13, got phase={:.3}",
            phase
        );
        assert!(
            name == "Full Moon" || name == "Waxing Gibbous" || name == "Waning Gibbous",
            "got {}",
            name
        );
        assert!(illum > 85, "expected high illumination, got {}%", illum);
    }

    #[test]
    fn uv_category_thresholds() {
        assert_eq!(uv_category(0.0), "Low");
        assert_eq!(uv_category(1.5), "Low");
        assert_eq!(uv_category(2.9), "Low");
        assert_eq!(uv_category(3.0), "Moderate");
        assert_eq!(uv_category(5.9), "Moderate");
        assert_eq!(uv_category(6.0), "High");
        assert_eq!(uv_category(7.9), "High");
        assert_eq!(uv_category(8.0), "Very High");
        assert_eq!(uv_category(10.5), "Very High");
        assert_eq!(uv_category(11.0), "Extreme");
        assert_eq!(uv_category(14.0), "Extreme");
    }

    #[test]
    fn format_output_renders() {
        let om = OpenMeteoResponse {
            current: Some(OpenMeteoCurrent {
                time: Some("2026-04-27T14:00".to_string()),
                uv_index: Some(3.90),
                uv_index_clear_sky: Some(7.50),
            }),
            daily: Some(OpenMeteoDaily {
                time: vec!["2026-04-27".to_string(), "2026-04-28".to_string()],
                sunrise: vec![
                    "2026-04-27T06:17".to_string(),
                    "2026-04-28T06:16".to_string(),
                ],
                sunset: vec![
                    "2026-04-27T19:53".to_string(),
                    "2026-04-28T19:54".to_string(),
                ],
                uv_index_max: vec![4.25, 7.40],
                uv_index_clear_sky_max: vec![7.50, 8.10],
            }),
        };
        let twilight = TwilightData {
            civil_begin: "5:50 AM".to_string(),
            civil_end: "8:21 PM".to_string(),
            nautical_begin: "5:17 AM".to_string(),
            nautical_end: "8:54 PM".to_string(),
            astro_begin: "4:42 AM".to_string(),
            astro_end: "9:29 PM".to_string(),
        };

        let output = format_output(&om, Some(&twilight), DEFAULT_LAT, DEFAULT_LON, 2);

        // Verify structure
        assert!(output.contains("# Sun, Moon & UV"));
        assert!(output.contains("## Today"));
        assert!(output.contains("**Sunrise**: 6:17 AM"));
        assert!(output.contains("**Sunset**: 7:53 PM"));
        assert!(output.contains("Day length"));
        assert!(output.contains("Civil twilight"));
        assert!(output.contains("Nautical twilight"));
        assert!(output.contains("Astronomical twilight"));
        assert!(output.contains("**Moon**:"));
        assert!(output.contains("## UV Index"));
        assert!(output.contains("**Current**: 3.9 (Moderate)"));
        assert!(output.contains("Clear-sky: 7.5"));
        assert!(output.contains("**Today max**: 4.2 (Moderate)"));
        assert!(output.contains("**Tomorrow max**: 7.4 (High)"));
        assert!(output.contains("## Tomorrow"));
        assert!(output.contains("**Sunrise**: 6:16 AM"));
        assert!(output.contains("Source: Open-Meteo + sunrise-sunset.org"));
    }

    #[test]
    fn format_output_without_twilight() {
        let om = OpenMeteoResponse {
            current: Some(OpenMeteoCurrent {
                time: Some("2026-04-27T14:00".to_string()),
                uv_index: Some(3.90),
                uv_index_clear_sky: Some(7.50),
            }),
            daily: Some(OpenMeteoDaily {
                time: vec!["2026-04-27".to_string()],
                sunrise: vec!["2026-04-27T06:17".to_string()],
                sunset: vec!["2026-04-27T19:53".to_string()],
                uv_index_max: vec![4.25],
                uv_index_clear_sky_max: vec![7.50],
            }),
        };

        let output = format_output(&om, None, DEFAULT_LAT, DEFAULT_LON, 1);

        assert!(output.contains("Twilight times unavailable"));
        assert!(!output.contains("Civil twilight"));
        assert!(output.contains("**Sunrise**: 6:17 AM"));
    }

    #[test]
    fn moon_emoji_mapping() {
        assert_eq!(moon_emoji("New Moon"), "🌑");
        assert_eq!(moon_emoji("Full Moon"), "🌕");
        assert_eq!(moon_emoji("First Quarter"), "🌓");
        assert_eq!(moon_emoji("Waning Crescent"), "🌘");
    }

    #[test]
    fn compute_day_length_basic() {
        let dl = compute_day_length("2026-04-27T06:17", "2026-04-27T19:53").unwrap();
        assert_eq!(dl, "13h 36m");
    }

    #[test]
    fn format_local_time_basic() {
        assert_eq!(format_local_time("2026-04-27T06:17"), "6:17 AM");
        assert_eq!(format_local_time("2026-04-27T19:53"), "7:53 PM");
        assert_eq!(format_local_time("2026-04-27T00:05"), "12:05 AM");
        assert_eq!(format_local_time("2026-04-27T12:00"), "12:00 PM");
    }

    #[test]
    fn utc_to_pacific_display_basic() {
        // 2026-04-27T12:50:18+00:00 => Pacific is UTC-7 (PDT in April) => 5:50 AM
        let result = utc_to_pacific_display("2026-04-27T12:50:18+00:00").unwrap();
        assert_eq!(result, "5:50 AM");
    }
}
