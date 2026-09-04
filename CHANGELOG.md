# Changelog

## [Unreleased]

### Fixed — live tool check follow-ups (2026-09-04)
- **`search_classes`**: the instructor filter posted `binds[:instr_name]`, but PISA reads `binds[:instructor]` — every instructor query silently returned no results.
- **eBird species lookups**: the taxonomy index asked for all ~1050 California species codes in one `species=` value (~7.5 KB), which eBird answers 400; codes are now fetched in batches. Fixes `find_nearest_bird_sighting` and species-filtered `search_bird_observations`.
- **`search_events`**: the category is checked against the events API's own slug catalog (cached 6 h; a failed catalog fetch doesn't block the search) instead of forwarding an unknown slug and surfacing the upstream 400. The parameter description no longer suggests `"lecture"`, which is not a valid slug.
- **Invalid-argument failures** (`search_events`, `search_outdoors`) are returned as tool-level errors (`isError: true`) instead of `-32603`, which most MCP clients render opaquely — the list of valid values now actually reaches the caller.
- **`get_nutrition_info`**: an unknown recipe id returns a not-found message instead of a well-formed label with an empty title and every nutrient `N/A`.
- **`get_tides`**: non-default stations are named from NOAA's station metadata, degrading to the bare id — `San Francisco, CA (9414290)` rather than `station 9414290 (9414290)`.
- **`get_sc_wharf_state`**: sample age renders as `18 days ago` instead of `latency 25998m`.

### Changed — dependencies (2026-09-03)
- **rmcp 2.1 → 3.2**: `call_tool` returns `CallToolResponse`; `ListToolsResult` gains cache-hint fields; `StreamableHttpServerConfig::stateful_mode` is now `legacy_session_mode`.
- **aes-gcm 0.10 → 0.11 / sha2 0.10 → 0.11** (RustCrypto hybrid-array line): session nonce via `aead::Generate`, digest returned directly as the AES key.
- **base64 0.22 → 0.23, quick-xml 0.41 → 0.42**, plus all semver-compatible updates (tokio 1.53, clap 4.6.6, regex 1.13, moka 0.12.16, schemars 1.2.2, …).

### Fixed — tool audit (2026-09-03)
- **Transit**: stop search folds `&`/"and", accepts a majority of query tokens, and resolves hub aliases ("Metro Center" → Pacific & Center, "Main Entrance" → Bay & High); BusTime is now the primary prediction source (its stop ids match the catalog; GTFS-RT trip_updates uses a different id namespace) with a 30 s cache and occupancy joined from the GTFS-RT vehicles feed; system alerts are filtered by `active_period` with an Upcoming section; per-route alert bulletins are cached as data (errors are never cached); a partially failed stops fan-out is no longer cached for a day; unassigned buses render as "Not in service"; `get_route_delays` explains that SC Metro publishes no delay values.
- **Dining**: the upstream `Mo 1:00-13:00` placeholder is dropped at parse time; dated ranges that carry opens/closes are hours, not closures; `M/D/YYYY` menu dates normalize to ISO (closure detection and cache keys agree); multi-day closures are always kept in "Upcoming Special Hours".
- **Events**: all-day Tribe events render as dates (a DST-shifted `00:59:59` end no longer spills to the next day).
- **Recreation**: group-exercise term label derives from the scraped page's URL (description no longer hardcodes a term); facility schedules render the open windows implied by "Facility Closed" blocks; `BYSETPOS=-1` (last weekday of month) expands correctly; schedule cache key includes the date.
- **Ocean**: SC Wharf falls back to the newest available sample (`orderByMax`) with a STALE banner when the ERDDAP dataset lags; the research-snapshot checksum now hashes a source manifest (a missing source can't collide with a complete snapshot) and reports `sources_present`; ERDDAP no-data answers are a typed error; oversized responses are rejected by `Content-Length` before buffering.
- **Buoys / climbing / air / sky**: wave-buoy defaults drop the dead 46114 feed for 46239 (Point Sur); climbing area search ranks exact-name and California matches first; AirNow observations are labelled PDT/PST correctly; tide, twilight, and Open-Meteo caches roll over at midnight; twilight is requested for the Pacific date instead of the UTC date.
- **Robustness**: NWS alert outages are reported as unavailable rather than "0 alerts"; USGS earthquake windows are computed in UTC; empty classroom/summer-calendar scrapes are never cached; PISA/LibCal/directory fetches check HTTP status; library dates are validated; half-specified bird-search coordinates are rejected; SWPC total failures are not cached; Overpass fetches a generous set so nearest-first is real; class-search page offsets are bounded.

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
- **Study-room booking** — removed the `book_study_room` tool and the entire LibCal checkout protocol (`book_room`, slot-claim/hold, patron-form parsing). LibCal finalizes patron checkout with a JavaScript token→session exchange that a non-JS HTTP client can't complete, so booking couldn't reliably succeed over the wire. Study-room **availability** (`get_study_room_availability`) is unaffected.
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
