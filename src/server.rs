use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::auth::AuthManager;
use crate::cache::CacheStore;
use crate::config::Config;
use crate::dining::DiningService;
use crate::events::EventsService;

#[derive(Clone)]
pub struct SlugMcpServer {
    #[allow(dead_code)]
    config: Arc<Config>,
    #[allow(dead_code)]
    cache: Arc<CacheStore>,
    auth: Arc<AuthManager>,
    dining: Arc<DiningService>,
    events: Arc<EventsService>,
    tool_router: ToolRouter<Self>,
}

impl SlugMcpServer {
    pub fn new(
        config: Arc<Config>,
        cache: Arc<CacheStore>,
        auth: Arc<AuthManager>,
        dining: Arc<DiningService>,
        events: Arc<EventsService>,
    ) -> Self {
        Self {
            config,
            cache,
            auth,
            dining,
            events,
            tool_router: Self::tool_router(),
        }
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

#[tool_router]
impl SlugMcpServer {
    #[tool(description = "Login to UCSC via CAS SSO. Opens your browser for authentication with CruzID and Duo MFA.")]
    async fn login(&self) -> Result<CallToolResult, ErrorData> {
        let username = self.auth.login().await.map_err(|e| {
            ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None)
        })?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Successfully logged in as **{}**. Session valid for 8 hours.",
            username
        ))]))
    }

    #[tool(description = "Check if you are currently authenticated with UCSC")]
    async fn check_auth(&self) -> Result<CallToolResult, ErrorData> {
        let status = self.auth.check_auth().map_err(|e| {
            ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None)
        })?;

        Ok(CallToolResult::success(vec![Content::text(status.format())]))
    }

    #[tool(description = "Get your UCSC meal plan balance (Slug Points, Banana Bucks). Requires authentication - use 'login' first.")]
    async fn get_meal_balance(&self) -> Result<CallToolResult, ErrorData> {
        let status = self.auth.check_auth().map_err(|e| {
            ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None)
        })?;

        if !status.authenticated {
            return Ok(CallToolResult::success(vec![Content::text(
                "You need to log in first. Use the `login` tool to authenticate with your UCSC credentials.",
            )]));
        }

        // Use a basic client for now; in the future we'd use authenticated cookies
        let client = reqwest::Client::new();
        let balance = self.dining.get_balance(&client).await.map_err(|e| {
            ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None)
        })?;

        Ok(CallToolResult::success(vec![Content::text(balance.format())]))
    }

    #[tool(description = "Get the menu for a UCSC dining hall")]
    async fn get_dining_menu(
        &self,
        Parameters(req): Parameters<DiningMenuRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let result = self
            .dining
            .get_menu(req.hall.as_deref(), req.meal.as_deref())
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
}

#[tool_handler]
impl ServerHandler for SlugMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "UCSC campus services MCP server. Provides dining menus, events, \
                 meal plan balances, and more for UC Santa Cruz students.",
            )
    }
}
