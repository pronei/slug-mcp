pub mod scraper;

use std::sync::Arc;

use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StudyRoomAvailabilityRequest {
    /// Library name: "McHenry" or "Science & Engineering". If omitted, returns both.
    pub library: Option<String>,
    /// Date in YYYY-MM-DD format. If omitted, returns today's availability.
    pub date: Option<String>,
}

use crate::cache::CacheStore;
use scraper::{find_library, library_names, scrape_availability, RoomAvailability, LIBRARIES};

pub struct LibraryService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl LibraryService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    pub async fn get_availability(
        &self,
        library: Option<&str>,
        date: Option<&str>,
    ) -> Result<String> {
        let today = crate::util::now_pacific().format("%Y-%m-%d").to_string();
        let date = date.unwrap_or(&today);

        let lids: Vec<(u32, &str)> = if let Some(query) = library {
            let lib = find_library(query).ok_or_else(|| {
                anyhow::anyhow!(
                    "Library '{}' not found. Available: {}",
                    query,
                    library_names()
                )
            })?;
            vec![(lib.lid, lib.name)]
        } else {
            LIBRARIES.iter().map(|l| (l.lid, l.name)).collect()
        };

        let futures: Vec<_> = lids
            .iter()
            .map(|(lid, _name)| {
                let cache_key = format!("library:availability:{}:{}", lid, date);
                let lid = *lid;
                let cache = &self.cache;
                let http = &self.http;
                let date = date.to_string();
                async move {
                    cache
                        .get_or_fetch(&cache_key, 300, || scrape_availability(http, lid, &date))
                        .await
                }
            })
            .collect();
        let availabilities: Vec<RoomAvailability> = futures_util::future::join_all(futures)
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()?;

        let output = availabilities
            .iter()
            .map(|a| a.format())
            .collect::<Vec<_>>()
            .join("\n\n");

        Ok(output)
    }
}
