pub mod tribe;

use std::sync::Arc;

use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchEventsRequest {
    /// Search query string
    pub query: Option<String>,
    /// Event category/type filter (e.g., "workshop", "lecture")
    pub category: Option<String>,
    /// Max results (default 10, max 50)
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpcomingEventsRequest {
    /// Number of events to return (default 10, max 50)
    pub limit: Option<u32>,
}

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

        let client = &self.client;
        let events: Vec<TribeEvent> = self
            .cache
            .get_or_fetch(&cache_key, 900, || async {
                let mut params: Vec<(&str, &str)> = vec![("per_page", &limit_str)];
                if let Some(q) = query {
                    params.push(("search", q));
                }
                if let Some(cat) = category {
                    params.push(("categories", cat));
                }
                let resp = client.fetch_events(&params).await?;
                Ok(resp.events)
            })
            .await?;

        Ok(events)
    }

    pub async fn get_upcoming_events(&self, limit: u32) -> Result<Vec<TribeEvent>> {
        self.search_events(None, None, None, Some(limit)).await
    }
}
