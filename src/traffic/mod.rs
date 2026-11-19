//! Real-time traffic data for Santa Cruz County highways (Hwy 1, 9, 17).
//!
//! Combines two public feeds:
//! - CHP CAD incidents (XML) — active police/traffic events
//! - Caltrans CWWP2 District 5 Lane Closure System (JSON) — planned and
//!   emergency lane closures
//!
//! The combined "traffic summary" tool fetches both in parallel and gracefully
//! degrades if either source is unavailable.
//!
//! Caltrans CWWP2 District 5 CCTV (JSON) separately backs the traffic-camera
//! lookup.

pub mod caltrans;
pub mod cameras;
pub mod chp;

use std::future::Future;
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Duration};
use chrono_tz::Tz;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::cache::CacheStore;
use caltrans::LaneClosure;
use cameras::TrafficCamera;
use chp::Incident;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TrafficRequest {
    /// Route number to filter (e.g. "1", "9", "17", "101"). If omitted, shows all Santa Cruz County routes.
    pub route: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TrafficCamerasRequest {
    /// Highway to filter by, e.g. "1", "17", "SR-17", "Hwy 17" or "Highway 1". Santa Cruz County's Caltrans cameras are on Hwy 1 and Hwy 17. If omitted, lists every camera in the county.
    pub route: Option<String>,
}

pub struct TrafficService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
    // Not in `CacheStore`: overwriting a live key there keeps the old expiry,
    // so a last good list rewritten every minute would expire with the first.
    chp: Mutex<ChpState>,
}

const CHP_FRESH_SECS: i64 = 60;
/// How long the last good list is still worth showing with a warning.
const CHP_KEEP_MINUTES: i64 = 30;
// Asking CHP again soon after a bad copy returned the same copy every time
// (35 of 35 on 2026-11-19), so the adapter stays quiet instead. If CHP pins a
// caller to one server by recent activity, the quiet is also what frees it.
const CHP_PAUSE_MINUTES: i64 = 5;

#[derive(Default)]
struct ChpState {
    /// The last good list and when it was fetched.
    last_good: Option<(chp::Snapshot, DateTime<Tz>)>,
    paused: Option<ChpProblem>,
}

/// Why the CHP list on hand isn't a fresh one.
#[derive(Debug, Clone)]
struct ChpProblem {
    reason: String,
    /// Set after a bad copy: CHP isn't asked again before this time.
    retry_at: Option<DateTime<Tz>>,
}

#[derive(Debug)]
enum ChpLoad {
    Fresh(chp::Snapshot),
    Stale {
        snapshot: chp::Snapshot,
        problem: ChpProblem,
    },
    Unavailable(ChpProblem),
}

impl TrafficService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self {
            http,
            cache,
            chp: Mutex::new(ChpState::default()),
        }
    }

    /// CHP incidents in Santa Cruz County, optionally filtered by route.
    pub async fn get_chp_incidents(&self, route: Option<&str>) -> Result<String> {
        Ok(render_chp(&self.load_chp().await, route))
    }

    /// Caltrans D5 lane closures in Santa Cruz County, optionally filtered by route.
    pub async fn get_lane_closures(&self, route: Option<&str>) -> Result<String> {
        let closures = match self.load_caltrans_closures().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Caltrans fetch failed: {}", e);
                return Ok(format!(
                    "⚠ Caltrans D5 lane closures temporarily unreachable. Try again in a minute.\n(details: {})",
                    e
                ));
            }
        };

        let filtered: Vec<&LaneClosure> = match route {
            Some(r) => caltrans::filter_by_route(&closures, r),
            None => closures.iter().collect(),
        };

        Ok(format_caltrans(&filtered, route, closures.len()))
    }

    /// Caltrans D5 traffic cameras in Santa Cruz County, optionally filtered by route.
    pub async fn get_traffic_cameras(&self, route: Option<&str>) -> Result<String> {
        match self.load_cameras().await {
            Ok(cams) => Ok(cameras::format_cameras(&cams, route)),
            Err(e) => {
                tracing::warn!("Caltrans CCTV fetch failed: {}", e);
                Ok(format!(
                    "⚠ Caltrans D5 traffic cameras temporarily unreachable. Try again in a minute.\n(details: {})",
                    e
                ))
            }
        }
    }

    /// Combined CHP + Caltrans view, fetched in parallel. If one source fails,
    /// the other is still rendered with a warning line.
    pub async fn get_traffic_summary(&self, route: Option<&str>) -> Result<String> {
        let (chp_load, ct_res) =
            futures_util::future::join(self.load_chp(), self.load_caltrans_closures()).await;

        let mut out = if let Some(r) = route {
            format!("# Santa Cruz Traffic Summary — Route {}\n\n", r)
        } else {
            "# Santa Cruz Traffic Summary\n\n".to_string()
        };

        // ─── CHP section ───
        out.push_str("## CHP incidents\n\n");
        write_chp_section(&mut out, &chp_load, route);

        // ─── Caltrans section ───
        out.push_str("## Caltrans lane closures\n\n");
        match ct_res {
            Ok(closures) => {
                let filtered: Vec<&LaneClosure> = match route {
                    Some(r) => caltrans::filter_by_route(&closures, r),
                    None => closures.iter().collect(),
                };
                if filtered.is_empty() {
                    out.push_str(if route.is_some() {
                        "_No active Caltrans lane closures on this route in SC County._\n\n"
                    } else {
                        "_No active Caltrans lane closures in Santa Cruz County._\n\n"
                    });
                } else {
                    write_caltrans_body(&mut out, &filtered);
                }
            }
            Err(e) => {
                tracing::warn!("Caltrans fetch failed in summary: {}", e);
                out.push_str(&format!("⚠ Caltrans feed unavailable: {}\n\n", e));
            }
        }

        let now = crate::util::now_pacific();
        out.push_str(&format!(
            "_Sources: CHP CAD + Caltrans CWWP2 District 5. Last updated: {}_\n",
            now.format("%-I:%M %p")
        ));
        Ok(out)
    }

    async fn load_chp(&self) -> ChpLoad {
        let http = self.http.clone();
        self.load_chp_with(crate::util::now_pacific(), || async move {
            chp::fetch_sc_incidents(&http).await
        })
        .await
    }

    async fn load_chp_with<F, Fut>(&self, now: DateTime<Tz>, fetch: F) -> ChpLoad
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<chp::Snapshot>>,
    {
        // Held across the fetch, so concurrent calls share one request.
        let mut state = self.chp.lock().await;
        if let Some((snapshot, fetched_at)) = &state.last_good
            && now - *fetched_at < Duration::seconds(CHP_FRESH_SECS)
        {
            return ChpLoad::Fresh(snapshot.clone());
        }

        let paused = state.paused.clone().filter(|p| p.retry_at > Some(now));
        let problem = match paused {
            Some(problem) => problem,
            None => match fetch().await {
                Ok(snapshot) => {
                    state.last_good = Some((snapshot.clone(), now));
                    state.paused = None;
                    return ChpLoad::Fresh(snapshot);
                }
                Err(e) => {
                    // The feed URL carries no key, so the whole chain is safe to show.
                    let reason = format!("{e:#}");
                    tracing::warn!("CHP fetch failed: {reason}");
                    let retry_at = e
                        .downcast_ref::<chp::BadCopy>()
                        .map(|_| now + Duration::minutes(CHP_PAUSE_MINUTES));
                    let problem = ChpProblem { reason, retry_at };
                    state.paused = retry_at.map(|_| problem.clone());
                    problem
                }
            },
        };

        match &state.last_good {
            Some((snapshot, fetched_at))
                if now - *fetched_at < Duration::minutes(CHP_KEEP_MINUTES) =>
            {
                ChpLoad::Stale {
                    snapshot: snapshot.clone(),
                    problem,
                }
            }
            _ => ChpLoad::Unavailable(problem),
        }
    }

    async fn load_caltrans_closures(&self) -> Result<Vec<LaneClosure>> {
        let key = "traffic:caltrans:d5:sc:raw";
        let http = self.http.clone();
        self.cache
            .get_or_fetch::<Vec<LaneClosure>, _, _>(key, 300, move || async move {
                caltrans::fetch_sc_closures(&http).await
            })
            .await
    }

    async fn load_cameras(&self) -> Result<Vec<TrafficCamera>> {
        let key = "traffic:caltrans:d5:cctv:sc:raw";
        let http = self.http.clone();
        // The camera list changes only when Caltrans adds, moves or takes down
        // a camera; image URLs are fixed and serve a fresh frame every couple
        // of minutes, so a long TTL never makes an image stale.
        self.cache
            .get_or_fetch::<Vec<TrafficCamera>, _, _>(key, 6 * 3600, move || async move {
                cameras::fetch_sc_cameras(&http).await
            })
            .await
    }
}

// ───── formatting helpers ─────

fn chp_on_route<'a>(incidents: &'a [Incident], route: Option<&str>) -> Vec<&'a Incident> {
    match route {
        Some(r) => chp::filter_by_route(incidents, r),
        None => incidents.iter().collect(),
    }
}

/// The warning for a CHP list that isn't fresh. `shown` is the time of the
/// list being shown under it, if there is one.
fn chp_notice(problem: &ChpProblem, shown: Option<DateTime<Tz>>) -> String {
    let clock = |t: DateTime<Tz>| t.format("%-I:%M %p").to_string();
    match (problem.retry_at, shown) {
        (Some(retry), Some(as_of)) => format!(
            "⚠ CHP is sending this server a broken copy of its feed. This is the last good list, from {}; next check after {}.",
            clock(as_of),
            clock(retry)
        ),
        (Some(retry), None) => format!(
            "⚠ CHP is sending this server a broken copy of its incident feed, and there's no recent good list to show. Asking again right away returns the same copy; the next check is after {}. Caltrans lane closures (get_lane_closures) are unaffected.",
            clock(retry)
        ),
        (None, Some(as_of)) => format!(
            "⚠ CHP's feed couldn't be read just now. This is the last good list, from {}.",
            clock(as_of)
        ),
        (None, None) => {
            "⚠ CHP incident feed temporarily unreachable. Try again in a minute.".to_string()
        }
    }
}

fn render_chp(load: &ChpLoad, route: Option<&str>) -> String {
    let (snapshot, problem) = match load {
        ChpLoad::Fresh(snapshot) => (snapshot, None),
        ChpLoad::Stale { snapshot, problem } => (snapshot, Some(problem)),
        ChpLoad::Unavailable(problem) => {
            return format!(
                "{}\n(details: {})",
                chp_notice(problem, None),
                problem.reason
            );
        }
    };
    let filtered = chp_on_route(&snapshot.incidents, route);
    format_chp(
        &filtered,
        route,
        snapshot.incidents.len(),
        snapshot.as_of,
        problem,
    )
}

fn write_chp_section(out: &mut String, load: &ChpLoad, route: Option<&str>) {
    let snapshot = match load {
        ChpLoad::Fresh(snapshot) => snapshot,
        ChpLoad::Stale { snapshot, problem } => {
            out.push_str(&format!(
                "{}\n(details: {})\n\n",
                chp_notice(problem, Some(snapshot.as_of)),
                problem.reason
            ));
            snapshot
        }
        ChpLoad::Unavailable(problem) if problem.retry_at.is_some() => {
            out.push_str(&format!(
                "{}\n(details: {})\n\n",
                chp_notice(problem, None),
                problem.reason
            ));
            return;
        }
        ChpLoad::Unavailable(problem) => {
            out.push_str(&format!("⚠ CHP feed unavailable: {}\n\n", problem.reason));
            return;
        }
    };
    let filtered = chp_on_route(&snapshot.incidents, route);
    if filtered.is_empty() {
        out.push_str(if route.is_some() {
            "_No active CHP incidents on this route in SC County._\n\n"
        } else {
            "_No active CHP incidents in Santa Cruz County._\n\n"
        });
    } else {
        write_chp_body(out, &filtered);
    }
}

fn format_chp(
    filtered: &[&Incident],
    route: Option<&str>,
    total: usize,
    as_of: DateTime<Tz>,
    problem: Option<&ChpProblem>,
) -> String {
    let mut out = if let Some(r) = route {
        format!(
            "# CHP Incidents — Route {} ({} matched of {} in SC County)\n\n",
            r,
            filtered.len(),
            total
        )
    } else {
        format!("# CHP Incidents — Santa Cruz County ({} active)\n\n", total)
    };

    if let Some(problem) = problem {
        out.push_str(&chp_notice(problem, Some(as_of)));
        out.push_str("\n\n");
    }

    if filtered.is_empty() {
        out.push_str("No matching active CHP incidents.\n");
    } else {
        write_chp_body(&mut out, filtered);
    }

    out.push_str(&format!(
        "\n_Source: CHP CAD (media.chp.ca.gov). Last updated: {}_\n",
        as_of.format("%-I:%M %p")
    ));
    if let Some(problem) = problem {
        out.push_str(&format!("(details: {})\n", problem.reason));
    }
    out
}

fn write_chp_body(out: &mut String, incidents: &[&Incident]) {
    for i in incidents {
        let loc = if !i.location_desc.is_empty() && i.location_desc != i.location {
            format!("{} ({})", i.location, i.location_desc)
        } else {
            i.location.clone()
        };
        out.push_str(&format!(
            "- **{}** @ {} · {} — {}\n",
            i.log_type, i.log_time, i.area, loc
        ));
    }
}

fn format_caltrans(filtered: &[&LaneClosure], route: Option<&str>, total: usize) -> String {
    let mut out = if let Some(r) = route {
        format!(
            "# Caltrans D5 Lane Closures — Route {} ({} matched of {} in SC County)\n\n",
            r,
            filtered.len(),
            total
        )
    } else {
        format!(
            "# Caltrans D5 Lane Closures — Santa Cruz County ({} active)\n\n",
            total
        )
    };

    if filtered.is_empty() {
        out.push_str("No matching active Caltrans lane closures.\n");
    } else {
        write_caltrans_body(&mut out, filtered);
    }

    let now = crate::util::now_pacific();
    out.push_str(&format!(
        "\n_Source: Caltrans CWWP2 District 5. Last updated: {}_\n",
        now.format("%-I:%M %p")
    ));
    out
}

fn write_caltrans_body(out: &mut String, closures: &[&LaneClosure]) {
    for c in closures {
        let location = [&c.location_name, &c.nearby_place, &c.free_form]
            .into_iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" · ");
        let end = if c.end_indefinite {
            "indefinite".to_string()
        } else {
            format!("{} {}", c.end_date, c.end_time)
        };
        let lanes = if !c.lanes_closed.is_empty() && !c.total_lanes.is_empty() {
            format!(" · {}/{} lanes closed", c.lanes_closed, c.total_lanes)
        } else {
            String::new()
        };
        // Only show the delay line when it parses as a positive number — Caltrans
        // sometimes returns "Not Reported" or similar string sentinels.
        let delay = match c.estimated_delay.parse::<u32>() {
            Ok(mins) if mins > 0 => format!(" · ~{} min delay", mins),
            _ => String::new(),
        };
        out.push_str(&format!(
            "- **{} {}** [{}]{}{}\n  {} · {} → {}\n  Work: {}\n",
            c.route,
            c.direction,
            c.type_of_closure,
            lanes,
            delay,
            location,
            c.start_date,
            end,
            c.type_of_work,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closure(delay: &str, indefinite: bool) -> LaneClosure {
        LaneClosure {
            closure_id: "C1SC".to_string(),
            route: "SR-1".to_string(),
            direction: "North / South".to_string(),
            county: "Santa Cruz".to_string(),
            nearby_place: "Davenport".to_string(),
            location_name: "Swanton Road".to_string(),
            free_form: String::new(),
            type_of_closure: "One-Way Traffic".to_string(),
            type_of_work: "Emergency Work".to_string(),
            lanes_closed: "1".to_string(),
            total_lanes: "2".to_string(),
            estimated_delay: delay.to_string(),
            facility: "Conventional Hwy".to_string(),
            start_date: "2026-07-06".to_string(),
            start_time: "08:01:00".to_string(),
            end_date: "2026-07-06".to_string(),
            end_time: "18:01:00".to_string(),
            end_indefinite: indefinite,
        }
    }

    fn incident(location: &str, desc: &str) -> Incident {
        Incident {
            id: "260707MY0001".to_string(),
            log_time: "Jul  7 2026  6:50AM".to_string(),
            log_type: "1125-Traffic Hazard".to_string(),
            location: location.to_string(),
            location_desc: desc.to_string(),
            area: "Santa Cruz".to_string(),
            latlon: "37143411:121984839".to_string(),
        }
    }

    fn pacific(hour: u32, minute: u32) -> DateTime<Tz> {
        use chrono::TimeZone;
        chrono_tz::US::Pacific
            .with_ymd_and_hms(2026, 11, 19, hour, minute, 0)
            .unwrap()
    }

    fn service() -> TrafficService {
        TrafficService::new(reqwest::Client::new(), Arc::new(CacheStore::new(16)))
    }

    fn list(location: &str, as_of: DateTime<Tz>) -> chp::Snapshot {
        chp::Snapshot {
            incidents: vec![incident(location, "")],
            as_of,
        }
    }

    fn cut_off() -> anyhow::Error {
        anyhow::Error::new(chp::BadCopy(
            "cut-off copy: HTTP 200 text/xml, 180224 bytes".to_string(),
        ))
    }

    async fn must_not_fetch() -> Result<chp::Snapshot> {
        panic!("CHP was asked when it shouldn't have been")
    }

    fn location(load: &ChpLoad) -> &str {
        match load {
            ChpLoad::Fresh(s) | ChpLoad::Stale { snapshot: s, .. } => &s.incidents[0].location,
            ChpLoad::Unavailable(p) => panic!("no list: {p:?}"),
        }
    }

    fn secs(n: i64) -> chrono::Duration {
        chrono::Duration::seconds(n)
    }

    #[tokio::test]
    async fn fresh_list_is_reused_for_a_minute_then_fetched_again() {
        let svc = service();
        let t0 = pacific(9, 0);
        let first = svc
            .load_chp_with(t0, || async { Ok(list("Sr17 N / Summit", t0)) })
            .await;
        assert!(matches!(first, ChpLoad::Fresh(_)), "{first:?}");

        let reused = svc.load_chp_with(t0 + secs(30), must_not_fetch).await;
        assert!(matches!(reused, ChpLoad::Fresh(_)), "{reused:?}");
        assert_eq!(location(&reused), "Sr17 N / Summit");

        let later = t0 + secs(61);
        let refetched = svc
            .load_chp_with(
                later,
                || async move { Ok(list("Sr9 / Clear Creek Rd", later)) },
            )
            .await;
        assert_eq!(location(&refetched), "Sr9 / Clear Creek Rd");
    }

    #[tokio::test]
    async fn bad_copy_shows_the_last_good_list_and_pauses_five_minutes() {
        let svc = service();
        let t0 = pacific(9, 0);
        svc.load_chp_with(t0, || async { Ok(list("Sr17 N / Summit", t0)) })
            .await;

        let after_bad = svc
            .load_chp_with(pacific(9, 2), || async { Err(cut_off()) })
            .await;
        let ChpLoad::Stale { problem, .. } = &after_bad else {
            panic!("expected the last good list, got {after_bad:?}");
        };
        assert_eq!(location(&after_bad), "Sr17 N / Summit");
        assert!(problem.reason.contains("cut-off copy"), "{problem:?}");
        assert_eq!(problem.retry_at, Some(pacific(9, 7)));

        // Still paused at 9:06:59: CHP isn't asked.
        let paused = svc
            .load_chp_with(pacific(9, 6) + secs(59), must_not_fetch)
            .await;
        assert!(matches!(paused, ChpLoad::Stale { .. }), "{paused:?}");

        // At 9:07 the pause is over and the next call fetches.
        let resumed = svc
            .load_chp_with(pacific(9, 7), || async {
                Ok(list("Sr1 N / Soquel Dr Ofr", pacific(9, 7)))
            })
            .await;
        assert!(matches!(resumed, ChpLoad::Fresh(_)), "{resumed:?}");
        assert_eq!(location(&resumed), "Sr1 N / Soquel Dr Ofr");
    }

    #[tokio::test]
    async fn bad_copy_with_nothing_saved_is_unavailable_and_pauses() {
        let svc = service();
        let first = svc
            .load_chp_with(pacific(9, 0), || async { Err(cut_off()) })
            .await;
        let ChpLoad::Unavailable(problem) = &first else {
            panic!("expected unavailable, got {first:?}");
        };
        assert_eq!(problem.retry_at, Some(pacific(9, 5)));

        let again = svc.load_chp_with(pacific(9, 3), must_not_fetch).await;
        assert!(matches!(again, ChpLoad::Unavailable(_)), "{again:?}");
    }

    #[tokio::test]
    async fn last_good_list_is_dropped_after_half_an_hour() {
        let svc = service();
        let t0 = pacific(9, 0);
        svc.load_chp_with(t0, || async { Ok(list("Sr17 N / Summit", t0)) })
            .await;

        let still_shown = svc
            .load_chp_with(pacific(9, 29), || async { Err(cut_off()) })
            .await;
        assert!(
            matches!(still_shown, ChpLoad::Stale { .. }),
            "{still_shown:?}"
        );

        let too_old = svc
            .load_chp_with(pacific(9, 35), || async { Err(cut_off()) })
            .await;
        assert!(matches!(too_old, ChpLoad::Unavailable(_)), "{too_old:?}");
    }

    #[tokio::test]
    async fn network_error_shows_the_last_good_list_without_pausing() {
        let svc = service();
        let t0 = pacific(9, 0);
        svc.load_chp_with(t0, || async { Ok(list("Sr17 N / Summit", t0)) })
            .await;

        let failed = svc
            .load_chp_with(pacific(9, 2), || async {
                Err(anyhow::anyhow!("connection reset").context("GET CHP sa.xml"))
            })
            .await;
        let ChpLoad::Stale { problem, .. } = &failed else {
            panic!("expected the last good list, got {failed:?}");
        };
        assert_eq!(problem.retry_at, None);
        assert!(problem.reason.contains("connection reset"), "{problem:?}");

        // Ten seconds later CHP is asked again, and answers.
        let next = pacific(9, 2) + secs(10);
        let recovered = svc
            .load_chp_with(
                next,
                || async move { Ok(list("Sr9 / Clear Creek Rd", next)) },
            )
            .await;
        assert!(matches!(recovered, ChpLoad::Fresh(_)), "{recovered:?}");
        assert_eq!(location(&recovered), "Sr9 / Clear Creek Rd");
    }

    fn broken_copy_problem() -> ChpProblem {
        ChpProblem {
            reason: "cut-off copy: HTTP 200 text/xml, 180224 bytes".to_string(),
            retry_at: Some(pacific(9, 31)),
        }
    }

    #[test]
    fn stale_list_says_how_old_it_is_and_when_chp_is_asked_again() {
        let load = ChpLoad::Stale {
            snapshot: list("Sr17 N / Summit", pacific(9, 15)),
            problem: broken_copy_problem(),
        };
        let out = render_chp(&load, None);
        assert!(
            out.starts_with("# CHP Incidents — Santa Cruz County (1 active)"),
            "{out}"
        );
        assert!(out.contains("last good list, from 9:15 AM"), "{out}");
        assert!(out.contains("next check after 9:31 AM"), "{out}");
        assert!(out.contains("Sr17 N / Summit"), "{out}");
        assert!(
            out.contains("(details: cut-off copy: HTTP 200 text/xml, 180224 bytes)"),
            "{out}"
        );
    }

    #[test]
    fn fresh_list_carries_no_warning() {
        let out = render_chp(
            &ChpLoad::Fresh(list("Sr17 N / Summit", pacific(9, 15))),
            None,
        );
        assert!(!out.contains('⚠'), "{out}");
        assert!(!out.contains("details"), "{out}");
    }

    #[test]
    fn broken_copy_with_no_list_says_when_chp_is_asked_again() {
        let out = render_chp(&ChpLoad::Unavailable(broken_copy_problem()), None);
        assert!(out.contains("broken copy of its incident feed"), "{out}");
        assert!(out.contains("next check is after 9:31 AM"), "{out}");
        assert!(out.contains("get_lane_closures"), "{out}");
        assert!(out.contains("(details: cut-off copy"), "{out}");
    }

    #[test]
    fn other_failures_keep_the_unreachable_message() {
        let problem = ChpProblem {
            reason: "GET CHP sa.xml: connection reset".to_string(),
            retry_at: None,
        };
        let out = render_chp(&ChpLoad::Unavailable(problem), None);
        assert!(out.contains("temporarily unreachable"), "{out}");
        assert!(
            out.contains("(details: GET CHP sa.xml: connection reset)"),
            "{out}"
        );
    }

    #[test]
    fn summary_section_shows_a_stale_list_under_its_warning() {
        let load = ChpLoad::Stale {
            snapshot: list("Sr17 N / Summit", pacific(9, 15)),
            problem: broken_copy_problem(),
        };
        let mut out = String::new();
        write_chp_section(&mut out, &load, Some("17"));
        assert!(out.contains("last good list, from 9:15 AM"), "{out}");
        assert!(out.contains("Sr17 N / Summit"), "{out}");

        let mut out = String::new();
        write_chp_section(&mut out, &ChpLoad::Unavailable(broken_copy_problem()), None);
        assert!(out.contains("next check is after 9:31 AM"), "{out}");
    }

    #[test]
    fn chp_footer_shows_the_feeds_time_not_the_clock() {
        let i = incident("Sr17 N / Summit", "");
        let out = format_chp(&[&i], None, 1, pacific(3, 7), None);
        assert!(out.contains("Last updated: 3:07 AM"), "{out}");
    }

    #[test]
    fn caltrans_delay_sentinels_are_suppressed() {
        // Caltrans sends "Not Reported" and "0" — neither is a real delay.
        for sentinel in ["Not Reported", "0", ""] {
            let c = closure(sentinel, false);
            let out = format_caltrans(&[&c], None, 1);
            assert!(
                !out.contains("min delay"),
                "delay {sentinel:?} leaked:\n{out}"
            );
        }
        let c = closure("15", false);
        let out = format_caltrans(&[&c], None, 1);
        assert!(out.contains("~15 min delay"));
    }

    #[test]
    fn caltrans_indefinite_end_renders() {
        let c = closure("3", true);
        let out = format_caltrans(&[&c], None, 1);
        assert!(out.contains("→ indefinite"));
        assert!(out.contains("1/2 lanes closed"));
    }

    #[test]
    fn caltrans_empty_filter_message() {
        let out = format_caltrans(&[], Some("17"), 3);
        assert!(out.contains("Route 17 (0 matched of 3"));
        assert!(out.contains("No matching active Caltrans lane closures."));
    }

    #[test]
    fn chp_location_desc_dedupe() {
        // When LocationDesc duplicates Location it is not repeated in parens.
        let same = incident("Sr17 N / Summit", "Sr17 N / Summit");
        let out = format_chp(&[&same], None, 1, pacific(9, 15), None);
        assert!(out.contains("Sr17 N / Summit"));
        assert!(!out.contains("(Sr17 N / Summit)"));

        let diff = incident("Sr17 N / Summit", "NB 17 AT THE SUMMIT");
        let out = format_chp(&[&diff], None, 1, pacific(9, 15), None);
        assert!(out.contains("Sr17 N / Summit (NB 17 AT THE SUMMIT)"));
    }

    #[test]
    fn chp_empty_filter_message() {
        let out = format_chp(&[], Some("9"), 2, pacific(9, 15), None);
        assert!(out.contains("No matching active CHP incidents."));
    }
}
