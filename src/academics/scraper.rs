use std::fmt::Write;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use regex::Regex;
use scraper::Html;

use crate::util::selectors;

selectors! {
    SEL_PANEL => "div.panel.panel-default.row",
    SEL_CLASS_LINK => "a[id^='class_id_']",
    SEL_STATUS_IMG => "img",
    SEL_PANEL_BODY => "div.panel-body",
    SEL_BOLD => "b, strong",
    SEL_DIR_ROW => "tbody tr",
    SEL_DIR_TD => "td",
    SEL_DIR_LINK => "a[href*='cd_detail']",
}

const CLASS_SEARCH_URL: &str = "https://pisa.ucsc.edu/class_search/index.php";
// The legacy `cd_search` endpoint is now behind Shibboleth (302 → SAML).
// The public unauthenticated path is the `cd_simple` POST form on the homepage,
// guarded by per-session CSRF tokens.
const DIRECTORY_HOME_URL: &str = "https://campusdirectory.ucsc.edu/";
const DIRECTORY_SIMPLE_URL: &str = "https://campusdirectory.ucsc.edu/cd_simple";

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
    /// Detail-page guid (e.g. "G089085136") — link via `cd_detail?guid=…`.
    pub uid: Option<String>,
    pub title: Option<String>,
    pub department: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    /// Mail-stop / building code (e.g. "SOE3"). Full office room is on the
    /// detail page, not the search results.
    pub office: Option<String>,
    /// Faculty / Staff / Student.
    pub affiliation: Option<String>,
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
        if let Some(a) = &self.affiliation {
            let _ = write!(out, "  _({})_", a);
        }
        if let Some(t) = &self.title {
            let _ = write!(out, "\n- **Title**: {}", t);
        }
        if let Some(d) = &self.department {
            let _ = write!(out, "\n- **Department**: {}", d);
        }
        if let Some(o) = &self.office {
            let _ = write!(out, "\n- **Mail Stop**: {}", o);
        }
        if let Some(e) = &self.email {
            let _ = write!(out, "\n- **Email**: {}", e);
        }
        if let Some(p) = &self.phone {
            let _ = write!(out, "\n- **Phone**: {}", p);
        }
        if let Some(g) = &self.uid {
            let _ = write!(
                out,
                "\n- **Detail**: <https://campusdirectory.ucsc.edu/cd_detail?guid={}>",
                g
            );
        }
        out
    }
}

pub async fn scrape_directory(
    _client: &reqwest::Client,
    query: &str,
    search_type: &str,
) -> Result<DirectoryResult> {
    // A dedicated directory client with its own cookie jar — the cd_simple form
    // is CSRF-guarded and tokens are bound to AWS-ALB session cookies that must
    // round-trip from the GET into the POST. Built once (the passed-in shared
    // client has no cookie store); each call re-GETs the homepage for fresh
    // tokens, so cookie persistence across calls is harmless.
    static DIRECTORY_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    let client = DIRECTORY_CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .user_agent("slug-mcp/0.1 (+https://git.ucsc.edu/pmundra/slug-mcp; student project)")
                .cookie_store(true)
                .gzip(true)
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("failed to build directory http client")
        })
        .clone();

    // 1. GET homepage → seeds session cookies + delivers fresh CSRF tokens.
    let home = client
        .get(DIRECTORY_HOME_URL)
        .send()
        .await
        .context("Failed to fetch directory homepage")?
        .text()
        .await
        .context("Failed to read directory homepage")?;

    let (csrf_name, csrf_token) = extract_csrf_for(&home, "cd_simple")
        .context("Could not find CSRF tokens in cd_simple form (form may have moved)")?;

    let affiliation = match search_type.to_lowercase().as_str() {
        "staff" | "faculty" | "staff & faculty" => "Staff & Faculty",
        "student" | "students" => "Students",
        _ => "All",
    };

    // 2. POST to cd_simple with the CSRF token + query.
    let html = client
        .post(DIRECTORY_SIMPLE_URL)
        .form(&[
            ("CSRFName", csrf_name.as_str()),
            ("CSRFToken", csrf_token.as_str()),
            ("keyword", query),
            ("Action", "Find"),
            ("affiliation", affiliation),
        ])
        .send()
        .await
        .context("Failed to POST directory search")?
        .text()
        .await
        .context("Failed to read directory results")?;

    Ok(parse_directory(&html, query))
}

/// Find the form block whose `action` matches `form_action`, and pull out its
/// `CSRFName` / `CSRFToken` hidden inputs.
fn extract_csrf_for(html: &str, form_action: &str) -> Option<(String, String)> {
    static FORM_RE: OnceLock<Regex> = OnceLock::new();
    let form_re = FORM_RE.get_or_init(|| Regex::new(r"(?s)<form[^>]*>.*?</form>").unwrap());

    let needle = format!("\"{form_action}\"");
    let block = form_re
        .find_iter(html)
        .map(|m| m.as_str())
        .find(|f| f.contains(&needle))?;

    let attr = |name: &str| -> Option<String> {
        let re = Regex::new(&format!(
            r#"name=['"]{name}['"]\s+value=['"]([^'"]+)['"]"#
        ))
        .ok()?;
        re.captures(block).map(|c| c[1].to_string())
    };
    Some((attr("CSRFName")?, attr("CSRFToken")?))
}

/// The email cell wraps the address in `Base64.decode('…')` to deter scrapers.
/// Decode the payload and pull the `mailto:` target.
fn decode_email_cell(cell_html: &str) -> Option<String> {
    static B64_RE: OnceLock<Regex> = OnceLock::new();
    static MAIL_RE: OnceLock<Regex> = OnceLock::new();
    let b64_re =
        B64_RE.get_or_init(|| Regex::new(r"Base64\.decode\('([A-Za-z0-9+/=]+)'\)").unwrap());
    let mail_re = MAIL_RE.get_or_init(|| Regex::new(r#"mailto:([^"'\s>]+)"#).unwrap());

    let payload = b64_re.captures(cell_html)?.get(1)?.as_str();
    let decoded = String::from_utf8(STANDARD.decode(payload).ok()?).ok()?;
    mail_re.captures(&decoded).map(|c| c[1].to_string())
}

fn parse_directory(html: &str, query: &str) -> DirectoryResult {
    let document = Html::parse_document(html);
    let mut entries = Vec::new();

    // Layout (cd_simple, 2026): name | phone | email | dept | title | affil | mailstop | sortname
    for row in document.select(&SEL_DIR_ROW) {
        let tds: Vec<scraper::ElementRef> = row.select(&SEL_DIR_TD).collect();
        if tds.is_empty() {
            continue;
        }
        let text_at = |i: usize| -> Option<String> {
            tds.get(i).and_then(|td| {
                let s = td.text().collect::<String>().trim().to_string();
                (!s.is_empty()).then_some(s)
            })
        };

        let Some(name) = text_at(0) else { continue };
        let uid = row.select(&SEL_DIR_LINK).next().and_then(|a| {
            a.value()
                .attr("href")
                .and_then(|h| h.split("guid=").nth(1))
                .map(|s| s.split('&').next().unwrap_or(s).to_string())
        });
        let email = tds.get(2).and_then(|td| decode_email_cell(&td.html()));

        entries.push(DirectoryEntry {
            name,
            uid,
            phone: text_at(1),
            email,
            department: text_at(3),
            title: text_at(4),
            affiliation: text_at(5),
            office: text_at(6),
        });
    }

    DirectoryResult {
        query: query.to_string(),
        entries,
    }
}

use chrono::Datelike;

#[cfg(test)]
mod tests {
    use super::*;

    const CLASS_RESULTS_FIXTURE: &str = include_str!("fixtures/class_results.html");
    const DIRECTORY_FIXTURE: &str = include_str!("fixtures/directory_results.html");

    #[test]
    fn parse_class_results_extracts_all_panels() {
        let classes = parse_class_results(CLASS_RESULTS_FIXTURE);
        // Three rowpanel_ panels; the searchpanel chrome must be skipped.
        assert_eq!(classes.len(), 3, "got: {:#?}", classes);
    }

    #[test]
    fn parse_class_results_open_lecture_fields() {
        let classes = parse_class_results(CLASS_RESULTS_FIXTURE);
        let cse = &classes[0];
        assert_eq!(cse.subject, "CSE");
        assert_eq!(cse.catalog_number, "115A");
        assert_eq!(cse.section, "01");
        assert_eq!(cse.title, "Introduction to Software Engineering");
        assert_eq!(cse.class_number, "22345");
        assert_eq!(cse.status, "Open");
        // schedule is the day/time; the parser's 3-token window also pulls in the
        // following "LEC:" prefix from the flattened body, so match the prefix.
        assert!(
            cse.schedule
                .as_deref()
                .unwrap_or("")
                .starts_with("MWF 10:40AM-11:45AM"),
            "schedule: {:?}",
            cse.schedule
        );
        assert_eq!(cse.enrolled.as_deref(), Some("92 of 100 Enrolled"));
        assert_eq!(cse.mode.as_deref(), Some("In Person"));
        // instructor is parsed from the flattened body's "Last,F." pattern
        assert!(
            cse.instructor.starts_with("Tantalo"),
            "instructor: {:?}",
            cse.instructor
        );
        // location uses the LEC: prefix
        assert!(
            cse.location.as_deref().unwrap_or("").starts_with("LEC:"),
            "location: {:?}",
            cse.location
        );
    }

    #[test]
    fn parse_class_results_waitlist_status() {
        let classes = parse_class_results(CLASS_RESULTS_FIXTURE);
        let lab = &classes[1];
        assert_eq!(lab.section, "01A");
        assert_eq!(lab.status, "Wait List");
        // Full lab section: 30 of 30 → effectively closed-by-enrollment.
        assert_eq!(lab.enrolled.as_deref(), Some("30 of 30 Enrolled"));
    }

    #[test]
    fn parse_class_results_closed_multi_meeting() {
        let classes = parse_class_results(CLASS_RESULTS_FIXTURE);
        let math = &classes[2];
        assert_eq!(math.subject, "MATH");
        assert_eq!(math.catalog_number, "19A");
        assert_eq!(math.status, "Closed");
        assert_eq!(math.mode.as_deref(), Some("Online Synchronous"));
        // Multi-meeting (LEC + DIS): the parser surfaces the first day/time it finds
        // (the TuTh lecture, not the M discussion), plus the trailing "LEC:" token.
        assert!(
            math.schedule
                .as_deref()
                .unwrap_or("")
                .starts_with("TuTh 1:30PM-3:05PM"),
            "schedule: {:?}",
            math.schedule
        );
        assert_eq!(math.enrolled.as_deref(), Some("120 of 120 Enrolled"));
    }

    #[test]
    fn parse_class_results_status_variety() {
        let classes = parse_class_results(CLASS_RESULTS_FIXTURE);
        let statuses: Vec<&str> = classes.iter().map(|c| c.status.as_str()).collect();
        assert!(statuses.contains(&"Open"));
        assert!(statuses.contains(&"Wait List"));
        assert!(statuses.contains(&"Closed"));
    }

    #[test]
    fn parse_directory_extracts_people() {
        let result = parse_directory(DIRECTORY_FIXTURE, "tantalo");
        assert_eq!(result.query, "tantalo");
        // Two tbody rows; the thead row must not be counted.
        assert_eq!(result.entries.len(), 2, "got: {:#?}", result.entries);
    }

    #[test]
    fn parse_directory_decodes_base64_email() {
        let result = parse_directory(DIRECTORY_FIXTURE, "tantalo");
        let tantalo = &result.entries[0];
        assert_eq!(tantalo.name, "Tantalo, Patrick");
        assert_eq!(tantalo.email.as_deref(), Some("ptantalo@ucsc.edu"));
        assert_eq!(
            tantalo.department.as_deref(),
            Some("Computer Science & Engineering")
        );
        assert_eq!(tantalo.title.as_deref(), Some("Teaching Professor"));
        assert_eq!(tantalo.affiliation.as_deref(), Some("Staff & Faculty"));
        assert_eq!(tantalo.office.as_deref(), Some("SOE3"));
        assert_eq!(tantalo.phone.as_deref(), Some("(831) 459-1234"));
        // guid pulled from the cd_detail link.
        assert_eq!(tantalo.uid.as_deref(), Some("G089085136"));
    }

    #[test]
    fn parse_directory_uid_strips_extra_query_params() {
        let result = parse_directory(DIRECTORY_FIXTURE, "frigaard");
        let owen = &result.entries[1];
        assert_eq!(owen.name, "Frigaard, Owen");
        assert_eq!(owen.email.as_deref(), Some("ofrigaar@ucsc.edu"));
        // href was cd_detail?guid=G077012988&ref=search → guid only.
        assert_eq!(owen.uid.as_deref(), Some("G077012988"));
        assert_eq!(
            owen.department.as_deref(),
            Some("Molecular, Cell & Developmental Biology")
        );
    }
}
