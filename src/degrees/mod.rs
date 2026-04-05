pub mod breadth;
pub mod programs;
pub mod progress;
pub mod scraper;

use std::sync::Arc;

use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::cache::CacheStore;
use breadth::{scrape_breadth_requirements, BreadthRequirements};
use programs::{resolve_program, ProgramIndex};
use scraper::{scrape_program_list, scrape_requirements, DegreeType};

const BACHELORS_URL: &str =
    "https://catalog.ucsc.edu/en/current/general-catalog/academic-programs/bachelors-degrees/";
const MASTERS_URL: &str =
    "https://catalog.ucsc.edu/en/current/general-catalog/academic-programs/masters-degrees/";
const CATALOG_BASE: &str = "https://catalog.ucsc.edu";

const CACHE_TTL: u64 = 86400; // 24 hours

/// Slugs of programs that have breadth requirements from the CSE grad page.
const CSE_MS_SLUG: &str = "computer-science-and-engineering-ms";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DegreeRequirementsRequest {
    /// Program name or slug (e.g., "Computer Science BS", "computer-science-bs",
    /// "Applied Mathematics MS"). Case-insensitive, partial matches accepted.
    pub program: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DegreeProgressRequest {
    /// Program name or slug (same as get_degree_requirements).
    pub program: String,
    /// List of completed course codes (e.g., ["CSE 12", "CSE 30", "MATH 19A"]).
    /// Whitespace-insensitive.
    pub completed_courses: Vec<String>,
    /// Completed GE area codes (e.g., ["CC", "ER", "MF"]). Optional.
    pub completed_ge: Option<Vec<String>>,
}

pub struct DegreeService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl DegreeService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    async fn load_program_index(&self) -> Result<ProgramIndex> {
        let http = self.http.clone();
        self.cache
            .get_or_fetch("degrees:program_index", CACHE_TTL, || async {
                let bachelors = scrape_program_list(&http, BACHELORS_URL).await?;
                let masters = scrape_program_list(&http, MASTERS_URL).await?;
                Ok(ProgramIndex { bachelors, masters })
            })
            .await
    }

    async fn load_breadth_requirements(&self) -> Result<BreadthRequirements> {
        let http = self.http.clone();
        self.cache
            .get_or_fetch("degrees:cse_breadth", CACHE_TTL, || {
                scrape_breadth_requirements(&http)
            })
            .await
    }

    fn has_breadth_requirements(slug: &str) -> bool {
        slug == CSE_MS_SLUG
    }

    pub async fn get_requirements(&self, program_query: &str) -> Result<String> {
        let index = self.load_program_index().await?;
        let entry = resolve_program(program_query, &index)?;

        let url = format!("{}{}", CATALOG_BASE, entry.url_path);
        let cache_key = format!("degrees:requirements:{}", entry.slug);
        let http = self.http.clone();
        let program_name = entry.name.clone();
        let degree_type = entry.degree_type.clone();

        let reqs = self
            .cache
            .get_or_fetch(&cache_key, CACHE_TTL, || {
                scrape_requirements(&http, &url, &program_name, &degree_type)
            })
            .await?;

        let mut output = reqs.to_string();

        // Append breadth requirements for CSE MS
        if Self::has_breadth_requirements(&entry.slug) {
            if let Ok(breadth) = self.load_breadth_requirements().await {
                output.push_str(&format!("\n{}", breadth));
            }
        }

        Ok(output)
    }

    pub async fn check_progress(
        &self,
        program_query: &str,
        completed_courses: &[String],
        completed_ge: Option<&[String]>,
    ) -> Result<String> {
        let index = self.load_program_index().await?;
        let entry = resolve_program(program_query, &index)?;

        let url = format!("{}{}", CATALOG_BASE, entry.url_path);
        let cache_key = format!("degrees:requirements:{}", entry.slug);
        let http = self.http.clone();
        let program_name = entry.name.clone();
        let degree_type = entry.degree_type.clone();

        let reqs = self
            .cache
            .get_or_fetch(&cache_key, CACHE_TTL, || {
                scrape_requirements(&http, &url, &program_name, &degree_type)
            })
            .await?;

        let mut report = progress::check_progress(&reqs, completed_courses, completed_ge);

        // Append breadth progress for CSE MS
        if Self::has_breadth_requirements(&entry.slug) {
            if let Ok(breadth_reqs) = self.load_breadth_requirements().await {
                let breadth_progress =
                    breadth::check_breadth_progress(&breadth_reqs, completed_courses);
                report.breadth_progress = Some(breadth_progress);
            }
        }

        Ok(report.to_string())
    }
}
