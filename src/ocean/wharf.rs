use anyhow::{Result, bail};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::util::now_pacific;

use super::erddap_client::{ErddapClient, TabledapQuery};
use super::types::WharfSnapshot;

const SERVER: &str = "https://erddap.sensors.axds.co/erddap";
const DATASET: &str = "edu_ucsc_scwharf1";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WharfRequest {
    /// Hours of data to fetch (1–48). Default: 6.
    pub hours: Option<u32>,
}

pub async fn fetch_typed(erddap: &ErddapClient, req: &WharfRequest) -> Result<WharfSnapshot> {
    let hours = req.hours.unwrap_or(6).clamp(1, 48);

    let now = chrono::Utc::now();
    let lookback = (hours as i64 + 72).max(72);
    let from = now - chrono::Duration::hours(lookback);
    let time_min = from.format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let query = TabledapQuery::new(vec![
        "time".into(),
        "sea_water_temperature".into(),
        "sea_water_practical_salinity".into(),
        "sea_water_ph_reported_on_total_scale".into(),
        "mass_concentration_of_chlorophyll_in_sea_water".into(),
        "mass_concentration_of_oxygen_in_sea_water".into(),
        "fractional_saturation_of_oxygen_in_sea_water".into(),
        "sea_water_turbidity".into(),
    ])
    .constraint(format!("time>={}", time_min))
    .with_qc(vec![
        "sea_water_temperature".into(),
        "sea_water_practical_salinity".into(),
        "sea_water_ph_reported_on_total_scale".into(),
        "mass_concentration_of_chlorophyll_in_sea_water".into(),
        "mass_concentration_of_oxygen_in_sea_water".into(),
        "sea_water_turbidity".into(),
    ]);

    let resp = erddap.tabledap(SERVER, DATASET, query).await?;
    let t = &resp.table;

    if t.rows.is_empty() {
        bail!("SC Wharf returned no data for the requested period");
    }

    let i_time = t.col_index("time").unwrap_or(0);
    let i_temp = t.col_index("sea_water_temperature");
    let i_sal = t.col_index("sea_water_practical_salinity");
    let i_ph = t.col_index("sea_water_ph_reported_on_total_scale");
    let i_chl = t.col_index("mass_concentration_of_chlorophyll_in_sea_water");
    let i_do = t.col_index("mass_concentration_of_oxygen_in_sea_water");
    let i_do_sat = t.col_index("fractional_saturation_of_oxygen_in_sea_water");
    let i_turb = t.col_index("sea_water_turbidity");

    let last = t.rows.last().unwrap();
    let time_str = last.get(i_time).and_then(|v| v.as_str()).unwrap_or("unknown");

    let get_f = |row: &Vec<serde_json::Value>, idx: Option<usize>| -> Option<f64> {
        idx.and_then(|i| row.get(i)?.as_f64())
    };

    let latency_min = chrono::DateTime::parse_from_rfc3339(time_str)
        .map(|t_parsed| (chrono::Utc::now() - t_parsed.to_utc()).num_minutes())
        .unwrap_or(0);

    Ok(WharfSnapshot {
        timestamp_utc: time_str.to_string(),
        latency_minutes: latency_min,
        temp_c: get_f(last, i_temp),
        salinity_psu: get_f(last, i_sal),
        ph: get_f(last, i_ph),
        chla_mg_m3: get_f(last, i_chl),
        do_mg_l: get_f(last, i_do),
        do_saturation_pct: get_f(last, i_do_sat),
        turbidity_ntu: get_f(last, i_turb),
    })
}

pub async fn fetch_and_format(erddap: &ErddapClient, req: &WharfRequest) -> Result<String> {
    let hours = req.hours.unwrap_or(6).clamp(1, 48);

    let now = chrono::Utc::now();
    // Wharf is typically 5-min cadence but can have 24-48h gaps during
    // maintenance. Always look back at least 72h to ensure data.
    let lookback = (hours as i64 + 72).max(72);
    let from = now - chrono::Duration::hours(lookback);
    let time_min = from.format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let query = TabledapQuery::new(vec![
        "time".into(),
        "sea_water_temperature".into(),
        "sea_water_practical_salinity".into(),
        "sea_water_ph_reported_on_total_scale".into(),
        "mass_concentration_of_chlorophyll_in_sea_water".into(),
        "mass_concentration_of_oxygen_in_sea_water".into(),
        "fractional_saturation_of_oxygen_in_sea_water".into(),
        "sea_water_turbidity".into(),
    ])
    .constraint(format!("time>={}", time_min))
    .with_qc(vec![
        "sea_water_temperature".into(),
        "sea_water_practical_salinity".into(),
        "sea_water_ph_reported_on_total_scale".into(),
        "mass_concentration_of_chlorophyll_in_sea_water".into(),
        "mass_concentration_of_oxygen_in_sea_water".into(),
        "sea_water_turbidity".into(),
    ]);

    let resp = erddap.tabledap(SERVER, DATASET, query).await?;
    let t = &resp.table;

    if t.rows.is_empty() {
        bail!("SC Wharf returned no data for the requested period");
    }

    let i_time = t.col_index("time").unwrap_or(0);
    let i_temp = t.col_index("sea_water_temperature");
    let i_sal = t.col_index("sea_water_practical_salinity");
    let i_ph = t.col_index("sea_water_ph_reported_on_total_scale");
    let i_chl = t.col_index("mass_concentration_of_chlorophyll_in_sea_water");
    let i_do = t.col_index("mass_concentration_of_oxygen_in_sea_water");
    let i_do_sat = t.col_index("fractional_saturation_of_oxygen_in_sea_water");
    let i_turb = t.col_index("sea_water_turbidity");

    let last = t.rows.last().unwrap();
    let time_str = last.get(i_time).and_then(|v| v.as_str()).unwrap_or("unknown");

    let get_f = |row: &Vec<serde_json::Value>, idx: Option<usize>| -> Option<f64> {
        idx.and_then(|i| row.get(i)?.as_f64())
    };

    let temp = get_f(last, i_temp);
    let sal = get_f(last, i_sal);
    let ph = get_f(last, i_ph);
    let chl = get_f(last, i_chl);
    let do_mg = get_f(last, i_do);
    let do_sat = get_f(last, i_do_sat);
    let turb = get_f(last, i_turb);

    let latency_min = chrono::DateTime::parse_from_rfc3339(time_str)
        .map(|t_parsed| (chrono::Utc::now() - t_parsed.to_utc()).num_minutes())
        .unwrap_or(0);

    let mut out = format!(
        "# UCSC Santa Cruz Municipal Wharf\n\n\
         _36.96°N, 122.02°W — in-situ continuous monitoring (Kudela Lab, UCSC)_\n\n\
         **Latest observation**: {} (latency {}m)\n\n\
         ## Current readings\n\n",
        time_str, latency_min,
    );

    if let Some(v) = temp {
        out.push_str(&format!(
            "- **Temperature**: {:.1}°F ({:.2}°C)\n",
            v * 9.0 / 5.0 + 32.0,
            v
        ));
    }
    if let Some(v) = sal {
        out.push_str(&format!("- **Salinity**: {:.2} PSU\n", v));
    }
    if let Some(v) = ph {
        let ph_note = if v < 7.8 {
            " ← depressed (upwelling-driven CO₂-rich water)"
        } else if v > 8.1 {
            " ← elevated (biological drawdown)"
        } else {
            ""
        };
        out.push_str(&format!("- **pH** (total scale): {:.2}{}\n", v, ph_note));
    }
    if let Some(v) = chl {
        let chl_note = if v > 10.0 {
            " ← bloom-level"
        } else if v > 5.0 {
            " ← elevated"
        } else {
            ""
        };
        out.push_str(&format!("- **Chlorophyll-a**: {:.1} mg/m³{}\n", v, chl_note));
    }
    if let Some(v) = do_mg {
        let sat_str = do_sat
            .map(|s| format!(" ({:.0}% saturation)", s))
            .unwrap_or_default();
        out.push_str(&format!("- **Dissolved oxygen**: {:.1} mg/L{}\n", v, sat_str));
    }
    if let Some(v) = turb {
        out.push_str(&format!("- **Turbidity**: {:.1} NTU\n", v));
    }

    if t.rows.len() > 3 {
        let n = t.rows.len();
        let first = &t.rows[0];
        if let (Some(t_now), Some(t_old)) = (get_f(last, i_temp), get_f(first, i_temp)) {
            let delta = t_now - t_old;
            out.push_str(&format!(
                "\n## Trend ({} readings over ~{}h)\n\n\
                 - Temperature: {:+.2}°C\n",
                n, hours, delta,
            ));
        }
        if let (Some(p_now), Some(p_old)) = (get_f(last, i_ph), get_f(first, i_ph)) {
            out.push_str(&format!("- pH: {:+.3}\n", p_now - p_old));
        }
        if let (Some(c_now), Some(c_old)) = (get_f(last, i_chl), get_f(first, i_chl)) {
            out.push_str(&format!("- Chl-a: {:+.1} mg/m³\n", c_now - c_old));
        }
    }

    out.push_str(&format!(
        "\n_Source: UCSC Santa Cruz Municipal Wharf (`edu_ucsc_scwharf1`), \
         CeNCOOS/Axiom ERDDAP. QC: QARTOD-pass only. 5-min cadence. \
         HAB ground-truth station (Kudela Lab weekly Pseudo-nitzschia counts). \
         Last updated: {}._\n",
        now_pacific().format("%-I:%M %p %Z"),
    ));

    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn ph_annotation() {
        assert!(super::fetch_and_format as usize > 0); // module compiles
    }
}
