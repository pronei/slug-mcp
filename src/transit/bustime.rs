// BusTime API client. As of 2026-04-10, `get_predictions` is the fallback
// path in `TransitService::get_predictions` (GTFS-RT is primary; BusTime
// takes over when GTFS-RT has no absolute-time data for a matched stop).
// `get_service_bulletins` remains the backend for `get_service_alerts` —
// GTFS-RT's alerts feed exists but doesn't support per-route/per-stop
// filtering as cleanly as BusTime's bulletin API.

use anyhow::{Context, Result, bail};
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
    let mut params = vec![("key", api_key), ("stpid", stop_id), ("format", "json")];

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

    let body = resp
        .text()
        .await
        .context("failed to read BusTime response")?;

    parse_predictions(&body, crate::util::now_pacific().naive_local())
}

/// Parse a BusTime v2 getpredictions body. `now` anchors the ETA math so
/// tests stay deterministic.
fn parse_predictions(body: &str, now: NaiveDateTime) -> Result<Vec<Prediction>> {
    let body: BustimeResponse =
        serde_json::from_str(body).context("failed to parse BusTime response")?;

    let prd_response = body.bustime_response;

    // Check for API errors
    if let Some(errors) = prd_response.error {
        let msgs: Vec<String> = errors.iter().map(|e| e.msg.clone()).collect();
        bail!("{}", msgs.join("; "));
    }

    let Some(predictions) = prd_response.prd else {
        return Ok(Vec::new());
    };

    let results = predictions
        .into_iter()
        .filter_map(|p| {
            // BusTime returns times like "20260330 14:35"
            let predicted = NaiveDateTime::parse_from_str(&p.prdtm, "%Y%m%d %H:%M").ok()?;
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
                passenger_load: if p.psgld.is_empty() {
                    None
                } else {
                    Some(p.psgld)
                },
                next_bus_minutes: if p.nbus.is_empty() {
                    None
                } else {
                    Some(p.nbus)
                },
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
    let mut params: Vec<(&str, &str)> = vec![("key", api_key), ("format", "json")];

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

    let body = resp
        .text()
        .await
        .context("failed to read BusTime bulletin response")?;

    parse_bulletins(&body)
}

/// Parse a BusTime v2 getservicebulletins body.
fn parse_bulletins(body: &str) -> Result<Vec<ServiceBulletin>> {
    let body: BustimeBulletinResponse =
        serde_json::from_str(body).context("failed to parse BusTime bulletin response")?;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> NaiveDateTime {
        NaiveDateTime::parse_from_str("20260707 06:00", "%Y%m%d %H:%M").unwrap()
    }

    #[test]
    fn parse_predictions_full_v2_row() {
        let body = r#"{"bustime-response":{"prd":[{
            "rt":"19","rtdir":"OUTBOUND","des":"UCSC via West Gate","prdtm":"20260707 06:37",
            "prdctdn":"37","dly":false,"dyn":0,"psgld":"HALF_EMPTY","nbus":"29","vid":"11027"
        }]}}"#;
        let preds = parse_predictions(body, now()).unwrap();
        assert_eq!(preds.len(), 1);
        let p = &preds[0];
        assert_eq!(p.route, "19");
        assert_eq!(p.direction, "OUTBOUND");
        assert_eq!(p.destination, "UCSC via West Gate");
        assert_eq!(p.eta_minutes, 37);
        assert_eq!(p.countdown, "37");
        assert!(!p.is_delayed);
        assert_eq!(p.trip_status, TripStatus::Normal);
        assert_eq!(p.passenger_load.as_deref(), Some("HALF_EMPTY"));
        assert_eq!(p.next_bus_minutes.as_deref(), Some("29"));
        assert_eq!(p.vehicle_id, "11027");
    }

    // SC Metro's v2 API ships the psgld field but never populates it (empty
    // string in practice — verified live 2026-07-07). Empty must map to None
    // so the formatter doesn't render an empty load marker.
    #[test]
    fn parse_predictions_empty_psgld_and_nbus_are_none() {
        let body = r#"{"bustime-response":{"prd":[{
            "rt":"19","rtdir":"OUTBOUND","des":"","prdtm":"20260707 06:37",
            "prdctdn":"37","dly":false,"dyn":0,"psgld":"","nbus":"","vid":""
        }]}}"#;
        let preds = parse_predictions(body, now()).unwrap();
        assert_eq!(preds[0].passenger_load, None);
        assert_eq!(preds[0].next_bus_minutes, None);
    }

    #[test]
    fn parse_predictions_missing_optional_fields_default() {
        // Only the three required fields — everything else #[serde(default)].
        let body = r#"{"bustime-response":{"prd":[{
            "rt":"11","rtdir":"INBOUND","prdtm":"20260707 06:10"
        }]}}"#;
        let preds = parse_predictions(body, now()).unwrap();
        let p = &preds[0];
        assert_eq!(p.eta_minutes, 10);
        assert_eq!(p.trip_status, TripStatus::Normal);
        assert_eq!(p.passenger_load, None);
        assert!(!p.is_delayed);
    }

    #[test]
    fn parse_predictions_error_envelope_bails_with_message() {
        let body = r#"{"bustime-response":{"error":[{"msg":"No data found for parameter"}]}}"#;
        let err = parse_predictions(body, now()).unwrap_err();
        assert!(err.to_string().contains("No data found for parameter"));
    }

    #[test]
    fn parse_predictions_absent_prd_is_empty() {
        let body = r#"{"bustime-response":{}}"#;
        assert!(parse_predictions(body, now()).unwrap().is_empty());
    }

    #[test]
    fn parse_predictions_unparseable_time_row_is_dropped() {
        let body = r#"{"bustime-response":{"prd":[
            {"rt":"19","rtdir":"OUTBOUND","prdtm":"not a time"},
            {"rt":"11","rtdir":"INBOUND","prdtm":"20260707 06:20"}
        ]}}"#;
        let preds = parse_predictions(body, now()).unwrap();
        assert_eq!(preds.len(), 1);
        assert_eq!(preds[0].route, "11");
    }

    #[test]
    fn parse_predictions_truncated_body_errors() {
        let body = r#"{"bustime-response":{"prd":[{"rt":"19","#;
        assert!(parse_predictions(body, now()).is_err());
    }

    #[test]
    fn parse_predictions_past_time_clamps_eta_to_zero() {
        let body = r#"{"bustime-response":{"prd":[{
            "rt":"19","rtdir":"OUTBOUND","prdtm":"20260707 05:50"
        }]}}"#;
        let preds = parse_predictions(body, now()).unwrap();
        assert_eq!(preds[0].eta_minutes, 0);
    }

    #[test]
    fn parse_predictions_dyn_flag_variants() {
        let body = r#"{"bustime-response":{"prd":[
            {"rt":"1","rtdir":"A","prdtm":"20260707 06:10","dyn":1},
            {"rt":"2","rtdir":"B","prdtm":"20260707 06:10","dyn":2},
            {"rt":"3","rtdir":"C","prdtm":"20260707 06:10","dyn":7}
        ]}}"#;
        let preds = parse_predictions(body, now()).unwrap();
        assert_eq!(preds[0].trip_status, TripStatus::Canceled);
        assert_eq!(preds[1].trip_status, TripStatus::Expressed);
        assert_eq!(preds[2].trip_status, TripStatus::Normal);
    }

    #[test]
    fn parse_bulletins_extracts_routes_and_defaults() {
        let body = r#"{"bustime-response":{"sb":[
            {"sbj":"Route 18 detour","dtl":"Detour via Bay St.","brf":"Detour",
             "prty":"Medium","srvc":[{"rt":"18"},{"rt":null},{"rt":"19"}]},
            {"sbj":"Bare minimum"}
        ]}}"#;
        let bulletins = parse_bulletins(body).unwrap();
        assert_eq!(bulletins.len(), 2);
        assert_eq!(bulletins[0].subject, "Route 18 detour");
        assert_eq!(bulletins[0].affected_routes, vec!["18", "19"]);
        assert_eq!(bulletins[1].subject, "Bare minimum");
        assert!(bulletins[1].affected_routes.is_empty());
        assert!(bulletins[1].priority.is_empty());
    }

    #[test]
    fn parse_bulletins_absent_sb_is_empty() {
        assert!(
            parse_bulletins(r#"{"bustime-response":{}}"#)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn parse_bulletins_error_envelope_bails() {
        let body = r#"{"bustime-response":{"error":[{"msg":"Invalid API access key supplied"}]}}"#;
        let err = parse_bulletins(body).unwrap_err();
        assert!(err.to_string().contains("Invalid API access key"));
    }

    #[test]
    fn parse_bulletins_truncated_body_errors() {
        assert!(parse_bulletins(r#"{"bustime-response":{"sb":[{"sb"#).is_err());
    }
}
