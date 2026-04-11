//! Hardcoded Santa Cruz surf spot table.
//!
//! Keep the list small and human-curated. These coordinates are break
//! locations; Open-Meteo snaps to the nearest ocean grid cell when queried,
//! so spots that are geographically close may return identical conditions.

#[derive(Debug, Clone, Copy)]
pub struct SurfSpot {
    pub slug: &'static str,
    pub name: &'static str,
    pub lat: f64,
    pub lon: f64,
    pub notes: &'static str,
}

pub const SURF_SPOTS: &[SurfSpot] = &[
    SurfSpot {
        slug: "steamer-lane",
        name: "Steamer Lane",
        lat: 36.9519,
        lon: -122.0264,
        notes: "Big-wave right-hander at Lighthouse Point, works on NW swell",
    },
    SurfSpot {
        slug: "pleasure-point",
        name: "Pleasure Point",
        lat: 36.9575,
        lon: -121.9700,
        notes: "Clean right-hand point break, all levels",
    },
    SurfSpot {
        slug: "cowells",
        name: "Cowell's",
        lat: 36.9619,
        lon: -122.0218,
        notes: "Beginner-friendly longboard spot, small and mellow",
    },
    SurfSpot {
        slug: "natural-bridges",
        name: "Natural Bridges",
        lat: 36.9490,
        lon: -122.0580,
        notes: "West-facing beach break, good on small swell",
    },
    SurfSpot {
        slug: "the-hook",
        name: "The Hook",
        lat: 36.9570,
        lon: -121.9658,
        notes: "Consistent right-hander, usually crowded",
    },
    SurfSpot {
        slug: "manresa",
        name: "Manresa",
        lat: 36.9320,
        lon: -121.8540,
        notes: "South county beach break, west-facing",
    },
];

/// Case-insensitive fuzzy lookup. Matches slug or name with substring containment.
pub fn find(query: &str) -> Option<&'static SurfSpot> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return None;
    }
    // Try exact slug first, then fallback to substring in slug or name.
    SURF_SPOTS
        .iter()
        .find(|s| s.slug == q)
        .or_else(|| {
            SURF_SPOTS
                .iter()
                .find(|s| s.slug.contains(&q) || s.name.to_lowercase().contains(&q))
        })
}

pub fn names_list() -> String {
    SURF_SPOTS
        .iter()
        .map(|s| s.name)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_find_name() {
        assert_eq!(find("Steamer").unwrap().slug, "steamer-lane");
        assert_eq!(find("steamer lane").unwrap().slug, "steamer-lane");
        assert_eq!(find("THE HOOK").unwrap().slug, "the-hook");
        assert_eq!(find("pleasure").unwrap().slug, "pleasure-point");
    }

    #[test]
    fn fuzzy_find_slug() {
        assert_eq!(find("steamer-lane").unwrap().slug, "steamer-lane");
        assert_eq!(find("the-hook").unwrap().slug, "the-hook");
    }

    #[test]
    fn fuzzy_find_miss() {
        assert!(find("malibu").is_none());
        assert!(find("").is_none());
    }
}
