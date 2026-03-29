pub mod scraper;

use std::sync::Arc;

use anyhow::Result;

use crate::cache::CacheStore;
use scraper::{
    book_room, find_library, library_names, scrape_availability, RoomAvailability, LIBRARIES,
};

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

            let avail = if let Some(cached) = self.cache.get(&cache_key).await {
                serde_json::from_str::<RoomAvailability>(&cached).ok()
            } else {
                None
            };

            let avail = match avail {
                Some(a) => a,
                None => {
                    let a = scrape_availability(&self.http, *lid, date).await?;
                    if let Ok(json) = serde_json::to_string(&a) {
                        self.cache.set(&cache_key, &json, 300).await; // 5 minutes
                    }
                    a
                }
            };

            outputs.push(avail.to_string());
        }

        Ok(outputs.join("\n\n"))
    }

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
