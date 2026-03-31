use std::io::{Cursor, Read};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const GTFS_ZIP_URL: &str = "https://scmtd.com/en/gtfs";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stop {
    pub stop_id: String,
    pub stop_name: String,
    pub stop_lat: f64,
    pub stop_lon: f64,
}

/// Download the GTFS ZIP from scmtd.com, extract stops.txt, and parse it.
pub async fn download_and_parse_stops(http: &reqwest::Client) -> Result<Vec<Stop>> {
    let bytes = http
        .get(GTFS_ZIP_URL)
        .send()
        .await
        .context("failed to download GTFS feed")?
        .error_for_status()
        .context("GTFS feed returned error status")?
        .bytes()
        .await
        .context("failed to read GTFS feed bytes")?;

    let cursor = Cursor::new(bytes.as_ref());
    let mut archive = zip::ZipArchive::new(cursor).context("failed to open GTFS ZIP")?;

    let mut stops_csv = String::new();
    archive
        .by_name("stops.txt")
        .context("stops.txt not found in GTFS ZIP")?
        .read_to_string(&mut stops_csv)
        .context("failed to read stops.txt")?;

    parse_stops_csv(&stops_csv)
}

fn parse_stops_csv(csv_data: &str) -> Result<Vec<Stop>> {
    let mut reader = csv::Reader::from_reader(csv_data.as_bytes());
    let mut stops = Vec::new();

    for result in reader.deserialize() {
        let record: StopRecord = result.context("failed to parse stop record")?;
        stops.push(Stop {
            stop_id: record.stop_id,
            stop_name: record.stop_name,
            stop_lat: record.stop_lat.unwrap_or(0.0),
            stop_lon: record.stop_lon.unwrap_or(0.0),
        });
    }

    Ok(stops)
}

/// CSV record matching GTFS stops.txt columns.
#[derive(Debug, Deserialize)]
struct StopRecord {
    stop_id: String,
    stop_name: String,
    stop_lat: Option<f64>,
    stop_lon: Option<f64>,
    // Remaining GTFS columns are ignored via #[serde(flatten)]
}

/// Search stops by name using case-insensitive substring matching.
/// Returns up to `limit` matches, sorted by relevance (exact prefix > contains).
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
    matches.into_iter().take(limit).map(|(_, stop)| stop).collect()
}
