//! UCSC Summer Session academic calendar — session-aware deadlines.
//!
//! Summer has four overlapping sessions (Session 1, Session 2, 8-Week, 10-Week)
//! each with *different* add/drop/withdraw/grade-option deadlines, and — unlike
//! the regular year — there is **no Add by Petition**, so the deadlines are hard
//! stops. This tool surfaces them per session and flags which are still upcoming.
//!
//! Source: <https://summer.ucsc.edu/summer-student-guide/academic-calendar/>
//! (static WordPress HTML: `h2[id]` section → `h3` date range → `ul/li` deadlines).

use std::fmt::Write;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{Datelike, NaiveDate};
use regex::Regex;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::cache::CacheStore;
use crate::util::now_pacific;

const CALENDAR_URL: &str = "https://summer.ucsc.edu/summer-student-guide/academic-calendar/";

/// The four scheduled summer sessions, in display order. `id` matches the
/// page's `<h2 id="…">` anchor; `aliases` are what a user might pass to filter.
const SESSIONS: &[(&str, &str, &[&str])] = &[
    (
        "session-1",
        "Session 1",
        &["1", "s1", "session1", "session 1"],
    ),
    (
        "session-2",
        "Session 2",
        &["2", "s2", "session2", "session 2"],
    ),
    (
        "8-week",
        "8-Week Session",
        &["8", "8week", "8-week", "8 week"],
    ),
    (
        "10-week",
        "10-Week Session",
        &["10", "10week", "10-week", "10 week"],
    ),
];

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SummerDeadlinesRequest {
    /// Filter to one session: "1", "2", "8-week", or "10-week". If omitted,
    /// shows all four sessions.
    pub session: Option<String>,
    /// If true, list only deadlines that haven't passed yet. Default false.
    pub upcoming_only: Option<bool>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SummerSession {
    pub id: String,
    pub name: String,
    /// Raw date-range header, e.g. "June 22 – July 24, 2026".
    pub date_range: String,
    pub deadlines: Vec<Deadline>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Deadline {
    /// e.g. "Add/Swap", "Drop", "Request \"W\" Grade".
    pub label: String,
    /// Human date as printed, e.g. "Thursday, June 25".
    pub date_text: String,
    /// Parsed calendar date when one could be extracted (for upcoming/passed).
    pub date: Option<NaiveDate>,
    /// Parenthetical note, e.g. "tuition reversed".
    pub note: Option<String>,
}

pub struct SummerService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl SummerService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    pub async fn get_deadlines(
        &self,
        session: Option<&str>,
        upcoming_only: bool,
    ) -> Result<String> {
        let http = &self.http;
        let sessions: Vec<SummerSession> = self
            .cache
            .get_or_fetch("summer:deadlines", 86_400, || async move {
                let html = http
                    .get(CALENDAR_URL)
                    .send()
                    .await
                    .context("failed to reach summer.ucsc.edu")?
                    .error_for_status()
                    .context("summer.ucsc.edu returned an error status")?
                    .text()
                    .await
                    .context("failed to read summer calendar page")?;
                Ok(parse_calendar(&html))
            })
            .await?;

        if sessions.is_empty() {
            return Ok(
                "Could not read the summer academic calendar (the page layout may have changed). \
                 See <https://summer.ucsc.edu/summer-student-guide/academic-calendar/>."
                    .to_string(),
            );
        }

        // Resolve a session filter to its canonical id.
        let want_id = session.and_then(resolve_session_id);
        if let Some(req) = session
            && want_id.is_none()
        {
            return Ok(format!(
                "Unknown session '{}'. Valid: 1, 2, 8-week, 10-week.",
                req
            ));
        }

        Ok(format_deadlines(
            &sessions,
            want_id.as_deref(),
            upcoming_only,
        ))
    }
}

/// Map a user-supplied session string to the canonical section id.
fn resolve_session_id(input: &str) -> Option<String> {
    let q = input.trim().to_lowercase();
    SESSIONS
        .iter()
        .find(|(id, _, aliases)| *id == q || aliases.contains(&q.as_str()))
        .map(|(id, _, _)| id.to_string())
}

fn parse_calendar(html: &str) -> Vec<SummerSession> {
    let mut out = Vec::new();
    for (id, name, _) in SESSIONS {
        // Slice from this section's <h2 id="…"> to the next <h2 (any) or EOF.
        let Some(start) = html.find(&format!("id=\"{id}\"")) else {
            continue;
        };
        let after = &html[start..];
        let end = after[1..].find("<h2").map(|i| i + 1).unwrap_or(after.len());
        let block = &after[..end];

        // First <h3>…</h3> is the date range.
        let date_range = capture(block, &H3_RE)
            .map(|s| clean(&s))
            .unwrap_or_default();
        let year = YEAR_RE
            .captures(&date_range)
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().parse::<i32>().ok());

        let deadlines = LI_RE
            .captures_iter(block)
            .filter_map(|c| c.get(1))
            .map(|m| parse_deadline(&clean(m.as_str()), year))
            .collect::<Vec<_>>();

        out.push(SummerSession {
            id: id.to_string(),
            name: name.to_string(),
            date_range,
            deadlines,
        });
    }
    out
}

/// Split a deadline `<li>` like `Add/Swap – Thursday, June 25 (tuition reversed)`
/// into (label, date_text, parsed date, note).
fn parse_deadline(text: &str, year: Option<i32>) -> Deadline {
    // Label is everything before the first en-/em-/hyphen dash separator.
    let (label, rest) = match text.split_once('–').or_else(|| text.split_once(" - ")) {
        Some((l, r)) => (l.trim().to_string(), r.trim().to_string()),
        None => (text.to_string(), String::new()),
    };

    // Pull a parenthetical note out of the remainder.
    let (rest_no_note, note) = match (rest.find('('), rest.find(')')) {
        (Some(a), Some(b)) if b > a => {
            let note = rest[a + 1..b].trim().to_string();
            let cleaned = format!("{}{}", &rest[..a], &rest[b + 1..]);
            (cleaned.trim().to_string(), Some(note))
        }
        _ => (rest.clone(), None),
    };

    let date_text = if rest_no_note.is_empty() {
        label.clone()
    } else {
        rest_no_note.clone()
    };

    // Parse "Month Day" + year → NaiveDate.
    let date = MONTH_DAY_RE.captures(&date_text).and_then(|c| {
        let month = c.get(1)?.as_str();
        let day: u32 = c.get(2)?.as_str().parse().ok()?;
        let y = year.unwrap_or_else(|| now_pacific().year());
        let mnum = month_number(month)?;
        NaiveDate::from_ymd_opt(y, mnum, day)
    });

    Deadline {
        label,
        date_text,
        date,
        note,
    }
}

fn format_deadlines(
    sessions: &[SummerSession],
    want_id: Option<&str>,
    upcoming_only: bool,
) -> String {
    let today = now_pacific().date_naive();
    let year = sessions
        .iter()
        .find_map(|s| YEAR_RE.captures(&s.date_range))
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_default();

    let mut out = format!("# UCSC Summer {year} — Academic Deadlines\n\n");
    out.push_str(
        "_Summer has no Add by Petition — these are hard deadlines, and each session has its own._\n",
    );

    for s in sessions
        .iter()
        .filter(|s| want_id.is_none_or(|w| s.id == w))
    {
        let _ = write!(out, "\n## {} — {}\n", s.name, s.date_range);
        let mut shown = 0;
        for d in &s.deadlines {
            let upcoming = d.date.map(|dt| dt >= today);
            if upcoming_only && upcoming == Some(false) {
                continue;
            }
            shown += 1;
            // Marker: ⏰ upcoming, ✅ passed, • undated/info.
            let marker = match upcoming {
                Some(true) => "⏰",
                Some(false) => "✅",
                None => "•",
            };
            // Info items (no "Label – Date" split) render as a plain bullet
            // instead of duplicating the text as both label and value.
            if d.label == d.date_text {
                let _ = write!(out, "- {} {}", marker, d.date_text);
            } else {
                let _ = write!(out, "- {} **{}**: {}", marker, d.label, d.date_text);
            }
            if let Some(note) = &d.note {
                let _ = write!(out, " _({note})_");
            }
            if let (Some(dt), Some(true)) = (d.date, upcoming) {
                let days = (dt - today).num_days();
                let _ = match days {
                    0 => write!(out, " — **today**"),
                    1 => write!(out, " — tomorrow"),
                    n => write!(out, " — in {n} days"),
                };
            }
            out.push('\n');
        }
        if shown == 0 {
            out.push_str("- _No upcoming deadlines._\n");
        }
    }

    out.push_str(&format!(
        "\n_Source: summer.ucsc.edu academic calendar. Last updated: {}_\n",
        now_pacific().format("%-I:%M %p")
    ));
    out
}

// ─── helpers ───

fn capture(haystack: &str, re: &Regex) -> Option<String> {
    re.captures(haystack)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// Strip tags and decode the handful of HTML entities WordPress emits here.
fn clean(s: &str) -> String {
    let text = crate::util::strip_html_tags(s);
    text.replace("&#8211;", "–")
        .replace("&#8212;", "—")
        .replace("&#8220;", "\"")
        .replace("&#8221;", "\"")
        .replace("&#8217;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn month_number(name: &str) -> Option<u32> {
    Some(match name {
        "January" => 1,
        "February" => 2,
        "March" => 3,
        "April" => 4,
        "May" => 5,
        "June" => 6,
        "July" => 7,
        "August" => 8,
        "September" => 9,
        "October" => 10,
        "November" => 11,
        "December" => 12,
        _ => return None,
    })
}

use std::sync::LazyLock;
static H3_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<h3[^>]*>(.*?)</h3>").unwrap());
static LI_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<li[^>]*>(.*?)</li>").unwrap());
static YEAR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(20\d{2})").unwrap());
static MONTH_DAY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(January|February|March|April|May|June|July|August|September|October|November|December)\s+(\d{1,2})").unwrap()
});

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("fixtures/academic_calendar.html");

    #[test]
    fn parses_all_four_sessions() {
        let s = parse_calendar(FIXTURE);
        assert_eq!(s.len(), 4);
        assert_eq!(s[0].id, "session-1");
        assert_eq!(s[0].name, "Session 1");
        assert!(s[0].date_range.contains("June 22"));
        assert!(s[0].date_range.contains("2026"));
        assert_eq!(s[2].name, "8-Week Session");
        assert_eq!(s[3].name, "10-Week Session");
    }

    #[test]
    fn parses_deadline_label_date_and_note() {
        let s = parse_calendar(FIXTURE);
        let s1 = &s[0];
        let add = s1
            .deadlines
            .iter()
            .find(|d| d.label.starts_with("Add"))
            .expect("Add/Swap deadline");
        assert!(add.date_text.contains("June 25"));
        assert_eq!(add.date, NaiveDate::from_ymd_opt(2026, 6, 25));

        let drop = s1
            .deadlines
            .iter()
            .find(|d| d.label == "Drop")
            .expect("Drop deadline");
        assert_eq!(drop.note.as_deref(), Some("tuition reversed"));
        assert_eq!(drop.date, NaiveDate::from_ymd_opt(2026, 6, 29));
    }

    #[test]
    fn session_alias_resolution() {
        assert_eq!(resolve_session_id("1").as_deref(), Some("session-1"));
        assert_eq!(
            resolve_session_id("Session 2").as_deref(),
            Some("session-2")
        );
        assert_eq!(resolve_session_id("10week").as_deref(), Some("10-week"));
        assert_eq!(resolve_session_id("8-Week").as_deref(), Some("8-week"));
        assert_eq!(resolve_session_id("bogus"), None);
    }

    #[test]
    fn format_filters_by_session_and_marks_upcoming() {
        let s = parse_calendar(FIXTURE);
        let out = format_deadlines(&s, Some("session-1"), false);
        assert!(out.contains("## Session 1"));
        assert!(!out.contains("## Session 2")); // filtered out
        assert!(out.contains("Add/Swap"));
        // All 2026 summer dates are in the past relative to "now" in tests run
        // later, but the marker logic must still render (✅ or ⏰).
        assert!(out.contains("Drop"));
        assert!(out.contains("tuition reversed"));
    }

    // ── error paths ──

    #[test]
    fn parse_calendar_maintenance_page_yields_empty() {
        assert!(parse_calendar("<html><body>Maintenance</body></html>").is_empty());
        assert!(parse_calendar("").is_empty());
    }

    #[test]
    fn parse_calendar_renamed_section_ids_yield_empty() {
        // Structure drift: WordPress anchors renamed → no sessions, no panic.
        let html = FIXTURE
            .replace("id=\"session-", "id=\"term-")
            .replace("id=\"8-week\"", "id=\"eight-week\"")
            .replace("id=\"10-week\"", "id=\"ten-week\"");
        assert!(parse_calendar(&html).is_empty());
    }

    #[test]
    fn parse_calendar_section_without_h3_still_parses_deadlines() {
        // Date-range header missing → year falls back to the current Pacific
        // year; month/day still extracted from each <li>.
        let html = r#"<h2 id="session-1">Session 1</h2>
            <ul><li>Add/Swap &#8211; Thursday, June 25</li></ul>"#;
        let sessions = parse_calendar(html);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].date_range, "");
        let d = &sessions[0].deadlines[0];
        assert_eq!(d.label, "Add/Swap");
        let date = d.date.expect("date parsed with fallback year");
        assert_eq!((date.month(), date.day()), (6, 25));
    }

    #[test]
    fn parse_deadline_unclosed_parenthesis_no_note() {
        let d = parse_deadline("Drop – Monday, June 29 (tuition reversed", Some(2026));
        assert_eq!(d.label, "Drop");
        assert_eq!(d.note, None);
        assert_eq!(d.date, NaiveDate::from_ymd_opt(2026, 6, 29));
    }

    #[test]
    fn parse_deadline_undated_info_line() {
        let d = parse_deadline(
            "Final examinations are scheduled by instructors",
            Some(2026),
        );
        assert_eq!(d.date, None);
        assert_eq!(d.label, d.date_text);
        // Undated lines render as plain bullets with the "•" marker.
        let s = SummerSession {
            id: "session-1".into(),
            name: "Session 1".into(),
            date_range: "June 22 – July 24, 2026".into(),
            deadlines: vec![d],
        };
        let out = format_deadlines(&[s], None, false);
        assert!(out.contains("- • Final examinations"));
    }

    #[test]
    fn parse_calendar_multibyte_between_sections_no_panic() {
        // Multibyte chars flush against the sliced boundaries (h2 lookups and
        // block ends) must not split char boundaries.
        let html = "🎓émoji <h2 id=\"session-1\">Session 1 ☀️</h2>\
                    <h3>June 22 \u{2013} July 24, 2026</h3>\
                    <ul><li>Drop \u{2013} Montag, Juni 29 (Studiengebühr erstattet)</li>\
                    <li>Add/Swap \u{2013} Thursday, June 25 🗓</li></ul>\
                    <h2 id=\"session-2\">🌙</h2><ul><li>Drop – June 30</li></ul>";
        let sessions = parse_calendar(html);
        assert_eq!(sessions.len(), 2);
        let s1 = &sessions[0];
        assert_eq!(s1.deadlines.len(), 2);
        // German month name doesn't match MONTH_DAY_RE → date None, note intact.
        assert_eq!(s1.deadlines[0].date, None);
        assert_eq!(
            s1.deadlines[0].note.as_deref(),
            Some("Studiengebühr erstattet")
        );
        assert_eq!(s1.deadlines[1].date, NaiveDate::from_ymd_opt(2026, 6, 25));
    }

    #[test]
    fn format_zero_deadline_session_notes_nothing_upcoming() {
        let s = SummerSession {
            id: "8-week".into(),
            name: "8-Week Session".into(),
            date_range: "June 22 – August 14, 2026".into(),
            deadlines: vec![],
        };
        let out = format_deadlines(&[s], None, false);
        assert!(out.contains("## 8-Week Session"));
        assert!(out.contains("_No upcoming deadlines._"));
    }
}
