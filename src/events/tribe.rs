use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::util;

const EVENTS_API_URL: &str =
    "https://events.ucsc.edu/wp-json/tribe/events/v1/events";

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

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum TribeVenueField {
    Venue(TribeVenue),
    Empty(Vec<()>),
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

    pub async fn fetch_events(
        &self,
        params: &[(&str, &str)],
    ) -> Result<TribeEventsResponse> {
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
    pub fn venue_name(&self) -> Option<&str> {
        match &self.venue {
            TribeVenueField::Venue(v) => Some(&v.venue),
            TribeVenueField::Empty(_) => None,
        }
    }

    pub fn venue_location(&self) -> Option<String> {
        match &self.venue {
            TribeVenueField::Venue(v) => {
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
            TribeVenueField::Empty(_) => None,
        }
    }

    pub fn format_summary(&self) -> String {
        let mut out = format!("## {}\n", self.title);
        out.push_str(&format!("- **When**: {} to {}\n", self.start_date, self.end_date));

        if let Some(venue) = self.venue_name() {
            out.push_str(&format!("- **Where**: {}", venue));
            if let Some(loc) = self.venue_location() {
                out.push_str(&format!(" ({})", loc));
            }
            out.push('\n');
        }

        if let Some(cost) = &self.cost {
            if !cost.is_empty() {
                out.push_str(&format!("- **Cost**: {}\n", cost));
            }
        }

        if !self.categories.is_empty() {
            let cats: Vec<&str> = self.categories.iter().map(|c| c.name.as_str()).collect();
            out.push_str(&format!("- **Category**: {}\n", cats.join(", ")));
        }

        if !self.organizer.is_empty() {
            let orgs: Vec<&str> = self.organizer.iter().map(|o| o.organizer.as_str()).collect();
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
