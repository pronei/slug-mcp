//! Marine forecast + surf spot comparison for Santa Cruz.
//!
//! Uses the Open-Meteo marine and forecast APIs (no auth, non-commercial).

pub mod open_meteo;
pub mod spots;

use std::sync::Arc;

use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::cache::CacheStore;
use crate::util::degrees_to_compass;
use open_meteo::{ForecastResponse, MarineHourly, MarineResponse};
use spots::SurfSpot;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SurfConditionsRequest {
    /// Surf spot name or slug (e.g., "Steamer Lane", "pleasure-point", "Cowell's"). If omitted and no lat/lon given, compares all known Santa Cruz spots.
    pub spot: Option<String>,
    /// Custom latitude (use with lon). Ignored if `spot` is set.
    pub lat: Option<f64>,
    /// Custom longitude (use with lat). Ignored if `spot` is set.
    pub lon: Option<f64>,
    /// Optional display label for custom coordinates (e.g., "Twin Lakes State Beach").
    pub label: Option<String>,
    /// Append an hourly wave/swell forecast table for the next N hours (1-24).
    /// Omit for the current-conditions snapshot only. Applies to a single spot
    /// or custom coordinates; ignored in the all-spots comparison.
    pub forecast_hours: Option<u32>,
}

pub struct MarineService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SpotConditions {
    marine: MarineResponse,
    wind: ForecastResponse,
}

impl MarineService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    /// Compare all known SC surf spots, return conditions for a single named
    /// spot, or fetch conditions for custom lat/lon coordinates. When
    /// `forecast_hours` is set, single-spot/custom output gains an hourly
    /// forecast table (the data ships in the same Open-Meteo response, so
    /// this costs no extra fetch).
    pub async fn get_surf_conditions(
        &self,
        spot_query: Option<&str>,
        lat: Option<f64>,
        lon: Option<f64>,
        label: Option<&str>,
        forecast_hours: Option<u32>,
    ) -> Result<String> {
        let hours = forecast_hours.map(|h| h.clamp(1, 24) as usize);
        if let Some(q) = spot_query {
            let spot = match spots::find(q) {
                Some(s) => s,
                None => {
                    anyhow::bail!(
                        "Surf spot '{}' not found. Known spots: {}",
                        q,
                        spots::names_list()
                    );
                }
            };
            let conditions = self.load_spot(spot).await?;
            return Ok(format_single_spot(spot, &conditions, hours));
        }

        // Custom coordinates
        if let (Some(lat), Some(lon)) = (lat, lon) {
            let name = label.unwrap_or("Custom spot");
            let slug = format!("custom-{:.4}-{:.4}", lat, lon);
            let conditions = self.load_by_coords(&slug, lat, lon).await?;
            return Ok(format_custom_spot(name, lat, lon, &conditions, hours));
        }

        // No spot filter: fetch all known spots in parallel
        let futures = spots::SURF_SPOTS.iter().map(|spot| async move {
            let result = self.load_spot(spot).await;
            (spot, result)
        });
        let results: Vec<(&'static SurfSpot, Result<SpotConditions>)> =
            futures_util::future::join_all(futures).await;

        Ok(format_all_spots(&results))
    }

    async fn load_spot(&self, spot: &SurfSpot) -> Result<SpotConditions> {
        let key = format!("marine:spot:{}", spot.slug);
        let http = self.http.clone();
        let lat = spot.lat;
        let lon = spot.lon;
        self.cache
            .get_or_fetch::<SpotConditions, _, _>(&key, 1800, move || async move {
                let (marine, wind) = futures_util::future::join(
                    open_meteo::get_marine(&http, lat, lon),
                    open_meteo::get_forecast(&http, lat, lon),
                )
                .await;
                Ok(SpotConditions {
                    marine: marine?,
                    wind: wind?,
                })
            })
            .await
    }

    async fn load_by_coords(&self, slug: &str, lat: f64, lon: f64) -> Result<SpotConditions> {
        let key = format!("marine:spot:{}", slug);
        let http = self.http.clone();
        self.cache
            .get_or_fetch::<SpotConditions, _, _>(&key, 1800, move || async move {
                let (marine, wind) = futures_util::future::join(
                    open_meteo::get_marine(&http, lat, lon),
                    open_meteo::get_forecast(&http, lat, lon),
                )
                .await;
                Ok(SpotConditions {
                    marine: marine?,
                    wind: wind?,
                })
            })
            .await
    }

}

// ───── formatting ─────

fn m_to_ft(m: f64) -> f64 {
    m * 3.28084
}

fn format_single_spot(spot: &SurfSpot, c: &SpotConditions, hours: Option<usize>) -> String {
    let mut out = format!("# {} ({})\n\n", spot.name, spot.slug);
    out.push_str(&format!("_{}_\n\n", spot.notes));
    write_spot_body(&mut out, c);
    if let (Some(hours), Some(hourly)) = (hours, &c.marine.hourly) {
        out.push('\n');
        write_hourly_table(&mut out, hourly, hours);
    }
    let now = crate::util::now_pacific();
    out.push_str(&format!(
        "\n_Source: Open-Meteo marine + forecast. Last updated: {}_\n",
        now.format("%-I:%M %p")
    ));
    out
}

fn format_custom_spot(
    name: &str,
    lat: f64,
    lon: f64,
    c: &SpotConditions,
    hours: Option<usize>,
) -> String {
    let mut out = format!("# {} ({:.4}, {:.4})\n\n", name, lat, lon);
    write_spot_body(&mut out, c);
    if let (Some(hours), Some(hourly)) = (hours, &c.marine.hourly) {
        out.push('\n');
        write_hourly_table(&mut out, hourly, hours);
    }
    let now = crate::util::now_pacific();
    out.push_str(&format!(
        "\n_Source: Open-Meteo marine + forecast. Last updated: {}_\n",
        now.format("%-I:%M %p")
    ));
    out
}

fn format_all_spots(results: &[(&'static SurfSpot, Result<SpotConditions>)]) -> String {
    let mut out = String::from("# Santa Cruz Surf Conditions\n\n");
    for (spot, res) in results {
        match res {
            Ok(c) => {
                out.push_str(&format!("## {} ({})\n", spot.name, spot.slug));
                out.push_str(&format!("_{}_\n\n", spot.notes));
                write_spot_body(&mut out, c);
                out.push('\n');
            }
            Err(e) => {
                out.push_str(&format!("## {} ({})\n", spot.name, spot.slug));
                out.push_str(&format!("  ⚠ conditions unavailable: {}\n\n", e));
            }
        }
    }
    let now = crate::util::now_pacific();
    out.push_str(&format!(
        "_Source: Open-Meteo marine + forecast. Last updated: {}_\n",
        now.format("%-I:%M %p")
    ));
    out
}

fn write_spot_body(out: &mut String, c: &SpotConditions) {
    if let Some(current) = &c.marine.current {
        if let Some(wave_m) = current.wave_height {
            let wave_ft = m_to_ft(wave_m);
            let period = current
                .wave_period
                .map(|p| format!(" · {:.0}s period", p))
                .unwrap_or_default();
            let dir = current
                .wave_direction
                .map(|d| format!(" · {}° ({})", d as i32, degrees_to_compass(d)))
                .unwrap_or_default();
            out.push_str(&format!(
                "- **Combined wave**: {:.1} ft ({:.1} m){}{}\n",
                wave_ft, wave_m, period, dir
            ));
        }
        if let Some(swell_m) = current.swell_wave_height {
            let swell_ft = m_to_ft(swell_m);
            let period = current
                .swell_wave_period
                .map(|p| format!(" · {:.0}s period", p))
                .unwrap_or_default();
            let dir = current
                .swell_wave_direction
                .map(|d| format!(" · {}° ({})", d as i32, degrees_to_compass(d)))
                .unwrap_or_default();
            out.push_str(&format!(
                "- **Primary swell**: {:.1} ft ({:.1} m){}{}\n",
                swell_ft, swell_m, period, dir
            ));
        }
        if let Some(wind_wave_m) = current.wind_wave_height
            && wind_wave_m > 0.0
        {
            out.push_str(&format!(
                "- **Wind wave**: {:.1} ft ({:.1} m)\n",
                m_to_ft(wind_wave_m),
                wind_wave_m
            ));
        }
    } else {
        out.push_str("  ⚠ No current marine data\n");
    }

    if let Some(wind) = &c.wind.current {
        let speed = wind
            .wind_speed_10m
            .map(|s| format!("{:.0} mph", s))
            .unwrap_or_else(|| "—".into());
        let gust = wind
            .wind_gusts_10m
            .map(|g| format!(" (gusts {:.0})", g))
            .unwrap_or_default();
        let dir = wind
            .wind_direction_10m
            .map(|d| format!(" · {}° ({})", d as i32, degrees_to_compass(d)))
            .unwrap_or_default();
        let temp = wind
            .temperature_2m
            .map(|t| format!(" · air {:.0}°F", t))
            .unwrap_or_default();
        out.push_str(&format!("- **Wind**: {}{}{}{}\n", speed, gust, dir, temp));
    }
}

/// Append the hourly wave/swell table, starting at the current Pacific hour
/// (or the top of the data when it's entirely outside the current hour).
fn write_hourly_table(out: &mut String, hourly: &MarineHourly, hours: usize) {
    let now_hour = crate::util::now_pacific()
        .format("%Y-%m-%dT%H:00")
        .to_string();
    let start = hourly
        .time
        .iter()
        .position(|t| t.as_str() >= now_hour.as_str())
        .unwrap_or(0);
    out.push_str(&format!("## Next {} hours\n", hours));
    out.push_str("| Time | Wave ft | Period | Dir | Swell ft | Swell period |\n");
    out.push_str("|---|---|---|---|---|---|\n");
    for i in start..(start + hours).min(hourly.time.len()) {
        let time = hourly.time.get(i).map(|s| s.as_str()).unwrap_or("\u{2014}");
        let wave_ft = hourly
            .wave_height
            .get(i)
            .copied()
            .flatten()
            .map(m_to_ft)
            .map(|v| format!("{:.1}", v))
            .unwrap_or_else(|| "\u{2014}".into());
        let period = hourly
            .wave_period
            .get(i)
            .copied()
            .flatten()
            .map(|v| format!("{:.0}s", v))
            .unwrap_or_else(|| "\u{2014}".into());
        let dir = hourly
            .wave_direction
            .get(i)
            .copied()
            .flatten()
            .map(|d| degrees_to_compass(d).to_string())
            .unwrap_or_else(|| "\u{2014}".into());
        let swell_ft = hourly
            .swell_wave_height
            .get(i)
            .copied()
            .flatten()
            .map(m_to_ft)
            .map(|v| format!("{:.1}", v))
            .unwrap_or_else(|| "\u{2014}".into());
        let swell_period = hourly
            .swell_wave_period
            .get(i)
            .copied()
            .flatten()
            .map(|v| format!("{:.0}s", v))
            .unwrap_or_else(|| "\u{2014}".into());
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            time, wave_ft, period, dir, swell_ft, swell_period
        ));
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_meteo::{ForecastCurrent, MarineCurrent, MarineHourly};

    #[test]
    fn meters_to_feet() {
        assert!((m_to_ft(1.0) - 3.28084).abs() < 0.001);
        assert!((m_to_ft(2.0) - 6.56168).abs() < 0.001);
    }

    fn hourly_3() -> MarineHourly {
        MarineHourly {
            time: vec![
                "2026-04-10T00:00".to_string(),
                "2026-04-10T01:00".to_string(),
                "2026-04-10T02:00".to_string(),
            ],
            wave_height: vec![Some(1.0)],
            wave_direction: vec![],
            wave_period: vec![Some(10.0), None],
            swell_wave_height: vec![],
            swell_wave_direction: vec![],
            swell_wave_period: vec![],
            wind_wave_height: vec![],
        }
    }

    fn conditions_with_hourly() -> SpotConditions {
        SpotConditions {
            marine: MarineResponse {
                latitude: 36.9519,
                longitude: -122.0264,
                current_units: None,
                current: None,
                hourly_units: None,
                hourly: Some(hourly_3()),
            },
            wind: ForecastResponse {
                current: Some(ForecastCurrent {
                    time: "2026-04-10T17:00".to_string(),
                    temperature_2m: Some(61.0),
                    wind_speed_10m: Some(8.0),
                    wind_direction_10m: Some(290.0),
                    wind_gusts_10m: None,
                }),
            },
        }
    }

    // The forecast table is opt-in: format_single_spot appends it only when
    // forecast_hours is set (the merged surf tool's contract).
    #[test]
    fn single_spot_appends_hourly_table_only_when_requested() {
        let spot = spots::find("steamer").unwrap();
        let c = conditions_with_hourly();
        let with = format_single_spot(spot, &c, Some(6));
        assert!(with.contains("## Next 6 hours"), "got: {with}");
        assert!(with.contains("| 2026-04-10T00:00 | 3.3 | 10s |"));
        let without = format_single_spot(spot, &c, None);
        assert!(!without.contains("Next 6 hours"));
    }

    #[test]
    fn custom_spot_appends_hourly_table_only_when_requested() {
        let c = conditions_with_hourly();
        let with = format_custom_spot("Twin Lakes", 36.96, -122.01, &c, Some(12));
        assert!(with.contains("## Next 12 hours"));
        let without = format_custom_spot("Twin Lakes", 36.96, -122.01, &c, None);
        assert!(!without.contains("Next 12 hours"));
    }

    // Schema drift: hourly value arrays shorter than `time` must render
    // placeholders, never index out of bounds.
    #[test]
    fn hourly_table_short_parallel_arrays() {
        let mut out = String::new();
        write_hourly_table(&mut out, &hourly_3(), 12);
        assert!(out.contains("## Next 12 hours"));
        assert!(out.contains("| 2026-04-10T00:00 | 3.3 | 10s |"));
        // Missing entries degrade to em-dash cells.
        assert!(out.contains("| 2026-04-10T02:00 | \u{2014} | \u{2014} | \u{2014} | \u{2014} | \u{2014} |"));
    }

    #[test]
    fn spot_body_without_current_marine_data() {
        let c = SpotConditions {
            marine: MarineResponse {
                latitude: 36.9519,
                longitude: -122.0264,
                current_units: None,
                current: None,
                hourly_units: None,
                hourly: None,
            },
            wind: ForecastResponse {
                current: Some(ForecastCurrent {
                    time: "2026-04-10T17:00".to_string(),
                    temperature_2m: Some(61.0),
                    wind_speed_10m: Some(8.0),
                    wind_direction_10m: Some(290.0),
                    wind_gusts_10m: None,
                }),
            },
        };
        let mut out = String::new();
        write_spot_body(&mut out, &c);
        assert!(out.contains("No current marine data"));
        assert!(out.contains("**Wind**: 8 mph"));
    }

    #[test]
    fn spot_body_current_with_all_null_values() {
        let c = SpotConditions {
            marine: MarineResponse {
                latitude: 36.9519,
                longitude: -122.0264,
                current_units: None,
                current: Some(MarineCurrent {
                    time: "2026-04-10T17:00".to_string(),
                    wave_height: None,
                    wave_direction: None,
                    wave_period: None,
                    swell_wave_height: None,
                    swell_wave_direction: None,
                    swell_wave_period: None,
                    wind_wave_height: None,
                }),
                hourly_units: None,
                hourly: None,
            },
            wind: ForecastResponse { current: None },
        };
        let mut out = String::new();
        write_spot_body(&mut out, &c);
        // Nothing to report, but no panic and no bogus numbers.
        assert!(!out.contains("NaN"));
        assert!(!out.contains("ft"));
    }
}
