pub mod scraper;

use std::sync::Arc;

use anyhow::{bail, Result};
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

use std::time::Duration;

use crate::cache::CacheStore;
use scraper::{
    find_hall, hall_names, scrape_balance, scrape_hours, scrape_menu, scrape_nutrition,
    BalanceResult, DiningLocation, MealBalance, DINING_HALLS,
};

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

    pub async fn get_menu(
        &self,
        hall: Option<&str>,
        meal: Option<&str>,
        date: Option<&str>,
        include_all: bool,
    ) -> Result<String> {
        // Convert ISO date (YYYY-MM-DD) to M/D/YYYY for the nutrition site.
        // Cache key always uses the canonical ISO date for consistency.
        let (iso_date, formatted_date) = match date {
            Some(d) => {
                if let Ok(parsed) = chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d") {
                    let iso = d.to_string();
                    let formatted = format!("{}/{}/{}", parsed.month(), parsed.day(), parsed.year());
                    (Some(iso), Some(formatted))
                } else {
                    // Assume it's already in M/D/YYYY — store raw as cache key
                    (Some(d.to_string()), Some(d.to_string()))
                }
            }
            None => (None, None),
        };
        let scraper_date = formatted_date.as_deref();
        let cache_date = iso_date.as_deref().unwrap_or("today");

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
            DINING_HALLS.iter().take(5).collect()
        };

        let mut menus = Vec::new();
        for hall in halls {
            let cache_key = format!("dining:menu:{}:{}", hall.location_num, cache_date);
            let http = &self.http;
            let menu = self
                .cache
                .get_or_fetch(&cache_key, 3600, || scrape_menu(http, hall, scraper_date))
                .await?;
            menus.push(menu);
        }

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
            for menu in &mut menus {
                for meal in &mut menu.meals {
                    meal.categories.retain(|c| {
                        !FILTERED_CATEGORIES.contains(&c.name.to_lowercase().as_str())
                    });
                }
            }
        }

        let output: String = menus
            .iter()
            .map(|m| m.to_string())
            .collect::<Vec<_>>()
            .join("\n---\n\n");

        if output.trim().is_empty() {
            bail!("No menu data available. The nutrition site may be temporarily down.");
        }

        Ok(output)
    }

    pub async fn get_nutrition(&self, recipe_id: &str) -> Result<String> {
        let info = scrape_nutrition(&self.http, recipe_id).await?;
        Ok(info.to_string())
    }

    pub async fn get_hours(&self, location: Option<&str>) -> Result<String> {
        let http = &self.http;
        let locations: Vec<DiningLocation> = self
            .cache
            .get_or_fetch("dining:hours", 21600, || scrape_hours(http))
            .await?;

        let today = chrono::Local::now().format("%Y-%m-%d").to_string();

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

    pub async fn get_balance(&self, auth_client: &reqwest::Client) -> Result<BalanceResult> {
        // Balance uses conditional caching (only cache on success), so we don't
        // use get_or_fetch here.
        if let Some(cached) = self.cache.get("dining:balance").await {
            if let Ok(balance) = serde_json::from_str::<MealBalance>(&cached) {
                return Ok(BalanceResult { balance, debug_snippet: None });
            }
        }

        let result = scrape_balance(auth_client).await?;

        if result.debug_snippet.is_none() {
            if let Ok(json) = serde_json::to_string(&result.balance) {
                self.cache.set("dining:balance", &json, 300).await;
            }
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
        loop {
            let delay = duration_until_next_5am();
            tracing::info!("Next dining cache refresh in {}h {}m", delay.as_secs() / 3600, (delay.as_secs() % 3600) / 60);
            tokio::time::sleep(delay).await;

            let now = chrono::Local::now();
            let scraper_date = format!("{}/{}/{}", now.month(), now.day(), now.year());
            let iso_date = now.format("%Y-%m-%d").to_string();

            for hall in DINING_HALLS.iter().take(5) {
                match scrape_menu(&http, hall, Some(&scraper_date)).await {
                    Ok(menu) => {
                        if let Ok(json) = serde_json::to_string(&menu) {
                            let key = format!("dining:menu:{}:{}", hall.location_num, iso_date);
                            cache.set(&key, &json, 3600).await;
                        }
                        tracing::info!("Refreshed dining cache for {}", hall.name);
                    }
                    Err(e) => tracing::warn!("Cache refresh failed for {}: {}", hall.name, e),
                }
            }
        }
    })
}

fn duration_until_next_5am() -> Duration {
    use chrono::Local;
    let now = Local::now();
    let today_5am = now.date_naive().and_hms_opt(5, 0, 0).unwrap();
    let next_5am = if now.time() < chrono::NaiveTime::from_hms_opt(5, 0, 0).unwrap() {
        today_5am
    } else {
        today_5am + chrono::Duration::days(1)
    };
    let local_next = next_5am
        .and_local_timezone(Local)
        .single()
        .expect("ambiguous local time");
    (local_next - now)
        .to_std()
        .unwrap_or(Duration::from_secs(3600))
}
