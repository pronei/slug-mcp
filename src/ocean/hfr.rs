use anyhow::{Result, bail};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::util::now_pacific;

use super::erddap_client::{ErddapClient, grid_selector};
use super::types::HfrSnapshot as HfrSnapTyped;

const SERVER: &str = "https://coastwatch.pfeg.noaa.gov/erddap";

const DEFAULT_LAT: (f64, f64) = (36.5, 37.2);
const DEFAULT_LON: (f64, f64) = (-122.5, -121.8);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HfrRequest {
    /// Resolution: "6km" (default, bay-scale) or "2km" (nearshore detail).
    pub resolution: Option<String>,
    /// Southern latitude. Default: 36.5.
    pub lat_min: Option<f64>,
    /// Northern latitude. Default: 37.2.
    pub lat_max: Option<f64>,
    /// Western longitude. Default: -122.5.
    pub lon_min: Option<f64>,
    /// Eastern longitude. Default: -121.8.
    pub lon_max: Option<f64>,
}

pub async fn fetch_typed(erddap: &ErddapClient, req: &HfrRequest) -> Result<HfrSnapTyped> {
    let res = req.resolution.as_deref().unwrap_or("6km");
    let dataset = match res {
        "2km" => "ucsdHfrW2",
        _ => "ucsdHfrW6",
    };

    let lat_range = (
        req.lat_min.unwrap_or(DEFAULT_LAT.0),
        req.lat_max.unwrap_or(DEFAULT_LAT.1),
    );
    let lon_range = (
        req.lon_min.unwrap_or(DEFAULT_LON.0),
        req.lon_max.unwrap_or(DEFAULT_LON.1),
    );

    let resp = {
        let now = chrono::Utc::now();
        let mut result = None;
        for hours_back in 0..6 {
            let t = now - chrono::Duration::hours(hours_back);
            let time_str = format!("({})", t.format("%Y-%m-%dT%H:00:00Z"));
            let sel_u = grid_selector("water_u", &time_str, lat_range, lon_range);
            let sel_v = grid_selector("water_v", &time_str, lat_range, lon_range);
            match erddap.griddap(SERVER, dataset, &[sel_u, sel_v]).await {
                Ok(r) => {
                    let i_u_check = r.table.col_index("water_u");
                    let has_valid = r.table.rows.iter().any(|row| {
                        i_u_check
                            .and_then(|i| row.get(i)?.as_f64())
                            .is_some()
                    });
                    if has_valid {
                        result = Some(r);
                        break;
                    }
                }
                Err(_) => continue,
            }
        }
        result.ok_or_else(|| anyhow::anyhow!("HF Radar: no valid data in the last 6 hours"))?
    };
    let t = &resp.table;

    if t.rows.is_empty() {
        bail!("HF Radar returned no data for the requested area");
    }

    let i_time = t.col_index("time").unwrap_or(0);
    let i_lat = t.col_index("latitude").unwrap_or(1);
    let i_lon = t.col_index("longitude").unwrap_or(2);
    let i_u = t.col_index("water_u");
    let i_v = t.col_index("water_v");

    let timestamp = t.rows.first()
        .and_then(|r| r.get(i_time)?.as_str())
        .unwrap_or("unknown")
        .to_string();

    let mut cells: Vec<Cell> = Vec::new();
    let mut total_cells = 0;

    for row in &t.rows {
        total_cells += 1;
        let lat = row.get(i_lat).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let lon = row.get(i_lon).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let u = match i_u.and_then(|i| row.get(i)?.as_f64()) {
            Some(v) if v.abs() < 2.0 => v,
            _ => continue,
        };
        let v = match i_v.and_then(|i| row.get(i)?.as_f64()) {
            Some(v) if v.abs() < 2.0 => v,
            _ => continue,
        };
        cells.push(Cell { lat, lon, u, v });
    }

    if cells.is_empty() {
        bail!("HF Radar: all cells were NaN or failed QC in the requested area");
    }

    let n_valid = cells.len();
    let mean_u = cells.iter().map(|c| c.u).sum::<f64>() / n_valid as f64;
    let mean_v = cells.iter().map(|c| c.v).sum::<f64>() / n_valid as f64;
    let mean_speed = cells
        .iter()
        .map(|c| (c.u * c.u + c.v * c.v).sqrt())
        .sum::<f64>()
        / n_valid as f64;
    let max_speed = cells
        .iter()
        .map(|c| (c.u * c.u + c.v * c.v).sqrt())
        .fold(0.0_f64, f64::max);

    let flow_dir = (mean_v.atan2(mean_u).to_degrees() + 360.0) % 360.0;

    let divergence = compute_divergence(&cells, res);

    Ok(HfrSnapTyped {
        timestamp_utc: timestamp,
        resolution: res.to_string(),
        mean_speed_ms: mean_speed,
        max_speed_ms: max_speed,
        mean_u_ms: mean_u,
        mean_v_ms: mean_v,
        flow_direction_deg: flow_dir,
        n_cells_valid: n_valid,
        n_cells_total: total_cells,
        divergence_per_s: divergence,
    })
}

pub async fn fetch_and_format(erddap: &ErddapClient, req: &HfrRequest) -> Result<String> {
    let res = req.resolution.as_deref().unwrap_or("6km");
    let dataset = match res {
        "2km" => "ucsdHfrW2",
        _ => "ucsdHfrW6",
    };

    let lat_range = (
        req.lat_min.unwrap_or(DEFAULT_LAT.0),
        req.lat_max.unwrap_or(DEFAULT_LAT.1),
    );
    let lon_range = (
        req.lon_min.unwrap_or(DEFAULT_LON.0),
        req.lon_max.unwrap_or(DEFAULT_LON.1),
    );

    // The very latest HFR timestamp often has all-NaN cells while processing
    // completes. Try explicit recent hours until we find valid data.
    let resp = {
        let now = chrono::Utc::now();
        let mut result = None;
        for hours_back in 0..6 {
            let t = now - chrono::Duration::hours(hours_back);
            let time_str = format!("({})", t.format("%Y-%m-%dT%H:00:00Z"));
            let sel_u = grid_selector("water_u", &time_str, lat_range, lon_range);
            let sel_v = grid_selector("water_v", &time_str, lat_range, lon_range);
            match erddap.griddap(SERVER, dataset, &[sel_u, sel_v]).await {
                Ok(r) => {
                    let i_u_check = r.table.col_index("water_u");
                    let has_valid = r.table.rows.iter().any(|row| {
                        i_u_check
                            .and_then(|i| row.get(i)?.as_f64())
                            .is_some()
                    });
                    if has_valid {
                        result = Some(r);
                        break;
                    }
                }
                Err(_) => continue,
            }
        }
        result.ok_or_else(|| anyhow::anyhow!("HF Radar: no valid data in the last 6 hours"))?
    };
    let t = &resp.table;

    if t.rows.is_empty() {
        bail!("HF Radar returned no data for the requested area");
    }

    let i_time = t.col_index("time").unwrap_or(0);
    let i_lat = t.col_index("latitude").unwrap_or(1);
    let i_lon = t.col_index("longitude").unwrap_or(2);
    let i_u = t.col_index("water_u");
    let i_v = t.col_index("water_v");

    let timestamp = t.rows.first()
        .and_then(|r| r.get(i_time)?.as_str())
        .unwrap_or("unknown")
        .to_string();

    let mut cells: Vec<Cell> = Vec::new();
    let mut total_cells = 0;

    for row in &t.rows {
        total_cells += 1;
        let lat = row.get(i_lat).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let lon = row.get(i_lon).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let u = match i_u.and_then(|i| row.get(i)?.as_f64()) {
            Some(v) if v.abs() < 2.0 => v,
            _ => continue,
        };
        let v = match i_v.and_then(|i| row.get(i)?.as_f64()) {
            Some(v) if v.abs() < 2.0 => v,
            _ => continue,
        };
        cells.push(Cell { lat, lon, u, v });
    }

    if cells.is_empty() {
        bail!("HF Radar: all cells were NaN or failed QC in the requested area");
    }

    let n_valid = cells.len();
    let mean_u = cells.iter().map(|c| c.u).sum::<f64>() / n_valid as f64;
    let mean_v = cells.iter().map(|c| c.v).sum::<f64>() / n_valid as f64;
    let mean_speed = cells
        .iter()
        .map(|c| (c.u * c.u + c.v * c.v).sqrt())
        .sum::<f64>()
        / n_valid as f64;
    let max_speed = cells
        .iter()
        .map(|c| (c.u * c.u + c.v * c.v).sqrt())
        .fold(0.0_f64, f64::max);

    let flow_dir = (mean_v.atan2(mean_u).to_degrees() + 360.0) % 360.0;
    let flow_compass = crate::util::degrees_to_compass(flow_dir);

    let divergence = compute_divergence(&cells, res);

    let mut out = format!(
        "# HF Radar Surface Currents — Monterey Bay\n\n\
         _Resolution: {} ({})_\n\n\
         **Snapshot**: {}\n\n\
         ## Bay-mean current\n\n\
         | Metric | Value |\n\
         |---|---|\n\
         | Mean speed | {:.3} m/s ({:.1} cm/s) |\n\
         | Max speed | {:.3} m/s |\n\
         | Mean flow | toward {} ({:.0}°) |\n\
         | Mean u (east) | {:+.3} m/s |\n\
         | Mean v (north) | {:+.3} m/s |\n\
         | Valid cells | {} / {} ({:.0}%) |\n",
        dataset, res,
        timestamp,
        mean_speed, mean_speed * 100.0,
        max_speed,
        flow_compass, flow_dir,
        mean_u,
        mean_v,
        n_valid, total_cells,
        n_valid as f64 / total_cells as f64 * 100.0,
    );

    if let Some(div) = divergence {
        let div_note = if div > 1e-6 {
            " ← positive (surface divergence, consistent with upwelling)"
        } else if div < -1e-6 {
            " ← negative (convergence)"
        } else {
            ""
        };
        out.push_str(&format!(
            "| Divergence | {:.2e} s⁻¹{} |\n",
            div, div_note,
        ));
    }

    out.push_str(&format!(
        "\n_Source: UCSD HFRNet via CoastWatch ERDDAP (`{}`). \
         QC: |u|,|v| < 2.0 m/s gross-range filter. \
         Coverage gaps are common in the canyon shadow (~36.7°N, -122°W). \
         Last updated: {}._\n",
        dataset,
        now_pacific().format("%-I:%M %p %Z"),
    ));

    Ok(out)
}

fn compute_divergence(cells: &[Cell], resolution: &str) -> Option<f64> {
    if cells.len() < 4 {
        return None;
    }

    let grid_km = match resolution {
        "2km" => 2.0,
        _ => 6.0,
    };
    let grid_m = grid_km * 1000.0;

    // Sort cells into a sparse grid and compute ∂u/∂x + ∂v/∂y at each
    // interior point. Take the mean divergence.
    let mut du_dx_sum = 0.0;
    let mut dv_dy_sum = 0.0;
    let mut count = 0;

    let deg_per_cell_lat = grid_km / 111.0;
    let deg_per_cell_lon = grid_km / (111.0 * 36.85_f64.to_radians().cos());

    for i in 0..cells.len() {
        let east = cells.iter().find(|c| {
            (c.lat - cells[i].lat).abs() < deg_per_cell_lat * 0.5
                && (c.lon - cells[i].lon - deg_per_cell_lon).abs() < deg_per_cell_lon * 0.5
        });
        let north = cells.iter().find(|c| {
            (c.lon - cells[i].lon).abs() < deg_per_cell_lon * 0.5
                && (c.lat - cells[i].lat - deg_per_cell_lat).abs() < deg_per_cell_lat * 0.5
        });

        if let (Some(e), Some(n)) = (east, north) {
            du_dx_sum += (e.u - cells[i].u) / grid_m;
            dv_dy_sum += (n.v - cells[i].v) / grid_m;
            count += 1;
        }
    }

    if count > 0 {
        Some((du_dx_sum + dv_dy_sum) / count as f64)
    } else {
        None
    }
}

struct Cell {
    lat: f64,
    lon: f64,
    u: f64,
    v: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divergence_uniform_field() {
        let cells = vec![
            Cell { lat: 36.8, lon: -122.0, u: 0.1, v: 0.1 },
            Cell { lat: 36.8, lon: -121.94, u: 0.1, v: 0.1 },
            Cell { lat: 36.854, lon: -122.0, u: 0.1, v: 0.1 },
            Cell { lat: 36.854, lon: -121.94, u: 0.1, v: 0.1 },
        ];
        let div = compute_divergence(&cells, "6km");
        assert!(div.is_some());
        assert!(div.unwrap().abs() < 1e-6, "uniform field should have ~0 divergence");
    }
}
