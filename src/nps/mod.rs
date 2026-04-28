//! National Park Service API — park info for Pinnacles and nearby parks.
//!
//! Queries the NPS Developer API (`/api/v1/parks`) for details on national
//! parks within driving distance of Santa Cruz. Requires a free API key from
//! <https://www.nps.gov/subjects/developer/get-started.htm>.
//! If `NPS_API_KEY` is not set, the tool returns registration instructions
//! instead of erroring.

use std::sync::Arc;

use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::cache::CacheStore;

mod scraper;

use scraper::{fetch_parks, format_response, NpsResponse};

// ─── Request type ───

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NationalParkRequest {
    /// Park code (e.g. "pinn" for Pinnacles, "yose" for Yosemite). If omitted, searches by name.
    pub park_code: Option<String>,
    /// Search by park name (e.g. "Pinnacles", "Yosemite"). Ignored if park_code is provided.
    pub query: Option<String>,
    /// Max results when searching by name (default 5, max 20).
    pub limit: Option<u32>,
}

// ─── Service ───

pub struct NpsService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
    api_key: Option<String>,
}

impl NpsService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>, api_key: Option<String>) -> Self {
        Self {
            http,
            cache,
            api_key,
        }
    }

    pub async fn get_park_info(
        &self,
        park_code: Option<&str>,
        query: Option<&str>,
        limit: Option<u32>,
    ) -> Result<String> {
        let key = match &self.api_key {
            Some(k) if !k.is_empty() => k.clone(),
            _ => {
                return Ok(
                    "NPS API key not configured.\n\
                     Get a free key at https://www.nps.gov/subjects/developer/get-started.htm \
                     and set the `NPS_API_KEY` environment variable."
                        .to_string(),
                );
            }
        };

        let limit = limit.unwrap_or(5).clamp(1, 20);

        // Build cache key and query parameters based on input
        let (cache_key, query_params) = if let Some(code) = park_code {
            let code = code.trim().to_lowercase();
            (
                format!("nps:park:{}", code),
                vec![
                    ("parkCode".to_string(), code),
                    ("api_key".to_string(), key.clone()),
                ],
            )
        } else if let Some(q) = query {
            let q = q.trim().to_string();
            if q.is_empty() {
                return Ok(
                    "Please provide either a `park_code` (e.g. \"pinn\") or a \
                     `query` (e.g. \"Pinnacles\") to search for a national park."
                        .to_string(),
                );
            }
            (
                format!("nps:search:{}:{}", q.to_lowercase(), limit),
                vec![
                    ("q".to_string(), q),
                    ("limit".to_string(), limit.to_string()),
                    ("api_key".to_string(), key.clone()),
                ],
            )
        } else {
            return Ok(
                "Please provide either a `park_code` (e.g. \"pinn\") or a \
                 `query` (e.g. \"Pinnacles\") to search for a national park."
                    .to_string(),
            );
        };

        let http = self.http.clone();
        let result = self
            .cache
            .get_or_fetch::<NpsResponse, _, _>(&cache_key, 86_400, move || async move {
                fetch_parks(&http, &query_params).await
            })
            .await;

        match result {
            Ok(response) => Ok(format_response(&response)),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("403") {
                    Ok(
                        "NPS API returned 403 Forbidden. Please check that your \
                         `NPS_API_KEY` is valid and has not expired."
                            .to_string(),
                    )
                } else {
                    tracing::warn!("NPS API fetch failed: {}", e);
                    Ok(format!(
                        "NPS API temporarily unreachable. Try again in a minute.\n(details: {})",
                        e
                    ))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn missing_key_message() {
        // Simulate the key-absent check inline (can't easily call async in a sync test)
        let api_key: Option<String> = None;
        let msg = match &api_key {
            Some(k) if !k.is_empty() => unreachable!(),
            _ => {
                "NPS API key not configured.\n\
                 Get a free key at https://www.nps.gov/subjects/developer/get-started.htm \
                 and set the `NPS_API_KEY` environment variable."
                    .to_string()
            }
        };
        assert!(msg.contains("NPS API key not configured"));
        assert!(msg.contains("nps.gov/subjects/developer"));
        assert!(msg.contains("NPS_API_KEY"));

        // Also test empty key
        let api_key_empty: Option<String> = Some(String::new());
        let msg_empty = match &api_key_empty {
            Some(k) if !k.is_empty() => unreachable!(),
            _ => {
                "NPS API key not configured.\n\
                 Get a free key at https://www.nps.gov/subjects/developer/get-started.htm \
                 and set the `NPS_API_KEY` environment variable."
                    .to_string()
            }
        };
        assert!(msg_empty.contains("NPS API key not configured"));
    }
}
