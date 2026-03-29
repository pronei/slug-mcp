use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::academics::AcademicsService;
use crate::auth::session::SessionData;
use crate::auth::AuthManager;
use crate::cache::CacheStore;
use crate::classrooms::ClassroomService;
use crate::config::Config;
use crate::dining::DiningService;
use crate::events::EventsService;
use crate::library::LibraryService;
use crate::recreation::RecreationService;

#[derive(Clone)]
pub struct SlugMcpServer {
    #[allow(dead_code)]
    config: Arc<Config>,
    #[allow(dead_code)]
    cache: Arc<CacheStore>,
    auth: Arc<AuthManager>,
    /// Per-session auth state for SSE mode (set via `authenticate` tool).
    session_auth: Arc<RwLock<Option<SessionData>>>,
    dining: Arc<DiningService>,
    events: Arc<EventsService>,
    recreation: Arc<RecreationService>,
    library: Arc<LibraryService>,
    academics: Arc<AcademicsService>,
    classrooms: Arc<ClassroomService>,
    tool_router: ToolRouter<Self>,
}

impl SlugMcpServer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Arc<Config>,
        cache: Arc<CacheStore>,
        auth: Arc<AuthManager>,
        dining: Arc<DiningService>,
        events: Arc<EventsService>,
        recreation: Arc<RecreationService>,
        library: Arc<LibraryService>,
        academics: Arc<AcademicsService>,
        classrooms: Arc<ClassroomService>,
    ) -> Self {
        Self {
            config,
            cache,
            auth,
            session_auth: Arc::new(RwLock::new(None)),
            dining,
            events,
            recreation,
            library,
            academics,
            classrooms,
            tool_router: Self::tool_router(),
        }
    }

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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchEventsRequest {
    /// Search query string
    pub query: Option<String>,
    /// Event category/type filter (e.g., "workshop", "lecture")
    pub category: Option<String>,
    /// Max results (default 10, max 50)
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpcomingEventsRequest {
    /// Number of events to return (default 10, max 50)
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiningMenuRequest {
    /// Dining hall name (e.g., "Crown", "Porter", "College Nine"). If omitted, returns all halls.
    pub hall: Option<String>,
    /// Meal period: "breakfast", "lunch", "dinner", or "late night". If omitted, returns all meals.
    pub meal: Option<String>,
    /// Date in YYYY-MM-DD format (e.g., "2026-03-19"). If omitted, returns today's menu.
    pub date: Option<String>,
    /// Set to true to include all categories (condiments, beverages, cereal, etc.). Default: only main food items.
    pub include_all_categories: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NutritionRequest {
    /// Recipe ID from the menu (e.g., "061002*3"). Get this from get_dining_menu output.
    pub recipe_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiningHoursRequest {
    /// Location name to filter by (e.g., "Crown", "Porter"). If omitted, returns all locations.
    pub location: Option<String>,
}

// ─── Recreation ───

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

// ─── Library ───

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StudyRoomAvailabilityRequest {
    /// Library name: "McHenry" or "Science & Engineering". If omitted, returns both.
    pub library: Option<String>,
    /// Date in YYYY-MM-DD format. If omitted, returns today's availability.
    pub date: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BookStudyRoomRequest {
    /// Space/room ID from get_study_room_availability output.
    pub space_id: u32,
    /// Date in YYYY-MM-DD format.
    pub date: String,
    /// Start time (e.g., "09:00", "2:00 PM").
    pub start_time: String,
    /// End time (e.g., "10:00", "3:00 PM").
    pub end_time: String,
}

// ─── Academics ───

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchClassesRequest {
    /// Term code (e.g., "2262" for Spring 2026). If omitted, uses current term.
    pub term: Option<String>,
    /// Subject/department code (e.g., "CSE", "MATH", "PHYS").
    pub subject: Option<String>,
    /// Course catalog number (e.g., "115A", "19A").
    pub course_number: Option<String>,
    /// Instructor last name.
    pub instructor: Option<String>,
    /// Course title keyword.
    pub title: Option<String>,
    /// General Education requirement code.
    pub ge: Option<String>,
    /// If true, only show open classes. Default: show all.
    pub open_only: Option<bool>,
    /// Page number for pagination (25 results per page). Default: 0.
    pub page: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchDirectoryRequest {
    /// Search query (name, department, etc.)
    pub query: String,
    /// Search type: "people" (default) or "departments".
    pub search_type: Option<String>,
}

// ─── Authentication ───

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AuthenticateRequest {
    /// Portable auth token from `slug-mcp export-token`. Base64-encoded session data.
    pub token: String,
}

// ─── Classrooms ───

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchClassroomsRequest {
    /// Classroom or building name to search for (e.g., "Baskin", "Classroom Unit").
    pub name: Option<String>,
    /// Minimum seating capacity.
    pub min_capacity: Option<u32>,
    /// Maximum seating capacity.
    pub max_capacity: Option<u32>,
    /// Campus area or building filter (e.g., "crown-college", "science-hill").
    pub building: Option<String>,
    /// Required technology (e.g., "lecture-capture", "wireless-projection").
    pub technology: Option<String>,
    /// Required physical feature (e.g., "ada-accessible", "chalkboards").
    pub feature: Option<String>,
}

#[tool_router]
impl SlugMcpServer {
    #[tool(description = "Login to UCSC SSO. Opens your browser for Shibboleth authentication with CruzID and Duo MFA. After completing login in your browser, the tool will detect your session automatically. For remote servers, use `authenticate` with a token from `slug-mcp export-token` instead.")]
    async fn login(&self) -> Result<CallToolResult, ErrorData> {
        let username = self.auth.login().await.map_err(|e| {
            ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None)
        })?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Successfully logged in as **{}**. Session valid for 8 hours.",
            username
        ))]))
    }

    #[tool(description = "Authenticate with a portable UCSC session token. Run `slug-mcp export-token` on your local machine to get a token, then pass it here. Use this for remote/SSE server connections where browser login is not available.")]
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
        let status = self.auth.check_auth().map_err(|e| {
            ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None)
        })?;

        Ok(CallToolResult::success(vec![Content::text(status.format())]))
    }

    #[tool(description = "Get your UCSC meal plan balance (Slug Points, Banana Bucks). Requires authentication - use 'login' or 'authenticate' first.")]
    async fn get_meal_balance(&self) -> Result<CallToolResult, ErrorData> {
        let session = match self.get_active_session().await {
            Some(s) => s,
            None => {
                return Ok(CallToolResult::success(vec![Content::text(
                    "You need to log in first. Use the `login` tool (local) or `authenticate` tool (remote) with a token from `slug-mcp export-token`.",
                )]));
            }
        };

        // Build an authenticated client with IdP cookies for SAML auto-approval
        let client =
            crate::auth::build_authenticated_client(&session.cookies).map_err(|e| {
                ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None)
            })?;

        let result = self.dining.get_balance(&client).await.map_err(|e| {
            ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None)
        })?;

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
            .map_err(|e| {
                ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None)
            })?;

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
            .map_err(|e| {
                ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None)
            })?;

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
            .map_err(|e| {
                ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None)
            })?;

        Ok(CallToolResult::success(vec![Content::text(hours)]))
    }

    #[tool(description = "Search for UCSC campus events by keyword or category")]
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
            .map_err(|e| {
                ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None)
            })?;

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

    #[tool(description = "Get upcoming UCSC campus events")]
    async fn get_upcoming_events(
        &self,
        Parameters(req): Parameters<UpcomingEventsRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = req.limit.unwrap_or(10);
        let events = self
            .events
            .get_upcoming_events(limit)
            .await
            .map_err(|e| {
                ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None)
            })?;

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
            .map_err(|e| ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None))?;

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
            .map_err(|e| ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None))?;

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
            .map_err(|e| ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(description = "Book a study room at a UCSC library. Requires authentication - use 'login' or 'authenticate' first. Use space_id from get_study_room_availability.")]
    async fn book_study_room(
        &self,
        Parameters(req): Parameters<BookStudyRoomRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let session = match self.get_active_session().await {
            Some(s) => s,
            None => {
                return Ok(CallToolResult::success(vec![Content::text(
                    "You need to log in first. Use the `login` tool (local) or `authenticate` tool (remote) with a token from `slug-mcp export-token`.",
                )]));
            }
        };

        let client =
            crate::auth::build_authenticated_client(&session.cookies).map_err(|e| {
                ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None)
            })?;

        let result = self
            .library
            .book(&client, req.space_id, &req.date, &req.start_time, &req.end_time)
            .await
            .map_err(|e| ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None))?;

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
                req.open_only.unwrap_or(false),
                req.page,
            )
            .await
            .map_err(|e| ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None))?;

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
            .map_err(|e| ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None))?;

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
            .map_err(|e| ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(result)]))
    }
}

#[tool_handler]
impl ServerHandler for SlugMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "UCSC campus services MCP server. Provides dining menus, nutrition info, \
                 meal plan balances, campus events, recreation facility occupancy, \
                 library study room availability and booking, class schedule search, \
                 campus directory lookup, and classroom search for UC Santa Cruz students.",
            )
    }
}
