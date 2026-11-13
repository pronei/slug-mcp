pub mod eventbrite;
pub mod tribe;

use std::sync::Arc;

use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::cache::CacheStore;
use eventbrite::{Event as EventbriteEvent, EventbriteClient};
use tribe::{TribeClient, TribeEvent};

/// Default location for Eventbrite searches (Eventbrite URL slug format).
const DEFAULT_LOCATION_SLUG: &str = "ca--santa-cruz";

/// Reject a category the events API doesn't know, so the caller gets the valid
/// slugs back instead of an opaque upstream 400. An empty `slugs` means the
/// catalog fetch failed — don't block the search on it.
fn validate_event_category(category: &str, slugs: &[String]) -> Result<()> {
    if slugs.is_empty() || slugs.iter().any(|s| s == category) {
        return Ok(());
    }
    Err(crate::util::InvalidInput::err(format!(
        "Unknown event category '{}'. Valid categories: {}",
        category,
        slugs.join(", ")
    )))
}

// Tribe's default (and its `start_date`) is day-granular, so by evening the
// list leads with events that ended that morning. These filters take a time.
#[derive(Clone, Copy)]
enum Window {
    /// Starts after now: what `get_upcoming_events` promises.
    StartsLater,
    /// Hasn't ended yet, so a search still finds an exhibit that's running.
    NotYetEnded,
}

impl Window {
    fn param(self) -> &'static str {
        match self {
            Window::StartsLater => "starts_after",
            Window::NotYetEnded => "ends_after",
        }
    }
}

/// Tribe compares against the site's local (Pacific) wall-clock time.
fn tribe_timestamp(now: chrono::DateTime<chrono_tz::Tz>) -> String {
    now.format("%Y-%m-%d %H:%M:%S").to_string()
}

// ─── Request types ───

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchEventsRequest {
    /// Search query string
    pub query: Option<String>,
    /// Event category slug (e.g., "workshop", "lectures-presentations",
    /// "seminars", "exhibits"). Must match an events.ucsc.edu category slug.
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
        category: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<TribeEvent>> {
        self.fetch_tribe(query, category, limit, Window::NotYetEnded)
            .await
    }

    async fn fetch_tribe(
        &self,
        query: Option<&str>,
        category: Option<&str>,
        limit: Option<u32>,
        window: Window,
    ) -> Result<Vec<TribeEvent>> {
        let limit = limit.unwrap_or(10).min(50);
        let limit_str = limit.to_string();

        let cache_key = format!(
            "events:tribe:{}:{}:{}:{}",
            window.param(),
            query.unwrap_or(""),
            category.unwrap_or(""),
            limit
        );

        let client = &self.tribe;

        if let Some(cat) = category {
            // Category slugs are near-static; a failed catalog fetch degrades to
            // no validation rather than blocking the search.
            let slugs: Vec<String> = self
                .cache
                .get_or_fetch("events:tribe:category-slugs", 21_600, || async {
                    client.fetch_category_slugs().await
                })
                .await
                .unwrap_or_default();
            validate_event_category(cat, &slugs)?;
        }

        let events: Vec<TribeEvent> = self
            .cache
            .get_or_fetch(&cache_key, 900, || async {
                let now = tribe_timestamp(crate::util::now_pacific());
                let mut params: Vec<(&str, &str)> =
                    vec![("per_page", &limit_str), (window.param(), &now)];
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
        self.fetch_tribe(None, None, Some(limit), Window::StartsLater)
            .await
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
            .map(eventbrite::location_to_slug)
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn timestamp_is_pacific_wall_clock() {
        // 05:45 UTC on Nov 13 is 21:45 the evening before in Santa Cruz.
        let utc = chrono::Utc
            .with_ymd_and_hms(2026, 11, 13, 5, 45, 23)
            .unwrap();
        let pacific = utc.with_timezone(&chrono_tz::US::Pacific);
        assert_eq!(tribe_timestamp(pacific), "2026-11-12 21:45:23");
    }

    #[test]
    fn upcoming_and_search_use_time_aware_filters() {
        assert_eq!(Window::StartsLater.param(), "starts_after");
        assert_eq!(Window::NotYetEnded.param(), "ends_after");
    }

    fn slugs() -> Vec<String> {
        ["workshop", "lectures-presentations", "seminars", "exhibits"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn known_slug_is_accepted() {
        assert!(validate_event_category("workshop", &slugs()).is_ok());
        assert!(validate_event_category("lectures-presentations", &slugs()).is_ok());
    }

    #[test]
    fn unknown_slug_is_rejected_with_the_valid_list() {
        // "lecture" was the tool's own documented example; upstream 400s on it.
        let err = validate_event_category("lecture", &slugs()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("lecture"), "{msg}");
        assert!(msg.contains("lectures-presentations"), "{msg}");
        assert!(
            err.downcast_ref::<crate::util::InvalidInput>().is_some(),
            "not typed as a caller error: {msg}"
        );
    }

    #[test]
    fn empty_catalog_does_not_block_the_search() {
        assert!(validate_event_category("anything", &[]).is_ok());
    }
}
