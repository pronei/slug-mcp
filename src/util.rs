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
}
