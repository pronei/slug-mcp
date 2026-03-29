<p align="center">
  <img src="assets/logo.png" alt="slug-mcp" width="400" />
</p>

<h1 align="center">slug-mcp</h1>

<p align="center">
  An <a href="https://modelcontextprotocol.io/">MCP</a> server that gives AI assistants access to UC Santa Cruz campus services.<br/>
  Ask about dining menus, gym crowding, class schedules, study rooms, and more — all from one interface.<br/><br/>
  Built with Rust and <a href="https://github.com/anthropics/rmcp">rmcp</a> for the UCSC community.
</p>

---

## Features

| Service | What you can do |
|---------|----------------|
| **Dining** | Browse menus for all 5 dining halls with dietary tags and allergen info, look up full nutrition facts for any item, check operating hours for all locations, view your Slug Points / Banana Bucks balance |
| **Events** | Search campus events by keyword or category, list upcoming events |
| **Recreation** | See live headcounts for gym, pool, fields, climbing wall, and wellness center; view facility schedules (open gym, lap swim, intramurals) |
| **Library** | Check study room availability at McHenry and S&E Library by time slot, book a room (authenticated) |
| **Academics** | Search the class schedule by subject, instructor, GE, course number — with enrollment counts, meeting times, and instruction mode; look up faculty/staff contact info and departments |
| **Classrooms** | Find general-assignment classrooms by capacity, building, AV equipment, seating style, and accessibility features |

## Quick Start (Local)

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

Or to connect to the hosted server:

```bash
claude mcp add slug-mcp --transport sse https://<TBD>/mcp
```

### ChatGPT Desktop

Open **ChatGPT** → **Settings** → **Beta Features** → enable **MCP Servers**, then:

**Settings** → **MCP Servers** → **Add Server** → **Command-line (stdio)**

- **Name:** `slug-mcp`
- **Command:** `/path/to/slug-mcp`

### Gemini

In **Google AI Studio** or the **Gemini API**, add the server as a tool source using the Streamable HTTP endpoint:

```
https://<TBD>/mcp
```

Gemini supports MCP via its [tool use](https://ai.google.dev/gemini-api/docs/function-calling) framework — point it at the SSE server URL.

### Any MCP-compatible client

**Local (stdio):** run `/path/to/slug-mcp` as the command — no arguments needed.

**Remote (SSE):** connect to `https://<TBD>/mcp` using Streamable HTTP transport.

## Connecting to the Hosted Server

A shared instance is available for UCSC students:

> **Server URL:** `https://<TBD>/mcp`
>
> *Connection details will be provided by the course staff.*

Most tools work immediately with no login. For authenticated tools (meal balance, room booking), generate a token on your local machine:

```bash
# 1. Generate a token locally (opens browser for CruzID + Duo MFA)
slug-mcp export-token
# Prints a base64 token valid for 8 hours

# 2. In your AI assistant, call the authenticate tool with the token
```

Each connected client gets independent auth state — your credentials are not shared with other users.

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

Each module follows the pattern: `scraper.rs` (HTTP + HTML parsing) → `mod.rs` (service layer with caching) → `server.rs` (MCP tool handler).

## License

[MIT](LICENSE)
