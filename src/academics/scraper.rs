use std::fmt::Write;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use regex::Regex;
use scraper::{ElementRef, Html};

use crate::util::selectors;

selectors! {
    SEL_PANEL => "div.panel.panel-default.row",
    SEL_CLASS_LINK => "a[id^='class_id_']",
    SEL_CLASS_NBR => "a[id^='class_nbr_']",
    SEL_STATUS_IMG => "img[alt]",
    SEL_SR_ONLY => "i.sr-only",
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
    /// Academic session, e.g. "Summer Session 1 (5 Weeks)", "Summer Session 10
    /// Weeks". Only present for terms that have sessions (summer); `None` for
    /// regular fall/winter/spring terms.
    pub session: Option<String>,
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
        if let Some(sess) = &self.session {
            let _ = write!(out, "\n- **Session**: {}", sess);
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
    /// PISA `binds[:session_code]` value (e.g. "5S1", "5S2", "S10", "S8W").
    /// Only meaningful for summer terms.
    pub session_code: Option<String>,
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
    if let Some(ref sc) = params.session_code {
        form.push(("binds[:session_code]", sc.clone()));
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
        // Only real result panels carry an id like "rowpanel_3"; the search-form
        // chrome and pagination rows reuse the panel class but not the id.
        let panel_id = panel.value().attr("id").unwrap_or("");
        if !panel_id.starts_with("rowpanel_") {
            continue;
        }

        // Course header: "AM 10 - 01\u{a0}\u{a0}\u{a0}Lin Algebra for Engrs".
        let Some(link) = panel.select(&SEL_CLASS_LINK).next() else {
            continue;
        };
        let header = normalize_ws(&link.text().collect::<String>());
        let (subject, catalog_number, section, title) = parse_course_header(&header);

        // Class number: the dedicated `class_nbr_` link is the source of truth;
        // fall back to the `class_id_<n>` element id.
        let class_number = panel
            .select(&SEL_CLASS_NBR)
            .next()
            .map(|a| a.text().collect::<String>().trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                link.value()
                    .attr("id")
                    .and_then(|id| id.strip_prefix("class_id_"))
                    .map(str::to_string)
            })
            .unwrap_or_default();

        // Status comes from the seat-status image alt (skip the page logo).
        let status = panel
            .select(&SEL_STATUS_IMG)
            .find_map(|img| {
                let alt = img.value().attr("alt").unwrap_or("");
                is_status_alt(alt).then(|| normalize_status(alt))
            })
            .unwrap_or_default();

        // The rest of the fields are screen-reader-labeled rows of the form
        // `<i class="sr-only">Label:</i> value` — a stable semantic contract,
        // far sturdier than scraping flattened body text.
        let (mut instructor, mut location, mut schedule, mut session, mut mode) =
            (String::new(), None, None, None, None);
        for sr in panel.select(&SEL_SR_ONLY) {
            let Some((label, value)) = labeled_value(sr) else {
                continue;
            };
            if value.is_empty() {
                continue;
            }
            match label.as_str() {
                "Instructor" => instructor = value,
                "Location" => location = Some(value),
                "Day and Time" => schedule = Some(value),
                "Session" => session = Some(value),
                "Instruction Mode" => mode = Some(value),
                _ => {}
            }
        }

        // Enrollment ("87 of 150 Enrolled") sits in its own unlabeled column.
        let enrolled = extract_enrollment(&normalize_ws(&panel.text().collect::<String>()));

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
            session,
            enrolled,
            mode,
        });
    }

    classes
}

/// Collapse `&nbsp;` (U+00A0) and runs of whitespace to single ASCII spaces.
fn normalize_ws(s: &str) -> String {
    s.replace('\u{a0}', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// "AM 10 - 01 Lin Algebra for Engrs" → ("AM", "10", "01", "Lin Algebra for Engrs").
/// Expects whitespace already normalized.
fn parse_course_header(text: &str) -> (String, String, String, String) {
    let Some((left, right)) = text.split_once(" - ") else {
        return (text.to_string(), String::new(), String::new(), String::new());
    };
    let (subject, catalog) = left
        .trim()
        .split_once(' ')
        .map(|(s, c)| (s.trim().to_string(), c.trim().to_string()))
        .unwrap_or((left.trim().to_string(), String::new()));
    let (section, title) = right
        .trim()
        .split_once(char::is_whitespace)
        .map(|(s, t)| (s.trim().to_string(), t.trim().to_string()))
        .unwrap_or((right.trim().to_string(), String::new()));
    (subject, catalog, section, title)
}

/// Given a `<i class="sr-only">Label:</i>` element, return `(Label, value)` where
/// the value is the remaining text of the row after the label. The sibling
/// `<i class="fa">` icon contributes no text, so the row text is just
/// `"Label: value"`.
fn labeled_value(sr: ElementRef) -> Option<(String, String)> {
    let label = sr.text().collect::<String>();
    let label = label.trim().trim_end_matches(':').trim().to_string();
    if label.is_empty() {
        return None;
    }
    let parent = ElementRef::wrap(sr.parent()?)?;
    let full = normalize_ws(&parent.text().collect::<String>());
    let value = full
        .strip_prefix(&format!("{label}:"))
        .unwrap_or(&full)
        .trim()
        .to_string();
    Some((label, value))
}

/// Is this image-alt a seat-status indicator (vs. the page logo)?
fn is_status_alt(alt: &str) -> bool {
    alt.contains("Open") || alt.contains("Closed") || alt.contains("Wait")
}

/// Normalize PISA status alts ("Open", "Closed", "Closed with Wait List") to a
/// short label. "… Wait List" wins over "Closed" since a wait list is joinable.
fn normalize_status(alt: &str) -> String {
    if alt.contains("Wait") {
        "Wait List".to_string()
    } else if alt.contains("Closed") {
        "Closed".to_string()
    } else if alt.contains("Open") {
        "Open".to_string()
    } else {
        alt.to_string()
    }
}

/// Map a friendly summer-session string to PISA's `binds[:session_code]` value.
/// Accepts "1"/"session 1", "2", "10"/"10 week", "8"/"8 week", or a raw code
/// (e.g. "5S1") which passes through uppercased. The digit(s) are the signal:
///   1 → 5S1 · 2 → 5S2 · 8 → S8W · 10 → S10
pub fn normalize_session_code(s: &str) -> String {
    let lowered = s.to_lowercase();
    // Drop "summer"/"session" words so a raw "5S1" still yields digits "51"
    // (no match → raw passthrough) while "session 10" yields "10".
    let stripped = lowered.replace("summer", "").replace("session", "");
    let digits: String = stripped.chars().filter(|c| c.is_ascii_digit()).collect();
    match digits.as_str() {
        "10" => "S10".to_string(),
        "1" => "5S1".to_string(),
        "2" => "5S2".to_string(),
        "8" => "S8W".to_string(),
        // Already a raw code (e.g. "5S1", "IND", "ED1") — pass through uppercased.
        _ => s.trim().to_uppercase(),
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
        // 5 rowpanel_ panels (3 summer + 2 spring); search-form chrome is skipped.
        assert_eq!(classes.len(), 5, "got: {:#?}", classes);
    }

    #[test]
    fn parse_class_results_summer_session1_all_fields() {
        let classes = parse_class_results(CLASS_RESULTS_FIXTURE);
        let am10 = &classes[0];
        assert_eq!(am10.subject, "AM");
        assert_eq!(am10.catalog_number, "10");
        assert_eq!(am10.section, "01");
        assert_eq!(am10.title, "Lin Algebra for Engrs");
        assert_eq!(am10.class_number, "70307");
        assert_eq!(am10.status, "Open");
        assert_eq!(am10.instructor, "Katznelson,J.R.");
        assert_eq!(am10.location.as_deref(), Some("LEC: Online"));
        assert_eq!(am10.schedule.as_deref(), Some("MTuThF 03:00PM-04:45PM"));
        // The summer-only Session field — the whole point of this work.
        assert_eq!(am10.session.as_deref(), Some("Summer Session 1 (5 Weeks)"));
        assert_eq!(am10.enrolled.as_deref(), Some("87 of 150 Enrolled"));
        assert_eq!(am10.mode.as_deref(), Some("Synchronous Online"));
    }

    #[test]
    fn parse_class_results_session_variety() {
        let classes = parse_class_results(CLASS_RESULTS_FIXTURE);
        let sessions: Vec<Option<&str>> = classes.iter().map(|c| c.session.as_deref()).collect();
        // Distinct summer sessions parsed verbatim from the Session field.
        assert!(sessions.contains(&Some("Summer Session 1 (5 Weeks)")));
        assert!(sessions.contains(&Some("Summer Session 2 (5 Weeks)")));
        assert!(sessions.contains(&Some("Summer Session 8 Weeks")));
        // Regular (spring) panels have no session.
        assert!(sessions.contains(&None));
    }

    #[test]
    fn parse_class_results_closed_in_person_regular_term() {
        let classes = parse_class_results(CLASS_RESULTS_FIXTURE);
        // The two trailing panels are regular-term (Spring) CSE classes.
        let cse = classes
            .iter()
            .find(|c| c.subject == "CSE" && c.catalog_number == "3")
            .expect("CSE 3 panel");
        assert_eq!(cse.status, "Closed");
        assert_eq!(cse.session, None);
        assert_eq!(cse.mode.as_deref(), Some("In Person"));
        assert_eq!(cse.instructor, "Moulds,G.B.");
        assert_eq!(cse.location.as_deref(), Some("LEC: R Carson Acad 240"));
        assert_eq!(cse.schedule.as_deref(), Some("TuTh 03:20PM-04:55PM"));
        assert_eq!(cse.enrolled.as_deref(), Some("84 of 84 Enrolled"));
    }

    #[test]
    fn parse_class_results_status_variety() {
        let classes = parse_class_results(CLASS_RESULTS_FIXTURE);
        let statuses: Vec<&str> = classes.iter().map(|c| c.status.as_str()).collect();
        assert!(statuses.contains(&"Open"));
        assert!(statuses.contains(&"Closed"));
    }

    #[test]
    fn normalize_status_maps_waitlist_and_closed() {
        assert_eq!(normalize_status("Open"), "Open");
        assert_eq!(normalize_status("Closed"), "Closed");
        // "Closed with Wait List" is joinable → "Wait List".
        assert_eq!(normalize_status("Closed with Wait List"), "Wait List");
        assert!(!is_status_alt("UCSC Logo"));
    }

    #[test]
    fn parse_course_header_splits_subject_catalog_section_title() {
        // &nbsp;-normalized header.
        assert_eq!(
            parse_course_header("AM 10 - 01 Lin Algebra for Engrs"),
            ("AM".into(), "10".into(), "01".into(), "Lin Algebra for Engrs".into())
        );
        assert_eq!(
            parse_course_header("CSE 115A - 01A Intro to Software Eng"),
            ("CSE".into(), "115A".into(), "01A".into(), "Intro to Software Eng".into())
        );
    }

    #[test]
    fn normalize_session_code_maps_friendly_and_raw() {
        assert_eq!(normalize_session_code("1"), "5S1");
        assert_eq!(normalize_session_code("session 1"), "5S1");
        assert_eq!(normalize_session_code("2"), "5S2");
        assert_eq!(normalize_session_code("10"), "S10");
        assert_eq!(normalize_session_code("10 week"), "S10");
        assert_eq!(normalize_session_code("8"), "S8W");
        // Raw codes pass through uppercased.
        assert_eq!(normalize_session_code("5S1"), "5S1");
        assert_eq!(normalize_session_code("s8w"), "S8W");
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
