# Changelog

## [Unreleased]

### Added

#### Environmental & Recreational Tools
- **`get_sun_moon`** — sunrise/sunset, civil/nautical/astronomical twilight, moon phase, and UV index for Santa Cruz or custom coordinates (Open-Meteo + sunrise-sunset.org).
- **`get_space_weather`** — current Kp index, NOAA G/R/S storm scales, and solar wind summary from NOAA SWPC. No auth.
- **`search_outdoors`** — OpenStreetMap features near a location via Overpass API: trails, peaks, viewpoints, water/restrooms, parking. No auth. Cross-references `get_national_park_info` for NPS units.
- **`search_climbing_routes`** — OpenBeta GraphQL climbing route database. Best coverage for Pinnacles National Park (319 routes) and Santa Cruz bouldering (39 routes); Castle Rock not yet indexed upstream.
- **`get_earthquakes`** — recent USGS seismic events near Santa Cruz (default 50 km radius, M1.0+, last 7 days). Magnitude, depth, location, felt reports, tsunami flags.
- **`get_beach_water_quality`** — California BeachWatch bacteria monitoring (Enterococcus, Total/Fecal Coliform, E. coli) with AB411 threshold assessments for 24+ Santa Cruz County beaches. data.ca.gov CKAN, no auth.
- **`get_national_park_info`** — NPS Developer API: hours, fees, activities, directions, weather, contacts. Authoritative source for NPS units. Requires free `NPS_API_KEY` (graceful-disable with registration link).
- **`get_air_quality_forecast`** — Open-Meteo hourly PM2.5/PM10/AQI forecast (1–5 days) plus pollen forecasts (grass, birch, alder, ragweed, olive, mugwort). No auth. Complements the regulatory-monitor `get_air_quality` (AirNow).

#### Field Research Tools
- **`get_tides`** — NOAA CO-OPS high/low tide predictions for any coastal station (default 9413450 Monterey). Heights in feet above MLLW, grouped by date, up to 7 days.
- **`get_buoy_observations`** — NDBC realtime2 text feed renderer: latest wind, significant wave height/period, air + water temperature, pressure, and a ~3h water-temp trend. Default station 46042 (Monterey Bay).
- **`get_wave_buoy`** — CDIP/NDBC `.spec` swell vs wind-wave breakdown (height, period, direction, steepness) across Monterey-area waveriders (default 46114 Pt. Sur, 46236 Monterey Canyon, 46042 Monterey).
- **`get_stream_conditions`** — USGS NWIS Instantaneous Values API for discharge (cfs), gage height (ft), and water temperature. Default gauge 11160500 (San Lorenzo River at Big Trees). Overridable parameter codes.
- **`search_species_observations`** — iNaturalist v1 API (no auth) for recent species observations near Santa Cruz. Filters by free-text query, iconic taxon, days back, custom lat/lon.
- **`search_bird_observations`** — eBird API 2.0 recent observations near Santa Cruz. Requires `EBIRD_API_KEY` (graceful-disable with registration link).
- **`get_air_quality`** — EPA AirNow current AQI by ZIP code (default 95064 UCSC) with one row per pollutant. Requires `AIRNOW_API_KEY` (graceful-disable).
- Shared `degrees_to_compass` helper moved to `src/util.rs` so tides/buoy/wave_buoy/marine all format direction labels consistently.

#### Campus Service Modules
- **Eventbrite event search** — search community events, concerts, meetups, and workshops near Santa Cruz (25-mile radius) via Eventbrite scraper. Returns event details with direct registration links.
- **Recreation facility occupancy** — live headcounts for UCSC gym, pool, fields, climbing wall, and wellness center from `campusrec.ucsc.edu`. Includes facility schedule lookup.
- **Library study room availability** — room-by-room time slot availability for McHenry Library and Science & Engineering Library via LibCal. Includes authenticated room booking through UCSC Shibboleth SSO.
- **Class search** — search the UCSC class schedule via PISA (`pisa.ucsc.edu`). Filter by subject, course number, instructor, title, GE requirement. Returns enrollment counts, meeting times, locations, and instruction mode.
- **Campus directory** — look up UCSC faculty, staff, and graduate student contact info, office locations, and department affiliations.
- **Classroom directory** — search general-assignment classrooms by capacity, building, seating style, AV equipment, and physical features.
- **Shared HTML utility module** (`src/util.rs`) — reusable `clean_html` and `extract_text` helpers for scraper modules.

#### Multi-User Auth (SSE Deployment)
- **`slug-mcp export-token`** CLI subcommand — runs browser login locally (CruzID + Duo MFA), prints a portable base64 auth token to stdout.
- **`authenticate` MCP tool** — accepts a token from `export-token` and stores it as per-session auth state on the SSE server. Each connected client gets independent credentials.
- **`slug-mcp serve`** subcommand — explicit entry point for stdio/SSE mode (bare `slug-mcp` still defaults to stdio for backward compatibility).

### Changed
- **`get_air_quality` and `get_air_quality_forecast` tool descriptions** — clarified when to prefer each (regulatory measured vs modeled forecast/no-key) so the LLM picks correctly.
- **`search_outdoors` and `get_national_park_info` tool descriptions** — added mutual cross-references for NPS units vs everything-else.
- **Split `src/nps/mod.rs`** into `mod.rs` (service + request type) and `scraper.rs` (API client + JSON types + formatters), matching the existing `recreation/`, `library/`, `dining/` pattern.
- **Parallelized dining and library fetches** — dining menu and library availability scraping now use `futures_util::future::join_all` for concurrent requests instead of sequential loops.
- **Improved event tool descriptions** — campus event and Eventbrite tools now cross-reference each other so LLMs call both for complete event coverage.
- **Server instructions** updated to guide LLMs to pair campus + Eventbrite event tools together.
- Scrapers refactored to use shared `util.rs` helpers, reducing HTML-handling duplication across modules.
- Auth-dependent tools (`get_meal_balance`, `book_study_room`) now check both per-session token (SSE) and disk session (stdio), with updated help messages mentioning both `login` and `authenticate`.
- `check_auth` reports token-based auth separately from disk-based auth.
- Replaced legacy CAS module with CDP-based browser automation supporting full Shibboleth + Duo MFA flow.

### Removed
- **SlugLoop campus loop bus module** — removed `src/slugloop/` (api.rs, mod.rs, stops.rs) and associated tool handlers (`get_loop_bus_locations`, `get_loop_bus_eta`). The SlugLoop API was decommissioned (site converted to React SPA with Firebase backend, no public REST endpoints). SC Metro BusTime API remains available for metro bus tracking.

## [0.1.0] — 2026-03-19

### Added
- Initial MCP server with stdio + SSE (Streamable HTTP) transports.
- **Dining menus** — scrapes `nutrition.sa.ucsc.edu` for all 5 dining halls with dietary tags (vegetarian, vegan, halal, gluten-free, allergens).
- **Nutrition facts** — per-item nutrition label lookup by recipe ID.
- **Dining hours** — location hours from `din.dining.ucsc.edu` with schema.org microdata parsing.
- **Meal plan balance** — authenticated scrape of Slug Points / Banana Bucks from GET/CBORD via SAML.
- **Campus events** — search and list upcoming events from `events.ucsc.edu` Tribe Events REST API.
- **UCSC SSO login** — browser-based Shibboleth authentication with encrypted session persistence (AES-256-GCM, 8-hour TTL).
- Moka in-memory cache with per-resource TTLs.
