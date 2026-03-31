use anyhow::{bail, Context, Result};
use chrono::{Local, NaiveDateTime};
use serde::Deserialize;

const BUSTIME_BASE_URL: &str = "https://rt.scmetro.org/bustime/api/v2";

#[derive(Debug)]
pub struct Prediction {
    /// Route number (e.g., "10", "15")
    pub route: String,
    /// Direction/destination (e.g., "Metro Center", "Westside")
    pub direction: String,
    /// Predicted arrival time as a formatted string
    pub predicted_time: String,
    /// ETA in minutes from now
    pub eta_minutes: i64,
    /// Whether the prediction is delayed vs. on schedule
    pub is_delayed: bool,
}

/// Fetch real-time arrival predictions for a stop.
pub async fn get_predictions(
    http: &reqwest::Client,
    api_key: &str,
    stop_id: &str,
    route: Option<&str>,
) -> Result<Vec<Prediction>> {
    let mut params = vec![
        ("key", api_key),
        ("stpid", stop_id),
        ("format", "json"),
    ];

    if let Some(rt) = route {
        params.push(("rt", rt));
    }

    let resp = http
        .get(format!("{}/getpredictions", BUSTIME_BASE_URL))
        .query(&params)
        .send()
        .await
        .context("failed to reach BusTime API")?
        .error_for_status()
        .context("BusTime API returned error status")?;

    let body: BustimeResponse = resp
        .json()
        .await
        .context("failed to parse BusTime response")?;

    let prd_response = body.bustime_response;

    // Check for API errors
    if let Some(errors) = prd_response.error {
        let msgs: Vec<String> = errors.iter().map(|e| e.msg.clone()).collect();
        bail!("{}", msgs.join("; "));
    }

    let Some(predictions) = prd_response.prd else {
        return Ok(Vec::new());
    };

    let now = Local::now().naive_local();

    let results = predictions
        .into_iter()
        .filter_map(|p| {
            // BusTime returns times like "20260330 14:35"
            let predicted =
                NaiveDateTime::parse_from_str(&p.prdtm, "%Y%m%d %H:%M").ok()?;
            let eta = (predicted - now).num_minutes();

            Some(Prediction {
                route: p.rt,
                direction: p.rtdir,
                predicted_time: predicted.format("%-I:%M %p").to_string(),
                eta_minutes: eta.max(0),
                is_delayed: p.dly,
            })
        })
        .collect();

    Ok(results)
}

// ─── BusTime API response types ───

#[derive(Debug, Deserialize)]
struct BustimeResponse {
    #[serde(rename = "bustime-response")]
    bustime_response: PredictionResponse,
}

#[derive(Debug, Deserialize)]
struct PredictionResponse {
    prd: Option<Vec<BustimePrediction>>,
    error: Option<Vec<BustimeError>>,
}

#[derive(Debug, Deserialize)]
struct BustimePrediction {
    /// Route designator
    rt: String,
    /// Route direction (e.g., "OUTBOUND")
    rtdir: String,
    /// Predicted arrival/departure time (YYYYMMDD HH:MM format)
    prdtm: String,
    /// Whether the vehicle is delayed
    dly: bool,
}

#[derive(Debug, Deserialize)]
struct BustimeError {
    msg: String,
}
