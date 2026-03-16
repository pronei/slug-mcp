pub mod scraper;

use std::sync::Arc;

use anyhow::{bail, Result};

use crate::cache::CacheStore;
use scraper::{
    find_hall, hall_names, scrape_menu, scrape_balance, scrape_nutrition, scrape_hours,
    DiningMenu, MealBalance, DiningLocation, DINING_HALLS,
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
    ) -> Result<String> {
        let mut menus = if let Some(hall_query) = hall {
            let hall = find_hall(hall_query).ok_or_else(|| {
                anyhow::anyhow!(
                    "Dining hall '{}' not found. Available halls: {}",
                    hall_query,
                    hall_names()
                )
            })?;

            let cache_key = format!("dining:menu:{}", hall.location_num);
            if let Some(cached) = self.cache.get(&cache_key).await {
                let menu: DiningMenu = serde_json::from_str(&cached).unwrap_or(DiningMenu {
                    hall_name: hall.name.to_string(),
                    meals: vec![],
                });
                vec![menu]
            } else {
                let menu = scrape_menu(&self.http, hall).await?;
                if let Ok(json) = serde_json::to_string(&menu) {
                    self.cache.set(&cache_key, &json, 3600).await;
                }
                vec![menu]
            }
        } else {
            let mut all_menus = Vec::new();
            for hall in DINING_HALLS.iter().take(5) {
                let cache_key = format!("dining:menu:{}", hall.location_num);
                let menu = if let Some(cached) = self.cache.get(&cache_key).await {
                    serde_json::from_str(&cached).unwrap_or(DiningMenu {
                        hall_name: hall.name.to_string(),
                        meals: vec![],
                    })
                } else {
                    let menu = scrape_menu(&self.http, hall).await?;
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
                menu.meals.retain(|m| m.name.to_lowercase().contains(&filter));
            }
        }

        let output: String = menus.iter().map(|m| m.format()).collect::<Vec<_>>().join("\n---\n\n");

        if output.trim().is_empty() {
            bail!("No menu data available. The nutrition site may be temporarily down.");
        }

        Ok(output)
    }

    pub async fn get_nutrition(&self, recipe_id: &str) -> Result<String> {
        let info = scrape_nutrition(&self.http, recipe_id).await?;
        Ok(info.format())
    }

    pub async fn get_hours(&self, location: Option<&str>) -> Result<String> {
        let cache_key = "dining:hours";
        let locations: Vec<DiningLocation> = if let Some(cached) = self.cache.get(cache_key).await {
            serde_json::from_str(&cached).unwrap_or_default()
        } else {
            let locs = scrape_hours(&self.http).await?;
            if let Ok(json) = serde_json::to_string(&locs) {
                self.cache.set(cache_key, &json, 21600).await; // 6hr TTL
            }
            locs
        };

        let filtered: Vec<&DiningLocation> = if let Some(query) = location {
            let q = query.to_lowercase();
            locations.iter().filter(|l| l.name.to_lowercase().contains(&q)).collect()
        } else {
            locations.iter().collect()
        };

        if filtered.is_empty() {
            if location.is_some() {
                bail!("Location '{}' not found.", location.unwrap());
            }
            bail!("No hours data available.");
        }

        let output: String = filtered.iter().map(|l| l.format()).collect::<Vec<_>>().join("");
        Ok(format!("# UCSC Dining Hours\n\n{}", output))
    }

    pub async fn get_balance(&self, auth_client: &reqwest::Client) -> Result<MealBalance> {
        let cache_key = "dining:balance";
        if let Some(cached) = self.cache.get(cache_key).await {
            if let Ok(balance) = serde_json::from_str::<CachedBalance>(&cached) {
                return Ok(MealBalance {
                    slug_points: balance.slug_points,
                    banana_bucks: balance.banana_bucks,
                    meal_swipes: balance.meal_swipes,
                });
            }
        }

        let balance = scrape_balance(auth_client).await?;

        if let Ok(json) = serde_json::to_string(&CachedBalance {
            slug_points: balance.slug_points,
            banana_bucks: balance.banana_bucks,
            meal_swipes: balance.meal_swipes,
        }) {
            self.cache.set(cache_key, &json, 300).await; // 5 min TTL
        }

        Ok(balance)
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedBalance {
    slug_points: Option<f64>,
    banana_bucks: Option<f64>,
    meal_swipes: Option<u32>,
}
