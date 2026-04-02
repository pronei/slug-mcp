use std::collections::HashSet;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

const BUSTIME_BASE_URL: &str = "https://rt.scmetro.org/bustime/api/v2";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stop {
    pub stop_id: String,
    pub stop_name: String,
    pub stop_lat: f64,
    pub stop_lon: f64,
}

/// Fetch all stops from BusTime by enumerating routes and directions.
/// Deduplicates stops that appear on multiple routes.
pub async fn fetch_all_stops(http: &reqwest::Client, api_key: &str) -> Result<Vec<Stop>> {
    // 1. Get all routes
    let routes = get_routes(http, api_key).await?;

    // 2. Get directions for all routes concurrently
    let dir_futures: Vec<_> = routes
        .iter()
        .map(|r| get_directions(http, api_key, &r.rt))
        .collect();
    let dir_results = futures_util::future::join_all(dir_futures).await;

    // 3. Build (route, direction) pairs
    let mut route_dir_pairs: Vec<(String, String)> = Vec::new();
    for (route, dirs_result) in routes.iter().zip(dir_results) {
        if let Ok(dirs) = dirs_result {
            for dir in dirs {
                route_dir_pairs.push((route.rt.clone(), dir.dir));
            }
        }
    }

    // 4. Fetch stops for all route+direction pairs concurrently
    let stop_futures: Vec<_> = route_dir_pairs
        .iter()
        .map(|(rt, dir)| get_stops(http, api_key, rt, dir))
        .collect();
    let stop_results = futures_util::future::join_all(stop_futures).await;

    // 5. Deduplicate by stop_id
    let mut seen = HashSet::new();
    let mut all_stops = Vec::new();
    for result in stop_results {
        if let Ok(stops) = result {
            for stop in stops {
                if seen.insert(stop.stop_id.clone()) {
                    all_stops.push(stop);
                }
            }
        }
    }

    Ok(all_stops)
}

async fn get_routes(http: &reqwest::Client, api_key: &str) -> Result<Vec<Route>> {
    let resp: RoutesResponse = http
        .get(format!("{}/getroutes", BUSTIME_BASE_URL))
        .query(&[("key", api_key), ("format", "json")])
        .send()
        .await
        .context("failed to reach BusTime API")?
        .error_for_status()
        .context("BusTime getroutes returned error")?
        .json()
        .await
        .context("failed to parse getroutes response")?;

    if let Some(errors) = resp.bustime_response.error {
        let msgs: Vec<String> = errors.iter().map(|e| e.msg.clone()).collect();
        bail!("BusTime getroutes error: {}", msgs.join("; "));
    }

    Ok(resp.bustime_response.routes.unwrap_or_default())
}

async fn get_directions(
    http: &reqwest::Client,
    api_key: &str,
    route: &str,
) -> Result<Vec<Direction>> {
    let resp: DirectionsResponse = http
        .get(format!("{}/getdirections", BUSTIME_BASE_URL))
        .query(&[("key", api_key), ("rt", route), ("format", "json")])
        .send()
        .await
        .context("failed to reach BusTime API")?
        .error_for_status()
        .context("BusTime getdirections returned error")?
        .json()
        .await
        .context("failed to parse getdirections response")?;

    Ok(resp.bustime_response.directions.unwrap_or_default())
}

async fn get_stops(
    http: &reqwest::Client,
    api_key: &str,
    route: &str,
    direction: &str,
) -> Result<Vec<Stop>> {
    let resp: StopsResponse = http
        .get(format!("{}/getstops", BUSTIME_BASE_URL))
        .query(&[
            ("key", api_key),
            ("rt", route),
            ("dir", direction),
            ("format", "json"),
        ])
        .send()
        .await
        .context("failed to reach BusTime API")?
        .error_for_status()
        .context("BusTime getstops returned error")?
        .json()
        .await
        .context("failed to parse getstops response")?;

    Ok(resp
        .bustime_response
        .stops
        .unwrap_or_default()
        .into_iter()
        .map(|s| Stop {
            stop_id: s.stpid,
            stop_name: s.stpnm,
            stop_lat: s.lat,
            stop_lon: s.lon,
        })
        .collect())
}

// ─── BusTime API response types ───

#[derive(Debug, Deserialize)]
struct BustimeError {
    msg: String,
}

#[derive(Debug, Deserialize)]
struct RoutesResponse {
    #[serde(rename = "bustime-response")]
    bustime_response: RoutesInner,
}

#[derive(Debug, Deserialize)]
struct RoutesInner {
    routes: Option<Vec<Route>>,
    error: Option<Vec<BustimeError>>,
}

#[derive(Debug, Deserialize)]
struct Route {
    rt: String,
}

#[derive(Debug, Deserialize)]
struct DirectionsResponse {
    #[serde(rename = "bustime-response")]
    bustime_response: DirectionsInner,
}

#[derive(Debug, Deserialize)]
struct DirectionsInner {
    directions: Option<Vec<Direction>>,
}

#[derive(Debug, Deserialize)]
struct Direction {
    dir: String,
}

#[derive(Debug, Deserialize)]
struct StopsResponse {
    #[serde(rename = "bustime-response")]
    bustime_response: StopsInner,
}

#[derive(Debug, Deserialize)]
struct StopsInner {
    stops: Option<Vec<BustimeStop>>,
}

#[derive(Debug, Deserialize)]
struct BustimeStop {
    stpid: String,
    stpnm: String,
    lat: f64,
    lon: f64,
}

/// Search stops by name using case-insensitive substring matching.
/// Returns up to `limit` matches, sorted by relevance (exact > prefix > contains > words).
pub fn search_stops<'a>(stops: &'a [Stop], query: &str, limit: usize) -> Vec<&'a Stop> {
    let query_lower = query.to_lowercase();

    let mut matches: Vec<(usize, &Stop)> = stops
        .iter()
        .filter_map(|stop| {
            let name_lower = stop.stop_name.to_lowercase();
            if name_lower == query_lower {
                Some((0, stop)) // exact match
            } else if name_lower.starts_with(&query_lower) {
                Some((1, stop)) // prefix match
            } else if name_lower.contains(&query_lower) {
                Some((2, stop)) // substring match
            } else {
                // Also match individual words
                let query_words: Vec<&str> = query_lower.split_whitespace().collect();
                let all_words_match = !query_words.is_empty()
                    && query_words.iter().all(|w| name_lower.contains(w));
                if all_words_match {
                    Some((3, stop))
                } else {
                    None
                }
            }
        })
        .collect();

    matches.sort_by_key(|(rank, _)| *rank);
    matches
        .into_iter()
        .take(limit)
        .map(|(_, stop)| stop)
        .collect()
}
