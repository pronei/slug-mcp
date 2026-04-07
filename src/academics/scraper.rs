use std::fmt;

use anyhow::{Context, Result};
use scraper::{Html, Selector};

use crate::util::{sel, selectors};

selectors! {
    SEL_PANEL => "div.panel.panel-default",
    SEL_CLASS_LINK => "a[id^='class_id_']",
    SEL_STATUS_IMG => "img[alt]",
    SEL_PANEL_BODY => "div.panel-body",
    SEL_BOLD => "b",
    SEL_DIR_ROW => "tr",
    SEL_DIR_LINK => "a[href]",
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

impl fmt::Display for ClassSearchResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "## Class Search Results ({})", self.term)?;
        writeln!(f, "Showing {} results", self.classes.len())?;
        for class in &self.classes {
            write!(f, "\n{}", class)?;
        }
        Ok(())
    }
}

impl fmt::Display for ClassEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "### {} {} - {} {}\n- **Status**: {}",
            self.subject, self.catalog_number, self.section, self.title, self.status
        )?;
        if !self.instructor.is_empty() {
            write!(f, "\n- **Instructor**: {}", self.instructor)?;
        }
        if let Some(sched) = &self.schedule {
            write!(f, "\n- **Schedule**: {}", sched)?;
        }
        if let Some(loc) = &self.location {
            write!(f, "\n- **Location**: {}", loc)?;
        }
        if let Some(enr) = &self.enrolled {
            write!(f, "\n- **Enrollment**: {}", enr)?;
        }
        if let Some(mode) = &self.mode {
            write!(f, "\n- **Mode**: {}", mode)?;
        }
        write!(f, "\n- **Class #**: {}", self.class_number)?;
        Ok(())
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
    let panel_sel = sel(&SEL_PANEL, "div.panel.panel-default.row");
    let link_sel = sel(&SEL_CLASS_LINK, "a[id^='class_id_']");
    let img_sel = sel(&SEL_STATUS_IMG, "img");
    let body_sel = sel(&SEL_PANEL_BODY, "div.panel-body");
    let bold_sel = sel(&SEL_BOLD, "b, strong");

    let mut classes = Vec::new();

    for panel in document.select(panel_sel) {
        // Check if this is a class panel (has an id starting with rowpanel_)
        let panel_id = panel.value().attr("id").unwrap_or("");
        if !panel_id.starts_with("rowpanel_") {
            continue;
        }

        // Extract course info from the link text
        let (subject, catalog_number, section, title, class_number) =
            if let Some(link) = panel.select(link_sel).next() {
                let text = link.text().collect::<String>();
                let id = link.value().attr("id").unwrap_or("");
                let class_num = id.strip_prefix("class_id_").unwrap_or("").to_string();
                parse_course_header(&text, class_num)
            } else {
                continue;
            };

        // Extract status from img alt
        let status = panel
            .select(img_sel)
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
        let body_text = if let Some(body) = panel.select(body_sel).next() {
            body.text().collect::<String>()
        } else {
            String::new()
        };

        let instructor = extract_after_icon(&body_text, "instructor");
        let location = extract_field(&body_text, "location");
        let schedule = extract_field(&body_text, "schedule");
        let enrolled = extract_enrollment(&body_text);

        // Extract instruction mode from bold text
        let mode = panel.select(bold_sel).find_map(|b| {
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
    let now = chrono::Local::now();
    let year = now.year();
    let month = now.month();

    // Determine which term we're in or approaching
    let (term_year, term_suffix) = match month {
        1..=3 => (year, 60),   // Winter → show current quarter (Spring registration likely open)
        4..=6 => (year, 62),   // Spring
        7..=8 => (year, 64),   // Summer
        9..=12 => (year, 68),  // Fall
        _ => (year, 62),
    };

    // UCSC term code: century_offset + last2digits * 10 + suffix_ones_digit
    // 2026 → "22" prefix, then suffix: 60,62,64,68
    // Actually the pattern is: first 3 digits = year encoding, last digit = term
    // 2260 → year=2026, term=Winter(0)
    // 2262 → year=2026, term=Spring(2)
    // Formula: (year - 1900) * 10 + term_digit... let's just do: year * 10 - 18740 + term_digit
    // 2026: 2026*10 - 18740 = 20260-18740 = 1520 ... that's not right
    // Let me use: 2260 for 2026 Winter: (year - 1900 - 100) * 10 + 2000 + term_digit
    // Simpler: for 2026: base = 2260, then +2=spring, +4=summer, +8=fall
    // For 2027: base = 2270
    // Pattern: base = (year - 1900) * 10 + 600... no
    // 2024: Fall = 2248, 2025: Winter=2250, Spring=2252, Summer=2254, Fall=2258
    // 2026: Winter=2260, Spring=2262
    // Pattern: (year - 2000 + 100) * 10 + term_digit = (year - 1900) * 10 + term_digit
    // 2026: 126 * 10 = 1260... no
    // Actually: 2260 for 2026 Winter. 2260 / 10 = 226. 226 + 1900 = 2126? No.
    // Let me just look at the raw numbers:
    // 2250 = Winter 2025, 2252 = Spring 2025, 2254 = Summer 2025, 2258 = Fall 2025
    // 2260 = Winter 2026, 2262 = Spring 2026
    // Diff between years: 2260 - 2250 = 10
    // So: base = 2200 + (year - 2020) * 10 = 2200 + 60 = 2260 for 2026 ✓
    // term_digits: Winter=0, Spring=2, Summer=4, Fall=8

    let base = 2200 + (term_year - 2020) * 10;
    let term_digit = match term_suffix {
        60 => 0,
        62 => 2,
        64 => 4,
        68 => 8,
        _ => 2,
    };

    format!("{}", base + term_digit)
}

fn term_code_to_name(code: &str) -> String {
    if let Ok(num) = code.parse::<i32>() {
        let term_digit = num % 10;
        let year_part = num / 10;
        let year = 2020 + (year_part - 220);
        let term = match term_digit {
            0 => "Winter",
            2 => "Spring",
            4 => "Summer",
            8 => "Fall",
            _ => "Unknown",
        };
        format!("{} {}", term, year)
    } else {
        code.to_string()
    }
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

impl fmt::Display for DirectoryResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "## Directory Search: \"{}\"\n{} results\n",
            self.query,
            self.entries.len()
        )?;
        for entry in &self.entries {
            write!(f, "\n{}", entry)?;
        }
        Ok(())
    }
}

impl fmt::Display for DirectoryEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "### {}", self.name)?;
        if let Some(t) = &self.title {
            write!(f, "\n- **Title**: {}", t)?;
        }
        if let Some(d) = &self.department {
            write!(f, "\n- **Department**: {}", d)?;
        }
        if let Some(e) = &self.email {
            write!(f, "\n- **Email**: {}", e)?;
        }
        if let Some(p) = &self.phone {
            write!(f, "\n- **Phone**: {}", p)?;
        }
        Ok(())
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
    let row_sel = sel(&SEL_DIR_ROW, "tr");
    let link_sel = sel(&SEL_DIR_LINK, "a[href*='cd_detail']");

    let mut entries = Vec::new();

    // Try to find results in table rows (common directory pattern)
    for row in document.select(row_sel) {
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
        let uid = row.select(link_sel).next().and_then(|a| {
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
        if let Ok(result_sel) = Selector::parse(".cd-result, .search-result, .result-row") {
            for el in document.select(&result_sel) {
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
    }

    DirectoryResult {
        query: query.to_string(),
        entries,
    }
}

use chrono::Datelike;
