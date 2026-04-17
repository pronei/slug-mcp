use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
#[cfg(feature = "auth")]
use tokio::sync::RwLock;

use crate::academics::{AcademicsService, SearchClassesRequest, SearchDirectoryRequest};
use crate::biodiversity::{BiodiversityService, BirdRequest, SpeciesRequest};
use crate::buoy::{BuoyRequest, BuoyService};
#[cfg(feature = "auth")]
use crate::auth::session::SessionData;
#[cfg(feature = "auth")]
use crate::auth::AuthManager;
use crate::cache::CacheStore;
use crate::classrooms::{ClassroomService, SearchClassroomsRequest};
use crate::config::Config;
use crate::degrees::{DegreeProgressRequest, DegreeRequirementsRequest, DegreeService};
use crate::dining::{DiningHoursRequest, DiningMenuRequest, DiningService, NutritionRequest};
use crate::events::{EventsService, SearchEventbriteRequest, SearchEventsRequest, UpcomingEventsRequest};
use crate::fire::{FireDetectionsRequest, FireService};
#[cfg(feature = "auth")]
use crate::library::BookStudyRoomRequest;
use crate::library::{LibraryService, StudyRoomAvailabilityRequest};
use crate::marine::{MarineForecastRequest, MarineService, SurfConditionsRequest};
use crate::recreation::{
    FacilityOccupancyRequest, FacilityScheduleRequest, GroupExerciseRequest, RecreationService,
};
use crate::tides::{TidesRequest, TidesService};
use crate::traffic::{TrafficRequest, TrafficService};
use crate::usgs_water::{StreamConditionsRequest, UsgsWaterService};
use crate::wave_buoy::{WaveBuoyRequest, WaveBuoyService};
use crate::transit::TransitService;
use crate::weather::{WeatherForecastRequest, WeatherService};

fn internal_err(e: impl std::fmt::Display) -> ErrorData {
    ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None)
}

/// Shared service dependencies, constructed once and cloned into each MCP session.
#[derive(Clone)]
pub struct ServiceContext {
    pub config: Arc<Config>,
    pub cache: Arc<CacheStore>,
    #[cfg(feature = "auth")]
    pub auth: Arc<AuthManager>,
    pub degrees: Arc<DegreeService>,
    pub dining: Arc<DiningService>,
    pub events: Arc<EventsService>,
    pub recreation: Arc<RecreationService>,
    pub library: Arc<LibraryService>,
    pub academics: Arc<AcademicsService>,
    pub classrooms: Arc<ClassroomService>,
    pub transit: Arc<TransitService>,
    pub weather: Arc<WeatherService>,
    pub marine: Arc<MarineService>,
    pub fire: Arc<FireService>,
    pub traffic: Arc<TrafficService>,
    pub tides: Arc<TidesService>,
    pub buoy: Arc<BuoyService>,
    pub wave_buoy: Arc<WaveBuoyService>,
    pub usgs_water: Arc<UsgsWaterService>,
    pub biodiversity: Arc<BiodiversityService>,
}

#[derive(Clone)]
pub struct SlugMcpServer {
    #[allow(dead_code)]
    config: Arc<Config>,
    #[allow(dead_code)]
    cache: Arc<CacheStore>,
    #[cfg(feature = "auth")]
    auth: Arc<AuthManager>,
    #[cfg(feature = "auth")]
    /// Per-session auth state for SSE mode (set via `authenticate` tool).
    session_auth: Arc<RwLock<Option<SessionData>>>,
    degrees: Arc<DegreeService>,
    dining: Arc<DiningService>,
    events: Arc<EventsService>,
    recreation: Arc<RecreationService>,
    library: Arc<LibraryService>,
    academics: Arc<AcademicsService>,
    classrooms: Arc<ClassroomService>,
    transit: Arc<TransitService>,
    weather: Arc<WeatherService>,
    marine: Arc<MarineService>,
    fire: Arc<FireService>,
    traffic: Arc<TrafficService>,
    tides: Arc<TidesService>,
    buoy: Arc<BuoyService>,
    wave_buoy: Arc<WaveBuoyService>,
    usgs_water: Arc<UsgsWaterService>,
    biodiversity: Arc<BiodiversityService>,
    tool_router: ToolRouter<Self>,
}

impl SlugMcpServer {
    pub fn new(ctx: ServiceContext) -> Self {
        Self {
            config: ctx.config,
            cache: ctx.cache,
            #[cfg(feature = "auth")]
            auth: ctx.auth,
            #[cfg(feature = "auth")]
            session_auth: Arc::new(RwLock::new(None)),
            degrees: ctx.degrees,
            dining: ctx.dining,
            events: ctx.events,
            recreation: ctx.recreation,
            library: ctx.library,
            academics: ctx.academics,
            classrooms: ctx.classrooms,
            transit: ctx.transit,
            weather: ctx.weather,
            marine: ctx.marine,
            fire: ctx.fire,
            traffic: ctx.traffic,
            tides: ctx.tides,
            buoy: ctx.buoy,
            wave_buoy: ctx.wave_buoy,
            usgs_water: ctx.usgs_water,
            biodiversity: ctx.biodiversity,
            tool_router: Self::tool_router(),
        }
    }

    #[cfg(feature = "auth")]
    /// Get the active session from either per-session token (SSE) or disk (stdio).
    async fn get_active_session(&self) -> Option<SessionData> {
        // 1. Check per-session token (set via `authenticate` tool)
        if let Some(data) = self.session_auth.read().await.as_ref() {
            if !data.is_expired() {
                return Some(data.clone());
            }
        }
        // 2. Fall back to disk-based session (set via `login` tool)
        self.auth.get_session().ok().flatten()
    }
}

// ─── Authentication ───

#[cfg(feature = "auth")]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AuthenticateRequest {
    /// Portable auth token from `slug-mcp export-token`. Base64-encoded session data.
    pub token: String,
}

// ─── Transit ───

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BusPredictionRequest {
    /// Stop name to search for (e.g., "Science Hill", "Metro Center", "Oakes College").
    pub stop: String,
    /// Route number to filter (e.g., "10", "15"). If omitted, shows all routes at the stop.
    pub route: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ServiceAlertRequest {
    /// Route number to check alerts for (e.g., "10", "15"). At least one of route or stop_id should be specified.
    pub route: Option<String>,
    /// Stop ID to check alerts for (e.g., "1234"). At least one of route or stop_id should be specified.
    pub stop_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TransitRouteRequest {
    /// Route number to filter (e.g. "10", "17", "20"). If omitted, shows all routes.
    pub route: Option<String>,
}

// ─── Tool definitions ───
// The `define_tools!` macro wraps the `#[tool_router]` impl so that public tools
// are defined once, while auth-only tools are injected via the `$extra` parameter.
// Declarative macros expand before proc macros, so `#[tool_router]` sees the
// fully-expanded impl block regardless of which cfg variant is active.

macro_rules! define_tools {
    ({ $($extra:tt)* }) => {
        #[tool_router]
        impl SlugMcpServer {
            // ─── Dining Tools ───

            #[tool(description = "Get the menu for a UCSC dining hall")]
            async fn get_dining_menu(
                &self,
                Parameters(req): Parameters<DiningMenuRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .dining
                    .get_menu(
                        req.hall.as_deref(),
                        req.meal.as_deref(),
                        req.date.as_deref(),
                        req.include_all_categories.unwrap_or(false),
                    )
                    .await
                    .map_err(internal_err)?;

                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            #[tool(description = "Get detailed nutrition facts for a specific menu item. Use the recipe ID from get_dining_menu output.")]
            async fn get_nutrition_info(
                &self,
                Parameters(req): Parameters<NutritionRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .dining
                    .get_nutrition(&req.recipe_id)
                    .await
                    .map_err(internal_err)?;

                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            #[tool(description = "Get UCSC dining location hours. Optionally filter by location name.")]
            async fn get_dining_hours(
                &self,
                Parameters(req): Parameters<DiningHoursRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let hours = self
                    .dining
                    .get_hours(req.location.as_deref())
                    .await
                    .map_err(internal_err)?;

                Ok(CallToolResult::success(vec![Content::text(hours)]))
            }

            // ─── Events Tools ───

            #[tool(description = "Search for UCSC campus events by keyword or category. For broader event discovery, also call search_eventbrite_events to include off-campus and community events.")]
            async fn search_events(
                &self,
                Parameters(req): Parameters<SearchEventsRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let events = self
                    .events
                    .search_events(
                        req.query.as_deref(),
                        None,
                        req.category.as_deref(),
                        req.limit,
                    )
                    .await
                    .map_err(internal_err)?;

                if events.is_empty() {
                    return Ok(CallToolResult::success(vec![Content::text(
                        "No events found matching your search.",
                    )]));
                }

                let formatted: Vec<String> = events.iter().map(|e| e.format_summary()).collect();
                Ok(CallToolResult::success(vec![Content::text(
                    formatted.join("\n---\n\n"),
                )]))
            }

            #[tool(description = "Get upcoming UCSC campus events. For a complete picture of what's happening in the area, also call search_eventbrite_events.")]
            async fn get_upcoming_events(
                &self,
                Parameters(req): Parameters<UpcomingEventsRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let limit = req.limit.unwrap_or(10);
                let events = self
                    .events
                    .get_upcoming_events(limit)
                    .await
                    .map_err(internal_err)?;

                if events.is_empty() {
                    return Ok(CallToolResult::success(vec![Content::text(
                        "No upcoming events found.",
                    )]));
                }

                let formatted: Vec<String> = events.iter().map(|e| e.format_summary()).collect();
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "# Upcoming UCSC Events\n\n{}",
                    formatted.join("\n---\n\n")
                ))]))
            }

            #[tool(description = "Search Eventbrite for community events, concerts, meetups, and workshops around Santa Cruz (25-mile radius). Complements UCSC campus event tools — call both for complete event coverage. Returns direct registration links.")]
            async fn search_eventbrite_events(
                &self,
                Parameters(req): Parameters<SearchEventbriteRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let events = self
                    .events
                    .search_eventbrite(
                        req.query.as_deref(),
                        req.location.as_deref(),
                        req.limit,
                    )
                    .await
                    .map_err(internal_err)?;

                if events.is_empty() {
                    return Ok(CallToolResult::success(vec![Content::text(
                        "No Eventbrite events found matching your search near Santa Cruz.",
                    )]));
                }

                let formatted: Vec<String> = events.iter().map(|e| e.format_summary()).collect();
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "# Eventbrite Events near Santa Cruz\n\n{}",
                    formatted.join("\n---\n\n")
                ))]))
            }

            // ─── Recreation Tools ───

            #[tool(description = "Get current occupancy for UCSC recreation facilities (gym, pool, fields, climbing wall). Shows live headcounts.")]
            async fn get_facility_occupancy(
                &self,
                Parameters(req): Parameters<FacilityOccupancyRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .recreation
                    .get_occupancy(req.facility.as_deref())
                    .await
                    .map_err(internal_err)?;

                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            #[tool(description = "Get the schedule for a specific UCSC recreation facility. Use the facility UUID from get_facility_occupancy output.")]
            async fn get_facility_schedule(
                &self,
                Parameters(req): Parameters<FacilityScheduleRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .recreation
                    .get_schedule(&req.facility_id)
                    .await
                    .map_err(internal_err)?;

                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            #[tool(description = "Get UCSC group exercise class schedule (Spring 2026). Classes include yoga, pilates, cycling, kickboxing, Zumba, conditioning, and self-defense. Filter by day of week and/or class name.")]
            async fn get_group_exercise_classes(
                &self,
                Parameters(req): Parameters<GroupExerciseRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .recreation
                    .get_group_exercise(req.day.as_deref(), req.class_name.as_deref())
                    .await
                    .map_err(internal_err)?;

                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            // ─── Library Tools ───

            #[tool(description = "Get available study rooms at UCSC libraries (McHenry, Science & Engineering). Shows room availability by time slot.")]
            async fn get_study_room_availability(
                &self,
                Parameters(req): Parameters<StudyRoomAvailabilityRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .library
                    .get_availability(req.library.as_deref(), req.date.as_deref())
                    .await
                    .map_err(internal_err)?;

                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            // ─── Academics Tools ───

            #[tool(description = "Search the UCSC class schedule. Filter by subject, course number, instructor, title, or GE requirement. Returns enrollment counts and meeting times.")]
            async fn search_classes(
                &self,
                Parameters(req): Parameters<SearchClassesRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .academics
                    .search_classes(
                        req.term.as_deref(),
                        req.subject.as_deref(),
                        req.course_number.as_deref(),
                        req.instructor.as_deref(),
                        req.title.as_deref(),
                        req.ge.as_deref(),
                        req.career.as_deref(),
                        req.open_only.unwrap_or(false),
                        req.page,
                    )
                    .await
                    .map_err(internal_err)?;

                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            #[tool(description = "Search the UCSC campus directory for people or departments. Find faculty/staff contact info, office locations, and emails.")]
            async fn search_directory(
                &self,
                Parameters(req): Parameters<SearchDirectoryRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .academics
                    .search_directory(&req.query, req.search_type.as_deref())
                    .await
                    .map_err(internal_err)?;

                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            // ─── Degree Planner Tools ───

            #[tool(description = "Get degree requirements for a UCSC program. Returns the full course requirement tree including lower-division, upper-division, electives, and comprehensive requirements. Includes GE requirements for undergraduate programs. Supports all BS/BA/MS/MA programs.")]
            async fn get_degree_requirements(
                &self,
                Parameters(req): Parameters<DegreeRequirementsRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .degrees
                    .get_requirements(&req.program)
                    .await
                    .map_err(internal_err)?;

                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            #[tool(description = "Check progress toward completing a UCSC degree. Provide your program and list of completed courses to see which requirements are satisfied vs remaining. Handles all selection rules (all-of, one-of, either-or). Checks GE progress for undergrad programs.")]
            async fn check_degree_progress(
                &self,
                Parameters(req): Parameters<DegreeProgressRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .degrees
                    .check_progress(
                        &req.program,
                        &req.completed_courses,
                        req.completed_ge.as_deref(),
                    )
                    .await
                    .map_err(internal_err)?;

                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            // ─── Transit Tools ───

            #[tool(description = "Real-time bus arrival predictions for a Santa Cruz Metro stop. Search by stop name; optionally filter by route. Primary source is GTFS-RT (no per-call API key, rich vehicle positions and occupancy). Automatically falls back to BusTime when GTFS-RT has no absolute-time data for the matched stop — BusTime adds destination headsigns, DUE/DLY countdown labels, and canceled/express trip flags. Output footer shows which backend answered. Both sources report passenger load and delays. For system-wide queries (no specific stop) prefer `get_system_service_alerts`, `get_vehicle_positions`, or `get_route_delays` — those don't need a key. All UCSC students ride free with student ID.")]
            async fn get_bus_predictions(
                &self,
                Parameters(req): Parameters<BusPredictionRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .transit
                    .get_predictions(&req.stop, req.route.as_deref())
                    .await
                    .map_err(internal_err)?;

                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            #[tool(description = "Get active service alerts and bulletins for Santa Cruz Metro bus routes. Shows detours, disruptions, and schedule changes. Specify a route number or stop ID. Backed by the BusTime bulletin API (requires key).")]
            async fn get_service_alerts(
                &self,
                Parameters(req): Parameters<ServiceAlertRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .transit
                    .get_service_alerts(req.route.as_deref(), req.stop_id.as_deref())
                    .await
                    .map_err(internal_err)?;

                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            #[tool(description = "Get system-wide Santa Cruz Metro service alerts via the GTFS-RT alerts feed. No API key required, covers all active alerts across the system.")]
            async fn get_system_service_alerts(&self) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .transit
                    .get_system_alerts()
                    .await
                    .map_err(internal_err)?;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            #[tool(description = "Get live Santa Cruz Metro bus positions via the GTFS-RT vehicles feed. Shows lat/lon, speed, and occupancy for active buses. Optionally filter by route. No API key required.")]
            async fn get_vehicle_positions(
                &self,
                Parameters(req): Parameters<TransitRouteRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .transit
                    .get_vehicle_positions(req.route.as_deref())
                    .await
                    .map_err(internal_err)?;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            #[tool(description = "Get per-route delay statistics for Santa Cruz Metro via GTFS-RT trip updates. Shows average and max delay per route across all currently-active trips. Optionally filter by route. No API key required.")]
            async fn get_route_delays(
                &self,
                Parameters(req): Parameters<TransitRouteRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .transit
                    .get_route_delays(req.route.as_deref())
                    .await
                    .map_err(internal_err)?;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            // ─── Weather Tools ───

            #[tool(description = "Get the multi-period NOAA National Weather Service forecast for Santa Cruz. Covers the next several days (NWS returns ~2 periods per day: daytime + overnight) with temperature, wind, short/long descriptions, and precipitation probability.")]
            async fn get_weather_forecast(
                &self,
                Parameters(req): Parameters<WeatherForecastRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let periods = req.periods.unwrap_or(7);
                let result = self
                    .weather
                    .get_forecast(periods)
                    .await
                    .map_err(internal_err)?;

                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            #[tool(description = "Get active NOAA NWS weather alerts for Santa Cruz coastal (CAZ529) and mountain (CAZ512) public forecast zones. Covers high-wind, flood, winter storm, fire-weather, marine, and other watch/warning/advisory events.")]
            async fn get_weather_alerts(&self) -> Result<CallToolResult, ErrorData> {
                let result = self.weather.get_alerts().await.map_err(internal_err)?;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            // ─── Marine / Surf Tools ───

            #[tool(description = "Get current surf conditions. Without any parameters, compares all known SC surf spots side-by-side. With `spot`, returns conditions for a named spot (Steamer Lane, Pleasure Point, Cowell's, Natural Bridges, The Hook, Manresa). With `lat`+`lon`, returns conditions for any coastal coordinates — use `label` to give it a name. Shows wave/swell height in feet, period, direction, and local wind.")]
            async fn get_surf_conditions(
                &self,
                Parameters(req): Parameters<SurfConditionsRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .marine
                    .get_surf_conditions(
                        req.spot.as_deref(),
                        req.lat,
                        req.lon,
                        req.label.as_deref(),
                    )
                    .await
                    .map_err(internal_err)?;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            #[tool(description = "Get the full Open-Meteo marine forecast (next 12 hourly timesteps) for a named Santa Cruz surf spot or custom lat/lon. Includes wave height, period, direction, and swell components. Useful for planning surf or water activities further out than the 'now' snapshot.")]
            async fn get_marine_forecast(
                &self,
                Parameters(req): Parameters<MarineForecastRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .marine
                    .get_marine_forecast(req.spot.as_deref(), req.lat, req.lon)
                    .await
                    .map_err(internal_err)?;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            // ─── Biodiversity Tools ───

            #[tool(description = "Search iNaturalist for recent species observations in the Santa Cruz area (default 25 km around downtown). Filter by free-text query, iconic taxon (Plantae/Animalia/Fungi/Mollusca/Aves/Mammalia/Reptilia/etc.), days back, or custom lat/lon. No API key required. Returns observer, date, location, and a link to each observation.")]
            async fn search_species_observations(
                &self,
                Parameters(req): Parameters<SpeciesRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .biodiversity
                    .search_species(
                        req.query.as_deref(),
                        req.lat,
                        req.lon,
                        req.radius_km,
                        req.days,
                        req.iconic_taxon.as_deref(),
                        req.limit,
                    )
                    .await
                    .map_err(internal_err)?;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            #[tool(description = "Search eBird for recent bird observations around Santa Cruz (default 25 km, 7 days). Returns species, count, location, and observation date. Requires a free eBird API key (set EBIRD_API_KEY) — otherwise this tool returns registration instructions instead of erroring.")]
            async fn search_bird_observations(
                &self,
                Parameters(req): Parameters<BirdRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .biodiversity
                    .search_birds(req.lat, req.lon, req.radius_km, req.days, req.limit)
                    .await
                    .map_err(internal_err)?;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            // ─── USGS Stream / Water Tools ───

            #[tool(description = "Get real-time stream conditions from a USGS gauge (discharge in cfs, gage height in ft, water temperature). Defaults to site 11160500 (San Lorenzo River at Big Trees, the Santa Cruz County reference gauge). Pass a different USGS site ID or override parameter codes (`00060`=discharge, `00065`=gage height, `00010`=water temp).")]
            async fn get_stream_conditions(
                &self,
                Parameters(req): Parameters<StreamConditionsRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .usgs_water
                    .get_stream_conditions(
                        req.site.as_deref(),
                        req.parameters.as_deref(),
                    )
                    .await
                    .map_err(internal_err)?;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            // ─── Wave Buoy (CDIP) Tools ───

            #[tool(description = "Get swell vs wind-wave spectral summary from CDIP/NDBC waverider buoys. Unlike get_buoy_observations (single station, met+ocean summary), this compares multiple waveriders and separates swell height/period/direction from local wind-wave energy. Defaults to Monterey-area stations 46114 (CDIP 158 Pt. Sur), 46236 (CDIP 185 Monterey Canyon), 46042 (Monterey). Pass `stations` as comma-separated NDBC IDs to override.")]
            async fn get_wave_buoy(
                &self,
                Parameters(req): Parameters<WaveBuoyRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .wave_buoy
                    .get_wave_data(req.stations.as_deref())
                    .await
                    .map_err(internal_err)?;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            // ─── Buoy Tools ───

            #[tool(description = "Get real-time observations from an NDBC weather/ocean buoy. Defaults to station 46042 (Monterey Bay, the NOAA 3-meter discus offshore of Santa Cruz). Pass another NDBC station ID for other locations. Returns latest wind, significant wave height/period, air+water temperature, pressure, and dew point, plus a ~3h water-temp trend.")]
            async fn get_buoy_observations(
                &self,
                Parameters(req): Parameters<BuoyRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .buoy
                    .get_observations(req.station.as_deref())
                    .await
                    .map_err(internal_err)?;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            // ─── Tide Tools ───

            #[tool(description = "Get NOAA tide predictions (high/low) for a coastal station. Defaults to Monterey (station 9413450), the closest official tide station to Santa Cruz. Pass a different NOAA CO-OPS station ID for other locations. Returns up to 7 days (default 3) grouped by day with heights in feet above MLLW.")]
            async fn get_tides(
                &self,
                Parameters(req): Parameters<TidesRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .tides
                    .get_tides(req.station.as_deref(), req.days)
                    .await
                    .map_err(internal_err)?;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            // ─── Fire Detection Tools ───

            #[tool(description = "Get NASA FIRMS satellite fire/thermal-anomaly detections in Santa Cruz County for the last 1-5 days. Uses VIIRS_SNPP_NRT (375m resolution, ~60s latency). Detections include industrial heat sources (quarries, flares) as well as wildfires — cross-check with CAL FIRE before acting. Requires a free FIRMS map key (SLUG_MCP_FIRMS_KEY).")]
            async fn get_fire_detections(
                &self,
                Parameters(req): Parameters<FireDetectionsRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let days = req.days.unwrap_or(1);
                let result = self
                    .fire
                    .get_detections(days)
                    .await
                    .map_err(internal_err)?;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            // ─── Traffic Tools ───

            #[tool(description = "Get active CHP incidents in Santa Cruz County. Pulls from the CHP CAD XML feed (Monterey comm center / MYCC dispatch) and filters to SC County areas. Optionally filter by route number (e.g. '1', '9', '17').")]
            async fn get_traffic_incidents(
                &self,
                Parameters(req): Parameters<TrafficRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .traffic
                    .get_chp_incidents(req.route.as_deref())
                    .await
                    .map_err(internal_err)?;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            #[tool(description = "Get active Caltrans District 5 lane closures in Santa Cruz County, including planned and emergency closures. Optionally filter by route number (e.g. '1', '9', '17').")]
            async fn get_lane_closures(
                &self,
                Parameters(req): Parameters<TrafficRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .traffic
                    .get_lane_closures(req.route.as_deref())
                    .await
                    .map_err(internal_err)?;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            #[tool(description = "Get a combined Santa Cruz County traffic summary: active CHP incidents plus Caltrans D5 lane closures, fetched in parallel. If one source is unavailable, the other is still rendered with a warning. Use this for 'should I drive Hwy 17 right now?' style questions.")]
            async fn get_traffic_summary(
                &self,
                Parameters(req): Parameters<TrafficRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .traffic
                    .get_traffic_summary(req.route.as_deref())
                    .await
                    .map_err(internal_err)?;
                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            // ─── Classroom Tools ───

            #[tool(description = "Search UCSC classrooms by capacity, building, technology, and features. Find rooms with specific AV equipment or seating arrangements.")]
            async fn search_classrooms(
                &self,
                Parameters(req): Parameters<SearchClassroomsRequest>,
            ) -> Result<CallToolResult, ErrorData> {
                let result = self
                    .classrooms
                    .search(
                        req.name.as_deref(),
                        req.min_capacity,
                        req.max_capacity,
                        req.building.as_deref(),
                        req.technology.as_deref(),
                        req.feature.as_deref(),
                    )
                    .await
                    .map_err(internal_err)?;

                Ok(CallToolResult::success(vec![Content::text(result)]))
            }

            // ─── Extra tools (auth or empty) ───
            $($extra)*
        }
    };
}

// Full build: public tools + auth tools
#[cfg(feature = "auth")]
define_tools!({
    // ─── Auth Tools ───

    #[tool(description = "Login to UCSC SSO. This opens a Chrome window on your machine for CruzID + Duo MFA authentication — use this tool directly, it works from any MCP client including Claude Desktop. After you complete login in the browser, the session is captured automatically.")]
    async fn login(&self) -> Result<CallToolResult, ErrorData> {
        match self.auth.login().await {
            Ok(username) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Successfully logged in as **{}**. Session valid for 8 hours.",
                username
            ))])),
            Err(e) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Browser login failed: {}. If the server is running on a remote or headless machine, \
                 use the `authenticate` tool with a token from `slug-mcp export-token` run on a machine with a browser.",
                e
            ))])),
        }
    }

    #[tool(description = "Authenticate using a portable session token. Only needed when the MCP server is running on a remote or headless machine where a browser cannot open. Run `slug-mcp export-token` on a machine with a browser to get a token, then pass it here.")]
    async fn authenticate(
        &self,
        Parameters(req): Parameters<AuthenticateRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let session_data =
            crate::auth::token::decode_token(&req.token).map_err(|e| {
                ErrorData::new(ErrorCode::INVALID_PARAMS, e.to_string(), None)
            })?;

        let username = session_data.username.clone();
        let remaining_secs = (session_data.expires_at
            - std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64)
            .max(0) as u64;
        let hours = remaining_secs / 3600;
        let mins = (remaining_secs % 3600) / 60;

        *self.session_auth.write().await = Some(session_data);

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Authenticated as **{}**. Session valid for {}h {}m.",
            username, hours, mins
        ))]))
    }

    #[tool(description = "Check if you are currently authenticated with UCSC")]
    async fn check_auth(&self) -> Result<CallToolResult, ErrorData> {
        // Check per-session token first (SSE mode)
        if let Some(data) = self.session_auth.read().await.as_ref() {
            if !data.is_expired() {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                let remaining = (data.expires_at - now).max(0) as u64;
                let hours = remaining / 3600;
                let mins = (remaining % 3600) / 60;

                return Ok(CallToolResult::success(vec![Content::text(format!(
                    "Authenticated as **{}** (expires in {}h {}m) — via token",
                    data.username, hours, mins
                ))]));
            }
        }

        // Fall back to disk-based session (stdio mode)
        let status = self.auth.check_auth().map_err(internal_err)?;

        Ok(CallToolResult::success(vec![Content::text(status.format())]))
    }

    #[tool(description = "Get your UCSC meal plan balance (Slug Points, Banana Bucks). Requires authentication — call 'login' first if not already logged in.")]
    async fn get_meal_balance(&self) -> Result<CallToolResult, ErrorData> {
        let session = match self.get_active_session().await {
            Some(s) => s,
            None => {
                return Ok(CallToolResult::success(vec![Content::text(
                    "You need to log in first. Call the `login` tool to authenticate.",
                )]));
            }
        };

        // Build an authenticated client with IdP cookies for SAML auto-approval
        let client =
            crate::auth::build_authenticated_client(&session.cookies).map_err(internal_err)?;

        let result = self.dining.get_balance(&client).await.map_err(internal_err)?;

        let mut output = result.balance.to_string();

        // If parsing failed, include page snippet for debugging
        if let Some(snippet) = &result.debug_snippet {
            output.push_str(&format!(
                "\n\n---\n**Debug**: Balance page text:\n```\n{}\n```",
                snippet
            ));
        }

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    #[tool(description = "Book a study room at a UCSC library. Requires authentication — call 'login' first if not already logged in. Use space_id from get_study_room_availability.")]
    async fn book_study_room(
        &self,
        Parameters(req): Parameters<BookStudyRoomRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let session = match self.get_active_session().await {
            Some(s) => s,
            None => {
                return Ok(CallToolResult::success(vec![Content::text(
                    "You need to log in first. Call the `login` tool to authenticate.",
                )]));
            }
        };

        let result = self
            .library
            .book(&session.cookies, req.space_id, &req.date, &req.start_time, &req.end_time)
            .await
            .map_err(internal_err)?;

        Ok(CallToolResult::success(vec![Content::text(result)]))
    }
});

// Public-only build: no auth tools
#[cfg(not(feature = "auth"))]
define_tools!({});

#[tool_handler]
impl ServerHandler for SlugMcpServer {
    fn get_info(&self) -> ServerInfo {
        let instructions = if cfg!(feature = "auth") {
            "UCSC + Santa Cruz MCP server. Campus services: dining menus, \
             nutrition info, meal plan balances, campus events and Eventbrite \
             community events (use both event tools together for complete \
             coverage), recreation facility occupancy, group exercise class \
             schedules, library study room availability and booking, class \
             schedule search, campus directory \
             lookup, classroom search, real-time bus arrival predictions via \
             GTFS-RT, transit service alerts, system-wide SC Metro service \
             alerts, live vehicle positions, and per-route delay stats, \
             degree requirements lookup, and degree progress tracking. \
             Santa Cruz city/county services: 7-day NOAA NWS weather forecasts \
             and active alerts (coastal CAZ529 + mountains CAZ512), CHP \
             incidents and Caltrans District 5 lane closures for Hwy 1 / 9 / \
             17 / 101 (individually and combined), Open-Meteo marine forecasts \
             and surf conditions for Steamer Lane / Pleasure Point / Cowell's \
             / Natural Bridges / The Hook / Manresa, and NASA FIRMS satellite \
             wildfire detections for Santa Cruz County."
        } else {
            "UCSC + Santa Cruz MCP server (public mode). Campus services: \
             dining menus, nutrition info, campus events and Eventbrite \
             community events (use both event tools together for complete \
             coverage), recreation facility occupancy, group exercise class \
             schedules, library study room availability, class schedule \
             search, campus directory lookup, \
             classroom search, real-time bus arrival predictions via GTFS-RT, \
             transit service alerts, system-wide SC Metro service alerts, \
             live vehicle positions, and per-route delay stats, degree \
             requirements lookup, and degree progress tracking. Santa Cruz \
             city/county services: 7-day NOAA NWS weather forecasts and \
             active alerts (coastal CAZ529 + mountains CAZ512), CHP incidents \
             and Caltrans District 5 lane closures for Hwy 1 / 9 / 17 / 101, \
             Open-Meteo marine forecasts and surf conditions for the six \
             named SC surf spots (Steamer Lane, Pleasure Point, Cowell's, \
             Natural Bridges, The Hook, Manresa), and NASA FIRMS satellite \
             wildfire detections for Santa Cruz County."
        };

        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(instructions)
    }
}
