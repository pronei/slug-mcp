pub mod locations;
pub mod scraper;

use std::sync::Arc;

use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchClassroomsRequest {
    /// Classroom or building name to search for (e.g., "Baskin", "Classroom Unit").
    pub name: Option<String>,
    /// Minimum seating capacity.
    pub min_capacity: Option<u32>,
    /// Maximum seating capacity.
    pub max_capacity: Option<u32>,
    /// Campus area or building filter (e.g., "crown-college", "science-hill").
    pub building: Option<String>,
    /// Required technology (e.g., "lecture-capture", "wireless-projection").
    pub technology: Option<String>,
    /// Required physical feature (e.g., "ada-accessible", "chalkboards").
    pub feature: Option<String>,
}

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
        let body: Vec<String> = filtered
            .iter()
            .map(|c| {
                let loc = c
                    .area
                    .as_deref()
                    .and_then(locations::lookup_by_area);
                c.format_with_location(loc)
            })
            .collect();
        Ok(format!("{}{}", header, body.join("\n\n")))
    }

    async fn get_all(&self) -> Result<Vec<Classroom>> {
        let http = &self.http;
        self.cache
            .get_or_fetch("classrooms:all", 86400, || scrape_classrooms(http))
            .await
    }
}
