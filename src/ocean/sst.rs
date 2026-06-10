use anyhow::{Result, bail};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::util::now_pacific;

use super::erddap_client::{ErddapClient, grid_selector_stride};
use super::types::SstSnapshot;

const SERVER: &str = "https://coastwatch.pfeg.noaa.gov/erddap";
const SST_DATASET: &str = "jplMURSST41";
const ANOM_DATASET: &str = "jplMURSST41anom1day";

const DEFAULT_LAT: (f64, f64) = (36.5, 37.2);
const DEFAULT_LON: (f64, f64) = (-122.5, -121.8);
const DEFAULT_STRIDE: usize = 2;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SstRequest {
    /// Southern latitude of bounding box. Default: 36.5 (Monterey Bay).
    pub lat_min: Option<f64>,
    /// Northern latitude. Default: 37.2.
    pub lat_max: Option<f64>,
    /// Western longitude. Default: -122.5.
    pub lon_min: Option<f64>,
    /// Eastern longitude. Default: -121.8.
    pub lon_max: Option<f64>,
    /// Grid stride (1 = native 0.01°, 2 = 0.02°, 5 = 0.05°). Default: 2.
    pub stride: Option<usize>,
}

pub async fn fetch_typed(erddap: &ErddapClient, req: &SstRequest) -> Result<SstSnapshot> {
    let lat_range = (
        req.lat_min.unwrap_or(DEFAULT_LAT.0),
        req.lat_max.unwrap_or(DEFAULT_LAT.1),
    );
    let lon_range = (
        req.lon_min.unwrap_or(DEFAULT_LON.0),
        req.lon_max.unwrap_or(DEFAULT_LON.1),
    );
    let stride = req.stride.unwrap_or(DEFAULT_STRIDE).max(1);

    let sel_sst = grid_selector_stride("analysed_sst", "last", lat_range, stride, lon_range, stride);
    let sel_anom = grid_selector_stride("sstAnom", "last", lat_range, stride, lon_range, stride);

    let sst_sels = vec![sel_sst];
    let anom_sels = vec![sel_anom];
    let (sst_res, anom_res) = tokio::join!(
        erddap.griddap(SERVER, SST_DATASET, &sst_sels),
        erddap.griddap(SERVER, ANOM_DATASET, &anom_sels),
    );

    let sst = sst_res?;
    let anom = anom_res.ok();

    if sst.table.rows.is_empty() {
        bail!("MUR SST returned no data for the requested area");
    }

    let i_time = sst.table.col_index("time").unwrap_or(0);
    let i_lat = sst.table.col_index("latitude").unwrap_or(1);
    let i_lon = sst.table.col_index("longitude").unwrap_or(2);
    let i_sst = sst.table.col_index("analysed_sst").unwrap_or(3);

    let timestamp = sst.table.rows.first()
        .and_then(|r| r.get(i_time)?.as_str())
        .unwrap_or("unknown")
        .to_string();

    let sst_vals: Vec<f64> = sst.table.rows.iter()
        .filter_map(|r| r.get(i_sst)?.as_f64())
        .collect();

    if sst_vals.is_empty() {
        bail!("MUR SST: all cells were NaN");
    }

    let mean_sst = sst_vals.iter().sum::<f64>() / sst_vals.len() as f64;
    let min_sst = sst_vals.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_sst = sst_vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

    let max_grad = compute_max_gradient(&sst.table.rows, i_lat, i_lon, i_sst, stride);

    let mean_anom = if let Some(ref a) = anom {
        let i_a = a.table.col_index("sstAnom").unwrap_or(3);
        let anom_vals: Vec<f64> = a.table.rows.iter()
            .filter_map(|r| r.get(i_a)?.as_f64())
            .collect();
        if anom_vals.is_empty() {
            None
        } else {
            Some(anom_vals.iter().sum::<f64>() / anom_vals.len() as f64)
        }
    } else {
        None
    };

    Ok(SstSnapshot {
        timestamp_utc: timestamp,
        mean_sst_c: mean_sst,
        min_sst_c: min_sst,
        max_sst_c: max_sst,
        mean_anom_c: mean_anom,
        max_grad_c_per_km: max_grad,
        n_cells: sst_vals.len(),
    })
}

pub async fn fetch_and_format(erddap: &ErddapClient, req: &SstRequest) -> Result<String> {
    let lat_range = (
        req.lat_min.unwrap_or(DEFAULT_LAT.0),
        req.lat_max.unwrap_or(DEFAULT_LAT.1),
    );
    let lon_range = (
        req.lon_min.unwrap_or(DEFAULT_LON.0),
        req.lon_max.unwrap_or(DEFAULT_LON.1),
    );
    let stride = req.stride.unwrap_or(DEFAULT_STRIDE).max(1);

    let sel_sst = grid_selector_stride("analysed_sst", "last", lat_range, stride, lon_range, stride);
    let sel_anom = grid_selector_stride("sstAnom", "last", lat_range, stride, lon_range, stride);

    let sst_sels = vec![sel_sst];
    let anom_sels = vec![sel_anom];
    let (sst_res, anom_res) = tokio::join!(
        erddap.griddap(SERVER, SST_DATASET, &sst_sels),
        erddap.griddap(SERVER, ANOM_DATASET, &anom_sels),
    );

    let sst = sst_res?;
    let anom = anom_res.ok();

    if sst.table.rows.is_empty() {
        bail!("MUR SST returned no data for the requested area");
    }

    let i_time = sst.table.col_index("time").unwrap_or(0);
    let i_lat = sst.table.col_index("latitude").unwrap_or(1);
    let i_lon = sst.table.col_index("longitude").unwrap_or(2);
    let i_sst = sst.table.col_index("analysed_sst").unwrap_or(3);

    let timestamp = sst.table.rows.first()
        .and_then(|r| r.get(i_time)?.as_str())
        .unwrap_or("unknown")
        .to_string();

    let sst_vals: Vec<f64> = sst.table.rows.iter()
        .filter_map(|r| r.get(i_sst)?.as_f64())
        .collect();

    if sst_vals.is_empty() {
        bail!("MUR SST: all cells were NaN");
    }

    let mean_sst = sst_vals.iter().sum::<f64>() / sst_vals.len() as f64;
    let min_sst = sst_vals.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_sst = sst_vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

    let max_grad = compute_max_gradient(&sst.table.rows, i_lat, i_lon, i_sst, stride);

    let (mean_anom, anom_count) = if let Some(ref a) = anom {
        let i_a = a.table.col_index("sstAnom").unwrap_or(3);
        let anom_vals: Vec<f64> = a.table.rows.iter()
            .filter_map(|r| r.get(i_a)?.as_f64())
            .collect();
        if anom_vals.is_empty() {
            (None, 0)
        } else {
            let mean = anom_vals.iter().sum::<f64>() / anom_vals.len() as f64;
            (Some(mean), anom_vals.len())
        }
    } else {
        (None, 0)
    };

    let mut out = format!(
        "# MUR SST — Monterey Bay\n\n\
         _0.01° resolution (L4 gap-filled), JPL MUR v4.1_\n\n\
         **Snapshot**: {}\n\n\
         ## Sea surface temperature\n\n\
         | Metric | Value |\n\
         |---|---|\n\
         | Mean SST | {:.2}°C ({:.1}°F) |\n\
         | Min SST | {:.2}°C |\n\
         | Max SST | {:.2}°C |\n\
         | Range | {:.2}°C |\n\
         | Grid cells | {} |\n",
        timestamp,
        mean_sst,
        mean_sst * 9.0 / 5.0 + 32.0,
        min_sst,
        max_sst,
        max_sst - min_sst,
        sst_vals.len(),
    );

    if let Some(grad) = max_grad {
        out.push_str(&format!(
            "| Max gradient | {:.3}°C/km{} |\n",
            grad,
            if grad > 0.3 { " ← frontal zone" } else { "" },
        ));
    }

    if let Some(anom) = mean_anom {
        let anom_note = if anom > 2.0 {
            " (warm anomaly — marine heatwave territory)"
        } else if anom > 1.0 {
            " (moderately warm)"
        } else if anom < -1.0 {
            " (cool anomaly — active upwelling signal)"
        } else {
            ""
        };
        out.push_str(&format!(
            "\n## SST anomaly (vs 2003–2014 climatology)\n\n\
             - **Mean anomaly**: {:+.2}°C{}\n\
             - Based on {} cells\n",
            anom, anom_note, anom_count,
        ));
    }

    out.push_str(&format!(
        "\n_Bounding box: ({:.1}–{:.1}°N, {:.1}–{:.1}°W), stride {}. \
         MUR L4 is gap-filled (no NaN). Anomaly base: 2003–2014. \
         Source: CoastWatch ERDDAP (`{}`). \
         Last updated: {}._\n",
        lat_range.0, lat_range.1, lon_range.0.abs(), lon_range.1.abs(),
        stride, SST_DATASET,
        now_pacific().format("%-I:%M %p %Z"),
    ));

    Ok(out)
}

fn compute_max_gradient(
    rows: &[Vec<serde_json::Value>],
    i_lat: usize,
    i_lon: usize,
    i_sst: usize,
    stride: usize,
) -> Option<f64> {
    // Simple: find max |ΔT| between adjacent rows that share lat or lon
    let km_per_deg_lat = 111.0;
    let km_per_deg_lon = 111.0 * (36.85_f64.to_radians().cos());
    let grid_spacing_deg = 0.01 * stride as f64;

    let mut max_grad = 0.0_f64;

    for i in 0..rows.len() {
        let lat_i = rows[i].get(i_lat).and_then(|v| v.as_f64())?;
        let lon_i = rows[i].get(i_lon).and_then(|v| v.as_f64())?;
        let sst_i = rows[i].get(i_sst).and_then(|v| v.as_f64())?;

        for j in (i + 1)..rows.len().min(i + 200) {
            let lat_j = rows[j].get(i_lat).and_then(|v| v.as_f64()).unwrap_or(0.0);
            let lon_j = rows[j].get(i_lon).and_then(|v| v.as_f64()).unwrap_or(0.0);
            let sst_j = match rows[j].get(i_sst).and_then(|v| v.as_f64()) {
                Some(v) => v,
                None => continue,
            };

            let dlat = (lat_j - lat_i).abs();
            let dlon = (lon_j - lon_i).abs();

            let is_neighbor = (dlat < grid_spacing_deg * 1.5 && dlon < 0.001)
                || (dlon < grid_spacing_deg * 1.5 && dlat < 0.001);

            if !is_neighbor {
                continue;
            }

            let dist_km = if dlat > dlon {
                dlat * km_per_deg_lat
            } else {
                dlon * km_per_deg_lon
            };

            if dist_km > 0.01 {
                let grad = (sst_j - sst_i).abs() / dist_km;
                max_grad = max_grad.max(grad);
            }
        }
    }

    if max_grad > 0.0 { Some(max_grad) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradient_computation() {
        let rows = vec![
            vec![
                serde_json::json!("2026-05-01"),
                serde_json::json!(36.9),
                serde_json::json!(-122.0),
                serde_json::json!(14.0),
            ],
            vec![
                serde_json::json!("2026-05-01"),
                serde_json::json!(36.9),
                serde_json::json!(-121.98),
                serde_json::json!(14.5),
            ],
        ];
        let grad = compute_max_gradient(&rows, 1, 2, 3, 2);
        assert!(grad.is_some());
        let g = grad.unwrap();
        assert!(g > 0.1, "gradient too low: {}", g);
    }
}
