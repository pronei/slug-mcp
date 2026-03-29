pub mod scraper;

use std::sync::Arc;

use anyhow::Result;

use crate::cache::CacheStore;
use scraper::{filter_classrooms, scrape_classrooms, Classroom};

pub struct ClassroomService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl ClassroomService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    pub async fn search(
        &self,
        name: Option<&str>,
        min_capacity: Option<u32>,
        max_capacity: Option<u32>,
        building: Option<&str>,
        technology: Option<&str>,
        feature: Option<&str>,
    ) -> Result<String> {
        let all = self.get_all().await?;

        let filtered = filter_classrooms(&all, name, min_capacity, max_capacity, building, technology, feature);

        if filtered.is_empty() {
            return Ok("No classrooms found matching your criteria.".to_string());
        }

        let header = format!("## UCSC Classrooms ({} results)\n", filtered.len());
        let body: Vec<String> = filtered.iter().map(|c| c.to_string()).collect();
        Ok(format!("{}{}", header, body.join("\n\n")))
    }

    async fn get_all(&self) -> Result<Vec<Classroom>> {
        let cache_key = "classrooms:all";

        if let Some(cached) = self.cache.get(cache_key).await {
            if let Ok(classrooms) = serde_json::from_str::<Vec<Classroom>>(&cached) {
                return Ok(classrooms);
            }
        }

        let classrooms = scrape_classrooms(&self.http).await?;

        if let Ok(json) = serde_json::to_string(&classrooms) {
            self.cache.set(cache_key, &json, 86400).await; // 24 hours
        }

        Ok(classrooms)
    }
}
