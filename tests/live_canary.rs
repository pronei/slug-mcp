//! Live upstream canaries: one test per public tool, calling the real upstream
//! through the real binary and checking that each adapter still produces the
//! output it promises. A failure names the tool and the adapter file to fix.
//!
//! All tests are `#[ignore]`, so `cargo test` stays offline. Run them with:
//!
//!   cargo test --test live_canary -- --ignored
//!   cargo nextest run --profile live --test live_canary --run-ignored ignored-only
//!
//! Each test spawns the local binary over stdio. Set
//! `SLUG_MCP_CANARY_URL=https://slug-mcp.fly.dev/mcp` to run the same checks
//! against a deployed server instead.
//!
//! Keyed tools (eBird, AirNow, FIRMS, NPS, BusTime) expect live data when their
//! env var is set, or when the target is a deployed server (which holds its own
//! keys), and the "not configured" text otherwise. `.github/workflows/canary.yml`
//! runs both targets daily.

mod common;

use std::time::{Duration, Instant};

use chrono::Datelike;
use common::{Client, StdioServer, call_tool, is_tool_error, text_of};
use rmcp::model::CallToolResult;
use rmcp::service::ServiceError;
use serde_json::{Value, json};

/// The adapter and upstream behind each public tool. A failing canary prints
/// its row, and `every_public_tool_has_a_canary` checks this list against the
/// server's tools/list.
const ADAPTERS: &[(&str, &str)] = &[
    // Campus
    (
        "get_dining_hours",
        "src/dining/scraper.rs ← dining.ucsc.edu/locations-hours",
    ),
    (
        "get_dining_menu",
        "src/dining/scraper.rs ← nutrition.sa.ucsc.edu shortmenu/longmenu",
    ),
    (
        "get_nutrition_info",
        "src/dining/scraper.rs ← nutrition.sa.ucsc.edu label.aspx",
    ),
    (
        "get_facility_occupancy",
        "src/recreation/scraper.rs ← campusrec.ucsc.edu/FacilityOccupancy",
    ),
    (
        "get_facility_schedule",
        "src/recreation/scraper.rs ← campusrec.ucsc.edu scheduler XHR",
    ),
    (
        "get_group_exercise_classes",
        "src/recreation/scraper.rs (GROUP_EXERCISE_URL) ← goslugs.com group-exercise schedule page",
    ),
    (
        "get_study_room_availability",
        "src/library/scraper.rs ← calendar.library.ucsc.edu (LibCal)",
    ),
    (
        "search_classes",
        "src/academics/scraper.rs ← pisa.ucsc.edu class search",
    ),
    (
        "search_directory",
        "src/academics/scraper.rs ← campusdirectory.ucsc.edu",
    ),
    (
        "search_classrooms",
        "src/classrooms/scraper.rs ← classrooms.ucsc.edu",
    ),
    (
        "get_degree_requirements",
        "src/degrees/scraper.rs ← catalog.ucsc.edu program pages",
    ),
    (
        "check_degree_progress",
        "src/degrees/progress.rs + scraper.rs ← catalog.ucsc.edu",
    ),
    (
        "get_summer_deadlines",
        "src/summer/mod.rs ← summer.ucsc.edu academic calendar",
    ),
    (
        "get_upcoming_events",
        "src/events/tribe.rs ← events.ucsc.edu Tribe Events REST",
    ),
    (
        "search_events",
        "src/events/tribe.rs ← events.ucsc.edu Tribe Events REST",
    ),
    (
        "search_eventbrite_events",
        "src/events/eventbrite.rs ← eventbrite.com discover page",
    ),
    // Transit & roads
    (
        "get_bus_predictions",
        "src/transit/bustime.rs ← rt.scmetro.org BusTime getpredictions",
    ),
    (
        "get_service_alerts",
        "src/transit/gtfs_rt.rs (system) + bustime.rs (route/stop) ← rt.scmetro.org",
    ),
    (
        "get_vehicle_positions",
        "src/transit/gtfs_rt.rs ← rt.scmetro.org GTFS-RT vehicles",
    ),
    (
        "get_route_delays",
        "src/transit/gtfs_rt.rs ← rt.scmetro.org GTFS-RT trip updates",
    ),
    (
        "get_traffic_incidents",
        "src/traffic/chp.rs ← media.chp.ca.gov incident XML",
    ),
    (
        "get_lane_closures",
        "src/traffic/caltrans.rs ← cwwp2.dot.ca.gov D5 lane closures",
    ),
    (
        "get_traffic_summary",
        "src/traffic/mod.rs ← CHP + Caltrans feeds",
    ),
    (
        "get_traffic_cameras",
        "src/traffic/cameras.rs ← cwwp2.dot.ca.gov D5 CCTV status JSON",
    ),
    (
        "get_bike_share",
        "src/bike_share/mod.rs ← gbfs.bcycle.com Santa Cruz BCycle GBFS",
    ),
    // Weather, air, sky
    (
        "get_weather_forecast",
        "src/weather/nws.rs ← api.weather.gov",
    ),
    (
        "get_weather_alerts",
        "src/weather/nws.rs ← api.weather.gov alerts",
    ),
    ("get_air_quality", "src/air_quality/mod.rs ← airnowapi.org"),
    (
        "get_air_quality_forecast",
        "src/air_forecast/mod.rs ← air-quality-api.open-meteo.com",
    ),
    (
        "get_sun_moon",
        "src/astronomy/mod.rs ← api.open-meteo.com + api.sunrise-sunset.org",
    ),
    (
        "get_space_weather",
        "src/space_weather/mod.rs ← services.swpc.noaa.gov",
    ),
    (
        "get_fire_detections",
        "src/fire/mod.rs ← firms.modaps.eosdis.nasa.gov",
    ),
    (
        "get_earthquakes",
        "src/earthquakes/mod.rs ← earthquake.usgs.gov FDSN",
    ),
    // Ocean & coast
    (
        "get_tides",
        "src/tides/mod.rs ← api.tidesandcurrents.noaa.gov",
    ),
    (
        "get_surf_conditions",
        "src/marine/open_meteo.rs ← marine-api.open-meteo.com",
    ),
    (
        "get_buoy_observations",
        "src/buoy/mod.rs ← ndbc.noaa.gov realtime2",
    ),
    (
        "get_wave_buoy",
        "src/wave_buoy/mod.rs ← ndbc.noaa.gov .spec",
    ),
    (
        "get_beach_water_quality",
        "src/beach_water/mod.rs ← data.ca.gov CKAN datastore_search_sql",
    ),
    (
        "get_stream_conditions",
        "src/usgs_water/mod.rs ← waterservices.usgs.gov IV",
    ),
    (
        "get_sc_wharf_state",
        "src/ocean/wharf.rs ← erddap.sensors.axds.co",
    ),
    (
        "get_m1_water_column",
        "src/ocean/m1.rs ← erddap.sensors.axds.co",
    ),
    (
        "get_sst_state",
        "src/ocean/sst.rs ← coastwatch.pfeg.noaa.gov ERDDAP (MUR SST)",
    ),
    (
        "get_hfradar_currents",
        "src/ocean/hfr.rs ← coastwatch.pfeg.noaa.gov ERDDAP (HFRNet)",
    ),
    (
        "get_upwelling_indices",
        "src/ocean/upwelling.rs ← mjacox.com CUTI/BEUTI",
    ),
    (
        "get_hab_risk_forecast",
        "src/ocean/charm.rs ← coastwatch.pfeg.noaa.gov ERDDAP (C-HARM)",
    ),
    (
        "monterey_bay_hab_risk",
        "src/ocean/fusion.rs ← C-HARM, wharf, SST and eBird adapters",
    ),
    (
        "monterey_bay_upwelling_state",
        "src/ocean/fusion.rs ← the ocean adapters above",
    ),
    (
        "monterey_bay_research_snapshot",
        "src/ocean/fusion.rs ← the ocean adapters above",
    ),
    // Outdoors & biodiversity
    (
        "search_climbing_routes",
        "src/climbing/mod.rs ← api.openbeta.io GraphQL",
    ),
    (
        "get_national_park_info",
        "src/nps/scraper.rs ← developer.nps.gov",
    ),
    (
        "get_bird_hotspots",
        "src/biodiversity/ebird.rs ← api.ebird.org",
    ),
    (
        "search_bird_observations",
        "src/biodiversity/ebird.rs ← api.ebird.org",
    ),
    (
        "find_nearest_bird_sighting",
        "src/biodiversity/ebird.rs ← api.ebird.org",
    ),
    (
        "get_historic_bird_observations",
        "src/biodiversity/ebird.rs ← api.ebird.org",
    ),
    (
        "search_species_observations",
        "src/biodiversity/inaturalist.rs ← api.inaturalist.org",
    ),
];

fn adapter_for(tool: &str) -> &'static str {
    ADAPTERS
        .iter()
        .find(|(name, _)| *name == tool)
        .map(|(_, adapter)| *adapter)
        .unwrap_or("no ADAPTERS row for this tool")
}

/// Where the calls go: a freshly spawned local binary, or a deployed server.
enum Target {
    Local(StdioServer),
    Remote(Client),
}

impl Target {
    async fn connect() -> Self {
        match std::env::var("SLUG_MCP_CANARY_URL") {
            Ok(url) if !url.trim().is_empty() => {
                Self::Remote(common::connect_http(url.trim()).await)
            }
            _ => Self::Local(StdioServer::spawn(true).await),
        }
    }

    fn client(&self) -> &Client {
        match self {
            Self::Local(server) => &server.client,
            Self::Remote(client) => client,
        }
    }

    fn is_remote(&self) -> bool {
        matches!(self, Self::Remote(_))
    }

    async fn shutdown(self) {
        match self {
            Self::Local(server) => server.shutdown().await,
            Self::Remote(client) => {
                let _ = client.cancel().await;
            }
        }
    }
}

/// A check on a tool's text beyond its markers; `Err` explains the drift.
type TextCheck = fn(&str) -> Result<(), String>;

/// What a canary expects from a tool call.
struct Expect {
    /// Substrings that must all appear in the text when the call carries data.
    markers: &'static [&'static str],
    /// Env var gating the upstream; unset means the "not configured" text is
    /// the correct answer.
    keyed_by: Option<&'static str>,
    /// Upstream conditions that are documented, not drift: a protocol or tool
    /// error whose message contains one of these passes.
    tolerated_errors: &'static [&'static str],
    /// An extra check on the text, for drift that keeps the output's shape but
    /// serves stale data.
    check: Option<TextCheck>,
}

impl Expect {
    const fn markers(markers: &'static [&'static str]) -> Self {
        Self {
            markers,
            keyed_by: None,
            tolerated_errors: &[],
            check: None,
        }
    }

    const fn keyed(env_var: &'static str, markers: &'static [&'static str]) -> Self {
        Self {
            markers,
            keyed_by: Some(env_var),
            tolerated_errors: &[],
            check: None,
        }
    }

    const fn tolerating(mut self, errors: &'static [&'static str]) -> Self {
        self.tolerated_errors = errors;
        self
    }

    const fn checking(mut self, check: TextCheck) -> Self {
        self.check = Some(check);
        self
    }
}

enum Outcome {
    Data(String),
    ToolError(String),
    ProtocolError(String),
}

fn classify(result: Result<CallToolResult, ServiceError>) -> Outcome {
    match result {
        Ok(r) if is_tool_error(&r) => Outcome::ToolError(text_of(&r)),
        Ok(r) => Outcome::Data(text_of(&r)),
        Err(e) => Outcome::ProtocolError(e.to_string()),
    }
}

fn preview(text: &str) -> String {
    let mut p: String = text.chars().take(700).collect();
    if text.len() > 700 {
        p.push_str(" …");
    }
    p
}

/// Call `tool` once (twice if the first attempt is a protocol-level failure,
/// to ride out a momentary upstream blip) and check the outcome against `expect`.
/// Returns the text so chained canaries can pull ids out of it.
///
/// Every failure message starts with `<tool>: <what broke> — adapter <file>`,
/// which is what the daily report and issue show.
async fn canary(tool: &str, args: Value, expect: Expect) -> String {
    let target = Target::connect().await;
    let started = Instant::now();
    let mut outcome = classify(call_tool(target.client(), tool, args.clone()).await);
    if matches!(outcome, Outcome::ProtocolError(_)) {
        tokio::time::sleep(Duration::from_secs(15)).await;
        outcome = classify(call_tool(target.client(), tool, args.clone()).await);
    }
    let elapsed = started.elapsed();
    let remote = target.is_remote();
    target.shutdown().await;

    let adapter = adapter_for(tool);
    let (kind, text) = match &outcome {
        Outcome::Data(t) => ("data", t),
        Outcome::ToolError(t) => ("tool error", t),
        Outcome::ProtocolError(t) => ("protocol error", t),
    };
    println!("{tool} {args} → {kind} in {elapsed:.1?}\n{}", preview(text));

    if !matches!(outcome, Outcome::Data(_)) {
        let tolerated = expect
            .tolerated_errors
            .iter()
            .any(|needle| text.contains(needle));
        assert!(
            tolerated,
            "{tool}: {kind} after {elapsed:.1?} — adapter {adapter}\nargs: {args}\n{}",
            preview(text)
        );
        return text.clone();
    }

    // A deployed server holds its own keys; for the local binary the env decides.
    let key_missing = !remote
        && expect
            .keyed_by
            .is_some_and(|var| std::env::var(var).map(|v| v.is_empty()).unwrap_or(true));
    if key_missing {
        assert!(
            text.contains("not configured"),
            "{tool}: {} is unset but the output is not the 'not configured' text — adapter {adapter}\nargs: {args}\n{}",
            expect.keyed_by.unwrap_or_default(),
            preview(text)
        );
        return text.clone();
    }

    let missing: Vec<&str> = expect
        .markers
        .iter()
        .copied()
        .filter(|m| !text.contains(m))
        .collect();
    assert!(
        missing.is_empty(),
        "{tool}: output lost {missing:?}, so the upstream or the formatter changed shape — adapter {adapter}\nargs: {args}\n{}",
        preview(text)
    );
    if let Some(entity) = undecoded_entity(text) {
        panic!(
            "{tool}: output contains the undecoded HTML entity `{entity}` — adapter {adapter}\nargs: {args}\n{}",
            preview(text)
        );
    }
    if let Some(check) = expect.check
        && let Err(reason) = check(text)
    {
        panic!(
            "{tool}: {reason} — adapter {adapter}\nargs: {args}\n{}",
            preview(text)
        );
    }
    text.clone()
}

/// The first raw HTML entity in `text`: an adapter passed upstream HTML through
/// without decoding it, so the model reads `&#8211;` instead of a dash.
fn undecoded_entity(text: &str) -> Option<&str> {
    static ENTITY: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"&(#[0-9]{2,6}|#x[0-9a-fA-F]{2,6}|amp|quot|lt|gt|nbsp|apos);")
            .expect("entity regex")
    });
    ENTITY.find(text).map(|m| m.as_str())
}

macro_rules! canary {
    ($name:ident, $tool:literal, $args:tt, $expect:expr) => {
        #[tokio::test]
        #[ignore = "live upstream canary"]
        async fn $name() {
            canary($tool, json!($args), $expect).await;
        }
    };
}

/// The UCSC quarter a date falls in ("Fall 2026") and the one before it, by
/// calendar month. Close enough for staleness checks; not the registrar's dates.
fn quarter_labels(date: chrono::NaiveDate) -> (String, String) {
    let y = date.year();
    match date.month() {
        1..=3 => (format!("Winter {y}"), format!("Fall {}", y - 1)),
        4..=6 => (format!("Spring {y}"), format!("Winter {y}")),
        7..=9 => (format!("Summer {y}"), format!("Spring {y}")),
        _ => (format!("Fall {y}"), format!("Summer {y}")),
    }
}

/// The group-exercise adapter pins one term's goslugs.com page, so it keeps
/// serving last term's classes until someone bumps the URL. Require the
/// current quarter, allowing the previous one for three weeks while a new
/// term's page goes up.
fn group_exercise_is_current(text: &str) -> Result<(), String> {
    let today = chrono::Utc::now()
        .with_timezone(&chrono_tz::US::Pacific)
        .date_naive();
    let (current, previous) = quarter_labels(today);
    let grace = matches!(today.month(), 1 | 4 | 7 | 10) && today.day() <= 21;
    if text.contains(&current) || (grace && text.contains(&previous)) {
        Ok(())
    } else {
        Err(format!(
            "the schedule is not for {current}: point GROUP_EXERCISE_URL at the current term's page"
        ))
    }
}

// ─── Campus ───

canary!(
    dining_hours,
    "get_dining_hours",
    {},
    Expect::markers(&["# UCSC Dining Hours", "\n### "])
);

/// Menu → nutrition label, chained on a recipe id the menu printed.
#[tokio::test]
#[ignore = "live upstream canary"]
async fn dining_menu_then_nutrition_label() {
    let menu = canary(
        "get_dining_menu",
        json!({ "hall": "College Nine" }),
        Expect::markers(&["College Nine Dining Hall", "### ", "(recipe: "]),
    )
    .await;
    let recipe = menu
        .split("(recipe: ")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .expect("menu printed at least one recipe id")
        .to_string();
    canary(
        "get_nutrition_info",
        json!({ "recipe_id": recipe }),
        Expect::markers(&["**Serving Size:**", "| Nutrient | Amount |"]),
    )
    .await;
}

/// Occupancy → schedule, chained on a facility UUID from the occupancy list.
#[tokio::test]
#[ignore = "live upstream canary"]
async fn recreation_occupancy_then_schedule() {
    let occupancy = canary(
        "get_facility_occupancy",
        json!({}),
        Expect::markers(&[
            "## UCSC Recreation Facility Occupancy",
            "**Occupancy**",
            "**ID**: `",
        ]),
    )
    .await;
    let uuid = occupancy
        .split("**ID**: `")
        .nth(1)
        .and_then(|rest| rest.split('`').next())
        .expect("occupancy printed at least one facility id")
        .to_string();
    canary(
        "get_facility_schedule",
        json!({ "facility_id": uuid }),
        Expect::markers(&["## Schedule:"]),
    )
    .await;
}

canary!(
    recreation_group_exercise,
    "get_group_exercise_classes",
    {},
    Expect::markers(&["## UCSC Group Exercise Schedule", "### "])
        .checking(group_exercise_is_current)
);
canary!(
    library_study_rooms,
    "get_study_room_availability",
    { "library": "McHenry" },
    Expect::markers(&["## McHenry Library", "Reserve a room"])
);
canary!(
    academics_class_search,
    "search_classes",
    { "subject": "CSE", "course_number": "115A" },
    Expect::markers(&["## Class Search Results", "115A"])
);
canary!(
    academics_directory,
    "search_directory",
    { "query": "registrar" },
    Expect::markers(&["## Directory Search:"])
);
canary!(
    classrooms_search,
    "search_classrooms",
    { "name": "Thimann" },
    Expect::markers(&["## UCSC Classrooms (", "Thimann"])
);

/// Requirements → progress, the two halves of the degree scraper.
#[tokio::test]
#[ignore = "live upstream canary"]
async fn degrees_requirements_then_progress() {
    canary(
        "get_degree_requirements",
        json!({ "program": "Computer Science BS" }),
        Expect::markers(&["Requirements"]),
    )
    .await;
    canary(
        "check_degree_progress",
        json!({ "program": "Computer Science BS", "completed_courses": ["CSE 12", "CSE 30", "MATH 19A"] }),
        Expect::markers(&["# Degree Progress:"]),
    )
    .await;
}

canary!(
    summer_deadlines,
    "get_summer_deadlines",
    {},
    Expect::markers(&["# UCSC Summer"])
);

// ─── Events ───

canary!(
    events_upcoming,
    "get_upcoming_events",
    { "limit": 5 },
    Expect::markers(&["# Upcoming UCSC Events", "**When**"])
);
canary!(
    events_search,
    "search_events",
    { "query": "music", "limit": 5 },
    Expect::markers(&["**When**"])
);
// Eventbrite's CDN answers data-center IPs (GitHub runners, Fly) with a
// CAPTCHA page; that refusal is documented upstream behaviour, not drift.
canary!(
    events_eventbrite,
    "search_eventbrite_events",
    { "query": "music", "limit": 5 },
    Expect::markers(&[]).tolerating(&[
        "discover page returned 403",
        "discover page returned 405",
        "discover page returned 429",
    ])
);

// ─── Transit ───

canary!(
    transit_predictions,
    "get_bus_predictions",
    { "stop": "Science Hill" },
    Expect::keyed("SLUG_MCP_BUSTIME_KEY", &["Bus arrivals at", "Science Hill"])
);
canary!(
    transit_vehicles,
    "get_vehicle_positions",
    {},
    Expect::markers(&["# Santa Cruz Metro — Live vehicles system-wide"])
);
canary!(
    transit_route_delays,
    "get_route_delays",
    {},
    Expect::markers(&["delay"])
);
canary!(
    transit_alerts,
    "get_service_alerts",
    {},
    Expect::markers(&["# Santa Cruz Metro — System Service Alerts"])
);

// ─── Roads ───

canary!(
    traffic_incidents,
    "get_traffic_incidents",
    {},
    Expect::markers(&["# CHP Incidents"])
);
canary!(
    traffic_lane_closures,
    "get_lane_closures",
    { "route": "9" },
    Expect::markers(&["# Caltrans D5 Lane Closures"])
);
canary!(
    traffic_summary,
    "get_traffic_summary",
    {},
    Expect::markers(&[
        "# Santa Cruz Traffic Summary",
        "## CHP incidents",
        "## Caltrans lane closures",
    ])
);

canary!(
    traffic_cameras,
    "get_traffic_cameras",
    { "route": "17" },
    Expect::markers(&[
        "# Caltrans Traffic Cameras — Route 17",
        "[Current image](https://cwwp2.dot.ca.gov/",
    ])
);
// BCycle has said it will likely leave campus after Fall 2026. When the UCSC
// stations go, this fails on "open dock", and the tool needs retiring.
canary!(
    bike_share,
    "get_bike_share",
    {},
    Expect::markers(&["# Santa Cruz BCycle: UCSC campus stations", "open dock"])
);

// ─── Weather, air, sky ───

canary!(
    weather_forecast,
    "get_weather_forecast",
    {},
    Expect::markers(&["# Santa Cruz Weather Forecast", "°F"])
);
canary!(
    weather_alerts,
    "get_weather_alerts",
    {},
    Expect::markers(&["# NWS Active Alerts"])
);
canary!(
    air_quality_airnow,
    "get_air_quality",
    {},
    Expect::keyed("AIRNOW_API_KEY", &["# Air quality"])
);
canary!(
    air_quality_forecast,
    "get_air_quality_forecast",
    {},
    Expect::markers(&["# Air Quality Forecast", "## Hourly Forecast"])
);
canary!(
    sun_moon,
    "get_sun_moon",
    { "days": 1 },
    Expect::markers(&["# Sun, Moon & UV", "## Today", "## UV Index"])
);
canary!(
    space_weather,
    "get_space_weather",
    {},
    Expect::markers(&["# Space Weather Summary", "## Current Conditions"])
);
canary!(
    fire_detections,
    "get_fire_detections",
    {},
    Expect::keyed(
        "SLUG_MCP_FIRMS_KEY",
        &["# Santa Cruz County fire detections"]
    )
);
canary!(
    earthquakes,
    "get_earthquakes",
    {},
    Expect::markers(&["# Recent Earthquakes", "## Context"])
);

// ─── Ocean & coast ───

canary!(
    tides,
    "get_tides",
    { "days": 1 },
    Expect::markers(&["# Tide predictions", "\n## "])
);
canary!(
    surf_conditions,
    "get_surf_conditions",
    {},
    Expect::markers(&["# Santa Cruz Surf Conditions", "## Steamer Lane"])
);
canary!(
    buoy_observations,
    "get_buoy_observations",
    {},
    Expect::markers(&["# Buoy observations", "**Observed**"])
);
canary!(
    wave_buoy,
    "get_wave_buoy",
    {},
    Expect::markers(&["# Wave Buoy — Spectral Summary", "\n## "])
);
canary!(
    beach_water_quality,
    "get_beach_water_quality",
    { "beach": "Cowell" },
    Expect::markers(&["# Beach Water Quality", "## Cowell"])
);
canary!(
    stream_conditions,
    "get_stream_conditions",
    {},
    Expect::markers(&["# Stream conditions", "**Streamflow**"])
);
canary!(
    wharf_state,
    "get_sc_wharf_state",
    {},
    Expect::markers(&["# UCSC Santa Cruz Municipal Wharf", "## Current readings"])
);
canary!(
    m1_water_column,
    "get_m1_water_column",
    {},
    Expect::markers(&["# M1 Mooring", "## Surface"])
);
canary!(
    sst_state,
    "get_sst_state",
    {},
    Expect::markers(&["# MUR SST", "**Snapshot**"])
);
// HFRNet's latest snapshot is sometimes older than the tool's 6 h window; the
// tool reports that explicitly (documented in docs/audit-2026-09-03.md).
canary!(
    hfradar_currents,
    "get_hfradar_currents",
    {},
    Expect::markers(&["# HF Radar Surface Currents"]).tolerating(&["no valid data"])
);
canary!(
    upwelling_indices,
    "get_upwelling_indices",
    {},
    Expect::markers(&["# Coastal upwelling indices", "## Rolling averages"])
);
canary!(
    hab_risk_forecast,
    "get_hab_risk_forecast",
    {},
    Expect::markers(&["# C-HARM", "| Nowcast |"])
);
canary!(
    fusion_hab_risk,
    "monterey_bay_hab_risk",
    {},
    Expect::markers(&["# Monterey Bay HAB Risk Summary"])
);
canary!(
    fusion_upwelling_state,
    "monterey_bay_upwelling_state",
    {},
    Expect::markers(&["# Monterey Bay Upwelling State"])
);

#[tokio::test]
#[ignore = "live upstream canary"]
async fn fusion_research_snapshot_is_json_with_a_checksum() {
    let text = canary(
        "monterey_bay_research_snapshot",
        json!({}),
        Expect::markers(&["deterministic_checksum"]),
    )
    .await;
    let snapshot: Value = serde_json::from_str(text.trim()).unwrap_or_else(|e| {
        panic!(
            "monterey_bay_research_snapshot: output is not JSON ({e}) — adapter {}\n{}",
            adapter_for("monterey_bay_research_snapshot"),
            preview(&text)
        )
    });
    assert!(
        snapshot
            .get("deterministic_checksum")
            .and_then(|v| v.as_str())
            .is_some(),
        "monterey_bay_research_snapshot: deterministic_checksum is not a string — adapter {}\n{}",
        adapter_for("monterey_bay_research_snapshot"),
        preview(&text)
    );
}

// ─── Outdoors & biodiversity ───

canary!(
    climbing_routes,
    "search_climbing_routes",
    { "area": "Santa Cruz" },
    Expect::markers(&["# Climbing"])
);
canary!(
    national_park_info,
    "get_national_park_info",
    { "park_code": "pinn" },
    Expect::keyed("NPS_API_KEY", &["Pinnacles"])
);
canary!(
    birds_hotspots,
    "get_bird_hotspots",
    {},
    Expect::keyed("EBIRD_API_KEY", &["# eBird hotspots"])
);
canary!(
    birds_recent,
    "search_bird_observations",
    { "notable_only": true, "back": 14 },
    Expect::keyed("EBIRD_API_KEY", &["# eBird recent observations"])
);
canary!(
    birds_nearest,
    "find_nearest_bird_sighting",
    { "species": "Great Horned Owl" },
    Expect::keyed("EBIRD_API_KEY", &["# Nearest eBird sightings"])
);
canary!(
    birds_historic,
    "get_historic_bird_observations",
    { "year": 2025, "month": 9, "day": 3 },
    Expect::keyed("EBIRD_API_KEY", &["# eBird historic observations"])
);
canary!(
    species_inaturalist,
    "search_species_observations",
    { "query": "banana slug" },
    Expect::markers(&["# iNaturalist observations", "observations._"])
);

/// Every served public tool has an ADAPTERS row and a canary that calls it, and
/// every row is for a tool the server still serves. Fails when a tool is added
/// without a canary, and, against a deployed server, when it lags this branch.
#[tokio::test]
#[ignore = "live upstream canary"]
async fn every_public_tool_has_a_canary() {
    // Auth tools need a CruzID session and a browser, so they have no canary.
    const AUTH_ONLY: &[&str] = &["login", "authenticate", "check_auth", "get_meal_balance"];

    let target = Target::connect().await;
    let tools = target.client().list_all_tools().await.expect("tools/list");
    target.shutdown().await;
    let served: Vec<String> = tools
        .iter()
        .map(|t| t.name.to_string())
        .filter(|n| !AUTH_ONLY.contains(&n.as_str()))
        .collect();

    // This file names each tool once in ADAPTERS and again in the canary that
    // calls it, so fewer than two quoted mentions means no canary.
    let source = include_str!("live_canary.rs");
    let mut problems = Vec::new();
    for tool in &served {
        if !ADAPTERS.iter().any(|(name, _)| name == tool) {
            problems.push(format!("{tool}: no ADAPTERS row"));
        }
        if source.matches(&format!("\"{tool}\"")).count() < 2 {
            problems.push(format!("{tool}: no canary calls it"));
        }
    }
    for (name, _) in ADAPTERS {
        if !served.iter().any(|s| s == name) {
            problems.push(format!(
                "{name}: has a canary but the server does not serve it"
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "tool surface and canaries disagree:\n{}",
        problems.join("\n")
    );
}

#[test]
fn undecoded_entities_are_found() {
    assert_eq!(undecoded_entity("NTW26 &#8211; Transfer"), Some("&#8211;"));
    assert_eq!(undecoded_entity("Tom &amp; Jerry"), Some("&amp;"));
    assert_eq!(undecoded_entity("Cowell & Stevenson · a=1&b=2"), None);
}

#[test]
fn quarter_labels_follow_the_calendar() {
    let d = |m, day| chrono::NaiveDate::from_ymd_opt(2026, m, day).unwrap();
    assert_eq!(quarter_labels(d(10, 19)).0, "Fall 2026");
    assert_eq!(
        quarter_labels(d(2, 1)),
        ("Winter 2026".into(), "Fall 2025".into())
    );
    assert_eq!(quarter_labels(d(5, 1)).0, "Spring 2026");
    assert_eq!(quarter_labels(d(8, 1)).0, "Summer 2026");
}
