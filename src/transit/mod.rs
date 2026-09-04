pub mod bustime;
pub mod gtfs_rt;
pub mod stops;

use std::sync::Arc;

use anyhow::{Result, bail};

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
    /// **Dispatch**: BusTime is the primary source — its `stpid`s are the ids
    /// in the stops catalog, and it carries headsigns and DUE/DLY labels.
    /// Occupancy is joined in from the GTFS-RT vehicles feed by vehicle id
    /// (SC Metro leaves BusTime's passenger-load field blank). GTFS-RT
    /// trip_updates is only a fallback when BusTime itself fails: its
    /// stop_ids are a different namespace from BusTime's, so in practice it
    /// rarely matches a catalog stop.
    pub async fn get_predictions(&self, stop_query: &str, route: Option<&str>) -> Result<String> {
        // Stop lookup comes from BusTime (cached 24h). We need the BusTime
        // key for stop search — graceful degradation if it's missing.
        let api_key = match &self.api_key {
            Some(key) => key.clone(),
            None => {
                return Ok(
                    "BusTime API key not configured. Set the `SLUG_MCP_BUSTIME_KEY` environment variable — stop lookup and arrival predictions both need it.\n\
                     Register for developer access at https://rt.scmetro.org".to_string()
                );
            }
        };

        let stops = self.load_stops(&api_key).await?;
        let matches = stops::search_stops(&stops, stop_query, 5);
        if matches.is_empty() {
            return Ok(format!(
                "No stops found matching \"{}\". Try a stop or street name (e.g., \"Science Hill\", \"Pacific & Center\", \"Oakes College\").",
                stop_query
            ));
        }

        let best_match = matches[0];
        let other_matches: Vec<&Stop> = matches.iter().skip(1).copied().collect();

        let cache_key = format!(
            "transit:bustime:pred:{}:{}",
            best_match.stop_id,
            route.unwrap_or("")
        );
        let bustime_result = self
            .cache
            .get_or_fetch(&cache_key, 30, {
                let http = self.http.clone();
                let api_key = api_key.clone();
                let stop_id = best_match.stop_id.clone();
                let route = route.map(str::to_string);
                move || async move {
                    bustime::get_predictions(&http, &api_key, &stop_id, route.as_deref()).await
                }
            })
            .await;

        let output = match bustime_result {
            Ok(mut preds) => {
                match gtfs_rt::occupancy_lookup(&self.http, &self.cache).await {
                    Ok(lookup) => apply_occupancy(&mut preds, &lookup),
                    Err(e) => {
                        tracing::info!("GTFS-RT vehicles feed unavailable for occupancy: {}", e)
                    }
                }
                format_bustime_predictions(best_match, &preds, &other_matches, None)
            }
            Err(e) => {
                tracing::warn!(
                    "BusTime predictions failed for stop {}: {} (trying GTFS-RT)",
                    best_match.stop_id,
                    e
                );
                match gtfs_rt::get_predictions_for_stop(
                    &self.http,
                    &self.cache,
                    &best_match.stop_id,
                    route,
                )
                .await
                {
                    Ok(preds) if !preds.is_empty() => {
                        gtfs_rt::format_predictions(best_match, &preds, &other_matches)
                    }
                    Ok(_) => format!(
                        "Could not fetch predictions for {} (Stop #{}):\n  - BusTime: {}\n  - GTFS-RT trip_updates has no entry for this stop\n",
                        best_match.stop_name, best_match.stop_id, e
                    ),
                    Err(e2) => format!(
                        "Could not fetch predictions for {} (Stop #{}):\n  - BusTime: {}\n  - GTFS-RT: {}\n",
                        best_match.stop_name, best_match.stop_id, e, e2
                    ),
                }
            }
        };

        Ok(output)
    }

    /// System-wide Santa Cruz Metro service alerts via GTFS-RT (no API key).
    /// Reached through `get_service_alerts` with no filter.
    async fn get_system_alerts(&self) -> Result<String> {
        match gtfs_rt::fetch_system_alerts(&self.http, &self.cache).await {
            Ok(alerts) => Ok(gtfs_rt::format_system_alerts(
                &alerts,
                crate::util::now_pacific().timestamp(),
            )),
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

    /// Service alerts: system-wide via GTFS-RT when no filter is given
    /// (keyless), or per-route/stop bulletins via BusTime.
    pub async fn get_service_alerts(
        &self,
        route: Option<&str>,
        stop_id: Option<&str>,
    ) -> Result<String> {
        if route.is_none() && stop_id.is_none() {
            return self.get_system_alerts().await;
        }

        let api_key = match &self.api_key {
            Some(key) => key.clone(),
            None => {
                return Ok(
                    "BusTime API key not configured for per-route/stop alert filtering. Set the `SLUG_MCP_BUSTIME_KEY` environment variable, or call this tool without arguments for system-wide alerts (no key needed).\n\
                     Register for developer access at https://rt.scmetro.org".to_string()
                );
            }
        };

        let cache_key = format!(
            "transit:alerts:{}:{}",
            route.unwrap_or(""),
            stop_id.unwrap_or("")
        );
        // Cache the parsed bulletins (not the rendered text) so the footer
        // timestamp is always fresh and a transient error is never pinned.
        let bulletins = self
            .cache
            .get_or_fetch(&cache_key, 300, {
                let http = self.http.clone();
                let api_key = api_key.clone();
                let route = route.map(str::to_string);
                let stop_id = stop_id.map(str::to_string);
                move || async move {
                    match bustime::get_service_bulletins(
                        &http,
                        &api_key,
                        route.as_deref(),
                        stop_id.as_deref(),
                    )
                    .await
                    {
                        Ok(b) => Ok(b),
                        // BusTime answers "No data found" for zero bulletins
                        // on a known route/stop (and for a bad identifier).
                        Err(e) if e.to_string().contains("No data found") => Ok(Vec::new()),
                        Err(e) => Err(e),
                    }
                }
            })
            .await;

        let scope = route
            .map(|r| format!("route {}", r))
            .or_else(|| stop_id.map(|s| format!("stop {}", s)))
            .unwrap_or_default();
        Ok(match bulletins {
            Ok(b) if b.is_empty() => format!(
                "No active service alerts for {}.\n\
                 (If this route/stop seems wrong, double-check the identifier.)",
                scope
            ),
            Ok(b) => format_service_bulletins(&b),
            Err(e) => format!("Could not fetch service alerts: {}", e),
        })
    }

    async fn load_stops(&self, api_key: &str) -> Result<Vec<Stop>> {
        let cache_key = "transit:bustime:stops";
        let http = self.http.clone();
        let api_key = api_key.to_string();
        self.cache
            .get_or_fetch(cache_key, 86400, || async move {
                let stops = stops::fetch_all_stops(&http, &api_key)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to load stops from BusTime API: {}", e))?;
                if stops.is_empty() {
                    bail!(
                        "BusTime API returned no stops — the API may be temporarily unavailable."
                    );
                }
                Ok(stops)
            })
            .await
    }
}

/// Render BusTime predictions (DUE/DLY countdown labels, destination
/// headsigns, canceled or expressed trip flags, occupancy joined from
/// GTFS-RT). `reason` is set only when BusTime answered as a fallback.
fn format_bustime_predictions(
    stop: &Stop,
    predictions: &[bustime::Prediction],
    other_matches: &[&Stop],
    reason: Option<&str>,
) -> String {
    let mut out = format!(
        "Bus arrivals at {} (Stop #{}):\n",
        stop.stop_name, stop.stop_id
    );
    if let Some(reason) = reason {
        out.push_str(&format!(
            "_{}; showing BusTime pre-computed ETAs._\n",
            reason
        ));
    }

    if predictions.is_empty() {
        out.push_str(
            "\nNo upcoming buses reported. Service may have ended for the day or no buses are currently running.\n",
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

    let now = crate::util::now_pacific();
    let source = if reason.is_some() {
        "BusTime (fallback)"
    } else {
        "BusTime"
    };
    out.push_str(&format!(
        "\nLast updated: {} · source: {}\n",
        now.format("%-I:%M %p"),
        source
    ));

    if !other_matches.is_empty() {
        out.push_str("\nOther matching stops:\n");
        for s in other_matches.iter().take(4) {
            out.push_str(&format!("  - {} (Stop #{})\n", s.stop_name, s.stop_id));
        }
    }

    out
}

/// Fill BusTime predictions' empty passenger-load field from a GTFS-RT
/// vehicle_id -> occupancy lookup (SC Metro never populates BusTime's field).
fn apply_occupancy(
    predictions: &mut [bustime::Prediction],
    occupancy: &std::collections::HashMap<String, String>,
) {
    for p in predictions {
        if p.passenger_load.is_none()
            && let Some(label) = occupancy.get(&p.vehicle_id)
        {
            p.passenger_load = Some(label.clone());
        }
    }
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

    let now = crate::util::now_pacific();
    out.push_str(&format!("Last checked: {}\n", now.format("%-I:%M %p")));

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stop(id: &str, name: &str) -> stops::Stop {
        stops::Stop {
            stop_id: id.to_string(),
            stop_name: name.to_string(),
            stop_lat: 36.97,
            stop_lon: -122.03,
        }
    }

    fn pred(route: &str, countdown: &str, eta: i64) -> bustime::Prediction {
        bustime::Prediction {
            route: route.to_string(),
            direction: "OUTBOUND".to_string(),
            destination: "UCSC via West Gate".to_string(),
            predicted_time: "6:37 AM".to_string(),
            eta_minutes: eta,
            countdown: countdown.to_string(),
            is_delayed: false,
            trip_status: bustime::TripStatus::Normal,
            passenger_load: None,
            next_bus_minutes: None,
            vehicle_id: String::new(),
        }
    }

    #[test]
    fn friendly_load_maps_bustime_enum() {
        assert_eq!(friendly_load("EMPTY"), "empty");
        assert_eq!(friendly_load("HALF_EMPTY"), "half-empty");
        assert_eq!(friendly_load("FULL"), "full");
        assert_eq!(friendly_load("STANDING_ROOM_ONLY"), "standing room");
        assert_eq!(friendly_load("NOT_ACCEPTING_PASSENGERS"), "not accepting");
        assert_eq!(friendly_load("SOMETHING_NEW"), "SOMETHING_NEW");
    }

    #[test]
    fn format_bustime_empty_predictions_explains_no_service() {
        let s = stop("2674", "Science Hill");
        let out = format_bustime_predictions(&s, &[], &[], Some("GTFS-RT returned no data"));
        assert!(out.contains("No upcoming buses reported"));
        assert!(out.contains("GTFS-RT returned no data"));
        assert!(out.contains("source: BusTime (fallback)"));
    }

    #[test]
    fn format_bustime_renders_markers_and_grouping() {
        let s = stop("2674", "Science Hill");
        let mut due = pred("19", "DUE", 0);
        due.passenger_load = Some("FULL".to_string());
        due.vehicle_id = "11027".to_string();
        let mut delayed = pred("19", "DLY", 4);
        delayed.is_delayed = true;
        let mut canceled = pred("11", "12", 12);
        canceled.trip_status = bustime::TripStatus::Canceled;
        canceled.next_bus_minutes = Some("30".to_string());

        let preds = vec![due, delayed, canceled];
        let other = stop("1615", "Science Hill (inbound)");
        let out = format_bustime_predictions(&s, &preds, &[&other], Some("fallback test"));

        // Route grouping with destination arrow.
        assert!(out.contains("Route 19 (OUTBOUND → UCSC via West Gate):"));
        // DUE and DLY countdown labels.
        assert!(out.contains("DUE (6:37 AM)"));
        assert!(out.contains("delayed (6:37 AM)"));
        // Passenger-load marker uses the friendly label, vehicle id appended.
        assert!(out.contains("load: full"));
        assert!(out.contains("(bus #11027)"));
        // Canceled marker and next-bus gap.
        assert!(out.contains("CANCELED"));
        assert!(out.contains("next in 30 min"));
        // Alternate stops listed.
        assert!(out.contains("Other matching stops:"));
        assert!(out.contains("Science Hill (inbound) (Stop #1615)"));
    }

    #[test]
    fn format_bustime_eta_wording() {
        let s = stop("1", "Metro Center");
        let arriving = pred("18", "1", 1);
        let later = pred("18", "9", 9);
        let out = format_bustime_predictions(&s, &[arriving, later], &[], Some("r"));
        assert!(out.contains("arriving (6:37 AM)"));
        assert!(out.contains("9 min (6:37 AM)"));
    }

    #[test]
    fn format_bulletins_empty_and_populated() {
        assert_eq!(format_service_bulletins(&[]), "No active service alerts.");

        let bulletins = vec![bustime::ServiceBulletin {
            subject: "Route 18 detour".to_string(),
            detail: "Detour via Bay St until further notice.".to_string(),
            brief: "Detour".to_string(),
            priority: "Medium".to_string(),
            affected_routes: vec!["18".to_string(), "19".to_string()],
        }];
        let out = format_service_bulletins(&bulletins);
        assert!(out.contains("# Service Alerts (1)"));
        assert!(out.contains("**[Medium]** Route 18 detour"));
        assert!(out.contains("Detour via Bay St"));
        assert!(out.contains("Affects routes: 18, 19"));
    }

    #[test]
    fn format_bulletins_skips_detail_equal_to_brief() {
        let bulletins = vec![bustime::ServiceBulletin {
            subject: "Elevator outage".to_string(),
            detail: "Same text".to_string(),
            brief: "Same text".to_string(),
            priority: String::new(),
            affected_routes: vec![],
        }];
        let out = format_service_bulletins(&bulletins);
        assert_eq!(out.matches("Same text").count(), 1);
        // No priority bracket when priority is empty.
        assert!(out.contains("**Elevator outage**"));
    }
}

#[cfg(test)]
mod audit_tests {
    use super::*;
    use std::collections::HashMap;

    fn stop() -> Stop {
        Stop {
            stop_id: "2674".into(),
            stop_name: "McLaughlin Dr (UCSC - Science Hill)".into(),
            stop_lat: 36.99,
            stop_lon: -122.06,
        }
    }

    fn pred(vid: &str) -> bustime::Prediction {
        bustime::Prediction {
            route: "19".into(),
            direction: "OUTBOUND".into(),
            destination: "UCSC".into(),
            predicted_time: "12:23 PM".into(),
            eta_minutes: 5,
            countdown: "5".into(),
            is_delayed: false,
            trip_status: bustime::TripStatus::Normal,
            passenger_load: None,
            next_bus_minutes: None,
            vehicle_id: vid.into(),
        }
    }

    #[test]
    fn bustime_primary_footer_has_no_fallback_note() {
        let out = format_bustime_predictions(&stop(), &[pred("2812")], &[], None);
        assert!(out.contains("source: BusTime\n"), "{out}");
        assert!(!out.contains("fallback"), "{out}");
        assert!(!out.contains("GTFS-RT returned no data"), "{out}");
    }

    #[test]
    fn bustime_with_reason_keeps_the_note() {
        let out = format_bustime_predictions(
            &stop(),
            &[pred("2812")],
            &[],
            Some("GTFS-RT feed unreachable"),
        );
        assert!(out.contains("GTFS-RT feed unreachable"), "{out}");
    }

    #[test]
    fn occupancy_is_filled_from_gtfs_rt_vehicle_lookup() {
        let mut preds = vec![pred("2812"), pred("9999")];
        let mut lookup = HashMap::new();
        lookup.insert("2812".to_string(), "few seats".to_string());
        apply_occupancy(&mut preds, &lookup);
        assert_eq!(preds[0].passenger_load.as_deref(), Some("few seats"));
        assert_eq!(preds[1].passenger_load, None);
    }

    #[test]
    fn occupancy_does_not_override_bustime_value() {
        let mut preds = vec![pred("2812")];
        preds[0].passenger_load = Some("FULL".into());
        let mut lookup = HashMap::new();
        lookup.insert("2812".to_string(), "empty".to_string());
        apply_occupancy(&mut preds, &lookup);
        assert_eq!(preds[0].passenger_load.as_deref(), Some("FULL"));
    }
}
