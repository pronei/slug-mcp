use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpwellingSnapshot {
    pub lat_band: String,
    pub data_date: String,
    pub today_cuti: f64,
    pub today_beuti: f64,
    pub climatology_cuti: f64,
    pub climatology_beuti: f64,
    pub anomaly_cuti: f64,
    pub z_cuti: f64,
    pub rolling_5d_cuti: f64,
    pub rolling_5d_beuti: f64,
    pub days_above_threshold_30d: usize,
    pub regime: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct M1Snapshot {
    pub timestamp_utc: String,
    pub latency_hours: f64,
    pub surface_temp_c: Option<f64>,
    pub wind_speed_ms: Option<f64>,
    pub wind_dir_from_deg: Option<f64>,
    pub equatorward_wind_ms: Option<f64>,
    pub profile: Vec<ProfileLevel>,
    pub stratification_index: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileLevel {
    pub z_m: f64,
    pub temp_c: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WharfSnapshot {
    pub timestamp_utc: String,
    pub latency_minutes: i64,
    pub temp_c: Option<f64>,
    pub salinity_psu: Option<f64>,
    pub ph: Option<f64>,
    pub chla_mg_m3: Option<f64>,
    pub do_mg_l: Option<f64>,
    pub do_saturation_pct: Option<f64>,
    pub turbidity_ntu: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HabDayForecast {
    pub dataset: String,
    pub date: String,
    pub p_pseudo_nitzschia: Option<f64>,
    pub p_particulate_domoic: Option<f64>,
    pub p_cellular_domoic: Option<f64>,
    pub chla_filled: Option<f64>,
    pub risk_class: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HabSnapshot {
    pub query_lat: f64,
    pub query_lon: f64,
    pub forecasts: Vec<HabDayForecast>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SstSnapshot {
    pub timestamp_utc: String,
    pub mean_sst_c: f64,
    pub min_sst_c: f64,
    pub max_sst_c: f64,
    pub mean_anom_c: Option<f64>,
    pub max_grad_c_per_km: Option<f64>,
    pub n_cells: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HfrSnapshot {
    pub timestamp_utc: String,
    pub resolution: String,
    pub mean_speed_ms: f64,
    pub max_speed_ms: f64,
    pub mean_u_ms: f64,
    pub mean_v_ms: f64,
    pub flow_direction_deg: f64,
    pub n_cells_valid: usize,
    pub n_cells_total: usize,
    pub divergence_per_s: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BirdSummary {
    pub species: Vec<SpeciesCount>,
    pub total_observations: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeciesCount {
    pub common_name: String,
    pub count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InatSummary {
    pub query: String,
    pub total_observations: usize,
    pub notable: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartialFailure {
    pub source: String,
    pub error: String,
}
