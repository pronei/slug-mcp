//! NPS Developer API client + markdown formatters.

use std::fmt::Write;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub(super) const NPS_API_BASE: &str = "https://developer.nps.gov/api/v1/parks";

// ─── NPS API response types ───

#[derive(Debug, Deserialize, Serialize, Clone)]
pub(super) struct NpsResponse {
    pub total: String,
    pub data: Vec<NpsPark>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub(super) struct NpsPark {
    #[serde(rename = "fullName")]
    pub full_name: String,
    #[serde(rename = "parkCode")]
    pub park_code: String,
    pub description: Option<String>,
    pub latitude: Option<String>,
    pub longitude: Option<String>,
    pub states: Option<String>,
    pub url: Option<String>,
    #[serde(rename = "directionsInfo")]
    pub directions_info: Option<String>,
    #[serde(rename = "weatherInfo")]
    pub weather_info: Option<String>,
    #[serde(rename = "operatingHours", default)]
    pub operating_hours: Vec<NpsHours>,
    #[serde(rename = "entranceFees", default)]
    pub entrance_fees: Vec<NpsFee>,
    #[serde(rename = "entrancePasses", default)]
    pub entrance_passes: Vec<NpsPass>,
    #[serde(default)]
    pub activities: Vec<NpsActivity>,
    pub contacts: Option<NpsContacts>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub(super) struct NpsHours {
    pub name: Option<String>,
    pub description: Option<String>,
    #[serde(rename = "standardHours")]
    pub standard_hours: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub(super) struct NpsFee {
    pub cost: Option<String>,
    pub description: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub(super) struct NpsPass {
    pub cost: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub(super) struct NpsActivity {
    pub name: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub(super) struct NpsContacts {
    #[serde(rename = "phoneNumbers", default)]
    pub phone_numbers: Vec<NpsPhone>,
    #[serde(rename = "emailAddresses", default)]
    pub email_addresses: Vec<NpsEmail>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub(super) struct NpsPhone {
    #[serde(rename = "phoneNumber")]
    pub phone_number: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub(super) struct NpsEmail {
    #[serde(rename = "emailAddress")]
    pub email_address: String,
}

// ─── API fetch ───

pub(super) async fn fetch_parks(
    http: &reqwest::Client,
    query_params: &[(String, String)],
) -> Result<NpsResponse> {
    let resp = http
        .get(NPS_API_BASE)
        .query(query_params)
        .send()
        .await
        .context("NPS API HTTP request failed")?;

    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("NPS API returned HTTP {}", status);
    }

    let body = resp
        .text()
        .await
        .context("reading NPS API response body")?;

    serde_json::from_str::<NpsResponse>(&body).context("parsing NPS API JSON response")
}

// ─── Formatting ───

pub(super) fn format_response(response: &NpsResponse) -> String {
    if response.data.is_empty() {
        return format!(
            "No national parks found matching your search.\n\n\
             _Source: National Park Service API. Last updated: {}_\n",
            crate::util::now_pacific().format("%-I:%M %p")
        );
    }

    if response.data.len() == 1 {
        format_single_park(&response.data[0])
    } else {
        format_search_results(response)
    }
}

fn format_single_park(park: &NpsPark) -> String {
    let mut out = String::new();

    // Title
    let _ = writeln!(out, "# {}", park.full_name);
    out.push('\n');

    // Subtitle: state + URL
    let state = park.states.as_deref().unwrap_or("US");
    if let Some(url) = &park.url {
        let _ = writeln!(
            out,
            "_Located in {} · [nps.gov/{}]({})_",
            state, park.park_code, url
        );
    } else {
        let _ = writeln!(out, "_Located in {}_", state);
    }
    out.push('\n');

    // Description
    if let Some(desc) = &park.description {
        if !desc.is_empty() {
            let _ = writeln!(out, "## Description");
            let _ = writeln!(out, "{}", desc);
            out.push('\n');
        }
    }

    // Operating hours
    if !park.operating_hours.is_empty() {
        let _ = writeln!(out, "## Hours");
        for hours in &park.operating_hours {
            let name = hours.name.as_deref().unwrap_or("General");
            let summary = summarize_hours(hours);
            let _ = writeln!(out, "- **{}**: {}", name, summary);
        }
        out.push('\n');
    }

    // Entrance fees + passes
    if !park.entrance_fees.is_empty() || !park.entrance_passes.is_empty() {
        let _ = writeln!(out, "## Fees");
        for fee in &park.entrance_fees {
            let title = fee.title.as_deref().unwrap_or("Entrance Fee");
            let cost = fee.cost.as_deref().unwrap_or("N/A");
            let _ = writeln!(out, "- **{}**: ${}", title, cost);
        }
        for pass in &park.entrance_passes {
            let title = pass.title.as_deref().unwrap_or("Pass");
            let cost = pass.cost.as_deref().unwrap_or("N/A");
            let _ = writeln!(out, "- **{}**: ${}", title, cost);
        }
        out.push('\n');
    }

    // Activities
    if !park.activities.is_empty() {
        let _ = writeln!(out, "## Activities");
        let names: Vec<&str> = park.activities.iter().map(|a| a.name.as_str()).collect();
        let _ = writeln!(out, "{}", names.join(", "));
        out.push('\n');
    }

    // Directions
    if let Some(dir) = &park.directions_info {
        if !dir.is_empty() {
            let _ = writeln!(out, "## Directions");
            let _ = writeln!(out, "{}", dir);
            out.push('\n');
        }
    }

    // Weather
    if let Some(weather) = &park.weather_info {
        if !weather.is_empty() {
            let _ = writeln!(out, "## Weather");
            let _ = writeln!(out, "{}", weather);
            out.push('\n');
        }
    }

    // Contact
    if let Some(contacts) = &park.contacts {
        let has_phone = !contacts.phone_numbers.is_empty();
        let has_email = !contacts.email_addresses.is_empty();
        if has_phone || has_email {
            let _ = writeln!(out, "## Contact");
            for phone in &contacts.phone_numbers {
                if !phone.phone_number.is_empty() {
                    let _ = writeln!(out, "- Phone: {}", phone.phone_number);
                }
            }
            for email in &contacts.email_addresses {
                if !email.email_address.is_empty() {
                    let _ = writeln!(out, "- Email: {}", email.email_address);
                }
            }
            out.push('\n');
        }
    }

    let _ = writeln!(
        out,
        "_Source: National Park Service API. Last updated: {}_",
        crate::util::now_pacific().format("%-I:%M %p")
    );
    out
}

fn format_search_results(response: &NpsResponse) -> String {
    let mut out = String::new();

    // Try to extract the search term context for the title
    let _ = writeln!(
        out,
        "# National Parks ({} results)\n",
        response.data.len()
    );

    for (i, park) in response.data.iter().enumerate() {
        let state = park.states.as_deref().unwrap_or("US");
        let _ = writeln!(
            out,
            "{}. **{}** ({}) — {}",
            i + 1,
            park.full_name,
            park.park_code,
            state
        );

        if let Some(desc) = &park.description {
            if !desc.is_empty() {
                let snippet = crate::util::truncate(desc, 150);
                let _ = writeln!(out, "   _{}_", snippet);
            }
        }

        if let Some(url) = &park.url {
            let _ = writeln!(out, "   [nps.gov/{}]({})", park.park_code, url);
        }
        out.push('\n');
    }

    let _ = writeln!(
        out,
        "_Source: National Park Service API. Last updated: {}_",
        crate::util::now_pacific().format("%-I:%M %p")
    );
    out
}

/// Summarize standard hours from the NPS JSON value.
///
/// The `standardHours` field is an object like `{"sunday": "All Day", "monday": "7:30AM - 8:00PM", ...}`.
/// We collapse identical days into ranges for concise output.
fn summarize_hours(hours: &NpsHours) -> String {
    let Some(std_hours) = &hours.standard_hours else {
        return hours
            .description
            .as_deref()
            .unwrap_or("Hours not available")
            .to_string();
    };

    let Some(obj) = std_hours.as_object() else {
        return hours
            .description
            .as_deref()
            .unwrap_or("Hours not available")
            .to_string();
    };

    let days = [
        "sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday",
    ];

    let values: Vec<String> = days
        .iter()
        .map(|d| {
            obj.get(*d)
                .and_then(|v| v.as_str())
                .unwrap_or("Closed")
                .to_string()
        })
        .collect();

    // Check if all days are the same
    if values.iter().all(|v| v == &values[0]) {
        let h = &values[0];
        if h.eq_ignore_ascii_case("All Day") {
            return "Open 24/7".to_string();
        }
        return format!("{} daily", h);
    }

    // Build a compact summary by grouping consecutive identical hours
    let day_abbrs = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let mut parts = Vec::new();
    let mut i = 0;
    while i < 7 {
        let mut j = i;
        while j + 1 < 7 && values[j + 1] == values[i] {
            j += 1;
        }
        let range = if i == j {
            day_abbrs[i].to_string()
        } else {
            format!("{}-{}", day_abbrs[i], day_abbrs[j])
        };
        let h = &values[i];
        if h.eq_ignore_ascii_case("All Day") {
            parts.push(format!("{}: Open 24 hours", range));
        } else {
            parts.push(format!("{}: {}", range, h));
        }
        i = j + 1;
    }

    parts.join("; ")
}

// ─── Tests ───

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_park() -> NpsPark {
        NpsPark {
            full_name: "Pinnacles National Park".to_string(),
            park_code: "pinn".to_string(),
            description: Some(
                "Some of the most unique rock formations in the world."
                    .to_string(),
            ),
            latitude: Some("36.49".to_string()),
            longitude: Some("-121.18".to_string()),
            states: Some("CA".to_string()),
            url: Some("https://www.nps.gov/pinn/index.htm".to_string()),
            directions_info: Some("From Santa Cruz, take Highway 101 south.".to_string()),
            weather_info: Some("Hot and dry summers, mild winters.".to_string()),
            operating_hours: vec![NpsHours {
                name: Some("East Entrance".to_string()),
                description: Some("Open all day".to_string()),
                standard_hours: Some(serde_json::json!({
                    "sunday": "All Day",
                    "monday": "All Day",
                    "tuesday": "All Day",
                    "wednesday": "All Day",
                    "thursday": "All Day",
                    "friday": "All Day",
                    "saturday": "All Day"
                })),
            }],
            entrance_fees: vec![NpsFee {
                cost: Some("30.00".to_string()),
                description: Some("Per vehicle".to_string()),
                title: Some("Private Vehicle".to_string()),
            }],
            entrance_passes: vec![NpsPass {
                cost: Some("55.00".to_string()),
                title: Some("Annual Pass".to_string()),
            }],
            activities: vec![
                NpsActivity {
                    name: "Hiking".to_string(),
                },
                NpsActivity {
                    name: "Rock Climbing".to_string(),
                },
                NpsActivity {
                    name: "Bird Watching".to_string(),
                },
            ],
            contacts: Some(NpsContacts {
                phone_numbers: vec![NpsPhone {
                    phone_number: "(831) 389-4486".to_string(),
                }],
                email_addresses: vec![NpsEmail {
                    email_address: "pinn_visitor_information@nps.gov".to_string(),
                }],
            }),
        }
    }

    #[test]
    fn parse_nps_response() {
        let json = r#"{
            "total": "1",
            "data": [{
                "fullName": "Pinnacles National Park",
                "parkCode": "pinn",
                "description": "Talus caves and rock spires.",
                "latitude": "36.49",
                "longitude": "-121.18",
                "states": "CA",
                "url": "https://www.nps.gov/pinn/index.htm",
                "directionsInfo": "Take Hwy 101.",
                "weatherInfo": "Hot summers.",
                "operatingHours": [{
                    "name": "East Entrance",
                    "description": "Open all day",
                    "standardHours": {
                        "sunday": "All Day",
                        "monday": "All Day",
                        "tuesday": "All Day",
                        "wednesday": "All Day",
                        "thursday": "All Day",
                        "friday": "All Day",
                        "saturday": "All Day"
                    }
                }],
                "entranceFees": [{
                    "cost": "30.00",
                    "description": "Per vehicle",
                    "title": "Private Vehicle"
                }],
                "entrancePasses": [{
                    "cost": "55.00",
                    "title": "Annual Pass"
                }],
                "activities": [
                    {"name": "Hiking"},
                    {"name": "Rock Climbing"}
                ],
                "contacts": {
                    "phoneNumbers": [{"phoneNumber": "(831) 389-4486"}],
                    "emailAddresses": [{"emailAddress": "pinn@nps.gov"}]
                },
                "images": [{"url": "https://example.com/img.jpg", "title": "View", "caption": "A view"}]
            }]
        }"#;

        let resp: NpsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.total, "1");
        assert_eq!(resp.data.len(), 1);
        assert_eq!(resp.data[0].full_name, "Pinnacles National Park");
        assert_eq!(resp.data[0].park_code, "pinn");
        assert_eq!(resp.data[0].entrance_fees.len(), 1);
        assert_eq!(
            resp.data[0].entrance_fees[0].cost.as_deref(),
            Some("30.00")
        );
        assert_eq!(resp.data[0].activities.len(), 2);
        assert_eq!(resp.data[0].activities[0].name, "Hiking");
        assert!(resp.data[0].contacts.is_some());
        let contacts = resp.data[0].contacts.as_ref().unwrap();
        assert_eq!(contacts.phone_numbers[0].phone_number, "(831) 389-4486");
    }

    #[test]
    fn format_single_park_renders_all_sections() {
        let park = mock_park();
        let output = super::format_single_park(&park);

        assert!(output.contains("# Pinnacles National Park"));
        assert!(output.contains("Located in CA"));
        assert!(output.contains("nps.gov/pinn"));
        assert!(output.contains("## Description"));
        assert!(output.contains("unique rock formations"));
        assert!(output.contains("## Hours"));
        assert!(output.contains("East Entrance"));
        assert!(output.contains("Open 24/7"));
        assert!(output.contains("## Fees"));
        assert!(output.contains("$30.00"));
        assert!(output.contains("$55.00"));
        assert!(output.contains("## Activities"));
        assert!(output.contains("Hiking, Rock Climbing, Bird Watching"));
        assert!(output.contains("## Directions"));
        assert!(output.contains("## Weather"));
        assert!(output.contains("## Contact"));
        assert!(output.contains("(831) 389-4486"));
        assert!(output.contains("pinn_visitor_information@nps.gov"));
        assert!(output.contains("_Source: National Park Service API."));
    }

    #[test]
    fn format_search_results_renders_list() {
        let response = NpsResponse {
            total: "2".to_string(),
            data: vec![
                NpsPark {
                    full_name: "Pinnacles National Park".to_string(),
                    park_code: "pinn".to_string(),
                    description: Some("Talus caves and rock spires.".to_string()),
                    latitude: None,
                    longitude: None,
                    states: Some("CA".to_string()),
                    url: Some("https://www.nps.gov/pinn/index.htm".to_string()),
                    directions_info: None,
                    weather_info: None,
                    operating_hours: vec![],
                    entrance_fees: vec![],
                    entrance_passes: vec![],
                    activities: vec![],
                    contacts: None,
                },
                NpsPark {
                    full_name: "Yosemite National Park".to_string(),
                    park_code: "yose".to_string(),
                    description: Some("Granite cliffs and giant sequoias.".to_string()),
                    latitude: None,
                    longitude: None,
                    states: Some("CA".to_string()),
                    url: Some("https://www.nps.gov/yose/index.htm".to_string()),
                    directions_info: None,
                    weather_info: None,
                    operating_hours: vec![],
                    entrance_fees: vec![],
                    entrance_passes: vec![],
                    activities: vec![],
                    contacts: None,
                },
            ],
        };

        let output = super::format_search_results(&response);

        assert!(output.contains("2 results"));
        assert!(output.contains("1. **Pinnacles National Park** (pinn) — CA"));
        assert!(output.contains("2. **Yosemite National Park** (yose) — CA"));
        assert!(output.contains("Talus caves"));
        assert!(output.contains("Granite cliffs"));
        assert!(output.contains("nps.gov/pinn"));
        assert!(output.contains("nps.gov/yose"));
        assert!(output.contains("_Source: National Park Service API."));
    }

    #[test]
    fn summarize_hours_all_day() {
        let hours = NpsHours {
            name: Some("Main Gate".to_string()),
            description: None,
            standard_hours: Some(serde_json::json!({
                "sunday": "All Day",
                "monday": "All Day",
                "tuesday": "All Day",
                "wednesday": "All Day",
                "thursday": "All Day",
                "friday": "All Day",
                "saturday": "All Day"
            })),
        };
        assert_eq!(summarize_hours(&hours), "Open 24/7");
    }

    #[test]
    fn summarize_hours_mixed() {
        let hours = NpsHours {
            name: Some("West Side".to_string()),
            description: None,
            standard_hours: Some(serde_json::json!({
                "sunday": "7:30AM - 8:00PM",
                "monday": "7:30AM - 8:00PM",
                "tuesday": "7:30AM - 8:00PM",
                "wednesday": "7:30AM - 8:00PM",
                "thursday": "7:30AM - 8:00PM",
                "friday": "7:30AM - 8:00PM",
                "saturday": "7:30AM - 8:00PM"
            })),
        };
        assert_eq!(summarize_hours(&hours), "7:30AM - 8:00PM daily");
    }

    #[test]
    fn summarize_hours_no_standard_hours() {
        let hours = NpsHours {
            name: Some("Visitor Center".to_string()),
            description: Some("Open seasonally".to_string()),
            standard_hours: None,
        };
        assert_eq!(summarize_hours(&hours), "Open seasonally");
    }

    #[test]
    fn format_empty_response() {
        let response = NpsResponse {
            total: "0".to_string(),
            data: vec![],
        };
        let output = format_response(&response);
        assert!(output.contains("No national parks found"));
    }

    #[test]
    fn format_single_result_shows_detail() {
        let response = NpsResponse {
            total: "1".to_string(),
            data: vec![mock_park()],
        };
        let output = format_response(&response);
        // Single result should show full detail, not summary list
        assert!(output.contains("# Pinnacles National Park"));
        assert!(output.contains("## Description"));
        assert!(!output.contains("1. **Pinnacles"));
    }
}
