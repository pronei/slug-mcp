pub mod bustime;
pub mod stops;

use std::sync::Arc;

use anyhow::{bail, Result};

use crate::cache::CacheStore;
use stops::Stop;

pub struct TransitService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
    api_key: Option<String>,
}

impl TransitService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>, api_key: Option<String>) -> Self {
        Self {
            http,
            cache,
            api_key,
        }
    }

    /// Get real-time bus arrival predictions for a stop by name.
    pub async fn get_predictions(&self, stop_query: &str, route: Option<&str>) -> Result<String> {
        let api_key = match &self.api_key {
            Some(key) => key.clone(),
            None => {
                return Ok(
                    "BusTime API key not configured. Set the `SLUG_MCP_BUSTIME_KEY` environment variable.\n\
                     Register for developer access at https://rt.scmetro.org".to_string()
                );
            }
        };

        // Load stops (cached for 24h)
        let stops = self.load_stops(&api_key).await?;

        // Search for matching stops
        let matches = stops::search_stops(&stops, stop_query, 5);
        if matches.is_empty() {
            return Ok(format!(
                "No stops found matching \"{}\". Try a different search term (e.g., \"Science Hill\", \"Metro Center\", \"Oakes\").",
                stop_query
            ));
        }

        let best_match = matches[0];

        // Check predictions cache (60s TTL)
        let cache_key = format!(
            "transit:predictions:{}:{}",
            best_match.stop_id,
            route.unwrap_or("")
        );

        if let Some(cached) = self.cache.get(&cache_key).await {
            return Ok(cached);
        }

        // Fetch real-time predictions
        let predictions =
            bustime::get_predictions(&self.http, &api_key, &best_match.stop_id, route).await;

        let output = match predictions {
            Ok(preds) => format_predictions(best_match, &preds, &matches[1..]),
            Err(e) => {
                let err_msg = e.to_string();
                if err_msg.contains("No arrival times") || err_msg.contains("No service") {
                    format!(
                        "No upcoming buses at {} (Stop #{}).\nService may have ended for the day or no buses are currently running on this route.",
                        best_match.stop_name, best_match.stop_id
                    )
                } else {
                    format!(
                        "Could not fetch predictions for {} (Stop #{}): {}",
                        best_match.stop_name, best_match.stop_id, err_msg
                    )
                }
            }
        };

        self.cache.set(&cache_key, &output, 60).await;

        Ok(output)
    }

    async fn load_stops(&self, api_key: &str) -> Result<Vec<Stop>> {
        let cache_key = "transit:bustime:stops";

        if let Some(cached) = self.cache.get(cache_key).await {
            if let Ok(stops) = serde_json::from_str::<Vec<Stop>>(&cached) {
                return Ok(stops);
            }
        }

        let stops = stops::fetch_all_stops(&self.http, api_key)
            .await
            .map_err(|e| {
                anyhow::anyhow!("Failed to load stops from BusTime API: {}", e)
            })?;

        if stops.is_empty() {
            bail!("BusTime API returned no stops — the API may be temporarily unavailable.");
        }

        if let Ok(json) = serde_json::to_string(&stops) {
            self.cache.set(cache_key, &json, 86400).await; // 24h
        }

        Ok(stops)
    }
}

fn format_predictions(
    stop: &Stop,
    predictions: &[bustime::Prediction],
    other_matches: &[&Stop],
) -> String {
    let mut out = format!(
        "Bus arrivals at {} (Stop #{}):\n",
        stop.stop_name, stop.stop_id
    );

    if predictions.is_empty() {
        out.push_str(
            "\nNo upcoming buses at this stop. Service may have ended for the day or no buses are currently running on this route.\n",
        );
    } else {
        // Group predictions by route
        let mut by_route: Vec<(String, Vec<&bustime::Prediction>)> = Vec::new();
        for pred in predictions {
            if let Some((_, preds)) = by_route.iter_mut().find(|(rt, _)| *rt == pred.route) {
                preds.push(pred);
            } else {
                by_route.push((pred.route.clone(), vec![pred]));
            }
        }

        for (route, preds) in &by_route {
            let direction = &preds[0].direction;
            out.push_str(&format!("\nRoute {} -> {}:\n", route, direction));

            for pred in preds {
                let eta_str = if pred.eta_minutes <= 1 {
                    format!("arriving ({})", pred.predicted_time)
                } else {
                    format!("{} min ({})", pred.eta_minutes, pred.predicted_time)
                };

                let delay_marker = if pred.is_delayed { " [delayed]" } else { "" };
                out.push_str(&format!("  - {}{}\n", eta_str, delay_marker));
            }
        }
    }

    let now = chrono::Local::now();
    out.push_str(&format!("\nLast updated: {}\n", now.format("%-I:%M %p")));

    // If there were other stop name matches, mention them
    if !other_matches.is_empty() {
        out.push_str("\nOther matching stops:\n");
        for s in other_matches.iter().take(4) {
            out.push_str(&format!("  - {} (Stop #{})\n", s.stop_name, s.stop_id));
        }
    }

    out
}
