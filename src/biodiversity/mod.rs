//! Biodiversity observation tools — iNaturalist + eBird.
//!
//! Two distinct public APIs exposed as two tools:
//! - `search_species_observations` → iNaturalist v1 (no key)
//! - `search_bird_observations`    → eBird API 2.0 (requires `EBIRD_API_KEY`)
//!
//! Defaults to Santa Cruz (36.9741, -122.0308) with 25 km radius.
//!
//! iNaturalist: <https://api.inaturalist.org/v1/>
//! eBird: <https://documenter.getpostman.com/view/664302/S1ENwy59>

use std::sync::Arc;

use anyhow::{Context, Result};
use crate::util::now_pacific;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cache::CacheStore;

const DEFAULT_LAT: f64 = 36.9741;
const DEFAULT_LON: f64 = -122.0308;
const DEFAULT_RADIUS_KM: f64 = 25.0;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpeciesRequest {
    /// Free-text query (e.g. "banana slug", "kelp", "raptor"). Optional.
    pub query: Option<String>,
    /// Latitude (default 36.9741, Santa Cruz).
    pub lat: Option<f64>,
    /// Longitude (default -122.0308, Santa Cruz).
    pub lon: Option<f64>,
    /// Search radius in km (default 25, max 200).
    pub radius_km: Option<f64>,
    /// Days back to search (default 30).
    pub days: Option<u32>,
    /// Iconic taxon name filter: `Plantae`, `Animalia`, `Fungi`,
    /// `Mollusca`, `Aves`, `Mammalia`, `Reptilia`, etc. Optional.
    pub iconic_taxon: Option<String>,
    /// Max results (default 20, hard cap 100).
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BirdRequest {
    /// Latitude (default 36.9741, Santa Cruz).
    pub lat: Option<f64>,
    /// Longitude (default -122.0308, Santa Cruz).
    pub lon: Option<f64>,
    /// Search radius in km (default 25, max 50 per eBird limit).
    pub radius_km: Option<f64>,
    /// Days back to search (default 7, max 30).
    pub days: Option<u32>,
    /// Max results (default 25, hard cap 200).
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub common_name: Option<String>,
    pub scientific_name: Option<String>,
    pub observed_on: Option<String>,
    pub location: Option<String>,
    pub observer: Option<String>,
    pub url: Option<String>,
    pub iconic_taxon: Option<String>,
    pub count: Option<u32>,
}

pub struct BiodiversityService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
    ebird_key: Option<String>,
}

impl BiodiversityService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>, ebird_key: Option<String>) -> Self {
        Self {
            http,
            cache,
            ebird_key,
        }
    }

    pub async fn search_species(
        &self,
        query: Option<&str>,
        lat: Option<f64>,
        lon: Option<f64>,
        radius_km: Option<f64>,
        days: Option<u32>,
        iconic_taxon: Option<&str>,
        limit: Option<u32>,
    ) -> Result<String> {
        let lat = lat.unwrap_or(DEFAULT_LAT);
        let lon = lon.unwrap_or(DEFAULT_LON);
        let radius = radius_km.unwrap_or(DEFAULT_RADIUS_KM).min(200.0);
        let days = days.unwrap_or(30);
        let limit = limit.unwrap_or(20).min(100);
        let query = query.map(|s| s.to_string());
        let iconic = iconic_taxon.map(|s| s.to_string());

        let cache_key = format!(
            "bio:inat:{}:{:.3}:{:.3}:{:.1}:{}:{}:{}",
            query.as_deref().unwrap_or(""),
            lat,
            lon,
            radius,
            days,
            iconic.as_deref().unwrap_or(""),
            limit
        );
        let http = self.http.clone();
        let observations = self
            .cache
            .get_or_fetch::<Vec<Observation>, _, _>(&cache_key, 1800, move || async move {
                fetch_inaturalist(
                    &http,
                    query.as_deref(),
                    lat,
                    lon,
                    radius,
                    days,
                    iconic.as_deref(),
                    limit,
                )
                .await
            })
            .await?;

        Ok(format_species(&observations, lat, lon, radius, days))
    }

    pub async fn search_birds(
        &self,
        lat: Option<f64>,
        lon: Option<f64>,
        radius_km: Option<f64>,
        days: Option<u32>,
        limit: Option<u32>,
    ) -> Result<String> {
        let key = match &self.ebird_key {
            Some(k) if !k.is_empty() => k.clone(),
            _ => {
                return Ok(
                    "eBird API key not configured.\n\
                     Get a free key at https://ebird.org/api/keygen and set \
                     the `EBIRD_API_KEY` environment variable."
                        .to_string(),
                );
            }
        };

        let lat = lat.unwrap_or(DEFAULT_LAT);
        let lon = lon.unwrap_or(DEFAULT_LON);
        let radius = radius_km.unwrap_or(DEFAULT_RADIUS_KM).min(50.0);
        let days = days.unwrap_or(7).clamp(1, 30);
        let limit = limit.unwrap_or(25).min(200);

        let cache_key = format!(
            "bio:ebird:{:.3}:{:.3}:{:.1}:{}:{}",
            lat, lon, radius, days, limit
        );
        let http = self.http.clone();
        let observations = self
            .cache
            .get_or_fetch::<Vec<Observation>, _, _>(&cache_key, 1800, move || async move {
                fetch_ebird(&http, &key, lat, lon, radius, days, limit).await
            })
            .await?;

        Ok(format_birds(&observations, lat, lon, radius, days))
    }
}

// ─── iNaturalist ───

async fn fetch_inaturalist(
    http: &reqwest::Client,
    query: Option<&str>,
    lat: f64,
    lon: f64,
    radius_km: f64,
    days: u32,
    iconic: Option<&str>,
    limit: u32,
) -> Result<Vec<Observation>> {
    let mut params: Vec<(&str, String)> = vec![
        ("lat", format!("{}", lat)),
        ("lng", format!("{}", lon)),
        ("radius", format!("{}", radius_km)),
        ("per_page", format!("{}", limit)),
        ("order", "desc".to_string()),
        ("order_by", "observed_on".to_string()),
        ("d1", chrono::Utc::now()
            .checked_sub_signed(chrono::Duration::days(days as i64))
            .unwrap()
            .format("%Y-%m-%d")
            .to_string()),
    ];
    if let Some(q) = query {
        params.push(("q", q.to_string()));
    }
    if let Some(t) = iconic {
        params.push(("iconic_taxa", t.to_string()));
    }

    let resp = http
        .get("https://api.inaturalist.org/v1/observations")
        .query(&params)
        .send()
        .await
        .context("iNaturalist HTTP request failed")?;
    if !resp.status().is_success() {
        anyhow::bail!("iNaturalist returned HTTP {}", resp.status());
    }
    let body: InatResponse = resp.json().await.context("parsing iNaturalist JSON")?;

    Ok(body
        .results
        .into_iter()
        .map(|r| Observation {
            common_name: r
                .taxon
                .as_ref()
                .and_then(|t| t.preferred_common_name.clone()),
            scientific_name: r.taxon.as_ref().map(|t| t.name.clone()),
            observed_on: r.observed_on,
            location: r.place_guess,
            observer: r.user.map(|u| u.login),
            url: Some(r.uri),
            iconic_taxon: r.taxon.and_then(|t| t.iconic_taxon_name),
            count: None,
        })
        .collect())
}

#[derive(Deserialize)]
struct InatResponse {
    #[serde(default)]
    results: Vec<InatObservation>,
}
#[derive(Deserialize)]
struct InatObservation {
    uri: String,
    observed_on: Option<String>,
    place_guess: Option<String>,
    user: Option<InatUser>,
    taxon: Option<InatTaxon>,
}
#[derive(Deserialize)]
struct InatUser {
    login: String,
}
#[derive(Deserialize)]
struct InatTaxon {
    name: String,
    preferred_common_name: Option<String>,
    iconic_taxon_name: Option<String>,
}

// ─── eBird ───

async fn fetch_ebird(
    http: &reqwest::Client,
    key: &str,
    lat: f64,
    lon: f64,
    radius_km: f64,
    days: u32,
    limit: u32,
) -> Result<Vec<Observation>> {
    let url = format!(
        "https://api.ebird.org/v2/data/obs/geo/recent?lat={:.4}&lng={:.4}&dist={:.1}&back={}&maxResults={}",
        lat, lon, radius_km, days, limit
    );
    let resp = http
        .get(&url)
        .header("X-eBirdApiToken", key)
        .send()
        .await
        .context("eBird HTTP request failed")?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        anyhow::bail!("eBird returned 401 — check EBIRD_API_KEY");
    }
    if !resp.status().is_success() {
        anyhow::bail!("eBird returned HTTP {}", resp.status());
    }
    let body: Vec<EBirdObs> = resp.json().await.context("parsing eBird JSON")?;

    Ok(body
        .into_iter()
        .map(|o| Observation {
            common_name: Some(o.com_name),
            scientific_name: Some(o.sci_name),
            observed_on: Some(o.obs_dt),
            location: Some(o.loc_name),
            observer: None,
            url: Some(format!(
                "https://ebird.org/species/{}",
                o.species_code
            )),
            iconic_taxon: Some("Aves".to_string()),
            count: o.how_many,
        })
        .collect())
}

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

// ─── formatting ───

fn format_species(
    obs: &[Observation],
    lat: f64,
    lon: f64,
    radius_km: f64,
    days: u32,
) -> String {
    if obs.is_empty() {
        return format!(
            "No iNaturalist observations found within {:.0} km of ({:.3}, {:.3}) in the last {} days.\n",
            radius_km, lat, lon, days
        );
    }
    let mut out = format!(
        "# iNaturalist observations — ({:.3}, {:.3}) · {:.0} km radius · last {} days\n\n",
        lat, lon, radius_km, days
    );
    out.push_str(&format!("_{} observations._\n\n", obs.len()));

    for o in obs {
        let name = match (&o.common_name, &o.scientific_name) {
            (Some(c), Some(s)) => format!("**{}** (_{}_)", c, s),
            (Some(c), None) => format!("**{}**", c),
            (None, Some(s)) => format!("_{}_", s),
            (None, None) => "**unknown**".to_string(),
        };
        out.push_str(&format!("- {}", name));
        if let Some(t) = &o.iconic_taxon {
            out.push_str(&format!(" · {}", t));
        }
        if let Some(d) = &o.observed_on {
            out.push_str(&format!(" · {}", d));
        }
        if let Some(l) = &o.location {
            out.push_str(&format!(" · {}", l));
        }
        if let Some(u) = &o.observer {
            out.push_str(&format!(" · @{}", u));
        }
        if let Some(url) = &o.url {
            out.push_str(&format!(" · [iNat]({})", url));
        }
        out.push('\n');
    }

    out.push_str(&format!(
        "\n_Source: iNaturalist v1 API. Last updated: {}_\n",
        now_pacific().format("%-I:%M %p")
    ));
    out
}

fn format_birds(
    obs: &[Observation],
    lat: f64,
    lon: f64,
    radius_km: f64,
    days: u32,
) -> String {
    if obs.is_empty() {
        return format!(
            "No eBird observations found within {:.0} km of ({:.3}, {:.3}) in the last {} days.\n",
            radius_km, lat, lon, days
        );
    }
    let mut out = format!(
        "# eBird recent observations — ({:.3}, {:.3}) · {:.0} km radius · last {} days\n\n",
        lat, lon, radius_km, days
    );
    out.push_str(&format!("_{} observations._\n\n", obs.len()));

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

    out.push_str(&format!(
        "\n_Source: eBird API 2.0. Last updated: {}_\n",
        now_pacific().format("%-I:%M %p")
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_species_empty() {
        let out = format_species(&[], 36.97, -122.03, 25.0, 30);
        assert!(out.contains("No iNaturalist"));
    }

    #[test]
    fn format_birds_renders() {
        let obs = vec![Observation {
            common_name: Some("Brown Pelican".to_string()),
            scientific_name: Some("Pelecanus occidentalis".to_string()),
            observed_on: Some("2026-04-17 08:30".to_string()),
            location: Some("Natural Bridges State Beach".to_string()),
            observer: None,
            url: Some("https://ebird.org/species/brnpel".to_string()),
            iconic_taxon: Some("Aves".to_string()),
            count: Some(12),
        }];
        let out = format_birds(&obs, 36.97, -122.03, 25.0, 7);
        assert!(out.contains("Brown Pelican"));
        assert!(out.contains("12 individual(s)"));
        assert!(out.contains("Natural Bridges"));
    }

    #[test]
    fn format_species_renders() {
        let obs = vec![Observation {
            common_name: Some("Pacific Banana Slug".to_string()),
            scientific_name: Some("Ariolimax columbianus".to_string()),
            observed_on: Some("2026-04-10".to_string()),
            location: Some("Henry Cowell Redwoods".to_string()),
            observer: Some("slugwatcher".to_string()),
            url: Some("https://inaturalist.org/observations/1".to_string()),
            iconic_taxon: Some("Mollusca".to_string()),
            count: None,
        }];
        let out = format_species(&obs, 36.97, -122.03, 25.0, 30);
        assert!(out.contains("Banana Slug"));
        assert!(out.contains("Ariolimax"));
        assert!(out.contains("@slugwatcher"));
    }
}
