pub mod bustime;
pub mod stops;

use std::sync::Arc;

use anyhow::{bail, Result};

use crate::cache::CacheStore;
use stops::Stop;

pub(crate) const BUSTIME_BASE_URL: &str = "https://rt.scmetro.org/bustime/api/v2";

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

    /// Get service alerts for a route or stop.
    pub async fn get_service_alerts(
        &self,
        route: Option<&str>,
        stop_id: Option<&str>,
    ) -> Result<String> {
        let api_key = match &self.api_key {
            Some(key) => key.clone(),
            None => {
                return Ok(
                    "BusTime API key not configured. Set the `SLUG_MCP_BUSTIME_KEY` environment variable.\n\
                     Register for developer access at https://rt.scmetro.org".to_string()
                );
            }
        };

        if route.is_none() && stop_id.is_none() {
            return Ok(
                "Please specify a route number or stop ID to check service alerts for.".to_string(),
            );
        }

        let cache_key = format!(
            "transit:alerts:{}:{}",
            route.unwrap_or(""),
            stop_id.unwrap_or("")
        );

        if let Some(cached) = self.cache.get(&cache_key).await {
            return Ok(cached);
        }

        let bulletins =
            bustime::get_service_bulletins(&self.http, &api_key, route, stop_id).await;

        let output = match bulletins {
            Ok(b) => format_service_bulletins(&b),
            Err(e) => {
                let msg = e.to_string();
                // "No data found for parameter" means zero bulletins when the
                // route/stop is known-valid. But we can't distinguish that from
                // a genuinely bad parameter here, so surface it transparently.
                if msg.contains("No data found") {
                    format!(
                        "No active service alerts for {}.\n\
                         (If this route/stop seems wrong, double-check the identifier.)",
                        route
                            .map(|r| format!("route {}", r))
                            .or_else(|| stop_id.map(|s| format!("stop {}", s)))
                            .unwrap_or_default()
                    )
                } else {
                    format!("Could not fetch service alerts: {}", msg)
                }
            }
        };

        self.cache.set(&cache_key, &output, 300).await;

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
            let dest = &preds[0].destination;
            if !dest.is_empty() && dest != direction {
                out.push_str(&format!(
                    "\nRoute {} -> {} (to {}):\n",
                    route, direction, dest
                ));
            } else {
                out.push_str(&format!("\nRoute {} -> {}:\n", route, direction));
            }

            for pred in preds {
                let eta_str = if pred.countdown == "DUE" || pred.eta_minutes <= 1 {
                    format!("arriving ({})", pred.predicted_time)
                } else {
                    format!("{} min ({})", pred.eta_minutes, pred.predicted_time)
                };

                let mut markers = Vec::new();
                if pred.trip_status == bustime::TripStatus::Canceled {
                    markers.push("CANCELED".to_string());
                } else if pred.trip_status == bustime::TripStatus::Expressed {
                    markers.push("express".to_string());
                }
                if pred.is_delayed {
                    markers.push("delayed".to_string());
                }
                if let Some(ref load) = pred.passenger_load {
                    markers.push(format!("load: {}", friendly_load(load)));
                }

                let marker_str = if markers.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", markers.join(", "))
                };

                let mut line = format!("  - {}{}", eta_str, marker_str);

                if let Some(ref next) = pred.next_bus_minutes {
                    line.push_str(&format!(" | next in {} min", next));
                }

                if !pred.vehicle_id.is_empty() {
                    line.push_str(&format!(" (bus #{})", pred.vehicle_id));
                }

                out.push_str(&line);
                out.push('\n');
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

fn friendly_load(raw: &str) -> &str {
    match raw {
        "EMPTY" | "E" => "empty",
        "HALF_EMPTY" | "H" => "seats available",
        "FULL" | "F" => "standing room",
        _ => raw,
    }
}

fn format_service_bulletins(bulletins: &[bustime::ServiceBulletin]) -> String {
    if bulletins.is_empty() {
        return "No active service alerts.".to_string();
    }

    let mut out = format!("# Service Alerts ({})\n\n", bulletins.len());

    for b in bulletins {
        if !b.priority.is_empty() {
            out.push_str(&format!("**[{}]** {}\n", b.priority, b.subject));
        } else {
            out.push_str(&format!("**{}**\n", b.subject));
        }
        if !b.brief.is_empty() {
            out.push_str(&format!("{}\n", b.brief));
        }
        if !b.detail.is_empty() && b.detail != b.brief {
            out.push_str(&format!("{}\n", b.detail));
        }
        if !b.affected_routes.is_empty() {
            out.push_str(&format!(
                "- Affects routes: {}\n",
                b.affected_routes.join(", ")
            ));
        }
        out.push('\n');
    }

    let now = chrono::Local::now();
    out.push_str(&format!("Last checked: {}\n", now.format("%-I:%M %p")));

    out
}
