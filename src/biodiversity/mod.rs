//! Biodiversity observation tools — iNaturalist + eBird.
//!
//! Five public tools exposed:
//! - `search_species_observations`     → iNaturalist v1 (no key)
//! - `search_bird_observations`        → eBird recent obs (region + geo, with optional species and notable-only)
//! - `get_historic_bird_observations`  → eBird date-specific (closes the 30-day rolling window)
//! - `get_bird_hotspots`               → eBird hotspot reference
//! - `find_nearest_bird_sighting`      → eBird nearest-species-by-coords
//!
//! Defaults to Santa Cruz County (`US-CA-087`) or Santa Cruz coords
//! (36.9741, -122.0308) with 25 km radius depending on the query mode.
//!
//! iNaturalist: <https://api.inaturalist.org/v1/>
//! eBird: <https://documenter.getpostman.com/view/664302/S1ENwy59>

use std::sync::Arc;

use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

use crate::cache::CacheStore;

mod ebird;
mod ebird_taxonomy;
mod inaturalist;

use ebird::Hotspot;
use ebird_taxonomy::{SpeciesLookup, TaxonomyIndex};

const DEFAULT_LAT: f64 = 36.9741;
const DEFAULT_LON: f64 = -122.0308;
const DEFAULT_RADIUS_KM: f64 = 25.0;
/// Santa Cruz County. The eBird region code most relevant to UCSC users.
const DEFAULT_REGION: &str = "US-CA-087";

// ─── Request types ───

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
    /// eBird region code (e.g. `US-CA-087` for Santa Cruz County, `US-CA` for
    /// California, `world`, or a hotspot `L######`). Defaults to `US-CA-087`
    /// when neither region nor lat/lon are supplied. If both region and
    /// lat/lon are supplied, region wins.
    pub region: Option<String>,
    /// Latitude for geographic search (alternative to `region`).
    pub lat: Option<f64>,
    /// Longitude for geographic search (alternative to `region`).
    pub lon: Option<f64>,
    /// Geographic search radius in km (default 25, max 50 per eBird limit).
    pub radius_km: Option<f64>,
    /// Free-text species name (common, scientific, banding code, or 6-letter
    /// eBird species code). Ambiguous names return a "did you mean..." list.
    /// When set, `notable_only` is ignored.
    pub species: Option<String>,
    /// Days back to search (default 7, clamped 1-30).
    pub back: Option<u32>,
    /// If true and no `species` is set, return only sightings flagged "notable"
    /// by eBird reviewers (locally unusual, not globally rare). Default false.
    pub notable_only: Option<bool>,
    /// Max results (default 25, hard cap 200).
    pub max_results: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HistoricBirdRequest {
    /// eBird region code (default `US-CA-087` = Santa Cruz County).
    pub region: Option<String>,
    /// Four-digit year. Required.
    pub year: u32,
    /// Month 1-12. Required.
    pub month: u32,
    /// Day 1-31. Required.
    pub day: u32,
    /// `mrec` (most-recent observation per species, default) or `create`
    /// (first reported).
    pub rank: Option<String>,
    /// Max results (default 50, hard cap 200).
    pub max_results: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HotspotRequest {
    /// eBird region code (default `US-CA-087`). Alternative to `lat`/`lon`.
    pub region: Option<String>,
    /// Latitude for geographic hotspot search (alternative to `region`).
    pub lat: Option<f64>,
    /// Longitude for geographic hotspot search.
    pub lon: Option<f64>,
    /// Geographic search radius in km (default 25, max 50).
    pub radius_km: Option<f64>,
    /// If set, restrict to hotspots with observations in the last N days (1-30).
    pub back: Option<u32>,
    /// Max results to render (default 25, hard cap 200).
    pub max_results: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NearestBirdRequest {
    /// Species (common name, scientific name, banding code, or 6-letter eBird
    /// code). Required. Ambiguous names return a "did you mean..." list.
    pub species: String,
    /// Latitude to search from (default 36.9741, Santa Cruz).
    pub lat: Option<f64>,
    /// Longitude to search from (default -122.0308, Santa Cruz).
    pub lon: Option<f64>,
    /// Search radius in km (default 50, max 50 per eBird limit).
    pub radius_km: Option<f64>,
    /// Days back to search (default 30, clamped 1-30).
    pub back: Option<u32>,
    /// Max results (default 10, hard cap 1000 per eBird limit on this endpoint).
    pub max_results: Option<u32>,
}

// ─── Shared observation shape ───

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

// ─── Service ───

pub struct BiodiversityService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
    ebird_key: Option<String>,
    /// Process-lifetime taxonomy index. Built on first species-typed query;
    /// prevents cache stampede that `CacheStore::get_or_fetch` doesn't protect
    /// against.
    taxonomy: OnceCell<Arc<TaxonomyIndex>>,
}

impl BiodiversityService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>, ebird_key: Option<String>) -> Self {
        Self {
            http,
            cache,
            ebird_key,
            taxonomy: OnceCell::new(),
        }
    }

    fn ebird_key(&self) -> Option<String> {
        self.ebird_key.as_ref().filter(|k| !k.is_empty()).cloned()
    }

    fn ebird_key_required(&self) -> std::result::Result<String, String> {
        self.ebird_key().ok_or_else(|| {
            "eBird API key not configured.\n\
             Get a free key at https://ebird.org/api/keygen and set \
             the `EBIRD_API_KEY` environment variable."
                .to_string()
        })
    }

    /// Lazy-init the taxonomy index. The OnceCell is the real single-flight
    /// guard; the CacheStore entry is just so a server restart within 24h can
    /// rebuild from the cached spplist/taxonomy bodies (currently the build
    /// re-fetches, so this is forward-looking).
    async fn taxonomy(&self, key: &str) -> Result<Arc<TaxonomyIndex>> {
        self.taxonomy
            .get_or_try_init(|| async {
                let idx = ebird_taxonomy::build_index(&self.http, key).await?;
                Ok(Arc::new(idx))
            })
            .await
            .cloned()
    }

    // ─── iNaturalist ───

    #[allow(clippy::too_many_arguments)]
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
                inaturalist::fetch_inaturalist(
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

        Ok(inaturalist::format_species(
            &observations,
            lat,
            lon,
            radius,
            days,
        ))
    }

    pub async fn fetch_species_typed(
        &self,
        query: Option<&str>,
        lat: f64,
        lon: f64,
        radius_km: f64,
        days: u32,
        limit: u32,
    ) -> Result<Vec<Observation>> {
        let cache_key = format!(
            "bio:inat:typed:{}:{:.3}:{:.3}:{:.1}:{}:{}",
            query.unwrap_or(""),
            lat,
            lon,
            radius_km,
            days,
            limit
        );
        let http = self.http.clone();
        let query = query.map(|s| s.to_string());
        self.cache
            .get_or_fetch(&cache_key, 1800, move || async move {
                inaturalist::fetch_inaturalist(
                    &http,
                    query.as_deref(),
                    lat,
                    lon,
                    radius_km,
                    days,
                    None,
                    limit,
                )
                .await
            })
            .await
    }

    // ─── eBird: recent observations ───

    pub async fn search_birds(&self, req: &BirdRequest) -> Result<String> {
        let key = match self.ebird_key_required() {
            Ok(k) => k,
            Err(msg) => return Ok(msg),
        };

        let back = req.back.unwrap_or(7).clamp(1, 30);
        let max_results = req.max_results.unwrap_or(25).min(200);
        let notable = req.notable_only.unwrap_or(false);

        // Decide region vs geo mode. Region wins if both are supplied.
        let use_region = req.region.is_some() || (req.lat.is_none() && req.lon.is_none());

        // Resolve species name → code if requested.
        let species_code = if let Some(q) = req.species.as_deref().filter(|s| !s.trim().is_empty())
        {
            match self.resolve_species(&key, q).await? {
                SpeciesResolved::Code(c) => Some(c),
                SpeciesResolved::Message(m) => return Ok(m),
            }
        } else {
            None
        };

        if use_region {
            let region = req.region.as_deref().unwrap_or(DEFAULT_REGION).to_string();
            let title = format!("region `{region}` · last {back} days");
            let empty = format!("for region `{region}` in the last {back} days");

            let observations = if let Some(code) = species_code {
                let title = format!("{title} · species `{code}`");
                let empty =
                    format!("for species `{code}` in region `{region}` in the last {back} days");
                let cache_key =
                    format!("bio:ebird:recent:region:{region}:species:{code}:{back}:{max_results}");
                let http = self.http.clone();
                let key2 = key.clone();
                let region2 = region.clone();
                let code2 = code.clone();
                let obs = self
                    .cache
                    .get_or_fetch::<Vec<Observation>, _, _>(&cache_key, 1800, move || async move {
                        ebird::fetch_recent_region_species(
                            &http,
                            &key2,
                            &region2,
                            &code2,
                            back,
                            max_results,
                        )
                        .await
                    })
                    .await?;
                return Ok(ebird::format_recent(&obs, &title, &empty));
            } else if notable {
                let cache_key =
                    format!("bio:ebird:recent:region:{region}:notable:{back}:{max_results}");
                let http = self.http.clone();
                let key2 = key.clone();
                let region2 = region.clone();
                self.cache
                    .get_or_fetch::<Vec<Observation>, _, _>(&cache_key, 1800, move || async move {
                        ebird::fetch_recent_region_notable(
                            &http,
                            &key2,
                            &region2,
                            back,
                            max_results,
                        )
                        .await
                    })
                    .await?
            } else {
                let cache_key =
                    format!("bio:ebird:recent:region:{region}:all:{back}:{max_results}");
                let http = self.http.clone();
                let key2 = key.clone();
                let region2 = region.clone();
                self.cache
                    .get_or_fetch::<Vec<Observation>, _, _>(&cache_key, 1800, move || async move {
                        ebird::fetch_recent_region(&http, &key2, &region2, back, max_results).await
                    })
                    .await?
            };

            let title = if notable {
                format!("notable in region `{region}` · last {back} days")
            } else {
                title
            };
            Ok(ebird::format_recent(&observations, &title, &empty))
        } else {
            // Geographic mode.
            let lat = req.lat.unwrap_or(DEFAULT_LAT);
            let lon = req.lon.unwrap_or(DEFAULT_LON);
            let radius = req.radius_km.unwrap_or(DEFAULT_RADIUS_KM).min(50.0);
            let title_base = format!(
                "({:.3}, {:.3}) · {:.0} km radius · last {} days",
                lat, lon, radius, back
            );
            let empty_base = format!(
                "within {:.0} km of ({:.3}, {:.3}) in the last {} days",
                radius, lat, lon, back
            );

            let observations = if let Some(code) = species_code {
                let title = format!("{title_base} · species `{code}`");
                let empty = format!("for species `{code}` {empty_base}");
                let cache_key = format!(
                    "bio:ebird:recent:geo:{:.3}:{:.3}:{:.1}:species:{code}:{back}:{max_results}",
                    lat, lon, radius
                );
                let http = self.http.clone();
                let key2 = key.clone();
                let code2 = code.clone();
                let obs = self
                    .cache
                    .get_or_fetch::<Vec<Observation>, _, _>(&cache_key, 1800, move || async move {
                        ebird::fetch_recent_geo_species(
                            &http,
                            &key2,
                            &code2,
                            lat,
                            lon,
                            radius,
                            back,
                            max_results,
                        )
                        .await
                    })
                    .await?;
                return Ok(ebird::format_recent(&obs, &title, &empty));
            } else if notable {
                let cache_key = format!(
                    "bio:ebird:recent:geo:{:.3}:{:.3}:{:.1}:notable:{back}:{max_results}",
                    lat, lon, radius
                );
                let http = self.http.clone();
                let key2 = key.clone();
                self.cache
                    .get_or_fetch::<Vec<Observation>, _, _>(&cache_key, 1800, move || async move {
                        ebird::fetch_recent_geo_notable(
                            &http,
                            &key2,
                            lat,
                            lon,
                            radius,
                            back,
                            max_results,
                        )
                        .await
                    })
                    .await?
            } else {
                let cache_key = format!(
                    "bio:ebird:recent:geo:{:.3}:{:.3}:{:.1}:all:{back}:{max_results}",
                    lat, lon, radius
                );
                let http = self.http.clone();
                let key2 = key.clone();
                self.cache
                    .get_or_fetch::<Vec<Observation>, _, _>(&cache_key, 1800, move || async move {
                        ebird::fetch_recent_geo(&http, &key2, lat, lon, radius, back, max_results)
                            .await
                    })
                    .await?
            };

            let title = if notable {
                format!("notable · {title_base}")
            } else {
                title_base
            };
            Ok(ebird::format_recent(&observations, &title, &empty_base))
        }
    }

    /// Preserved for ocean-fusion callers. Same signature and cache key as
    /// before the refactor — internally uses the geo-recent fetcher.
    pub async fn fetch_birds_typed(
        &self,
        lat: f64,
        lon: f64,
        radius_km: f64,
        days: u32,
        limit: u32,
    ) -> Result<Vec<Observation>> {
        let key = match self.ebird_key() {
            Some(k) => k,
            None => return Ok(Vec::new()),
        };
        let cache_key = format!(
            "bio:ebird:typed:{:.3}:{:.3}:{:.1}:{}:{}",
            lat, lon, radius_km, days, limit
        );
        let http = self.http.clone();
        self.cache
            .get_or_fetch(&cache_key, 1800, move || async move {
                ebird::fetch_recent_geo(&http, &key, lat, lon, radius_km, days, limit).await
            })
            .await
    }

    // ─── eBird: historic ───

    pub async fn get_historic_birds(&self, req: &HistoricBirdRequest) -> Result<String> {
        let key = match self.ebird_key_required() {
            Ok(k) => k,
            Err(msg) => return Ok(msg),
        };
        if let Err(msg) = validate_date(req.year, req.month, req.day) {
            return Ok(msg);
        }
        let region = req.region.as_deref().unwrap_or(DEFAULT_REGION).to_string();
        let rank = match req.rank.as_deref() {
            Some("create") => "create",
            _ => "mrec",
        }
        .to_string();
        let max_results = req.max_results.unwrap_or(50).min(200);
        let year = req.year;
        let month = req.month;
        let day = req.day;

        let cache_key = format!(
            "bio:ebird:historic:{region}:{year:04}-{month:02}-{day:02}:{rank}:{max_results}"
        );
        let http = self.http.clone();
        let key2 = key.clone();
        let region2 = region.clone();
        let rank2 = rank.clone();
        let observations = self
            .cache
            .get_or_fetch::<Vec<Observation>, _, _>(&cache_key, 86400, move || async move {
                ebird::fetch_historic_region(
                    &http,
                    &key2,
                    &region2,
                    year,
                    month,
                    day,
                    &rank2,
                    max_results,
                )
                .await
            })
            .await?;

        Ok(ebird::format_historic(
            &observations,
            &region,
            year,
            month,
            day,
        ))
    }

    // ─── eBird: hotspots ───

    pub async fn get_bird_hotspots(&self, req: &HotspotRequest) -> Result<String> {
        let key = match self.ebird_key_required() {
            Ok(k) => k,
            Err(msg) => return Ok(msg),
        };
        let max_results = req.max_results.unwrap_or(25).min(200) as usize;
        let back = req.back.map(|b| b.clamp(1, 30));

        let use_region = req.region.is_some() || (req.lat.is_none() && req.lon.is_none());

        let mut spots: Vec<Hotspot> = if use_region {
            let region = req.region.as_deref().unwrap_or(DEFAULT_REGION).to_string();
            let cache_key = format!(
                "bio:ebird:hotspot:region:{region}:{}",
                back.map(|b| b.to_string()).unwrap_or_else(|| "any".into())
            );
            let http = self.http.clone();
            let key2 = key.clone();
            let region2 = region.clone();
            self.cache
                .get_or_fetch::<Vec<Hotspot>, _, _>(&cache_key, 86400, move || async move {
                    ebird::fetch_hotspots_region(&http, &key2, &region2, back).await
                })
                .await?
        } else {
            let lat = req.lat.unwrap_or(DEFAULT_LAT);
            let lon = req.lon.unwrap_or(DEFAULT_LON);
            let radius = req.radius_km.unwrap_or(DEFAULT_RADIUS_KM).min(50.0);
            let cache_key = format!(
                "bio:ebird:hotspot:geo:{:.3}:{:.3}:{:.1}:{}",
                lat,
                lon,
                radius,
                back.map(|b| b.to_string()).unwrap_or_else(|| "any".into())
            );
            let http = self.http.clone();
            let key2 = key.clone();
            self.cache
                .get_or_fetch::<Vec<Hotspot>, _, _>(&cache_key, 86400, move || async move {
                    ebird::fetch_hotspots_geo(&http, &key2, lat, lon, radius, back).await
                })
                .await?
        };

        // Sort by species count desc to surface the best spots first.
        spots.sort_by(|a, b| {
            b.num_species_all_time
                .unwrap_or(0)
                .cmp(&a.num_species_all_time.unwrap_or(0))
        });
        spots.truncate(max_results);

        let (title, empty) = if use_region {
            let region = req.region.as_deref().unwrap_or(DEFAULT_REGION);
            (
                format!("region `{region}`"),
                format!("in region `{region}`"),
            )
        } else {
            let lat = req.lat.unwrap_or(DEFAULT_LAT);
            let lon = req.lon.unwrap_or(DEFAULT_LON);
            let radius = req.radius_km.unwrap_or(DEFAULT_RADIUS_KM).min(50.0);
            (
                format!("({:.3}, {:.3}) · {:.0} km radius", lat, lon, radius),
                format!("within {:.0} km of ({:.3}, {:.3})", radius, lat, lon),
            )
        };

        Ok(ebird::format_hotspots(&spots, &title, &empty))
    }

    // ─── eBird: nearest species sighting ───

    pub async fn find_nearest_bird(&self, req: &NearestBirdRequest) -> Result<String> {
        let key = match self.ebird_key_required() {
            Ok(k) => k,
            Err(msg) => return Ok(msg),
        };

        let lat = req.lat.unwrap_or(DEFAULT_LAT);
        let lon = req.lon.unwrap_or(DEFAULT_LON);
        let radius = req.radius_km.unwrap_or(50.0).min(50.0);
        let back = req.back.unwrap_or(30).clamp(1, 30);
        let max_results = req.max_results.unwrap_or(10).min(1000);

        let (code, label) = match self.resolve_species(&key, &req.species).await? {
            SpeciesResolved::Code(c) => (c.clone(), c),
            SpeciesResolved::Message(m) => return Ok(m),
        };

        let cache_key = format!(
            "bio:ebird:nearest:{code}:{:.3}:{:.3}:{:.1}:{back}:{max_results}",
            lat, lon, radius
        );
        let http = self.http.clone();
        let key2 = key.clone();
        let code2 = code.clone();
        let observations = self
            .cache
            .get_or_fetch::<Vec<Observation>, _, _>(&cache_key, 900, move || async move {
                ebird::fetch_nearest_species(
                    &http,
                    &key2,
                    &code2,
                    lat,
                    lon,
                    radius,
                    back,
                    max_results,
                )
                .await
            })
            .await?;

        Ok(ebird::format_nearest(&observations, &label, lat, lon))
    }

    // ─── Helpers ───

    async fn resolve_species(&self, key: &str, query: &str) -> Result<SpeciesResolved> {
        let idx = self.taxonomy(key).await?;
        Ok(match idx.lookup(query) {
            SpeciesLookup::Exact(code) => SpeciesResolved::Code(code),
            SpeciesLookup::Ambiguous(candidates) => SpeciesResolved::Message(format!(
                "Multiple species match `{query}`. Did you mean one of these? \
                 Re-call with a more specific name or with the species code.\n\n{}",
                ebird_taxonomy::format_candidates(&candidates)
            )),
            SpeciesLookup::NotFound(suggestions) => {
                if suggestions.is_empty() {
                    SpeciesResolved::Message(format!(
                        "No species matching `{query}` in the California list. \
                         Use a 6-letter eBird species code or a more common name."
                    ))
                } else {
                    SpeciesResolved::Message(format!(
                        "No species matching `{query}` in the California list. \
                         Closest suggestions:\n\n{}",
                        ebird_taxonomy::format_candidates(&suggestions)
                    ))
                }
            }
        })
    }
}

enum SpeciesResolved {
    Code(String),
    Message(String),
}

fn validate_date(year: u32, month: u32, day: u32) -> std::result::Result<(), String> {
    let current_year = chrono::Utc::now()
        .format("%Y")
        .to_string()
        .parse::<u32>()
        .unwrap_or(2026);
    if year < 1900 || year > current_year + 1 {
        return Err(format!(
            "Invalid year `{year}` — eBird historic data starts in 1900."
        ));
    }
    if !(1..=12).contains(&month) {
        return Err(format!("Invalid month `{month}` — must be 1-12."));
    }
    if !(1..=31).contains(&day) {
        return Err(format!("Invalid day `{day}` — must be 1-31."));
    }
    if chrono::NaiveDate::from_ymd_opt(year as i32, month, day).is_none() {
        return Err(format!("Invalid date {year:04}-{month:02}-{day:02}."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_date_accepts_valid() {
        assert!(validate_date(2025, 4, 10).is_ok());
        assert!(validate_date(2024, 2, 29).is_ok()); // leap year
    }

    #[test]
    fn validate_date_rejects_bad_month_day() {
        assert!(validate_date(2025, 13, 1).is_err());
        assert!(validate_date(2025, 4, 31).is_err());
        assert!(validate_date(2023, 2, 29).is_err()); // non-leap
    }

    #[test]
    fn validate_date_rejects_out_of_range_year() {
        assert!(validate_date(1899, 1, 1).is_err());
        assert!(validate_date(3000, 1, 1).is_err());
    }
}
