use std::sync::OnceLock;

use scraper::Selector;

/// Parse and memoize a CSS selector. Panics on invalid selectors (which are
/// hardcoded constants, so this is fine).
pub fn sel<'a>(cell: &'a OnceLock<Selector>, css: &str) -> &'a Selector {
    cell.get_or_init(|| Selector::parse(css).expect("hardcoded selector"))
}

/// Declare memoized CSS selector statics. Reduces boilerplate in scraper modules.
///
/// ```ignore
/// selectors! { SEL_TITLE => "h2.title", SEL_LINK => "a.link" }
/// ```
macro_rules! selectors {
    ($($name:ident => $css:expr),+ $(,)?) => {
        $(static $name: std::sync::OnceLock<scraper::Selector> = std::sync::OnceLock::new();)+
    };
}
pub(crate) use selectors;

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
    fn test_sel_memoization() {
        static TEST_SEL: OnceLock<Selector> = OnceLock::new();
        let s1 = sel(&TEST_SEL, "div.test");
        let s2 = sel(&TEST_SEL, "div.test");
        assert!(std::ptr::eq(s1, s2)); // same pointer = memoized
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
