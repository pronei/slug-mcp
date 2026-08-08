pub mod scraper;

use std::sync::Arc;

use anyhow::{Result, bail};
use chrono::Datelike;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiningMenuRequest {
    /// Dining hall name (e.g., "Crown", "Porter", "College Nine"). If omitted, returns all halls.
    pub hall: Option<String>,
    /// Meal period: "breakfast", "lunch", "dinner", or "late night". If omitted, returns all meals.
    pub meal: Option<String>,
    /// Date in YYYY-MM-DD format (e.g., "2026-03-19"). If omitted, returns today's menu.
    pub date: Option<String>,
    /// Set to true to include all categories (condiments, beverages, cereal, etc.). Default: only main food items.
    pub include_all_categories: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NutritionRequest {
    /// Recipe ID from the menu (e.g., "061002*3"). Get this from get_dining_menu output.
    pub recipe_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiningHoursRequest {
    /// Location name to filter by (e.g., "Crown", "Porter"). If omitted, returns all locations.
    pub location: Option<String>,
}

use std::collections::HashMap;
use std::time::Duration;

use crate::cache::CacheStore;
use crate::util::FuzzyMatcher;
#[cfg(feature = "auth")]
use scraper::{BalanceResult, MealBalance, scrape_balance};
use scraper::{
    DINING_HALLS, DiningLocation, HallKind, find_hall, hall_names, scrape_hours, scrape_menu,
    scrape_nutrition,
};

/// Category names we hide by default (condiments, beverages, etc.) so the menu
/// surfaces actual food. Matched fuzzily — case-insensitive, whitespace-collapsed
/// — so minor upstream renames don't break the filter silently.
const FILTERED_CATEGORIES: &[&str] = &[
    "condiments",
    "all day",
    "beverages",
    "bread & bagels",
    "bread and bagels",
    "cereal",
];

pub struct DiningService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl DiningService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    /// Residential halls shut on `date`, keyed by `location_num`, with a
    /// human-readable span ("closed through 2026-09-17").
    ///
    /// Between quarters UCSC consolidates dining into a single hall, but the
    /// nutrition site keeps publishing menus for the dark ones — on 2026-08-07
    /// all five halls returned full, distinct breakfast/lunch/dinner menus
    /// while only Cowell/Stevenson was operating — so the menu feed cannot
    /// tell us who is open. The hours page can: it marks a shutdown as a
    /// vacation range carrying no serving hours.
    ///
    /// Returns `None` when the hours page is unavailable or nothing is closed,
    /// so every caller degrades to the all-halls behaviour rather than hiding
    /// menus on a bad scrape.
    async fn closed_halls(&self, date: &str) -> Option<HashMap<&'static str, String>> {
        let http = &self.http;
        let locations: Vec<DiningLocation> = self
            .cache
            .get_or_fetch("dining:hours", 21600, || scrape_hours(http))
            .await
            .map_err(|e| tracing::warn!("closure check unavailable, assuming all halls open: {e}"))
            .ok()?;

        let closed: HashMap<&'static str, String> = locations
            .iter()
            .filter(|loc| loc.is_dining_hall())
            .filter_map(|loc| {
                let closure = loc.closure_covering(date)?;
                let hall = find_hall(&loc.name)?;
                Some((hall.location_num, closure.closure_label()))
            })
            .collect();

        (!closed.is_empty()).then_some(closed)
    }

    pub async fn get_menu(
        &self,
        hall: Option<&str>,
        meal: Option<&str>,
        date: Option<&str>,
        include_all: bool,
    ) -> Result<String> {
        // Convert ISO date (YYYY-MM-DD) to M/D/YYYY for the nutrition site.
        // Cache key always uses the canonical ISO date for consistency. A
        // missing date canonicalizes to *today in Pacific* (not a literal
        // "today" key) so the key matches what the 5 AM pre-warmer writes and
        // rolls over at midnight instead of serving yesterday's menu.
        let (iso_date, formatted_date) = match date {
            Some(d) => {
                if let Ok(parsed) = chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d") {
                    let iso = d.to_string();
                    let formatted =
                        format!("{}/{}/{}", parsed.month(), parsed.day(), parsed.year());
                    (iso, Some(formatted))
                } else {
                    // Assume it's already in M/D/YYYY — store raw as cache key
                    (d.to_string(), Some(d.to_string()))
                }
            }
            None => {
                let now = crate::util::now_pacific();
                (now.format("%Y-%m-%d").to_string(), None)
            }
        };
        let scraper_date = formatted_date.as_deref();
        let cache_date = iso_date.as_str();

        let closed = self.closed_halls(cache_date).await.unwrap_or_default();

        // An explicit hall request is always honoured — the caller asked for
        // that hall by name, so return its menu and flag the closure rather
        // than answering with nothing. Only the implicit "all halls" fan-out
        // drops closed halls, which is what keeps stale between-quarter menus
        // out of the default answer.
        let mut skipped: Vec<String> = Vec::new();
        let halls: Vec<&scraper::DiningHall> = if let Some(hall_query) = hall {
            let hall = find_hall(hall_query).ok_or_else(|| {
                anyhow::anyhow!(
                    "Dining hall '{}' not found. Available halls: {}",
                    hall_query,
                    hall_names()
                )
            })?;
            vec![hall]
        } else {
            DINING_HALLS
                .iter()
                .filter(|h| h.kind == HallKind::Full)
                .filter(|h| match closed.get(h.location_num) {
                    Some(span) => {
                        skipped.push(format!("{} ({})", h.name, span));
                        false
                    }
                    None => true,
                })
                .collect()
        };

        let futures: Vec<_> = halls
            .iter()
            .map(|hall| {
                let cache_key = format!("dining:menu:{}:{}", hall.location_num, cache_date);
                let cache = &self.cache;
                let http = &self.http;
                async move {
                    cache
                        .get_or_fetch(&cache_key, 3600, || scrape_menu(http, hall, scraper_date))
                        .await
                }
            })
            .collect();
        let mut menus: Vec<_> = futures_util::future::join_all(futures)
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()?;

        // Filter by meal if specified
        if let Some(meal_filter) = meal {
            let filter = meal_filter.to_lowercase();
            for menu in &mut menus {
                menu.meals
                    .retain(|m| m.name.to_lowercase().contains(&filter));
            }
        }

        // Filter out noisy categories (condiments, beverages, etc.) by default
        if !include_all {
            let denylist = FuzzyMatcher::new(FILTERED_CATEGORIES.iter().copied())
                .case_insensitive()
                .whitespace_collapsed();
            for menu in &mut menus {
                for meal in &mut menu.meals {
                    meal.categories.retain(|c| !denylist.matches(&c.name));
                }
            }
        }

        let output: String = menus
            .iter()
            .map(|m| m.format())
            .collect::<Vec<_>>()
            .join("\n---\n\n");

        if output.trim().is_empty() {
            if !skipped.is_empty() {
                bail!(
                    "No dining halls are open on {}. Closed: {}.",
                    cache_date,
                    skipped.join("; ")
                );
            }
            bail!("No menu data available. The nutrition site may be temporarily down.");
        }

        // Warn when a caller explicitly asked for a hall that isn't serving:
        // the nutrition site still returns a plausible-looking menu for it.
        let mut prefix = String::new();
        for hall in &halls {
            if let Some(span) = closed.get(hall.location_num) {
                prefix.push_str(&format!(
                    "> **{}** is {} — the menu below is published upstream but the hall is not serving.\n\n",
                    hall.name, span
                ));
            }
        }

        let mut output = format!("{prefix}{output}");
        if !skipped.is_empty() {
            output.push_str(&format!(
                "\n_Not shown (closed): {}._\n",
                skipped.join("; ")
            ));
        }
        Ok(output)
    }

    pub async fn get_nutrition(&self, recipe_id: &str) -> Result<String> {
        let info = scrape_nutrition(&self.http, recipe_id).await?;
        Ok(info.format())
    }

    pub async fn get_hours(&self, location: Option<&str>) -> Result<String> {
        let http = &self.http;
        let locations: Vec<DiningLocation> = self
            .cache
            .get_or_fetch("dining:hours", 21600, || scrape_hours(http))
            .await?;

        let today = crate::util::now_pacific().format("%Y-%m-%d").to_string();

        let filtered: Vec<&DiningLocation> = if let Some(query) = location {
            let q = query.to_lowercase();
            locations
                .iter()
                .filter(|l| l.name.to_lowercase().contains(&q))
                .collect()
        } else {
            locations.iter().collect()
        };

        if filtered.is_empty() {
            if let Some(query) = location {
                bail!("Location '{}' not found.", query);
            }
            bail!("No hours data available.");
        }

        let output: String = filtered
            .iter()
            .map(|l| l.format_with_date(&today))
            .collect::<Vec<_>>()
            .join("");
        Ok(format!("# UCSC Dining Hours\n\n{}", output))
    }

    #[cfg(feature = "auth")]
    pub async fn get_balance(
        &self,
        auth_client: &reqwest::Client,
        session_key: &str,
    ) -> Result<BalanceResult> {
        // Balance is per-user, so the cache key MUST include a session
        // discriminator — a global key would serve one user's balance to
        // another in SSE mode (multiple authenticated sessions per process).
        // Conditional caching (only on success): a debug_snippet means the
        // parse failed and we want to refetch next time, so no get_or_fetch.
        let cache_key = format!("dining:balance:{session_key}");
        if let Some(balance) = self.cache.get::<MealBalance>(&cache_key).await {
            return Ok(BalanceResult {
                balance,
                debug_snippet: None,
            });
        }

        let result = scrape_balance(auth_client).await?;

        if result.debug_snippet.is_none() {
            self.cache
                .set(
                    &cache_key,
                    result.balance.clone(),
                    std::time::Duration::from_secs(300),
                )
                .await;
        }

        Ok(result)
    }
}

/// Spawns a background task that pre-warms the dining menu cache daily at 5:00 AM Pacific.
pub fn start_cache_refresher(
    http: reqwest::Client,
    cache: Arc<CacheStore>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Shares the caller's cache, so the closure lookup reuses whatever the
        // `dining:hours` entry already holds instead of refetching each night.
        let service = DiningService::new(http.clone(), cache.clone());
        loop {
            let delay = duration_until_next_5am();
            tracing::info!(
                "Next dining cache refresh in {}h {}m",
                delay.as_secs() / 3600,
                (delay.as_secs() % 3600) / 60
            );
            tokio::time::sleep(delay).await;

            let now = crate::util::now_pacific();
            let scraper_date = format!("{}/{}/{}", now.month(), now.day(), now.year());
            let iso_date = now.format("%Y-%m-%d").to_string();

            // Don't pre-warm halls that aren't serving — between quarters that
            // is four of the five, each costing a shortmenu plus one longmenu
            // per meal. Reuses the same fail-open closure lookup as get_menu.
            let closed = service.closed_halls(&iso_date).await.unwrap_or_default();

            for hall in DINING_HALLS
                .iter()
                .filter(|h| h.kind == HallKind::Full)
                .filter(|h| !closed.contains_key(h.location_num))
            {
                match scrape_menu(&http, hall, Some(&scraper_date)).await {
                    Ok(menu) => {
                        let key = format!("dining:menu:{}:{}", hall.location_num, iso_date);
                        cache.set(&key, menu, Duration::from_secs(3600)).await;
                        tracing::info!("Refreshed dining cache for {}", hall.name);
                    }
                    Err(e) => tracing::warn!("Cache refresh failed for {}: {}", hall.name, e),
                }
            }
        }
    })
}

fn duration_until_next_5am() -> Duration {
    let now = crate::util::now_pacific();
    let today_5am = now.date_naive().and_hms_opt(5, 0, 0).unwrap();
    let next_5am = if now.time() < chrono::NaiveTime::from_hms_opt(5, 0, 0).unwrap() {
        today_5am
    } else {
        today_5am + chrono::Duration::days(1)
    };
    // 5 AM Pacific is never ambiguous (DST transitions happen at 2 AM); on the
    // off chance chrono returns Ambiguous/None, fall back to 1h to retry.
    let pacific_next = next_5am.and_local_timezone(chrono_tz::US::Pacific).single();
    match pacific_next {
        Some(t) => (t - now).to_std().unwrap_or(Duration::from_secs(3600)),
        None => {
            tracing::warn!("ambiguous Pacific time at 5 AM; retrying in 1h");
            Duration::from_secs(3600)
        }
    }
}
