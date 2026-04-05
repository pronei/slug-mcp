use std::fmt;

use anyhow::{Context, Result};

const BREADTH_CSV_URL: &str = "https://docs.google.com/spreadsheets/d/e/2PACX-1vRe3tr6pCCrMOsFuo18NVXhARefyN4btXtTBJqRm60PK_JRtgQFirsRsiMFKbVdMxaikaerxMy8JfCj/pub?gid=0&single=true&output=csv";

/// Rule: 3 courses from 3 different breadth categories (15 credits total).
pub const BREADTH_COURSES_REQUIRED: usize = 3;

// ─── Data Model ───

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BreadthRequirements {
    pub categories: Vec<BreadthCategory>,
    pub not_allowed: Vec<BreadthCourse>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BreadthCategory {
    pub name: String,
    pub courses: Vec<BreadthCourse>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BreadthCourse {
    pub code: String,
    pub title: String,
}

// ─── Scraping ───

pub async fn scrape_breadth_requirements(client: &reqwest::Client) -> Result<BreadthRequirements> {
    let resp = client
        .get(BREADTH_CSV_URL)
        .send()
        .await
        .context("Failed to fetch breadth requirements CSV")?;

    let csv_text = resp
        .text()
        .await
        .context("Failed to read breadth requirements CSV")?;

    parse_breadth_csv(&csv_text)
}

fn parse_breadth_csv(csv: &str) -> Result<BreadthRequirements> {
    let mut categories: Vec<BreadthCategory> = Vec::new();
    let mut not_allowed: Vec<BreadthCourse> = Vec::new();
    let mut current_category: Option<BreadthCategory> = None;
    let mut in_not_allowed = false;
    let mut past_title = false;

    for line in csv.lines() {
        let line = line.trim().trim_end_matches(',').trim_matches('"').trim();

        // Skip the title line
        if !past_title {
            if line.contains("Breadth Categories") || line.contains("breadth categories") {
                past_title = true;
            }
            // Also handle case where first non-empty line is just a header
            if !line.is_empty() && !past_title {
                past_title = true;
                // Check if this line itself is a category header
                if !is_course_line(line) && !line.is_empty() {
                    if line.to_uppercase().contains("NOT ALLOWED") {
                        in_not_allowed = true;
                    } else {
                        current_category = Some(BreadthCategory {
                            name: line.to_string(),
                            courses: Vec::new(),
                        });
                    }
                }
            }
            continue;
        }

        // Empty line = category boundary
        if line.is_empty() {
            if let Some(cat) = current_category.take() {
                if !cat.courses.is_empty() {
                    categories.push(cat);
                }
            }
            continue;
        }

        // Check for "NOT ALLOWED" section
        if line.to_uppercase().contains("NOT ALLOWED") {
            if let Some(cat) = current_category.take() {
                if !cat.courses.is_empty() {
                    categories.push(cat);
                }
            }
            in_not_allowed = true;
            continue;
        }

        // Try to parse as a course line (e.g., "CSE 242: Machine Learning")
        if let Some(course) = parse_course_line(line) {
            if in_not_allowed {
                not_allowed.push(course);
            } else {
                if current_category.is_none() {
                    // Shouldn't happen, but handle gracefully
                    current_category = Some(BreadthCategory {
                        name: "Unknown".to_string(),
                        courses: Vec::new(),
                    });
                }
                if let Some(ref mut cat) = current_category {
                    cat.courses.push(course);
                }
            }
        } else {
            // Non-course, non-empty line = category header
            if let Some(cat) = current_category.take() {
                if !cat.courses.is_empty() {
                    categories.push(cat);
                }
            }
            if !in_not_allowed {
                current_category = Some(BreadthCategory {
                    name: line.to_string(),
                    courses: Vec::new(),
                });
            }
        }
    }

    // Flush remaining category
    if let Some(cat) = current_category.take() {
        if !cat.courses.is_empty() {
            categories.push(cat);
        }
    }

    Ok(BreadthRequirements {
        categories,
        not_allowed,
    })
}

fn is_course_line(line: &str) -> bool {
    // Course lines match pattern: DEPT NNN[A-Z]?: Title
    // e.g., "CSE 242: Machine Learning" or "STAT 203: Introduction to Probability"
    let parts: Vec<&str> = line.splitn(2, ':').collect();
    if parts.len() < 2 {
        return false;
    }
    let code_part = parts[0].trim();
    let words: Vec<&str> = code_part.split_whitespace().collect();
    if words.len() != 2 {
        return false;
    }
    // First word should be all uppercase letters (department)
    let dept = words[0];
    let num = words[1];
    dept.chars().all(|c| c.is_ascii_uppercase())
        && num.chars().next().is_some_and(|c| c.is_ascii_digit())
}

fn parse_course_line(line: &str) -> Option<BreadthCourse> {
    let parts: Vec<&str> = line.splitn(2, ':').collect();
    if parts.len() < 2 {
        return None;
    }
    let code = parts[0].trim().to_string();
    let title = parts[1].trim().to_string();

    // Validate it looks like a course code
    let words: Vec<&str> = code.split_whitespace().collect();
    if words.len() != 2 {
        return None;
    }
    let dept = words[0];
    let num = words[1];
    if !dept.chars().all(|c| c.is_ascii_uppercase()) {
        return None;
    }
    if !num.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }

    Some(BreadthCourse { code, title })
}

// ─── Progress Checking ───

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BreadthProgress {
    pub categories: Vec<CategoryProgress>,
    pub satisfied: bool,
    pub categories_covered: usize,
    pub courses_matched: usize,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CategoryProgress {
    pub name: String,
    pub completed_courses: Vec<String>,
    pub available_courses: Vec<String>,
}

pub fn check_breadth_progress(
    reqs: &BreadthRequirements,
    completed_courses: &[String],
) -> BreadthProgress {
    use std::collections::HashSet;
    use super::progress::normalize_course_code;

    let completed_set: HashSet<String> = completed_courses
        .iter()
        .map(|c| normalize_course_code(c))
        .collect();

    let categories: Vec<CategoryProgress> = reqs
        .categories
        .iter()
        .map(|cat| {
            let completed: Vec<String> = cat
                .courses
                .iter()
                .filter(|c| completed_set.contains(&normalize_course_code(&c.code)))
                .map(|c| c.code.clone())
                .collect();

            let available: Vec<String> = cat
                .courses
                .iter()
                .filter(|c| !completed_set.contains(&normalize_course_code(&c.code)))
                .map(|c| c.code.clone())
                .collect();

            CategoryProgress {
                name: cat.name.clone(),
                completed_courses: completed,
                available_courses: available,
            }
        })
        .collect();

    let categories_covered = categories
        .iter()
        .filter(|c| !c.completed_courses.is_empty())
        .count();

    let courses_matched: usize = categories
        .iter()
        .map(|c| c.completed_courses.len().min(1)) // only count 1 per category
        .sum();

    BreadthProgress {
        satisfied: categories_covered >= BREADTH_COURSES_REQUIRED,
        categories_covered,
        courses_matched,
        categories,
    }
}

// ─── Display ───

impl fmt::Display for BreadthRequirements {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "## CSE M.S. Breadth Requirements\n")?;
        writeln!(
            f,
            "> Select one course from {} different breadth categories ({} courses, 15 credits total).\n",
            BREADTH_COURSES_REQUIRED, BREADTH_COURSES_REQUIRED
        )?;

        for cat in &self.categories {
            writeln!(f, "### {}\n", cat.name)?;
            for course in &cat.courses {
                writeln!(f, "- {} — {}", course.code, course.title)?;
            }
            writeln!(f)?;
        }

        if !self.not_allowed.is_empty() {
            writeln!(f, "### Courses NOT Allowed as Breadth\n")?;
            for course in &self.not_allowed {
                writeln!(f, "- {} — {}", course.code, course.title)?;
            }
        }

        Ok(())
    }
}

impl fmt::Display for BreadthProgress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status = if self.satisfied {
            "SATISFIED"
        } else {
            "IN PROGRESS"
        };
        writeln!(
            f,
            "## Breadth Requirements [{status}] ({}/{} categories covered)\n",
            self.categories_covered, BREADTH_COURSES_REQUIRED
        )?;

        for cat in &self.categories {
            if !cat.completed_courses.is_empty() {
                writeln!(f, "**{}** [COVERED]", cat.name)?;
                for course in &cat.completed_courses {
                    writeln!(f, "- [x] {}", course)?;
                }
                writeln!(f)?;
            }
        }

        // Show uncovered categories with available courses
        let uncovered: Vec<&CategoryProgress> = self
            .categories
            .iter()
            .filter(|c| c.completed_courses.is_empty())
            .collect();

        if !uncovered.is_empty() && !self.satisfied {
            writeln!(f, "**Uncovered categories (choose from):**\n")?;
            for cat in uncovered {
                writeln!(f, "- {} ({} courses available)", cat.name, cat.available_courses.len())?;
            }
            writeln!(f)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CSV: &str = r#"CSE MS: Breadth Categories
,
Computer Architecture/Networks,
CSE 202: Computer Architecture,
CSE 222A: VLSI Digital System Design,
,
Machine Learning/Artificial Intelligence,
CSE 242: Machine Learning,
CSE 244A: Foundations of Machine Learning,
CSE 250A: Applied Machine Learning,
,
Theoretical Computer Science,
CSE 200S: Automata Theory,
CSE 201: Analysis of Algorithms,
,
Courses NOT ALLOWED as Breadth,
CSE 200: Research and Teaching,
CSE 280S: Seminar,
"#;

    #[test]
    fn test_parse_breadth_csv() {
        let reqs = parse_breadth_csv(TEST_CSV).unwrap();
        assert_eq!(reqs.categories.len(), 3);
        assert_eq!(reqs.categories[0].name, "Computer Architecture/Networks");
        assert_eq!(reqs.categories[0].courses.len(), 2);
        assert_eq!(reqs.categories[0].courses[0].code, "CSE 202");
        assert_eq!(reqs.categories[1].name, "Machine Learning/Artificial Intelligence");
        assert_eq!(reqs.categories[1].courses.len(), 3);
        assert_eq!(reqs.categories[2].name, "Theoretical Computer Science");
        assert_eq!(reqs.categories[2].courses.len(), 2);
        assert_eq!(reqs.not_allowed.len(), 2);
        assert_eq!(reqs.not_allowed[0].code, "CSE 200");
    }

    #[test]
    fn test_breadth_progress_satisfied() {
        let reqs = parse_breadth_csv(TEST_CSV).unwrap();
        let completed = vec![
            "CSE 202".to_string(),  // Architecture
            "CSE 242".to_string(),  // ML/AI
            "CSE 201".to_string(),  // Theory
        ];
        let progress = check_breadth_progress(&reqs, &completed);
        assert!(progress.satisfied);
        assert_eq!(progress.categories_covered, 3);
    }

    #[test]
    fn test_breadth_progress_not_satisfied() {
        let reqs = parse_breadth_csv(TEST_CSV).unwrap();
        let completed = vec![
            "CSE 202".to_string(),  // Architecture
            "CSE 242".to_string(),  // ML/AI
            // Missing third category
        ];
        let progress = check_breadth_progress(&reqs, &completed);
        assert!(!progress.satisfied);
        assert_eq!(progress.categories_covered, 2);
    }

    #[test]
    fn test_two_courses_same_category_counts_as_one() {
        let reqs = parse_breadth_csv(TEST_CSV).unwrap();
        let completed = vec![
            "CSE 242".to_string(),  // ML/AI
            "CSE 250A".to_string(), // ML/AI (same category)
            "CSE 202".to_string(),  // Architecture
        ];
        let progress = check_breadth_progress(&reqs, &completed);
        assert!(!progress.satisfied); // Only 2 categories covered, not 3
        assert_eq!(progress.categories_covered, 2);
    }

    #[test]
    fn test_parse_course_line() {
        let c = parse_course_line("CSE 242: Machine Learning").unwrap();
        assert_eq!(c.code, "CSE 242");
        assert_eq!(c.title, "Machine Learning");

        let c = parse_course_line("STAT 203: Introduction to Probability").unwrap();
        assert_eq!(c.code, "STAT 203");

        assert!(parse_course_line("Not a course line").is_none());
        assert!(parse_course_line("").is_none());
    }
}
