use std::sync::Arc;

use anyhow::{Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cache::CacheStore;
use crate::util::now_pacific;

use super::types::UpwellingSnapshot;

const CUTI_URL: &str = "https://www.mjacox.com/wp-content/uploads/CUTI_daily.csv";
const BEUTI_URL: &str = "https://www.mjacox.com/wp-content/uploads/BEUTI_daily.csv";
const DEFAULT_LAT_BAND: &str = "37N";
const UPWELLING_THRESHOLD: f64 = 1.0;
const CLIM_START_YEAR: i32 = 1988;
const CLIM_END_YEAR: i32 = 2018;

const LAT_BANDS: &[&str] = &[
    "31N", "32N", "33N", "34N", "35N", "36N", "37N", "38N", "39N", "40N", "41N", "42N", "43N",
    "44N", "45N", "46N", "47N",
];

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpwellingRequest {
    /// Latitude band (e.g. "37N" for Monterey Bay/Santa Cruz, "36N" for southern
    /// Monterey). Santa Cruz (36.97°N) falls in the 37N band (36.5–37.5°N).
    /// Available bands: 31N through 47N in 1° steps. Default: "37N".
    pub lat_band: Option<String>,
    /// Number of days of recent history to include (1–90). Default: 7.
    pub days_back: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexRow {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub values: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexData {
    pub rows: Vec<IndexRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClimatologyEntry {
    mean: f64,
    std: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Climatology {
    by_doy: Vec<Vec<ClimatologyEntry>>,
}

fn day_of_year(month: u32, day: u32) -> usize {
    let cumulative = [0, 31, 60, 91, 121, 152, 182, 213, 244, 274, 305, 335];
    (cumulative[month as usize - 1] + day as usize).saturating_sub(1)
}

fn lat_band_index(band: &str) -> Option<usize> {
    LAT_BANDS.iter().position(|b| b.eq_ignore_ascii_case(band))
}

fn parse_index_csv(body: &str) -> Result<IndexData> {
    let mut lines = body.lines();
    let header = lines.next().unwrap_or("");
    let cols: Vec<&str> = header.split(',').collect();
    if cols.len() < 20 || cols[0] != "year" {
        bail!("unexpected CSV header: {}", header);
    }

    let mut rows = Vec::with_capacity(14000);
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split(',').collect();
        if fields.len() < 20 {
            continue;
        }
        let year: i32 = fields[0].parse().unwrap_or(0);
        let month: u32 = fields[1].parse().unwrap_or(0);
        let day: u32 = fields[2].parse().unwrap_or(0);
        if year == 0 || month == 0 || day == 0 {
            continue;
        }
        let values: Vec<f64> = fields[3..20]
            .iter()
            .map(|s| s.trim().parse::<f64>().unwrap_or(f64::NAN))
            .collect();
        rows.push(IndexRow {
            year,
            month,
            day,
            values,
        });
    }

    if rows.is_empty() {
        bail!("CSV contained no data rows");
    }
    Ok(IndexData { rows })
}

fn compute_climatology(data: &IndexData) -> Climatology {
    let n_bands = LAT_BANDS.len();
    let mut sums = vec![vec![(0.0_f64, 0.0_f64, 0u32); n_bands]; 366];

    for row in &data.rows {
        if row.year < CLIM_START_YEAR || row.year > CLIM_END_YEAR {
            continue;
        }
        let doy = day_of_year(row.month, row.day);
        if doy >= 366 {
            continue;
        }
        for (i, &val) in row.values.iter().enumerate().take(n_bands) {
            if val.is_finite() {
                sums[doy][i].0 += val;
                sums[doy][i].1 += val * val;
                sums[doy][i].2 += 1;
            }
        }
    }

    let by_doy = sums
        .iter()
        .map(|day| {
            day.iter()
                .map(|&(sum, sum_sq, n)| {
                    if n < 5 {
                        ClimatologyEntry {
                            mean: f64::NAN,
                            std: f64::NAN,
                        }
                    } else {
                        let n_f = n as f64;
                        let mean = sum / n_f;
                        let variance = (sum_sq / n_f - mean * mean).max(0.0);
                        ClimatologyEntry {
                            mean,
                            std: variance.sqrt(),
                        }
                    }
                })
                .collect()
        })
        .collect();

    Climatology { by_doy }
}

pub async fn fetch_typed(
    http: &reqwest::Client,
    cache: &Arc<CacheStore>,
    req: &UpwellingRequest,
) -> Result<UpwellingSnapshot> {
    let lat_band = req.lat_band.as_deref().unwrap_or(DEFAULT_LAT_BAND);
    let band_idx = lat_band_index(lat_band).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown latitude band '{}'. Available: 31N–47N in 1° steps",
            lat_band
        )
    })?;

    // Read the parsed archives as `Arc` to avoid deep-cloning ~2 MB out of the
    // cache on every call.
    let (cuti_data, beuti_data) = tokio::try_join!(
        fetch_index_arc(http, cache, "cuti", CUTI_URL),
        fetch_index_arc(http, cache, "beuti", BEUTI_URL),
    )?;

    // Compute-or-get climatology; the index data is only cloned into the
    // compute path on a cache miss.
    let cuti_clim = get_or_compute_climatology(cache, "ocean:cuti_climatology", &cuti_data).await;
    let beuti_clim =
        get_or_compute_climatology(cache, "ocean:beuti_climatology", &beuti_data).await;

    let n_cuti = cuti_data.rows.len();
    let n_beuti = beuti_data.rows.len();
    if n_cuti == 0 || n_beuti == 0 {
        bail!("CUTI or BEUTI data is empty");
    }

    let latest_cuti = &cuti_data.rows[n_cuti - 1];
    let latest_beuti = &beuti_data.rows[n_beuti - 1];

    let today_cuti = latest_cuti.values[band_idx];
    let today_beuti = latest_beuti.values[band_idx];

    let doy = day_of_year(latest_cuti.month, latest_cuti.day);
    let clim_cuti = &cuti_clim.by_doy[doy.min(365)][band_idx];
    let clim_beuti = &beuti_clim.by_doy[doy.min(365)][band_idx];

    let anomaly_cuti = today_cuti - clim_cuti.mean;
    let z_cuti = if clim_cuti.std > 0.001 {
        anomaly_cuti / clim_cuti.std
    } else {
        0.0
    };

    let rolling_5d_cuti = if n_cuti >= 5 {
        let s: f64 = cuti_data.rows[n_cuti - 5..]
            .iter()
            .map(|r| r.values[band_idx])
            .filter(|v| v.is_finite())
            .sum();
        s / 5.0
    } else {
        today_cuti
    };

    let rolling_5d_beuti = if n_beuti >= 5 {
        let s: f64 = beuti_data.rows[n_beuti - 5..]
            .iter()
            .map(|r| r.values[band_idx])
            .filter(|v| v.is_finite())
            .sum();
        s / 5.0
    } else {
        today_beuti
    };

    let thirty_start = n_cuti.saturating_sub(30);
    let days_above = cuti_data.rows[thirty_start..]
        .iter()
        .filter(|r| r.values[band_idx] > UPWELLING_THRESHOLD)
        .count();

    let data_date = format!(
        "{}-{:02}-{:02}",
        latest_cuti.year, latest_cuti.month, latest_cuti.day
    );

    let regime = classify_regime(today_cuti, rolling_5d_cuti, days_above);

    Ok(UpwellingSnapshot {
        lat_band: lat_band.to_string(),
        data_date,
        today_cuti,
        today_beuti,
        climatology_cuti: clim_cuti.mean,
        climatology_beuti: clim_beuti.mean,
        anomaly_cuti,
        z_cuti,
        rolling_5d_cuti,
        rolling_5d_beuti,
        days_above_threshold_30d: days_above,
        regime: regime.to_string(),
    })
}

pub async fn fetch_and_format(
    http: &reqwest::Client,
    cache: &Arc<CacheStore>,
    req: &UpwellingRequest,
) -> Result<String> {
    let lat_band = req.lat_band.as_deref().unwrap_or(DEFAULT_LAT_BAND);
    let band_idx = lat_band_index(lat_band).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown latitude band '{}'. Available: 31N–47N in 1° steps",
            lat_band
        )
    })?;
    let days_back = req.days_back.unwrap_or(7).clamp(1, 90) as usize;

    // Read the parsed archives as `Arc` to avoid deep-cloning ~2 MB out of the
    // cache on every call.
    let (cuti_data, beuti_data) = tokio::try_join!(
        fetch_index_arc(http, cache, "cuti", CUTI_URL),
        fetch_index_arc(http, cache, "beuti", BEUTI_URL),
    )?;

    // Compute-or-get climatology; the index data is only cloned into the
    // compute path on a cache miss.
    let cuti_clim = get_or_compute_climatology(cache, "ocean:cuti_climatology", &cuti_data).await;
    let beuti_clim =
        get_or_compute_climatology(cache, "ocean:beuti_climatology", &beuti_data).await;

    let n_cuti = cuti_data.rows.len();
    let n_beuti = beuti_data.rows.len();
    if n_cuti == 0 || n_beuti == 0 {
        bail!("CUTI or BEUTI data is empty");
    }

    let latest_cuti = &cuti_data.rows[n_cuti - 1];
    let latest_beuti = &beuti_data.rows[n_beuti - 1];

    let today_cuti = latest_cuti.values[band_idx];
    let today_beuti = latest_beuti.values[band_idx];

    let doy = day_of_year(latest_cuti.month, latest_cuti.day);
    let clim_cuti = &cuti_clim.by_doy[doy.min(365)][band_idx];
    let clim_beuti = &beuti_clim.by_doy[doy.min(365)][band_idx];

    let anomaly_cuti = today_cuti - clim_cuti.mean;
    let z_cuti = if clim_cuti.std > 0.001 {
        anomaly_cuti / clim_cuti.std
    } else {
        0.0
    };

    let anomaly_beuti = today_beuti - clim_beuti.mean;

    let start = n_cuti.saturating_sub(days_back);
    let recent_cuti: Vec<_> = cuti_data.rows[start..].to_vec();
    let recent_beuti: Vec<_> =
        beuti_data.rows[start.max(n_beuti.saturating_sub(days_back))..n_beuti].to_vec();

    let rolling_5d_cuti = if n_cuti >= 5 {
        let s: f64 = cuti_data.rows[n_cuti - 5..]
            .iter()
            .map(|r| r.values[band_idx])
            .filter(|v| v.is_finite())
            .sum();
        s / 5.0
    } else {
        today_cuti
    };

    let rolling_5d_beuti = if n_beuti >= 5 {
        let s: f64 = beuti_data.rows[n_beuti - 5..]
            .iter()
            .map(|r| r.values[band_idx])
            .filter(|v| v.is_finite())
            .sum();
        s / 5.0
    } else {
        today_beuti
    };

    let thirty_start = n_cuti.saturating_sub(30);
    let days_above = cuti_data.rows[thirty_start..]
        .iter()
        .filter(|r| r.values[band_idx] > UPWELLING_THRESHOLD)
        .count();

    let data_date = format!(
        "{}-{:02}-{:02}",
        latest_cuti.year, latest_cuti.month, latest_cuti.day
    );
    let today = now_pacific().format("%Y-%m-%d").to_string();
    let latency_note = if data_date != today {
        format!(
            " (data through {}, ~{}d lag)",
            data_date,
            latency_days(&data_date)
        )
    } else {
        String::new()
    };

    let regime = classify_regime(today_cuti, rolling_5d_cuti, days_above);

    let mut out = format!(
        "# Coastal upwelling indices — {}{}\n\n\
         **Regime**: {}\n\n\
         ## Today ({})\n\n\
         | Index | Value | Climatology (1988–2018) | Anomaly | Z-score |\n\
         |---|---|---|---|---|\n\
         | CUTI | {:+.3} m²/s | {:+.3} m²/s | {:+.3} | {:.2}σ |\n\
         | BEUTI | {:+.3} mmol·s⁻¹·m⁻¹ | {:+.3} | {:+.3} | — |\n\n",
        lat_band,
        latency_note,
        regime,
        data_date,
        today_cuti,
        clim_cuti.mean,
        anomaly_cuti,
        z_cuti,
        today_beuti,
        clim_beuti.mean,
        anomaly_beuti,
    );

    out.push_str(&format!(
        "## Rolling averages\n\n\
         - **5-day CUTI**: {:+.3} m²/s\n\
         - **5-day BEUTI**: {:+.3} mmol·s⁻¹·m⁻¹\n\
         - **Days CUTI > {:.1} in last 30d**: {} / 30 ({:.0}%)\n\n",
        rolling_5d_cuti,
        rolling_5d_beuti,
        UPWELLING_THRESHOLD,
        days_above,
        days_above as f64 / 30.0 * 100.0,
    ));

    out.push_str(&format!(
        "## Recent {} days\n\n\
         | Date | CUTI | BEUTI |\n\
         |---|---|---|\n",
        recent_cuti.len()
    ));
    for (i, row) in recent_cuti.iter().enumerate() {
        let beuti_val = recent_beuti
            .get(i)
            .map(|r| r.values[band_idx])
            .unwrap_or(f64::NAN);
        out.push_str(&format!(
            "| {}-{:02}-{:02} | {:+.3} | {:+.3} |\n",
            row.year, row.month, row.day, row.values[band_idx], beuti_val,
        ));
    }

    out.push_str(&format!(
        "\n_Positive CUTI = upwelling-favorable (Ekman transport offshore). \
         Positive BEUTI = nutrient flux into euphotic zone. \
         Climatology: Jacox et al. 2018 (DOI:10.1029/2018JC014187), 1988–2018 day-of-year mean at {}. \
         Source: mjacox.com, updated daily ~18:00 UTC._\n",
        lat_band,
    ));

    Ok(out)
}

fn classify_regime(today_cuti: f64, rolling_5d: f64, days_above_30d: usize) -> &'static str {
    if rolling_5d > 1.5 && days_above_30d > 20 {
        "**Strong persistent upwelling** — sustained favorable forcing"
    } else if rolling_5d > 1.0 || days_above_30d > 15 {
        "**Active upwelling** — favorable regime with intermittent relaxation"
    } else if today_cuti > 0.3 || rolling_5d > 0.3 {
        "**Transitional / weak upwelling** — some favorable forcing"
    } else if today_cuti > -0.3 {
        "**Neutral** — minimal cross-shore transport"
    } else {
        "**Downwelling-favorable** — onshore Ekman transport"
    }
}

fn latency_days(data_date: &str) -> i64 {
    let today = now_pacific().date_naive();
    chrono::NaiveDate::parse_from_str(data_date, "%Y-%m-%d")
        .map(|d| (today - d).num_days())
        .unwrap_or(0)
}

/// Fetch and cache the parsed CUTI/BEUTI archive, returning the cached
/// `Arc<IndexData>` directly to avoid a deep clone of the ~2 MB archive on
/// cache hits.
async fn fetch_index_arc(
    http: &reqwest::Client,
    cache: &Arc<CacheStore>,
    name: &str,
    url: &str,
) -> Result<Arc<IndexData>> {
    let cache_key = format!("ocean:{}:csv", name);
    if let Some(arc) = cache.get_arc::<IndexData>(&cache_key).await {
        return Ok(arc);
    }
    let body = http
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let data = Arc::new(parse_index_csv(&body)?);
    cache
        .set_arc(
            &cache_key,
            Arc::clone(&data),
            std::time::Duration::from_secs(21600),
        )
        .await;
    Ok(data)
}

/// Return the cached climatology if present, otherwise compute it (cloning the
/// index data only on this miss path) and cache it.
async fn get_or_compute_climatology(
    cache: &Arc<CacheStore>,
    cache_key: &str,
    data: &Arc<IndexData>,
) -> Arc<Climatology> {
    if let Some(arc) = cache.get_arc::<Climatology>(cache_key).await {
        return arc;
    }
    let clim = Arc::new(compute_climatology(data));
    cache
        .set_arc(
            cache_key,
            Arc::clone(&clim),
            std::time::Duration::from_secs(86400 * 365),
        )
        .await;
    clim
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_CSV: &str = "\
year,month,day,31N,32N,33N,34N,35N,36N,37N,38N,39N,40N,41N,42N,43N,44N,45N,46N,47N
1988,1,1,-0.432,0.073,-0.011,-0.37,-0.316,-0.083,-0.316,-0.346,-0.183,0.21,0.165,-0.195,-0.058,-0.186,0.263,0.154,0.114
1988,1,2,-0.248,-0.005,0.209,-0.371,-0.266,-0.28,-0.765,-0.825,-0.712,-0.977,-0.933,-0.789,-0.84,-0.659,0.035,0.057,0.011
2026,4,28,1.444,0.699,0.987,1.235,0.825,0.664,0.799,0.698,0.999,1.08,0.736,0.982,1.058,0.642,0.547,0.291,-0.136";

    const CUTI_HEAD_FIXTURE: &str = include_str!("fixtures/cuti_head.csv");

    #[test]
    fn parse_csv() {
        let data = parse_index_csv(SAMPLE_CSV).unwrap();
        assert_eq!(data.rows.len(), 3);
        assert_eq!(data.rows[0].year, 1988);
        assert_eq!(data.rows[0].month, 1);
        assert_eq!(data.rows[0].day, 1);
        let band_37n = lat_band_index("37N").unwrap();
        assert!((data.rows[0].values[band_37n] - (-0.316)).abs() < 0.001);
        assert!((data.rows[2].values[band_37n] - 0.799).abs() < 0.001);
    }

    #[test]
    fn parse_cuti_head_fixture() {
        // Real captured header + rows: 17 bands (31N..47N) intact.
        let data = parse_index_csv(CUTI_HEAD_FIXTURE).unwrap();
        assert!(data.rows.len() >= 10, "fixture should have ~10 rows");
        for row in &data.rows {
            assert_eq!(
                row.values.len(),
                LAT_BANDS.len(),
                "each row must hold 17 bands"
            );
        }
        let band_37n = lat_band_index("37N").unwrap();
        assert!(data.rows[0].values[band_37n].is_finite());
    }

    #[test]
    fn parse_bails_on_wrong_band_count() {
        // Header truncated to fewer than the 17 bands → cols.len() < 20 → bail.
        let csv = "year,month,day,31N,32N,33N\n2026,1,1,0.1,0.2,0.3";
        let e = parse_index_csv(csv).err().unwrap().to_string();
        assert!(e.contains("unexpected CSV header"), "unexpected: {e}");
    }

    #[test]
    fn parse_bails_on_empty_file() {
        let e = parse_index_csv("").err().unwrap().to_string();
        assert!(e.contains("unexpected CSV header"), "unexpected: {e}");
    }

    #[test]
    fn parse_bails_when_only_header_no_rows() {
        let header =
            "year,month,day,31N,32N,33N,34N,35N,36N,37N,38N,39N,40N,41N,42N,43N,44N,45N,46N,47N";
        let e = parse_index_csv(header).err().unwrap().to_string();
        assert!(e.contains("no data rows"), "unexpected: {e}");
    }

    #[test]
    fn parse_row_non_numeric_becomes_nan() {
        let header =
            "year,month,day,31N,32N,33N,34N,35N,36N,37N,38N,39N,40N,41N,42N,43N,44N,45N,46N,47N";
        // 37N (band index 6, field index 9) is garbage text → NaN, others fine.
        let row = "2026,1,1,0.1,0.2,0.3,0.4,0.5,0.6,oops,0.8,0.9,1.0,1.1,1.2,1.3,1.4,1.5,1.6,1.7";
        let data = parse_index_csv(&format!("{header}\n{row}")).unwrap();
        assert_eq!(data.rows.len(), 1);
        let band_37n = lat_band_index("37N").unwrap();
        assert!(data.rows[0].values[band_37n].is_nan(), "non-numeric → NaN");
        assert_eq!(data.rows[0].values.len(), LAT_BANDS.len());
    }

    #[test]
    fn parse_index_csv_never_emits_row_with_wrong_value_count() {
        // Invariant guarding values[band_idx] indexing elsewhere: every emitted
        // row holds exactly 17 values, regardless of trailing extra columns or
        // short rows (short rows are dropped, not emitted with a wrong width).
        let header =
            "year,month,day,31N,32N,33N,34N,35N,36N,37N,38N,39N,40N,41N,42N,43N,44N,45N,46N,47N";
        let normal = "2026,1,1,0.1,0.2,0.3,0.4,0.5,0.6,0.7,0.8,0.9,1.0,1.1,1.2,1.3,1.4,1.5,1.6,1.7";
        // 23 fields (extra trailing columns beyond the 20-wide schema).
        let extra = "2026,1,2,0.1,0.2,0.3,0.4,0.5,0.6,0.7,0.8,0.9,1.0,1.1,1.2,1.3,1.4,1.5,1.6,1.7,9.9,8.8,7.7";
        // Too few fields — must be skipped entirely.
        let short = "2026,1,3,0.1,0.2";
        let csv = format!("{header}\n{normal}\n{extra}\n{short}");
        let data = parse_index_csv(&csv).unwrap();
        assert_eq!(data.rows.len(), 2, "short row must be dropped");
        for row in &data.rows {
            assert_eq!(
                row.values.len(),
                LAT_BANDS.len(),
                "invariant: exactly 17 values per row"
            );
        }
    }

    #[test]
    fn day_of_year_works() {
        assert_eq!(day_of_year(1, 1), 0);
        assert_eq!(day_of_year(2, 1), 31);
        assert_eq!(day_of_year(12, 31), 365);
    }

    #[test]
    fn lat_band_index_works() {
        assert_eq!(lat_band_index("31N"), Some(0));
        assert_eq!(lat_band_index("37N"), Some(6));
        assert_eq!(lat_band_index("47N"), Some(16));
        assert_eq!(lat_band_index("99N"), None);
    }

    #[test]
    fn classify_regime_works() {
        assert!(classify_regime(2.0, 2.0, 25).contains("Strong"));
        assert!(classify_regime(1.2, 1.2, 18).contains("Active"));
        assert!(classify_regime(0.5, 0.5, 5).contains("Transitional"));
        assert!(classify_regime(0.0, 0.0, 0).contains("Neutral"));
        assert!(classify_regime(-1.0, -1.0, 0).contains("Downwelling"));
    }
}
