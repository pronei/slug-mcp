//! eBird API 2.0 endpoint fetchers + per-tool formatters.
//!
//! All requests authenticate with the `X-eBirdApiToken` header. The functions
//! here are intentionally thin — they build a URL, fetch, deserialize, and map
//! to the shared `Observation` (or `Hotspot`) shape. Caching, defaulting, and
//! dispatch live in `mod.rs` so the same fetchers can serve multiple tools.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::util::now_pacific;

use super::Observation;

const BASE: &str = "https://api.ebird.org/v2";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Hotspot {
    pub loc_id: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub country_code: Option<String>,
    pub subnational1_code: Option<String>,
    pub subnational2_code: Option<String>,
    pub latest_obs_dt: Option<String>,
    pub num_species_all_time: Option<u32>,
}

// ─── Shared response types ───

#[derive(Deserialize)]
struct EBirdObs {
    #[serde(rename = "speciesCode")]
    species_code: String,
    #[serde(rename = "comName")]
    com_name: String,
    #[serde(rename = "sciName")]
    sci_name: String,
    #[serde(rename = "locName")]
    loc_name: String,
    #[serde(rename = "obsDt")]
    obs_dt: String,
    #[serde(rename = "howMany")]
    how_many: Option<u32>,
}

#[derive(Deserialize)]
struct EBirdHotspot {
    #[serde(rename = "locId")]
    loc_id: String,
    #[serde(rename = "locName")]
    loc_name: String,
    lat: f64,
    lng: f64,
    #[serde(rename = "countryCode", default)]
    country_code: Option<String>,
    #[serde(rename = "subnational1Code", default)]
    subnational1_code: Option<String>,
    #[serde(rename = "subnational2Code", default)]
    subnational2_code: Option<String>,
    #[serde(rename = "latestObsDt", default)]
    latest_obs_dt: Option<String>,
    #[serde(rename = "numSpeciesAllTime", default)]
    num_species_all_time: Option<u32>,
}

// ─── HTTP helpers ───

async fn fetch_observations(http: &reqwest::Client, key: &str, url: &str) -> Result<Vec<Observation>> {
    let resp = http
        .get(url)
        .header("X-eBirdApiToken", key)
        .send()
        .await
        .context("eBird HTTP request failed")?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        anyhow::bail!("eBird returned 401 — check EBIRD_API_KEY");
    }
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!("eBird returned 404 — check region code or species code");
    }
    if !resp.status().is_success() {
        anyhow::bail!("eBird returned HTTP {}", resp.status());
    }
    let body: Vec<EBirdObs> = resp.json().await.context("parsing eBird JSON")?;
    Ok(body.into_iter().map(to_observation).collect())
}

fn to_observation(o: EBirdObs) -> Observation {
    Observation {
        common_name: Some(o.com_name),
        scientific_name: Some(o.sci_name),
        observed_on: Some(o.obs_dt),
        location: Some(o.loc_name),
        observer: None,
        url: Some(format!("https://ebird.org/species/{}", o.species_code)),
        iconic_taxon: Some("Aves".to_string()),
        count: o.how_many,
    }
}

// ─── Recent observations: geographic ───

#[allow(clippy::too_many_arguments)]
pub async fn fetch_recent_geo(
    http: &reqwest::Client,
    key: &str,
    lat: f64,
    lon: f64,
    radius_km: f64,
    days: u32,
    limit: u32,
) -> Result<Vec<Observation>> {
    let url = format!(
        "{BASE}/data/obs/geo/recent?lat={:.4}&lng={:.4}&dist={:.1}&back={}&maxResults={}",
        lat, lon, radius_km, days, limit
    );
    fetch_observations(http, key, &url).await
}

#[allow(clippy::too_many_arguments)]
pub async fn fetch_recent_geo_species(
    http: &reqwest::Client,
    key: &str,
    code: &str,
    lat: f64,
    lon: f64,
    radius_km: f64,
    days: u32,
    limit: u32,
) -> Result<Vec<Observation>> {
    let url = format!(
        "{BASE}/data/obs/geo/recent/{code}?lat={:.4}&lng={:.4}&dist={:.1}&back={}&maxResults={}",
        lat, lon, radius_km, days, limit
    );
    fetch_observations(http, key, &url).await
}

pub async fn fetch_recent_geo_notable(
    http: &reqwest::Client,
    key: &str,
    lat: f64,
    lon: f64,
    radius_km: f64,
    days: u32,
    limit: u32,
) -> Result<Vec<Observation>> {
    let url = format!(
        "{BASE}/data/obs/geo/recent/notable?lat={:.4}&lng={:.4}&dist={:.1}&back={}&maxResults={}",
        lat, lon, radius_km, days, limit
    );
    fetch_observations(http, key, &url).await
}

// ─── Recent observations: by region code ───

pub async fn fetch_recent_region(
    http: &reqwest::Client,
    key: &str,
    region: &str,
    days: u32,
    limit: u32,
) -> Result<Vec<Observation>> {
    let url = format!(
        "{BASE}/data/obs/{region}/recent?back={days}&maxResults={limit}"
    );
    fetch_observations(http, key, &url).await
}

pub async fn fetch_recent_region_species(
    http: &reqwest::Client,
    key: &str,
    region: &str,
    code: &str,
    days: u32,
    limit: u32,
) -> Result<Vec<Observation>> {
    let url = format!(
        "{BASE}/data/obs/{region}/recent/{code}?back={days}&maxResults={limit}"
    );
    fetch_observations(http, key, &url).await
}

pub async fn fetch_recent_region_notable(
    http: &reqwest::Client,
    key: &str,
    region: &str,
    days: u32,
    limit: u32,
) -> Result<Vec<Observation>> {
    let url = format!(
        "{BASE}/data/obs/{region}/recent/notable?back={days}&maxResults={limit}"
    );
    fetch_observations(http, key, &url).await
}

// ─── Historic observations: by date ───

#[allow(clippy::too_many_arguments)]
pub async fn fetch_historic_region(
    http: &reqwest::Client,
    key: &str,
    region: &str,
    year: u32,
    month: u32,
    day: u32,
    rank: &str,
    limit: u32,
) -> Result<Vec<Observation>> {
    let url = format!(
        "{BASE}/data/obs/{region}/historic/{year}/{month}/{day}?rank={rank}&maxResults={limit}"
    );
    fetch_observations(http, key, &url).await
}

// ─── Nearest sighting of a species ───

#[allow(clippy::too_many_arguments)]
pub async fn fetch_nearest_species(
    http: &reqwest::Client,
    key: &str,
    code: &str,
    lat: f64,
    lon: f64,
    radius_km: f64,
    days: u32,
    limit: u32,
) -> Result<Vec<Observation>> {
    let url = format!(
        "{BASE}/data/nearest/geo/recent/{code}?lat={:.4}&lng={:.4}&dist={:.1}&back={}&maxResults={}",
        lat, lon, radius_km, days, limit
    );
    fetch_observations(http, key, &url).await
}

// ─── Hotspots ───

pub async fn fetch_hotspots_region(
    http: &reqwest::Client,
    key: &str,
    region: &str,
    back: Option<u32>,
) -> Result<Vec<Hotspot>> {
    let mut url = format!("{BASE}/ref/hotspot/{region}?fmt=json");
    if let Some(b) = back {
        url.push_str(&format!("&back={b}"));
    }
    fetch_hotspots(http, key, &url).await
}

pub async fn fetch_hotspots_geo(
    http: &reqwest::Client,
    key: &str,
    lat: f64,
    lon: f64,
    radius_km: f64,
    back: Option<u32>,
) -> Result<Vec<Hotspot>> {
    let mut url = format!(
        "{BASE}/ref/hotspot/geo?lat={:.4}&lng={:.4}&dist={:.1}&fmt=json",
        lat, lon, radius_km
    );
    if let Some(b) = back {
        url.push_str(&format!("&back={b}"));
    }
    fetch_hotspots(http, key, &url).await
}

async fn fetch_hotspots(http: &reqwest::Client, key: &str, url: &str) -> Result<Vec<Hotspot>> {
    let resp = http
        .get(url)
        .header("X-eBirdApiToken", key)
        .send()
        .await
        .context("eBird hotspot HTTP request failed")?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        anyhow::bail!("eBird returned 401 — check EBIRD_API_KEY");
    }
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!("eBird returned 404 — check region code");
    }
    if !resp.status().is_success() {
        anyhow::bail!("eBird returned HTTP {}", resp.status());
    }
    let body: Vec<EBirdHotspot> = resp.json().await.context("parsing eBird hotspot JSON")?;
    Ok(body
        .into_iter()
        .map(|h| Hotspot {
            loc_id: h.loc_id,
            name: h.loc_name,
            lat: h.lat,
            lon: h.lng,
            country_code: h.country_code,
            subnational1_code: h.subnational1_code,
            subnational2_code: h.subnational2_code,
            latest_obs_dt: h.latest_obs_dt,
            num_species_all_time: h.num_species_all_time,
        })
        .collect())
}

// ─── Formatters ───

pub fn format_recent(
    obs: &[Observation],
    title_suffix: &str,
    empty_suffix: &str,
) -> String {
    if obs.is_empty() {
        return format!("No eBird observations found {empty_suffix}.\n");
    }
    let mut out = format!("# eBird recent observations — {title_suffix}\n\n");
    out.push_str(&format!("_{} observations._\n\n", obs.len()));
    push_obs_lines(&mut out, obs);
    out.push_str(&format!(
        "\n_Source: eBird API 2.0. Last updated: {}_\n",
        now_pacific().format("%-I:%M %p")
    ));
    out
}

pub fn format_historic(
    obs: &[Observation],
    region: &str,
    year: u32,
    month: u32,
    day: u32,
) -> String {
    let date = format!("{year:04}-{month:02}-{day:02}");
    if obs.is_empty() {
        return format!(
            "No eBird historic observations found for region `{region}` on {date}.\n"
        );
    }
    let mut out = format!(
        "# eBird historic observations — region `{region}` · {date}\n\n"
    );
    out.push_str(&format!("_{} species reported._\n\n", obs.len()));
    push_obs_lines(&mut out, obs);
    out.push_str(&format!(
        "\n_Source: eBird API 2.0 historic endpoint. Last updated: {}_\n",
        now_pacific().format("%-I:%M %p")
    ));
    out
}

pub fn format_nearest(obs: &[Observation], species_label: &str, lat: f64, lon: f64) -> String {
    if obs.is_empty() {
        return format!(
            "No recent eBird sightings of `{species_label}` within the search radius of ({:.3}, {:.3}).\n",
            lat, lon
        );
    }
    let mut out = format!(
        "# Nearest eBird sightings — `{species_label}` near ({:.3}, {:.3})\n\n",
        lat, lon
    );
    out.push_str(&format!("_{} sighting(s), nearest first._\n\n", obs.len()));
    push_obs_lines(&mut out, obs);
    out.push_str(&format!(
        "\n_Source: eBird API 2.0 nearest-species endpoint. Last updated: {}_\n",
        now_pacific().format("%-I:%M %p")
    ));
    out
}

pub fn format_hotspots(spots: &[Hotspot], title_suffix: &str, empty_suffix: &str) -> String {
    if spots.is_empty() {
        return format!("No eBird hotspots found {empty_suffix}.\n");
    }
    let mut out = format!("# eBird hotspots — {title_suffix}\n\n");
    out.push_str(&format!("_{} hotspot(s)._\n\n", spots.len()));
    for s in spots {
        out.push_str(&format!("- **{}** · `{}`", s.name, s.loc_id));
        if let Some(n) = s.num_species_all_time {
            out.push_str(&format!(" · {} species all-time", n));
        }
        if let Some(d) = &s.latest_obs_dt {
            out.push_str(&format!(" · last obs {}", d));
        }
        out.push_str(&format!(" · ({:.4}, {:.4})", s.lat, s.lon));
        out.push_str(&format!(
            " · [eBird](https://ebird.org/hotspot/{})\n",
            s.loc_id
        ));
    }
    out.push_str(&format!(
        "\n_Source: eBird API 2.0 hotspot reference. Last updated: {}_\n",
        now_pacific().format("%-I:%M %p")
    ));
    out
}

fn push_obs_lines(out: &mut String, obs: &[Observation]) {
    for o in obs {
        let name = match (&o.common_name, &o.scientific_name) {
            (Some(c), Some(s)) => format!("**{}** (_{}_)", c, s),
            (Some(c), None) => format!("**{}**", c),
            _ => "**unknown**".to_string(),
        };
        out.push_str(&format!("- {}", name));
        if let Some(n) = o.count {
            out.push_str(&format!(" · {} individual(s)", n));
        }
        if let Some(d) = &o.observed_on {
            out.push_str(&format!(" · {}", d));
        }
        if let Some(l) = &o.location {
            out.push_str(&format!(" · {}", l));
        }
        if let Some(u) = &o.url {
            out.push_str(&format!(" · [eBird]({})", u));
        }
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pelican_obs() -> Observation {
        Observation {
            common_name: Some("Brown Pelican".to_string()),
            scientific_name: Some("Pelecanus occidentalis".to_string()),
            observed_on: Some("2026-04-17 08:30".to_string()),
            location: Some("Natural Bridges State Beach".to_string()),
            observer: None,
            url: Some("https://ebird.org/species/brnpel".to_string()),
            iconic_taxon: Some("Aves".to_string()),
            count: Some(12),
        }
    }

    #[test]
    fn format_recent_renders_with_header() {
        let out = format_recent(&[pelican_obs()], "test", "in test");
        assert!(out.contains("Brown Pelican"));
        assert!(out.contains("12 individual(s)"));
        assert!(out.contains("Source: eBird API 2.0"));
    }

    #[test]
    fn format_recent_empty() {
        let out = format_recent(&[], "test", "near (36.97, -122.03)");
        assert!(out.contains("No eBird observations"));
        assert!(out.contains("near (36.97, -122.03)"));
    }

    #[test]
    fn format_historic_includes_date() {
        let out = format_historic(&[pelican_obs()], "US-CA-087", 2025, 4, 10);
        assert!(out.contains("2025-04-10"));
        assert!(out.contains("US-CA-087"));
        assert!(out.contains("Brown Pelican"));
    }

    #[test]
    fn format_nearest_includes_species() {
        let out = format_nearest(&[pelican_obs()], "Brown Pelican", 36.97, -122.03);
        assert!(out.contains("Brown Pelican"));
        assert!(out.contains("nearest first"));
    }

    #[test]
    fn format_hotspots_renders() {
        let spots = vec![Hotspot {
            loc_id: "L123456".to_string(),
            name: "Natural Bridges State Beach".to_string(),
            lat: 36.95,
            lon: -122.06,
            country_code: Some("US".to_string()),
            subnational1_code: Some("US-CA".to_string()),
            subnational2_code: Some("US-CA-087".to_string()),
            latest_obs_dt: Some("2026-04-17 09:00".to_string()),
            num_species_all_time: Some(213),
        }];
        let out = format_hotspots(&spots, "Santa Cruz County", "in Santa Cruz County");
        assert!(out.contains("Natural Bridges"));
        assert!(out.contains("L123456"));
        assert!(out.contains("213 species all-time"));
    }
}
