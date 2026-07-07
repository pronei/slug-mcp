use anyhow::{Result, bail};
use futures_util::future::join_all;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::util::now_pacific;

use super::erddap_client::{ErddapClient, grid_selector, lon_to_360};
use super::types::{HabDayForecast as HabDayTyped, HabSnapshot};

const SERVER: &str = "https://coastwatch.pfeg.noaa.gov/erddap";
const DATASETS: &[&str] = &[
    "wvcharmV3_0day",
    "wvcharmV3_1day",
    "wvcharmV3_2day",
    "wvcharmV3_3day",
];
const RISK_THRESHOLD: f64 = 0.6;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HabRequest {
    /// Latitude (default: 36.96 = Santa Cruz Wharf).
    pub lat: Option<f64>,
    /// Longitude in Western convention, e.g. -122.02. Auto-converted to 0-360
    /// for C-HARM. Default: -122.02.
    pub lon: Option<f64>,
    /// Search radius in grid cells for nearest non-NaN value. Coastal cells
    /// are often masked. Default: 5 (~15 km at 0.03° resolution).
    pub snap_radius: Option<usize>,
}

struct DayForecast {
    dataset: String,
    date: String,
    p_pseudo_nitzschia: Option<f64>,
    p_particulate_domoic: Option<f64>,
    p_cellular_domoic: Option<f64>,
    chla_filled: Option<f64>,
}

pub async fn fetch_typed(erddap: &ErddapClient, req: &HabRequest) -> Result<HabSnapshot> {
    // Cache the typed snapshot under the same TTL as the single-tool string
    // path (3600s) so fusion callers hit cache instead of refetching ERDDAP.
    let cache_key = format!(
        "ocean:charm:typed:{:.2}:{:.2}:r{}",
        req.lat.unwrap_or(36.96),
        req.lon.unwrap_or(-122.02),
        req.snap_radius.unwrap_or(5),
    );
    erddap
        .cache()
        .get_or_fetch(&cache_key, 3600, || fetch_typed_uncached(erddap, req))
        .await
}

async fn fetch_typed_uncached(erddap: &ErddapClient, req: &HabRequest) -> Result<HabSnapshot> {
    let lat = req.lat.unwrap_or(36.96);
    let lon_west = req.lon.unwrap_or(-122.02);
    let lon_360 = lon_to_360(lon_west);
    let snap_r = req.snap_radius.unwrap_or(5);

    let lat_lo = lat - 0.1 * snap_r as f64;
    let lat_hi = lat + 0.1 * snap_r as f64;
    let lon_lo = lon_360 - 0.1 * snap_r as f64;
    let lon_hi = lon_360 + 0.1 * snap_r as f64;

    // Fetch all 4 forecast horizons concurrently. Partial failures are
    // tolerated: a failed dataset becomes an "unavailable" placeholder so the
    // remaining horizons still render.
    let results =
        join_all(DATASETS.iter().map(|&ds| {
            fetch_one_day(erddap, ds, (lat_lo, lat_hi), (lon_lo, lon_hi), lat, lon_360)
        }))
        .await;

    let forecasts: Vec<DayForecast> = DATASETS
        .iter()
        .zip(results)
        .map(|(&ds, res)| match res {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("C-HARM {} fetch failed: {}", ds, e);
                DayForecast {
                    dataset: ds.to_string(),
                    date: "unavailable".into(),
                    p_pseudo_nitzschia: None,
                    p_particulate_domoic: None,
                    p_cellular_domoic: None,
                    chla_filled: None,
                }
            }
        })
        .collect();

    if forecasts.iter().all(|f| f.p_pseudo_nitzschia.is_none()) {
        bail!(
            "C-HARM: all 4 forecast horizons returned no data near ({}, {})",
            lat,
            lon_west
        );
    }

    let typed_forecasts: Vec<HabDayTyped> = forecasts
        .iter()
        .map(|f| HabDayTyped {
            dataset: f.dataset.clone(),
            date: f.date.clone(),
            p_pseudo_nitzschia: f.p_pseudo_nitzschia,
            p_particulate_domoic: f.p_particulate_domoic,
            p_cellular_domoic: f.p_cellular_domoic,
            chla_filled: f.chla_filled,
            risk_class: risk_class(f.p_pseudo_nitzschia).to_string(),
        })
        .collect();

    Ok(HabSnapshot {
        query_lat: lat,
        query_lon: lon_west,
        forecasts: typed_forecasts,
    })
}

pub async fn fetch_and_format(erddap: &ErddapClient, req: &HabRequest) -> Result<String> {
    let snap = fetch_typed(erddap, req).await?;
    format_output(&snap)
}

async fn fetch_one_day(
    erddap: &ErddapClient,
    dataset: &str,
    lat_range: (f64, f64),
    lon_range: (f64, f64),
    target_lat: f64,
    target_lon: f64,
) -> Result<DayForecast> {
    let sel_pn = grid_selector("pseudo_nitzschia", "last", lat_range, lon_range);
    let sel_pd = grid_selector("particulate_domoic", "last", lat_range, lon_range);
    let sel_cd = grid_selector("cellular_domoic", "last", lat_range, lon_range);
    let sel_chl = grid_selector("chla_filled", "last", lat_range, lon_range);

    let resp = erddap
        .griddap(SERVER, dataset, &[sel_pn, sel_pd, sel_cd, sel_chl])
        .await?;

    let t = &resp.table;
    let i_time = t.col_index("time").unwrap_or(0);
    let i_lat = t.col_index("latitude").unwrap_or(1);
    let i_lon = t.col_index("longitude").unwrap_or(2);
    let i_pn = t.col_index("pseudo_nitzschia");
    let i_pd = t.col_index("particulate_domoic");
    let i_cd = t.col_index("cellular_domoic");
    let i_chl = t.col_index("chla_filled");

    let date = t
        .rows
        .first()
        .and_then(|r| r.get(i_time)?.as_str())
        .unwrap_or("")
        .to_string();

    let nearest = find_nearest_valid(&t.rows, i_lat, i_lon, i_pn, target_lat, target_lon);

    let (pn, pd, cd, chl) = if let Some(row_idx) = nearest {
        let row = &t.rows[row_idx];
        let f = |i: Option<usize>| i.and_then(|idx| row.get(idx)?.as_f64());
        (f(i_pn), f(i_pd), f(i_cd), f(i_chl))
    } else {
        (None, None, None, None)
    };

    Ok(DayForecast {
        dataset: dataset.to_string(),
        date,
        p_pseudo_nitzschia: pn,
        p_particulate_domoic: pd,
        p_cellular_domoic: cd,
        chla_filled: chl,
    })
}

fn find_nearest_valid(
    rows: &[Vec<serde_json::Value>],
    i_lat: usize,
    i_lon: usize,
    i_val: Option<usize>,
    target_lat: f64,
    target_lon: f64,
) -> Option<usize> {
    let i_val = i_val?;
    let mut best_idx = None;
    let mut best_dist = f64::MAX;

    for (idx, row) in rows.iter().enumerate() {
        let val = row.get(i_val).and_then(|v| v.as_f64());
        if val.is_none() {
            continue;
        }
        let rlat = row.get(i_lat).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let rlon = row.get(i_lon).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let dist = (rlat - target_lat).powi(2) + (rlon - target_lon).powi(2);
        if dist < best_dist {
            best_dist = dist;
            best_idx = Some(idx);
        }
    }

    best_idx
}

fn risk_class(p: Option<f64>) -> &'static str {
    match p {
        Some(v) if v >= RISK_THRESHOLD => "**HIGH**",
        Some(v) if v >= 0.3 => "Moderate",
        Some(_) => "Low",
        None => "N/A",
    }
}

fn format_output(snap: &HabSnapshot) -> Result<String> {
    let lat = snap.query_lat;
    let lon_west = snap.query_lon;
    let forecasts = &snap.forecasts;

    let mut out = format!(
        "# C-HARM v3.1 — HAB Risk Forecast\n\n\
         _Nearest valid cell to ({:.2}°N, {:.2}°W)_\n\n\
         | Horizon | Date | P(*Pseudo-nitzschia*) | Risk | P(pDA) | P(cDA) | Chl-a (DINEOF) |\n\
         |---|---|---|---|---|---|---|\n",
        lat,
        lon_west.abs(),
    );

    for (i, f) in forecasts.iter().enumerate() {
        let label = match i {
            0 => "Nowcast",
            1 => "+1 day",
            2 => "+2 day",
            3 => "+3 day",
            _ => "—",
        };
        let pn_str = f
            .p_pseudo_nitzschia
            .map(|v| format!("{:.2}", v))
            .unwrap_or("—".into());
        let pd_str = f
            .p_particulate_domoic
            .map(|v| format!("{:.2}", v))
            .unwrap_or("—".into());
        let cd_str = f
            .p_cellular_domoic
            .map(|v| format!("{:.2}", v))
            .unwrap_or("—".into());
        let chl_str = f
            .chla_filled
            .map(|v| format!("{:.1} mg/m³", v))
            .unwrap_or("—".into());
        let risk = risk_class(f.p_pseudo_nitzschia);

        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            label,
            if f.date.len() > 10 {
                &f.date[..10]
            } else {
                &f.date
            },
            pn_str,
            risk,
            pd_str,
            cd_str,
            chl_str,
        ));
    }

    let any_high = forecasts
        .iter()
        .any(|f| f.p_pseudo_nitzschia.is_some_and(|v| v >= RISK_THRESHOLD));
    if any_high {
        out.push_str(
            "\n> **HAB Advisory**: Elevated *Pseudo-nitzschia* bloom probability detected. \
             Check with SC County Environmental Health before consuming locally harvested shellfish.\n",
        );
    }

    out.push_str(&format!(
        "\n_Thresholds per Anderson et al. 2016: P > {:.1} = High risk of bloom (>10⁴ cells/L). \
         Datasets: {}. \
         Source: CoastWatch ERDDAP, C-HARM v3.1. \
         Last updated: {}._\n",
        RISK_THRESHOLD,
        forecasts
            .iter()
            .map(|f| f.dataset.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        now_pacific().format("%-I:%M %p %Z"),
    ));

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn risk_class_thresholds() {
        assert_eq!(risk_class(Some(0.8)), "**HIGH**");
        assert_eq!(risk_class(Some(0.4)), "Moderate");
        assert_eq!(risk_class(Some(0.1)), "Low");
        assert_eq!(risk_class(None), "N/A");
    }

    #[test]
    fn lon_conversion() {
        assert!((lon_to_360(-122.0) - 238.0).abs() < 0.01);
        assert!((lon_to_360(238.0) - 238.0).abs() < 0.01);
    }

    #[test]
    fn find_nearest_skips_null() {
        let rows = vec![
            vec![
                serde_json::json!("t"),
                serde_json::json!(36.9),
                serde_json::json!(238.0),
                serde_json::Value::Null,
            ],
            vec![
                serde_json::json!("t"),
                serde_json::json!(36.95),
                serde_json::json!(238.05),
                serde_json::json!(0.7),
            ],
        ];
        let idx = find_nearest_valid(&rows, 1, 2, Some(3), 36.96, 238.03);
        assert_eq!(idx, Some(1));
    }
}
