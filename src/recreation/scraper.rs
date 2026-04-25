use std::fmt::Write;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;
use scraper::Html;

use crate::util::selectors;

selectors! {
    SEL_CANVAS => "canvas.occupancy-chart",
    SEL_STRONG => "strong",
    SEL_SCHEDULE_ROW => "tr",
    SEL_TD => "td",
    SEL_TITLE => "h2, h3, .panel-title, title",
    SEL_FC_EVENT => ".fc-event, .fc-event-title, .event-item",
    SEL_TR => "tr",
    SEL_PROGRAM_LINK => "a[href*=\"GetProgramDetails\"]",
}

static TIME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d{1,2}:\d{2}[ap])").expect("hardcoded regex"));

const OCCUPANCY_URL: &str = "https://campusrec.ucsc.edu/FacilityOccupancy";
const SCHEDULE_URL: &str = "https://campusrec.ucsc.edu/Facility/GetSchedule";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FacilityOccupancy {
    pub name: String,
    pub uuid: String,
    pub current_occupancy: u32,
    pub max_capacity: u32,
}

impl FacilityOccupancy {
    pub fn format(&self) -> String {
        let pct = if self.max_capacity > 0 {
            (self.current_occupancy as f64 / self.max_capacity as f64 * 100.0) as u32
        } else {
            0
        };
        let remaining = self.max_capacity.saturating_sub(self.current_occupancy);
        format!(
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

impl FacilitySchedule {
    pub fn format(&self) -> String {
        let mut out = format!("## Schedule: {}\n", self.facility_name);
        if self.entries.is_empty() {
            out.push_str("No scheduled events.");
        } else {
            for entry in &self.entries {
                let _ = write!(out, "\n- **{}** — {}", entry.time, entry.event);
            }
        }
        out
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

    let mut facilities = Vec::new();
    let mut seen_uuids = std::collections::HashSet::new();

    for canvas in document.select(&SEL_CANVAS) {
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

        // Walk up the DOM from the canvas to find the enclosing card, then look
        // for a facility-name <strong> within it.
        let name = find_facility_name(canvas)
            .unwrap_or_else(|| format!("Facility {}", uuid.chars().take(8).collect::<String>()));

        facilities.push(FacilityOccupancy {
            name,
            uuid: uuid.to_string(),
            current_occupancy: occupancy,
            max_capacity,
        });
    }

    facilities
}

/// Walk up from the canvas element looking for a `<strong>` descendant in each
/// ancestor that looks like a facility name. The page structure puts facility
/// names in a card container that also wraps the canvas — so the first ancestor
/// that contains a facility-shaped `<strong>` is the card.
fn find_facility_name(canvas: scraper::ElementRef) -> Option<String> {
    let mut node = canvas.parent()?;
    loop {
        if let Some(el) = scraper::ElementRef::wrap(node) {
            for strong in el.select(&SEL_STRONG) {
                let text = strong.text().collect::<String>().trim().to_string();
                if is_facility_name_text(&text) {
                    return Some(clean_facility_name(&text));
                }
            }
        }
        node = node.parent()?;
    }
}

fn is_facility_name_text(text: &str) -> bool {
    !text.is_empty()
        && text.parse::<f64>().is_err()
        && !text.starts_with("Max")
        && !text.contains("Occupancy")
}

/// Strip leading "N. " numeric prefix that the page uses to enumerate facilities.
fn clean_facility_name(text: &str) -> String {
    let cleaned = text
        .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.')
        .trim();
    if cleaned.is_empty() {
        text.to_string()
    } else {
        cleaned.to_string()
    }
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

    let mut entries = Vec::new();
    let mut facility_name = String::new();

    // Try to get facility name from page title or header
    if let Some(el) = document.select(&SEL_TITLE).next() {
        let text = el.text().collect::<String>().trim().to_string();
        if !text.is_empty() && !text.contains("Schedule") {
            facility_name = text;
        } else if text.contains(" - ") {
            facility_name = text.split(" - ").next().unwrap_or("").trim().to_string();
        }
    }

    // Parse table rows for schedule entries
    for row in document.select(&SEL_SCHEDULE_ROW) {
        let cells: Vec<String> = row
            .select(&SEL_TD)
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
        for el in document.select(&SEL_FC_EVENT) {
            let text = el.text().collect::<String>().trim().to_string();
            if !text.is_empty() {
                entries.push(ScheduleEntry {
                    time: String::new(),
                    event: text,
                });
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

// ───── Group Exercise Classes ─────

const GROUP_EXERCISE_URL: &str =
    "https://goslugs.com/sports/2026/2/26/groupexercise_schedules_spring26";

/// Location abbreviation expansion.
fn expand_location(abbr: &str) -> &str {
    match abbr.to_uppercase().as_str() {
        "MAS" => "Martial Arts Studio",
        "DNC" => "Dance Studio",
        "ACT" => "Activity Room",
        _ => abbr,
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GroupExerciseClass {
    pub name: String,
    pub day: String,
    pub time: String,
    pub instructor: String,
    pub location: String,
    pub location_full: String,
    pub registration_url: Option<String>,
}

impl GroupExerciseClass {
    pub fn format(&self) -> String {
        let mut out = format!(
            "- **{}** {} — {} w/ {} @ {} ({})",
            self.time, self.name, self.day, self.instructor, self.location, self.location_full,
        );
        if let Some(url) = &self.registration_url {
            let _ = write!(out, " [Register]({})", url);
        }
        out
    }
}

pub async fn scrape_group_exercise(client: &reqwest::Client) -> Result<Vec<GroupExerciseClass>> {
    let resp = client
        .get(GROUP_EXERCISE_URL)
        .send()
        .await
        .context("Failed to fetch group exercise schedule")?;

    let html = resp
        .text()
        .await
        .context("Failed to read group exercise body")?;

    Ok(parse_group_exercise(&html))
}

fn parse_group_exercise(html: &str) -> Vec<GroupExerciseClass> {
    let document = Html::parse_document(html);

    // Day names we expect as column headers (the page uses "MONDAYS", "TUESDAYS", etc.)
    let day_names = [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ];

    // Collect all <td> cells. The first row with day-name cells gives us the column→day mapping.
    // Subsequent rows align by column index.
    let all_rows: Vec<Vec<scraper::ElementRef>> = document
        .select(&SEL_TR)
        .map(|tr| tr.select(&SEL_TD).collect::<Vec<_>>())
        .filter(|cells| !cells.is_empty())
        .collect();

    // Find the header row: the row where cells contain day names.
    let mut day_columns: Vec<(usize, String)> = Vec::new(); // (col_index, day_name)
    let mut header_row_idx = None;

    for (row_idx, row) in all_rows.iter().enumerate() {
        let mut found_days = Vec::new();
        for (col_idx, cell) in row.iter().enumerate() {
            let text = cell.text().collect::<String>();
            let text_lower = text.trim().to_lowercase();
            for day in &day_names {
                if text_lower.contains(&day.to_lowercase()) {
                    found_days.push((col_idx, day.to_string()));
                    break;
                }
            }
        }
        if found_days.len() >= 3 {
            // Found the header row
            day_columns = found_days;
            header_row_idx = Some(row_idx);
            break;
        }
    }

    let header_row_idx = match header_row_idx {
        Some(i) => i,
        None => return Vec::new(),
    };

    let mut classes = Vec::new();

    // Parse data rows after the header. Each cell has at most one class entry.
    // The real HTML nests class names in <strong><span>NAME</span></strong> inside
    // the <a> tag, and uses "w/INSTRUCTOR" and "@ LOCATION" in surrounding text.
    for row in all_rows.iter().skip(header_row_idx + 1) {
        for &(col_idx, ref day) in &day_columns {
            if let Some(cell) = row.get(col_idx) {
                let link = match cell.select(&SEL_PROGRAM_LINK).next() {
                    Some(l) => l,
                    None => continue,
                };

                let reg_url = link.value().attr("href").map(String::from);
                let class_name: String = link
                    .text()
                    .collect::<String>()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");

                if class_name.is_empty() {
                    continue;
                }

                // Extract time, instructor, and location from the cell's full text.
                let full_text: String = cell
                    .text()
                    .collect::<String>()
                    .chars()
                    .map(|c| if c == '\n' { ' ' } else { c })
                    .collect();
                // Collapse runs of whitespace
                let full_text = full_text.split_whitespace().collect::<Vec<_>>().join(" ");

                let time = TIME_RE
                    .find(&full_text)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default();

                // Instructor: text between "w/" and "@" (or end of string)
                let instructor = full_text
                    .find("w/")
                    .map(|i| {
                        let after = &full_text[i + 2..];
                        let end = after.find('@').unwrap_or(after.len());
                        after[..end].trim().to_string()
                    })
                    .unwrap_or_default();

                // Location: word after "@"
                let location = full_text
                    .rfind('@')
                    .map(|i| full_text[i + 1..].trim().split_whitespace().next().unwrap_or(""))
                    .unwrap_or("")
                    .to_string();

                classes.push(GroupExerciseClass {
                    name: class_name,
                    day: day.clone(),
                    time,
                    instructor,
                    location_full: expand_location(&location).to_string(),
                    location,
                    registration_url: reg_url,
                });
            }
        }
    }

    classes
}

#[cfg(test)]
mod group_exercise_tests {
    use super::*;

    #[test]
    fn parse_schedule_table() {
        // Matches the real goslugs.com HTML structure: one class per cell,
        // nested <strong><span> with <br /> inside class names.
        let html = r#"<html><body><table>
<tr>
<td style="text-align: center;"><strong><strong>MONDAYS</strong></strong></td>
<td style="text-align: center;"><strong><strong>TUESDAYS</strong></strong></td>
<td style="text-align: center;"><strong><strong>WEDNESDAYS</strong></strong></td>
</tr>
<tr>
<td style="vertical-align: text-top;"><strong>7:30a<br />
<a href="https://campusrec.ucsc.edu/Program/GetProgramDetails?courseId=abc-123"><strong><span style="font-size: 1.25vw; color: green;">SUNRISE<br />
YOGA</span></strong></a><br />
<span style="font-size: 1vw;"><strong>w/<a href="/bio#padma">Padma</a></strong><br />
@ MAS</span></strong></td>
<td style="vertical-align: text-top;"><strong>10:45a<br />
<a href="https://campusrec.ucsc.edu/Program/GetProgramDetails?courseId=def-456"><strong><span style="font-size: 1.25vw; color: red;">MAT<br />
PILATES</span></strong></a><br />
<span style="font-size: 1vw;"><strong>w/<a href="/bio#sam">Sam</a></strong><br />
@ DNC</span></strong></td>
<td style="vertical-align: text-top;"></td>
</tr>
<tr>
<td style="vertical-align: text-top;"><strong>10:45a<br />
<a href="https://campusrec.ucsc.edu/Program/GetProgramDetails?courseId=ghi-789"><strong><span style="font-size: 1.25vw; color: blue;">INDOOR<br />
CYCLING</span></strong></a><br />
<span style="font-size: 1vw;"><strong>w/<a href="/bio#alex">Alex</a></strong><br />
@ ACT</span></strong></td>
<td style="vertical-align: text-top;"></td>
<td style="vertical-align: text-top;"></td>
</tr>
</table></body></html>"#;

        let classes = parse_group_exercise(html);
        assert_eq!(classes.len(), 3, "got: {:?}", classes);

        assert_eq!(classes[0].name, "SUNRISE YOGA");
        assert_eq!(classes[0].day, "Monday");
        assert_eq!(classes[0].time, "7:30a");
        assert_eq!(classes[0].instructor, "Padma");
        assert_eq!(classes[0].location, "MAS");
        assert_eq!(classes[0].location_full, "Martial Arts Studio");
        assert!(classes[0].registration_url.as_ref().unwrap().contains("abc-123"));

        assert_eq!(classes[1].name, "MAT PILATES");
        assert_eq!(classes[1].day, "Tuesday");
        assert_eq!(classes[1].time, "10:45a");
        assert_eq!(classes[1].instructor, "Sam");
        assert_eq!(classes[1].location, "DNC");
        assert_eq!(classes[1].location_full, "Dance Studio");

        assert_eq!(classes[2].name, "INDOOR CYCLING");
        assert_eq!(classes[2].day, "Monday");
        assert_eq!(classes[2].time, "10:45a");
        assert_eq!(classes[2].instructor, "Alex");
        assert_eq!(classes[2].location, "ACT");
        assert_eq!(classes[2].location_full, "Activity Room");
    }

    #[test]
    fn parse_empty_html() {
        let classes = parse_group_exercise("<html><body></body></html>");
        assert!(classes.is_empty());
    }

}
