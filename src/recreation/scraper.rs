use std::fmt;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use scraper::{Html, Selector};

fn sel<'a>(cell: &'a OnceLock<Selector>, s: &str) -> &'a Selector {
    cell.get_or_init(|| Selector::parse(s).expect("hardcoded selector"))
}

static SEL_CANVAS: OnceLock<Selector> = OnceLock::new();
static SEL_STRONG: OnceLock<Selector> = OnceLock::new();
static SEL_SCHEDULE_ROW: OnceLock<Selector> = OnceLock::new();
static SEL_TD: OnceLock<Selector> = OnceLock::new();

const OCCUPANCY_URL: &str = "https://campusrec.ucsc.edu/FacilityOccupancy";
const SCHEDULE_URL: &str = "https://campusrec.ucsc.edu/Facility/GetSchedule";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FacilityOccupancy {
    pub name: String,
    pub uuid: String,
    pub current_occupancy: u32,
    pub max_capacity: u32,
}

impl fmt::Display for FacilityOccupancy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pct = if self.max_capacity > 0 {
            (self.current_occupancy as f64 / self.max_capacity as f64 * 100.0) as u32
        } else {
            0
        };
        let remaining = self.max_capacity.saturating_sub(self.current_occupancy);
        write!(
            f,
            "### {}\n- **Occupancy**: {} / {} ({} remaining)\n- **Utilization**: {}%\n- **ID**: `{}`",
            self.name, self.current_occupancy, self.max_capacity, remaining, pct, self.uuid
        )
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FacilitySchedule {
    pub facility_name: String,
    pub facility_id: String,
    pub entries: Vec<ScheduleEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScheduleEntry {
    pub time: String,
    pub event: String,
}

impl fmt::Display for FacilitySchedule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "## Schedule: {}", self.facility_name)?;
        if self.entries.is_empty() {
            write!(f, "No scheduled events.")?;
        } else {
            for entry in &self.entries {
                write!(f, "\n- **{}** — {}", entry.time, entry.event)?;
            }
        }
        Ok(())
    }
}

pub async fn scrape_occupancy(client: &reqwest::Client) -> Result<Vec<FacilityOccupancy>> {
    let resp = client
        .get(OCCUPANCY_URL)
        .send()
        .await
        .context("Failed to fetch facility occupancy page")?;

    let html = resp
        .text()
        .await
        .context("Failed to read occupancy body")?;

    Ok(parse_occupancy(&html))
}

fn parse_occupancy(html: &str) -> Vec<FacilityOccupancy> {
    let document = Html::parse_document(html);
    let canvas_sel = sel(&SEL_CANVAS, "canvas.occupancy-chart");
    let strong_sel = sel(&SEL_STRONG, "strong");

    let mut facilities = Vec::new();
    let mut seen_uuids = std::collections::HashSet::new();

    for canvas in document.select(canvas_sel) {
        let id = canvas.value().attr("id").unwrap_or("");
        // Skip the "-sm" (small/mobile) duplicates
        if id.ends_with("-sm") {
            continue;
        }

        let uuid = id.strip_prefix("occupancyChart-").unwrap_or(id);
        if uuid.is_empty() || !seen_uuids.insert(uuid.to_string()) {
            continue;
        }

        let occupancy: u32 = canvas
            .value()
            .attr("data-occupancy")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let remaining: u32 = canvas
            .value()
            .attr("data-remaining")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let max_capacity = occupancy + remaining;

        // Walk up to find the facility name — look for the nearest card container's <strong>
        // The page structure has facility name in a nearby <h2><strong> or just <strong>
        // We find the parent card by searching siblings/ancestors
        let name = find_facility_name(&document, uuid, strong_sel);

        facilities.push(FacilityOccupancy {
            name,
            uuid: uuid.to_string(),
            current_occupancy: occupancy,
            max_capacity,
        });
    }

    facilities
}

/// Attempt to find the facility name by searching for text near the canvas element
fn find_facility_name(document: &Html, uuid: &str, strong_sel: &Selector) -> String {
    // Strategy: find all <strong> elements in the page and match by proximity to the canvas ID.
    // The page structure has numbered facilities like "1. East Upper Field" in <strong> tags
    // within each card. We collect all strong texts and match by position.
    let all_strongs: Vec<String> = document
        .select(strong_sel)
        .filter_map(|el| {
            let text = el.text().collect::<String>().trim().to_string();
            // Facility names are numbered "N. Name" or just names; skip numeric-only values
            if text.is_empty()
                || text.parse::<f64>().is_ok()
                || text.starts_with("Max")
                || text.contains("Occupancy")
            {
                return None;
            }
            Some(text)
        })
        .collect();

    // The page renders facilities in order, and we process canvases in document order.
    // Use a simple heuristic: search the raw HTML for the canvas ID and grab the
    // closest preceding facility-name strong text.
    // Alternatively, just use the nth unique strong text that looks like a facility name.
    // For robustness, search the HTML for the UUID and look for the name nearby.
    let search = format!("occupancyChart-{}", uuid);
    let html_str = document.root_element().html();
    if let Some(canvas_pos) = html_str.find(&search) {
        // Look backwards for the last strong-like facility name
        let before = &html_str[..canvas_pos];
        // Find the last occurrence of a strong tag with facility-like content
        for name_candidate in all_strongs.iter().rev() {
            if before.rfind(name_candidate.as_str()).is_some() {
                // Strip leading number prefix like "1. " or "10. "
                let cleaned = name_candidate
                    .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.')
                    .trim();
                return if cleaned.is_empty() {
                    name_candidate.clone()
                } else {
                    cleaned.to_string()
                };
            }
        }
    }

    format!("Facility {}", uuid.chars().take(8).collect::<String>())
}

pub fn find_facility<'a>(query: &str, facilities: &'a [FacilityOccupancy]) -> Vec<&'a FacilityOccupancy> {
    let q = query.to_lowercase();
    facilities
        .iter()
        .filter(|f| f.name.to_lowercase().contains(&q) || f.uuid.starts_with(&q))
        .collect()
}

pub async fn scrape_schedule(
    client: &reqwest::Client,
    facility_id: &str,
) -> Result<FacilitySchedule> {
    let url = format!("{}?facilityId={}", SCHEDULE_URL, facility_id);
    let resp = client
        .get(&url)
        .send()
        .await
        .context("Failed to fetch facility schedule")?;

    let html = resp
        .text()
        .await
        .context("Failed to read schedule body")?;

    Ok(parse_schedule(&html, facility_id))
}

fn parse_schedule(html: &str, facility_id: &str) -> FacilitySchedule {
    let document = Html::parse_document(html);

    // The schedule page uses FullCalendar. Try to extract events from the page.
    // Look for table rows or list items that contain schedule data.
    let row_sel = sel(&SEL_SCHEDULE_ROW, "tr");
    let td_sel = sel(&SEL_TD, "td");

    let mut entries = Vec::new();
    let mut facility_name = String::new();

    // Try to get facility name from page title or header
    if let Ok(title_sel) = Selector::parse("h2, h3, .panel-title, title") {
        if let Some(el) = document.select(&title_sel).next() {
            let text = el.text().collect::<String>().trim().to_string();
            if !text.is_empty() && !text.contains("Schedule") {
                facility_name = text;
            } else if text.contains(" - ") {
                facility_name = text.split(" - ").next().unwrap_or("").trim().to_string();
            }
        }
    }

    // Parse table rows for schedule entries
    for row in document.select(row_sel) {
        let cells: Vec<String> = row
            .select(td_sel)
            .map(|td| td.text().collect::<String>().trim().to_string())
            .collect();

        if cells.len() >= 2 {
            let time = cells[0].clone();
            let event = cells[1..].join(" — ");
            if !time.is_empty() && !event.is_empty() {
                entries.push(ScheduleEntry { time, event });
            }
        }
    }

    // If no table rows found, try extracting from FullCalendar event elements
    if entries.is_empty() {
        if let Ok(event_sel) = Selector::parse(".fc-event, .fc-event-title, .event-item") {
            for el in document.select(&event_sel) {
                let text = el.text().collect::<String>().trim().to_string();
                if !text.is_empty() {
                    entries.push(ScheduleEntry {
                        time: String::new(),
                        event: text,
                    });
                }
            }
        }
    }

    if facility_name.is_empty() {
        facility_name = format!("Facility {}", &facility_id[..8.min(facility_id.len())]);
    }

    FacilitySchedule {
        facility_name,
        facility_id: facility_id.to_string(),
        entries,
    }
}
