use std::sync::LazyLock;

use anyhow::{Context, Result};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};

use crate::util;

const EVENTBRITE_BASE: &str = "https://www.eventbrite.com";

static SEL_EVENT_ID: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("[data-event-id]").expect("hardcoded selector"));
static SEL_EVENT_HREF: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("a[href*='/e/']").expect("hardcoded selector"));

// ─── Step 1: Scrape event IDs from the Eventbrite discover page ───

/// Build a discover URL like `https://www.eventbrite.com/d/ca--santa-cruz/music/`
fn build_discover_url(location_slug: &str, query: Option<&str>) -> String {
    let q = query.unwrap_or("events");
    // Replace spaces with dashes for URL
    let q_slug: String = q
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
        .to_lowercase();
    format!("{}/d/{}/{}/", EVENTBRITE_BASE, location_slug, q_slug)
}

/// Convert a location string like "Santa Cruz, CA" to a slug like "ca--santa-cruz".
pub fn location_to_slug(location: &str) -> String {
    // Expected formats: "Santa Cruz, CA" or "CA, Santa Cruz" or just "Santa Cruz"
    let parts: Vec<&str> = location.split(',').map(|s| s.trim()).collect();

    let (city, state) = if parts.len() >= 2 {
        // Try to detect which part is the state (2 chars)
        if parts[1].len() <= 3 {
            (parts[0], Some(parts[1]))
        } else if parts[0].len() <= 3 {
            (parts[1], Some(parts[0]))
        } else {
            (parts[0], Some(parts[1]))
        }
    } else {
        (parts[0], None)
    };

    let city_slug = city.to_lowercase().replace(' ', "-");
    match state {
        Some(s) => format!("{}--{}", s.to_lowercase(), city_slug),
        None => city_slug,
    }
}

/// Scrape event IDs from an Eventbrite discover page HTML.
fn extract_event_ids(html: &str) -> Vec<String> {
    let document = Html::parse_document(html);

    // Try multiple selectors — Eventbrite changes their DOM
    let mut ids = Vec::new();

    // Primary: data-event-id on links
    for el in document.select(&SEL_EVENT_ID) {
        if let Some(id) = el.value().attr("data-event-id") {
            if !id.is_empty() && !ids.contains(&id.to_string()) {
                ids.push(id.to_string());
            }
        }
    }

    // Fallback: parse event IDs from href patterns like /e/...-tickets-1234567890
    if ids.is_empty() {
        for el in document.select(&SEL_EVENT_HREF) {
            if let Some(href) = el.value().attr("href") {
                if let Some(id) = extract_id_from_href(href) {
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
            }
        }
    }

    ids
}

/// Extract event ID from href like "/e/event-name-tickets-1234567890" or full URL.
fn extract_id_from_href(href: &str) -> Option<String> {
    // Look for the numeric ID at the end of the URL path
    let path = href.split('?').next().unwrap_or(href);
    let last_segment = path.rsplit('/').next()?;

    // Pattern: "event-name-tickets-1234567890" or just "1234567890"
    if let Some(pos) = last_segment.rfind('-') {
        let candidate = &last_segment[pos + 1..];
        if candidate.len() >= 6 && candidate.chars().all(|c| c.is_ascii_digit()) {
            return Some(candidate.to_string());
        }
    }

    // Maybe the whole segment is an ID
    if last_segment.len() >= 6 && last_segment.chars().all(|c| c.is_ascii_digit()) {
        return Some(last_segment.to_string());
    }

    None
}

// ─── Step 2: Fetch event details from destination API ───

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DestinationResponse {
    pub events: Vec<Event>,
    pub pagination: Option<DestinationPagination>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DestinationPagination {
    pub object_count: Option<u32>,
    pub page_count: Option<u32>,
    pub page_number: Option<u32>,
    pub has_more_items: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Event {
    pub id: String,
    pub name: String,
    pub summary: Option<String>,
    pub url: String, // Direct registration deep link
    pub start_date: Option<String>,
    pub start_time: Option<String>,
    pub end_date: Option<String>,
    pub end_time: Option<String>,
    pub timezone: Option<String>,
    pub is_online_event: Option<bool>,
    pub status: Option<String>,
    pub is_cancelled: Option<bool>,
    pub primary_venue: Option<Venue>,
    pub primary_organizer: Option<Organizer>,
    pub ticket_availability: Option<TicketAvailability>,
    pub image: Option<Image>,
    pub tags: Option<Vec<Tag>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Venue {
    pub name: Option<String>,
    pub id: Option<String>,
    pub address: Option<Address>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Address {
    pub address_1: Option<String>,
    pub city: Option<String>,
    pub region: Option<String>,
    pub postal_code: Option<String>,
    pub localized_address_display: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Organizer {
    pub name: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TicketAvailability {
    pub is_free: Option<bool>,
    pub is_sold_out: Option<bool>,
    pub has_available_tickets: Option<bool>,
    pub minimum_ticket_price: Option<Price>,
    pub maximum_ticket_price: Option<Price>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Price {
    pub display: Option<String>,
    pub major_value: Option<String>,
    pub currency: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Image {
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Tag {
    pub display_name: Option<String>,
    pub tag: Option<String>,
}

// ─── Client ───

pub struct EventbriteClient {
    http: reqwest::Client,
}

impl EventbriteClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    /// Search Eventbrite for events near a location.
    /// Two-step: scrape discover page for IDs, then fetch details via destination API.
    pub async fn search_events(
        &self,
        query: Option<&str>,
        location_slug: &str,
        limit: u32,
    ) -> Result<Vec<Event>> {
        // Step 1: Scrape event IDs from the discover page
        let discover_url = build_discover_url(location_slug, query);
        let html = self
            .http
            .get(&discover_url)
            .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)")
            .send()
            .await
            .context("failed to fetch Eventbrite discover page")?
            .text()
            .await
            .context("failed to read Eventbrite discover page")?;

        let mut event_ids = extract_event_ids(&html);
        if event_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Limit IDs to requested amount
        event_ids.truncate(limit as usize);

        // Step 2: Fetch full details via destination events API
        let ids_param = event_ids.join(",");
        let detail_url = format!("{}/api/v3/destination/events/", EVENTBRITE_BASE);

        let resp = self
            .http
            .get(&detail_url)
            .query(&[
                ("event_ids", ids_param.as_str()),
                ("page_size", &limit.to_string()),
                (
                    "expand",
                    "image,primary_venue,ticket_availability,primary_organizer",
                ),
            ])
            .send()
            .await
            .context("failed to fetch Eventbrite event details")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Eventbrite detail API returned {}: {}", status, &body[..body.len().min(200)]);
        }

        let data: DestinationResponse = resp
            .json()
            .await
            .context("failed to parse Eventbrite event details")?;

        Ok(data.events)
    }
}

// ─── Formatting ───

impl Event {
    pub fn format_summary(&self) -> String {
        let mut out = format!("## {}\n", self.name);

        // Date/time
        if let (Some(sd), Some(st)) = (&self.start_date, &self.start_time) {
            let start = format_date_time(sd, st);
            if let (Some(ed), Some(et)) = (&self.end_date, &self.end_time) {
                let end = if ed == sd {
                    format_time(et)
                } else {
                    format_date_time(ed, et)
                };
                out.push_str(&format!("- **When**: {} to {}\n", start, end));
            } else {
                out.push_str(&format!("- **When**: {}\n", start));
            }
        }

        // Location
        if self.is_online_event == Some(true) {
            out.push_str("- **Where**: Online Event\n");
        } else if let Some(venue) = &self.primary_venue {
            let name = venue.name.as_deref().unwrap_or("TBD");
            out.push_str(&format!("- **Where**: {}", name));
            if let Some(addr) = &venue.address {
                if let Some(display) = &addr.localized_address_display {
                    out.push_str(&format!(" ({})", display));
                }
            }
            out.push('\n');
        }

        // Cost
        if let Some(ta) = &self.ticket_availability {
            if ta.is_free == Some(true) {
                out.push_str("- **Cost**: Free\n");
            } else if let Some(min) = &ta.minimum_ticket_price {
                if let Some(max) = &ta.maximum_ticket_price {
                    let min_d = min.display.as_deref().unwrap_or("?");
                    let max_d = max.display.as_deref().unwrap_or("?");
                    if min_d == max_d {
                        out.push_str(&format!("- **Cost**: {}\n", min_d));
                    } else {
                        out.push_str(&format!("- **Cost**: {} – {}\n", min_d, max_d));
                    }
                }
            }
            if ta.is_sold_out == Some(true) {
                out.push_str("- **Status**: SOLD OUT\n");
            }
        }

        // Organizer
        if let Some(org) = &self.primary_organizer {
            if let Some(name) = &org.name {
                out.push_str(&format!("- **Organizer**: {}\n", name));
            }
        }

        // Tags/categories
        if let Some(tags) = &self.tags {
            let names: Vec<&str> = tags
                .iter()
                .filter_map(|t| t.display_name.as_deref())
                .collect();
            if !names.is_empty() {
                out.push_str(&format!("- **Category**: {}\n", names.join(", ")));
            }
        }

        // Registration deep link
        out.push_str(&format!("- **Register**: {}\n", self.url));
        out.push_str("- **Source**: Eventbrite\n");

        // Description
        if let Some(summary) = &self.summary {
            let trimmed = util::truncate(summary.trim(), 300);
            if !trimmed.is_empty() {
                out.push_str(&format!("- **Description**: {}\n", trimmed));
            }
        }

        out
    }
}

/// Format "2026-04-15" + "19:00" → "Apr 15, 2026 7:00 PM"
fn format_date_time(date: &str, time: &str) -> String {
    let combined = format!("{}T{}", date, time);
    chrono::NaiveDateTime::parse_from_str(&combined, "%Y-%m-%dT%H:%M")
        .map(|dt| dt.format("%b %-d, %Y %-I:%M %p").to_string())
        .unwrap_or_else(|_| format!("{} {}", date, time))
}

/// Format just a time string "19:00" → "7:00 PM"
fn format_time(time: &str) -> String {
    chrono::NaiveTime::parse_from_str(time, "%H:%M")
        .map(|t| t.format("%-I:%M %p").to_string())
        .unwrap_or_else(|_| time.to_string())
}

// ─── Tests ───

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_location_to_slug() {
        assert_eq!(location_to_slug("Santa Cruz, CA"), "ca--santa-cruz");
        assert_eq!(location_to_slug("San Francisco, CA"), "ca--san-francisco");
        assert_eq!(location_to_slug("Santa Cruz"), "santa-cruz");
    }

    #[test]
    fn test_build_discover_url() {
        assert_eq!(
            build_discover_url("ca--santa-cruz", Some("music")),
            "https://www.eventbrite.com/d/ca--santa-cruz/music/"
        );
        assert_eq!(
            build_discover_url("ca--santa-cruz", None),
            "https://www.eventbrite.com/d/ca--santa-cruz/events/"
        );
        assert_eq!(
            build_discover_url("ca--santa-cruz", Some("tech meetup")),
            "https://www.eventbrite.com/d/ca--santa-cruz/tech-meetup/"
        );
    }

    #[test]
    fn test_extract_id_from_href() {
        assert_eq!(
            extract_id_from_href("/e/rust-meetup-tickets-1234567890"),
            Some("1234567890".to_string())
        );
        assert_eq!(
            extract_id_from_href("https://www.eventbrite.com/e/event-name-tickets-9876543210"),
            Some("9876543210".to_string())
        );
        assert_eq!(extract_id_from_href("/e/short"), None);
    }

    #[test]
    fn test_extract_event_ids_from_html() {
        let html = r#"
        <html><body>
            <a data-event-id="111222333" href="/e/test-tickets-111222333">Event 1</a>
            <a data-event-id="444555666" href="/e/test2-tickets-444555666">Event 2</a>
            <a data-event-id="111222333" href="/e/test-tickets-111222333">Event 1 dup</a>
        </body></html>"#;
        let ids = extract_event_ids(html);
        assert_eq!(ids, vec!["111222333", "444555666"]);
    }

    #[test]
    fn test_extract_event_ids_fallback() {
        // No data-event-id, but has href with IDs
        let html = r#"
        <html><body>
            <a href="/e/cool-event-tickets-9876543210">Cool Event</a>
            <a href="/e/another-event-tickets-1234567890">Another</a>
        </body></html>"#;
        let ids = extract_event_ids(html);
        assert_eq!(ids, vec!["9876543210", "1234567890"]);
    }

    #[test]
    fn test_format_date_time() {
        assert_eq!(
            format_date_time("2026-04-15", "19:00"),
            "Apr 15, 2026 7:00 PM"
        );
        assert_eq!(
            format_date_time("2026-12-25", "09:30"),
            "Dec 25, 2026 9:30 AM"
        );
    }

    #[test]
    fn test_format_summary() {
        let event = Event {
            id: "123".into(),
            name: "Rust Meetup".into(),
            summary: Some("Learn Rust with us!".into()),
            url: "https://www.eventbrite.com/e/rust-meetup-tickets-123".into(),
            start_date: Some("2024-03-15".into()),
            start_time: Some("18:00".into()),
            end_date: Some("2024-03-15".into()),
            end_time: Some("20:00".into()),
            timezone: Some("America/Los_Angeles".into()),
            is_online_event: Some(false),
            status: Some("live".into()),
            is_cancelled: None,
            primary_venue: Some(Venue {
                id: Some("456".into()),
                name: Some("Santa Cruz Tech Hub".into()),
                address: Some(Address {
                    address_1: Some("123 Pacific Ave".into()),
                    city: Some("Santa Cruz".into()),
                    region: Some("CA".into()),
                    postal_code: Some("95060".into()),
                    localized_address_display: Some("123 Pacific Ave, Santa Cruz, CA".into()),
                }),
            }),
            primary_organizer: Some(Organizer {
                name: Some("SC Tech Group".into()),
                url: None,
            }),
            ticket_availability: Some(TicketAvailability {
                is_free: Some(true),
                is_sold_out: Some(false),
                has_available_tickets: Some(true),
                minimum_ticket_price: None,
                maximum_ticket_price: None,
            }),
            image: None,
            tags: Some(vec![Tag {
                display_name: Some("Science & Technology".into()),
                tag: Some("EventbriteCategory/102".into()),
            }]),
        };

        let summary = event.format_summary();
        assert!(summary.contains("## Rust Meetup"));
        assert!(summary.contains("Register**: https://www.eventbrite.com/e/rust-meetup-tickets-123"));
        assert!(summary.contains("Santa Cruz Tech Hub"));
        assert!(summary.contains("**Cost**: Free"));
        assert!(summary.contains("**Source**: Eventbrite"));
        assert!(summary.contains("SC Tech Group"));
    }
}
