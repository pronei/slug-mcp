# slug-mcp

An [MCP](https://modelcontextprotocol.io/) server that gives AI assistants access to UC Santa Cruz campus services. Ask your AI about dining menus, gym crowding, class schedules, study room availability, and more — all from one interface.

Built with Rust ([rmcp](https://github.com/anthropics/rmcp)) for the UCSC community.

## Features

| Tool | What it does | Source |
|------|-------------|--------|
| `get_dining_menu` | Menus for all 5 dining halls with dietary tags and allergens | nutrition.sa.ucsc.edu |
| `get_nutrition_info` | Full nutrition facts for any menu item | nutrition.sa.ucsc.edu |
| `get_dining_hours` | Operating hours for all dining locations | din.dining.ucsc.edu |
| `get_meal_balance` | Slug Points / Banana Bucks balance | get.cbord.com (auth) |
| `search_events` / `get_upcoming_events` | Campus events by keyword or category | events.ucsc.edu |
| `get_facility_occupancy` | Live headcounts for gym, pool, fields, climbing wall | campusrec.ucsc.edu |
| `get_facility_schedule` | Rec facility calendars (open gym, swim, intramurals) | campusrec.ucsc.edu |
| `get_study_room_availability` | Study room time slots at McHenry and S&E Library | calendar.library.ucsc.edu |
| `book_study_room` | Book a study room (authenticated) | calendar.library.ucsc.edu |
| `search_classes` | Class schedule search — enrollment, instructor, times, GE | pisa.ucsc.edu |
| `search_directory` | Faculty/staff contact info and department lookup | campusdirectory.ucsc.edu |
| `search_classrooms` | Find rooms by capacity, AV equipment, building, features | classrooms.ucsc.edu |

## Quick Start (Local)

**Prerequisites:** Rust 1.75+, Chrome/Chromium (for authenticated tools only)

```bash
git clone git@github.com:pronei/slug-mcp.git
cd slug-mcp
cargo build --release
```

### Use with Claude Desktop (stdio)

Add to your Claude Desktop config (`~/Library/Application Support/Claude/claude_desktop_config.json` on macOS):

```json
{
  "mcpServers": {
    "slug-mcp": {
      "command": "/path/to/slug-mcp"
    }
  }
}
```

Then ask Claude things like:
- *"What's for dinner at Crown tonight?"*
- *"How crowded is the gym right now?"*
- *"Find me a study room at McHenry after 2pm"*
- *"What CSE classes are open for Spring?"*

### Authentication (optional)

Some tools (meal balance, room booking) require UCSC login:

```bash
# Local use — opens Chrome for CruzID + Duo MFA
# (happens automatically when you use an auth-required tool)
```

## Connecting to the Hosted Server

A shared instance is available for UCSC students:

> **Server URL:** `https://<TBD>/mcp`
>
> *Connection details will be provided by the course staff.*

### Setup

Point your MCP client at the server URL. Most tools work immediately — no login needed for dining, events, gym, classes, classrooms, or directory.

For authenticated tools (meal balance, room booking), generate a token on your local machine and pass it to the server:

```bash
# 1. Generate a token locally (opens browser for Duo MFA)
cargo run -- export-token
# Prints a base64 token valid for 8 hours

# 2. In your MCP client, call the authenticate tool with the token
# authenticate(token: "eyJ...")
```

Each connected client gets independent auth state — your credentials are not shared with other users.

## Running Your Own Server (SSE)

```bash
# Start the SSE server
./slug-mcp serve --sse --port 3000

# With a reverse proxy (recommended for TLS)
# Example Caddyfile:
#   slug-mcp.example.com {
#       reverse_proxy localhost:3000
#   }
```

The server handles multiple concurrent clients. Each SSE session gets its own isolated state via the rmcp session factory. Shared resources (cache, HTTP client) are `Arc`-wrapped and thread-safe.

## Architecture

```
src/
├── main.rs              # CLI (serve / export-token) + service wiring
├── server.rs            # MCP tool handlers (rmcp macros)
├── cache.rs             # Moka TTL cache
├── auth/
│   ├── mod.rs           # AuthManager, SAML-aware HTTP client
│   ├── browser.rs       # Chrome CDP for Shibboleth + Duo login
│   ├── session.rs       # AES-256-GCM encrypted session storage
│   └── token.rs         # Portable base64 token encode/decode
├── dining/              # Menu scraping, nutrition, hours, balance
├── events/              # Tribe Events REST API client
├── recreation/          # Facility occupancy + schedules
├── library/             # LibCal study room availability + booking
├── academics/           # PISA class search + campus directory
└── classrooms/          # Classroom directory scraping
```

Each module follows the pattern: `scraper.rs` (HTTP + HTML parsing) + `mod.rs` (service layer with caching).

## License

MIT
