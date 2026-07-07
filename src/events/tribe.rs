use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::util;

const EVENTS_API_URL: &str = "https://events.ucsc.edu/wp-json/tribe/events/v1/events";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TribeEventsResponse {
    pub events: Vec<TribeEvent>,
    pub total: u64,
    pub total_pages: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TribeEvent {
    pub id: u64,
    pub title: String,
    pub description: Option<String>,
    pub url: String,
    pub start_date: String,
    pub end_date: String,
    pub all_day: bool,
    pub cost: Option<String>,
    pub venue: TribeVenueField,
    pub organizer: Vec<TribeOrganizer>,
    pub categories: Vec<TribeCategory>,
    pub tags: Vec<TribeTag>,
}

/// The Tribe API is inconsistent about `venue`: a single object when the event
/// has one venue, an **array** of venue objects for multi-venue events, or an
/// empty array `[]` when there's none. `Venues(Vec<_>)` covers both array
/// shapes (including empty); `Venue` covers the single-object shape.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum TribeVenueField {
    Venue(TribeVenue),
    Venues(Vec<TribeVenue>),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TribeVenue {
    pub venue: String,
    pub address: Option<String>,
    pub city: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TribeOrganizer {
    pub organizer: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TribeCategory {
    pub name: String,
    pub slug: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TribeTag {
    pub name: String,
}

pub struct TribeClient {
    http: reqwest::Client,
}

impl TribeClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    pub async fn fetch_events(&self, params: &[(&str, &str)]) -> Result<TribeEventsResponse> {
        let resp = self
            .http
            .get(EVENTS_API_URL)
            .query(params)
            .send()
            .await?
            .error_for_status()?;
        let data: TribeEventsResponse = resp.json().await?;
        Ok(data)
    }
}

impl TribeEvent {
    /// First venue, regardless of whether the API returned a single object or
    /// an array. `None` for an empty array.
    fn first_venue(&self) -> Option<&TribeVenue> {
        match &self.venue {
            TribeVenueField::Venue(v) => Some(v),
            TribeVenueField::Venues(vs) => vs.first(),
        }
    }

    pub fn venue_name(&self) -> Option<&str> {
        self.first_venue().map(|v| v.venue.as_str())
    }

    pub fn venue_location(&self) -> Option<String> {
        let v = self.first_venue()?;
        let parts: Vec<&str> = [v.address.as_deref(), v.city.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(", "))
        }
    }

    pub fn format_summary(&self) -> String {
        let mut out = format!("## {}\n", self.title);
        out.push_str(&format!(
            "- **When**: {} to {}\n",
            self.start_date, self.end_date
        ));

        if let Some(venue) = self.venue_name() {
            out.push_str(&format!("- **Where**: {}", venue));
            if let Some(loc) = self.venue_location() {
                out.push_str(&format!(" ({})", loc));
            }
            out.push('\n');
        }

        if let Some(cost) = &self.cost
            && !cost.is_empty()
        {
            out.push_str(&format!("- **Cost**: {}\n", cost));
        }

        if !self.categories.is_empty() {
            let cats: Vec<&str> = self.categories.iter().map(|c| c.name.as_str()).collect();
            out.push_str(&format!("- **Category**: {}\n", cats.join(", ")));
        }

        if !self.organizer.is_empty() {
            let orgs: Vec<&str> = self
                .organizer
                .iter()
                .map(|o| o.organizer.as_str())
                .collect();
            out.push_str(&format!("- **Organizer**: {}\n", orgs.join(", ")));
        }

        out.push_str(&format!("- **Link**: {}\n", self.url));

        if let Some(desc) = &self.description {
            let clean = util::strip_html_tags(desc);
            let trimmed = util::truncate(&clean, 300);
            if !trimmed.is_empty() {
                out.push_str(&format!("- **Description**: {}\n", trimmed));
            }
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRIBE_FIXTURE: &str = include_str!("fixtures/tribe_events.json");

    #[test]
    fn deserialize_tribe_response() {
        let resp: TribeEventsResponse = serde_json::from_str(TRIBE_FIXTURE).unwrap();
        assert_eq!(resp.total, 2);
        assert_eq!(resp.total_pages, 1);
        assert_eq!(resp.events.len(), 2);
    }

    #[test]
    fn event_with_full_venue_multiple_organizers_and_categories() {
        let resp: TribeEventsResponse = serde_json::from_str(TRIBE_FIXTURE).unwrap();
        let ev = &resp.events[0];
        assert_eq!(ev.id, 90210);
        assert_eq!(ev.venue_name(), Some("Lick Observatory"));
        assert_eq!(
            ev.venue_location().as_deref(),
            Some("7281 Mount Hamilton Rd, Mount Hamilton")
        );
        assert_eq!(ev.organizer.len(), 2);
        assert_eq!(ev.categories.len(), 2);
        assert_eq!(ev.tags.len(), 2);

        let summary = ev.format_summary();
        // venue line includes name + (address, city)
        assert!(
            summary
                .contains("**Where**: Lick Observatory (7281 Mount Hamilton Rd, Mount Hamilton)")
        );
        // multiple organizers are comma-joined
        assert!(summary.contains(
            "**Organizer**: Department of Astronomy & Astrophysics, UCSC Public Programs"
        ));
        // multiple categories are comma-joined
        assert!(summary.contains("**Category**: Science, Community"));
        assert!(summary.contains("**Cost**: Free"));
        // HTML in description is stripped
        assert!(summary.contains("**Description**: Join us for an evening of astronomy"));
        assert!(!summary.contains("<strong>"));
    }

    #[test]
    fn event_with_empty_venue_variant() {
        let resp: TribeEventsResponse = serde_json::from_str(TRIBE_FIXTURE).unwrap();
        let ev = &resp.events[1];
        assert_eq!(ev.id, 90211);
        // "venue": [] deserializes to an empty Venues array → no name/location.
        assert!(matches!(&ev.venue, TribeVenueField::Venues(v) if v.is_empty()));
        assert_eq!(ev.venue_name(), None);
        assert_eq!(ev.venue_location(), None);

        let summary = ev.format_summary();
        // No venue line when venue is empty.
        assert!(!summary.contains("**Where**"));
        // Empty cost string is suppressed.
        assert!(!summary.contains("**Cost**"));
        // null description → no description line.
        assert!(!summary.contains("**Description**"));
        // single organizer still rendered
        assert!(summary.contains("**Organizer**: Career Success"));
    }

    #[test]
    fn event_with_venue_as_nonempty_array() {
        // Multi-venue events return `venue` as an ARRAY of objects, not a single
        // object — this previously failed deserialization ("data did not match
        // any variant of untagged enum TribeVenueField"). venue_name() takes the
        // first.
        let json = r#"{
            "events": [{
                "id": 1, "title": "Multi-venue", "description": null,
                "url": "https://e/1", "start_date": "2026-06-12 10:00:00",
                "end_date": "2026-06-12 12:00:00", "all_day": false, "cost": "",
                "venue": [
                    {"venue": "Quarry Amphitheater", "address": "1156 High St", "city": "Santa Cruz"},
                    {"venue": "Music Center", "address": null, "city": null}
                ],
                "organizer": [], "categories": [], "tags": []
            }],
            "total": 1, "total_pages": 1
        }"#;
        let resp: TribeEventsResponse = serde_json::from_str(json).expect("multi-venue must parse");
        let ev = &resp.events[0];
        assert!(matches!(&ev.venue, TribeVenueField::Venues(v) if v.len() == 2));
        assert_eq!(ev.venue_name(), Some("Quarry Amphitheater"));
        assert_eq!(
            ev.venue_location().as_deref(),
            Some("1156 High St, Santa Cruz")
        );
    }

    #[test]
    fn wp_json_error_envelope_errors_gracefully() {
        // WordPress REST errors are a bare {code, message, data} object with
        // no `events` key — deserialization must fail cleanly (missing field),
        // never panic or produce a phantom empty response.
        let body = r#"{
            "code": "rest_no_route",
            "message": "No route was found matching the URL and request method.",
            "data": { "status": 404 }
        }"#;
        let err = serde_json::from_str::<TribeEventsResponse>(body)
            .unwrap_err()
            .to_string();
        assert!(err.contains("events"), "got: {err}");
    }

    #[test]
    fn empty_events_page_parses() {
        // A search with no hits still returns the envelope with events: [].
        let body = r#"{"events": [], "total": 0, "total_pages": 0}"#;
        let resp: TribeEventsResponse = serde_json::from_str(body).unwrap();
        assert!(resp.events.is_empty());
        assert_eq!(resp.total, 0);
    }

    #[test]
    fn truncated_json_errors_gracefully() {
        let cut = &TRIBE_FIXTURE[..TRIBE_FIXTURE.len() / 2];
        assert!(serde_json::from_str::<TribeEventsResponse>(cut).is_err());
    }

    #[test]
    fn event_missing_venue_key_errors() {
        // The live API always sends `venue` ([] when absent); if the key ever
        // disappears the parse fails with a clear missing-field error rather
        // than fabricating a venue.
        let body = r#"{
            "events": [{
                "id": 3, "title": "No venue key", "description": null,
                "url": "https://e/3", "start_date": "2026-06-12 10:00:00",
                "end_date": "2026-06-12 12:00:00", "all_day": false, "cost": null,
                "organizer": [], "categories": [], "tags": []
            }],
            "total": 1, "total_pages": 1
        }"#;
        let err = serde_json::from_str::<TribeEventsResponse>(body)
            .unwrap_err()
            .to_string();
        assert!(err.contains("venue"), "got: {err}");
    }

    #[test]
    fn event_with_single_venue_object() {
        // The single-object shape still works (the common case).
        let json = r#"{
            "events": [{
                "id": 2, "title": "Single", "description": null, "url": "https://e/2",
                "start_date": "2026-06-12 10:00:00", "end_date": "2026-06-12 12:00:00",
                "all_day": false, "cost": null,
                "venue": {"venue": "McHenry Library", "address": null, "city": "Santa Cruz"},
                "organizer": [], "categories": [], "tags": []
            }],
            "total": 1, "total_pages": 1
        }"#;
        let resp: TribeEventsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.events[0].venue_name(), Some("McHenry Library"));
    }
}
