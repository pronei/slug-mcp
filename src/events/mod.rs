pub mod eventbrite;
pub mod tribe;

use std::sync::Arc;

use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::cache::CacheStore;
use eventbrite::{EventbriteClient, Event as EventbriteEvent};
use tribe::{TribeClient, TribeEvent};

/// Default location for Eventbrite searches (Eventbrite URL slug format).
const DEFAULT_LOCATION_SLUG: &str = "ca--santa-cruz";

// ─── Request types ───

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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchEventbriteRequest {
    /// Search query (e.g., "hackathon", "music", "tech meetup")
    pub query: Option<String>,
    /// Location to search around (default: "Santa Cruz, CA"). Use "City, ST" format.
    pub location: Option<String>,
    /// Max results (default 10, max 20)
    pub limit: Option<u32>,
}

// ─── Service ───

pub struct EventsService {
    tribe: TribeClient,
    eventbrite: EventbriteClient,
    cache: Arc<CacheStore>,
}

impl EventsService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self {
            tribe: TribeClient::new(http.clone()),
            eventbrite: EventbriteClient::new(http),
            cache,
        }
    }

    /// Search UCSC campus events (Tribe Events API).
    pub async fn search_events(
        &self,
        query: Option<&str>,
        _days: Option<u32>,
        category: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<TribeEvent>> {
        let limit = limit.unwrap_or(10).min(50);
        let limit_str = limit.to_string();

        let cache_key = format!(
            "events:tribe:{}:{}:{}",
            query.unwrap_or(""),
            category.unwrap_or(""),
            limit
        );

        let client = &self.tribe;
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

    /// Get upcoming UCSC campus events.
    pub async fn get_upcoming_events(&self, limit: u32) -> Result<Vec<TribeEvent>> {
        self.search_events(None, None, None, Some(limit)).await
    }

    /// Search Eventbrite for events around a location (default: Santa Cruz, CA).
    pub async fn search_eventbrite(
        &self,
        query: Option<&str>,
        location: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<EventbriteEvent>> {
        let limit = limit.unwrap_or(10).min(20);

        let location_slug = location
            .map(|l| eventbrite::location_to_slug(l))
            .unwrap_or_else(|| DEFAULT_LOCATION_SLUG.to_string());

        let cache_key = format!(
            "events:eventbrite:{}:{}:{}",
            query.unwrap_or(""),
            location_slug,
            limit
        );

        let client = &self.eventbrite;
        let location_slug_clone = location_slug.clone();
        let events: Vec<EventbriteEvent> = self
            .cache
            .get_or_fetch(&cache_key, 900, || async {
                client
                    .search_events(query, &location_slug_clone, limit)
                    .await
            })
            .await?;

        Ok(events)
    }
}
