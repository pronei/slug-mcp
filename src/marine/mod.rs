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
use open_meteo::{ForecastResponse, MarineResponse};
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
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MarineForecastRequest {
    /// Surf spot name or slug. Takes precedence over lat/lon if provided.
    pub spot: Option<String>,
    /// Custom latitude (use with lon). Ignored if `spot` is set.
    pub lat: Option<f64>,
    /// Custom longitude (use with lat). Ignored if `spot` is set.
    pub lon: Option<f64>,
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

    /// Compare all known SC surf spots, return conditions for a single named spot,
    /// or fetch conditions for custom lat/lon coordinates.
    pub async fn get_surf_conditions(
        &self,
        spot_query: Option<&str>,
        lat: Option<f64>,
        lon: Option<f64>,
        label: Option<&str>,
    ) -> Result<String> {
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
            return Ok(format_single_spot(spot, &conditions));
        }

        // Custom coordinates
        if let (Some(lat), Some(lon)) = (lat, lon) {
            let name = label.unwrap_or("Custom spot");
            let slug = format!("custom-{:.4}-{:.4}", lat, lon);
            let conditions = self.load_by_coords(&slug, lat, lon).await?;
            return Ok(format_custom_spot(name, lat, lon, &conditions));
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

    /// Full marine forecast for a named spot or custom coordinates.
    pub async fn get_marine_forecast(
        &self,
        spot_query: Option<&str>,
        lat: Option<f64>,
        lon: Option<f64>,
    ) -> Result<String> {
        let (label, lat, lon, notes) = if let Some(q) = spot_query {
            let spot = spots::find(q).ok_or_else(|| {
                anyhow::anyhow!(
                    "Surf spot '{}' not found. Known spots: {}",
                    q,
                    spots::names_list()
                )
            })?;
            (spot.name.to_string(), spot.lat, spot.lon, Some(spot.notes))
        } else if let (Some(lat), Some(lon)) = (lat, lon) {
            (format!("{:.4}, {:.4}", lat, lon), lat, lon, None)
        } else {
            anyhow::bail!(
                "Provide either `spot` (e.g. 'Steamer Lane') or both `lat` and `lon`."
            );
        };

        let marine = self.load_marine_by_coords(lat, lon).await?;
        Ok(format_marine_detail(&label, notes, &marine))
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

    async fn load_marine_by_coords(&self, lat: f64, lon: f64) -> Result<MarineResponse> {
        let key = format!("marine:coords:{:.4},{:.4}", lat, lon);
        let http = self.http.clone();
        self.cache
            .get_or_fetch::<MarineResponse, _, _>(&key, 1800, move || async move {
                open_meteo::get_marine(&http, lat, lon).await
            })
            .await
    }
}

// ───── formatting ─────

fn m_to_ft(m: f64) -> f64 {
    m * 3.28084
}

fn format_single_spot(spot: &SurfSpot, c: &SpotConditions) -> String {
    let mut out = format!("# {} ({})\n\n", spot.name, spot.slug);
    out.push_str(&format!("_{}_\n\n", spot.notes));
    write_spot_body(&mut out, c);
    let now = crate::util::now_pacific();
    out.push_str(&format!(
        "\n_Source: Open-Meteo marine + forecast. Last updated: {}_\n",
        now.format("%-I:%M %p")
    ));
    out
}

fn format_custom_spot(name: &str, lat: f64, lon: f64, c: &SpotConditions) -> String {
    let mut out = format!("# {} ({:.4}, {:.4})\n\n", name, lat, lon);
    write_spot_body(&mut out, c);
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
        if let Some(wind_wave_m) = current.wind_wave_height {
            if wind_wave_m > 0.0 {
                out.push_str(&format!(
                    "- **Wind wave**: {:.1} ft ({:.1} m)\n",
                    m_to_ft(wind_wave_m),
                    wind_wave_m
                ));
            }
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

fn format_marine_detail(label: &str, notes: Option<&str>, m: &MarineResponse) -> String {
    let mut out = format!("# Marine Forecast — {}\n\n", label);
    if let Some(notes) = notes {
        out.push_str(&format!("_{}_\n\n", notes));
    }
    out.push_str(&format!(
        "Grid point: {:.4}, {:.4} (Open-Meteo snaps to the nearest ocean cell)\n\n",
        m.latitude, m.longitude
    ));

    if let Some(current) = &m.current {
        out.push_str("## Now\n");
        if let Some(wh) = current.wave_height {
            out.push_str(&format!(
                "- Wave height: {:.1} ft ({:.1} m)\n",
                m_to_ft(wh),
                wh
            ));
        }
        if let Some(wp) = current.wave_period {
            out.push_str(&format!("- Wave period: {:.1} s\n", wp));
        }
        if let Some(wd) = current.wave_direction {
            out.push_str(&format!(
                "- Wave direction: {:.0}° ({})\n",
                wd,
                degrees_to_compass(wd)
            ));
        }
        if let Some(sh) = current.swell_wave_height {
            out.push_str(&format!(
                "- Swell height: {:.1} ft ({:.1} m)\n",
                m_to_ft(sh),
                sh
            ));
        }
        if let Some(sp) = current.swell_wave_period {
            out.push_str(&format!("- Swell period: {:.1} s\n", sp));
        }
        if let Some(sd) = current.swell_wave_direction {
            out.push_str(&format!(
                "- Swell direction: {:.0}° ({})\n",
                sd,
                degrees_to_compass(sd)
            ));
        }
        out.push('\n');
    }

    if let Some(hourly) = &m.hourly {
        let now_hour = crate::util::now_pacific().format("%Y-%m-%dT%H:00").to_string();
        let start = hourly
            .time
            .iter()
            .position(|t| t.as_str() >= now_hour.as_str())
            .unwrap_or(0);
        out.push_str("## Next 12 hours\n");
        out.push_str("| Time | Wave ft | Period | Dir | Swell ft | Swell period |\n");
        out.push_str("|---|---|---|---|---|---|\n");
        for i in start..(start + 12).min(hourly.time.len()) {
            let time = hourly.time.get(i).map(|s| s.as_str()).unwrap_or("—");
            let wave_ft = hourly
                .wave_height
                .get(i)
                .copied()
                .flatten()
                .map(m_to_ft)
                .map(|v| format!("{:.1}", v))
                .unwrap_or_else(|| "—".into());
            let period = hourly
                .wave_period
                .get(i)
                .copied()
                .flatten()
                .map(|v| format!("{:.0}s", v))
                .unwrap_or_else(|| "—".into());
            let dir = hourly
                .wave_direction
                .get(i)
                .copied()
                .flatten()
                .map(|d| degrees_to_compass(d).to_string())
                .unwrap_or_else(|| "—".into());
            let swell_ft = hourly
                .swell_wave_height
                .get(i)
                .copied()
                .flatten()
                .map(m_to_ft)
                .map(|v| format!("{:.1}", v))
                .unwrap_or_else(|| "—".into());
            let swell_period = hourly
                .swell_wave_period
                .get(i)
                .copied()
                .flatten()
                .map(|v| format!("{:.0}s", v))
                .unwrap_or_else(|| "—".into());
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                time, wave_ft, period, dir, swell_ft, swell_period
            ));
        }
        out.push('\n');
    }

    let now = crate::util::now_pacific();
    out.push_str(&format!(
        "_Source: Open-Meteo marine. Last updated: {}_\n",
        now.format("%-I:%M %p")
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meters_to_feet() {
        assert!((m_to_ft(1.0) - 3.28084).abs() < 0.001);
        assert!((m_to_ft(2.0) - 6.56168).abs() < 0.001);
    }
}
