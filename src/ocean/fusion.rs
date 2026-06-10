use std::sync::Arc;

use anyhow::{Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::erddap_client::ErddapClient;
use super::types::*;
use super::{upwelling, m1, wharf, charm, sst, hfr};
use super::{UpwellingRequest, M1Request, WharfRequest, HabRequest, SstRequest, HfrRequest};
use crate::biodiversity::{BiodiversityService, Observation};
use crate::cache::CacheStore;
use crate::util::{degrees_to_compass, now_pacific};

// ─── Request types ───

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpwellingStateRequest {
    /// Output format: "narrative" (default) or "json".
    pub format: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HabRiskRequest {}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ResearchSnapshotRequest {
    /// If true, fail if any data source is unavailable instead of returning
    /// partial results. Default: false.
    pub strict: Option<bool>,
}

// ─── Full snapshot (all sources, all Option) ───

struct FullSnapshot {
    upwelling: Option<UpwellingSnapshot>,
    m1: Option<M1Snapshot>,
    wharf: Option<WharfSnapshot>,
    hab: Option<HabSnapshot>,
    sst_snap: Option<SstSnapshot>,
    hfr_snap: Option<HfrSnapshot>,
    birds: Option<BirdSummary>,
    inat: Vec<InatSummary>,
    partial_failures: Vec<PartialFailure>,
}

// ─── Research snapshot (JSON output) ───

#[derive(Serialize)]
pub struct ResearchSnapshot {
    pub retrieved_utc: String,
    pub upwelling: Option<UpwellingSnapshot>,
    pub m1: Option<M1Snapshot>,
    pub wharf: Option<WharfSnapshot>,
    pub hab: Option<HabSnapshot>,
    pub sst: Option<SstSnapshot>,
    pub hfr: Option<HfrSnapshot>,
    pub birds: Option<BirdSummary>,
    pub inat: Vec<InatSummary>,
    pub deterministic_checksum: String,
    pub partial_failures: Vec<PartialFailure>,
}

// ─── Helpers ───

fn capture<T>(r: Result<T>, source: &str, fails: &mut Vec<PartialFailure>) -> Option<T> {
    match r {
        Ok(v) => Some(v),
        Err(e) => {
            fails.push(PartialFailure {
                source: source.to_string(),
                error: e.to_string(),
            });
            None
        }
    }
}

// ─── Core: collect_snapshot ───

async fn collect_snapshot(
    http: &reqwest::Client,
    cache: &Arc<CacheStore>,
    erddap: &ErddapClient,
    bio: &Arc<BiodiversityService>,
) -> FullSnapshot {
    let upw_req = UpwellingRequest { lat_band: Some("37N".into()), days_back: Some(7) };
    let m1_req = M1Request { hours: Some(72), include_profile: Some(true) };
    let wharf_req = WharfRequest { hours: Some(6) };
    let hab_req = HabRequest { lat: None, lon: None, snap_radius: None };
    let sst_req = SstRequest { lat_min: None, lat_max: None, lon_min: None, lon_max: None, stride: None };
    let hfr_req = HfrRequest { resolution: Some("6km".into()), lat_min: None, lat_max: None, lon_min: None, lon_max: None };

    // Fan out all fetchers concurrently.
    // upwelling takes (http, cache, req); all erddap-based take (erddap, req).
    let (r_upw, r_m1, r_wharf, r_hab, r_sst, r_hfr, r_birds, r_inat_anchovy, r_inat_krill) = tokio::join!(
        upwelling::fetch_typed(http, cache, &upw_req),
        m1::fetch_typed(erddap, &m1_req),
        wharf::fetch_typed(erddap, &wharf_req),
        charm::fetch_typed(erddap, &hab_req),
        sst::fetch_typed(erddap, &sst_req),
        hfr::fetch_typed(erddap, &hfr_req),
        bio.fetch_birds_typed(36.85, -122.05, 25.0, 7, 200),
        bio.fetch_species_typed(Some("anchovy stranding"), 36.85, -122.05, 25.0, 14, 20),
        bio.fetch_species_typed(Some("krill washup"), 36.85, -122.05, 25.0, 14, 20),
    );

    let mut fails = Vec::new();

    let upwelling = capture(r_upw, "upwelling (CUTI/BEUTI)", &mut fails);
    let m1 = capture(r_m1, "M1 mooring", &mut fails);
    let wharf = capture(r_wharf, "SC Wharf", &mut fails);
    let hab = capture(r_hab, "C-HARM HAB", &mut fails);
    let sst_snap = capture(r_sst, "MUR SST", &mut fails);
    let hfr_snap = capture(r_hfr, "HF Radar", &mut fails);
    let birds_raw = capture(r_birds, "eBird", &mut fails);
    let inat_anchovy = capture(r_inat_anchovy, "iNat anchovy", &mut fails);
    let inat_krill = capture(r_inat_krill, "iNat krill", &mut fails);

    let birds = birds_raw.map(|obs| summarize_birds(&obs));

    let mut inat = Vec::new();
    if let Some(obs) = inat_anchovy {
        inat.push(summarize_inat("anchovy stranding", &obs));
    }
    if let Some(obs) = inat_krill {
        inat.push(summarize_inat("krill washup", &obs));
    }

    FullSnapshot {
        upwelling,
        m1,
        wharf,
        hab,
        sst_snap,
        hfr_snap,
        birds,
        inat,
        partial_failures: fails,
    }
}

// ─── Bird / iNat summarizers ───

const UPWELLING_INDICATOR_SPECIES: &[&str] = &[
    "Sooty Shearwater",
    "Common Murre",
    "Sabine's Gull",
    "Cassin's Auklet",
];

fn summarize_birds(obs: &[Observation]) -> BirdSummary {
    let mut species_counts: Vec<SpeciesCount> = Vec::new();

    for indicator in UPWELLING_INDICATOR_SPECIES {
        let count: u32 = obs
            .iter()
            .filter(|o| {
                o.common_name
                    .as_deref()
                    .is_some_and(|name| name.contains(indicator))
            })
            .map(|o| o.count.unwrap_or(1))
            .sum();
        if count > 0 {
            species_counts.push(SpeciesCount {
                common_name: indicator.to_string(),
                count,
            });
        }
    }

    let total: usize = species_counts.iter().map(|s| s.count as usize).sum();

    BirdSummary {
        species: species_counts,
        total_observations: total,
    }
}

fn summarize_inat(query: &str, obs: &[Observation]) -> InatSummary {
    let notable: Vec<String> = obs
        .iter()
        .take(5)
        .map(|o| {
            let name = o.common_name.as_deref().unwrap_or("unknown");
            let date = o.observed_on.as_deref().unwrap_or("?");
            format!("{} ({})", name, date)
        })
        .collect();

    InatSummary {
        query: query.to_string(),
        total_observations: obs.len(),
        notable,
    }
}

// ─── Tool A: render_narrative ───

fn render_narrative(snap: &FullSnapshot) -> String {
    let mut out = String::with_capacity(4096);
    let now = now_pacific();

    out.push_str("# Monterey Bay Upwelling State\n\n");

    // 1. Regime + CUTI/BEUTI
    if let Some(ref u) = snap.upwelling {
        out.push_str(&format!(
            "**Upwelling regime** ({}): {} CUTI = {:+.3} m\u{00b2}/s (anomaly {:+.3}, z = {:.2}\u{03c3}), \
             BEUTI = {:+.3} mmol\u{00b7}s\u{207b}\u{00b9}\u{00b7}m\u{207b}\u{00b9}. \
             5-day rolling CUTI {:+.3}, BEUTI {:+.3}. \
             {} of last 30 days upwelling-favorable.\n\n",
            u.data_date, u.regime,
            u.today_cuti, u.anomaly_cuti, u.z_cuti,
            u.today_beuti,
            u.rolling_5d_cuti, u.rolling_5d_beuti,
            u.days_above_threshold_30d,
        ));
    }

    // 2. M1 winds + stratification
    if let Some(ref m) = snap.m1 {
        out.push_str(&format!(
            "**M1 mooring** ({}): surface temp {}, ",
            m.timestamp_utc,
            m.surface_temp_c.map(|v| format!("{:.1}\u{00b0}C", v)).unwrap_or_else(|| "N/A".to_string()),
        ));
        if let (Some(spd), Some(dir)) = (m.wind_speed_ms, m.wind_dir_from_deg) {
            let compass = degrees_to_compass(dir);
            out.push_str(&format!(
                "wind {:.1} m/s from {} ({:.0}\u{00b0})",
                spd, compass, dir,
            ));
            if let Some(eq) = m.equatorward_wind_ms {
                out.push_str(&format!(", equatorward component {:.1} m/s", eq));
            }
        } else {
            out.push_str("wind N/A");
        }
        if let Some(strat) = m.stratification_index {
            out.push_str(&format!(
                ". Stratification index {:.2}\u{00b0}C{}",
                strat,
                if strat > 3.0 { " (strongly stratified)" }
                else if strat > 1.5 { " (moderately stratified)" }
                else if strat > 0.5 { " (weakly stratified, active mixing)" }
                else { " (well-mixed)" },
            ));
        }
        out.push_str(".\n\n");
    }

    // 3. MUR SST
    if let Some(ref s) = snap.sst_snap {
        out.push_str(&format!(
            "**MUR SST** ({}): mean {:.2}\u{00b0}C (range {:.2}\u{2013}{:.2})",
            s.timestamp_utc, s.mean_sst_c, s.min_sst_c, s.max_sst_c,
        ));
        if let Some(anom) = s.mean_anom_c {
            out.push_str(&format!(", anomaly {:+.2}\u{00b0}C", anom));
        }
        if let Some(grad) = s.max_grad_c_per_km {
            out.push_str(&format!(
                ", max gradient {:.3}\u{00b0}C/km{}",
                grad,
                if grad > 0.3 { " (frontal zone)" } else { "" },
            ));
        }
        out.push_str(&format!(" ({} cells).\n\n", s.n_cells));
    }

    // 4. HFR currents
    if let Some(ref h) = snap.hfr_snap {
        let compass = degrees_to_compass(h.flow_direction_deg);
        out.push_str(&format!(
            "**HF Radar** ({}, {}): mean {:.3} m/s toward {} ({:.0}\u{00b0}), \
             max {:.3} m/s, coverage {}/{} cells",
            h.timestamp_utc, h.resolution,
            h.mean_speed_ms, compass, h.flow_direction_deg,
            h.max_speed_ms,
            h.n_cells_valid, h.n_cells_total,
        ));
        if let Some(div) = h.divergence_per_s {
            out.push_str(&format!(
                ", divergence {:.2e} s\u{207b}\u{00b9}{}",
                div,
                if div > 1e-6 { " (upwelling-consistent)" }
                else if div < -1e-6 { " (convergence)" }
                else { "" },
            ));
        }
        out.push_str(".\n\n");
    }

    // 5. C-HARM HAB
    if let Some(ref h) = snap.hab {
        out.push_str("**C-HARM HAB forecast**: ");
        let parts: Vec<String> = h.forecasts.iter().enumerate().map(|(i, f)| {
            let label = match i {
                0 => "nowcast",
                1 => "+1d",
                2 => "+2d",
                3 => "+3d",
                _ => "?",
            };
            let pn = f.p_pseudo_nitzschia.map(|v| format!("{:.0}%", v * 100.0)).unwrap_or_else(|| "N/A".to_string());
            let chl = f.chla_filled.map(|v| format!("{:.1} mg/m\u{00b3}", v)).unwrap_or_else(|| "N/A".to_string());
            format!("{} P(PN) {} [{}] chl-a {}", label, pn, f.risk_class, chl)
        }).collect();
        out.push_str(&parts.join("; "));
        out.push_str(".\n\n");
    }

    // 6. SC Wharf in-situ
    if let Some(ref w) = snap.wharf {
        out.push_str(&format!(
            "**SC Wharf** ({}): ",
            w.timestamp_utc,
        ));
        let mut fields = Vec::new();
        if let Some(t) = w.temp_c { fields.push(format!("temp {:.1}\u{00b0}C", t)); }
        if let Some(c) = w.chla_mg_m3 { fields.push(format!("chl-a {:.1} mg/m\u{00b3}", c)); }
        if let Some(p) = w.ph { fields.push(format!("pH {:.2}", p)); }
        if let Some(d) = w.do_mg_l { fields.push(format!("DO {:.1} mg/L", d)); }
        if let Some(s) = w.salinity_psu { fields.push(format!("salinity {:.2} PSU", s)); }
        out.push_str(&fields.join(", "));
        out.push_str(".\n\n");
    }

    // 7. Bird indicators
    if let Some(ref b) = snap.birds {
        if b.total_observations > 0 {
            out.push_str("**Upwelling bird indicators** (eBird, 7d): ");
            let parts: Vec<String> = b.species.iter().map(|s| format!("{} ({})", s.common_name, s.count)).collect();
            out.push_str(&parts.join(", "));
            out.push_str(&format!(". Total: {}.\n\n", b.total_observations));
        } else {
            out.push_str("**Upwelling bird indicators**: no indicator species reported in last 7 days.\n\n");
        }
    }

    // 8. iNat strandings
    if !snap.inat.is_empty() {
        out.push_str("**iNaturalist strandings** (14d): ");
        let parts: Vec<String> = snap.inat.iter().map(|s| {
            if s.total_observations > 0 {
                format!("{}: {} obs", s.query, s.total_observations)
            } else {
                format!("{}: none", s.query)
            }
        }).collect();
        out.push_str(&parts.join("; "));
        out.push_str(".\n\n");
    }

    // 9. Partial failure notes
    if !snap.partial_failures.is_empty() {
        out.push_str("**Partial failures**: ");
        let parts: Vec<String> = snap.partial_failures.iter().map(|f| format!("{}: {}", f.source, f.error)).collect();
        out.push_str(&parts.join("; "));
        out.push_str(".\n\n");
    }

    // 10. Timestamp
    out.push_str(&format!(
        "_Retrieved: {}._\n",
        now.format("%Y-%m-%d %-I:%M %p %Z"),
    ));

    out
}

// ─── Tool B: render_hab_narrative ───

fn render_hab_narrative(snap: &FullSnapshot) -> String {
    let mut out = String::with_capacity(2048);
    let now = now_pacific();

    out.push_str("# Monterey Bay HAB Risk Summary\n\n");

    // C-HARM forecast table
    if let Some(ref h) = snap.hab {
        out.push_str(&format!(
            "_Nearest valid cell to ({:.2}\u{00b0}N, {:.2}\u{00b0}W)_\n\n",
            h.query_lat, h.query_lon.abs(),
        ));
        out.push_str("| Horizon | P(PN) | Risk | Chl-a |\n");
        out.push_str("|---|---|---|---|\n");

        for (i, f) in h.forecasts.iter().enumerate() {
            let label = match i {
                0 => "Nowcast",
                1 => "+1 day",
                2 => "+2 day",
                3 => "+3 day",
                _ => "--",
            };
            let pn = f.p_pseudo_nitzschia.map(|v| format!("{:.2}", v)).unwrap_or_else(|| "--".to_string());
            let chl = f.chla_filled.map(|v| format!("{:.1} mg/m\u{00b3}", v)).unwrap_or_else(|| "--".to_string());
            out.push_str(&format!("| {} | {} | {} | {} |\n", label, pn, f.risk_class, chl));
        }
        out.push('\n');
    } else {
        out.push_str("C-HARM forecast unavailable.\n\n");
    }

    // SC Wharf ground-truth
    if let Some(ref w) = snap.wharf {
        out.push_str("## SC Wharf ground-truth\n\n");
        if let Some(c) = w.chla_mg_m3 {
            let note = if c > 10.0 { " (bloom-level)" } else if c > 5.0 { " (elevated)" } else { "" };
            out.push_str(&format!("- Chl-a: {:.1} mg/m\u{00b3}{}\n", c, note));
        }
        if let Some(p) = w.ph {
            let note = if p < 7.8 { " (depressed, upwelling-driven CO\u{2082})" } else { "" };
            out.push_str(&format!("- pH: {:.2}{}\n", p, note));
        }
        if let Some(t) = w.temp_c {
            out.push_str(&format!("- Temp: {:.1}\u{00b0}C\n", t));
        }
        out.push('\n');
    }

    // SST context
    if let Some(ref s) = snap.sst_snap {
        out.push_str("## SST context\n\n");
        out.push_str(&format!("- Mean SST: {:.2}\u{00b0}C\n", s.mean_sst_c));
        if let Some(anom) = s.mean_anom_c {
            out.push_str(&format!("- Anomaly: {:+.2}\u{00b0}C\n", anom));
        }
        out.push('\n');
    }

    // Bird indicators
    if let Some(ref b) = snap.birds
        && b.total_observations > 0 {
            out.push_str("## Seabird indicators\n\n");
            for s in &b.species {
                out.push_str(&format!("- {}: {}\n", s.common_name, s.count));
            }
            out.push('\n');
        }

    // Partial failures
    if !snap.partial_failures.is_empty() {
        out.push_str("## Data gaps\n\n");
        for f in &snap.partial_failures {
            out.push_str(&format!("- {}: {}\n", f.source, f.error));
        }
        out.push('\n');
    }

    out.push_str(&format!(
        "_Retrieved: {}._\n",
        now.format("%Y-%m-%d %-I:%M %p %Z"),
    ));

    out
}

// ─── Tool C: build_research_snapshot ───

/// Collect all finite f64 values from the snapshot in a deterministic order.
/// Order: upwelling -> m1 -> wharf -> hab forecasts -> sst -> hfr -> bird counts.
/// When a source is None, its entire block is skipped (no sentinels inserted).
fn collect_f64s(snap: &FullSnapshot) -> Vec<f64> {
    let mut vals = Vec::new();

    if let Some(ref u) = snap.upwelling {
        for v in [u.today_cuti, u.today_beuti, u.climatology_cuti, u.climatology_beuti,
                  u.anomaly_cuti, u.z_cuti, u.rolling_5d_cuti, u.rolling_5d_beuti] {
            if v.is_finite() { vals.push(v); }
        }
    }

    if let Some(ref m) = snap.m1 {
        if let Some(v) = m.surface_temp_c && v.is_finite() { vals.push(v); }
        if let Some(v) = m.wind_speed_ms && v.is_finite() { vals.push(v); }
        if let Some(v) = m.wind_dir_from_deg && v.is_finite() { vals.push(v); }
        if let Some(v) = m.equatorward_wind_ms && v.is_finite() { vals.push(v); }
        if let Some(v) = m.stratification_index && v.is_finite() { vals.push(v); }
        // Skip m.latency_hours — it's wall-clock-derived (now - timestamp),
        // so including it would break checksum determinism across calls with
        // the same underlying data. Wharf's latency_minutes (i64) is also
        // excluded by type.
        for p in &m.profile {
            if p.z_m.is_finite() { vals.push(p.z_m); }
            if p.temp_c.is_finite() { vals.push(p.temp_c); }
        }
    }

    if let Some(ref w) = snap.wharf {
        if let Some(v) = w.temp_c && v.is_finite() { vals.push(v); }
        if let Some(v) = w.salinity_psu && v.is_finite() { vals.push(v); }
        if let Some(v) = w.ph && v.is_finite() { vals.push(v); }
        if let Some(v) = w.chla_mg_m3 && v.is_finite() { vals.push(v); }
        if let Some(v) = w.do_mg_l && v.is_finite() { vals.push(v); }
        if let Some(v) = w.do_saturation_pct && v.is_finite() { vals.push(v); }
        if let Some(v) = w.turbidity_ntu && v.is_finite() { vals.push(v); }
    }

    if let Some(ref h) = snap.hab {
        for f in &h.forecasts {
            if let Some(v) = f.p_pseudo_nitzschia && v.is_finite() { vals.push(v); }
            if let Some(v) = f.p_particulate_domoic && v.is_finite() { vals.push(v); }
            if let Some(v) = f.p_cellular_domoic && v.is_finite() { vals.push(v); }
            if let Some(v) = f.chla_filled && v.is_finite() { vals.push(v); }
        }
    }

    if let Some(ref s) = snap.sst_snap {
        for v in [s.mean_sst_c, s.min_sst_c, s.max_sst_c] {
            if v.is_finite() { vals.push(v); }
        }
        if let Some(v) = s.mean_anom_c && v.is_finite() { vals.push(v); }
        if let Some(v) = s.max_grad_c_per_km && v.is_finite() { vals.push(v); }
    }

    if let Some(ref h) = snap.hfr_snap {
        for v in [h.mean_speed_ms, h.max_speed_ms, h.mean_u_ms, h.mean_v_ms, h.flow_direction_deg] {
            if v.is_finite() { vals.push(v); }
        }
        if let Some(v) = h.divergence_per_s && v.is_finite() { vals.push(v); }
    }

    if let Some(ref b) = snap.birds {
        for s in &b.species {
            vals.push(s.count as f64);
        }
    }

    vals
}

fn compute_checksum(vals: &[f64]) -> String {
    let payload: String = vals
        .iter()
        .map(|v| format!("{:.6}", v))
        .collect::<Vec<_>>()
        .join(",");
    let hash = Sha256::digest(payload.as_bytes());
    format!("sha256:{:x}", hash)
}

// ─── Public entry points (called from OceanService) ───

pub async fn upwelling_state(
    http: &reqwest::Client,
    cache: &Arc<CacheStore>,
    erddap: &ErddapClient,
    bio: &Arc<BiodiversityService>,
    req: &UpwellingStateRequest,
) -> Result<String> {
    let snap = collect_snapshot(http, cache, erddap, bio).await;
    let fmt = req.format.as_deref().unwrap_or("narrative");
    if fmt == "json" {
        let vals = collect_f64s(&snap);
        let checksum = compute_checksum(&vals);
        let rs = ResearchSnapshot {
            retrieved_utc: chrono::Utc::now().to_rfc3339(),
            upwelling: snap.upwelling,
            m1: snap.m1,
            wharf: snap.wharf,
            hab: snap.hab,
            sst: snap.sst_snap,
            hfr: snap.hfr_snap,
            birds: snap.birds,
            inat: snap.inat,
            deterministic_checksum: checksum,
            partial_failures: snap.partial_failures,
        };
        Ok(serde_json::to_string_pretty(&rs)?)
    } else {
        Ok(render_narrative(&snap))
    }
}

pub async fn hab_risk(
    http: &reqwest::Client,
    cache: &Arc<CacheStore>,
    erddap: &ErddapClient,
    bio: &Arc<BiodiversityService>,
    _req: &HabRiskRequest,
) -> Result<String> {
    let snap = collect_snapshot(http, cache, erddap, bio).await;
    Ok(render_hab_narrative(&snap))
}

pub async fn research_snapshot(
    http: &reqwest::Client,
    cache: &Arc<CacheStore>,
    erddap: &ErddapClient,
    bio: &Arc<BiodiversityService>,
    req: &ResearchSnapshotRequest,
) -> Result<String> {
    let snap = collect_snapshot(http, cache, erddap, bio).await;

    if req.strict.unwrap_or(false) && !snap.partial_failures.is_empty() {
        let sources: Vec<&str> = snap.partial_failures.iter().map(|f| f.source.as_str()).collect();
        bail!("strict mode: unavailable sources: {}", sources.join(", "));
    }

    let vals = collect_f64s(&snap);
    let checksum = compute_checksum(&vals);

    let rs = ResearchSnapshot {
        retrieved_utc: chrono::Utc::now().to_rfc3339(),
        upwelling: snap.upwelling,
        m1: snap.m1,
        wharf: snap.wharf,
        hab: snap.hab,
        sst: snap.sst_snap,
        hfr: snap.hfr_snap,
        birds: snap.birds,
        inat: snap.inat,
        deterministic_checksum: checksum,
        partial_failures: snap.partial_failures,
    };

    Ok(serde_json::to_string_pretty(&rs)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_deterministic() {
        let vals = vec![1.0, 2.5, 3.14159];
        let c1 = compute_checksum(&vals);
        let c2 = compute_checksum(&vals);
        assert_eq!(c1, c2);
        assert!(c1.starts_with("sha256:"));
    }

    #[test]
    fn checksum_changes_with_data() {
        let c1 = compute_checksum(&[1.0, 2.0]);
        let c2 = compute_checksum(&[1.0, 3.0]);
        assert_ne!(c1, c2);
    }

    #[test]
    fn summarize_birds_filters_indicators() {
        let obs = vec![
            Observation {
                common_name: Some("Sooty Shearwater".to_string()),
                scientific_name: None,
                observed_on: None,
                location: None,
                observer: None,
                url: None,
                iconic_taxon: None,
                count: Some(50),
            },
            Observation {
                common_name: Some("House Sparrow".to_string()),
                scientific_name: None,
                observed_on: None,
                location: None,
                observer: None,
                url: None,
                iconic_taxon: None,
                count: Some(10),
            },
            Observation {
                common_name: Some("Common Murre".to_string()),
                scientific_name: None,
                observed_on: None,
                location: None,
                observer: None,
                url: None,
                iconic_taxon: None,
                count: None,
            },
        ];
        let summary = summarize_birds(&obs);
        assert_eq!(summary.species.len(), 2);
        assert_eq!(summary.species[0].common_name, "Sooty Shearwater");
        assert_eq!(summary.species[0].count, 50);
        assert_eq!(summary.species[1].common_name, "Common Murre");
        assert_eq!(summary.species[1].count, 1); // None -> 1
        assert_eq!(summary.total_observations, 51);
    }

    #[test]
    fn summarize_inat_limits_notable() {
        let obs: Vec<Observation> = (0..10)
            .map(|i| Observation {
                common_name: Some(format!("Species {}", i)),
                scientific_name: None,
                observed_on: Some(format!("2026-04-{:02}", i + 1)),
                location: None,
                observer: None,
                url: None,
                iconic_taxon: None,
                count: None,
            })
            .collect();
        let summary = summarize_inat("test query", &obs);
        assert_eq!(summary.total_observations, 10);
        assert_eq!(summary.notable.len(), 5);
        assert_eq!(summary.query, "test query");
    }

}
