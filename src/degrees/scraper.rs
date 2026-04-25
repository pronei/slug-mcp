use std::fmt::Write;

use anyhow::{Context, Result};
use scraper::{ElementRef, Html};

use crate::util::{selectors, FuzzyMatcher};

selectors! {
    SEL_REQ_CONTAINER => "div#degree-req-2",
    SEL_MAIN => "div#main",
    SEL_TR => "tr",
    SEL_COURSE_NUM => "td.sc-coursenumber",
    SEL_COURSE_LINK => "a.sc-courselink",
    SEL_COURSE_TITLE => "td.sc-coursetitle",
    SEL_CREDITS => "p.credits",
    SEL_CROSSLISTED => "div.sc-crosslisted",
    SEL_PROGRAM_LINK => "div#main > ul > li > a",
}

const CATALOG_BASE: &str = "https://catalog.ucsc.edu";

// ─── Data Model ───

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DegreeRequirements {
    pub program_name: String,
    pub program_url: String,
    pub degree_type: DegreeType,
    pub sections: Vec<RequirementSection>,
    pub general_education: Option<GeRequirements>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub enum DegreeType {
    BA,
    BS,
    BM,
    MA,
    MS,
    MFA,
    PhD,
    Other(String),
}

impl DegreeType {
    pub fn from_slug(slug: &str) -> Self {
        let lower = slug.to_lowercase();
        if lower.ends_with("-bs") || lower.ends_with(" b.s.") || lower.ends_with(" bs") {
            Self::BS
        } else if lower.ends_with("-ba") || lower.ends_with(" b.a.") || lower.ends_with(" ba") {
            Self::BA
        } else if lower.ends_with("-bm") || lower.ends_with(" b.m.") || lower.ends_with(" bm") {
            Self::BM
        } else if lower.ends_with("-ms") || lower.ends_with(" m.s.") || lower.ends_with(" ms") {
            Self::MS
        } else if lower.ends_with("-ma") || lower.ends_with(" m.a.") || lower.ends_with(" ma") {
            Self::MA
        } else if lower.ends_with("-mfa") || lower.ends_with(" m.f.a.") || lower.ends_with(" mfa")
        {
            Self::MFA
        } else if lower.ends_with("-phd") || lower.ends_with(" ph.d.") || lower.ends_with(" phd") {
            Self::PhD
        } else {
            Self::Other(slug.to_string())
        }
    }

    pub fn is_undergraduate(&self) -> bool {
        matches!(self, Self::BA | Self::BS | Self::BM)
    }
}

/// Top-level section, corresponds to h3.sc-RequiredCoursesHeading1
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RequirementSection {
    pub heading: String,
    pub subsections: Vec<RequirementSubsection>,
    pub notes: Vec<String>,
}

/// Subsection, corresponds to h4.sc-RequiredCoursesHeading2
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RequirementSubsection {
    pub heading: String,
    pub groups: Vec<RequirementGroup>,
    pub notes: Vec<String>,
}

/// Requirement group, corresponds to h5.sc-RequiredCoursesHeading3
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RequirementGroup {
    pub heading: Option<String>,
    pub rules: Vec<CourseRule>,
    pub notes: Vec<String>,
}

/// A specific selection rule with courses, corresponds to h6 + table
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CourseRule {
    pub rule_type: RuleType,
    pub heading: Option<String>,
    pub courses: Vec<Course>,
    pub alternative: Option<Vec<Course>>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub enum RuleType {
    AllOf,
    OneOf,
    NOf(u32),
    EitherOr,
    CreditsFrom(u32),
    Prose,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Course {
    pub code: String,
    pub title: String,
    pub credits: Option<u32>,
    pub url: Option<String>,
    pub cross_listed: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GeRequirements {
    pub required: Vec<GeArea>,
    pub perspectives: Vec<GeArea>,
    pub practice: Vec<GeArea>,
    pub composition: GeArea,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GeArea {
    pub code: String,
    pub name: String,
    pub credits: u32,
}

pub fn ge_requirements() -> GeRequirements {
    GeRequirements {
        required: vec![
            GeArea {
                code: "CC".into(),
                name: "Cross-Cultural Analysis".into(),
                credits: 5,
            },
            GeArea {
                code: "ER".into(),
                name: "Ethnicity and Race".into(),
                credits: 5,
            },
            GeArea {
                code: "IM".into(),
                name: "Interpreting Arts and Media".into(),
                credits: 5,
            },
            GeArea {
                code: "MF".into(),
                name: "Mathematical and Formal Reasoning".into(),
                credits: 5,
            },
            GeArea {
                code: "SI".into(),
                name: "Scientific Inquiry".into(),
                credits: 5,
            },
            GeArea {
                code: "SR".into(),
                name: "Statistical Reasoning".into(),
                credits: 5,
            },
            GeArea {
                code: "TA".into(),
                name: "Textual Analysis and Interpretation".into(),
                credits: 5,
            },
        ],
        perspectives: vec![
            GeArea {
                code: "PE-E".into(),
                name: "Perspectives: Environmental Awareness".into(),
                credits: 5,
            },
            GeArea {
                code: "PE-H".into(),
                name: "Perspectives: Human Behavior".into(),
                credits: 5,
            },
            GeArea {
                code: "PE-T".into(),
                name: "Perspectives: Technology and Society".into(),
                credits: 5,
            },
        ],
        practice: vec![
            GeArea {
                code: "PR-E".into(),
                name: "Practice: Collaborative Endeavor".into(),
                credits: 2,
            },
            GeArea {
                code: "PR-C".into(),
                name: "Practice: Creative Process".into(),
                credits: 2,
            },
            GeArea {
                code: "PR-S".into(),
                name: "Practice: Service Learning".into(),
                credits: 2,
            },
        ],
        composition: GeArea {
            code: "C".into(),
            name: "Composition".into(),
            credits: 5,
        },
    }
}

// ─── Scraping ───

pub async fn scrape_program_list(
    client: &reqwest::Client,
    url: &str,
) -> Result<Vec<ProgramEntry>> {
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("Failed to fetch program listing from {}", url))?;

    let html = resp
        .text()
        .await
        .context("Failed to read program listing response")?;

    Ok(parse_program_list(&html))
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProgramEntry {
    pub name: String,
    pub slug: String,
    pub url_path: String,
    pub degree_type: DegreeType,
}

fn parse_program_list(html: &str) -> Vec<ProgramEntry> {
    let document = Html::parse_document(html);

    let mut entries = Vec::new();

    for link in document.select(&SEL_PROGRAM_LINK) {
        let name = link.text().collect::<String>().trim().to_string();
        let href = link.value().attr("href").unwrap_or("").to_string();

        if name.is_empty() || href.is_empty() {
            continue;
        }

        let slug = href
            .rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or("")
            .to_string();

        let degree_type = DegreeType::from_slug(&name);

        entries.push(ProgramEntry {
            name,
            slug,
            url_path: href,
            degree_type,
        });
    }

    entries
}

pub async fn scrape_requirements(
    client: &reqwest::Client,
    url: &str,
    program_name: &str,
    degree_type: &DegreeType,
) -> Result<DegreeRequirements> {
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("Failed to fetch requirements for {}", program_name))?;

    let html = resp
        .text()
        .await
        .context("Failed to read requirements page")?;

    parse_requirements(&html, program_name, url, degree_type)
}

fn parse_requirements(
    html: &str,
    program_name: &str,
    url: &str,
    degree_type: &DegreeType,
) -> Result<DegreeRequirements> {
    let document = Html::parse_document(html);

    // Try to find the requirements container (div#degree-req-2 for undergrad, or just div#main)
    let container = document
        .select(&SEL_REQ_CONTAINER)
        .next()
        .or_else(|| document.select(&SEL_MAIN).next())
        .with_context(|| {
            format!(
                "Could not find requirements container for {}. The catalog page structure may have changed.",
                program_name
            )
        })?;

    let sections = parse_heading_hierarchy(container);

    // Drop "Planners" sections — they're informational per-quarter schedule grids,
    // not actual requirements. We classify each heading against a set of section
    // types; only drop if the heading lands unambiguously on "planner". Anything
    // ambiguous (e.g. "Course Planners and Requirements") gets kept so we don't
    // hide real reqs behind a fuzzy match. We deliberately omit `word_boundary`
    // here so "Planners" / "Planner" both match the needle "planner".
    let classifier = FuzzyMatcher::new(["planner", "requirement", "course", "elective"])
        .case_insensitive()
        .whitespace_collapsed();
    let sections: Vec<RequirementSection> = sections
        .into_iter()
        .filter(|s| classifier.matches_unambiguously(&s.heading) != Some("planner"))
        .collect();

    let general_education = if degree_type.is_undergraduate() {
        Some(ge_requirements())
    } else {
        None
    };

    Ok(DegreeRequirements {
        program_name: program_name.to_string(),
        program_url: url.to_string(),
        degree_type: degree_type.clone(),
        sections,
        general_education,
    })
}

/// Walk the flat sequence of sibling elements and reconstruct the heading hierarchy.
fn parse_heading_hierarchy(container: ElementRef) -> Vec<RequirementSection> {
    let mut sections: Vec<RequirementSection> = Vec::new();
    let mut current_section: Option<RequirementSection> = None;
    let mut current_subsection: Option<RequirementSubsection> = None;
    let mut current_group: Option<RequirementGroup> = None;
    let mut current_rule_heading: Option<String> = None;

    // Collect all direct and nested elements we care about by walking the DOM
    for child in container.children() {
        let Some(element) = ElementRef::wrap(child) else {
            continue;
        };

        let tag = element.value().name();

        // Check if this is one of our heading types
        if is_matching(element, "h3", "sc-RequiredCoursesHeading1") {
            // Push everything accumulated so far
            flush_group(&mut current_group, &current_rule_heading, &mut current_subsection);
            flush_subsection(&mut current_subsection, &mut current_section);
            flush_section(&mut current_section, &mut sections);

            let heading = element.text().collect::<String>().trim().to_string();
            current_section = Some(RequirementSection {
                heading,
                subsections: Vec::new(),
                notes: Vec::new(),
            });
            current_rule_heading = None;
        } else if is_matching(element, "h4", "sc-RequiredCoursesHeading2") {
            flush_group(&mut current_group, &current_rule_heading, &mut current_subsection);
            flush_subsection(&mut current_subsection, &mut current_section);

            let heading = element.text().collect::<String>().trim().to_string();
            current_subsection = Some(RequirementSubsection {
                heading,
                groups: Vec::new(),
                notes: Vec::new(),
            });
            current_rule_heading = None;
        } else if is_matching(element, "h5", "sc-RequiredCoursesHeading3") {
            flush_group(&mut current_group, &current_rule_heading, &mut current_subsection);

            let heading = element.text().collect::<String>().trim().to_string();
            current_group = Some(RequirementGroup {
                heading: Some(heading),
                rules: Vec::new(),
                notes: Vec::new(),
            });
            current_rule_heading = None;
        } else if is_matching(element, "h6", "sc-RequiredCoursesHeading4") {
            let heading = element.text().collect::<String>().trim().to_string();
            current_rule_heading = Some(heading);
        } else if tag == "table" {
            let courses = parse_course_table(element);
            if courses.is_empty() {
                continue;
            }

            // Determine rule type from the current h6 heading
            let (rule_type, primary, alternative) = classify_courses(
                courses,
                current_rule_heading.as_deref(),
            );

            let rule = CourseRule {
                rule_type,
                heading: current_rule_heading.take(),
                courses: primary,
                alternative,
                description: None,
            };

            // Attach rule to the current group, creating one if needed
            ensure_group(&mut current_group);
            if let Some(ref mut group) = current_group {
                group.rules.push(rule);
            }
        } else if tag == "div" && has_class(element, "sc-requirementsNote") {
            let note_text = element.text().collect::<String>().trim().to_string();
            if !note_text.is_empty() {
                // Attach note to the most specific current context
                if let Some(ref mut group) = current_group {
                    group.notes.push(note_text);
                } else if let Some(ref mut subsec) = current_subsection {
                    subsec.notes.push(note_text);
                } else if let Some(ref mut sec) = current_section {
                    sec.notes.push(note_text);
                }
            }
        } else if tag == "p" {
            let text = element.text().collect::<String>().trim().to_string();
            if text.is_empty() {
                continue;
            }

            // Check if this is a prose constraint that should become a rule
            let is_constraint = text.contains("must be completed")
                || text.contains("courses from")
                || text.contains("credits from")
                || text.contains("at least")
                || text.contains("at most");

            if is_constraint {
                ensure_group(&mut current_group);
                if let Some(ref mut group) = current_group {
                    let rule = CourseRule {
                        rule_type: parse_credits_from_prose(&text).unwrap_or(RuleType::Prose),
                        heading: None,
                        courses: Vec::new(),
                        alternative: None,
                        description: Some(text),
                    };
                    group.rules.push(rule);
                }
            } else if !text.is_empty() {
                // Add as note to current context
                if let Some(ref mut group) = current_group {
                    group.notes.push(text);
                } else if let Some(ref mut subsec) = current_subsection {
                    subsec.notes.push(text);
                } else if let Some(ref mut sec) = current_section {
                    sec.notes.push(text);
                }
            }
        } else if tag == "ol" || tag == "ul" {
            // Parse <ol>/<ul> items: extract structured rules from <li> items
            // that contain course links (or course codes in plain text).
            let mut note_items: Vec<String> = Vec::new();

            for li in element
                .children()
                .filter_map(ElementRef::wrap)
                .filter(|el| el.value().name() == "li")
            {
                let text = li.text().collect::<String>().trim().to_string();
                if text.is_empty() {
                    continue;
                }

                // Check if this <li> contains course links
                let mut courses: Vec<Course> = li
                    .select(&SEL_COURSE_LINK)
                    .filter_map(|a| {
                        let href = a.value().attr("href").unwrap_or("");
                        if href.contains("narrative-courses")
                            || !href.contains("/courses/")
                        {
                            return None;
                        }
                        let code = a.text().collect::<String>().trim().to_string();
                        if code.is_empty() {
                            return None;
                        }
                        Some(Course {
                            code,
                            title: String::new(),
                            credits: Some(5),
                            url: Some(format!("{}{}", CATALOG_BASE, href)),
                            cross_listed: None,
                        })
                    })
                    .collect();

                let lower = text.to_lowercase();

                // If no course links found, try extracting codes from plain text
                if courses.is_empty() && has_selection_language(&lower) {
                    courses = extract_course_codes_from_text(&text)
                        .into_iter()
                        .map(|code| Course {
                            code,
                            title: String::new(),
                            credits: None,
                            url: None,
                            cross_listed: None,
                        })
                        .collect();
                }

                // If this <li> has courses AND selection language, fix existing rules
                if courses.len() >= 2 && has_selection_language(&lower) {
                    let course_codes: Vec<String> =
                        courses.iter().map(|c| c.code.clone()).collect();
                    // Search already-flushed section for a matching AllOf rule and fix it
                    if let Some(ref mut sec) = current_section {
                        fix_allof_from_selection(sec, &course_codes, &lower);
                    }
                } else {
                    note_items.push(text);
                }
            }

            if !note_items.is_empty() {
                let note = note_items
                    .iter()
                    .enumerate()
                    .map(|(i, item)| format!("{}. {}", i + 1, item))
                    .collect::<Vec<_>>()
                    .join("\n");

                if let Some(ref mut group) = current_group {
                    group.notes.push(note);
                } else if let Some(ref mut subsec) = current_subsection {
                    subsec.notes.push(note);
                } else if let Some(ref mut sec) = current_section {
                    sec.notes.push(note);
                }
            }
        }
        // Recurse into div children that aren't notes (some pages wrap content in divs)
        else if tag == "div" && !has_class(element, "sc-requirementsNote") {
            let inner = parse_heading_hierarchy(element);
            for inner_section in inner {
                // Flush current group/subsection before merging
                flush_group(&mut current_group, &current_rule_heading, &mut current_subsection);
                flush_subsection(&mut current_subsection, &mut current_section);

                if current_section.is_some() {
                    // Merge inner sections' subsections into current section
                    for sub in inner_section.subsections {
                        if let Some(ref mut sec) = current_section {
                            sec.subsections.push(sub);
                        }
                    }
                } else {
                    flush_section(&mut current_section, &mut sections);
                    sections.push(inner_section);
                }
            }
        }
    }

    // Flush remaining state
    flush_group(&mut current_group, &current_rule_heading, &mut current_subsection);
    flush_subsection(&mut current_subsection, &mut current_section);
    flush_section(&mut current_section, &mut sections);

    sections
}

fn is_matching(element: ElementRef, tag: &str, class: &str) -> bool {
    element.value().name() == tag && has_class(element, class)
}

fn has_class(element: ElementRef, class: &str) -> bool {
    element
        .value()
        .attr("class")
        .is_some_and(|c| c.split_whitespace().any(|c| c == class))
}

fn ensure_group(group: &mut Option<RequirementGroup>) {
    if group.is_none() {
        *group = Some(RequirementGroup {
            heading: None,
            rules: Vec::new(),
            notes: Vec::new(),
        });
    }
}

fn flush_group(
    group: &mut Option<RequirementGroup>,
    rule_heading: &Option<String>,
    subsection: &mut Option<RequirementSubsection>,
) {
    if let Some(g) = group.take() {
        if !g.rules.is_empty() || !g.notes.is_empty() {
            ensure_subsection(subsection, rule_heading);
            if let Some(sub) = subsection.as_mut() {
                sub.groups.push(g);
            }
        }
    }
}

fn ensure_subsection(subsection: &mut Option<RequirementSubsection>, _hint: &Option<String>) {
    if subsection.is_none() {
        *subsection = Some(RequirementSubsection {
            heading: String::new(),
            groups: Vec::new(),
            notes: Vec::new(),
        });
    }
}

fn flush_subsection(
    subsection: &mut Option<RequirementSubsection>,
    section: &mut Option<RequirementSection>,
) {
    if let Some(sub) = subsection.take() {
        if !sub.groups.is_empty() || !sub.notes.is_empty() {
            ensure_section(section);
            if let Some(sec) = section.as_mut() {
                sec.subsections.push(sub);
            }
        }
    }
}

fn ensure_section(section: &mut Option<RequirementSection>) {
    if section.is_none() {
        *section = Some(RequirementSection {
            heading: String::new(),
            subsections: Vec::new(),
            notes: Vec::new(),
        });
    }
}

fn flush_section(
    section: &mut Option<RequirementSection>,
    sections: &mut Vec<RequirementSection>,
) {
    if let Some(sec) = section.take() {
        if !sec.subsections.is_empty() || !sec.notes.is_empty() {
            sections.push(sec);
        }
    }
}

/// Parse a course table, handling either/or narrative rows.
fn parse_course_table(table: ElementRef) -> Vec<ParsedRow> {
    let mut rows = Vec::new();

    for tr in table.select(&SEL_TR) {
        // Check for narrative row
        if let Some(link) = tr.select(&SEL_COURSE_LINK).next() {
            let href = link.value().attr("href").unwrap_or("");
            if href.contains("/narrative-courses/") {
                let title_text = tr
                    .select(&SEL_COURSE_TITLE)
                    .next()
                    .map(|t| t.text().collect::<String>().trim().to_string())
                    .unwrap_or_default();
                rows.push(ParsedRow::Narrative(title_text.to_lowercase()));
                continue;
            }
        }

        // Regular course row
        let num_cell = tr.select(&SEL_COURSE_NUM).next();
        let Some(num_cell) = num_cell else {
            continue;
        };

        let code = num_cell
            .select(&SEL_COURSE_LINK)
            .next()
            .map(|a| a.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        if code.is_empty() {
            continue;
        }

        let url = num_cell
            .select(&SEL_COURSE_LINK)
            .next()
            .and_then(|a| a.value().attr("href"))
            .map(|h| format!("{}{}", CATALOG_BASE, h));

        let cross_listed = num_cell
            .select(&SEL_CROSSLISTED)
            .next()
            .map(|d| d.text().collect::<String>().trim().trim_start_matches('/').trim().to_string());

        let title = tr
            .select(&SEL_COURSE_TITLE)
            .next()
            .map(|t| t.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        let credits = tr
            .select(&SEL_CREDITS)
            .next()
            .and_then(|p| {
                let text = p.text().collect::<String>().trim().to_string();
                text.parse::<u32>().ok()
            });

        rows.push(ParsedRow::Course(Course {
            code,
            title,
            credits,
            url,
            cross_listed,
        }));
    }

    rows
}

#[derive(Debug)]
enum ParsedRow {
    Course(Course),
    Narrative(String), // "either these courses", "or these courses", etc.
}

/// Classify parsed rows into a rule type with optional either/or splitting.
fn classify_courses(
    rows: Vec<ParsedRow>,
    heading: Option<&str>,
) -> (RuleType, Vec<Course>, Option<Vec<Course>>) {
    // Check for either/or pattern
    let has_either = rows.iter().any(|r| matches!(r, ParsedRow::Narrative(t) if t.contains("either")));
    let has_or = rows.iter().any(|r| matches!(r, ParsedRow::Narrative(t) if t.starts_with("or ")));

    if has_either && has_or {
        let mut primary = Vec::new();
        let mut alternative = Vec::new();
        let mut in_alternative = false;

        for row in rows {
            match row {
                ParsedRow::Narrative(t) if t.starts_with("or ") => {
                    in_alternative = true;
                }
                ParsedRow::Narrative(_) => {} // "either these courses" — just a marker
                ParsedRow::Course(c) => {
                    if in_alternative {
                        alternative.push(c);
                    } else {
                        primary.push(c);
                    }
                }
            }
        }

        return (RuleType::EitherOr, primary, Some(alternative));
    }

    // No either/or — collect all courses
    let courses: Vec<Course> = rows
        .into_iter()
        .filter_map(|r| match r {
            ParsedRow::Course(c) => Some(c),
            _ => None,
        })
        .collect();

    let rule_type = heading
        .map(|h| infer_rule_type(h))
        .unwrap_or(RuleType::AllOf);

    (rule_type, courses, None)
}

fn infer_rule_type(heading: &str) -> RuleType {
    let lower = heading.to_lowercase();

    if lower.contains("one of") || lower.contains("one course from") {
        return RuleType::OneOf;
    }

    // Match "N of the following" or "N courses"
    if let Some(n) = extract_number_from_heading(&lower) {
        if lower.contains("of the following")
            || lower.contains("courses from")
            || lower.contains("courses must")
        {
            return RuleType::NOf(n);
        }
    }

    if lower.contains("credits from") || lower.contains("credits of") {
        if let Some(n) = extract_number_from_heading(&lower) {
            return RuleType::CreditsFrom(n);
        }
    }

    // Default: all of
    RuleType::AllOf
}

fn extract_number_from_heading(text: &str) -> Option<u32> {
    let word_numbers = [
        ("one", 1),
        ("two", 2),
        ("three", 3),
        ("four", 4),
        ("five", 5),
        ("six", 6),
        ("seven", 7),
        ("eight", 8),
        ("nine", 9),
        ("ten", 10),
    ];

    for (word, num) in &word_numbers {
        if text.contains(word) {
            return Some(*num);
        }
    }

    // Try to find a digit
    for word in text.split_whitespace() {
        if let Ok(n) = word.parse::<u32>() {
            return Some(n);
        }
    }

    None
}

/// Check if text contains selection language indicating a structured rule.
fn has_selection_language(lower: &str) -> bool {
    lower.contains("any two out of")
        || lower.contains("any one of")
        || lower.contains("two out of the following")
        || lower.contains("one of the following")
        || lower.contains("one course each from")
        || lower.contains("must be met by taking")
}

/// Extract course codes (e.g., "CSE 200", "CSE 210A") from plain text.
/// Matches patterns like: 2-5 uppercase letters, space, 1-3 digits, optional uppercase letter.
fn extract_course_codes_from_text(text: &str) -> Vec<String> {
    let mut codes = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Look for start of a department code: 2+ uppercase letters
        if chars[i].is_ascii_uppercase() {
            let dept_start = i;
            while i < len && chars[i].is_ascii_uppercase() {
                i += 1;
            }
            let dept_len = i - dept_start;
            if dept_len >= 2 && dept_len <= 5 {
                // Expect whitespace then digits
                if i < len && chars[i] == ' ' {
                    i += 1; // skip space
                    let num_start = i;
                    while i < len && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                    let num_len = i - num_start;
                    if num_len >= 1 && num_len <= 3 {
                        // Optional trailing uppercase letter (e.g., "210A")
                        let end = if i < len && chars[i].is_ascii_uppercase()
                            && (i + 1 >= len || !chars[i + 1].is_ascii_uppercase())
                        {
                            i += 1;
                            i
                        } else {
                            i
                        };
                        let code: String = chars[dept_start..end].iter().collect();
                        codes.push(code);
                    }
                }
            }
        } else {
            i += 1;
        }
    }

    // Deduplicate
    let mut seen = std::collections::HashSet::new();
    codes.retain(|c| seen.insert(c.clone()));
    codes
}

/// When an <ol><li> contains selection language like "any two out of CSE 201, CSE 210A, CSE 220",
/// find the matching AllOf rule in the section's already-flushed subsections and fix it.
/// For "any two out of" with N courses, splits into: required course(s) + NOf(2) for the pool.
fn fix_allof_from_selection(
    section: &mut RequirementSection,
    course_codes: &[String],
    lower: &str,
) {
    use std::collections::HashSet;

    let code_set: HashSet<&str> = course_codes.iter().map(|s| s.as_str()).collect();

    for sub in &mut section.subsections {
        for group in &mut sub.groups {
            for rule in &mut group.rules {
                if !matches!(rule.rule_type, RuleType::AllOf) {
                    continue;
                }
                // Check if this rule's courses overlap significantly with the codes from the <li>
                let rule_codes: HashSet<&str> =
                    rule.courses.iter().map(|c| c.code.as_str()).collect();
                let overlap = code_set.intersection(&rule_codes).count();
                if overlap < 2 || overlap < code_set.len().saturating_sub(1) {
                    continue;
                }

                // Found a matching AllOf rule — fix it based on selection language
                if lower.contains("any two out of")
                    || lower.contains("two out of the following")
                {
                    // Split: courses mentioned before "any two" are required (AllOf),
                    // courses mentioned after are the pool (NOf(2)).
                    // In practice: first course in list is required, rest are pool.
                    // Keep only the pool courses in this rule; the required course
                    // stays as a separate AllOf entry.
                    let mut pool_courses = Vec::new();
                    let mut required_courses = Vec::new();

                    // The first course code in the <li> text is typically the required one
                    if course_codes.len() >= 3 {
                        let required_code = &course_codes[0];
                        for c in &rule.courses {
                            if c.code == *required_code {
                                required_courses.push(c.clone());
                            } else if code_set.contains(c.code.as_str()) {
                                pool_courses.push(c.clone());
                            } else {
                                // Courses not mentioned in the <li> — keep as required
                                required_courses.push(c.clone());
                            }
                        }
                    }

                    if pool_courses.len() >= 2 {
                        // Replace this rule with the required courses as AllOf
                        rule.courses = required_courses;
                        rule.rule_type = RuleType::AllOf;

                        // Add a new NOf(2) rule for the pool
                        group.rules.push(CourseRule {
                            rule_type: RuleType::NOf(2),
                            heading: None,
                            courses: pool_courses,
                            alternative: None,
                            description: None,
                        });
                        return; // Only fix the first matching rule
                    }
                } else if lower.contains("any one of")
                    || lower.contains("one of the following")
                {
                    rule.rule_type = RuleType::OneOf;
                    return;
                }
            }
        }
    }
}

fn parse_credits_from_prose(text: &str) -> Option<RuleType> {
    let lower = text.to_lowercase();
    if lower.contains("credits") {
        if let Some(n) = extract_number_from_heading(&lower) {
            return Some(RuleType::CreditsFrom(n));
        }
    }
    if lower.contains("courses") || lower.contains("must be completed") {
        if let Some(n) = extract_number_from_heading(&lower) {
            return Some(RuleType::NOf(n));
        }
    }
    None
}

// ─── Markdown rendering ───

impl DegreeRequirements {
    pub fn format(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "# {} Requirements\n", self.program_name);
        for section in &self.sections {
            out.push_str(&section.format());
        }
        if let Some(ge) = self.general_education.as_ref() {
            out.push_str("\n## General Education Requirements\n\n");
            out.push_str(&ge.format());
        }
        out
    }
}

impl RequirementSection {
    pub fn format(&self) -> String {
        let mut out = String::new();
        if !self.heading.is_empty() {
            let _ = writeln!(out, "## {}\n", self.heading);
        }
        for note in &self.notes {
            let _ = writeln!(out, "> {}\n", note);
        }
        for subsection in &self.subsections {
            out.push_str(&subsection.format());
        }
        out
    }
}

impl RequirementSubsection {
    pub fn format(&self) -> String {
        let mut out = String::new();
        if !self.heading.is_empty() {
            let _ = writeln!(out, "### {}\n", self.heading);
        }
        for note in &self.notes {
            let _ = writeln!(out, "> {}\n", note);
        }
        for group in &self.groups {
            out.push_str(&group.format());
        }
        out
    }
}

impl RequirementGroup {
    pub fn format(&self) -> String {
        let mut out = String::new();
        if let Some(heading) = self.heading.as_ref() {
            let _ = writeln!(out, "#### {}\n", heading);
        }
        for note in &self.notes {
            let _ = writeln!(out, "> {}\n", note);
        }
        for rule in &self.rules {
            out.push_str(&rule.format());
        }
        out
    }
}

impl CourseRule {
    pub fn format(&self) -> String {
        let mut out = String::new();
        // Show the rule heading/type
        match &self.rule_type {
            RuleType::AllOf => {
                if let Some(h) = self.heading.as_ref() {
                    let _ = writeln!(out, "**{}:**", h);
                }
            }
            RuleType::OneOf => {
                let _ = writeln!(
                    out,
                    "**{}:**",
                    self.heading.as_deref().unwrap_or("One of the following")
                );
            }
            RuleType::NOf(n) => {
                let _ = writeln!(
                    out,
                    "**{}:**",
                    self.heading
                        .as_deref()
                        .unwrap_or(&format!("{} of the following", n))
                );
            }
            RuleType::EitherOr => {
                let _ = writeln!(out, "**Either these courses:**");
            }
            RuleType::CreditsFrom(n) => {
                let _ = writeln!(
                    out,
                    "**{}:**",
                    self.heading
                        .as_deref()
                        .unwrap_or(&format!("{} credits from the following", n))
                );
            }
            RuleType::Prose => {
                if let Some(desc) = self.description.as_ref() {
                    let _ = writeln!(out, "*{}*\n", desc);
                }
                return out;
            }
        }

        for course in &self.courses {
            out.push_str(&course.format());
        }

        if let Some(alt) = self.alternative.as_ref() {
            let _ = writeln!(out, "\n**Or these courses:**");
            for course in alt {
                out.push_str(&course.format());
            }
        }

        out.push('\n');
        out
    }
}

impl Course {
    pub fn format(&self) -> String {
        let mut out = format!("- {} — {}", self.code, self.title);
        if let Some(credits) = self.credits {
            let _ = write!(out, " ({} credits)", credits);
        }
        if let Some(cross) = self.cross_listed.as_ref() {
            let _ = write!(out, " [also {}]", cross);
        }
        out.push('\n');
        out
    }
}

impl GeRequirements {
    pub fn format(&self) -> String {
        let mut out = String::from("**Required (all of the following):**\n");
        for area in &self.required {
            let _ = writeln!(out, "- {} — {} ({} credits)", area.code, area.name, area.credits);
        }
        out.push_str("\n**Perspectives (one of the following):**\n");
        for area in &self.perspectives {
            let _ = writeln!(out, "- {} — {} ({} credits)", area.code, area.name, area.credits);
        }
        out.push_str("\n**Practice (one of the following):**\n");
        for area in &self.practice {
            let _ = writeln!(out, "- {} — {} ({} credits)", area.code, area.name, area.credits);
        }
        let _ = writeln!(
            out,
            "\n**Writing:**\n- {} — {} ({} credits)\n- DC — Disciplinary Communication (satisfied via major)",
            self.composition.code, self.composition.name, self.composition.credits
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_program_list() {
        let html = r#"
        <html><body>
        <div id="main">
            <h1>Bachelor's Degrees</h1>
            <div class="combinedChild"></div>
            <ul>
                <li><a href="/en/current/general-catalog/academic-units/baskin-engineering/computer-science-and-engineering/computer-science-bs">Computer Science B.S.</a></li>
                <li><a href="/en/current/general-catalog/academic-units/baskin-engineering/computer-science-and-engineering/computer-science-ba">Computer Science B.A.</a></li>
                <li><a href="/en/current/general-catalog/academic-units/arts-division/music/music-bm">Music B.M.</a></li>
            </ul>
        </div>
        </body></html>"#;

        let entries = parse_program_list(html);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "Computer Science B.S.");
        assert_eq!(entries[0].slug, "computer-science-bs");
        assert_eq!(entries[0].degree_type, DegreeType::BS);
        assert_eq!(entries[1].degree_type, DegreeType::BA);
        assert_eq!(entries[2].degree_type, DegreeType::BM);
    }

    #[test]
    fn test_parse_simple_requirements() {
        let html = r#"
        <html><body>
        <div id="degree-req-2">
            <h2>Requirements and Planners</h2>
            <h3 class="sc-RequiredCoursesHeading1">Course Requirements</h3>
            <h4 class="sc-RequiredCoursesHeading2">Lower-Division Courses</h4>
            <h5 class="sc-RequiredCoursesHeading3">Computer Science</h5>
            <h6 class="sc-RequiredCoursesHeading4">All of the following</h6>
            <table>
                <tr>
                    <td class="sc-coursenumber"><a class="sc-courselink" href="/courses/cse/lower/cse-12">CSE 12</a></td>
                    <td class="sc-coursetitle">Computer Systems and Assembly Language</td>
                    <td><p class="credits">7</p></td>
                </tr>
                <tr>
                    <td class="sc-coursenumber"><a class="sc-courselink" href="/courses/cse/lower/cse-16">CSE 16</a></td>
                    <td class="sc-coursetitle">Applied Discrete Mathematics</td>
                    <td><p class="credits">5</p></td>
                </tr>
            </table>
        </div>
        </body></html>"#;

        let reqs = parse_requirements(html, "Test BS", "http://test", &DegreeType::BS).unwrap();
        assert_eq!(reqs.sections.len(), 1);
        assert_eq!(reqs.sections[0].heading, "Course Requirements");
        assert_eq!(reqs.sections[0].subsections.len(), 1);
        assert_eq!(reqs.sections[0].subsections[0].heading, "Lower-Division Courses");
        assert_eq!(reqs.sections[0].subsections[0].groups.len(), 1);
        assert_eq!(reqs.sections[0].subsections[0].groups[0].heading.as_deref(), Some("Computer Science"));
        assert_eq!(reqs.sections[0].subsections[0].groups[0].rules.len(), 1);
        assert_eq!(reqs.sections[0].subsections[0].groups[0].rules[0].rule_type, RuleType::AllOf);
        assert_eq!(reqs.sections[0].subsections[0].groups[0].rules[0].courses.len(), 2);
        assert_eq!(reqs.sections[0].subsections[0].groups[0].rules[0].courses[0].code, "CSE 12");
        assert_eq!(reqs.sections[0].subsections[0].groups[0].rules[0].courses[0].credits, Some(7));
    }

    #[test]
    fn test_parse_either_or() {
        let html = r#"
        <html><body>
        <div id="degree-req-2">
            <h3 class="sc-RequiredCoursesHeading1">Course Requirements</h3>
            <h4 class="sc-RequiredCoursesHeading2">Lower-Division</h4>
            <h5 class="sc-RequiredCoursesHeading3">Mathematics</h5>
            <table>
                <tr>
                    <td class="sc-coursenumber"><a class="sc-courselink" href="/narrative-courses/either-these-courses"> </a></td>
                    <td class="sc-coursetitle">Either these courses</td>
                    <td><p class="credits"></p></td>
                </tr>
                <tr>
                    <td class="sc-coursenumber"><a class="sc-courselink" href="/courses/math/math-19a">MATH 19A</a></td>
                    <td class="sc-coursetitle">Calculus I</td>
                    <td><p class="credits">5</p></td>
                </tr>
                <tr>
                    <td class="sc-coursenumber"><a class="sc-courselink" href="/courses/math/math-19b">MATH 19B</a></td>
                    <td class="sc-coursetitle">Calculus II</td>
                    <td><p class="credits">5</p></td>
                </tr>
                <tr>
                    <td class="sc-coursenumber"><a class="sc-courselink" href="/narrative-courses/or-these-courses"> </a></td>
                    <td class="sc-coursetitle">or these courses</td>
                    <td><p class="credits"></p></td>
                </tr>
                <tr>
                    <td class="sc-coursenumber"><a class="sc-courselink" href="/courses/math/math-20a">MATH 20A</a></td>
                    <td class="sc-coursetitle">Honors Calculus I</td>
                    <td><p class="credits">5</p></td>
                </tr>
                <tr>
                    <td class="sc-coursenumber"><a class="sc-courselink" href="/courses/math/math-20b">MATH 20B</a></td>
                    <td class="sc-coursetitle">Honors Calculus II</td>
                    <td><p class="credits">5</p></td>
                </tr>
            </table>
        </div>
        </body></html>"#;

        let reqs = parse_requirements(html, "Test", "http://test", &DegreeType::BS).unwrap();
        let rule = &reqs.sections[0].subsections[0].groups[0].rules[0];
        assert_eq!(rule.rule_type, RuleType::EitherOr);
        assert_eq!(rule.courses.len(), 2);
        assert_eq!(rule.courses[0].code, "MATH 19A");
        assert_eq!(rule.courses[1].code, "MATH 19B");
        let alt = rule.alternative.as_ref().unwrap();
        assert_eq!(alt.len(), 2);
        assert_eq!(alt[0].code, "MATH 20A");
        assert_eq!(alt[1].code, "MATH 20B");
    }

    #[test]
    fn test_empty_table_skipped() {
        let html = r#"
        <html><body>
        <div id="degree-req-2">
            <h3 class="sc-RequiredCoursesHeading1">Course Requirements</h3>
            <table>
            </table>
            <h4 class="sc-RequiredCoursesHeading2">Courses</h4>
            <h6 class="sc-RequiredCoursesHeading4">All of the following</h6>
            <table>
                <tr>
                    <td class="sc-coursenumber"><a class="sc-courselink" href="/courses/cse/cse-12">CSE 12</a></td>
                    <td class="sc-coursetitle">Computer Systems</td>
                    <td><p class="credits">7</p></td>
                </tr>
            </table>
        </div>
        </body></html>"#;

        let reqs = parse_requirements(html, "Test", "http://test", &DegreeType::BS).unwrap();
        // Should have 1 section with 1 subsection with courses
        assert_eq!(reqs.sections[0].subsections[0].groups[0].rules[0].courses.len(), 1);
    }

    #[test]
    fn test_planner_table_skipped() {
        let html = r#"
        <html><body>
        <div id="degree-req-2">
            <h3 class="sc-RequiredCoursesHeading1">Planners</h3>
            <table><tbody>
                <tr>
                    <td><strong>Fall</strong></td>
                    <td><strong>Winter</strong></td>
                </tr>
            </tbody></table>
        </div>
        </body></html>"#;

        let reqs = parse_requirements(html, "Test", "http://test", &DegreeType::BS).unwrap();
        // Planners section should be filtered out
        assert_eq!(reqs.sections.len(), 0);
    }

    #[test]
    fn test_infer_rule_type() {
        assert_eq!(infer_rule_type("All of the following"), RuleType::AllOf);
        assert_eq!(infer_rule_type("One of the following"), RuleType::OneOf);
        assert_eq!(infer_rule_type("Plus one of the following"), RuleType::OneOf);
        assert_eq!(infer_rule_type("Two of the following"), RuleType::NOf(2));
        assert_eq!(infer_rule_type("Four courses from the following"), RuleType::NOf(4));
        assert_eq!(infer_rule_type("15 credits from the following"), RuleType::CreditsFrom(15));
    }

    #[test]
    fn test_degree_type_from_slug() {
        assert_eq!(DegreeType::from_slug("Computer Science B.S."), DegreeType::BS);
        assert_eq!(DegreeType::from_slug("History B.A."), DegreeType::BA);
        assert_eq!(DegreeType::from_slug("computer-science-ms"), DegreeType::MS);
        assert_eq!(DegreeType::from_slug("Music B.M."), DegreeType::BM);
        assert!(DegreeType::from_slug("Computer Science B.S.").is_undergraduate());
        assert!(!DegreeType::from_slug("computer-science-ms").is_undergraduate());
    }

    #[test]
    fn test_cross_listed_courses() {
        let html = r#"
        <html><body>
        <div id="degree-req-2">
            <h3 class="sc-RequiredCoursesHeading1">Course Requirements</h3>
            <h4 class="sc-RequiredCoursesHeading2">Courses</h4>
            <table>
                <tr>
                    <td class="sc-coursenumber">
                        <a class="sc-courselink" href="/courses/cse/cse-185e">CSE 185E</a>
                        <div class="sc-crosslisted">/CSE 185S</div>
                    </td>
                    <td class="sc-coursetitle">Technical Writing</td>
                    <td><p class="credits">5</p></td>
                </tr>
            </table>
        </div>
        </body></html>"#;

        let reqs = parse_requirements(html, "Test", "http://test", &DegreeType::BS).unwrap();
        let course = &reqs.sections[0].subsections[0].groups[0].rules[0].courses[0];
        assert_eq!(course.code, "CSE 185E");
        assert_eq!(course.cross_listed.as_deref(), Some("CSE 185S"));
    }

    #[test]
    fn test_extract_course_codes_from_text() {
        let codes = extract_course_codes_from_text(
            "A core requirement must be met by taking CSE 200, and any two out of the following three courses: CSE 201, CSE 210A, and CSE 220."
        );
        assert_eq!(codes, vec!["CSE 200", "CSE 201", "CSE 210A", "CSE 220"]);

        let codes = extract_course_codes_from_text("MATH 19A and MATH 19B");
        assert_eq!(codes, vec!["MATH 19A", "MATH 19B"]);

        let codes = extract_course_codes_from_text("No courses here.");
        assert!(codes.is_empty());
    }

    #[test]
    fn test_fix_allof_from_selection() {
        // Simulate: table parsed as AllOf [CSE 200, CSE 201, CSE 210A, CSE 220],
        // then <ol><li> says "CSE 200, and any two out of CSE 201, CSE 210A, CSE 220"
        let html = r#"
        <html><body>
        <div id="degree-req-2">
            <h3 class="sc-RequiredCoursesHeading1">Course Requirements</h3>
            <table>
                <tr>
                    <td class="sc-coursenumber"><a class="sc-courselink" href="/courses/cse/cse-200">CSE 200</a></td>
                    <td class="sc-coursetitle">Research and Teaching</td>
                    <td><p class="credits">3</p></td>
                </tr>
                <tr>
                    <td class="sc-coursenumber"><a class="sc-courselink" href="/courses/cse/cse-201">CSE 201</a></td>
                    <td class="sc-coursetitle">Analysis of Algorithms</td>
                    <td><p class="credits">5</p></td>
                </tr>
                <tr>
                    <td class="sc-coursenumber"><a class="sc-courselink" href="/courses/cse/cse-210a">CSE 210A</a></td>
                    <td class="sc-coursetitle">Programming Languages</td>
                    <td><p class="credits">5</p></td>
                </tr>
                <tr>
                    <td class="sc-coursenumber"><a class="sc-courselink" href="/courses/cse/cse-220">CSE 220</a></td>
                    <td class="sc-coursetitle">Computer Architecture</td>
                    <td><p class="credits">5</p></td>
                </tr>
            </table>
            <h4 class="sc-RequiredCoursesHeading2">Thesis Plan I</h4>
            <ol>
                <li>A core requirement must be met by taking CSE 200, and any two out of the following three courses: CSE 201, CSE 210A, and CSE 220.</li>
                <li>Some other requirement text.</li>
            </ol>
        </div>
        </body></html>"#;

        let reqs = parse_requirements(html, "Test MS", "http://test", &DegreeType::MS).unwrap();
        let section = &reqs.sections[0];

        // The first subsection (from the table, before h4) should have been fixed
        let first_sub = &section.subsections[0];
        assert!(!first_sub.groups.is_empty(), "should have groups from the table");
        let group = &first_sub.groups[0];

        // Should now have 2 rules: AllOf [CSE 200] + NOf(2) [CSE 201, CSE 210A, CSE 220]
        assert_eq!(group.rules.len(), 2, "expected 2 rules after fix, got {}: {:?}", group.rules.len(), group.rules);
        assert_eq!(group.rules[0].rule_type, RuleType::AllOf);
        assert_eq!(group.rules[0].courses.len(), 1);
        assert_eq!(group.rules[0].courses[0].code, "CSE 200");
        assert_eq!(group.rules[1].rule_type, RuleType::NOf(2));
        assert_eq!(group.rules[1].courses.len(), 3);
        let pool_codes: Vec<&str> = group.rules[1].courses.iter().map(|c| c.code.as_str()).collect();
        assert!(pool_codes.contains(&"CSE 201"));
        assert!(pool_codes.contains(&"CSE 210A"));
        assert!(pool_codes.contains(&"CSE 220"));
    }
}
