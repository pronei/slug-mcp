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

#[cfg(feature = "auth")]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct BookStudyRoomRequest {
    /// Space/room ID from get_study_room_availability output.
    pub space_id: u32,
    /// Date in YYYY-MM-DD format.
    pub date: String,
    /// Start time (e.g., "09:00", "2:00 PM").
    pub start_time: String,
    /// End time (e.g., "10:00", "3:00 PM").
    pub end_time: String,
}

use crate::cache::CacheStore;
#[cfg(feature = "auth")]
use scraper::book_room;
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
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
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

        let mut outputs = Vec::new();

        for (lid, _name) in &lids {
            let cache_key = format!("library:availability:{}:{}", lid, date);
            let http = &self.http;
            let lid = *lid;
            let avail: RoomAvailability = self
                .cache
                .get_or_fetch(&cache_key, 300, || scrape_availability(http, lid, date))
                .await?;
            outputs.push(avail.to_string());
        }

        Ok(outputs.join("\n\n"))
    }

    #[cfg(feature = "auth")]
    pub async fn book(
        &self,
        auth_client: &reqwest::Client,
        space_id: u32,
        date: &str,
        start_time: &str,
        end_time: &str,
    ) -> Result<String> {
        let result = book_room(auth_client, space_id, date, start_time, end_time).await?;

        // Invalidate availability caches for all libraries on this date
        for lib in LIBRARIES {
            let cache_key = format!("library:availability:{}:{}", lib.lid, date);
            self.cache.invalidate(&cache_key).await;
        }

        Ok(result.to_string())
    }
}
