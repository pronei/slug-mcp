use std::collections::HashSet;
use std::fmt::Write;

use super::scraper::{
    CourseRule, DegreeRequirements, GeRequirements, RequirementGroup, RequirementSection,
    RequirementSubsection, RuleType,
};

// ─── Progress Report Data Model ───

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProgressReport {
    pub program_name: String,
    pub sections: Vec<SectionProgress>,
    pub ge_progress: Option<GeProgress>,
    pub breadth_progress: Option<super::breadth::BreadthProgress>,
    pub summary: ProgressSummary,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProgressSummary {
    pub total_rules: u32,
    pub satisfied_rules: u32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SectionProgress {
    pub heading: String,
    pub subsections: Vec<SubsectionProgress>,
    pub satisfied: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SubsectionProgress {
    pub heading: String,
    pub groups: Vec<GroupProgress>,
    pub satisfied: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GroupProgress {
    pub heading: Option<String>,
    pub rules: Vec<RuleProgress>,
    pub satisfied: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RuleProgress {
    pub rule_type: RuleType,
    pub heading: Option<String>,
    pub satisfied: bool,
    pub completed_courses: Vec<String>,
    pub remaining_courses: Vec<String>,
    pub alt_completed: Vec<String>,
    pub alt_remaining: Vec<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GeProgress {
    pub required: Vec<(String, String, bool)>, // (code, name, satisfied)
    pub perspective_satisfied: bool,
    pub perspective_completed: Option<String>,
    pub practice_satisfied: bool,
    pub practice_completed: Option<String>,
    pub composition_satisfied: bool,
}

// ─── Progress Checking ───

pub fn check_progress(
    reqs: &DegreeRequirements,
    completed_courses: &[String],
    completed_ge: Option<&[String]>,
) -> ProgressReport {
    let completed_set: HashSet<String> = completed_courses
        .iter()
        .map(|c| normalize_course_code(c))
        .collect();

    let mut total_rules = 0u32;
    let mut satisfied_rules = 0u32;

    let sections: Vec<SectionProgress> = reqs
        .sections
        .iter()
        .map(|s| {
            let sp = check_section(s, &completed_set);
            for sub in &sp.subsections {
                for group in &sub.groups {
                    for rule in &group.rules {
                        total_rules += 1;
                        if rule.satisfied {
                            satisfied_rules += 1;
                        }
                    }
                }
            }
            sp
        })
        .collect();

    let ge_progress = reqs
        .general_education
        .as_ref()
        .map(|ge| check_ge_progress(ge, completed_ge));

    ProgressReport {
        program_name: reqs.program_name.clone(),
        sections,
        ge_progress,
        breadth_progress: None, // Set by DegreeService for programs that have breadth reqs
        summary: ProgressSummary {
            total_rules,
            satisfied_rules,
        },
    }
}

fn check_section(section: &RequirementSection, completed: &HashSet<String>) -> SectionProgress {
    let subsections: Vec<SubsectionProgress> = section
        .subsections
        .iter()
        .map(|s| check_subsection(s, completed))
        .collect();

    let satisfied = subsections.iter().all(|s| s.satisfied);

    SectionProgress {
        heading: section.heading.clone(),
        subsections,
        satisfied,
    }
}

fn check_subsection(
    subsection: &RequirementSubsection,
    completed: &HashSet<String>,
) -> SubsectionProgress {
    let groups: Vec<GroupProgress> = subsection
        .groups
        .iter()
        .map(|g| check_group(g, completed))
        .collect();

    let satisfied = groups.iter().all(|g| g.satisfied);

    SubsectionProgress {
        heading: subsection.heading.clone(),
        groups,
        satisfied,
    }
}

fn check_group(group: &RequirementGroup, completed: &HashSet<String>) -> GroupProgress {
    let rules: Vec<RuleProgress> = group
        .rules
        .iter()
        .map(|r| evaluate_rule(r, completed))
        .collect();

    let satisfied = rules.iter().all(|r| r.satisfied);

    GroupProgress {
        heading: group.heading.clone(),
        rules,
        satisfied,
    }
}

fn evaluate_rule(rule: &CourseRule, completed_set: &HashSet<String>) -> RuleProgress {
    let completed: Vec<String> = rule
        .courses
        .iter()
        .filter(|c| completed_set.contains(&normalize_course_code(&c.code)))
        .map(|c| c.code.clone())
        .collect();

    let remaining: Vec<String> = rule
        .courses
        .iter()
        .filter(|c| !completed_set.contains(&normalize_course_code(&c.code)))
        .map(|c| c.code.clone())
        .collect();

    let (alt_completed, alt_remaining) = if let Some(ref alt) = rule.alternative {
        let ac: Vec<String> = alt
            .iter()
            .filter(|c| completed_set.contains(&normalize_course_code(&c.code)))
            .map(|c| c.code.clone())
            .collect();
        let ar: Vec<String> = alt
            .iter()
            .filter(|c| !completed_set.contains(&normalize_course_code(&c.code)))
            .map(|c| c.code.clone())
            .collect();
        (ac, ar)
    } else {
        (Vec::new(), Vec::new())
    };

    let satisfied = match &rule.rule_type {
        RuleType::AllOf => remaining.is_empty(),
        RuleType::OneOf => !completed.is_empty(),
        RuleType::NOf(n) => completed.len() >= *n as usize,
        RuleType::EitherOr => {
            let primary_done = remaining.is_empty();
            let alt_done = alt_remaining.is_empty()
                && rule.alternative.as_ref().is_some_and(|a| !a.is_empty());
            primary_done || alt_done
        }
        RuleType::CreditsFrom(n) => {
            let completed_credits: u32 = rule
                .courses
                .iter()
                .filter(|c| completed_set.contains(&normalize_course_code(&c.code)))
                .filter_map(|c| c.credits)
                .sum();
            completed_credits >= *n
        }
        RuleType::Prose => false,
    };

    RuleProgress {
        rule_type: rule.rule_type.clone(),
        heading: rule.heading.clone(),
        satisfied,
        completed_courses: completed,
        remaining_courses: remaining,
        alt_completed,
        alt_remaining,
        description: rule.description.clone(),
    }
}

fn check_ge_progress(ge: &GeRequirements, completed_ge: Option<&[String]>) -> GeProgress {
    let completed: HashSet<String> = completed_ge
        .unwrap_or(&[])
        .iter()
        .map(|s| s.trim().to_uppercase().replace('.', "").replace('-', "-"))
        .collect();

    let required: Vec<(String, String, bool)> = ge
        .required
        .iter()
        .map(|area| {
            (
                area.code.clone(),
                area.name.clone(),
                completed.contains(&area.code),
            )
        })
        .collect();

    let perspective_completed = ge
        .perspectives
        .iter()
        .find(|a| completed.contains(&a.code))
        .map(|a| a.code.clone());

    let practice_completed = ge
        .practice
        .iter()
        .find(|a| completed.contains(&a.code))
        .map(|a| a.code.clone());

    GeProgress {
        required,
        perspective_satisfied: perspective_completed.is_some(),
        perspective_completed,
        practice_satisfied: practice_completed.is_some(),
        practice_completed,
        composition_satisfied: completed.contains("C"),
    }
}

/// Normalize a course code for matching.
/// "CSE  12" -> "CSE 12", "cse12" -> "CSE 12", "math19a" -> "MATH 19A"
pub fn normalize_course_code(code: &str) -> String {
    let trimmed = code.trim().to_uppercase();

    // Split into alphabetic prefix and rest
    let alpha_end = trimmed
        .find(|c: char| !c.is_alphabetic())
        .unwrap_or(trimmed.len());
    let prefix = &trimmed[..alpha_end];
    let rest = trimmed[alpha_end..].trim();

    if rest.is_empty() {
        return trimmed;
    }

    format!("{} {}", prefix, rest)
}

// ─── Markdown rendering ───

impl ProgressReport {
    pub fn format(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "# Degree Progress: {}\n", self.program_name);
        let _ = writeln!(
            out,
            "**Overall: {}/{} requirement groups satisfied**\n",
            self.summary.satisfied_rules, self.summary.total_rules,
        );

        for section in &self.sections {
            out.push_str(&section.format());
        }

        if let Some(breadth) = self.breadth_progress.as_ref() {
            out.push('\n');
            out.push_str(&breadth.format());
        }

        if let Some(ge) = self.ge_progress.as_ref() {
            out.push_str("## General Education Progress\n\n");
            out.push_str(&ge.format());
        }

        out
    }
}

impl SectionProgress {
    pub fn format(&self) -> String {
        let mut out = String::new();
        let status = if self.satisfied { "COMPLETE" } else { "IN PROGRESS" };
        if !self.heading.is_empty() {
            let _ = writeln!(out, "## {} [{}]\n", self.heading, status);
        }
        for sub in &self.subsections {
            out.push_str(&sub.format());
        }
        out
    }
}

impl SubsectionProgress {
    pub fn format(&self) -> String {
        let mut out = String::new();
        if !self.heading.is_empty() {
            let satisfied_count = self.groups.iter().filter(|g| g.satisfied).count();
            let total = self.groups.len();
            let _ = writeln!(out, "### {} ({}/{})\n", self.heading, satisfied_count, total);
        }
        for group in &self.groups {
            out.push_str(&group.format());
        }
        out
    }
}

impl GroupProgress {
    pub fn format(&self) -> String {
        let mut out = String::new();
        if let Some(heading) = self.heading.as_ref() {
            let status = if self.satisfied { "SATISFIED" } else { "INCOMPLETE" };
            let _ = writeln!(out, "#### {} [{}]\n", heading, status);
        }
        for rule in &self.rules {
            out.push_str(&rule.format());
        }
        out
    }
}

impl RuleProgress {
    pub fn format(&self) -> String {
        let mut out = String::new();
        // Show rule heading if present
        if let Some(heading) = self.heading.as_ref() {
            if self.satisfied {
                let _ = writeln!(out, "**{} SATISFIED**", heading);
            } else {
                let _ = writeln!(out, "**{}**", heading);
            }
        }

        if self.rule_type == RuleType::Prose {
            if let Some(desc) = self.description.as_ref() {
                let _ = writeln!(out, "- [ ] *{} (manual review needed)*", desc);
            }
            return out;
        }

        // Show courses with checkboxes
        for course in &self.completed_courses {
            let _ = writeln!(out, "- [x] {}", course);
        }
        for course in &self.remaining_courses {
            let _ = writeln!(out, "- [ ] {}", course);
        }

        // Show alternative group for either/or
        if self.rule_type == RuleType::EitherOr && !self.alt_completed.is_empty()
            || !self.alt_remaining.is_empty()
        {
            out.push_str("\n**Or:**\n");
            for course in &self.alt_completed {
                let _ = writeln!(out, "- [x] {}", course);
            }
            for course in &self.alt_remaining {
                let _ = writeln!(out, "- [ ] {}", course);
            }
        }

        out.push('\n');
        out
    }
}

impl GeProgress {
    pub fn format(&self) -> String {
        let mut out = String::from("**Required areas:**\n");
        for (code, name, satisfied) in &self.required {
            let check = if *satisfied { "x" } else { " " };
            let _ = writeln!(out, "- [{}] {} — {}", check, code, name);
        }

        out.push_str("\n**Perspectives (need 1):**\n");
        if self.perspective_satisfied {
            let _ = writeln!(
                out,
                "- [x] Satisfied ({})",
                self.perspective_completed.as_deref().unwrap_or("?")
            );
        } else {
            out.push_str("- [ ] Not yet satisfied (choose PE-E, PE-H, or PE-T)\n");
        }

        out.push_str("\n**Practice (need 1):**\n");
        if self.practice_satisfied {
            let _ = writeln!(
                out,
                "- [x] Satisfied ({})",
                self.practice_completed.as_deref().unwrap_or("?")
            );
        } else {
            out.push_str("- [ ] Not yet satisfied (choose PR-E, PR-C, or PR-S)\n");
        }

        let check = if self.composition_satisfied { "x" } else { " " };
        let _ = writeln!(out, "\n**Composition:**\n- [{}] C — Composition", check);

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::degrees::scraper::{Course, GeArea, GeRequirements};

    fn course(code: &str) -> Course {
        Course {
            code: code.into(),
            title: format!("Test {}", code),
            credits: Some(5),
            url: None,
            cross_listed: None,
        }
    }

    #[test]
    fn test_normalize_course_code() {
        assert_eq!(normalize_course_code("CSE 12"), "CSE 12");
        assert_eq!(normalize_course_code("cse12"), "CSE 12");
        assert_eq!(normalize_course_code("  CSE  12  "), "CSE 12");
        assert_eq!(normalize_course_code("MATH 19A"), "MATH 19A");
        assert_eq!(normalize_course_code("math19a"), "MATH 19A");
    }

    #[test]
    fn test_all_of_satisfied() {
        let rule = CourseRule {
            rule_type: RuleType::AllOf,
            heading: None,
            courses: vec![course("CSE 12"), course("CSE 16")],
            alternative: None,
            description: None,
        };
        let completed: HashSet<String> = ["CSE 12", "CSE 16"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let progress = evaluate_rule(&rule, &completed);
        assert!(progress.satisfied);
        assert!(progress.remaining_courses.is_empty());
    }

    #[test]
    fn test_all_of_not_satisfied() {
        let rule = CourseRule {
            rule_type: RuleType::AllOf,
            heading: None,
            courses: vec![course("CSE 12"), course("CSE 16")],
            alternative: None,
            description: None,
        };
        let completed: HashSet<String> = ["CSE 12"].iter().map(|s| s.to_string()).collect();
        let progress = evaluate_rule(&rule, &completed);
        assert!(!progress.satisfied);
        assert_eq!(progress.remaining_courses, vec!["CSE 16"]);
    }

    #[test]
    fn test_one_of_satisfied() {
        let rule = CourseRule {
            rule_type: RuleType::OneOf,
            heading: None,
            courses: vec![course("AM 10"), course("MATH 21")],
            alternative: None,
            description: None,
        };
        let completed: HashSet<String> = ["MATH 21"].iter().map(|s| s.to_string()).collect();
        let progress = evaluate_rule(&rule, &completed);
        assert!(progress.satisfied);
    }

    #[test]
    fn test_n_of_satisfied() {
        let rule = CourseRule {
            rule_type: RuleType::NOf(2),
            heading: None,
            courses: vec![course("CSE 101"), course("CSE 102"), course("CSE 103"), course("CSE 104")],
            alternative: None,
            description: None,
        };
        let completed: HashSet<String> = ["CSE 101", "CSE 103"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let progress = evaluate_rule(&rule, &completed);
        assert!(progress.satisfied);
    }

    #[test]
    fn test_either_or_primary_satisfied() {
        let rule = CourseRule {
            rule_type: RuleType::EitherOr,
            heading: None,
            courses: vec![course("MATH 19A"), course("MATH 19B")],
            alternative: Some(vec![course("MATH 20A"), course("MATH 20B")]),
            description: None,
        };
        let completed: HashSet<String> = ["MATH 19A", "MATH 19B"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let progress = evaluate_rule(&rule, &completed);
        assert!(progress.satisfied);
    }

    #[test]
    fn test_either_or_alternative_satisfied() {
        let rule = CourseRule {
            rule_type: RuleType::EitherOr,
            heading: None,
            courses: vec![course("MATH 19A"), course("MATH 19B")],
            alternative: Some(vec![course("MATH 20A"), course("MATH 20B")]),
            description: None,
        };
        let completed: HashSet<String> = ["MATH 20A", "MATH 20B"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let progress = evaluate_rule(&rule, &completed);
        assert!(progress.satisfied);
    }

    #[test]
    fn test_either_or_neither_satisfied() {
        let rule = CourseRule {
            rule_type: RuleType::EitherOr,
            heading: None,
            courses: vec![course("MATH 19A"), course("MATH 19B")],
            alternative: Some(vec![course("MATH 20A"), course("MATH 20B")]),
            description: None,
        };
        let completed: HashSet<String> = ["MATH 19A", "MATH 20A"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let progress = evaluate_rule(&rule, &completed);
        assert!(!progress.satisfied);
    }

    #[test]
    fn test_credits_from_satisfied() {
        let rule = CourseRule {
            rule_type: RuleType::CreditsFrom(10),
            heading: None,
            courses: vec![course("CSE 101"), course("CSE 102"), course("CSE 103")],
            alternative: None,
            description: None,
        };
        let completed: HashSet<String> = ["CSE 101", "CSE 103"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let progress = evaluate_rule(&rule, &completed);
        assert!(progress.satisfied); // 5 + 5 = 10 >= 10
    }

    #[test]
    fn test_prose_never_satisfied() {
        let rule = CourseRule {
            rule_type: RuleType::Prose,
            heading: None,
            courses: Vec::new(),
            alternative: None,
            description: Some("At most two courses from AM/STAT/MATH".into()),
        };
        let completed: HashSet<String> = HashSet::new();
        let progress = evaluate_rule(&rule, &completed);
        assert!(!progress.satisfied);
    }

    #[test]
    fn test_ge_progress() {
        let ge = GeRequirements {
            required: vec![
                GeArea { code: "CC".into(), name: "Cross-Cultural Analysis".into(), credits: 5 },
                GeArea { code: "ER".into(), name: "Ethnicity and Race".into(), credits: 5 },
            ],
            perspectives: vec![
                GeArea { code: "PE-E".into(), name: "Environmental".into(), credits: 5 },
                GeArea { code: "PE-H".into(), name: "Human Behavior".into(), credits: 5 },
                GeArea { code: "PE-T".into(), name: "Technology".into(), credits: 5 },
            ],
            practice: vec![
                GeArea { code: "PR-E".into(), name: "Collaborative".into(), credits: 2 },
                GeArea { code: "PR-C".into(), name: "Creative".into(), credits: 2 },
                GeArea { code: "PR-S".into(), name: "Service".into(), credits: 2 },
            ],
            composition: GeArea { code: "C".into(), name: "Composition".into(), credits: 5 },
        };

        let completed = vec!["CC".to_string(), "PE-H".to_string(), "C".to_string()];
        let progress = check_ge_progress(&ge, Some(&completed));

        assert!(progress.required[0].2); // CC satisfied
        assert!(!progress.required[1].2); // ER not satisfied
        assert!(progress.perspective_satisfied);
        assert_eq!(progress.perspective_completed.as_deref(), Some("PE-H"));
        assert!(!progress.practice_satisfied);
        assert!(progress.composition_satisfied);
    }
}
