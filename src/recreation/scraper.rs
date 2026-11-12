use std::fmt::Write;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use chrono::{Datelike, Days, NaiveDate, NaiveDateTime, NaiveTime, Weekday};
use regex::Regex;
use scraper::Html;

use crate::util::selectors;

selectors! {
    SEL_CANVAS => "canvas.occupancy-chart",
    SEL_STRONG => "strong",
    SEL_TD => "td",
    SEL_TR => "tr",
    SEL_PROGRAM_LINK => "a[href*=\"GetProgramDetails\"]",
    SEL_LINK => "a",
}

static TERM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(fall|winter|spring|summer)\s+(20\d\d)$").expect("hardcoded regex")
});

static TIME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d{1,2}:\d{2}[ap])").expect("hardcoded regex"));

const OCCUPANCY_URL: &str = "https://campusrec.ucsc.edu/FacilityOccupancy";
// The old /Facility/GetSchedule page became a DevExtreme JS shell in 2026;
// this is the XHR feed that shell loads its appointments from. Recurring
// events arrive unexpanded (anchor dates + iCal RRULE).
const SCHEDULE_URL: &str =
    "https://campusrec.ucsc.edu/Facility/GetScheduleCustomAppointmentsForDevExtremeScheduler";

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
    /// Structured bounds; `#[serde(default)]` so entries cached before these
    /// fields existed still deserialize.
    #[serde(default)]
    pub start: Option<NaiveDateTime>,
    #[serde(default)]
    pub end: Option<NaiveDateTime>,
}

impl FacilitySchedule {
    pub fn format(&self) -> String {
        let mut out = format!("## Schedule: {}\n", self.facility_name);
        if self.entries.is_empty() {
            out.push_str("No scheduled events.");
            return out;
        }
        // The feed publishes closures, not opening hours; the gaps between
        // "Facility Closed" blocks are when the facility is open.
        let windows = open_windows(&self.entries);
        if !windows.is_empty() {
            out.push_str("\n**Open hours** (between posted closures):\n");
            for (day, spans) in &windows {
                let spans: Vec<String> = spans
                    .iter()
                    .map(|(s, e)| {
                        format!(
                            "{} \u{2013} {}",
                            s.format("%-I:%M %p"),
                            e.format("%-I:%M %p")
                        )
                    })
                    .collect();
                let _ = writeln!(
                    out,
                    "- **{}** · Open {}",
                    day.format("%a %b %-d"),
                    spans.join(", ")
                );
            }
            out.push_str("\n**Posted blocks:**");
        }
        for entry in &self.entries {
            let _ = write!(out, "\n- **{}** — {}", entry.time, entry.event);
        }
        out
    }
}

/// Per-day open windows implied by "Facility Closed" blocks. Multi-day blocks
/// are split at midnight; days with no closure blocks are omitted (unknown).
fn open_windows(entries: &[ScheduleEntry]) -> Vec<(NaiveDate, Vec<(NaiveTime, NaiveTime)>)> {
    use std::collections::BTreeMap;
    let midnight = NaiveTime::from_hms_opt(0, 0, 0).expect("valid time");
    let end_of_day = NaiveTime::from_hms_opt(23, 59, 59).expect("valid time");
    let mut by_day: BTreeMap<NaiveDate, Vec<(NaiveTime, NaiveTime)>> = BTreeMap::new();
    for e in entries {
        if !e.event.starts_with("Facility Closed") {
            continue;
        }
        let (Some(start), Some(end)) = (e.start, e.end) else {
            continue;
        };
        let mut cur = start;
        while cur < end {
            let stop = if end.date() > cur.date() {
                end_of_day
            } else {
                end.time()
            };
            by_day
                .entry(cur.date())
                .or_default()
                .push((cur.time(), stop));
            cur = (cur.date() + Days::new(1)).and_time(midnight);
        }
    }
    let mut out = Vec::new();
    for (day, mut spans) in by_day {
        spans.sort();
        let mut gaps = Vec::new();
        let mut cursor = midnight;
        for (s, e) in spans {
            if s > cursor {
                gaps.push((cursor, s));
            }
            if e > cursor {
                cursor = e;
            }
        }
        if cursor < end_of_day {
            gaps.push((cursor, end_of_day));
        }
        if !gaps.is_empty() {
            out.push((day, gaps));
        }
    }
    out
}

pub async fn scrape_occupancy(client: &reqwest::Client) -> Result<Vec<FacilityOccupancy>> {
    let resp = client
        .get(OCCUPANCY_URL)
        .send()
        .await
        .context("Failed to fetch facility occupancy page")?;

    let html = resp.text().await.context("Failed to read occupancy body")?;

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

pub fn find_facility<'a>(
    query: &str,
    facilities: &'a [FacilityOccupancy],
) -> Vec<&'a FacilityOccupancy> {
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
    let today = crate::util::now_pacific().date_naive();
    let url = format!(
        "{}?selectedFacilityId={}&start={}T00:00:00&end={}T00:00:00",
        SCHEDULE_URL,
        urlencoding::encode(facility_id),
        today,
        today + Days::new(7),
    );
    let resp = client
        .get(&url)
        .send()
        .await
        .context("Failed to fetch facility schedule")?
        .error_for_status()
        .context("Facility schedule feed returned an error status")?;

    let body = resp.text().await.context("Failed to read schedule body")?;
    let appointments = parse_schedule_json(&body)?;

    Ok(FacilitySchedule {
        // The feed carries no facility name; the service layer resolves it
        // from the occupancy list when possible.
        facility_name: format!(
            "Facility {}",
            facility_id.chars().take(8).collect::<String>()
        ),
        facility_id: facility_id.to_string(),
        entries: expand_appointments(&appointments, today, 7),
    })
}

/// One raw appointment from the DevExtreme scheduler feed. Recurring events
/// arrive unexpanded: an anchor StartDate/EndDate (sometimes years in the
/// past) plus an iCal RRULE the client is expected to expand.
#[derive(Debug, Clone, serde::Deserialize)]
struct Appointment {
    #[serde(rename = "Text", default)]
    text: String,
    #[serde(rename = "StartDate", default)]
    start_date: String,
    #[serde(rename = "EndDate", default)]
    end_date: String,
    #[serde(rename = "AllDay", default)]
    all_day: bool,
    #[serde(rename = "Description", default)]
    description: String,
    #[serde(rename = "RecurrenceRule", default)]
    recurrence_rule: Option<String>,
    #[serde(rename = "RecurrenceException", default)]
    recurrence_exception: Option<String>,
}

fn parse_schedule_json(body: &str) -> Result<Vec<Appointment>> {
    serde_json::from_str(body).context("Failed to parse schedule feed")
}

fn appt_datetime(s: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f").ok()
}

fn rule_field<'a>(rule: &'a str, key: &str) -> Option<&'a str> {
    rule.split(';')
        .find_map(|part| part.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
}

fn byday_weekdays(byday: &str) -> Vec<Weekday> {
    byday
        .split(',')
        .filter_map(|code| match code {
            "SU" => Some(Weekday::Sun),
            "MO" => Some(Weekday::Mon),
            "TU" => Some(Weekday::Tue),
            "WE" => Some(Weekday::Wed),
            "TH" => Some(Weekday::Thu),
            "FR" => Some(Weekday::Fri),
            "SA" => Some(Weekday::Sat),
            _ => None,
        })
        .collect()
}

/// UNTIL is UTC ("...Z"); convert to Pacific so a summer-hours cutoff doesn't
/// bleed an extra day past its end.
fn until_pacific(until: &str) -> Option<NaiveDateTime> {
    use chrono::TimeZone;
    let naive = NaiveDateTime::parse_from_str(until, "%Y%m%dT%H%M%SZ").ok()?;
    Some(
        chrono::Utc
            .from_utc_datetime(&naive)
            .with_timezone(&chrono_tz::US::Pacific)
            .naive_local(),
    )
}

fn exception_dates(exceptions: Option<&str>) -> Vec<NaiveDate> {
    exceptions
        .unwrap_or_default()
        .split(',')
        .filter_map(|e| NaiveDateTime::parse_from_str(e.trim(), "%Y%m%dT%H%M%S").ok())
        .map(|dt| dt.date())
        .collect()
}

/// Expand raw appointments into per-day entries for the window starting at
/// `from`, `days` long. Covers the recurrence shapes the feed actually uses
/// (weekly BYDAY with optional UNTIL/exceptions; yearly nth-weekday-of-month
/// holidays); anything else degrades to a single annotated entry instead of
/// silently vanishing.
fn expand_appointments(appts: &[Appointment], from: NaiveDate, days: u32) -> Vec<ScheduleEntry> {
    let window_end = from + Days::new(u64::from(days));
    let mut dated: Vec<(NaiveDateTime, ScheduleEntry)> = Vec::new();
    let mut undated: Vec<ScheduleEntry> = Vec::new();

    for appt in appts {
        let (Some(start), Some(end)) = (
            appt_datetime(&appt.start_date),
            appt_datetime(&appt.end_date),
        ) else {
            continue;
        };

        let rule = appt.recurrence_rule.as_deref().unwrap_or("");
        if rule.is_empty() {
            if start.date() < window_end && end.date() >= from {
                dated.push((start, entry_for(appt, start, end)));
            }
            continue;
        }

        match rule_field(rule, "FREQ") {
            Some("WEEKLY") => {
                let by = byday_weekdays(rule_field(rule, "BYDAY").unwrap_or(""));
                let until = rule_field(rule, "UNTIL").and_then(until_pacific);
                let skip = exception_dates(appt.recurrence_exception.as_deref());
                for i in 0..days {
                    let day = from + Days::new(u64::from(i));
                    let occ_start = day.and_time(start.time());
                    if by.contains(&day.weekday())
                        && day >= start.date()
                        && until.is_none_or(|u| occ_start <= u)
                        && !skip.contains(&day)
                    {
                        let occ_end = day.and_time(end.time());
                        dated.push((occ_start, entry_for(appt, occ_start, occ_end)));
                    }
                }
            }
            Some("YEARLY") => {
                // Observed shape: BYMONTH + BYDAY + BYSETPOS (nth weekday of
                // a month, e.g. MLK = third Monday of January).
                let month: Option<u32> = rule_field(rule, "BYMONTH").and_then(|v| v.parse().ok());
                let by = byday_weekdays(rule_field(rule, "BYDAY").unwrap_or(""));
                // BYSETPOS counts from the end of the month when negative
                // (-1 = last such weekday, e.g. Memorial Day).
                let setpos: Option<i32> = rule_field(rule, "BYSETPOS").and_then(|v| v.parse().ok());
                for i in 0..days {
                    let day = from + Days::new(u64::from(i));
                    let nth = ((day.day() - 1) / 7 + 1) as i32;
                    let from_end = -(((days_in_month(day) - day.day()) / 7 + 1) as i32);
                    if month == Some(day.month())
                        && by.contains(&day.weekday())
                        && setpos.is_none_or(|p| p == nth || p == from_end)
                        && day >= start.date()
                    {
                        let occ_start = day.and_time(start.time());
                        let occ_end = day.and_time(end.time());
                        dated.push((occ_start, entry_for(appt, occ_start, occ_end)));
                    }
                }
            }
            _ => undated.push(ScheduleEntry {
                time: "recurring".to_string(),
                event: format!("{} (recurring \u{2014} pattern not expanded)", appt.text),
                start: None,
                end: None,
            }),
        }
    }

    dated.sort_by_key(|(dt, _)| *dt);
    let mut entries: Vec<ScheduleEntry> = dated.into_iter().map(|(_, e)| e).collect();
    entries.extend(undated);
    entries
}

fn days_in_month(day: NaiveDate) -> u32 {
    let (y, m) = (day.year(), day.month());
    let first = NaiveDate::from_ymd_opt(y, m, 1).expect("valid first of month");
    let next = if m == 12 {
        NaiveDate::from_ymd_opt(y + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(y, m + 1, 1)
    }
    .expect("valid first of next month");
    (next - first).num_days() as u32
}

fn entry_for(appt: &Appointment, start: NaiveDateTime, end: NaiveDateTime) -> ScheduleEntry {
    let time = if appt.all_day {
        format!("{} \u{b7} all day", start.format("%a %b %-d"))
    } else if start.date() == end.date() {
        format!(
            "{} \u{b7} {} \u{2013} {}",
            start.format("%a %b %-d"),
            start.format("%-I:%M %p"),
            end.format("%-I:%M %p")
        )
    } else {
        format!(
            "{} {} \u{2013} {} {}",
            start.format("%a %b %-d"),
            start.format("%-I:%M %p"),
            end.format("%a %b %-d"),
            end.format("%-I:%M %p")
        )
    };
    let event = if appt.description.is_empty() || appt.description == appt.text {
        appt.text.clone()
    } else {
        format!("{} \u{2014} {}", appt.text, appt.description)
    };
    ScheduleEntry {
        time,
        event,
        start: Some(start),
        end: Some(end),
    }
}

// ───── Group Exercise Classes ─────

// The hub always shows the current term; goslugs.com moves past terms to
// suffixed pages (`_summer26`) linked from the hub's term row.
const GROUP_EXERCISE_URL: &str = "https://goslugs.com/sports/2025/8/6/groupexercise_schedules";

/// "fall 2026" / "FALL 2026" -> "Fall 2026"; `None` unless the whole string is a term.
fn normalize_term(text: &str) -> Option<String> {
    let caps = TERM_RE.captures(text.trim())?;
    let season = caps[1].to_ascii_lowercase();
    let mut chars = season.chars();
    let first = chars.next()?.to_ascii_uppercase();
    Some(format!("{first}{} {}", chars.as_str(), &caps[2]))
}

/// The term the hub page shows. Its "Fall 2026 | <a>Winter 2026</a> | …" row
/// links every other term, so the current one is the row's only unlinked
/// label. Falls back to a bare term heading such as `<strong>FALL 2026</strong>`.
fn term_from_hub(document: &Html) -> Option<String> {
    let term_rows = document
        .select(&SEL_LINK)
        .filter(|a| normalize_term(&a.text().collect::<String>()).is_some())
        .filter_map(|a| a.parent());
    for row in term_rows {
        for child in row.children() {
            if let Some(text) = child.value().as_text()
                && let Some(term) = text.split('|').find_map(normalize_term)
            {
                return Some(term);
            }
        }
    }
    document
        .select(&SEL_STRONG)
        .find_map(|el| normalize_term(&el.text().collect::<String>()))
}

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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GroupExerciseSchedule {
    /// `None` when the hub page carries no recognizable term label.
    pub term: Option<String>,
    pub classes: Vec<GroupExerciseClass>,
}

pub async fn scrape_group_exercise(client: &reqwest::Client) -> Result<GroupExerciseSchedule> {
    let resp = client
        .get(GROUP_EXERCISE_URL)
        .send()
        .await
        .context("Failed to fetch group exercise schedule")?
        .error_for_status()
        .context("goslugs.com group exercise page")?;

    let html = resp
        .text()
        .await
        .context("Failed to read group exercise body")?;

    Ok(parse_group_exercise_page(&html))
}

fn parse_group_exercise_page(html: &str) -> GroupExerciseSchedule {
    let document = Html::parse_document(html);
    GroupExerciseSchedule {
        term: term_from_hub(&document),
        classes: classes_from_document(&document),
    }
}

#[cfg(test)]
fn parse_group_exercise(html: &str) -> Vec<GroupExerciseClass> {
    classes_from_document(&Html::parse_document(html))
}

fn classes_from_document(document: &Html) -> Vec<GroupExerciseClass> {
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

    // The real HTML nests class names in <strong><span>NAME</span></strong> inside
    // the <a> tag, and uses "w/INSTRUCTOR" and "@ LOCATION" in surrounding text.
    for row in all_rows.iter().skip(header_row_idx + 1) {
        for &(col_idx, ref day) in &day_columns {
            if let Some(cell) = row.get(col_idx) {
                for entry in cell_entries(*cell) {
                    classes.push(GroupExerciseClass {
                        day: day.clone(),
                        location_full: expand_location(&entry.location).to_string(),
                        name: entry.name,
                        time: entry.time,
                        instructor: entry.instructor,
                        location: entry.location,
                        registration_url: entry.registration_url,
                    });
                }
            }
        }
    }

    classes
}

struct CellEntry {
    name: String,
    time: String,
    instructor: String,
    location: String,
    registration_url: Option<String>,
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A schedule cell holds one class or, since Fall 2026, several stacked ones,
/// each starting at its time ("12:00p MAT PILATES w/Padma @ DNC 12:00p …").
/// Splitting the cell text at time tokens keeps each class's name, instructor
/// and room together; a class name can span several program links.
fn cell_entries(cell: scraper::ElementRef) -> Vec<CellEntry> {
    let mut text = String::new();
    // (byte range in `text`, href) for text inside a program link.
    let mut link_spans: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    for node in cell.descendants() {
        let Some(t) = node.value().as_text() else {
            continue;
        };
        let start = text.len();
        text.push_str(t);
        let link = node
            .ancestors()
            .filter_map(scraper::ElementRef::wrap)
            .take_while(|el| el.id() != cell.id())
            .find(|el| SEL_PROGRAM_LINK.matches(el));
        if let Some(a) = link
            && !t.trim().is_empty()
        {
            let href = a.value().attr("href").unwrap_or_default().to_string();
            link_spans.push((start..text.len(), href));
        }
    }
    if link_spans.is_empty() {
        return Vec::new();
    }

    let mut bounds: Vec<usize> = TIME_RE.find_iter(&text).map(|m| m.start()).collect();
    if bounds.first() != Some(&0) {
        // Text before the first time belongs to the first class.
        if bounds.is_empty() {
            bounds.push(0);
        } else {
            bounds[0] = 0;
        }
    }
    bounds.push(text.len());

    let mut entries = Vec::new();
    for w in bounds.windows(2) {
        let seg = w[0]..w[1];
        let in_seg: Vec<&(std::ops::Range<usize>, String)> = link_spans
            .iter()
            .filter(|(r, _)| r.start >= seg.start && r.start < seg.end)
            .collect();
        let Some((_, href)) = in_seg.first() else {
            continue;
        };
        let name = collapse_ws(
            &in_seg
                .iter()
                .map(|(r, _)| &text[r.clone()])
                .collect::<String>(),
        );
        if name.is_empty() {
            continue;
        }
        let seg_text = collapse_ws(&text[seg]);
        let time = TIME_RE
            .find(&seg_text)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        // Instructor: text between "w/" and "@" (or end of segment).
        let instructor = seg_text
            .find("w/")
            .map(|i| {
                let after = &seg_text[i + 2..];
                let end = after.find('@').unwrap_or(after.len());
                after[..end].trim().to_string()
            })
            .unwrap_or_default();
        // Location: word after "@".
        let location = seg_text
            .rfind('@')
            .map(|i| seg_text[i + 1..].split_whitespace().next().unwrap_or(""))
            .unwrap_or("")
            .to_string();
        entries.push(CellEntry {
            name,
            time,
            instructor,
            location,
            registration_url: Some(href.clone()).filter(|h| !h.is_empty()),
        });
    }
    entries
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
        assert!(
            classes[0]
                .registration_url
                .as_ref()
                .unwrap()
                .contains("abc-123")
        );

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

    #[test]
    fn parse_group_exercise_unicode_class_names() {
        const FIXTURE: &str = include_str!("fixtures/group_exercise_unicode.html");
        let classes = parse_group_exercise(FIXTURE);
        assert_eq!(classes.len(), 2, "got: {:#?}", classes);

        let cafe = &classes[0];
        assert_eq!(cafe.name, "CAFÉ MOVEMENT FLOW");
        assert_eq!(cafe.day, "Monday");
        assert_eq!(cafe.time, "9:15a");
        assert_eq!(cafe.instructor, "Renée");
        assert_eq!(cafe.location, "DNC");
        assert_eq!(cafe.location_full, "Dance Studio");
        assert!(cafe.registration_url.as_ref().unwrap().contains("cafe-001"));

        let piyo = &classes[1];
        assert_eq!(piyo.name, "PIYO® STRENGTH");
        assert_eq!(piyo.day, "Tuesday");
        assert_eq!(piyo.time, "6:30p");
        assert_eq!(piyo.instructor, "Søren");
        assert_eq!(piyo.location, "MAS");
        assert_eq!(piyo.location_full, "Martial Arts Studio");
    }

    // ── error paths ──

    fn cell(inner: &str) -> String {
        format!(
            r#"<html><body><table>
            <tr><td>MONDAYS</td><td>TUESDAYS</td><td>WEDNESDAYS</td></tr>
            <tr><td>{inner}</td><td></td><td></td></tr>
            </table></body></html>"#
        )
    }

    #[test]
    fn parse_group_exercise_multibyte_after_markers_no_panic() {
        // Multibyte chars immediately after the "w/" and "@" byte anchors.
        let html = cell(
            r#"7:30a <a href="https://campusrec.ucsc.edu/Program/GetProgramDetails?courseId=x">ΖΟΥΜΠΑ</a> w/Ольга @ Στούντιο"#,
        );
        let classes = parse_group_exercise(&html);
        assert_eq!(classes.len(), 1, "got: {:#?}", classes);
        assert_eq!(classes[0].name, "ΖΟΥΜΠΑ");
        assert_eq!(classes[0].instructor, "Ольга");
        assert_eq!(classes[0].location, "Στούντιο");
        // Unknown abbreviation passes through unexpanded.
        assert_eq!(classes[0].location_full, "Στούντιο");
    }

    #[test]
    fn parse_group_exercise_cell_missing_markers_degrades() {
        // A class link with no time / "w/" / "@" text yields empty fields, not a panic.
        let html = cell(
            r#"<a href="https://campusrec.ucsc.edu/Program/GetProgramDetails?courseId=y">MYSTERY MOVES</a>"#,
        );
        let classes = parse_group_exercise(&html);
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].time, "");
        assert_eq!(classes[0].instructor, "");
        assert_eq!(classes[0].location, "");
    }

    #[test]
    fn parse_group_exercise_two_day_header_not_recognized() {
        // Fewer than 3 day columns → header row not identified → no classes.
        let html = r#"<html><body><table>
            <tr><td>MONDAYS</td><td>TUESDAYS</td></tr>
            <tr><td><a href="https://campusrec.ucsc.edu/Program/GetProgramDetails?courseId=z">YOGA</a></td><td></td></tr>
            </table></body></html>"#;
        assert!(parse_group_exercise(html).is_empty());
    }

    #[test]
    fn parse_group_exercise_maintenance_page_yields_empty() {
        assert!(parse_group_exercise("<html><body>Maintenance</body></html>").is_empty());
    }
}

#[cfg(test)]
mod occupancy_tests {
    use super::*;

    const OCCUPANCY_FIXTURE: &str = include_str!("fixtures/occupancy.html");

    #[test]
    fn parse_occupancy_dedupes_mobile_canvases() {
        let facilities = parse_occupancy(OCCUPANCY_FIXTURE);
        // Three facilities; each "-sm" mobile duplicate must be skipped.
        assert_eq!(facilities.len(), 3, "got: {:#?}", facilities);
    }

    #[test]
    fn parse_occupancy_at_capacity() {
        let facilities = parse_occupancy(OCCUPANCY_FIXTURE);
        let fitness = facilities
            .iter()
            .find(|f| f.name == "Fitness Center")
            .expect("fitness center present");
        // 250 occupied, 0 remaining → full.
        assert_eq!(fitness.current_occupancy, 250);
        assert_eq!(fitness.max_capacity, 250);
        // numeric "N. " enumeration prefix is stripped from the name.
        assert!(!fitness.name.starts_with('1'));
        // format() reports 100% utilization and 0 remaining.
        let rendered = fitness.format();
        assert!(rendered.contains("250 / 250"));
        assert!(rendered.contains("(0 remaining)"));
        assert!(rendered.contains("100%"));
    }

    #[test]
    fn parse_occupancy_partial() {
        let facilities = parse_occupancy(OCCUPANCY_FIXTURE);
        let pool = facilities
            .iter()
            .find(|f| f.name == "Swimming Pool")
            .expect("pool present");
        assert_eq!(pool.current_occupancy, 18);
        assert_eq!(pool.max_capacity, 60); // 18 + 42
    }

    #[test]
    fn parse_occupancy_unparseable_remaining_treated_as_full() {
        let facilities = parse_occupancy(OCCUPANCY_FIXTURE);
        let wall = facilities
            .iter()
            .find(|f| f.name == "Climbing Wall")
            .expect("climbing wall present");
        // data-remaining="closed" fails to parse → 0 → max == occupancy (100%).
        assert_eq!(wall.current_occupancy, 44);
        assert_eq!(wall.max_capacity, 44);
        assert!(wall.format().contains("100%"));
    }

    // ── error paths ──

    #[test]
    fn parse_occupancy_maintenance_page_yields_empty() {
        assert!(parse_occupancy("<html><body>Maintenance</body></html>").is_empty());
        assert!(parse_occupancy("").is_empty());
    }

    #[test]
    fn parse_occupancy_canvas_without_id_skipped() {
        let html =
            r#"<canvas class="occupancy-chart" data-occupancy="5" data-remaining="5"></canvas>"#;
        assert!(parse_occupancy(html).is_empty());
    }

    #[test]
    fn parse_occupancy_all_attrs_garbage_yields_zeroes() {
        let html = r#"<div><strong>Mystery Gym</strong>
            <canvas class="occupancy-chart" id="occupancyChart-abc123"
                    data-occupancy="NaN" data-remaining="—"></canvas></div>"#;
        let facilities = parse_occupancy(html);
        assert_eq!(facilities.len(), 1);
        assert_eq!(facilities[0].current_occupancy, 0);
        assert_eq!(facilities[0].max_capacity, 0);
        // format() divides by max_capacity — zero must not panic.
        assert!(facilities[0].format().contains("0 / 0"));
        assert!(facilities[0].format().contains("0%"));
    }
}

#[cfg(test)]
mod schedule_tests {
    use super::*;
    use chrono::NaiveDate;

    // Trimmed real capture of the DevExtreme scheduler XHR feed (2026-07-07),
    // plus one synthetic FREQ=MONTHLY row to pin unknown-pattern degradation.
    const SCHEDULE_XHR_FIXTURE: &str = include_str!("fixtures/schedule_xhr.json");

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn fixture_appointments() -> Vec<Appointment> {
        parse_schedule_json(SCHEDULE_XHR_FIXTURE).unwrap()
    }

    #[test]
    fn expand_july_week_covers_weekly_allday_and_unknown() {
        // Mon Jul 6 - Sun Jul 12, 2026: two weekday-closure rules fire Mon-Fri
        // (5 each), the all-day one-off lands Wed, the unknown MONTHLY rule
        // degrades to a single undated tail entry. Cheer camp (Jul 17) and the
        // January holiday are out of window.
        let entries = expand_appointments(&fixture_appointments(), day(2026, 7, 6), 7);
        assert_eq!(entries.len(), 12, "got: {:#?}", entries);

        // Sorted by occurrence: midnight closure precedes the evening one.
        assert_eq!(
            entries[0].time,
            "Mon Jul 6 \u{b7} 12:00 AM \u{2013} 6:00 AM"
        );
        assert_eq!(entries[0].event, "Facility Closed");
        assert!(entries.iter().any(|e| {
            e.time == "Mon Jul 6 \u{b7} 7:00 PM \u{2013} 11:00 PM"
                && e.event == "Facility Closed \u{2014} Summer hours"
        }));
        assert!(entries.iter().any(|e| {
            e.time == "Wed Jul 8 \u{b7} all day"
                && e.event == "Maintenance Shutdown \u{2014} Annual pool maintenance"
        }));
        assert!(!entries.iter().any(|e| e.event.contains("Cheer")));
        assert!(!entries.iter().any(|e| e.event.contains("Holiday")));
    }

    #[test]
    fn weekly_until_cutoff_excludes_expired_rule() {
        // UNTIL=20260914T060000Z: the "Summer hours" closure must not fire
        // the week of Sept 21, while the no-UNTIL weekday rule still does.
        let entries = expand_appointments(&fixture_appointments(), day(2026, 9, 21), 7);
        assert!(!entries.iter().any(|e| e.event.contains("Summer hours")));
        let weekday_closures = entries
            .iter()
            .filter(|e| e.event == "Facility Closed")
            .count();
        assert_eq!(weekday_closures, 5);
    }

    #[test]
    fn weekly_recurrence_exception_skips_thanksgiving() {
        // RecurrenceException 20261126T000000: Thu Nov 26 is skipped, the
        // other four weekdays still fire.
        let entries = expand_appointments(&fixture_appointments(), day(2026, 11, 23), 7);
        let closures: Vec<&ScheduleEntry> = entries
            .iter()
            .filter(|e| e.event == "Facility Closed")
            .collect();
        assert_eq!(closures.len(), 4, "got: {:#?}", closures);
        assert!(!closures.iter().any(|e| e.time.contains("Nov 26")));
    }

    #[test]
    fn yearly_bysetpos_holiday_fires_on_third_monday_only() {
        // FREQ=YEARLY;BYDAY=MO;BYMONTH=1;BYSETPOS=3 -> Mon Jan 18, 2027.
        let mlk_week = expand_appointments(&fixture_appointments(), day(2027, 1, 18), 7);
        assert!(
            mlk_week
                .iter()
                .any(|e| { e.event.contains("Holiday") && e.time.starts_with("Mon Jan 18") })
        );
        let next_week = expand_appointments(&fixture_appointments(), day(2027, 1, 25), 7);
        assert!(!next_week.iter().any(|e| e.event.contains("Holiday")));
    }

    #[test]
    fn one_off_event_appears_only_in_its_week() {
        let entries = expand_appointments(&fixture_appointments(), day(2026, 7, 13), 7);
        assert!(entries.iter().any(|e| {
            e.event == "Cheer Camp 2" && e.time == "Fri Jul 17 \u{b7} 12:00 PM \u{2013} 4:00 PM"
        }));
    }

    #[test]
    fn unknown_recurrence_degrades_to_annotated_tail_entry() {
        let entries = expand_appointments(&fixture_appointments(), day(2026, 7, 6), 7);
        let last = entries.last().unwrap();
        assert_eq!(last.time, "recurring");
        assert!(last.event.contains("Monthly Deep Clean"));
    }

    // \u{2500}\u{2500} error paths \u{2500}\u{2500}

    #[test]
    fn parse_schedule_json_empty_array_is_ok() {
        assert!(parse_schedule_json("[]").unwrap().is_empty());
    }

    #[test]
    fn parse_schedule_json_truncated_errs() {
        assert!(parse_schedule_json(r#"[{"Id":"x","Text":"y","#).is_err());
    }

    #[test]
    fn parse_schedule_json_html_body_errs_usefully() {
        // If campusrec changes the feed again and starts serving HTML here,
        // the tool must error visibly instead of showing an empty schedule.
        let err = parse_schedule_json("<html><body>Maintenance</body></html>").unwrap_err();
        assert!(err.to_string().contains("schedule feed"));
    }

    #[test]
    fn appointment_with_unparseable_dates_is_dropped_not_panicking() {
        let body = r#"[{"Id":"x","Text":"Ghost","StartDate":"garbage","EndDate":"2026-07-08T10:00:00.000",
            "AllDay":false,"Description":"","RecurrenceRule":null,"RecurrenceException":null}]"#;
        let appts = parse_schedule_json(body).unwrap();
        let entries = expand_appointments(&appts, day(2026, 7, 6), 7);
        assert!(entries.is_empty());
    }
}

#[cfg(test)]
mod audit_tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn dt(y: i32, m: u32, d: u32, h: u32, min: u32) -> NaiveDateTime {
        day(y, m, d).and_hms_opt(h, min, 0).unwrap()
    }

    const HUB_FIXTURE: &str = include_str!("fixtures/group_exercise_hub.html");

    #[test]
    fn hub_term_is_the_unlinked_label() {
        let schedule = parse_group_exercise_page(HUB_FIXTURE);
        assert_eq!(schedule.term.as_deref(), Some("Fall 2026"));
    }

    #[test]
    fn hub_fixture_yields_the_current_schedule() {
        let schedule = parse_group_exercise_page(HUB_FIXTURE);
        // One entry per time slot in the table, including stacked cells.
        assert_eq!(schedule.classes.len(), 40);
        let yoga = schedule
            .classes
            .iter()
            .find(|c| c.name == "SUNRISE YOGA" && c.day == "Tuesday")
            .expect("Tuesday sunrise yoga");
        assert_eq!(yoga.time, "7:30a");
        assert_eq!(yoga.instructor, "Padma");
        assert_eq!(yoga.location_full, "Martial Arts Studio");
    }

    #[test]
    fn stacked_cell_yields_one_entry_per_class() {
        let schedule = parse_group_exercise_page(HUB_FIXTURE);
        let find = |day: &str, name: &str, who: &str| {
            schedule
                .classes
                .iter()
                .find(|c| c.day == day && c.name == name && c.instructor == who)
                .unwrap_or_else(|| panic!("{day} {name} w/{who}"))
        };
        let pilates = find("Tuesday", "MAT PILATES", "Padma");
        assert_eq!(
            (pilates.time.as_str(), pilates.location.as_str()),
            ("12:00p", "DNC")
        );
        let cycling = find("Tuesday", "INDOOR CYCLING", "Mireya");
        assert_eq!(
            (cycling.time.as_str(), cycling.location.as_str()),
            ("12:00p", "ACT")
        );
        // A name split across two program links stays one class.
        let core = find("Monday", "CORE CONDITIONING", "Isabella");
        assert_eq!(
            (core.time.as_str(), core.location.as_str()),
            ("6:45p", "DNC")
        );
        assert!(
            core.registration_url
                .as_deref()
                .unwrap()
                .contains("GetProgramDetails")
        );
        // An empty program link ahead of the real one must not swallow the class.
        let power = find("Tuesday", "POWER YOGA", "Padma");
        assert!(
            power
                .registration_url
                .as_deref()
                .unwrap()
                .contains("db7e2b6c")
        );
    }

    #[test]
    fn term_moves_with_the_unlinked_label() {
        let html = r#"<p><span><a href="/x_fall26">Fall 2026</a> | Winter 2027 | <a href="/x_spring26">Spring 2026</a></span></p>"#;
        let schedule = parse_group_exercise_page(html);
        assert_eq!(schedule.term.as_deref(), Some("Winter 2027"));
    }

    #[test]
    fn term_falls_back_to_heading_then_none() {
        let heading = r#"<h2><span><strong>SPRING 2027</strong></span></h2>"#;
        assert_eq!(
            parse_group_exercise_page(heading).term.as_deref(),
            Some("Spring 2027")
        );
        let bare = "<html><body><p>Fall into fitness | 2026</p></body></html>";
        assert_eq!(parse_group_exercise_page(bare).term, None);
    }

    #[test]
    fn yearly_negative_bysetpos_means_last_weekday_of_month() {
        let body = r#"[{"Text":"Facility Closed (Holiday)","StartDate":"2025-05-26T00:00:00","EndDate":"2025-05-26T23:59:59","AllDay":true,"Description":"","RecurrenceRule":"FREQ=YEARLY;BYMONTH=5;BYDAY=MO;BYSETPOS=-1","RecurrenceException":null}]"#;
        let appts = parse_schedule_json(body).unwrap();
        let entries = expand_appointments(&appts, day(2026, 5, 4), 28);
        let holidays: Vec<&ScheduleEntry> = entries
            .iter()
            .filter(|e| e.event.contains("Holiday"))
            .collect();
        assert_eq!(holidays.len(), 1, "{holidays:?}");
        assert!(
            holidays[0].time.starts_with("Mon May 25"),
            "{}",
            holidays[0].time
        );
    }

    #[test]
    fn schedule_renders_open_windows_between_closure_blocks() {
        let closed = |s: NaiveDateTime, e: NaiveDateTime| ScheduleEntry {
            time: format!(
                "{} {} – {}",
                s.format("%a %b %-d"),
                s.format("%-I:%M %p"),
                e.format("%-I:%M %p")
            ),
            event: "Facility Closed".into(),
            start: Some(s),
            end: Some(e),
        };
        let schedule = FacilitySchedule {
            facility_name: "Fitness Center".into(),
            facility_id: "abc".into(),
            entries: vec![
                closed(dt(2026, 9, 3, 0, 0), dt(2026, 9, 3, 6, 0)),
                closed(dt(2026, 9, 3, 9, 0), dt(2026, 9, 3, 15, 0)),
                closed(dt(2026, 9, 3, 19, 0), dt(2026, 9, 3, 23, 0)),
                closed(dt(2026, 9, 3, 23, 0), dt(2026, 9, 4, 0, 0)),
            ],
        };
        let out = schedule.format();
        assert!(out.contains("Thu Sep 3"), "{out}");
        assert!(
            out.contains("Open 6:00 AM – 9:00 AM, 3:00 PM – 7:00 PM"),
            "{out}"
        );
    }

    #[test]
    fn schedule_without_closure_blocks_lists_events_only() {
        let schedule = FacilitySchedule {
            facility_name: "Pool".into(),
            facility_id: "abc".into(),
            entries: vec![ScheduleEntry {
                time: "Thu Sep 3 9:00 AM – 11:00 AM".into(),
                event: "Cheer Camp 2".into(),
                start: Some(dt(2026, 9, 3, 9, 0)),
                end: Some(dt(2026, 9, 3, 11, 0)),
            }],
        };
        let out = schedule.format();
        assert!(out.contains("Cheer Camp 2"), "{out}");
        assert!(!out.contains("Open "), "{out}");
    }
}
