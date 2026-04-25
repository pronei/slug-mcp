use std::fmt::Write;

use anyhow::{Context, Result};
use scraper::Html;

use super::locations::BuildingLocation;
use crate::util::selectors;

selectors! {
    SEL_POST_ITEM => "li.wp-block-post",
    SEL_TITLE_LINK => "h2.wp-block-post-title a",
}

const CLASSROOMS_URL: &str = "https://classrooms.ucsc.edu/classroomlist/";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Classroom {
    pub name: String,
    pub url: String,
    pub capacity: Option<u32>,
    pub seating_style: Option<String>,
    pub area: Option<String>,
    pub technology: Vec<String>,
    pub physical_features: Vec<String>,
}

impl Classroom {
    /// Format classroom info with optional location data from the buildings index.
    pub fn format_with_location(&self, location: Option<&BuildingLocation>) -> String {
        let mut out = format!("### {}", self.name);

        // Location info first (most useful for wayfinding)
        if let Some(loc) = location {
            let _ = write!(out, "\n{}", loc.format_location());
        }

        if let Some(cap) = self.capacity {
            let _ = write!(out, "\n- **Capacity**: {}", cap);
        }
        if let Some(style) = &self.seating_style {
            let _ = write!(out, "\n- **Seating**: {}", humanize(style));
        }
        // Only show raw area if no location data was found
        if location.is_none() {
            if let Some(area) = &self.area {
                let _ = write!(out, "\n- **Area**: {}", humanize(area));
            }
        }
        if !self.technology.is_empty() {
            let techs: Vec<String> = self.technology.iter().map(|t| humanize(t)).collect();
            let _ = write!(out, "\n- **Technology**: {}", techs.join(", "));
        }
        if !self.physical_features.is_empty() {
            let feats: Vec<String> = self.physical_features.iter().map(|t| humanize(t)).collect();
            let _ = write!(out, "\n- **Features**: {}", feats.join(", "));
        }

        out
    }
}

/// Convert kebab-case CSS class suffix to human-readable title case
fn humanize(s: &str) -> String {
    s.split('-')
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(c) => {
                    let upper: String = c.to_uppercase().collect();
                    format!("{}{}", upper, chars.as_str())
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub async fn scrape_classrooms(client: &reqwest::Client) -> Result<Vec<Classroom>> {
    let resp = client
        .get(CLASSROOMS_URL)
        .send()
        .await
        .context("Failed to fetch classrooms page")?;

    let html = resp.text().await.context("Failed to read classrooms body")?;
    Ok(parse_classrooms(&html))
}

fn parse_classrooms(html: &str) -> Vec<Classroom> {
    let document = Html::parse_document(html);

    let mut classrooms = Vec::new();

    for item in document.select(&SEL_POST_ITEM) {
        let classes: Vec<&str> = item
            .value()
            .attr("class")
            .unwrap_or("")
            .split_whitespace()
            .collect();

        // Extract name + URL
        let (name, url) = match item.select(&SEL_TITLE_LINK).next() {
            Some(a) => (
                a.text().collect::<String>().trim().to_string(),
                a.value().attr("href").unwrap_or("").to_string(),
            ),
            None => continue,
        };

        let mut capacity = None;
        let mut seating_style = None;
        let mut area = None;
        let mut technology = Vec::new();
        let mut physical_features = Vec::new();

        for class in &classes {
            if let Some(cap) = class.strip_prefix("seating-capacity-") {
                capacity = cap.parse().ok();
            } else if let Some(style) = class.strip_prefix("seating-style-") {
                seating_style = Some(style.to_string());
            } else if let Some(a) = class.strip_prefix("area-") {
                area = Some(a.to_string());
            } else if let Some(tech) = class.strip_prefix("technology-") {
                technology.push(tech.to_string());
            } else if let Some(feat) = class.strip_prefix("physical-feature-") {
                physical_features.push(feat.to_string());
            }
        }

        classrooms.push(Classroom {
            name,
            url,
            capacity,
            seating_style,
            area,
            technology,
            physical_features,
        });
    }

    classrooms
}

pub fn filter_classrooms<'a>(
    classrooms: &'a [Classroom],
    name: Option<&str>,
    min_capacity: Option<u32>,
    max_capacity: Option<u32>,
    building: Option<&str>,
    technology: Option<&str>,
    feature: Option<&str>,
) -> Vec<&'a Classroom> {
    classrooms
        .iter()
        .filter(|c| {
            if let Some(q) = name {
                if !c.name.to_lowercase().contains(&q.to_lowercase()) {
                    return false;
                }
            }
            if let Some(min) = min_capacity {
                if c.capacity.unwrap_or(0) < min {
                    return false;
                }
            }
            if let Some(max) = max_capacity {
                if c.capacity.unwrap_or(u32::MAX) > max {
                    return false;
                }
            }
            if let Some(b) = building {
                let b_lower = b.to_lowercase();
                let area_match = c
                    .area
                    .as_ref()
                    .is_some_and(|a| a.to_lowercase().contains(&b_lower));
                let name_match = c.name.to_lowercase().contains(&b_lower);
                if !area_match && !name_match {
                    return false;
                }
            }
            if let Some(t) = technology {
                let t_lower = t.to_lowercase();
                if !c.technology.iter().any(|tech| tech.to_lowercase().contains(&t_lower)) {
                    return false;
                }
            }
            if let Some(feat) = feature {
                let f_lower = feat.to_lowercase();
                if !c
                    .physical_features
                    .iter()
                    .any(|f| f.to_lowercase().contains(&f_lower))
                {
                    return false;
                }
            }
            true
        })
        .collect()
}
