pub mod tribe;

use std::sync::Arc;

use anyhow::Result;

use crate::cache::CacheStore;
use tribe::{TribeClient, TribeEvent};

pub struct EventsService {
    client: TribeClient,
    cache: Arc<CacheStore>,
}

impl EventsService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self {
            client: TribeClient::new(http),
            cache,
        }
    }

    pub async fn search_events(
        &self,
        query: Option<&str>,
        days: Option<u32>,
        category: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<TribeEvent>> {
        let limit = limit.unwrap_or(10).min(50);
        let limit_str = limit.to_string();

        let cache_key = format!(
            "events:search:{}:{}:{}:{}",
            query.unwrap_or(""),
            days.unwrap_or(0),
            category.unwrap_or(""),
            limit
        );

        if let Some(cached) = self.cache.get(&cache_key).await {
            if let Ok(events) = serde_json::from_str(&cached) {
                return Ok(events);
            }
        }

        let mut params: Vec<(&str, &str)> = vec![("per_page", &limit_str)];

        if let Some(q) = query {
            params.push(("search", q));
        }

        if let Some(cat) = category {
            params.push(("categories", cat));
        }

        // Tribe Events uses start_date/end_date for date filtering
        // Default: returns future events from now

        let resp = self.client.fetch_events(&params).await?;

        if let Ok(json) = serde_json::to_string(&resp.events) {
            self.cache.set(&cache_key, &json, 900).await; // 15 min TTL
        }

        Ok(resp.events)
    }

    pub async fn get_upcoming_events(&self, limit: u32) -> Result<Vec<TribeEvent>> {
        self.search_events(None, None, None, Some(limit)).await
    }
}
