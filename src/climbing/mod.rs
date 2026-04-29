use std::fmt::Write;
use std::sync::Arc;

use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cache::CacheStore;

// ---------------------------------------------------------------------------
// Request type
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClimbingRequest {
    /// Search by area name (e.g. "Pinnacles", "Santa Cruz", "Pinnacles National Park").
    pub area: Option<String>,
    /// Search by route name within matched area.
    pub route: Option<String>,
    /// Max results (default 20, max 50).
    pub limit: Option<u32>,
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

const OPENBETA_ENDPOINT: &str = "https://api.openbeta.io";

pub struct ClimbingService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl ClimbingService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    pub async fn search_routes(&self, req: &ClimbingRequest) -> Result<String> {
        let limit = req.limit.unwrap_or(20).min(50) as usize;

        let (cache_key, query) = if let Some(area) = &req.area {
            let key = format!("climbing:area:{}", area.to_lowercase());
            let q = format!(
                r#"{{ areas(filter: {{area_name: {{match: "{}"}}}}) {{ area_name totalClimbs metadata {{ lat lng }} pathTokens children {{ area_name totalClimbs }} climbs {{ name grades {{ yds }} type {{ sport trad bouldering tr }} fa length }} }} }}"#,
                area.replace('"', r#"\""#)
            );
            (key, q)
        } else {
            let key = "climbing:default:santa-cruz".to_string();
            let q = r#"{ areas(filter: {path_tokens: {tokens: ["California", "Central Coast", "Santa Cruz"]}}) { area_name totalClimbs metadata { lat lng } children { area_name totalClimbs } climbs { name grades { yds } type { sport trad bouldering tr } } } }"#.to_string();
            (key, q)
        };

        let http = &self.http;
        let query_owned = query.clone();
        let response: GqlResponse = self
            .cache
            .get_or_fetch(&cache_key, 21600, || async move {
                let body = serde_json::json!({ "query": query_owned });
                let resp = http
                    .post(OPENBETA_ENDPOINT)
                    .header("Content-Type", "application/json")
                    .json(&body)
                    .send()
                    .await
                    .context("failed to reach OpenBeta API")?;

                let gql: GqlResponse = resp
                    .json()
                    .await
                    .context("failed to parse OpenBeta response")?;

                Ok(gql)
            })
            .await?;

        // Handle GraphQL-level errors
        if let Some(errors) = &response.errors {
            if !errors.is_empty() {
                let mut out = String::from("# Climbing Search Error\n\n");
                for e in errors {
                    let _ = writeln!(out, "- {}", e.message);
                }
                return Ok(out);
            }
        }

        let areas = match &response.data {
            Some(d) => &d.areas,
            None => return Ok(no_results_message(req.area.as_deref())),
        };

        if areas.is_empty() {
            return Ok(no_results_message(req.area.as_deref()));
        }

        let route_filter = req.route.as_deref().map(|r| r.to_lowercase());

        let mut out = String::new();
        for area in areas {
            let _ = writeln!(out, "# Climbing \u{2014} {}", area.area_name);
            out.push('\n');

            // Metadata line
            let mut meta_parts = Vec::new();
            if let Some(tc) = area.total_climbs {
                meta_parts.push(format!("{} routes", tc));
            }
            if let Some(md) = &area.metadata {
                if let (Some(lat), Some(lng)) = (md.lat, md.lng) {
                    meta_parts.push(format!("{:.2}\u{00b0}N, {:.2}\u{00b0}W", lat, lng.abs()));
                }
            }
            if !meta_parts.is_empty() {
                let _ = writeln!(out, "_{}_", meta_parts.join(" \u{00b7} "));
                out.push('\n');
            }

            // Sub-areas
            if let Some(children) = &area.children {
                if !children.is_empty() {
                    let _ = writeln!(out, "## Sub-areas");
                    for child in children {
                        let count = child
                            .total_climbs
                            .map(|c| format!(" \u{2014} {} routes", c))
                            .unwrap_or_default();
                        let _ = writeln!(out, "- **{}**{}", child.area_name, count);
                    }
                    out.push('\n');
                }
            }

            // Routes
            if let Some(climbs) = &area.climbs {
                let filtered: Vec<&GqlClimb> = if let Some(ref rf) = route_filter {
                    climbs
                        .iter()
                        .filter(|c| c.name.to_lowercase().contains(rf))
                        .collect()
                } else {
                    climbs.iter().collect()
                };

                if !filtered.is_empty() {
                    let showing = filtered.len().min(limit);
                    if filtered.len() > limit {
                        let _ = writeln!(out, "## Routes (showing first {})", showing);
                    } else {
                        let _ = writeln!(out, "## Routes");
                    }
                    for (i, climb) in filtered.iter().take(limit).enumerate() {
                        let grade = climb
                            .grades
                            .as_ref()
                            .and_then(|g| g.yds.as_deref())
                            .unwrap_or("?");

                        let ctype = climb
                            .climb_type
                            .as_ref()
                            .map(|ct| climb_type_label(ct))
                            .unwrap_or_default();

                        let mut parts = vec![format!("**{}**", climb.name), grade.to_string()];
                        if !ctype.is_empty() {
                            parts.push(ctype);
                        }
                        if let Some(fa) = &climb.fa {
                            if !fa.is_empty() {
                                parts.push(format!("FA: {}", fa));
                            }
                        }
                        if let Some(len) = climb.length {
                            if len > 0 {
                                parts.push(format!("{} ft", len));
                            }
                        }

                        let _ = writeln!(out, "{}. {}", i + 1, parts.join(" \u{00b7} "));
                    }
                    out.push('\n');
                }
            }
        }

        let now = crate::util::now_pacific();
        let _ = write!(
            out,
            "_Source: OpenBeta (community climbing database). \
             Coverage varies \u{2014} Pinnacles NP has excellent data; \
             Castle Rock SP is not yet in the database. \
             Last updated: {}_\n",
            now.format("%-I:%M %p")
        );

        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn climb_type_label(ct: &GqlClimbType) -> String {
    let mut types = Vec::new();
    if ct.sport == Some(true) {
        types.push("Sport");
    }
    if ct.trad == Some(true) {
        types.push("Trad");
    }
    if ct.boulder == Some(true) {
        types.push("Boulder");
    }
    if ct.tr == Some(true) {
        types.push("TR");
    }
    types.join("/")
}

fn no_results_message(area: Option<&str>) -> String {
    let mut out = String::from("# Climbing Search \u{2014} No Results\n\n");
    if let Some(a) = area {
        let _ = writeln!(
            out,
            "No climbing areas found matching \"{}\".\n",
            a
        );
    } else {
        out.push_str("No climbing data found for the Santa Cruz area.\n\n");
    }
    out.push_str("**Known areas near Santa Cruz with good coverage:**\n");
    out.push_str("- Pinnacles National Park\n");
    out.push_str("- Castle Rock State Park (limited)\n");
    out.push_str("- Indian Rock (Berkeley)\n");
    out.push_str("- Yosemite Valley\n\n");
    out.push_str("_Try searching by name, e.g. area: \"Pinnacles\"_\n");
    out
}

// ---------------------------------------------------------------------------
// GraphQL response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize, Clone)]
struct GqlResponse {
    data: Option<GqlData>,
    errors: Option<Vec<GqlError>>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct GqlData {
    areas: Vec<GqlArea>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct GqlArea {
    area_name: String,
    #[serde(rename = "totalClimbs")]
    total_climbs: Option<u32>,
    metadata: Option<GqlMetadata>,
    #[serde(rename = "pathTokens")]
    path_tokens: Option<Vec<String>>,
    children: Option<Vec<GqlAreaChild>>,
    climbs: Option<Vec<GqlClimb>>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct GqlMetadata {
    lat: Option<f64>,
    lng: Option<f64>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct GqlAreaChild {
    area_name: String,
    #[serde(rename = "totalClimbs")]
    total_climbs: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct GqlClimb {
    name: String,
    grades: Option<GqlGrades>,
    #[serde(rename = "type")]
    climb_type: Option<GqlClimbType>,
    fa: Option<String>,
    length: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct GqlGrades {
    yds: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct GqlClimbType {
    sport: Option<bool>,
    trad: Option<bool>,
    #[serde(rename = "bouldering")]
    boulder: Option<bool>,
    tr: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct GqlError {
    message: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn climb_type_label_formats() {
        // Single types
        let sport = GqlClimbType {
            sport: Some(true),
            trad: Some(false),
            boulder: Some(false),
            tr: Some(false),
        };
        assert_eq!(climb_type_label(&sport), "Sport");

        let trad = GqlClimbType {
            sport: Some(false),
            trad: Some(true),
            boulder: Some(false),
            tr: Some(false),
        };
        assert_eq!(climb_type_label(&trad), "Trad");

        let boulder = GqlClimbType {
            sport: Some(false),
            trad: Some(false),
            boulder: Some(true),
            tr: Some(false),
        };
        assert_eq!(climb_type_label(&boulder), "Boulder");

        // Mixed
        let mixed = GqlClimbType {
            sport: Some(true),
            trad: Some(true),
            boulder: Some(false),
            tr: Some(false),
        };
        assert_eq!(climb_type_label(&mixed), "Sport/Trad");

        // All None
        let none = GqlClimbType {
            sport: None,
            trad: None,
            boulder: None,
            tr: None,
        };
        assert_eq!(climb_type_label(&none), "");
    }

    #[test]
    fn parse_gql_response() {
        let json = r#"{
            "data": {
                "areas": [{
                    "area_name": "Pinnacles National Park",
                    "totalClimbs": 319,
                    "metadata": { "lat": 36.4855, "lng": -121.1963 },
                    "pathTokens": ["USA", "California", "Central Coast"],
                    "children": [
                        { "area_name": "East Side", "totalClimbs": 166 },
                        { "area_name": "West Side", "totalClimbs": 116 }
                    ],
                    "climbs": [
                        {
                            "name": "The Oracle",
                            "grades": { "yds": "5.11a" },
                            "type": { "sport": true, "trad": false, "bouldering": false, "tr": false },
                            "fa": "John Bachar",
                            "length": 30
                        }
                    ]
                }]
            }
        }"#;

        let resp: GqlResponse = serde_json::from_str(json).unwrap();
        let data = resp.data.unwrap();
        assert_eq!(data.areas.len(), 1);

        let area = &data.areas[0];
        assert_eq!(area.area_name, "Pinnacles National Park");
        assert_eq!(area.total_climbs, Some(319));
        assert_eq!(area.metadata.as_ref().unwrap().lat, Some(36.4855));
        assert_eq!(area.metadata.as_ref().unwrap().lng, Some(-121.1963));

        let children = area.children.as_ref().unwrap();
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].area_name, "East Side");
        assert_eq!(children[0].total_climbs, Some(166));

        let climbs = area.climbs.as_ref().unwrap();
        assert_eq!(climbs.len(), 1);
        assert_eq!(climbs[0].name, "The Oracle");
        assert_eq!(climbs[0].grades.as_ref().unwrap().yds, Some("5.11a".to_string()));
        assert_eq!(climbs[0].climb_type.as_ref().unwrap().sport, Some(true));
        assert_eq!(climbs[0].climb_type.as_ref().unwrap().trad, Some(false));
        assert_eq!(climbs[0].fa, Some("John Bachar".to_string()));
        assert_eq!(climbs[0].length, Some(30));
    }

    #[test]
    fn format_area_overview() {
        let area = GqlArea {
            area_name: "Pinnacles National Park".to_string(),
            total_climbs: Some(319),
            metadata: Some(GqlMetadata {
                lat: Some(36.4855),
                lng: Some(-121.1963),
            }),
            path_tokens: Some(vec![
                "USA".to_string(),
                "California".to_string(),
                "Central Coast".to_string(),
            ]),
            children: Some(vec![
                GqlAreaChild {
                    area_name: "East Side".to_string(),
                    total_climbs: Some(166),
                },
                GqlAreaChild {
                    area_name: "West Side".to_string(),
                    total_climbs: Some(116),
                },
            ]),
            climbs: Some(vec![GqlClimb {
                name: "The Oracle".to_string(),
                grades: Some(GqlGrades {
                    yds: Some("5.11a".to_string()),
                }),
                climb_type: Some(GqlClimbType {
                    sport: Some(true),
                    trad: Some(false),
                    boulder: Some(false),
                    tr: Some(false),
                }),
                fa: Some("John Bachar".to_string()),
                length: Some(30),
            }]),
        };

        let response = GqlResponse {
            data: Some(GqlData {
                areas: vec![area],
            }),
            errors: None,
        };

        // Build the output the same way the service does (minus the async bits)
        let areas = &response.data.as_ref().unwrap().areas;
        let mut out = String::new();
        for a in areas {
            let _ = writeln!(out, "# Climbing \u{2014} {}", a.area_name);
            out.push('\n');

            let mut meta_parts = Vec::new();
            if let Some(tc) = a.total_climbs {
                meta_parts.push(format!("{} routes", tc));
            }
            if let Some(md) = &a.metadata {
                if let (Some(lat), Some(lng)) = (md.lat, md.lng) {
                    meta_parts.push(format!("{:.2}\u{00b0}N, {:.2}\u{00b0}W", lat, lng.abs()));
                }
            }
            if !meta_parts.is_empty() {
                let _ = writeln!(out, "_{}_", meta_parts.join(" \u{00b7} "));
                out.push('\n');
            }

            if let Some(children) = &a.children {
                if !children.is_empty() {
                    let _ = writeln!(out, "## Sub-areas");
                    for child in children {
                        let count = child
                            .total_climbs
                            .map(|c| format!(" \u{2014} {} routes", c))
                            .unwrap_or_default();
                        let _ = writeln!(out, "- **{}**{}", child.area_name, count);
                    }
                    out.push('\n');
                }
            }

            if let Some(climbs) = &a.climbs {
                let _ = writeln!(out, "## Routes");
                for (i, climb) in climbs.iter().enumerate() {
                    let grade = climb
                        .grades
                        .as_ref()
                        .and_then(|g| g.yds.as_deref())
                        .unwrap_or("?");
                    let ctype = climb
                        .climb_type
                        .as_ref()
                        .map(|ct| climb_type_label(ct))
                        .unwrap_or_default();
                    let mut parts = vec![format!("**{}**", climb.name), grade.to_string()];
                    if !ctype.is_empty() {
                        parts.push(ctype);
                    }
                    if let Some(fa) = &climb.fa {
                        if !fa.is_empty() {
                            parts.push(format!("FA: {}", fa));
                        }
                    }
                    if let Some(len) = climb.length {
                        if len > 0 {
                            parts.push(format!("{} ft", len));
                        }
                    }
                    let _ = writeln!(out, "{}. {}", i + 1, parts.join(" \u{00b7} "));
                }
            }
        }

        assert!(out.contains("# Climbing \u{2014} Pinnacles National Park"));
        assert!(out.contains("319 routes"));
        assert!(out.contains("36.49\u{00b0}N, 121.20\u{00b0}W"));
        assert!(out.contains("## Sub-areas"));
        assert!(out.contains("**East Side** \u{2014} 166 routes"));
        assert!(out.contains("**West Side** \u{2014} 116 routes"));
        assert!(out.contains("## Routes"));
        assert!(out.contains("**The Oracle** \u{00b7} 5.11a \u{00b7} Sport \u{00b7} FA: John Bachar \u{00b7} 30 ft"));
    }

    #[test]
    fn empty_results_message() {
        let msg = no_results_message(Some("Nonexistent Crag"));
        assert!(msg.contains("No Results"));
        assert!(msg.contains("Nonexistent Crag"));
        assert!(msg.contains("Pinnacles National Park"));

        let msg_default = no_results_message(None);
        assert!(msg_default.contains("No climbing data found"));
        assert!(msg_default.contains("Known areas"));
    }
}
