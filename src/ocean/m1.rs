use anyhow::{Result, bail};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::util::{degrees_to_compass, now_pacific};

use super::erddap_client::{ErddapClient, ErddapTable, TabledapQuery};
use super::types::{M1Snapshot as M1SnapTyped, ProfileLevel};

const SERVER: &str = "https://erddap.sensors.axds.co/erddap";
const DATASET: &str = "org_mbari_m1";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct M1Request {
    /// Hours of surface data to fetch (1–168). Default: 24.
    pub hours: Option<u32>,
    /// Include full water-column temperature profile at the latest timestep.
    /// Default: true.
    pub include_profile: Option<bool>,
}

pub async fn fetch_and_format(
    erddap: &ErddapClient,
    req: &M1Request,
) -> Result<String> {
    let hours = req.hours.unwrap_or(24).clamp(1, 168);
    let include_profile = req.include_profile.unwrap_or(true);

    let time_min = format_time_min(hours);

    let mut surface = fetch_surface(erddap, &time_min).await?;
    trim_to_latest_hours(&mut surface, hours);
    let wind = fetch_wind(erddap, &time_min).await;
    let profile = if include_profile {
        fetch_profile(erddap, &time_min).await.ok()
    } else {
        None
    };

    format_snapshot(&surface, wind.as_ref().ok(), profile.as_ref(), hours)
}

pub async fn fetch_typed(
    erddap: &ErddapClient,
    req: &M1Request,
) -> Result<M1SnapTyped> {
    // Cache the typed snapshot under the same TTL as the single-tool string
    // path (1800s) so fusion callers hit cache instead of refetching ERDDAP.
    let hours = req.hours.unwrap_or(24);
    let profile = req.include_profile.unwrap_or(true);
    let cache_key = format!("ocean:m1:typed:h{}:p{}", hours, profile);
    erddap
        .cache()
        .get_or_fetch(&cache_key, 1800, || fetch_typed_uncached(erddap, req))
        .await
}

async fn fetch_typed_uncached(
    erddap: &ErddapClient,
    req: &M1Request,
) -> Result<M1SnapTyped> {
    let hours = req.hours.unwrap_or(24).clamp(1, 168);
    let include_profile = req.include_profile.unwrap_or(true);

    let time_min = format_time_min(hours);

    let mut surface = fetch_surface(erddap, &time_min).await?;
    trim_to_latest_hours(&mut surface, hours);
    let wind = fetch_wind(erddap, &time_min).await;
    let profile = if include_profile {
        fetch_profile(erddap, &time_min).await.ok()
    } else {
        None
    };

    let latest = surface.rows.last().unwrap();
    let latency_hours = compute_latency(&latest.time);

    let (wind_speed, wind_dir, equatorward) = if let Ok(ref w) = wind {
        let w_latest = w.rows.last().unwrap();
        (
            Some(w_latest.speed_ms),
            Some(w_latest.dir_from_deg),
            Some(equatorward_component(w_latest.speed_ms, w_latest.dir_from_deg)),
        )
    } else {
        (None, None, None)
    };

    let (profile_levels, stratification_index) = if let Some(ref p) = profile {
        let levels: Vec<ProfileLevel> = p.levels.iter().map(|&(z, t)| ProfileLevel { z_m: z, temp_c: t }).collect();

        let surface_t = p.levels.iter().rev().find(|(z, _)| *z >= -5.0).map(|(_, t)| *t);
        let deep_50_t = p.levels.iter().find(|(z, _)| *z <= -45.0 && *z >= -55.0).map(|(_, t)| *t);
        let strat = match (surface_t, deep_50_t) {
            (Some(st), Some(dt)) => Some(st - dt),
            _ => None,
        };

        (levels, strat)
    } else {
        (Vec::new(), None)
    };

    Ok(M1SnapTyped {
        timestamp_utc: latest.time.clone(),
        latency_hours,
        surface_temp_c: Some(latest.temp_c),
        wind_speed_ms: wind_speed,
        wind_dir_from_deg: wind_dir,
        equatorward_wind_ms: equatorward,
        profile: profile_levels,
        stratification_index,
    })
}

struct SurfaceData {
    rows: Vec<SurfaceRow>,
}

struct SurfaceRow {
    time: String,
    z: f64,
    temp_c: f64,
}

struct WindData {
    rows: Vec<WindRow>,
}

struct WindRow {
    #[allow(dead_code)]
    time: String,
    speed_ms: f64,
    dir_from_deg: f64,
}

struct ProfileData {
    time: String,
    levels: Vec<(f64, f64)>,
}

fn trim_to_latest_hours(surface: &mut SurfaceData, hours: u32) {
    if let Some(latest) = surface.rows.last()
        && let Ok(latest_dt) = chrono::DateTime::parse_from_rfc3339(&latest.time) {
            let cutoff = latest_dt - chrono::Duration::hours(hours as i64);
            let cutoff_str = cutoff.format("%Y-%m-%dT%H:%M:%SZ").to_string();
            surface.rows.retain(|r| r.time >= cutoff_str);
        }
}

fn format_time_min(hours: u32) -> String {
    let now = chrono::Utc::now();
    // M1 mooring latency varies from ~24h to ~5 days. Always look back at
    // least 7 days to ensure we find data, then trim to the most recent
    // `hours` on the display side.
    let lookback = (hours as i64 + 168).max(168);
    let from = now - chrono::Duration::hours(lookback);
    from.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

async fn fetch_surface(erddap: &ErddapClient, time_min: &str) -> Result<SurfaceData> {
    let query = TabledapQuery::new(vec![
        "time".into(),
        "z".into(),
        "sea_water_temperature".into(),
    ])
    .constraint(format!("time>={}", time_min))
    .constraint("z>=-3")
    .with_qc(vec!["sea_water_temperature".into()]);

    let resp = erddap.tabledap(SERVER, DATASET, query).await?;
    let t = &resp.table;
    let (i_time, i_z, i_temp) = resolve_cols(t, "time", "z", "sea_water_temperature")?;

    let rows: Vec<SurfaceRow> = t
        .rows
        .iter()
        .filter_map(|r| {
            Some(SurfaceRow {
                time: r.get(i_time)?.as_str()?.to_string(),
                z: r.get(i_z)?.as_f64()?,
                temp_c: r.get(i_temp)?.as_f64()?,
            })
        })
        .collect();

    if rows.is_empty() {
        bail!("M1 mooring returned no surface temperature data");
    }

    Ok(SurfaceData { rows })
}

async fn fetch_wind(erddap: &ErddapClient, time_min: &str) -> Result<WindData> {
    let query = TabledapQuery::new(vec![
        "time".into(),
        "wind_speed_sonic".into(),
        "wind_from_direction_sonic".into(),
    ])
    .constraint(format!("time>={}", time_min))
    .constraint("z>=0")
    .with_qc(vec![
        "wind_speed_sonic".into(),
        "wind_from_direction_sonic".into(),
    ])
    .order_by_mean("time/1hour");

    let resp = erddap.tabledap(SERVER, DATASET, query).await?;
    let t = &resp.table;
    let i_time = t.col_index("time").ok_or_else(|| anyhow::anyhow!("missing time col"))?;
    let i_spd = t
        .col_index("wind_speed_sonic")
        .ok_or_else(|| anyhow::anyhow!("missing wind_speed_sonic col"))?;
    let i_dir = t
        .col_index("wind_from_direction_sonic")
        .ok_or_else(|| anyhow::anyhow!("missing wind_from_direction_sonic col"))?;

    let rows: Vec<WindRow> = t
        .rows
        .iter()
        .filter_map(|r| {
            Some(WindRow {
                time: r.get(i_time)?.as_str()?.to_string(),
                speed_ms: r.get(i_spd)?.as_f64()?,
                dir_from_deg: r.get(i_dir)?.as_f64()?,
            })
        })
        .collect();

    if rows.is_empty() {
        bail!("M1 mooring returned no wind data");
    }
    Ok(WindData { rows })
}

async fn fetch_profile(erddap: &ErddapClient, time_min: &str) -> Result<ProfileData> {
    let query = TabledapQuery::new(vec![
        "time".into(),
        "z".into(),
        "sea_water_temperature".into(),
    ])
    .constraint(format!("time>={}", time_min))
    .with_qc(vec!["sea_water_temperature".into()])
    .order_by("z");

    let resp = erddap.tabledap(SERVER, DATASET, query).await?;
    let t = &resp.table;
    let (i_time, i_z, i_temp) = resolve_cols(t, "time", "z", "sea_water_temperature")?;

    if t.rows.is_empty() {
        bail!("no profile data");
    }

    let last_time = t
        .rows
        .last()
        .and_then(|r| r.get(i_time)?.as_str())
        .unwrap_or("")
        .to_string();

    let mut levels: Vec<(f64, f64)> = t
        .rows
        .iter()
        .filter(|r| {
            r.get(i_time)
                .and_then(|v| v.as_str())
                .is_some_and(|s| s == last_time)
        })
        .filter_map(|r| {
            let z = r.get(i_z)?.as_f64()?;
            let temp = r.get(i_temp)?.as_f64()?;
            Some((z, temp))
        })
        .collect();

    levels.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    levels.dedup_by(|a, b| (a.0 - b.0).abs() < 0.1);

    if levels.is_empty() {
        bail!("no profile levels at latest timestep");
    }

    Ok(ProfileData {
        time: last_time,
        levels,
    })
}

fn resolve_cols(
    t: &ErddapTable,
    c1: &str,
    c2: &str,
    c3: &str,
) -> Result<(usize, usize, usize)> {
    Ok((
        t.col_index(c1)
            .ok_or_else(|| anyhow::anyhow!("missing column '{}'", c1))?,
        t.col_index(c2)
            .ok_or_else(|| anyhow::anyhow!("missing column '{}'", c2))?,
        t.col_index(c3)
            .ok_or_else(|| anyhow::anyhow!("missing column '{}'", c3))?,
    ))
}

fn format_snapshot(
    surface: &SurfaceData,
    wind: Option<&WindData>,
    profile: Option<&ProfileData>,
    hours: u32,
) -> Result<String> {
    let latest = surface.rows.last().unwrap();
    let oldest = surface.rows.first().unwrap();

    let latency = compute_latency(&latest.time);

    let mut out = format!(
        "# M1 Mooring — Monterey Bay (MBARI)\n\n\
         _36.75°N, 122.03°W — mouth of Monterey Bay, ~480 m depth_\n\n\
         **Latest observation**: {} (latency {:.0}h)\n\n",
        latest.time, latency,
    );

    out.push_str(&format!(
        "## Surface (z = {} m)\n\n\
         - **Temperature**: {:.1}°F ({:.2}°C)\n",
        latest.z,
        latest.temp_c * 9.0 / 5.0 + 32.0,
        latest.temp_c,
    ));

    if surface.rows.len() > 1 {
        let delta = latest.temp_c - oldest.temp_c;
        out.push_str(&format!(
            "- **{}-hour trend**: {:+.2}°C ({} → {})\n",
            hours.min(surface.rows.len() as u32),
            delta,
            &oldest.time[11..16],
            &latest.time[11..16],
        ));
    }

    if let Some(wind) = wind {
        out.push('\n');
        let w_latest = wind.rows.last().unwrap();

        let mph = w_latest.speed_ms * 2.23694;
        let dir_normalized = ((w_latest.dir_from_deg % 360.0) + 360.0) % 360.0;
        let compass = degrees_to_compass(dir_normalized);

        let equatorward = equatorward_component(w_latest.speed_ms, w_latest.dir_from_deg);

        out.push_str(&format!(
            "## Wind (sonic anemometer, hourly mean)\n\n\
             - **Speed**: {:.1} mph ({:.1} m/s) from {} ({:.0}°)\n\
             - **Equatorward component**: {:.1} m/s{}\n",
            mph,
            w_latest.speed_ms,
            compass,
            dir_normalized,
            equatorward,
            if equatorward > 5.0 {
                " ← strong upwelling-favorable"
            } else if equatorward > 2.0 {
                " ← moderate upwelling-favorable"
            } else if equatorward > 0.0 {
                " ← weak upwelling-favorable"
            } else {
                ""
            },
        ));

        if wind.rows.len() > 3 {
            let avg_speed: f64 =
                wind.rows.iter().map(|r| r.speed_ms).sum::<f64>() / wind.rows.len() as f64;
            let avg_equatorward: f64 = wind
                .rows
                .iter()
                .map(|r| equatorward_component(r.speed_ms, r.dir_from_deg))
                .sum::<f64>()
                / wind.rows.len() as f64;
            out.push_str(&format!(
                "- **Period average** ({} hours): {:.1} m/s, equatorward {:.1} m/s\n",
                wind.rows.len(),
                avg_speed,
                avg_equatorward,
            ));
        }
    }

    if let Some(profile) = profile {
        out.push_str(&format!(
            "\n## Water column profile ({})\n\n\
             | Depth (m) | Temp (°C) | Temp (°F) |\n\
             |---|---|---|\n",
            profile.time,
        ));

        for &(z, t) in &profile.levels {
            out.push_str(&format!(
                "| {:.0} | {:.2} | {:.1} |\n",
                z,
                t,
                t * 9.0 / 5.0 + 32.0,
            ));
        }

        let surface_t = profile
            .levels
            .iter()
            .rev()
            .find(|(z, _)| *z >= -5.0)
            .map(|(_, t)| *t);
        let deep_50_t = profile
            .levels
            .iter()
            .find(|(z, _)| *z <= -45.0 && *z >= -55.0)
            .map(|(_, t)| *t);

        if let (Some(st), Some(dt)) = (surface_t, deep_50_t) {
            let strat = st - dt;
            out.push_str(&format!(
                "\n**Stratification index** (surface − 50 m): {:.2}°C{}\n",
                strat,
                if strat > 3.0 {
                    " — strongly stratified (upwelled water not yet surfacing)"
                } else if strat > 1.5 {
                    " — moderately stratified"
                } else if strat > 0.5 {
                    " — weakly stratified (active mixing)"
                } else {
                    " — well-mixed (recent strong upwelling or winter conditions)"
                },
            ));
        }
    }

    out.push_str(&format!(
        "\n_Source: MBARI M1 Mooring (`org_mbari_m1`), CeNCOOS/Axiom ERDDAP. \
         QC: only QARTOD-pass (qc_agg=1) values shown. \
         Last updated: {}._\n",
        now_pacific().format("%-I:%M %p %Z"),
    ));

    Ok(out)
}

/// Decompose wind into the equatorward (upwelling-favorable) component.
/// Central CA coast orientation: equatorward = from ~325° (NW). Wind "from"
/// the NW drives offshore Ekman transport → upwelling.
fn equatorward_component(speed_ms: f64, from_deg: f64) -> f64 {
    let coast_angle_rad = 325.0_f64.to_radians();
    let wind_from_rad = from_deg.to_radians();
    speed_ms * (wind_from_rad - coast_angle_rad).cos()
}

fn compute_latency(timestamp: &str) -> f64 {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|t| {
            let diff = chrono::Utc::now() - t.to_utc();
            diff.num_minutes() as f64 / 60.0
        })
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equatorward_nw_wind_is_positive() {
        // NW wind (from 325°) is perfectly equatorward for central CA coast
        let eq = equatorward_component(5.0, 325.0);
        assert!((eq - 5.0).abs() < 0.01, "got {}", eq);
    }

    #[test]
    fn equatorward_se_wind_is_negative() {
        // SE wind (from 145° = 325°-180°) is poleward → negative
        let eq = equatorward_component(5.0, 145.0);
        assert!((eq - (-5.0)).abs() < 0.01, "got {}", eq);
    }

    #[test]
    fn equatorward_handles_negative_degrees() {
        // -94° is equivalent to 266° (W). cos((266-325)°) = cos(-59°) ≈ 0.515
        let eq1 = equatorward_component(4.3, -94.0);
        let eq2 = equatorward_component(4.3, 266.0);
        assert!((eq1 - eq2).abs() < 0.01, "got {} vs {}", eq1, eq2);
    }

    #[test]
    fn format_snapshot_renders_surface() {
        let surface = SurfaceData {
            rows: vec![
                SurfaceRow {
                    time: "2026-04-28T00:00:00Z".into(),
                    z: -1.0,
                    temp_c: 14.0,
                },
                SurfaceRow {
                    time: "2026-04-28T14:00:00Z".into(),
                    z: -1.0,
                    temp_c: 13.5,
                },
            ],
        };
        let out = format_snapshot(&surface, None, None, 24).unwrap();
        assert!(out.contains("M1 Mooring"), "missing header");
        assert!(out.contains("13.50°C"), "missing temperature");
        assert!(out.contains("-0.50°C"), "missing trend");
    }

    #[test]
    fn format_snapshot_renders_profile_with_stratification() {
        let surface = SurfaceData {
            rows: vec![SurfaceRow {
                time: "2026-04-28T14:00:00Z".into(),
                z: -1.0,
                temp_c: 13.5,
            }],
        };
        let profile = ProfileData {
            time: "2026-04-28T14:00:00Z".into(),
            levels: vec![(-300.0, 8.0), (-50.0, 10.5), (-1.0, 13.5)],
        };
        let out = format_snapshot(&surface, None, Some(&profile), 24).unwrap();
        assert!(out.contains("Stratification index"), "missing strat index");
        assert!(out.contains("3.00°C"), "wrong strat value");
        assert!(out.contains("stratified"), "missing strat label");
    }

    #[test]
    fn trim_to_latest_keeps_correct_range() {
        let mut data = SurfaceData {
            rows: vec![
                SurfaceRow {
                    time: "2026-04-27T00:00:00Z".into(),
                    z: -1.0,
                    temp_c: 14.0,
                },
                SurfaceRow {
                    time: "2026-04-27T12:00:00Z".into(),
                    z: -1.0,
                    temp_c: 13.8,
                },
                SurfaceRow {
                    time: "2026-04-28T00:00:00Z".into(),
                    z: -1.0,
                    temp_c: 13.6,
                },
                SurfaceRow {
                    time: "2026-04-28T12:00:00Z".into(),
                    z: -1.0,
                    temp_c: 13.5,
                },
            ],
        };
        trim_to_latest_hours(&mut data, 24);
        // Should keep only rows within 24h of the latest (2026-04-28T12:00)
        assert_eq!(data.rows.len(), 3); // 27T12:00, 28T00:00, 28T12:00
        assert_eq!(data.rows[0].time, "2026-04-27T12:00:00Z");
    }
}
