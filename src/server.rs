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
#[cfg(feature = "auth")]
use crate::library::BookStudyRoomRequest;
use crate::library::{LibraryService, StudyRoomAvailabilityRequest};
use crate::recreation::{FacilityOccupancyRequest, FacilityScheduleRequest, RecreationService};
use crate::transit::TransitService;

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

            #[tool(description = "Get real-time bus arrival predictions for a Santa Cruz Metro stop. Shows ETAs, delays, passenger load, and trip status (canceled/express). Search by stop name; optionally filter by route. All UCSC students ride free with student ID.")]
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

            #[tool(description = "Get active service alerts and bulletins for Santa Cruz Metro bus routes. Shows detours, disruptions, and schedule changes. Specify a route number or stop ID.")]
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

        let client =
            crate::auth::build_authenticated_client(&session.cookies).map_err(internal_err)?;

        let result = self
            .library
            .book(&client, req.space_id, &req.date, &req.start_time, &req.end_time)
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
            "UCSC campus services MCP server. Provides dining menus, nutrition info, \
             meal plan balances, campus events and Eventbrite community events \
             (use both event tools together for complete coverage), recreation \
             facility occupancy, library study room availability and booking, \
             class schedule search, campus directory lookup, classroom search, \
             real-time bus arrival predictions, transit service alerts, \
             degree requirements lookup, and degree progress tracking for \
             UC Santa Cruz students."
        } else {
            "UCSC campus services MCP server (public mode). Provides dining menus, \
             nutrition info, campus events and Eventbrite community events \
             (use both event tools together for complete coverage), recreation \
             facility occupancy, library study room availability, class schedule \
             search, campus directory lookup, classroom search, real-time bus \
             arrival predictions, transit service alerts, degree requirements \
             lookup, and degree progress tracking for UC Santa Cruz students."
        };

        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(instructions)
    }
}
