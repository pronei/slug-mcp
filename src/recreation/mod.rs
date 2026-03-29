pub mod scraper;

use std::sync::Arc;

use anyhow::Result;

use crate::cache::CacheStore;
use scraper::{find_facility, scrape_occupancy, scrape_schedule, FacilityOccupancy};

pub struct RecreationService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl RecreationService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    pub async fn get_occupancy(&self, facility_name: Option<&str>) -> Result<String> {
        let all = self.fetch_occupancy().await?;

        let display: Vec<&FacilityOccupancy> = if let Some(query) = facility_name {
            let matches = find_facility(query, &all);
            if matches.is_empty() {
                let names: Vec<&str> = all.iter().map(|f| f.name.as_str()).collect();
                anyhow::bail!(
                    "Facility '{}' not found. Available: {}",
                    query,
                    names.join(", ")
                );
            }
            matches
        } else {
            all.iter().collect()
        };

        let header = "## UCSC Recreation Facility Occupancy\n".to_string();
        let body: Vec<String> = display.iter().map(|f| f.to_string()).collect();
        Ok(format!("{}{}", header, body.join("\n\n")))
    }

    pub async fn get_schedule(&self, facility_id: &str) -> Result<String> {
        let cache_key = format!("recreation:schedule:{}", facility_id);

        if let Some(cached) = self.cache.get(&cache_key).await {
            return Ok(cached);
        }

        let schedule = scrape_schedule(&self.http, facility_id).await?;
        let output = schedule.to_string();

        self.cache.set(&cache_key, &output, 3600).await; // 1 hour
        Ok(output)
    }

    async fn fetch_occupancy(&self) -> Result<Vec<FacilityOccupancy>> {
        let cache_key = "recreation:occupancy";

        if let Some(cached) = self.cache.get(cache_key).await {
            if let Ok(data) = serde_json::from_str::<Vec<FacilityOccupancy>>(&cached) {
                return Ok(data);
            }
        }

        let data = scrape_occupancy(&self.http).await?;

        if let Ok(json) = serde_json::to_string(&data) {
            self.cache.set(cache_key, &json, 120).await; // 2 minutes
        }

        Ok(data)
    }
}
