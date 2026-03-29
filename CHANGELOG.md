# Changelog

## [Unreleased]

### Added

#### Campus Service Modules
- **Recreation facility occupancy** — live headcounts for UCSC gym, pool, fields, climbing wall, and wellness center from `campusrec.ucsc.edu`. Includes facility schedule lookup.
- **Library study room availability** — room-by-room time slot availability for McHenry Library and Science & Engineering Library via LibCal. Includes authenticated room booking through UCSC Shibboleth SSO.
- **Class search** — search the UCSC class schedule via PISA (`pisa.ucsc.edu`). Filter by subject, course number, instructor, title, GE requirement. Returns enrollment counts, meeting times, locations, and instruction mode.
- **Campus directory** — look up UCSC faculty, staff, and graduate student contact info, office locations, and department affiliations.
- **Classroom directory** — search general-assignment classrooms by capacity, building, seating style, AV equipment, and physical features.

#### Multi-User Auth (SSE Deployment)
- **`slug-mcp export-token`** CLI subcommand — runs browser login locally (CruzID + Duo MFA), prints a portable base64 auth token to stdout.
- **`authenticate` MCP tool** — accepts a token from `export-token` and stores it as per-session auth state on the SSE server. Each connected client gets independent credentials.
- **`slug-mcp serve`** subcommand — explicit entry point for stdio/SSE mode (bare `slug-mcp` still defaults to stdio for backward compatibility).

### Changed
- Auth-dependent tools (`get_meal_balance`, `book_study_room`) now check both per-session token (SSE) and disk session (stdio), with updated help messages mentioning both `login` and `authenticate`.
- `check_auth` reports token-based auth separately from disk-based auth.
- Replaced legacy CAS module with CDP-based browser automation supporting full Shibboleth + Duo MFA flow.

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
