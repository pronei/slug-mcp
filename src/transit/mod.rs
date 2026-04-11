pub mod bustime;
pub mod gtfs_rt;
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
    ///
    /// **Dispatch**: GTFS-RT is the primary source (no per-call key needed,
    /// rich vehicle + occupancy data). If GTFS-RT returns no predictions for
    /// the matched stop — e.g., Metro only published `delay`-based updates
    /// for that trip, or the trip_updates feed hasn't caught up — the
    /// service automatically falls back to the authenticated BusTime API,
    /// which gives pre-computed ETAs, destination headsigns, and DUE/DLY
    /// countdown labels. The stops catalog itself is always loaded via
    /// BusTime (cached 24h) so `SLUG_MCP_BUSTIME_KEY` is required for this
    /// tool to work at all.
    ///
    /// Rollback: to force-switch back to BusTime as the primary backend,
    /// swap the order of the two match arms below (call `bustime_predictions`
    /// first and fall back to GTFS-RT on empty). `bustime.rs` is kept live
    /// for this path.
    pub async fn get_predictions(&self, stop_query: &str, route: Option<&str>) -> Result<String> {
        // Stop lookup comes from BusTime (cached 24h). We need the BusTime
        // key for stop search — graceful degradation if it's missing.
        let api_key = match &self.api_key {
            Some(key) => key.clone(),
            None => {
                return Ok(
                    "BusTime API key not configured for stop lookups. Set the `SLUG_MCP_BUSTIME_KEY` environment variable.\n\
                     (Note: real-time predictions come from GTFS-RT primarily, with BusTime as a fallback for stops where GTFS-RT has no absolute-time data. The stops catalog itself still uses BusTime.)\n\
                     Register for developer access at https://rt.scmetro.org".to_string()
                );
            }
        };

        let stops = self.load_stops(&api_key).await?;
        let matches = stops::search_stops(&stops, stop_query, 5);
        if matches.is_empty() {
            return Ok(format!(
                "No stops found matching \"{}\". Try a different search term (e.g., \"Science Hill\", \"Metro Center\", \"Oakes\").",
                stop_query
            ));
        }

        let best_match = matches[0];
        let other_matches: Vec<&Stop> = matches.iter().skip(1).copied().collect();

        // Primary: GTFS-RT (feed-level cache is 30s inside gtfs_rt).
        let gtfs_result = gtfs_rt::get_predictions_for_stop(
            &self.http,
            &self.cache,
            &best_match.stop_id,
            route,
        )
        .await;

        let output = match gtfs_result {
            Ok(preds) if !preds.is_empty() => {
                gtfs_rt::format_predictions(best_match, &preds, &other_matches)
            }
            Ok(_) => {
                tracing::info!(
                    "GTFS-RT had no predictions for stop {} (route filter {:?}); falling back to BusTime",
                    best_match.stop_id,
                    route
                );
                self.bustime_fallback(
                    &api_key,
                    best_match,
                    route,
                    &other_matches,
                    "GTFS-RT returned no data for this stop",
                )
                .await
            }
            Err(e) => {
                tracing::warn!(
                    "GTFS-RT fetch failed for stop {}: {} (falling back to BusTime)",
                    best_match.stop_id,
                    e
                );
                self.bustime_fallback(
                    &api_key,
                    best_match,
                    route,
                    &other_matches,
                    &format!("GTFS-RT primary feed unreachable ({})", e),
                )
                .await
            }
        };

        Ok(output)
    }

    /// Fetch per-stop predictions from BusTime as a fallback when GTFS-RT has
    /// no data. Formats using the BusTime-specific helper so callers see the
    /// ergonomic extras (DUE/DLY countdown labels, headsigns, canceled trips).
    async fn bustime_fallback(
        &self,
        api_key: &str,
        stop: &Stop,
        route: Option<&str>,
        other_matches: &[&Stop],
        reason: &str,
    ) -> String {
        match bustime::get_predictions(&self.http, api_key, &stop.stop_id, route).await {
            Ok(preds) => format_bustime_predictions(stop, &preds, other_matches, reason),
            Err(e) => format!(
                "Could not fetch predictions for {} (Stop #{}):\n  - {}\n  - BusTime fallback also failed: {}",
                stop.stop_name, stop.stop_id, reason, e
            ),
        }
    }

    /// System-wide Santa Cruz Metro service alerts via GTFS-RT (no API key).
    pub async fn get_system_alerts(&self) -> Result<String> {
        match gtfs_rt::fetch_system_alerts(&self.http, &self.cache).await {
            Ok(alerts) => Ok(gtfs_rt::format_system_alerts(&alerts)),
            Err(e) => {
                tracing::warn!("GTFS-RT system alerts fetch failed: {}", e);
                Ok(format!(
                    "⚠ GTFS-RT alerts feed temporarily unreachable. Try again in a minute.\n(details: {})",
                    e
                ))
            }
        }
    }

    /// Live vehicle positions from GTFS-RT, optionally filtered by route.
    pub async fn get_vehicle_positions(&self, route: Option<&str>) -> Result<String> {
        match gtfs_rt::fetch_vehicle_positions(&self.http, &self.cache, route).await {
            Ok(positions) => Ok(gtfs_rt::format_vehicle_positions(&positions, route)),
            Err(e) => {
                tracing::warn!("GTFS-RT vehicle positions fetch failed: {}", e);
                Ok(format!(
                    "⚠ GTFS-RT vehicles feed temporarily unreachable. Try again in a minute.\n(details: {})",
                    e
                ))
            }
        }
    }

    /// Aggregated per-route delays from GTFS-RT trip_updates.
    pub async fn get_route_delays(&self, route: Option<&str>) -> Result<String> {
        match gtfs_rt::fetch_route_delays(&self.http, &self.cache, route).await {
            Ok(stats) => Ok(gtfs_rt::format_route_delays(&stats, route)),
            Err(e) => {
                tracing::warn!("GTFS-RT route delays fetch failed: {}", e);
                Ok(format!(
                    "⚠ GTFS-RT trip_updates feed temporarily unreachable. Try again in a minute.\n(details: {})",
                    e
                ))
            }
        }
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

/// Render BusTime predictions — used only when GTFS-RT had no data and the
/// service fell back to BusTime. Surfaces the ergonomic extras BusTime
/// provides (DUE/DLY countdown labels, destination headsigns, canceled or
/// expressed trip flags) that GTFS-RT can't match without a static GTFS
/// schedule loader.
fn format_bustime_predictions(
    stop: &Stop,
    predictions: &[bustime::Prediction],
    other_matches: &[&Stop],
    reason: &str,
) -> String {
    let mut out = format!(
        "Bus arrivals at {} (Stop #{}):\n_{}; showing BusTime pre-computed ETAs._\n",
        stop.stop_name, stop.stop_id, reason
    );

    if predictions.is_empty() {
        out.push_str(
            "\nNo upcoming buses from either GTFS-RT or BusTime. Service may have ended for the day or no buses are currently running.\n",
        );
    } else {
        let mut by_route: Vec<(String, Vec<&bustime::Prediction>)> = Vec::new();
        for pred in predictions {
            if let Some((_, preds)) = by_route.iter_mut().find(|(rt, _)| *rt == pred.route) {
                preds.push(pred);
            } else {
                by_route.push((pred.route.clone(), vec![pred]));
            }
        }

        for (route, preds) in &by_route {
            let dir = &preds[0].direction;
            let dest = &preds[0].destination;
            if dest.is_empty() {
                out.push_str(&format!("\nRoute {} ({}):\n", route, dir));
            } else {
                out.push_str(&format!("\nRoute {} ({} → {}):\n", route, dir, dest));
            }

            for p in preds {
                let eta_str = match p.countdown.as_str() {
                    "DUE" => format!("DUE ({})", p.predicted_time),
                    "DLY" => format!("delayed ({})", p.predicted_time),
                    _ if p.eta_minutes <= 1 => format!("arriving ({})", p.predicted_time),
                    _ => format!("{} min ({})", p.eta_minutes, p.predicted_time),
                };

                let mut markers: Vec<String> = Vec::new();
                if p.is_delayed && p.countdown != "DLY" {
                    markers.push("delayed".to_string());
                }
                match p.trip_status {
                    bustime::TripStatus::Canceled => markers.push("CANCELED".to_string()),
                    bustime::TripStatus::Expressed => markers.push("express".to_string()),
                    bustime::TripStatus::Normal => {}
                }
                if let Some(load) = &p.passenger_load {
                    markers.push(format!("load: {}", friendly_load(load)));
                }
                if let Some(next) = &p.next_bus_minutes {
                    markers.push(format!("next in {} min", next));
                }

                let marker_str = if markers.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", markers.join(", "))
                };

                let mut line = format!("  - {}{}", eta_str, marker_str);
                if !p.vehicle_id.is_empty() {
                    line.push_str(&format!(" (bus #{})", p.vehicle_id));
                }
                out.push_str(&line);
                out.push('\n');
            }
        }
    }

    let now = chrono::Local::now();
    out.push_str(&format!(
        "\nLast updated: {} · source: BusTime (fallback)\n",
        now.format("%-I:%M %p")
    ));

    if !other_matches.is_empty() {
        out.push_str("\nOther matching stops:\n");
        for s in other_matches.iter().take(4) {
            out.push_str(&format!("  - {} (Stop #{})\n", s.stop_name, s.stop_id));
        }
    }

    out
}

/// Translate BusTime's shouty occupancy enum to a short human label.
fn friendly_load(raw: &str) -> &str {
    match raw {
        "EMPTY" => "empty",
        "HALF_EMPTY" => "half-empty",
        "FULL" => "full",
        "STANDING_ROOM_ONLY" => "standing room",
        "NOT_ACCEPTING_PASSENGERS" => "not accepting",
        other => other,
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
