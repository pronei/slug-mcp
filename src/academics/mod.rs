pub mod scraper;

use std::sync::Arc;

use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchClassesRequest {
    /// Term code (e.g., "2262" for Spring 2026). If omitted, uses current term.
    pub term: Option<String>,
    /// Subject/department code (e.g., "CSE", "MATH", "PHYS").
    pub subject: Option<String>,
    /// Course catalog number (e.g., "115A", "19A").
    pub course_number: Option<String>,
    /// Instructor last name.
    pub instructor: Option<String>,
    /// Course title keyword.
    pub title: Option<String>,
    /// General Education requirement code.
    pub ge: Option<String>,
    /// Academic career: "UGRD" for undergraduate, "GRAD" for graduate. If omitted, searches all.
    pub career: Option<String>,
    /// If true, only show open classes. Default: show all.
    pub open_only: Option<bool>,
    /// Page number for pagination (25 results per page). Default: 0.
    pub page: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchDirectoryRequest {
    /// Search query (name, department, etc.)
    pub query: String,
    /// Search type: "people" (default) or "departments".
    pub search_type: Option<String>,
}

use crate::cache::CacheStore;
use scraper::{
    current_term_code, scrape_class_search, scrape_directory, ClassSearchParams, ClassSearchResult,
    DirectoryResult,
};

pub struct AcademicsService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl AcademicsService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn search_classes(
        &self,
        term: Option<&str>,
        subject: Option<&str>,
        catalog_number: Option<&str>,
        instructor: Option<&str>,
        title: Option<&str>,
        ge: Option<&str>,
        career: Option<&str>,
        open_only: bool,
        page: Option<u32>,
    ) -> Result<String> {
        let term_code = term
            .map(|t| t.to_string())
            .unwrap_or_else(current_term_code);

        let page_start = page.unwrap_or(0) * 25;

        let params = ClassSearchParams {
            term: term_code.clone(),
            subject: subject.map(|s| s.to_uppercase()),
            catalog_number: catalog_number.map(|s| s.to_string()),
            instructor: instructor.map(|s| s.to_string()),
            title: title.map(|s| s.to_string()),
            ge: ge.map(|s| s.to_string()),
            reg_status: if open_only { "O".to_string() } else { "all".to_string() },
            career: career.map(|s| s.to_uppercase()),
            page_start,
            page_size: 25,
        };

        // Build cache key from params
        let cache_key = format!(
            "academics:classes:{}:{}:{}:{}:{}:{}:{}:{}:{}",
            term_code,
            subject.unwrap_or(""),
            catalog_number.unwrap_or(""),
            instructor.unwrap_or(""),
            title.unwrap_or(""),
            ge.unwrap_or(""),
            career.unwrap_or(""),
            if open_only { "open" } else { "all" },
            page_start,
        );

        let http = &self.http;
        let result: ClassSearchResult = self
            .cache
            .get_or_fetch(&cache_key, 1800, || scrape_class_search(http, &params))
            .await?;

        if result.classes.is_empty() {
            return Ok("No classes found matching your search criteria.".to_string());
        }

        Ok(result.format())
    }

    pub async fn search_directory(
        &self,
        query: &str,
        search_type: Option<&str>,
    ) -> Result<String> {
        let stype = search_type.unwrap_or("people");
        let cache_key = format!("academics:directory:{}:{}", stype, query.to_lowercase());

        let http = &self.http;
        let result: DirectoryResult = self
            .cache
            .get_or_fetch(&cache_key, 21600, || scrape_directory(http, query, stype))
            .await?;

        if result.entries.is_empty() {
            return Ok(format!("No directory results found for \"{}\".", query));
        }

        Ok(result.format())
    }
}
