use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::BUSTIME_BASE_URL;

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

    merge_stop_results(stop_results)
}

/// Merge per-(route, direction) `getstops` results into one deduplicated
/// catalog. Refuses to produce a catalog when more than a quarter of the
/// fan-out failed: a partial catalog would be cached for a day and make every
/// stop on the failed routes report "No stops found".
pub fn merge_stop_results(results: Vec<Result<Vec<Stop>>>) -> Result<Vec<Stop>> {
    let total = results.len();
    let failed = results.iter().filter(|r| r.is_err()).count();
    if failed * 4 > total {
        bail!(
            "BusTime getstops failed for {failed} of {total} route-directions; refusing to cache a partial stop catalog"
        );
    }
    if failed > 0 {
        tracing::warn!("BusTime getstops failed for {failed} of {total} route-directions");
    }
    let mut seen = HashSet::new();
    let mut all_stops = Vec::new();
    for stops in results.into_iter().flatten() {
        for stop in stops {
            if seen.insert(stop.stop_id.clone()) {
                all_stops.push(stop);
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

/// Query aliases for hubs whose everyday name differs from the BusTime stop
/// name (all normalized: lowercase, `&` -> "and", punctuation stripped).
const STOP_ALIASES: &[(&str, &str)] = &[
    ("metro center", "pacific and center"),
    ("metro station", "pacific and center"),
    ("santa cruz metro", "pacific and center"),
    ("downtown transit center", "pacific and center"),
    ("main entrance", "bay and high"),
    ("base of campus", "bay and high"),
];

/// Lowercase, fold `&` to "and", and collapse punctuation/whitespace so
/// "Bay & High", "bay and high", and "BAY AND HIGH" compare equal.
fn normalize(text: &str) -> String {
    let folded = text.to_lowercase().replace('&', " and ");
    let spaced: String = folded
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Search stops by name. Ranking: exact > prefix > substring > every query
/// token present > a majority of tokens present. Falls back to the alias
/// table only when the literal query matches nothing.
pub fn search_stops<'a>(stops: &'a [Stop], query: &str, limit: usize) -> Vec<&'a Stop> {
    let query = normalize(query);
    // An empty query would prefix-match every stop and return `limit`
    // arbitrary ones; treat it as "no match" so the tool prompts for a name.
    if query.is_empty() {
        return Vec::new();
    }
    let direct = rank_stops(stops, &query, limit);
    if !direct.is_empty() {
        return direct;
    }
    match STOP_ALIASES.iter().find(|(alias, _)| *alias == query) {
        Some((_, target)) => rank_stops(stops, target, limit),
        None => Vec::new(),
    }
}

fn rank_stops<'a>(stops: &'a [Stop], query: &str, limit: usize) -> Vec<&'a Stop> {
    let tokens: Vec<&str> = query.split_whitespace().collect();
    let mut matches: Vec<(u8, usize, usize, &Stop)> = stops
        .iter()
        .filter_map(|stop| {
            let name = normalize(&stop.stop_name);
            let hits = tokens.iter().filter(|t| name.contains(*t)).count();
            let rank = if name == query {
                0
            } else if name.starts_with(query) {
                1
            } else if name.contains(query) {
                2
            } else if hits == tokens.len() {
                3
            } else if tokens.len() >= 2 && hits * 2 >= tokens.len() {
                4
            } else {
                return None;
            };
            Some((rank, tokens.len() - hits, name.len(), stop))
        })
        .collect();

    matches.sort_by_key(|(rank, misses, len, _)| (*rank, *misses, *len));
    matches
        .into_iter()
        .take(limit)
        .map(|(_, _, _, stop)| stop)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stop(id: &str, name: &str) -> Stop {
        Stop {
            stop_id: id.to_string(),
            stop_name: name.to_string(),
            stop_lat: 36.97,
            stop_lon: -122.03,
        }
    }

    fn catalog() -> Vec<Stop> {
        vec![
            stop("1", "Science Hill"),
            stop("2", "Science Hill North"),
            stop("3", "McLaughlin Dr (UCSC - Science Hill)"),
            stop("4", "Metro Center"),
            stop("5", "Bay & High"),
        ]
    }

    #[test]
    fn search_ranks_exact_before_prefix_before_substring() {
        let stops = catalog();
        let results = search_stops(&stops, "Science Hill", 5);
        let names: Vec<&str> = results.iter().map(|s| s.stop_name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Science Hill",
                "Science Hill North",
                "McLaughlin Dr (UCSC - Science Hill)"
            ]
        );
    }

    #[test]
    fn search_matches_all_words_regardless_of_order() {
        let stops = catalog();
        let results = search_stops(&stops, "hill science", 5);
        assert!(!results.is_empty());
        assert!(results.iter().all(|s| {
            let n = s.stop_name.to_lowercase();
            n.contains("science") && n.contains("hill")
        }));
    }

    #[test]
    fn search_is_case_insensitive_and_respects_limit() {
        let stops = catalog();
        let results = search_stops(&stops, "SCIENCE", 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].stop_name, "Science Hill");
    }

    #[test]
    fn search_no_match_returns_empty() {
        let stops = catalog();
        assert!(search_stops(&stops, "airport", 5).is_empty());
    }

    #[test]
    fn search_empty_or_whitespace_query_matches_nothing() {
        let stops = catalog();
        assert!(search_stops(&stops, "", 5).is_empty());
        assert!(search_stops(&stops, "   ", 5).is_empty());
    }

    #[test]
    fn stops_response_deserializes_bustime_shape() {
        let body = r#"{"bustime-response":{"stops":[
            {"stpid":"2674","stpnm":"McLaughlin Dr (UCSC - Science Hill)","lat":36.9997,"lon":-122.0603}
        ]}}"#;
        let parsed: StopsResponse = serde_json::from_str(body).unwrap();
        let stops = parsed.bustime_response.stops.unwrap();
        assert_eq!(stops[0].stpid, "2674");
        assert_eq!(stops[0].stpnm, "McLaughlin Dr (UCSC - Science Hill)");
        assert!((stops[0].lat - 36.9997).abs() < 1e-9);
    }

    #[test]
    fn routes_response_error_envelope_deserializes() {
        let body = r#"{"bustime-response":{"error":[{"msg":"Invalid API access key supplied"}]}}"#;
        let parsed: RoutesResponse = serde_json::from_str(body).unwrap();
        let errors = parsed.bustime_response.error.unwrap();
        assert_eq!(errors[0].msg, "Invalid API access key supplied");
        assert!(parsed.bustime_response.routes.is_none());
    }
}

#[cfg(test)]
mod audit_tests {
    use super::*;

    fn stop(id: &str, name: &str) -> Stop {
        Stop {
            stop_id: id.to_string(),
            stop_name: name.to_string(),
            stop_lat: 36.97,
            stop_lon: -122.03,
        }
    }

    #[test]
    fn search_resolves_metro_center_alias_to_pacific_and_center() {
        let stops = vec![stop("2697", "Pacific & Center"), stop("1", "Science Hill")];
        let results = search_stops(&stops, "Metro Center", 5);
        assert_eq!(results.first().map(|s| s.stop_id.as_str()), Some("2697"));
    }

    #[test]
    fn search_resolves_main_entrance_alias_to_bay_and_high() {
        let stops = vec![stop("5", "Bay & High"), stop("1", "Science Hill")];
        let results = search_stops(&stops, "Main Entrance", 5);
        assert_eq!(results.first().map(|s| s.stop_id.as_str()), Some("5"));
    }

    #[test]
    fn search_folds_ampersand_and_the_word_and() {
        let stops = vec![stop("5", "Bay & High")];
        assert_eq!(search_stops(&stops, "Bay and High", 5).len(), 1);
        assert_eq!(search_stops(&stops, "bay & high", 5).len(), 1);
        assert_eq!(search_stops(&stops, "BAY AND HIGH", 5).len(), 1);
    }

    #[test]
    fn search_accepts_majority_token_coverage_but_not_minority() {
        let stops = vec![stop("5", "Bay & High"), stop("1", "Science Hill")];
        let results = search_stops(&stops, "bay high entrance", 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].stop_id, "5");
        assert!(search_stops(&stops, "bay airport station road", 5).is_empty());
    }

    #[test]
    fn search_direct_match_wins_over_alias() {
        let stops = vec![stop("9", "Metro Center"), stop("2697", "Pacific & Center")];
        let results = search_stops(&stops, "Metro Center", 5);
        assert_eq!(results.first().map(|s| s.stop_id.as_str()), Some("9"));
    }

    #[test]
    fn merge_refuses_catalog_when_more_than_a_quarter_of_fetches_failed() {
        let results: Vec<anyhow::Result<Vec<Stop>>> = vec![
            Ok(vec![stop("1", "A")]),
            Ok(vec![stop("2", "B")]),
            Ok(vec![stop("3", "C")]),
            Err(anyhow::anyhow!("429")),
            Err(anyhow::anyhow!("429")),
        ];
        assert!(merge_stop_results(results).is_err());
    }

    #[test]
    fn merge_dedupes_and_tolerates_a_minority_of_failures() {
        let results: Vec<anyhow::Result<Vec<Stop>>> = vec![
            Ok(vec![stop("1", "A"), stop("2", "B")]),
            Ok(vec![stop("2", "B"), stop("3", "C")]),
            Ok(vec![stop("4", "D")]),
            Ok(vec![]),
            Err(anyhow::anyhow!("timeout")),
        ];
        let merged = merge_stop_results(results).unwrap();
        let ids: Vec<&str> = merged.iter().map(|s| s.stop_id.as_str()).collect();
        assert_eq!(ids, vec!["1", "2", "3", "4"]);
    }
}
