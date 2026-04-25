// BusTime API client. As of 2026-04-10, `get_predictions` is the fallback
// path in `TransitService::get_predictions` (GTFS-RT is primary; BusTime
// takes over when GTFS-RT has no absolute-time data for a matched stop).
// `get_service_bulletins` remains the backend for `get_service_alerts` —
// GTFS-RT's alerts feed exists but doesn't support per-route/per-stop
// filtering as cleanly as BusTime's bulletin API.

use anyhow::{bail, Context, Result};
use chrono::NaiveDateTime;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TripStatus {
    Normal,
    Canceled,
    Expressed,
}

impl TripStatus {
    fn from_dyn(value: i32) -> Self {
        match value {
            1 => Self::Canceled,
            2 => Self::Expressed,
            _ => Self::Normal,
        }
    }
}

#[derive(Debug)]
pub struct Prediction {
    /// Route number (e.g., "10", "15")
    pub route: String,
    /// Direction category (e.g., "OUTBOUND", "INBOUND")
    pub direction: String,
    /// Final destination sign on the bus
    pub destination: String,
    /// Predicted arrival time as a formatted string
    pub predicted_time: String,
    /// ETA in minutes from now
    pub eta_minutes: i64,
    /// Raw countdown from API (e.g., "5", "DUE", "DLY")
    pub countdown: String,
    /// Whether the vehicle is delayed
    pub is_delayed: bool,
    /// Trip dynamic status (normal, canceled, expressed)
    pub trip_status: TripStatus,
    /// Passenger load level (e.g., "EMPTY", "HALF_EMPTY", "FULL")
    pub passenger_load: Option<String>,
    /// Minutes until the next bus on this route
    pub next_bus_minutes: Option<String>,
    /// Vehicle ID
    pub vehicle_id: String,
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
        .get(format!("{}/getpredictions", super::BUSTIME_BASE_URL))
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

    let now = crate::util::now_pacific().naive_local();

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
                destination: p.des,
                predicted_time: predicted.format("%-I:%M %p").to_string(),
                eta_minutes: eta.max(0),
                countdown: p.prdctdn,
                is_delayed: p.dly,
                trip_status: TripStatus::from_dyn(p.dyn_flag),
                passenger_load: if p.psgld.is_empty() { None } else { Some(p.psgld) },
                next_bus_minutes: if p.nbus.is_empty() { None } else { Some(p.nbus) },
                vehicle_id: p.vid,
            })
        })
        .collect();

    Ok(results)
}

// ─── Service bulletins ───

#[derive(Debug)]
pub struct ServiceBulletin {
    pub subject: String,
    pub detail: String,
    pub brief: String,
    pub priority: String,
    pub affected_routes: Vec<String>,
}

/// Fetch active service bulletins for a route or stop.
pub async fn get_service_bulletins(
    http: &reqwest::Client,
    api_key: &str,
    route: Option<&str>,
    stop_id: Option<&str>,
) -> Result<Vec<ServiceBulletin>> {
    let mut params: Vec<(&str, &str)> = vec![
        ("key", api_key),
        ("format", "json"),
    ];

    if let Some(rt) = route {
        params.push(("rt", rt));
    }
    if let Some(sid) = stop_id {
        params.push(("stpid", sid));
    }

    let resp = http
        .get(format!("{}/getservicebulletins", super::BUSTIME_BASE_URL))
        .query(&params)
        .send()
        .await
        .context("failed to reach BusTime API")?
        .error_for_status()
        .context("BusTime API returned error status")?;

    let body: BustimeBulletinResponse = resp
        .json()
        .await
        .context("failed to parse BusTime bulletin response")?;

    let inner = body.bustime_response;

    if let Some(errors) = inner.error {
        let msgs: Vec<String> = errors.iter().map(|e| e.msg.clone()).collect();
        bail!("{}", msgs.join("; "));
    }

    let bulletins = inner
        .sb
        .unwrap_or_default()
        .into_iter()
        .map(|b| {
            let affected_routes = b.srvc.iter().filter_map(|s| s.rt.clone()).collect();
            ServiceBulletin {
                subject: b.sbj,
                detail: b.dtl,
                brief: b.brf,
                priority: b.prty,
                affected_routes,
            }
        })
        .collect();

    Ok(bulletins)
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
    #[serde(default)]
    dly: bool,
    /// Predicted countdown ("5", "DUE", "DLY")
    #[serde(default)]
    prdctdn: String,
    /// Dynamic flag: 0 = normal, 1 = canceled, 2 = expressed
    #[serde(default, rename = "dyn")]
    dyn_flag: i32,
    /// Passenger load level
    #[serde(default)]
    psgld: String,
    /// Next bus gap in minutes
    #[serde(default)]
    nbus: String,
    /// Final destination
    #[serde(default)]
    des: String,
    /// Vehicle ID
    #[serde(default)]
    vid: String,
}

#[derive(Debug, Deserialize)]
struct BustimeError {
    msg: String,
}

// ─── Service bulletin response types ───

#[derive(Debug, Deserialize)]
struct BustimeBulletinResponse {
    #[serde(rename = "bustime-response")]
    bustime_response: BulletinResponseInner,
}

#[derive(Debug, Deserialize)]
struct BulletinResponseInner {
    sb: Option<Vec<BustimeBulletin>>,
    error: Option<Vec<BustimeError>>,
}

#[derive(Debug, Deserialize)]
struct BustimeBulletin {
    #[serde(default)]
    sbj: String,
    #[serde(default)]
    dtl: String,
    #[serde(default)]
    brf: String,
    #[serde(default)]
    prty: String,
    #[serde(default)]
    srvc: Vec<BulletinService>,
}

#[derive(Debug, Deserialize)]
struct BulletinService {
    rt: Option<String>,
}
