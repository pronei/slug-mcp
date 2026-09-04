use chrono::DateTime;
use chrono_tz::Tz;

mod fuzzy;
pub use fuzzy::FuzzyMatcher;

/// Declare memoized CSS selector statics. The macro owns both the name and the
/// CSS string, so the two can't drift apart.
///
/// ```ignore
/// selectors! { SEL_TITLE => "h2.title", SEL_LINK => "a.link" }
/// // usage: document.select(&SEL_TITLE)
/// ```
macro_rules! selectors {
    ($($name:ident => $css:literal),+ $(,)?) => {
        $(
            static $name: std::sync::LazyLock<scraper::Selector> = std::sync::LazyLock::new(|| {
                scraper::Selector::parse($css).expect("hardcoded selector")
            });
        )+
    };
}
pub(crate) use selectors;

/// Current time in America/Los_Angeles. Use this anywhere a user-perceived
/// "now" is needed (display, calendar comparisons, day-of-week math) so the
/// output doesn't depend on the host's TZ.
pub fn now_pacific() -> DateTime<Tz> {
    chrono::Utc::now().with_timezone(&chrono_tz::US::Pacific)
}

/// Strip HTML tags from a string, collapsing whitespace.
pub fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(c),
            _ => {}
        }
    }
    result.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncate a string to `max_chars` characters (not bytes), appending "..." if truncated.
/// Safe on all UTF-8 input.
pub fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let t: String = s.chars().take(max_chars).collect();
        format!("{}...", t)
    }
}

/// Convert a compass bearing in degrees (0-360, wrapping) to a 16-point cardinal
/// direction string (e.g. 45° -> "NE"). Used by marine/buoy/tides modules.
pub fn degrees_to_compass(deg: f64) -> &'static str {
    let d = ((deg % 360.0) + 360.0) % 360.0;
    if !(11.25..348.75).contains(&d) {
        return "N";
    }
    match d {
        d if d < 33.75 => "NNE",
        d if d < 56.25 => "NE",
        d if d < 78.75 => "ENE",
        d if d < 101.25 => "E",
        d if d < 123.75 => "ESE",
        d if d < 146.25 => "SE",
        d if d < 168.75 => "SSE",
        d if d < 191.25 => "S",
        d if d < 213.75 => "SSW",
        d if d < 236.25 => "SW",
        d if d < 258.75 => "WSW",
        d if d < 281.25 => "W",
        d if d < 303.75 => "WNW",
        d if d < 326.25 => "NW",
        _ => "NNW",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_html_tags() {
        assert_eq!(strip_html_tags("<p>Hello <b>world</b></p>"), "Hello world");
        assert_eq!(strip_html_tags("no tags"), "no tags");
        assert_eq!(strip_html_tags("<script>bad</script>ok"), "badok");
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("hello world", 5), "hello...");
        // UTF-8 safe: doesn't panic on multi-byte chars
        assert_eq!(truncate("café résumé", 4), "café...");
    }

    #[test]
    fn test_now_pacific_is_in_pacific() {
        let now = now_pacific();
        // Pacific tz name surfaces as PST/PDT depending on DST
        let tz_str = now.format("%Z").to_string();
        assert!(tz_str == "PST" || tz_str == "PDT", "got: {}", tz_str);
    }

    #[test]
    fn test_degrees_to_compass() {
        assert_eq!(degrees_to_compass(0.0), "N");
        assert_eq!(degrees_to_compass(90.0), "E");
        assert_eq!(degrees_to_compass(180.0), "S");
        assert_eq!(degrees_to_compass(270.0), "W");
        assert_eq!(degrees_to_compass(45.0), "NE");
        assert_eq!(degrees_to_compass(225.0), "SW");
        // wrap-around
        assert_eq!(degrees_to_compass(360.0), "N");
        assert_eq!(degrees_to_compass(-90.0), "W");
        assert_eq!(degrees_to_compass(720.0 + 180.0), "S");
    }
}

/// Refuse to treat a zero-length scrape of a *catalog* (classrooms, summer
/// calendar) as a real answer: an empty catalog means the page changed or an
/// error page was parsed, and caching it would pin "no results" for the TTL.
pub fn reject_empty_catalog<T>(items: Vec<T>, what: &str) -> anyhow::Result<Vec<T>> {
    if items.is_empty() {
        anyhow::bail!("{what}: page parsed to zero entries (layout change or error page?)");
    }
    Ok(items)
}

/// Strict `YYYY-MM-DD` parse for user-supplied dates.
pub fn parse_iso_date(s: &str) -> Option<chrono::NaiveDate> {
    let s = s.trim();
    if s.len() != 10 {
        return None;
    }
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

#[cfg(test)]
mod audit_tests {
    use super::*;

    #[test]
    fn empty_catalog_is_an_error_not_a_result() {
        assert!(reject_empty_catalog(Vec::<u8>::new(), "classrooms").is_err());
        assert_eq!(
            reject_empty_catalog(vec![1u8], "classrooms").unwrap(),
            vec![1u8]
        );
    }

    #[test]
    fn iso_date_parse_is_strict() {
        assert!(parse_iso_date("2026-09-03").is_some());
        assert!(parse_iso_date("tomorrow").is_none());
        assert!(parse_iso_date("04/10/2026").is_none());
        assert!(parse_iso_date("2026-9-3").is_none());
    }
}

/// A failure caused by the caller's arguments rather than by this server or an
/// upstream. `server.rs` surfaces these as tool-level errors (`isError: true`)
/// so the explanation reaches the caller — most MCP clients render protocol
/// errors opaquely, which loses the message.
#[derive(Debug)]
pub struct InvalidInput(pub String);

impl InvalidInput {
    pub fn err(msg: impl Into<String>) -> anyhow::Error {
        anyhow::Error::new(InvalidInput(msg.into()))
    }
}

impl std::fmt::Display for InvalidInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InvalidInput {}
