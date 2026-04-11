//! GTFS-Realtime protobuf client for Santa Cruz Metro.
//!
//! Three public feeds, no auth required:
//! - Trip updates: <https://rt.scmetro.org/gtfsrt/trips>
//! - Vehicle positions: <https://rt.scmetro.org/gtfsrt/vehicles>
//! - Service alerts: <https://rt.scmetro.org/gtfsrt/alerts>
//!
//! **Not gRPC.** GTFS-RT is a static Protocol Buffer document served over
//! plain HTTP GET. We fetch the bytes, decode via `prost::Message::decode`,
//! and store the raw bytes (base64-encoded) in the shared `CacheStore` for a
//! short TTL so concurrent tool calls share a single fetch.
//!
//! Backend choice: since 2026-04-10 this module powers `get_bus_predictions`
//! instead of the authenticated BusTime API. The `bustime.rs` module is left
//! in place for rollback — revert the single function call in
//! `TransitService::get_predictions` if GTFS-RT predictions prove less
//! accurate than BusTime's pre-computed ETAs.

use std::collections::HashMap;

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use prost::Message;
use serde::{Deserialize, Serialize};

use crate::cache::CacheStore;
use crate::transit::stops::Stop;

pub const TRIPS_URL: &str = "https://rt.scmetro.org/gtfsrt/trips";
pub const VEHICLES_URL: &str = "https://rt.scmetro.org/gtfsrt/vehicles";
pub const ALERTS_URL: &str = "https://rt.scmetro.org/gtfsrt/alerts";

/// Per-feed cache TTL in seconds. Real-time feeds update every ~30s.
const FEED_TTL_SECS: u64 = 30;

/// Friendly backend name surfaced to the user.
pub const BACKEND_NAME: &str = "GTFS-RT (no API key)";

// ───── feed fetching (shared by all callers) ─────

/// Fetch a feed URL, caching the raw protobuf bytes (base64-encoded) in the
/// shared `CacheStore` so concurrent tool calls reuse a single upstream GET.
pub async fn fetch_feed(
    http: &reqwest::Client,
    cache: &CacheStore,
    feed_name: &'static str,
    url: &'static str,
) -> Result<gtfs_realtime::FeedMessage> {
    let cache_key = format!("transit:gtfsrt:{}", feed_name);
    if let Some(encoded) = cache.get(&cache_key).await {
        if let Ok(bytes) = STANDARD.decode(&encoded) {
            if let Ok(msg) = gtfs_realtime::FeedMessage::decode(&bytes[..]) {
                return Ok(msg);
            }
            tracing::warn!("cached GTFS-RT {} failed to decode; refetching", feed_name);
        }
    }

    let resp = http
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {}", url))?;
    if !resp.status().is_success() {
        anyhow::bail!("GTFS-RT {} returned HTTP {}", feed_name, resp.status());
    }
    let bytes = resp
        .bytes()
        .await
        .with_context(|| format!("reading {} body", feed_name))?;

    cache
        .set(&cache_key, &STANDARD.encode(&bytes), FEED_TTL_SECS)
        .await;

    gtfs_realtime::FeedMessage::decode(&bytes[..])
        .with_context(|| format!("decoding {} protobuf", feed_name))
}

// ───── predictions (replacement for bustime::get_predictions) ─────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prediction {
    pub route: String,
    pub direction: Option<u32>,
    pub eta_minutes: i64,
    pub predicted_time: String,
    pub is_delayed: bool,
    pub vehicle_id: String,
    pub passenger_load: Option<String>,
    pub trip_id: String,
}

/// Get predictions for a specific stop_id by decoding trip_updates and
/// joining with vehicle_positions for occupancy.
pub async fn get_predictions_for_stop(
    http: &reqwest::Client,
    cache: &CacheStore,
    stop_id: &str,
    route_filter: Option<&str>,
) -> Result<Vec<Prediction>> {
    let (trips_res, vehicles_res) = futures_util::future::join(
        fetch_feed(http, cache, "trips", TRIPS_URL),
        fetch_feed(http, cache, "vehicles", VEHICLES_URL),
    )
    .await;

    let trips = trips_res?;
    // Vehicles feed is optional — if it fails we still render predictions
    // without occupancy info.
    let vehicles_lookup = match vehicles_res {
        Ok(v) => build_vehicle_lookup(&v),
        Err(e) => {
            tracing::warn!("GTFS-RT vehicles feed failed (rendering without occupancy): {}", e);
            HashMap::new()
        }
    };

    let now = chrono::Local::now();
    let now_epoch = now.timestamp();

    let mut predictions: Vec<Prediction> = Vec::new();
    for entity in &trips.entity {
        let Some(trip_update) = &entity.trip_update else {
            continue;
        };
        let route_id = trip_update
            .trip
            .route_id
            .clone()
            .unwrap_or_default();
        if let Some(filter) = route_filter {
            if !route_id.eq_ignore_ascii_case(filter) {
                continue;
            }
        }

        for stu in &trip_update.stop_time_update {
            if stu.stop_id.as_deref() != Some(stop_id) {
                continue;
            }

            // Prefer arrival.time, then departure.time (absolute Unix
            // timestamps). If only delay is available, skip — we'd need the
            // static GTFS schedule to convert to an absolute ETA (v2 work).
            let time = stu
                .arrival
                .as_ref()
                .and_then(|a| a.time)
                .or_else(|| stu.departure.as_ref().and_then(|d| d.time));

            let delay_secs = stu
                .arrival
                .as_ref()
                .and_then(|a| a.delay)
                .or_else(|| stu.departure.as_ref().and_then(|d| d.delay));

            let Some(time) = time else {
                if delay_secs.is_some() {
                    tracing::debug!(
                        "GTFS-RT stop_time_update for stop {} only has delay (no absolute time); skipping until static GTFS loader ships",
                        stop_id
                    );
                }
                continue;
            };

            // Skip predictions that are in the past (already left the stop).
            if time < now_epoch - 60 {
                continue;
            }

            let eta_minutes = ((time - now_epoch) / 60).max(0);
            let predicted_time = chrono::DateTime::<chrono::Local>::from(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(time as u64),
            )
            .format("%-I:%M %p")
            .to_string();

            let is_delayed = delay_secs.map(|d| d > 60).unwrap_or(false);

            let vehicle_id = trip_update
                .vehicle
                .as_ref()
                .and_then(|v| v.id.clone())
                .unwrap_or_default();

            let passenger_load = vehicle_id
                .get(..)
                .and_then(|id| vehicles_lookup.get(id).and_then(|v| v.occupancy.clone()));

            predictions.push(Prediction {
                route: route_id.clone(),
                direction: trip_update.trip.direction_id,
                eta_minutes,
                predicted_time,
                is_delayed,
                vehicle_id,
                passenger_load,
                trip_id: trip_update.trip.trip_id.clone().unwrap_or_default(),
            });
        }
    }

    // Sort by ETA ascending
    predictions.sort_by_key(|p| p.eta_minutes);
    Ok(predictions)
}

struct VehicleInfo {
    occupancy: Option<String>,
}

fn build_vehicle_lookup(feed: &gtfs_realtime::FeedMessage) -> HashMap<String, VehicleInfo> {
    let mut out = HashMap::new();
    for entity in &feed.entity {
        let Some(vp) = &entity.vehicle else { continue };
        let Some(vehicle) = &vp.vehicle else { continue };
        let Some(id) = &vehicle.id else { continue };

        let occupancy = vp.occupancy_status.map(|code| occupancy_label(code).to_string());

        out.insert(id.clone(), VehicleInfo { occupancy });
    }
    out
}

fn occupancy_label(code: i32) -> &'static str {
    // Matches gtfs_realtime::vehicle_position::OccupancyStatus enum values.
    match code {
        0 => "empty",
        1 => "many seats",
        2 => "few seats",
        3 => "standing room",
        4 => "crushed",
        5 => "full",
        6 => "not accepting",
        _ => "unknown",
    }
}

// ───── system-wide service alerts (GTFS-RT alerts feed) ─────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemAlert {
    pub header: String,
    pub description: String,
    pub cause: String,
    pub effect: String,
    pub routes_affected: Vec<String>,
    pub stops_affected: Vec<String>,
    pub active_from: Option<i64>,
    pub active_until: Option<i64>,
    pub url: Option<String>,
}

pub async fn fetch_system_alerts(
    http: &reqwest::Client,
    cache: &CacheStore,
) -> Result<Vec<SystemAlert>> {
    let feed = fetch_feed(http, cache, "alerts", ALERTS_URL).await?;
    Ok(feed
        .entity
        .iter()
        .filter_map(|e| {
            let alert = e.alert.as_ref()?;
            Some(SystemAlert {
                header: translated_text(alert.header_text.as_ref()).unwrap_or_default(),
                description: translated_text(alert.description_text.as_ref()).unwrap_or_default(),
                cause: cause_label(alert.cause).to_string(),
                effect: effect_label(alert.effect).to_string(),
                routes_affected: alert
                    .informed_entity
                    .iter()
                    .filter_map(|e| e.route_id.clone())
                    .collect(),
                stops_affected: alert
                    .informed_entity
                    .iter()
                    .filter_map(|e| e.stop_id.clone())
                    .collect(),
                active_from: alert
                    .active_period
                    .first()
                    .and_then(|p| p.start)
                    .map(|s| s as i64),
                active_until: alert
                    .active_period
                    .first()
                    .and_then(|p| p.end)
                    .map(|e| e as i64),
                url: translated_text(alert.url.as_ref()),
            })
        })
        .collect())
}

fn translated_text(ts: Option<&gtfs_realtime::TranslatedString>) -> Option<String> {
    let ts = ts?;
    // Prefer English, fall back to first available.
    ts.translation
        .iter()
        .find(|t| t.language.as_deref().unwrap_or("").starts_with("en"))
        .or_else(|| ts.translation.first())
        .map(|t| t.text.clone())
}

fn cause_label(code: Option<i32>) -> &'static str {
    match code {
        Some(1) => "unknown",
        Some(2) => "other",
        Some(3) => "technical",
        Some(4) => "strike",
        Some(5) => "demonstration",
        Some(6) => "accident",
        Some(7) => "holiday",
        Some(8) => "weather",
        Some(9) => "maintenance",
        Some(10) => "construction",
        Some(11) => "police",
        Some(12) => "medical",
        _ => "—",
    }
}

fn effect_label(code: Option<i32>) -> &'static str {
    match code {
        Some(1) => "no service",
        Some(2) => "reduced service",
        Some(3) => "significant delays",
        Some(4) => "detour",
        Some(5) => "additional service",
        Some(6) => "modified service",
        Some(7) => "other effect",
        Some(8) => "unknown",
        Some(9) => "stop moved",
        Some(10) => "no effect",
        Some(11) => "accessibility issue",
        _ => "—",
    }
}

// ───── vehicle positions (system-wide) ─────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VehicleSnapshot {
    pub vehicle_id: String,
    pub vehicle_label: String,
    pub route: String,
    pub trip_id: String,
    pub latitude: f32,
    pub longitude: f32,
    pub bearing: Option<f32>,
    pub speed: Option<f32>,
    pub occupancy: Option<String>,
    pub timestamp: Option<i64>,
}

pub async fn fetch_vehicle_positions(
    http: &reqwest::Client,
    cache: &CacheStore,
    route_filter: Option<&str>,
) -> Result<Vec<VehicleSnapshot>> {
    let feed = fetch_feed(http, cache, "vehicles", VEHICLES_URL).await?;
    let mut out = Vec::new();
    for entity in &feed.entity {
        let Some(vp) = &entity.vehicle else { continue };
        let position = match &vp.position {
            Some(p) => p,
            None => continue,
        };
        let route = vp
            .trip
            .as_ref()
            .and_then(|t| t.route_id.clone())
            .unwrap_or_default();

        if let Some(filter) = route_filter {
            if !route.eq_ignore_ascii_case(filter) {
                continue;
            }
        }

        let vehicle_desc = vp.vehicle.as_ref();
        out.push(VehicleSnapshot {
            vehicle_id: vehicle_desc.and_then(|v| v.id.clone()).unwrap_or_default(),
            vehicle_label: vehicle_desc
                .and_then(|v| v.label.clone())
                .unwrap_or_default(),
            route,
            trip_id: vp
                .trip
                .as_ref()
                .and_then(|t| t.trip_id.clone())
                .unwrap_or_default(),
            latitude: position.latitude,
            longitude: position.longitude,
            bearing: position.bearing,
            speed: position.speed,
            occupancy: vp.occupancy_status.map(|c| occupancy_label(c).to_string()),
            timestamp: vp.timestamp.map(|t| t as i64),
        });
    }
    Ok(out)
}

// ───── route delays (aggregated from trip updates) ─────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteDelayStats {
    pub route: String,
    pub active_trips: usize,
    pub avg_delay_seconds: i32,
    pub max_delay_seconds: i32,
}

pub async fn fetch_route_delays(
    http: &reqwest::Client,
    cache: &CacheStore,
    route_filter: Option<&str>,
) -> Result<Vec<RouteDelayStats>> {
    let feed = fetch_feed(http, cache, "trips", TRIPS_URL).await?;

    let mut by_route: HashMap<String, Vec<i32>> = HashMap::new();
    for entity in &feed.entity {
        let Some(tu) = &entity.trip_update else { continue };
        let route = tu.trip.route_id.clone().unwrap_or_default();
        if let Some(filter) = route_filter {
            if !route.eq_ignore_ascii_case(filter) {
                continue;
            }
        }

        // Pick the most-forward stop_time_update delay that we have, or the
        // overall `delay` field if present.
        let latest_delay = tu.delay.or_else(|| {
            tu.stop_time_update
                .iter()
                .rev()
                .find_map(|s| s.arrival.as_ref().and_then(|a| a.delay))
        });
        if let Some(d) = latest_delay {
            by_route.entry(route).or_default().push(d);
        }
    }

    let mut out: Vec<RouteDelayStats> = by_route
        .into_iter()
        .map(|(route, delays)| {
            let active_trips = delays.len();
            let sum: i32 = delays.iter().sum();
            let avg = if active_trips > 0 {
                sum / active_trips as i32
            } else {
                0
            };
            let max = delays.iter().copied().max().unwrap_or(0);
            RouteDelayStats {
                route,
                active_trips,
                avg_delay_seconds: avg,
                max_delay_seconds: max,
            }
        })
        .collect();
    out.sort_by(|a, b| b.avg_delay_seconds.cmp(&a.avg_delay_seconds));
    Ok(out)
}

// ───── formatting helpers used by TransitService ─────

pub fn format_predictions(
    stop: &Stop,
    predictions: &[Prediction],
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
        let mut by_route: Vec<(String, Vec<&Prediction>)> = Vec::new();
        for pred in predictions {
            if let Some((_, preds)) = by_route.iter_mut().find(|(rt, _)| *rt == pred.route) {
                preds.push(pred);
            } else {
                by_route.push((pred.route.clone(), vec![pred]));
            }
        }

        for (route, preds) in &by_route {
            let direction_label = preds[0]
                .direction
                .map(|d| format!(" (dir {})", d))
                .unwrap_or_default();
            out.push_str(&format!("\nRoute {}{}:\n", route, direction_label));

            for pred in preds {
                let eta_str = if pred.eta_minutes <= 1 {
                    format!("arriving ({})", pred.predicted_time)
                } else {
                    format!("{} min ({})", pred.eta_minutes, pred.predicted_time)
                };

                let mut markers = Vec::new();
                if pred.is_delayed {
                    markers.push("delayed".to_string());
                }
                if let Some(load) = &pred.passenger_load {
                    markers.push(format!("load: {}", load));
                }
                let marker_str = if markers.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", markers.join(", "))
                };

                let mut line = format!("  - {}{}", eta_str, marker_str);
                if !pred.vehicle_id.is_empty() {
                    line.push_str(&format!(" (bus #{})", pred.vehicle_id));
                }
                out.push_str(&line);
                out.push('\n');
            }
        }
    }

    let now = chrono::Local::now();
    out.push_str(&format!(
        "\nLast updated: {} · source: {}\n",
        now.format("%-I:%M %p"),
        BACKEND_NAME
    ));

    if !other_matches.is_empty() {
        out.push_str("\nOther matching stops:\n");
        for s in other_matches.iter().take(4) {
            out.push_str(&format!("  - {} (Stop #{})\n", s.stop_name, s.stop_id));
        }
    }

    out
}

pub fn format_system_alerts(alerts: &[SystemAlert]) -> String {
    if alerts.is_empty() {
        return format!(
            "# Santa Cruz Metro — System Service Alerts\n\n\
             No active system alerts.\n\n\
             _Source: METRO GTFS-RT. Last checked: {}_\n",
            chrono::Local::now().format("%-I:%M %p")
        );
    }
    let mut out = format!(
        "# Santa Cruz Metro — System Service Alerts ({} active)\n\n",
        alerts.len()
    );
    for a in alerts {
        if !a.header.is_empty() {
            out.push_str(&format!("**{}**\n", a.header));
        }
        if !a.description.is_empty() {
            out.push_str(&format!("{}\n", a.description));
        }
        if !a.routes_affected.is_empty() {
            out.push_str(&format!(
                "- Routes: {}\n",
                a.routes_affected.join(", ")
            ));
        }
        if !a.stops_affected.is_empty() && a.stops_affected.len() <= 8 {
            out.push_str(&format!("- Stops: {}\n", a.stops_affected.join(", ")));
        }
        if a.cause != "—" && a.cause != "unknown" {
            out.push_str(&format!("- Cause: {}\n", a.cause));
        }
        if a.effect != "—" {
            out.push_str(&format!("- Effect: {}\n", a.effect));
        }
        if let Some(url) = &a.url {
            if !url.is_empty() {
                out.push_str(&format!("- More info: {}\n", url));
            }
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "_Source: METRO GTFS-RT. Last updated: {}_\n",
        chrono::Local::now().format("%-I:%M %p")
    ));
    out
}

pub fn format_vehicle_positions(
    positions: &[VehicleSnapshot],
    route_filter: Option<&str>,
) -> String {
    let mut out = if let Some(r) = route_filter {
        format!(
            "# Santa Cruz Metro — Live vehicles on route {} ({} active)\n\n",
            r,
            positions.len()
        )
    } else {
        format!(
            "# Santa Cruz Metro — Live vehicles system-wide ({} active)\n\n",
            positions.len()
        )
    };

    if positions.is_empty() {
        out.push_str("No active vehicles reporting position.\n");
    } else {
        for v in positions.iter().take(30) {
            let occ = v
                .occupancy
                .as_ref()
                .map(|o| format!(" · {}", o))
                .unwrap_or_default();
            let speed = v.speed.map(|s| format!(" · {:.0} m/s", s)).unwrap_or_default();
            out.push_str(&format!(
                "- **Route {}** bus #{} @ {:.4}, {:.4}{}{}\n",
                v.route, v.vehicle_id, v.latitude, v.longitude, speed, occ
            ));
        }
        if positions.len() > 30 {
            out.push_str(&format!(
                "_...and {} more positions omitted._\n",
                positions.len() - 30
            ));
        }
    }

    out.push_str(&format!(
        "\n_Source: METRO GTFS-RT vehicles feed. Last updated: {}_\n",
        chrono::Local::now().format("%-I:%M %p")
    ));
    out
}

pub fn format_route_delays(
    stats: &[RouteDelayStats],
    route_filter: Option<&str>,
) -> String {
    let mut out = if let Some(r) = route_filter {
        format!("# Santa Cruz Metro — Delay stats for route {}\n\n", r)
    } else {
        "# Santa Cruz Metro — Delay stats by route\n\n".to_string()
    };

    if stats.is_empty() {
        out.push_str("No delay information in the current trip_updates feed.\n");
    } else {
        out.push_str("| Route | Active trips | Avg delay | Max delay |\n");
        out.push_str("|---|---|---|---|\n");
        for s in stats {
            out.push_str(&format!(
                "| {} | {} | {:+}s | {:+}s |\n",
                s.route, s.active_trips, s.avg_delay_seconds, s.max_delay_seconds
            ));
        }
    }

    out.push_str(&format!(
        "\n_Source: METRO GTFS-RT trip_updates feed. Last updated: {}_\n",
        chrono::Local::now().format("%-I:%M %p")
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRIPS_FIXTURE: &[u8] = include_bytes!("fixtures/gtfsrt_trips.pb");
    const VEHICLES_FIXTURE: &[u8] = include_bytes!("fixtures/gtfsrt_vehicles.pb");
    const ALERTS_FIXTURE: &[u8] = include_bytes!("fixtures/gtfsrt_alerts.pb");

    #[test]
    fn trips_fixture_decodes() {
        let msg = gtfs_realtime::FeedMessage::decode(TRIPS_FIXTURE).unwrap();
        assert!(
            !msg.entity.is_empty(),
            "trips fixture should have at least one entity"
        );
        // Assert at least one entity has a TripUpdate
        let has_trip_update = msg.entity.iter().any(|e| e.trip_update.is_some());
        assert!(
            has_trip_update,
            "trips feed should contain trip updates"
        );
    }

    #[test]
    fn vehicles_fixture_decodes() {
        let msg = gtfs_realtime::FeedMessage::decode(VEHICLES_FIXTURE).unwrap();
        let has_vehicle = msg.entity.iter().any(|e| e.vehicle.is_some());
        assert!(
            has_vehicle,
            "vehicles feed should contain vehicle positions"
        );
        // Test that build_vehicle_lookup doesn't panic
        let lookup = build_vehicle_lookup(&msg);
        // At least one of the vehicles should have an id
        assert!(
            !lookup.is_empty() || msg.entity.is_empty(),
            "vehicle lookup should be populated or the feed was empty"
        );
    }

    #[test]
    fn alerts_fixture_decodes() {
        let msg = gtfs_realtime::FeedMessage::decode(ALERTS_FIXTURE).unwrap();
        // Alerts might be empty, but the feed should decode without error.
        // Test that translated_text doesn't panic on any alert.
        for entity in &msg.entity {
            if let Some(alert) = &entity.alert {
                let _ = translated_text(alert.header_text.as_ref());
                let _ = translated_text(alert.description_text.as_ref());
            }
        }
    }

    #[test]
    fn occupancy_labels() {
        assert_eq!(occupancy_label(0), "empty");
        assert_eq!(occupancy_label(3), "standing room");
        assert_eq!(occupancy_label(5), "full");
        assert_eq!(occupancy_label(99), "unknown");
    }
}
