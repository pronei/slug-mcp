//! Caltrans District 5 CCTV (traffic camera) JSON feed.
//!
//! Source: <https://cwwp2.dot.ca.gov/data/d5/cctv/cctvStatusD05.json>
//!
//! Lists every D5 camera with its location, an in-service flag, and a stable
//! still-image URL that Caltrans overwrites every couple of minutes. We filter
//! client-side to Santa Cruz County entries and optionally by route.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::caltrans::route_matches;

pub const D5_CCTV_URL: &str = "https://cwwp2.dot.ca.gov/data/d5/cctv/cctvStatusD05.json";

// ───── upstream JSON types ─────

#[derive(Debug, Deserialize)]
pub struct CctvFeed {
    #[serde(default)]
    pub data: Vec<CctvEntry>,
}

#[derive(Debug, Deserialize)]
pub struct CctvEntry {
    pub cctv: Cctv,
}

#[derive(Debug, Deserialize)]
pub struct Cctv {
    #[serde(default)]
    pub location: CctvLocation,
    #[serde(rename = "inService", default)]
    pub in_service: String,
    #[serde(rename = "imageData", default)]
    pub image_data: CctvImageData,
}

#[derive(Debug, Default, Deserialize)]
pub struct CctvLocation {
    #[serde(rename = "locationName", default)]
    pub location_name: String,
    #[serde(rename = "nearbyPlace", default)]
    pub nearby_place: String,
    #[serde(default)]
    pub county: String,
    #[serde(default)]
    pub route: String,
    #[serde(default)]
    pub direction: String,
    #[serde(default)]
    pub postmile: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct CctvImageData {
    #[serde(rename = "static", default)]
    pub still: CctvStillImage,
}

#[derive(Debug, Default, Deserialize)]
pub struct CctvStillImage {
    #[serde(rename = "currentImageURL", default)]
    pub current_image_url: String,
}

// ───── normalized camera for display ─────

#[derive(Debug, Clone)]
pub struct TrafficCamera {
    pub route: String,
    pub direction: String,
    pub county: String,
    pub location_name: String,
    pub nearby_place: String,
    pub postmile: Option<f64>,
    pub in_service: bool,
    pub image_url: String,
}

impl From<CctvEntry> for TrafficCamera {
    fn from(e: CctvEntry) -> Self {
        let cctv = e.cctv;
        let loc = cctv.location;
        TrafficCamera {
            location_name: clean_location_name(&loc.location_name, &loc.route),
            route: loc.route,
            direction: loc.direction,
            county: loc.county,
            nearby_place: loc.nearby_place,
            postmile: loc.postmile.trim().parse().ok(),
            // Only an explicit "false" marks a camera down; a missing flag is
            // not evidence that it is.
            in_service: !cctv.in_service.trim().eq_ignore_ascii_case("false"),
            image_url: cctv.image_data.still.current_image_url,
        }
    }
}

/// `locationName` repeats the route ("SR-17 : Fishhook") and sometimes
/// carries doubled spaces ("SR-1 :  Soquel Ave").
fn clean_location_name(name: &str, route: &str) -> String {
    let collapsed = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if route.is_empty() {
        return collapsed;
    }
    match collapsed
        .strip_prefix(route)
        .and_then(|rest| rest.trim_start().strip_prefix(':'))
        .map(str::trim)
    {
        Some(rest) if !rest.is_empty() => rest.to_string(),
        _ => collapsed,
    }
}

// ───── public API ─────

pub async fn fetch_sc_cameras(http: &reqwest::Client) -> Result<Vec<TrafficCamera>> {
    let resp = http
        .get(D5_CCTV_URL)
        .send()
        .await
        .context("GET Caltrans CWWP2 D5 CCTV")?;
    if !resp.status().is_success() {
        anyhow::bail!("Caltrans CCTV returned HTTP {}", resp.status());
    }
    let body = resp.text().await.context("reading Caltrans CCTV body")?;
    parse_sc_cameras(&body)
}

pub fn parse_sc_cameras(body: &str) -> Result<Vec<TrafficCamera>> {
    let feed: CctvFeed = serde_json::from_str(body).context("parsing Caltrans CCTV JSON")?;
    let mut out: Vec<TrafficCamera> = feed
        .data
        .into_iter()
        .map(TrafficCamera::from)
        .filter(|c| c.county.trim().eq_ignore_ascii_case("Santa Cruz"))
        .collect();
    // Postmiles count up from the south county line, so this lists each
    // highway's cameras in road order (Watsonville → Santa Cruz, Hwy 17 →
    // the Summit).
    out.sort_by(|a, b| {
        a.route.cmp(&b.route).then_with(|| {
            a.postmile
                .unwrap_or(f64::MAX)
                .total_cmp(&b.postmile.unwrap_or(f64::MAX))
        })
    });
    Ok(out)
}

/// Filter cameras to those on a specific route. Accepts the same spellings as
/// the lane-closure filter: "1", "17", "SR-17", "Hwy 17", "Highway 1", etc.
pub fn filter_by_route<'a>(cameras: &'a [TrafficCamera], route: &str) -> Vec<&'a TrafficCamera> {
    cameras
        .iter()
        .filter(|c| route_matches(&c.route, route))
        .collect()
}

/// Markdown listing of `cameras` (already SC-filtered), narrowed to `route`
/// when given. A blank route means no filter.
pub fn format_cameras(cameras: &[TrafficCamera], route: Option<&str>) -> String {
    let route = route.map(str::trim).filter(|r| !r.is_empty());
    let filtered: Vec<&TrafficCamera> = match route {
        Some(r) => filter_by_route(cameras, r),
        None => cameras.iter().collect(),
    };
    let total = cameras.len();

    let mut out = if let Some(r) = route {
        format!(
            "# Caltrans Traffic Cameras — Route {} ({} matched of {} in SC County)\n\n",
            r,
            filtered.len(),
            total
        )
    } else {
        let down = cameras.iter().filter(|c| !c.in_service).count();
        if down > 0 {
            format!(
                "# Caltrans Traffic Cameras — Santa Cruz County ({} cameras, {} out of service)\n\n",
                total, down
            )
        } else {
            format!(
                "# Caltrans Traffic Cameras — Santa Cruz County ({} cameras)\n\n",
                total
            )
        }
    };

    if total == 0 {
        out.push_str("No Caltrans traffic cameras are listed for Santa Cruz County right now.\n");
    } else if filtered.is_empty() {
        let mut per_route: BTreeMap<&str, usize> = BTreeMap::new();
        for c in cameras {
            *per_route.entry(c.route.as_str()).or_default() += 1;
        }
        let available = per_route
            .iter()
            .map(|(r, n)| format!("{r} ({n})"))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "No Caltrans cameras on route {} in Santa Cruz County. Routes with cameras: {}.\n",
            route.unwrap_or_default(),
            available
        ));
    } else {
        write_cameras_body(&mut out, &filtered);
    }

    let now = crate::util::now_pacific();
    out.push_str(&format!(
        "\n_Source: Caltrans CWWP2 District 5 CCTV. Image links always serve the latest frame, \
         which Caltrans refreshes every few minutes. Last updated: {}_\n",
        now.format("%-I:%M %p")
    ));
    out
}

fn write_cameras_body(out: &mut String, cameras: &[&TrafficCamera]) {
    for c in cameras {
        let heading = [&c.route, &c.direction]
            .into_iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        let mut parts = vec![format!("**{}**", heading)];
        parts.extend(
            [&c.location_name, &c.nearby_place]
                .into_iter()
                .filter(|s| !s.is_empty())
                .cloned(),
        );
        if !c.in_service {
            parts.push("⚠ out of service".to_string());
        }
        out.push_str(&format!("- {}\n", parts.join(" · ")));
        if c.image_url.is_empty() {
            out.push_str("  _No image URL published._\n");
        } else if c.in_service {
            out.push_str(&format!("  [Current image]({})\n", c.image_url));
        } else {
            out.push_str(&format!(
                "  [Image]({}) (Caltrans shows a placeholder while the camera is down)\n",
                c.image_url
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("fixtures/caltrans_d5_cctv_sample.json");

    fn fixture_cameras() -> Vec<TrafficCamera> {
        parse_sc_cameras(FIXTURE).unwrap()
    }

    #[test]
    fn parse_sc_cameras_from_fixture() {
        // Fixture: 6 Santa Cruz cameras plus Monterey and Santa Barbara ones.
        let cams = fixture_cameras();
        assert_eq!(cams.len(), 6);
        for c in &cams {
            assert_eq!(c.county, "Santa Cruz");
            assert!(c.route == "SR-1" || c.route == "SR-17", "route {}", c.route);
            assert!(
                c.image_url
                    .starts_with("https://cwwp2.dot.ca.gov/data/d5/cctv/image/"),
                "unescaped image URL expected, got {}",
                c.image_url
            );
            assert!(c.postmile.is_some());
        }
        let fishhook = cams.iter().find(|c| c.location_name == "Fishhook").unwrap();
        assert_eq!(fishhook.route, "SR-17");
        assert_eq!(fishhook.direction, "North");
        assert_eq!(fishhook.nearby_place, "Santa Cruz");
        assert!(fishhook.in_service);
    }

    #[test]
    fn location_names_lose_route_prefix_and_double_spaces() {
        let cams = fixture_cameras();
        let names: Vec<&str> = cams.iter().map(|c| c.location_name.as_str()).collect();
        // Raw: "SR-1 :  Between Park Ave and Bay Ave", "SR-1 : SR-129 Northbound Exit".
        assert!(names.contains(&"Between Park Ave and Bay Ave"), "{names:?}");
        assert!(names.contains(&"SR-129 Northbound Exit"), "{names:?}");
        assert!(names.iter().all(|n| !n.starts_with("SR-1 :")));
        assert_eq!(clean_location_name("Ocean St", "SR-1"), "Ocean St");
        assert_eq!(clean_location_name("SR-1 : ", "SR-1"), "SR-1 :");
        assert_eq!(
            clean_location_name("SR-17 : Fishhook", ""),
            "SR-17 : Fishhook"
        );
    }

    #[test]
    fn sorted_by_route_then_postmile() {
        let cams = fixture_cameras();
        let order: Vec<(&str, f64)> = cams
            .iter()
            .map(|c| (c.route.as_str(), c.postmile.unwrap()))
            .collect();
        assert_eq!(
            order,
            vec![
                ("SR-1", 0.46),
                ("SR-1", 10.76),
                ("SR-1", 12.81),
                ("SR-1", 15.57),
                ("SR-17", 0.2),
                ("SR-17", 11.54),
            ]
        );
    }

    #[test]
    fn non_sc_counties_are_filtered() {
        let body = r#"{"data": [
            {"cctv": {"location": {"county": "Monterey", "route": "SR-1",
                "locationName": "SR-1 : Salinas Road"}, "inService": "true"}},
            {"cctv": {"location": {"county": "Santa Cruz", "route": "SR-17",
                "locationName": "SR-17 : Fishhook"}, "inService": "true"}}
        ]}"#;
        let cams = parse_sc_cameras(body).unwrap();
        assert_eq!(cams.len(), 1);
        assert_eq!(cams[0].route, "SR-17");
    }

    #[test]
    fn route_filter_spellings() {
        let cams = fixture_cameras();
        for q in ["17", "SR-17", "sr17", "Hwy 17", "Highway 17", " 17 "] {
            let hits = filter_by_route(&cams, q);
            assert_eq!(hits.len(), 2, "query {q:?}");
            assert!(hits.iter().all(|c| c.route == "SR-17"));
        }
        for q in ["1", "SR-1", "Highway 1", "hwy1"] {
            let hits = filter_by_route(&cams, q);
            assert_eq!(hits.len(), 4, "query {q:?}");
            assert!(hits.iter().all(|c| c.route == "SR-1"));
        }
    }

    #[test]
    fn route_filtered_output_lists_only_that_route() {
        let out = format_cameras(&fixture_cameras(), Some("Highway 17"));
        assert!(out.contains("Route Highway 17 (2 matched of 6 in SC County)"));
        assert!(out.contains("**SR-17 North** · Fishhook · Santa Cruz"));
        assert!(out.contains("**SR-17 South** · South of Summit Road · Los Gatos"));
        assert!(!out.contains("**SR-1 "), "SR-1 leaked:\n{out}");
    }

    #[test]
    fn out_of_service_cameras_are_shown_and_flagged() {
        let out = format_cameras(&fixture_cameras(), None);
        assert!(out.contains("Santa Cruz County (6 cameras, 2 out of service)"));
        assert!(
            out.contains("**SR-1 South** · State Park Drive · Aptos · ⚠ out of service"),
            "{out}"
        );
        assert!(out.contains(
            "[Image](https://cwwp2.dot.ca.gov/data/d5/cctv/image/sr1stateparkdrive/sr1stateparkdrive.jpg) (Caltrans shows a placeholder"
        ));
        assert!(out.contains(
            "[Current image](https://cwwp2.dot.ca.gov/data/d5/cctv/image/sr17fishhook/sr17fishhook.jpg)"
        ));
        assert_eq!(
            out.matches("out of service").count(),
            3,
            "header + 2 entries"
        );
    }

    #[test]
    fn unknown_route_lists_routes_with_cameras() {
        let out = format_cameras(&fixture_cameras(), Some("9"));
        assert!(out.contains("Route 9 (0 matched of 6 in SC County)"));
        assert!(out.contains(
            "No Caltrans cameras on route 9 in Santa Cruz County. Routes with cameras: SR-1 (4), SR-17 (2)."
        ));
    }

    #[test]
    fn blank_route_means_no_filter() {
        let all = format_cameras(&fixture_cameras(), None);
        for blank in ["", "   "] {
            let out = format_cameras(&fixture_cameras(), Some(blank));
            assert!(out.contains("Santa Cruz County (6 cameras, 2 out of service)"));
            // Bodies match; only the "Last updated" minute could differ.
            assert_eq!(out.split("\n_Source").next(), all.split("\n_Source").next());
        }
    }

    #[test]
    fn no_sc_cameras_message() {
        let out = format_cameras(&[], None);
        assert!(out.contains("Santa Cruz County (0 cameras)"));
        assert!(out.contains("No Caltrans traffic cameras are listed for Santa Cruz County"));
        let out = format_cameras(&[], Some("17"));
        assert!(out.contains("No Caltrans traffic cameras are listed"));
    }

    #[test]
    fn empty_data_array_is_ok() {
        assert!(parse_sc_cameras(r#"{"data": []}"#).unwrap().is_empty());
    }

    #[test]
    fn missing_data_key_is_ok() {
        assert!(parse_sc_cameras("{}").unwrap().is_empty());
    }

    #[test]
    fn truncated_json_errors_gracefully() {
        let cut = &FIXTURE[..FIXTURE.len() / 2];
        let err = parse_sc_cameras(cut).unwrap_err().to_string();
        assert!(err.contains("parsing Caltrans CCTV JSON"), "got: {err}");
    }

    #[test]
    fn non_json_body_errors_gracefully() {
        // e.g. an nginx HTML error page served with 200.
        let err = parse_sc_cameras("<html><body>502 Bad Gateway</body></html>")
            .unwrap_err()
            .to_string();
        assert!(err.contains("parsing Caltrans CCTV JSON"), "got: {err}");
    }

    #[test]
    fn entry_missing_cctv_key_errors() {
        let err = format!("{:#}", parse_sc_cameras(r#"{"data": [{}]}"#).unwrap_err());
        assert!(err.contains("cctv"), "got: {err}");
    }

    #[test]
    fn skeletal_entry_defaults_and_renders() {
        // Every field under `cctv` is defaulted: a bare SC entry parses, counts
        // as in service, and renders without dangling separators.
        let body = r#"{"data": [{"cctv": {"location": {"county": "Santa Cruz",
            "route": "SR-1", "postmile": "Not Reported"}}}]}"#;
        let cams = parse_sc_cameras(body).unwrap();
        assert_eq!(cams.len(), 1);
        let c = &cams[0];
        assert!(c.in_service);
        assert_eq!(c.postmile, None);
        let out = format_cameras(&cams, None);
        assert!(
            out.contains("- **SR-1**\n  _No image URL published._"),
            "{out}"
        );
    }
}
