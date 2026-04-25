use std::fmt::Write;

use anyhow::{Context, Result};
use scraper::Html;

use crate::util::selectors;

selectors! {
    SEL_PANEL => "div.panel.panel-default.row",
    SEL_CLASS_LINK => "a[id^='class_id_']",
    SEL_STATUS_IMG => "img",
    SEL_PANEL_BODY => "div.panel-body",
    SEL_BOLD => "b, strong",
    SEL_DIR_ROW => "tr",
    SEL_DIR_LINK => "a[href*='cd_detail']",
    SEL_DIR_RESULT => ".cd-result, .search-result, .result-row",
}

const CLASS_SEARCH_URL: &str = "https://pisa.ucsc.edu/class_search/index.php";
const DIRECTORY_SEARCH_URL: &str = "https://campusdirectory.ucsc.edu/cd_search";

// ─── Class Search ───

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClassSearchResult {
    pub term: String,
    pub classes: Vec<ClassEntry>,
    pub total_results: usize,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClassEntry {
    pub class_number: String,
    pub subject: String,
    pub catalog_number: String,
    pub section: String,
    pub title: String,
    pub status: String,
    pub instructor: String,
    pub location: Option<String>,
    pub schedule: Option<String>,
    pub enrolled: Option<String>,
    pub mode: Option<String>,
}

impl ClassSearchResult {
    pub fn format(&self) -> String {
        let mut out = format!("## Class Search Results ({})\n", self.term);
        let _ = writeln!(out, "Showing {} results", self.classes.len());
        for class in &self.classes {
            out.push('\n');
            out.push_str(&class.format());
        }
        out
    }
}

impl ClassEntry {
    pub fn format(&self) -> String {
        let mut out = format!(
            "### {} {} - {} {}\n- **Status**: {}",
            self.subject, self.catalog_number, self.section, self.title, self.status
        );
        if !self.instructor.is_empty() {
            let _ = write!(out, "\n- **Instructor**: {}", self.instructor);
        }
        if let Some(sched) = &self.schedule {
            let _ = write!(out, "\n- **Schedule**: {}", sched);
        }
        if let Some(loc) = &self.location {
            let _ = write!(out, "\n- **Location**: {}", loc);
        }
        if let Some(enr) = &self.enrolled {
            let _ = write!(out, "\n- **Enrollment**: {}", enr);
        }
        if let Some(mode) = &self.mode {
            let _ = write!(out, "\n- **Mode**: {}", mode);
        }
        let _ = write!(out, "\n- **Class #**: {}", self.class_number);
        out
    }
}

#[derive(Debug, Clone)]
pub struct ClassSearchParams {
    pub term: String,
    pub subject: Option<String>,
    pub catalog_number: Option<String>,
    pub instructor: Option<String>,
    pub title: Option<String>,
    pub ge: Option<String>,
    pub reg_status: String, // "O" for open only, "all" for all
    pub career: Option<String>,
    pub page_start: u32,
    pub page_size: u32,
}

pub async fn scrape_class_search(
    client: &reqwest::Client,
    params: &ClassSearchParams,
) -> Result<ClassSearchResult> {
    // First GET the form page to pick up any session cookies
    let _ = client
        .get(CLASS_SEARCH_URL)
        .send()
        .await
        .context("Failed to load class search form")?;

    // Build the POST form data
    let mut form: Vec<(&str, String)> = vec![
        ("action", "results".to_string()),
        ("binds[:term]", params.term.clone()),
        ("binds[:reg_status]", params.reg_status.clone()),
        ("rec_start", params.page_start.to_string()),
        ("rec_dur", params.page_size.to_string()),
    ];

    if let Some(ref s) = params.subject {
        form.push(("binds[:subject]", s.clone()));
    }
    if let Some(ref n) = params.catalog_number {
        form.push(("binds[:catalog_nbr]", n.clone()));
        form.push(("binds[:catalog_nbr_op]", "=".to_string()));
    }
    if let Some(ref i) = params.instructor {
        form.push(("binds[:instr_name]", i.clone()));
        form.push(("binds[:instr_name_op]", "contains".to_string()));
    }
    if let Some(ref t) = params.title {
        form.push(("binds[:title]", t.clone()));
    }
    if let Some(ref g) = params.ge {
        form.push(("binds[:ge]", g.clone()));
    }
    if let Some(ref c) = params.career {
        form.push(("binds[:acad_career]", c.clone()));
    }

    let resp = client
        .post(CLASS_SEARCH_URL)
        .form(&form)
        .send()
        .await
        .context("Failed to submit class search")?;

    let html = resp
        .text()
        .await
        .context("Failed to read class search results")?;

    let classes = parse_class_results(&html);
    let term_name = term_code_to_name(&params.term);

    Ok(ClassSearchResult {
        term: term_name,
        total_results: classes.len(),
        classes,
    })
}

fn parse_class_results(html: &str) -> Vec<ClassEntry> {
    let document = Html::parse_document(html);

    let mut classes = Vec::new();

    for panel in document.select(&SEL_PANEL) {
        // Check if this is a class panel (has an id starting with rowpanel_)
        let panel_id = panel.value().attr("id").unwrap_or("");
        if !panel_id.starts_with("rowpanel_") {
            continue;
        }

        // Extract course info from the link text
        let (subject, catalog_number, section, title, class_number) =
            if let Some(link) = panel.select(&SEL_CLASS_LINK).next() {
                let text = link.text().collect::<String>();
                let id = link.value().attr("id").unwrap_or("");
                let class_num = id.strip_prefix("class_id_").unwrap_or("").to_string();
                parse_course_header(&text, class_num)
            } else {
                continue;
            };

        // Extract status from img alt
        let status = panel
            .select(&SEL_STATUS_IMG)
            .find_map(|img| {
                let alt = img.value().attr("alt").unwrap_or("");
                if alt.contains("Open") || alt.contains("Closed") || alt.contains("Wait") {
                    Some(alt.to_string())
                } else {
                    None
                }
            })
            .unwrap_or_default();

        // Extract details from panel body text
        let body_text = if let Some(body) = panel.select(&SEL_PANEL_BODY).next() {
            body.text().collect::<String>()
        } else {
            String::new()
        };

        let instructor = extract_after_icon(&body_text, "instructor");
        let location = extract_field(&body_text, "location");
        let schedule = extract_field(&body_text, "schedule");
        let enrolled = extract_enrollment(&body_text);

        // Extract instruction mode from bold text
        let mode = panel.select(&SEL_BOLD).find_map(|b| {
            let text = b.text().collect::<String>().trim().to_string();
            if text.contains("In Person")
                || text.contains("Online")
                || text.contains("Hybrid")
                || text.contains("Synchronous")
                || text.contains("Asynchronous")
            {
                Some(text)
            } else {
                None
            }
        });

        classes.push(ClassEntry {
            class_number,
            subject,
            catalog_number,
            section,
            title,
            status,
            instructor,
            location,
            schedule,
            enrolled,
            mode,
        });
    }

    classes
}

fn parse_course_header(text: &str, class_number: String) -> (String, String, String, String, String) {
    // Format: "SUBJECT NUMBER - SECTION  Title" or "SUBJECT NUMBER - SECTION Title"
    let text = text.trim();
    let parts: Vec<&str> = text.splitn(2, " - ").collect();
    if parts.len() == 2 {
        let subject_num: Vec<&str> = parts[0].trim().splitn(2, ' ').collect();
        let subject = subject_num.first().unwrap_or(&"").to_string();
        let catalog = subject_num.get(1).unwrap_or(&"").trim().to_string();

        // Section and title: "01  Introduction to Software Engineering"
        let sec_title = parts[1].trim();
        let sec_parts: Vec<&str> = sec_title.splitn(2, |c: char| c.is_whitespace()).collect();
        let section = sec_parts.first().unwrap_or(&"").trim().to_string();
        let title = sec_parts
            .get(1)
            .unwrap_or(&"")
            .trim()
            .to_string();

        (subject, catalog, section, title, class_number)
    } else {
        (text.to_string(), String::new(), String::new(), String::new(), class_number)
    }
}

fn extract_after_icon(body: &str, _field: &str) -> String {
    // The body text is flattened — instructor appears after icon text
    // We parse from the raw text looking for name patterns
    // Simple approach: extract anything that looks like "LastName,F." pattern
    let mut instructor = String::new();
    for word in body.split_whitespace() {
        if word.contains(',') && word.len() > 2 && word.chars().next().unwrap_or(' ').is_uppercase() {
            // Looks like "Smith,J." — grab it and the next word
            instructor = word.trim_end_matches(',').to_string();
            if let Some(rest) = body.split(word).nth(1) {
                let next: String = rest
                    .split_whitespace()
                    .take(1)
                    .collect::<Vec<_>>()
                    .join(" ");
                if !next.is_empty() {
                    instructor = format!("{},{}", instructor, next.trim_end_matches(','));
                }
            }
            break;
        }
    }
    instructor
}

fn extract_field(body: &str, field: &str) -> Option<String> {
    match field {
        "location" => {
            // Look for patterns like "LEC: Building Room" or "SEM: Building Room"
            for prefix in &["LEC:", "SEM:", "LAB:", "DIS:", "STU:", "FLD:", "TUT:"] {
                if let Some(pos) = body.find(prefix) {
                    let rest = &body[pos..];
                    let val: String = rest
                        .split_whitespace()
                        .take(4) // "LEC: Building Room Number"
                        .collect::<Vec<_>>()
                        .join(" ");
                    if !val.is_empty() {
                        return Some(val);
                    }
                }
            }
            None
        }
        "schedule" => {
            // Look for day+time patterns like "MWF 10:40AM-11:45AM" or "TR 1:30PM-3:05PM"
            let days_pattern = ["MWF", "TuTh", "MW", "MF", "WF", "Tu ", "Th ", "Sa ", "Su ", "M ", "W ", "F "];
            for pat in &days_pattern {
                if let Some(pos) = body.find(pat) {
                    let rest = &body[pos..];
                    let val: String = rest
                        .split_whitespace()
                        .take(3)
                        .collect::<Vec<_>>()
                        .join(" ");
                    if val.contains("AM") || val.contains("PM") {
                        return Some(val);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

fn extract_enrollment(body: &str) -> Option<String> {
    // Look for "X of Y Enrolled"
    let words: Vec<&str> = body.split_whitespace().collect();
    for (i, word) in words.iter().enumerate() {
        if *word == "of"
            && i > 0
            && i + 2 < words.len()
            && words[i + 2] == "Enrolled"
            && words[i - 1].parse::<u32>().is_ok()
            && words[i + 1].parse::<u32>().is_ok()
        {
            return Some(format!("{} of {} Enrolled", words[i - 1], words[i + 1]));
        }
    }
    None
}

// ─── Term Code Helpers ───

/// Determine the current/upcoming UCSC term code.
/// UCSC uses: 2260=Winter 2026, 2262=Spring 2026, 2264=Summer 2026, 2268=Fall 2026
pub fn current_term_code() -> String {
    let now = crate::util::now_pacific();
    let year = now.year();
    let month = now.month();

    let (term_digit, term_year) = TERMS
        .iter()
        .find(|t| t.month_range.contains(&month))
        .map(|t| (t.digit, year))
        .unwrap_or((2, year)); // fallback to Spring if month is somehow out of range

    // UCSC term codes encode (year, term) in 4 digits: e.g. 2262 = Spring 2026,
    // 2260 = Winter 2026, 2270 = Winter 2027. The arithmetic below is anchored
    // by the table — bumping `Term::base_year` is the only thing that needs to
    // change as years roll forward.
    let base = TERM_BASE_OFFSET + (term_year - TERM_BASE_YEAR) * 10;
    format!("{}", base + term_digit as i32)
}

/// Calendar-year → UCSC term mapping. The base year/offset anchor the encoding;
/// see `current_term_code` for the formula.
const TERM_BASE_YEAR: i32 = 2020;
const TERM_BASE_OFFSET: i32 = 2200; // i.e. 2200 = Winter 2020

struct Term {
    digit: u8,
    name: &'static str,
    month_range: std::ops::RangeInclusive<u32>,
}

const TERMS: &[Term] = &[
    Term { digit: 0, name: "Winter", month_range: 1..=3 },
    Term { digit: 2, name: "Spring", month_range: 4..=6 },
    Term { digit: 4, name: "Summer", month_range: 7..=8 },
    Term { digit: 8, name: "Fall",   month_range: 9..=12 },
];

/// Convenience: compute the human-readable name of the current/upcoming term
/// (e.g. "Spring 2026"). Useful for output headers in other modules.
pub fn current_term_name() -> String {
    term_code_to_name(&current_term_code())
}

fn term_code_to_name(code: &str) -> String {
    let Ok(num) = code.parse::<i32>() else {
        return code.to_string();
    };
    let term_digit = (num % 10) as u8;
    let year = TERM_BASE_YEAR + (num - TERM_BASE_OFFSET) / 10;
    let term = TERMS
        .iter()
        .find(|t| t.digit == term_digit)
        .map(|t| t.name)
        .unwrap_or("Unknown");
    format!("{} {}", term, year)
}

// ─── Campus Directory ───

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DirectoryResult {
    pub query: String,
    pub entries: Vec<DirectoryEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub uid: Option<String>,
    pub title: Option<String>,
    pub department: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
}

impl DirectoryResult {
    pub fn format(&self) -> String {
        let mut out = format!(
            "## Directory Search: \"{}\"\n{} results\n",
            self.query,
            self.entries.len()
        );
        for entry in &self.entries {
            out.push('\n');
            out.push_str(&entry.format());
        }
        out
    }
}

impl DirectoryEntry {
    pub fn format(&self) -> String {
        let mut out = format!("### {}", self.name);
        if let Some(t) = &self.title {
            let _ = write!(out, "\n- **Title**: {}", t);
        }
        if let Some(d) = &self.department {
            let _ = write!(out, "\n- **Department**: {}", d);
        }
        if let Some(e) = &self.email {
            let _ = write!(out, "\n- **Email**: {}", e);
        }
        if let Some(p) = &self.phone {
            let _ = write!(out, "\n- **Phone**: {}", p);
        }
        out
    }
}

pub async fn scrape_directory(
    client: &reqwest::Client,
    query: &str,
    search_type: &str,
) -> Result<DirectoryResult> {
    let url = format!(
        "{}?type={}&search={}",
        DIRECTORY_SEARCH_URL,
        search_type,
        urlencoding::encode(query)
    );

    let resp = client
        .get(&url)
        .send()
        .await
        .context("Failed to fetch directory search")?;

    let html = resp
        .text()
        .await
        .context("Failed to read directory results")?;

    Ok(parse_directory(&html, query))
}

fn parse_directory(html: &str, query: &str) -> DirectoryResult {
    let document = Html::parse_document(html);

    let mut entries = Vec::new();

    // Try to find results in table rows (common directory pattern)
    for row in document.select(&SEL_DIR_ROW) {
        let cells: Vec<String> = row
            .children()
            .filter_map(|child| {
                scraper::ElementRef::wrap(child).map(|el| el.text().collect::<String>().trim().to_string())
            })
            .filter(|s| !s.is_empty())
            .collect();

        if cells.is_empty() {
            continue;
        }

        // Extract link for uid
        let uid = row.select(&SEL_DIR_LINK).next().and_then(|a| {
            a.value()
                .attr("href")
                .and_then(|h| h.split("uid=").nth(1))
                .map(|s| s.to_string())
        });

        // Skip header rows
        if cells.first().is_some_and(|c| c == "Name" || c == "Department") {
            continue;
        }

        let name = cells.first().cloned().unwrap_or_default();
        if name.is_empty() {
            continue;
        }

        entries.push(DirectoryEntry {
            name,
            uid,
            title: cells.get(1).cloned().filter(|s| !s.is_empty()),
            department: cells.get(2).cloned().filter(|s| !s.is_empty()),
            email: cells.get(3).cloned().filter(|s| !s.is_empty()),
            phone: cells.get(4).cloned().filter(|s| !s.is_empty()),
        });
    }

    // If table parsing found nothing, try div-based layout
    if entries.is_empty() {
        for el in document.select(&SEL_DIR_RESULT) {
            let text = el.text().collect::<String>();
            let lines: Vec<&str> = text.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
            if let Some(name) = lines.first() {
                entries.push(DirectoryEntry {
                    name: name.to_string(),
                    uid: None,
                    title: lines.get(1).map(|s| s.to_string()),
                    department: lines.get(2).map(|s| s.to_string()),
                    email: lines.iter().find(|s| s.contains('@')).map(|s| s.to_string()),
                    phone: None,
                });
            }
        }
    }

    DirectoryResult {
        query: query.to_string(),
        entries,
    }
}

use chrono::Datelike;
