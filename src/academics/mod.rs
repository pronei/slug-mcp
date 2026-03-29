pub mod scraper;

use std::sync::Arc;

use anyhow::Result;

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
            career: None,
            page_start,
            page_size: 25,
        };

        // Build cache key from params
        let cache_key = format!(
            "academics:classes:{}:{}:{}:{}:{}:{}:{}:{}",
            term_code,
            subject.unwrap_or(""),
            catalog_number.unwrap_or(""),
            instructor.unwrap_or(""),
            title.unwrap_or(""),
            ge.unwrap_or(""),
            if open_only { "open" } else { "all" },
            page_start,
        );

        if let Some(cached) = self.cache.get(&cache_key).await {
            if let Ok(result) = serde_json::from_str::<ClassSearchResult>(&cached) {
                return Ok(result.to_string());
            }
        }

        let result = scrape_class_search(&self.http, &params).await?;

        if result.classes.is_empty() {
            return Ok("No classes found matching your search criteria.".to_string());
        }

        if let Ok(json) = serde_json::to_string(&result) {
            self.cache.set(&cache_key, &json, 1800).await; // 30 minutes
        }

        Ok(result.to_string())
    }

    pub async fn search_directory(
        &self,
        query: &str,
        search_type: Option<&str>,
    ) -> Result<String> {
        let stype = search_type.unwrap_or("people");
        let cache_key = format!("academics:directory:{}:{}", stype, query.to_lowercase());

        if let Some(cached) = self.cache.get(&cache_key).await {
            if let Ok(result) = serde_json::from_str::<DirectoryResult>(&cached) {
                return Ok(result.to_string());
            }
        }

        let result = scrape_directory(&self.http, query, stype).await?;

        if result.entries.is_empty() {
            return Ok(format!("No directory results found for \"{}\".", query));
        }

        if let Ok(json) = serde_json::to_string(&result) {
            self.cache.set(&cache_key, &json, 21600).await; // 6 hours
        }

        Ok(result.to_string())
    }
}
