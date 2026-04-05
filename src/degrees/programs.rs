use anyhow::{bail, Result};

use super::scraper::ProgramEntry;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProgramIndex {
    pub bachelors: Vec<ProgramEntry>,
    pub masters: Vec<ProgramEntry>,
}

impl ProgramIndex {
    pub fn all_programs(&self) -> impl Iterator<Item = &ProgramEntry> {
        self.bachelors.iter().chain(self.masters.iter())
    }
}

/// Resolve a user query to a single program entry.
/// Tries: exact slug match, exact name match (case-insensitive), substring match.
pub fn resolve_program<'a>(query: &str, index: &'a ProgramIndex) -> Result<&'a ProgramEntry> {
    let normalized = normalize_query(query);

    // 1. Exact slug match
    if let Some(entry) = index.all_programs().find(|e| e.slug == normalized) {
        return Ok(entry);
    }

    // 2. Exact name match (case-insensitive, dots stripped)
    if let Some(entry) = index
        .all_programs()
        .find(|e| normalize_query(&e.name) == normalized)
    {
        return Ok(entry);
    }

    // 3. Substring match on name
    let matches: Vec<&ProgramEntry> = index
        .all_programs()
        .filter(|e| normalize_query(&e.name).contains(&normalized))
        .collect();

    match matches.len() {
        0 => {
            // 4. Try fuzzy: split query into words and match all
            let words: Vec<&str> = normalized.split_whitespace().collect();
            let fuzzy_matches: Vec<&ProgramEntry> = index
                .all_programs()
                .filter(|e| {
                    let name_normalized = normalize_query(&e.name);
                    words.iter().all(|w| name_normalized.contains(w))
                })
                .collect();

            match fuzzy_matches.len() {
                0 => bail!(
                    "Program '{}' not found. Try searching with keywords like 'Computer Science BS' or 'History BA'.",
                    query
                ),
                1 => Ok(fuzzy_matches[0]),
                _ => {
                    let names: Vec<&str> = fuzzy_matches.iter().map(|e| e.name.as_str()).collect();
                    bail!(
                        "Multiple programs match '{}': {}. Please be more specific.",
                        query,
                        names.join(", ")
                    )
                }
            }
        }
        1 => Ok(matches[0]),
        _ => {
            let names: Vec<&str> = matches.iter().map(|e| e.name.as_str()).collect();
            bail!(
                "Multiple programs match '{}': {}. Please be more specific.",
                query,
                names.join(", ")
            )
        }
    }
}

fn normalize_query(query: &str) -> String {
    query
        .trim()
        .to_lowercase()
        .replace('.', "")
        .replace(',', "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::degrees::scraper::DegreeType;

    fn test_index() -> ProgramIndex {
        ProgramIndex {
            bachelors: vec![
                ProgramEntry {
                    name: "Computer Science B.S.".into(),
                    slug: "computer-science-bs".into(),
                    url_path: "/en/current/general-catalog/academic-units/baskin-engineering/computer-science-and-engineering/computer-science-bs".into(),
                    degree_type: DegreeType::BS,
                },
                ProgramEntry {
                    name: "Computer Science B.A.".into(),
                    slug: "computer-science-ba".into(),
                    url_path: "/en/current/general-catalog/academic-units/baskin-engineering/computer-science-and-engineering/computer-science-ba".into(),
                    degree_type: DegreeType::BA,
                },
                ProgramEntry {
                    name: "History B.A.".into(),
                    slug: "history-ba".into(),
                    url_path: "/en/current/general-catalog/academic-units/humanities-division/history/history-ba".into(),
                    degree_type: DegreeType::BA,
                },
            ],
            masters: vec![
                ProgramEntry {
                    name: "Computer Science and Engineering M.S.".into(),
                    slug: "computer-science-and-engineering-ms".into(),
                    url_path: "/en/current/general-catalog/academic-units/baskin-engineering/computer-science-and-engineering/computer-science-and-engineering-ms".into(),
                    degree_type: DegreeType::MS,
                },
            ],
        }
    }

    #[test]
    fn test_resolve_exact_slug() {
        let index = test_index();
        let result = resolve_program("computer-science-bs", &index).unwrap();
        assert_eq!(result.name, "Computer Science B.S.");
    }

    #[test]
    fn test_resolve_exact_name() {
        let index = test_index();
        let result = resolve_program("Computer Science B.S.", &index).unwrap();
        assert_eq!(result.slug, "computer-science-bs");
    }

    #[test]
    fn test_resolve_name_case_insensitive() {
        let index = test_index();
        let result = resolve_program("computer science bs", &index).unwrap();
        assert_eq!(result.slug, "computer-science-bs");
    }

    #[test]
    fn test_resolve_substring_unique() {
        let index = test_index();
        let result = resolve_program("History", &index).unwrap();
        assert_eq!(result.slug, "history-ba");
    }

    #[test]
    fn test_resolve_ambiguous() {
        let index = test_index();
        let result = resolve_program("Computer Science B", &index);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Multiple programs"));
    }

    #[test]
    fn test_resolve_not_found() {
        let index = test_index();
        let result = resolve_program("Nonexistent Program", &index);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_resolve_fuzzy_words() {
        let index = test_index();
        // "CS MS" should match "Computer Science and Engineering M.S."
        let result = resolve_program("engineering ms", &index).unwrap();
        assert_eq!(result.slug, "computer-science-and-engineering-ms");
    }
}
