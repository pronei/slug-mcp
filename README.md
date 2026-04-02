<p align="center">
  <img src="assets/logo.png" alt="slug-mcp" width="400" />
</p>

<h1 align="center">slug-mcp</h1>

<p align="center">
  An <a href="https://modelcontextprotocol.io/">MCP</a> server that gives AI assistants access to UC Santa Cruz campus services.<br/>
  Dining menus, gym crowding, class schedules, study rooms, bus times, and more — all from one conversation.<br/><br/>
  Built with Rust and <a href="https://github.com/anthropics/rmcp">rmcp</a> for the UCSC community.
</p>

---

## Why?

UCSC students juggle 6+ different websites daily — PISA for classes, nutrition.sa.ucsc.edu for menus, LibCal for study rooms, campusrec for gym status, Metro for buses, the events calendar. None of them talk to each other.

slug-mcp puts all of these behind a single MCP interface. Your AI assistant can combine data across services to answer questions that no single UCSC website can:

> **"I have CSE 115A at 4pm in Baskin — what's for dinner nearby after class, and when's the next bus home from Science Hill?"**
>
> The assistant checks your class schedule for the room and time, pulls tonight's dining hall menus, cross-references dining hours with your class end time, and gets real-time bus ETAs from the nearest stop. One question, four services, ten seconds.

## Try These

These prompts show what's possible when campus services are connected:

| Prompt | What happens behind the scenes |
|--------|-------------------------------|
| *"I need to study for 3 hours today — find me a room near food"* | Checks study room availability at both libraries, cross-references with dining hall hours and proximity, suggests a room + meal window |
| *"Is it worth going to the gym right now or should I wait?"* | Pulls live headcount from the rec center (updates every 2 min), checks the facility schedule for upcoming open-gym blocks |
| *"What are my options for a high-protein dinner tonight?"* | Gets tonight's menus across all 5 dining halls, looks up nutrition facts for the main proteins, ranks by protein-per-serving |
| *"Who teaches CSE 130 and when are their office hours?"* | Searches the class schedule for the instructor, then searches the campus directory for their office location and contact info |
| *"I have $12 in Slug Points — where should I eat?"* | Checks your meal balance, pulls current menus and dining hours, suggests halls that are open now |
| *"Find me a GE class that fits between my Tuesday 10am and 2pm classes"* | Searches for open GE courses in the current term filtered to TuTh, checks enrollment status, returns options that fit the gap |
| *"Any cool events this week I can get to by bus?"* | Searches upcoming campus events, checks bus routes and real-time ETAs from your stop |

## Features

| Service | Tools |
|---------|-------|
| **Dining** | Menus for all 5 dining halls with dietary/allergen tags, nutrition facts per item, operating hours, Slug Points / Banana Bucks balance |
| **Events** | Search campus events by keyword or category, list upcoming events chronologically |
| **Recreation** | Live headcounts for gym, pool, fields, climbing wall, wellness center; facility schedules |
| **Library** | Study room availability at McHenry and S&E Library by time slot, room booking |
| **Academics** | Class search by subject, instructor, GE, course number, career level — enrollment counts, meeting times, instruction mode; faculty/staff directory |
| **Classrooms** | Find rooms by capacity, building, AV equipment, seating style, accessibility |
| **Transit** | Real-time bus arrival predictions by stop and route via Santa Cruz Metro |

## Quick Start

**Prerequisites:** Rust 1.75+, Chrome/Chromium (for authenticated tools only)

```bash
git clone git@github.com:pronei/slug-mcp.git
cd slug-mcp
cargo build --release
```

## Client Setup

### Claude Desktop

Add to `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) or `%APPDATA%\Claude\claude_desktop_config.json` (Windows):

```json
{
  "mcpServers": {
    "slug-mcp": {
      "command": "/path/to/slug-mcp"
    }
  }
}
```

### Claude Code

```bash
claude mcp add slug-mcp /path/to/slug-mcp
```

### ChatGPT Desktop

Open **ChatGPT** > **Settings** > **Beta Features** > enable **MCP Servers**, then:

**Settings** > **MCP Servers** > **Add Server** > **Command-line (stdio)**

- **Name:** `slug-mcp`
- **Command:** `/path/to/slug-mcp`

### Any MCP-compatible client

Run `/path/to/slug-mcp` as a stdio subprocess — no arguments needed.

For remote connections, start the SSE server and point your client at the endpoint (see [Running Your Own Server](#running-your-own-server)).

## Authentication

Most tools work without login. Two tools require UCSC authentication:
- **Meal balance** — Slug Points / Banana Bucks
- **Room booking** — reserve study rooms

When you ask for something that needs auth, the assistant will call the `login` tool, which opens a Chrome window for CruzID + Duo MFA. The session is captured automatically and lasts 8 hours.

For remote/headless servers where a browser can't open, generate a portable token locally:

```bash
slug-mcp export-token    # opens browser, prints base64 token
```

Then pass the token to the `authenticate` tool on the remote server.

## Running Your Own Server

```bash
# Start the SSE server
./slug-mcp serve --sse --port 3000
```

For production, put a reverse proxy in front for TLS:

```
# Caddyfile
slug-mcp.example.com {
    reverse_proxy localhost:3000
}
```

The server handles multiple concurrent clients. Each SSE session gets its own isolated auth state via the rmcp session factory. Shared resources (cache, HTTP client) are `Arc`-wrapped and thread-safe.

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
├── classrooms/          # Classroom directory + campus locations
└── transit/             # Santa Cruz Metro real-time predictions
```

Each module follows the pattern: `scraper.rs` (HTTP + HTML parsing) > `mod.rs` (service layer with caching) > `server.rs` (MCP tool handler).

## License

[MIT](LICENSE)
