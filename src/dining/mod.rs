pub mod scraper;

use std::sync::Arc;

use anyhow::{bail, Result};

use crate::cache::CacheStore;
use scraper::{
    find_hall, hall_names, scrape_menu, scrape_balance, dining_hours,
    DiningMenu, MealBalance, DINING_HALLS,
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
        let menus = if let Some(hall_query) = hall {
            let hall = find_hall(hall_query).ok_or_else(|| {
                anyhow::anyhow!(
                    "Dining hall '{}' not found. Available halls: {}",
                    hall_query,
                    hall_names()
                )
            })?;

            let cache_key = format!("dining:menu:{}", hall.location_num);
            if let Some(cached) = self.cache.get(&cache_key).await {
                vec![serde_json::from_str::<CachedMenu>(&cached)
                    .map(|c| c.into_dining_menu())
                    .unwrap_or_else(|_| DiningMenu {
                        hall_name: hall.name.to_string(),
                        meals: vec![],
                    })]
            } else {
                let menu = scrape_menu(&self.http, hall).await?;
                if let Ok(json) = serde_json::to_string(&CachedMenu::from_dining_menu(&menu)) {
                    self.cache.set(&cache_key, &json, 3600).await;
                }
                vec![menu]
            }
        } else {
            // Fetch all dining halls
            let mut all_menus = Vec::new();
            for hall in DINING_HALLS.iter().take(5) {
                // Only main dining halls
                let cache_key = format!("dining:menu:{}", hall.location_num);
                let menu = if let Some(cached) = self.cache.get(&cache_key).await {
                    serde_json::from_str::<CachedMenu>(&cached)
                        .map(|c| c.into_dining_menu())
                        .unwrap_or_else(|_| scraper::DiningMenu {
                            hall_name: hall.name.to_string(),
                            meals: vec![],
                        })
                } else {
                    let menu = scrape_menu(&self.http, hall).await?;
                    if let Ok(json) = serde_json::to_string(&CachedMenu::from_dining_menu(&menu)) {
                        self.cache.set(&cache_key, &json, 3600).await;
                    }
                    menu
                };
                all_menus.push(menu);
            }
            all_menus
        };

        let mut output = String::new();
        for menu in &menus {
            let formatted = menu.format();
            let filtered = if let Some(meal_filter) = meal {
                filter_by_meal(&formatted, meal_filter)
            } else {
                formatted
            };
            output.push_str(&filtered);
            output.push_str("\n---\n\n");
        }

        if output.trim().is_empty() || output.trim() == "---" {
            bail!("No menu data available. The nutrition site may be temporarily down.");
        }

        Ok(output)
    }

    pub async fn get_hours(&self) -> Result<String> {
        let cache_key = "dining:hours";
        if let Some(cached) = self.cache.get(cache_key).await {
            return Ok(cached);
        }
        let hours = dining_hours();
        self.cache.set(cache_key, &hours, 21600).await;
        Ok(hours)
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

fn filter_by_meal(formatted_menu: &str, meal_filter: &str) -> String {
    let filter = meal_filter.to_lowercase();
    let mut result = String::new();
    let mut in_matching_section = false;
    let mut header_written = false;

    for line in formatted_menu.lines() {
        if line.starts_with("## ") {
            // Hall header - always include
            result.push_str(line);
            result.push('\n');
            header_written = true;
            continue;
        }
        if line.starts_with("### ") {
            let section = line.trim_start_matches("### ").to_lowercase();
            in_matching_section = section.contains(&filter);
            if in_matching_section {
                result.push_str(line);
                result.push('\n');
            }
            continue;
        }
        if in_matching_section {
            result.push_str(line);
            result.push('\n');
        }
    }

    if !header_written {
        return String::new();
    }
    result
}

// For serializing menus to cache
#[derive(serde::Serialize, serde::Deserialize)]
struct CachedMenu {
    hall_name: String,
    meals: Vec<CachedMeal>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedMeal {
    name: String,
    items: Vec<String>,
}

impl CachedMenu {
    fn from_dining_menu(menu: &DiningMenu) -> Self {
        Self {
            hall_name: menu.hall_name.clone(),
            meals: menu
                .meals
                .iter()
                .map(|m| CachedMeal {
                    name: m.name.clone(),
                    items: m.items.clone(),
                })
                .collect(),
        }
    }

    fn into_dining_menu(self) -> DiningMenu {
        DiningMenu {
            hall_name: self.hall_name,
            meals: self
                .meals
                .into_iter()
                .map(|m| scraper::Meal {
                    name: m.name,
                    items: m.items,
                })
                .collect(),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedBalance {
    slug_points: Option<f64>,
    banana_bucks: Option<f64>,
    meal_swipes: Option<u32>,
}
