//! Real-time traffic data for Santa Cruz County highways (Hwy 1, 9, 17).
//!
//! Combines two public feeds:
//! - CHP CAD incidents (XML) — active police/traffic events
//! - Caltrans CWWP2 District 5 Lane Closure System (JSON) — planned and
//!   emergency lane closures
//!
//! The combined "traffic summary" tool fetches both in parallel and gracefully
//! degrades if either source is unavailable.

pub mod caltrans;
pub mod chp;

use std::sync::Arc;

use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::cache::CacheStore;
use caltrans::LaneClosure;
use chp::Incident;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TrafficRequest {
    /// Route number to filter (e.g. "1", "9", "17", "101"). If omitted, shows all Santa Cruz County routes.
    pub route: Option<String>,
}

pub struct TrafficService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl TrafficService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    /// CHP incidents in Santa Cruz County, optionally filtered by route.
    pub async fn get_chp_incidents(&self, route: Option<&str>) -> Result<String> {
        let incidents = match self.load_chp_incidents().await {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!("CHP fetch failed: {}", e);
                return Ok(format!(
                    "⚠ CHP incident feed temporarily unreachable. Try again in a minute.\n(details: {})",
                    e
                ));
            }
        };

        let filtered: Vec<&Incident> = match route {
            Some(r) => chp::filter_by_route(&incidents, r),
            None => incidents.iter().collect(),
        };

        Ok(format_chp(&filtered, route, incidents.len()))
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

    /// Combined CHP + Caltrans view, fetched in parallel. If one source fails,
    /// the other is still rendered with a warning line.
    pub async fn get_traffic_summary(&self, route: Option<&str>) -> Result<String> {
        let (chp_res, ct_res) =
            futures_util::future::join(self.load_chp_incidents(), self.load_caltrans_closures())
                .await;

        let mut out = if let Some(r) = route {
            format!("# Santa Cruz Traffic Summary — Route {}\n\n", r)
        } else {
            "# Santa Cruz Traffic Summary\n\n".to_string()
        };

        // ─── CHP section ───
        out.push_str("## CHP incidents\n\n");
        match chp_res {
            Ok(incidents) => {
                let filtered: Vec<&Incident> = match route {
                    Some(r) => chp::filter_by_route(&incidents, r),
                    None => incidents.iter().collect(),
                };
                if filtered.is_empty() {
                    out.push_str(if route.is_some() {
                        "_No active CHP incidents on this route in SC County._\n\n"
                    } else {
                        "_No active CHP incidents in Santa Cruz County._\n\n"
                    });
                } else {
                    write_chp_body(&mut out, &filtered);
                }
            }
            Err(e) => {
                tracing::warn!("CHP fetch failed in summary: {}", e);
                out.push_str(&format!("⚠ CHP feed unavailable: {}\n\n", e));
            }
        }

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

    async fn load_chp_incidents(&self) -> Result<Vec<Incident>> {
        let key = "traffic:chp:sc:raw";
        let http = self.http.clone();
        self.cache
            .get_or_fetch::<Vec<Incident>, _, _>(key, 60, move || async move {
                chp::fetch_sc_incidents(&http).await
            })
            .await
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
}

// ───── formatting helpers ─────

fn format_chp(filtered: &[&Incident], route: Option<&str>, total: usize) -> String {
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

    if filtered.is_empty() {
        out.push_str("No matching active CHP incidents.\n");
    } else {
        write_chp_body(&mut out, filtered);
    }

    let now = crate::util::now_pacific();
    out.push_str(&format!(
        "\n_Source: CHP CAD (media.chp.ca.gov). Last updated: {}_\n",
        now.format("%-I:%M %p")
    ));
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
        let out = format_chp(&[&same], None, 1);
        assert!(out.contains("Sr17 N / Summit"));
        assert!(!out.contains("(Sr17 N / Summit)"));

        let diff = incident("Sr17 N / Summit", "NB 17 AT THE SUMMIT");
        let out = format_chp(&[&diff], None, 1);
        assert!(out.contains("Sr17 N / Summit (NB 17 AT THE SUMMIT)"));
    }

    #[test]
    fn chp_empty_filter_message() {
        let out = format_chp(&[], Some("9"), 2);
        assert!(out.contains("No matching active CHP incidents."));
    }
}
