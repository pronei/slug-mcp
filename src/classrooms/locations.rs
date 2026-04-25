use std::collections::HashMap;
use std::sync::LazyLock;

use serde::Deserialize;

const BUILDINGS_JSON: &str = include_str!("buildings.json");

static BUILDINGS: LazyLock<HashMap<String, BuildingLocation>> = LazyLock::new(|| {
    serde_json::from_str(BUILDINGS_JSON).expect("buildings.json should be valid JSON")
});

#[derive(Debug, Deserialize)]
pub struct BuildingLocation {
    pub name: String,
    pub college_area: String,
    pub lat: f64,
    pub lon: f64,
    pub landmarks: String,
    pub map_url: String,
}

/// Look up a building by its exact area code (from the classrooms scraper CSS class).
pub fn lookup_by_area(area_code: &str) -> Option<&'static BuildingLocation> {
    BUILDINGS.get(area_code)
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
