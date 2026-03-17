pub mod scraper;

use std::sync::Arc;

use anyhow::{bail, Result};
use chrono::Datelike;

use crate::cache::CacheStore;
use scraper::{
    find_hall, hall_names, scrape_balance, scrape_hours, scrape_menu, scrape_nutrition,
    DiningLocation, DiningMenu, MealBalance, DINING_HALLS,
};

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

        let mut menus = if let Some(hall_query) = hall {
            let hall = find_hall(hall_query).ok_or_else(|| {
                anyhow::anyhow!(
                    "Dining hall '{}' not found. Available halls: {}",
                    hall_query,
                    hall_names()
                )
            })?;

            let cache_key = format!("dining:menu:{}:{}", hall.location_num, cache_date);
            if let Some(cached) = self.cache.get(&cache_key).await {
                match serde_json::from_str::<DiningMenu>(&cached) {
                    Ok(menu) => vec![menu],
                    Err(e) => {
                        tracing::warn!("Cache deserialization failed for {}: {}", cache_key, e);
                        let menu = scrape_menu(&self.http, hall, scraper_date).await?;
                        if let Ok(json) = serde_json::to_string(&menu) {
                            self.cache.set(&cache_key, &json, 3600).await;
                        }
                        vec![menu]
                    }
                }
            } else {
                let menu = scrape_menu(&self.http, hall, scraper_date).await?;
                if let Ok(json) = serde_json::to_string(&menu) {
                    self.cache.set(&cache_key, &json, 3600).await;
                }
                vec![menu]
            }
        } else {
            let mut all_menus = Vec::new();
            for hall in DINING_HALLS.iter().take(5) {
                let cache_key = format!("dining:menu:{}:{}", hall.location_num, cache_date);
                let menu = if let Some(cached) = self.cache.get(&cache_key).await {
                    match serde_json::from_str::<DiningMenu>(&cached) {
                        Ok(menu) => menu,
                        Err(e) => {
                            tracing::warn!("Cache deserialization failed for {}: {}", cache_key, e);
                            let menu = scrape_menu(&self.http, hall, scraper_date).await?;
                            if let Ok(json) = serde_json::to_string(&menu) {
                                self.cache.set(&cache_key, &json, 3600).await;
                            }
                            menu
                        }
                    }
                } else {
                    let menu = scrape_menu(&self.http, hall, scraper_date).await?;
                    if let Ok(json) = serde_json::to_string(&menu) {
                        self.cache.set(&cache_key, &json, 3600).await;
                    }
                    menu
                };
                all_menus.push(menu);
            }
            all_menus
        };

        // Filter by meal if specified
        if let Some(meal_filter) = meal {
            let filter = meal_filter.to_lowercase();
            for menu in &mut menus {
                menu.meals
                    .retain(|m| m.name.to_lowercase().contains(&filter));
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
        let cache_key = "dining:hours";
        let locations: Vec<DiningLocation> = if let Some(cached) = self.cache.get(cache_key).await {
            match serde_json::from_str(&cached) {
                Ok(locs) => locs,
                Err(e) => {
                    tracing::warn!("Cache deserialization failed for {}: {}", cache_key, e);
                    let locs = scrape_hours(&self.http).await?;
                    if let Ok(json) = serde_json::to_string(&locs) {
                        self.cache.set(cache_key, &json, 21600).await;
                    }
                    locs
                }
            }
        } else {
            let locs = scrape_hours(&self.http).await?;
            if let Ok(json) = serde_json::to_string(&locs) {
                self.cache.set(cache_key, &json, 21600).await; // 6hr TTL
            }
            locs
        };

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

    pub async fn get_balance(&self, auth_client: &reqwest::Client) -> Result<MealBalance> {
        let cache_key = "dining:balance";
        if let Some(cached) = self.cache.get(cache_key).await {
            match serde_json::from_str::<MealBalance>(&cached) {
                Ok(balance) => return Ok(balance),
                Err(e) => {
                    tracing::warn!("Cache deserialization failed for {}: {}", cache_key, e);
                }
            }
        }

        let balance = scrape_balance(auth_client).await?;

        if let Ok(json) = serde_json::to_string(&balance) {
            self.cache.set(cache_key, &json, 300).await; // 5 min TTL
        }

        Ok(balance)
    }
}
