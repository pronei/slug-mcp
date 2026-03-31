use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

static BUILDINGS: OnceLock<HashMap<String, BuildingLocation>> = OnceLock::new();

const BUILDINGS_JSON: &str = include_str!("buildings.json");

#[derive(Debug, Deserialize)]
pub struct BuildingLocation {
    pub name: String,
    pub college_area: String,
    pub lat: f64,
    pub lon: f64,
    pub landmarks: String,
    pub map_url: String,
}

/// Get the buildings index, parsed once from the embedded JSON.
fn buildings() -> &'static HashMap<String, BuildingLocation> {
    BUILDINGS.get_or_init(|| {
        serde_json::from_str(BUILDINGS_JSON).expect("buildings.json should be valid JSON")
    })
}

/// Look up a building by its exact area code (from the classrooms scraper CSS class).
pub fn lookup_by_area(area_code: &str) -> Option<&'static BuildingLocation> {
    buildings().get(area_code)
}

/// Search buildings by a free-text query, matching against area codes, names, and college areas.
/// Returns the best match, if any.
pub fn search_building(query: &str) -> Option<&'static BuildingLocation> {
    let q = query.to_lowercase();
    let map = buildings();

    // Exact area code match
    if let Some(loc) = map.get(&q) {
        return Some(loc);
    }

    // Search by name or college_area
    let mut best: Option<(&BuildingLocation, usize)> = None;

    for (code, loc) in map.iter() {
        let name_lower = loc.name.to_lowercase();
        let area_lower = loc.college_area.to_lowercase();

        let rank = if name_lower == q || area_lower == q {
            0 // exact match
        } else if name_lower.starts_with(&q) || code.starts_with(&q) {
            1 // prefix match
        } else if name_lower.contains(&q) || area_lower.contains(&q) || code.contains(&q) {
            2 // substring match
        } else {
            continue;
        };

        if best.as_ref().is_none_or(|(_, r)| rank < *r) {
            best = Some((loc, rank));
        }
    }

    best.map(|(loc, _)| loc)
}

impl BuildingLocation {
    /// Format as markdown location info for display.
    pub fn format_location(&self) -> String {
        let mut out = format!("- **Location**: {} — {}", self.college_area, self.name);
        out.push_str(&format!("\n- **How to get there**: {}", self.landmarks));
        out.push_str(&format!(
            "\n- **Map**: [View on campus map]({}) | [Google Maps](https://maps.google.com/?q={},{})",
            self.map_url, self.lat, self.lon
        ));
        out
    }
}
