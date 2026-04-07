/// Hardcoded UCSC campus loop bus stops and fuzzy search.

#[derive(Debug, Clone)]
pub struct LoopStop {
    pub name: &'static str,
    pub lat: f64,
    pub lon: f64,
    pub direction: LoopDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopDirection {
    CW,
    CCW,
}

impl LoopDirection {
    pub fn label(self) -> &'static str {
        match self {
            Self::CW => "Clockwise",
            Self::CCW => "Counter-Clockwise",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Self::CW => "CW",
            Self::CCW => "CCW",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_uppercase().as_str() {
            "CW" | "CLOCKWISE" => Some(Self::CW),
            "CCW" | "COUNTERCLOCKWISE" | "COUNTER-CLOCKWISE" => Some(Self::CCW),
            _ => None,
        }
    }
}

/// All CW (clockwise) loop stops, in route order.
static CW_STOPS: &[LoopStop] = &[
    LoopStop { name: "Main Entrance", lat: 36.9778, lon: -122.0536, direction: LoopDirection::CW },
    LoopStop { name: "High Western", lat: 36.9813, lon: -122.0586, direction: LoopDirection::CW },
    LoopStop { name: "Arboretum", lat: 36.9838, lon: -122.0620, direction: LoopDirection::CW },
    LoopStop { name: "Oakes", lat: 36.9893, lon: -122.0651, direction: LoopDirection::CW },
    LoopStop { name: "Porter", lat: 36.9943, lon: -122.0655, direction: LoopDirection::CW },
    LoopStop { name: "Kerr Bridge", lat: 36.9977, lon: -122.0641, direction: LoopDirection::CW },
    LoopStop { name: "Kresge", lat: 36.9991, lon: -122.0643, direction: LoopDirection::CW },
    LoopStop { name: "Science Hill", lat: 37.0003, lon: -122.0617, direction: LoopDirection::CW },
    LoopStop { name: "9/10", lat: 37.0010, lon: -122.0575, direction: LoopDirection::CW },
    LoopStop { name: "Cowell", lat: 36.9972, lon: -122.0540, direction: LoopDirection::CW },
    LoopStop { name: "East Lot", lat: 36.9915, lon: -122.0518, direction: LoopDirection::CW },
    LoopStop { name: "Farm", lat: 36.9870, lon: -122.0530, direction: LoopDirection::CW },
    LoopStop { name: "Lower Campus", lat: 36.9815, lon: -122.0530, direction: LoopDirection::CW },
];

/// All CCW (counter-clockwise) loop stops, in route order.
static CCW_STOPS: &[LoopStop] = &[
    LoopStop { name: "Main Entrance", lat: 36.9778, lon: -122.0536, direction: LoopDirection::CCW },
    LoopStop { name: "Lower Campus", lat: 36.9815, lon: -122.0530, direction: LoopDirection::CCW },
    LoopStop { name: "Farm", lat: 36.9870, lon: -122.0530, direction: LoopDirection::CCW },
    LoopStop { name: "East Lot", lat: 36.9915, lon: -122.0518, direction: LoopDirection::CCW },
    LoopStop { name: "East Field", lat: 36.9930, lon: -122.0510, direction: LoopDirection::CCW },
    LoopStop { name: "Cowell", lat: 36.9972, lon: -122.0540, direction: LoopDirection::CCW },
    LoopStop { name: "Merrill", lat: 37.0000, lon: -122.0535, direction: LoopDirection::CCW },
    LoopStop { name: "9/10", lat: 37.0010, lon: -122.0575, direction: LoopDirection::CCW },
    LoopStop { name: "Science Hill", lat: 37.0003, lon: -122.0617, direction: LoopDirection::CCW },
    LoopStop { name: "Kresge", lat: 36.9991, lon: -122.0643, direction: LoopDirection::CCW },
    LoopStop { name: "Porter", lat: 36.9943, lon: -122.0655, direction: LoopDirection::CCW },
    LoopStop { name: "Family House", lat: 36.9920, lon: -122.0660, direction: LoopDirection::CCW },
    LoopStop { name: "Oakes", lat: 36.9893, lon: -122.0651, direction: LoopDirection::CCW },
    LoopStop { name: "Arboretum", lat: 36.9838, lon: -122.0620, direction: LoopDirection::CCW },
    LoopStop { name: "Tosca Terrace", lat: 36.9820, lon: -122.0600, direction: LoopDirection::CCW },
    LoopStop { name: "High Western", lat: 36.9813, lon: -122.0586, direction: LoopDirection::CCW },
];

/// Get all stops, optionally filtered by direction.
pub fn all_stops(direction: Option<LoopDirection>) -> Vec<&'static LoopStop> {
    match direction {
        Some(LoopDirection::CW) => CW_STOPS.iter().collect(),
        Some(LoopDirection::CCW) => CCW_STOPS.iter().collect(),
        None => CW_STOPS.iter().chain(CCW_STOPS.iter()).collect(),
    }
}

/// Get ordered stops for a specific direction.
pub fn route_stops(direction: LoopDirection) -> &'static [LoopStop] {
    match direction {
        LoopDirection::CW => CW_STOPS,
        LoopDirection::CCW => CCW_STOPS,
    }
}

/// Search stops by name using case-insensitive matching.
/// Returns up to `limit` matches, sorted by relevance (exact > prefix > substring > words).
pub fn search_stops(query: &str, direction: Option<LoopDirection>, limit: usize) -> Vec<&'static LoopStop> {
    let query_lower = query.to_lowercase();
    let stops = all_stops(direction);

    let mut matches: Vec<(usize, &LoopStop)> = stops
        .into_iter()
        .filter_map(|stop| {
            let name_lower = stop.name.to_lowercase();
            if name_lower == query_lower {
                Some((0, stop)) // exact match
            } else if name_lower.starts_with(&query_lower) {
                Some((1, stop)) // prefix match
            } else if name_lower.contains(&query_lower) {
                Some((2, stop)) // substring match
            } else {
                let query_words: Vec<&str> = query_lower.split_whitespace().collect();
                let all_words_match = !query_words.is_empty()
                    && query_words.iter().all(|w| name_lower.contains(w));
                if all_words_match {
                    Some((3, stop))
                } else {
                    None
                }
            }
        })
        .collect();

    matches.sort_by_key(|(rank, _)| *rank);
    matches.into_iter().take(limit).map(|(_, stop)| stop).collect()
}

/// Find the nearest stop to a given coordinate.
pub fn nearest_stop(lat: f64, lon: f64, direction: LoopDirection) -> &'static LoopStop {
    let stops = route_stops(direction);
    stops
        .iter()
        .min_by(|a, b| {
            let dist_a = haversine_approx(lat, lon, a.lat, a.lon);
            let dist_b = haversine_approx(lat, lon, b.lat, b.lon);
            dist_a.partial_cmp(&dist_b).unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap() // safe: stops is never empty
}

/// Approximate distance for sorting (squared diff, good enough for nearby points).
fn haversine_approx(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let dlat = lat1 - lat2;
    let dlon = (lon1 - lon2) * 0.8; // rough cos(37°) correction
    dlat * dlat + dlon * dlon
}
