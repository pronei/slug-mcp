pub mod scraper;

use std::sync::Arc;

use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FacilityOccupancyRequest {
    /// Facility name to filter (e.g., "East Gym", "Pool", "Wellness"). If omitted, returns all facilities.
    pub facility: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FacilityScheduleRequest {
    /// Facility UUID from get_facility_occupancy output.
    pub facility_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GroupExerciseRequest {
    /// Filter by day of week (e.g., "Monday", "Tuesday"). If omitted, returns all days.
    pub day: Option<String>,
    /// Filter by class name (substring match, e.g., "yoga", "cycling"). If omitted, returns all classes.
    pub class_name: Option<String>,
}

use crate::cache::CacheStore;
use scraper::{
    find_facility, scrape_group_exercise, scrape_occupancy, scrape_schedule, FacilityOccupancy,
    GroupExerciseClass,
};

pub struct RecreationService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl RecreationService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self {
        Self { http, cache }
    }

    pub async fn get_occupancy(&self, facility_name: Option<&str>) -> Result<String> {
        let all = self.fetch_occupancy().await?;

        let display: Vec<&FacilityOccupancy> = if let Some(query) = facility_name {
            let matches = find_facility(query, &all);
            if matches.is_empty() {
                let names: Vec<&str> = all.iter().map(|f| f.name.as_str()).collect();
                anyhow::bail!(
                    "Facility '{}' not found. Available: {}",
                    query,
                    names.join(", ")
                );
            }
            matches
        } else {
            all.iter().collect()
        };

        let header = "## UCSC Recreation Facility Occupancy\n".to_string();
        let body: Vec<String> = display.iter().map(|f| f.to_string()).collect();
        Ok(format!("{}{}", header, body.join("\n\n")))
    }

    pub async fn get_schedule(&self, facility_id: &str) -> Result<String> {
        let cache_key = format!("recreation:schedule:{}", facility_id);

        if let Some(cached) = self.cache.get(&cache_key).await {
            return Ok(cached);
        }

        let schedule = scrape_schedule(&self.http, facility_id).await?;
        let output = schedule.to_string();

        self.cache.set(&cache_key, &output, 3600).await; // 1 hour
        Ok(output)
    }

    pub async fn get_group_exercise(
        &self,
        day: Option<&str>,
        class_name: Option<&str>,
    ) -> Result<String> {
        let all = self.fetch_group_exercise().await?;

        let filtered: Vec<&GroupExerciseClass> = all
            .iter()
            .filter(|c| {
                if let Some(d) = day {
                    if !c.day.eq_ignore_ascii_case(d) {
                        return false;
                    }
                }
                if let Some(name) = class_name {
                    if !c.name.to_lowercase().contains(&name.to_lowercase()) {
                        return false;
                    }
                }
                true
            })
            .collect();

        if filtered.is_empty() {
            let mut msg = String::from("No group exercise classes found");
            if let Some(d) = day {
                msg.push_str(&format!(" on {}", d));
            }
            if let Some(n) = class_name {
                msg.push_str(&format!(" matching '{}'", n));
            }
            msg.push('.');
            return Ok(msg);
        }

        let mut out = String::from("## UCSC Group Exercise Schedule (Spring 2026)\n\n");
        let mut current_day = "";
        for class in &filtered {
            if class.day != current_day {
                if !current_day.is_empty() {
                    out.push('\n');
                }
                out.push_str(&format!("### {}\n", class.day));
                current_day = &class.day;
            }
            out.push_str(&format!("{}\n", class));
        }
        out.push_str("\n_Source: goslugs.com group exercise schedule_\n");
        Ok(out)
    }

    async fn fetch_occupancy(&self) -> Result<Vec<FacilityOccupancy>> {
        let http = &self.http;
        self.cache
            .get_or_fetch("recreation:occupancy", 120, || scrape_occupancy(http))
            .await
    }

    async fn fetch_group_exercise(&self) -> Result<Vec<GroupExerciseClass>> {
        let http = &self.http;
        self.cache
            .get_or_fetch("recreation:group_exercise", 3600, || {
                scrape_group_exercise(http)
            })
            .await
    }
}
