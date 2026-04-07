pub mod api;
pub mod stops;

use std::sync::Arc;

use anyhow::Result;

use crate::cache::CacheStore;
use stops::{LoopDirection, LoopStop};

pub struct SlugLoopService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl SlugLoopService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    /// Get real-time locations of campus loop buses.
    pub async fn get_bus_locations(&self, direction: Option<&str>) -> Result<String> {
        let dir_filter = direction.and_then(LoopDirection::from_str);

        let cache_key = format!(
            "slugloop:locations:{}",
            dir_filter.map_or("all", |d| d.short())
        );

        if let Some(cached) = self.cache.get(&cache_key).await {
            return Ok(cached);
        }

        let buses = match api::fetch_buses(&self.http).await {
            Ok(b) => b,
            Err(e) => {
                return Ok(format!(
                    "Could not reach SlugLoop — the campus loop bus tracker may be temporarily unavailable.\n\
                     Error: {}",
                    e
                ));
            }
        };

        if buses.is_empty() {
            return Ok(
                "No campus loop buses are currently active. Service may have ended for the day."
                    .to_string(),
            );
        }

        let filtered: Vec<_> = if let Some(dir) = dir_filter {
            let dir_str = dir.short();
            buses
                .iter()
                .filter(|b| b.direction.eq_ignore_ascii_case(dir_str))
                .collect()
        } else {
            buses.iter().collect()
        };

        if filtered.is_empty() {
            let dir_label = dir_filter.map_or("any direction", |d| d.label());
            return Ok(format!(
                "No loop buses currently running {}. {} total bus(es) active on other direction(s).",
                dir_label,
                buses.len()
            ));
        }

        let output = format_bus_locations(&filtered, dir_filter);
        self.cache.set(&cache_key, &output, 15).await;
        Ok(output)
    }

    /// Get ETAs for campus loop buses at a specific stop.
    pub async fn get_stop_eta(&self, stop_query: &str, direction: Option<&str>) -> Result<String> {
        let dir_filter = direction.and_then(LoopDirection::from_str);

        let matches = stops::search_stops(stop_query, dir_filter, 5);
        if matches.is_empty() {
            let stop_list = if dir_filter == Some(LoopDirection::CW) {
                "Main Entrance, High Western, Arboretum, Oakes, Porter, Kerr Bridge, Kresge, Science Hill, 9/10, Cowell, East Lot, Farm, Lower Campus"
            } else {
                "Main Entrance, Lower Campus, Farm, East Lot, East Field, Cowell, Merrill, 9/10, Science Hill, Kresge, Porter, Family House, Oakes, Arboretum, Tosca Terrace, High Western"
            };
            return Ok(format!(
                "No loop bus stop found matching \"{}\". Available stops: {}",
                stop_query, stop_list
            ));
        }

        let best = matches[0];

        let cache_key = format!(
            "slugloop:eta:{}:{}",
            best.name,
            best.direction.short()
        );

        if let Some(cached) = self.cache.get(&cache_key).await {
            return Ok(cached);
        }

        let etas = match api::fetch_etas(&self.http).await {
            Ok(e) => e,
            Err(e) => {
                return Ok(format!(
                    "Could not reach SlugLoop — the campus loop bus tracker may be temporarily unavailable.\n\
                     Error: {}",
                    e
                ));
            }
        };

        let output = format_stop_eta(best, &etas, &matches[1..]);
        self.cache.set(&cache_key, &output, 30).await;
        Ok(output)
    }
}

fn format_bus_locations(buses: &[&api::Bus], dir_filter: Option<LoopDirection>) -> String {
    let title = match dir_filter {
        Some(d) => format!("UCSC Loop Buses — {} ({})", d.label(), d.short()),
        None => "UCSC Loop Buses — All Active".to_string(),
    };

    let mut out = format!("{}\n\n", title);
    out.push_str(&format!("{} bus(es) currently active:\n\n", buses.len()));

    for bus in buses {
        let dir = LoopDirection::from_str(&bus.direction);
        let dir_label = dir.map_or(bus.direction.as_str(), |d| d.short());

        let nearest = if let Some(d) = dir {
            let stop = stops::nearest_stop(bus.lat, bus.lon, d);
            format!("near {}", stop.name)
        } else {
            format!("at ({:.4}, {:.4})", bus.lat, bus.lon)
        };

        let heading_str = heading_to_cardinal(bus.heading);

        let bus_id = if bus.id.is_empty() {
            "unknown".to_string()
        } else {
            bus.id.clone()
        };

        out.push_str(&format!(
            "- **Bus {}** [{}]: {} — heading {} ({:.0}°)\n",
            bus_id, dir_label, nearest, heading_str, bus.heading
        ));
    }

    let now = chrono::Local::now();
    out.push_str(&format!("\nLast updated: {}\n", now.format("%-I:%M %p")));

    out
}

fn format_stop_eta(
    stop: &LoopStop,
    etas: &api::BusEtaResponse,
    other_matches: &[&LoopStop],
) -> String {
    let dir_etas = match stop.direction {
        LoopDirection::CW => &etas.clockwise,
        LoopDirection::CCW => &etas.counter_clockwise,
    };

    let mut out = format!(
        "Loop Bus ETA at {} ({}):\n\n",
        stop.name,
        stop.direction.label()
    );

    let stop_key = stop.name.to_lowercase();

    match dir_etas {
        Some(eta_map) => {
            // Try exact match first, then try without spaces
            let eta_value = eta_map.get(&stop_key).or_else(|| {
                let no_spaces = stop_key.replace(' ', "");
                eta_map.get(&no_spaces)
            }).or_else(|| {
                // Try partial matching against keys
                eta_map.iter().find(|(k, _)| {
                    k.contains(&stop_key) || stop_key.contains(k.as_str())
                }).map(|(_, v)| v)
            });

            match eta_value {
                Some(Some(seconds)) => {
                    let minutes = (*seconds / 60.0).ceil() as i64;
                    if minutes <= 1 {
                        out.push_str("  - **Arriving now**\n");
                    } else {
                        out.push_str(&format!("  - **{} min** (~{} seconds)\n", minutes, *seconds as i64));
                    }
                }
                Some(None) => {
                    out.push_str("  - No bus currently heading to this stop\n");
                }
                None => {
                    out.push_str("  - No ETA data available for this stop\n");
                    // Show available keys for debugging
                    if !eta_map.is_empty() {
                        let available: Vec<_> = eta_map.keys().take(5).collect();
                        out.push_str(&format!("  - Available stops in data: {:?}\n", available));
                    }
                }
            }
        }
        None => {
            out.push_str("  - No ETA data available (no buses may be running this direction)\n");
        }
    }

    let now = chrono::Local::now();
    out.push_str(&format!("\nLast updated: {}\n", now.format("%-I:%M %p")));

    if !other_matches.is_empty() {
        out.push_str("\nOther matching stops:\n");
        for s in other_matches.iter().take(4) {
            out.push_str(&format!("  - {} ({})\n", s.name, s.direction.label()));
        }
    }

    out
}

fn heading_to_cardinal(degrees: f64) -> &'static str {
    let normalized = ((degrees % 360.0) + 360.0) % 360.0;
    match normalized as u32 {
        0..=22 | 338..=360 => "N",
        23..=67 => "NE",
        68..=112 => "E",
        113..=157 => "SE",
        158..=202 => "S",
        203..=247 => "SW",
        248..=292 => "W",
        293..=337 => "NW",
        _ => "N",
    }
}
