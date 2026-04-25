/// Builder-style matcher for substring lookups across multiple needles.
///
/// Replaces ad-hoc denylists, multi-pass `find_X` helpers, and case-folded
/// `contains()` chains scattered across scrapers. The matcher itself is cheap
/// to construct — it owns only references to the needles you pass in.
///
/// ```ignore
/// // Filter dining categories
/// let m = FuzzyMatcher::new(["condiments", "beverages"])
///     .case_insensitive()
///     .whitespace_collapsed();
/// assert!(m.matches("Beverages "));
/// assert!(!m.matches("Entrees"));
///
/// // Disambiguating a heading against multiple labels
/// let m = FuzzyMatcher::new(["planners", "courses"]).case_insensitive();
/// assert_eq!(m.matches_unambiguously("Course Planners"), None);
/// ```
pub struct FuzzyMatcher<'a> {
    needles: Vec<&'a str>,
    case_insensitive: bool,
    whitespace_collapsed: bool,
    word_boundary: bool,
}

impl<'a> FuzzyMatcher<'a> {
    pub fn new<I>(needles: I) -> Self
    where
        I: IntoIterator<Item = &'a str>,
    {
        Self {
            needles: needles.into_iter().collect(),
            case_insensitive: false,
            whitespace_collapsed: false,
            word_boundary: false,
        }
    }

    /// Compare in lowercase. ASCII-only, so don't rely on this for non-ASCII text.
    pub fn case_insensitive(mut self) -> Self {
        self.case_insensitive = true;
        self
    }

    /// Collapse runs of whitespace to a single space before matching.
    pub fn whitespace_collapsed(mut self) -> Self {
        self.whitespace_collapsed = true;
        self
    }

    /// Require matches to land at word boundaries (start/end of string or non-word
    /// character). Prevents `"plan"` matching `"planet"`. Currently unused at
    /// call sites — kept as part of the matcher's public API.
    #[allow(dead_code)]
    pub fn word_boundary(mut self) -> Self {
        self.word_boundary = true;
        self
    }

    /// True if any needle matches the haystack.
    pub fn matches(&self, haystack: &str) -> bool {
        let h = self.normalize(haystack);
        self.needles
            .iter()
            .any(|n| self.matches_one(&h, &self.normalize(n)))
    }

    /// Returns the unique matching needle, or `None` if zero or multiple match.
    /// Use this when you want to skip on ambiguity.
    pub fn matches_unambiguously(&self, haystack: &str) -> Option<&'a str> {
        let h = self.normalize(haystack);
        let mut found: Option<&'a str> = None;
        for &needle in &self.needles {
            if self.matches_one(&h, &self.normalize(needle)) {
                if found.is_some() {
                    return None;
                }
                found = Some(needle);
            }
        }
        found
    }

    fn normalize(&self, s: &str) -> String {
        let s = if self.case_insensitive {
            s.to_lowercase()
        } else {
            s.to_string()
        };
        if self.whitespace_collapsed {
            s.split_whitespace().collect::<Vec<_>>().join(" ")
        } else {
            s
        }
    }

    fn matches_one(&self, haystack: &str, needle: &str) -> bool {
        if needle.is_empty() {
            return false;
        }
        if !self.word_boundary {
            return haystack.contains(needle);
        }
        // Word-boundary scan: needle must abut start/end or a non-word char on both sides.
        let bytes = haystack.as_bytes();
        let n_len = needle.len();
        let mut start = 0;
        while let Some(idx) = haystack[start..].find(needle) {
            let abs_start = start + idx;
            let abs_end = abs_start + n_len;
            let before_ok = abs_start == 0 || !is_word_byte(bytes[abs_start - 1]);
            let after_ok = abs_end == bytes.len() || !is_word_byte(bytes[abs_end]);
            if before_ok && after_ok {
                return true;
            }
            start = abs_start + 1;
        }
        false
    }
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substring_match() {
        let m = FuzzyMatcher::new(["foo", "bar"]);
        assert!(m.matches("foobar"));
        assert!(m.matches("hello bar world"));
        assert!(!m.matches("baz"));
    }

    #[test]
    fn case_insensitive_matches_uppercase() {
        let m = FuzzyMatcher::new(["condiments"]).case_insensitive();
        assert!(m.matches("Condiments"));
        assert!(m.matches("CONDIMENTS"));
        assert!(!m.matches("Entrees"));
    }

    #[test]
    fn whitespace_collapsed_handles_padding() {
        let m = FuzzyMatcher::new(["bread and bagels"])
            .case_insensitive()
            .whitespace_collapsed();
        assert!(m.matches("  bread   and  bagels "));
    }

    #[test]
    fn word_boundary_rejects_partial() {
        let m = FuzzyMatcher::new(["plan"])
            .case_insensitive()
            .word_boundary();
        assert!(m.matches("plan"));
        assert!(m.matches("a plan B"));
        assert!(!m.matches("planet"));
        assert!(!m.matches("explain"));
    }

    #[test]
    fn unambiguous_returns_unique_match() {
        let m = FuzzyMatcher::new(["planner", "course"]).case_insensitive();
        assert_eq!(m.matches_unambiguously("Planners"), Some("planner"));
        assert_eq!(m.matches_unambiguously("Required Courses"), Some("course"));
        // "Course Planners" contains both "course" and "planner" — ambiguous.
        assert_eq!(m.matches_unambiguously("Course Planners"), None);
        assert_eq!(m.matches_unambiguously("nothing"), None);
    }

    #[test]
    fn empty_needle_never_matches() {
        let m = FuzzyMatcher::new([""]);
        assert!(!m.matches("anything"));
        assert!(!m.matches(""));
    }
}
