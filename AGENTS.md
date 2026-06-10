# AGENTS.md

Orientation for AI coding agents working on slug-mcp. For product context (what the
server does, install instructions, public tools), read [README.md](README.md). This
file covers the parts of the codebase that aren't obvious from reading source.

## Build, run, test

```bash
cargo build                                  # debug build, with auth feature
cargo build --release                        # release build
cargo build --no-default-features            # skip auth (no chromiumoxide, no Chrome dep)
cargo build --profile release-optimized --no-default-features  # smallest/fastest binary

cargo run -- serve                           # stdio MCP server (default)
cargo run -- serve --sse --port 3000        # HTTP/SSE server at /mcp
cargo run -- export-token                    # browser login, prints portable token to stdout

cargo test                                   # unit tests (most live in `mod tests` blocks)
cargo clippy --all-targets -- -D warnings    # lints; CI is strict
cargo fmt
```

Logging: `RUST_LOG=slug_mcp=debug cargo run -- serve` (writes to stderr; stdout is
reserved for MCP framing on stdio transport, and for the token on `export-token`).

## Module layout

```
src/
├── main.rs        # CLI parsing, service wiring, transport setup
├── server.rs      # All MCP tool handlers (one big file by design — single source
│                  # of truth for the tool surface). ~1100 lines.
├── cache.rs       # Type-erased TTL cache (moka). No JSON round-trip.
├── config.rs      # Env var → Config struct. Optional API keys.
├── progress.rs    # Progress notification helper for long-running tools.
├── util.rs        # `now_pacific()`, `strip_html_tags`, `truncate`,
│                  # `degrees_to_compass`, `selectors!` macro.
├── util/fuzzy.rs  # FuzzyMatcher — case/whitespace-insensitive name matching.
├── auth/          # CDP-driven CruzID+Duo login; AES-256-GCM session storage;
│                  # portable token codec. All gated on `feature = "auth"`.
└── <service>/     # 26 service modules — see "The pattern" below.
```

Service modules: `academics`, `air_forecast`, `air_quality`, `astronomy`,
`beach_water`, `biodiversity`, `buoy`, `classrooms`, `climbing`, `degrees`,
`dining`, `earthquakes`, `events`, `fire`, `library`, `marine`, `nps`, `outdoors`,
`recreation`, `space_weather`, `tides`, `traffic`, `transit`, `usgs_water`,
`wave_buoy`, `weather`.

## The pattern

Every service follows the same shape. Read [src/dining/mod.rs](src/dining/mod.rs)
for the canonical reference.

```rust
// 1. Request struct lives in the service's mod.rs (or in server.rs for tools
//    that don't fit a clean module boundary, e.g. transit's per-tool requests).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FooRequest {
    /// Doc comment becomes the JSON Schema description shown to the model.
    pub bar: Option<String>,
}

// 2. Service struct: holds the shared HTTP client and cache. Optional API keys
//    are passed as constructor args, not pulled from env at call time.
pub struct FooService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
}

impl FooService {
    pub fn new(http: reqwest::Client, cache: Arc<CacheStore>) -> Self { ... }

    pub async fn get_foo(&self, bar: Option<&str>) -> Result<String> {
        let key = format!("foo:{}", bar.unwrap_or(""));
        self.cache.get_or_fetch(&key, 300, || async {
            // upstream HTTP, parse, return concrete type (NOT a JSON string —
            // cache stores typed values via Arc<dyn Any>)
        }).await
    }
}
```

Then wire it in three places:

1. `main.rs` → add `Arc<FooService>` field to `ServiceContext` and construct it.
2. `server.rs` → add the matching field to `SlugMcpServer` and copy it across in
   `SlugMcpServer::new(ctx)`.
3. `server.rs` → add the `#[tool]` handler **inside the `define_tools! { ... }`
   macro body**, not in a sibling impl block. The macro exists so auth-only
   tools can be cfg-gated through `$extra` while keeping `#[tool_router]` happy.

## Conventions

- **rmcp v1.2 API.** Tool handler params use `Parameters<T>` wrapping the request
  struct — *not* `#[tool(aggr)]` (older API). `ServerInfo::new(...)` is a builder;
  the underlying struct is `#[non_exhaustive]`, so don't construct it directly.

- **Optional API keys degrade, never error.** If a key is `None`, return a friendly
  string explaining the env var to set. Don't return `Err`. The exception is
  `SLUG_MCP_BUSTIME_KEY` — the SC Metro stops catalog needs it on startup, but
  GTFS-RT positions/alerts still work without it.

- **Time is Pacific.** Use `util::now_pacific()` for any user-facing "today",
  weekday math, "is it open right now", or business-hours comparison. Never
  `chrono::Local` (host TZ varies — hosted server is in UTC).

- **Auth is feature-gated.** Anything touching `chromiumoxide`, session decryption,
  or the meal-balance / room-booking tools must be behind `#[cfg(feature = "auth")]`
  on every item the compiler sees. The `define_tools!` macro takes an `$extra`
  branch for the auth-only tool block — see existing usage in `server.rs`.

- **Cache stores concrete types.** `CacheStore::set` / `get_or_fetch` are generic
  over `T: Clone + Send + Sync + 'static`. Don't pre-serialize to JSON to "save
  space" — type-erasure via `Arc<dyn Any>` is the whole point.

- **Per-entry TTLs.** TTL is set at insert time, not globally. Pick TTL by upstream
  freshness: real-time (bus predictions) ≈ 30s; sub-hourly (AQI, weather) ≈ 5–15
  min; daily (dining menu) several hours; static catalogs (stops, classrooms) up
  to a day.

- **Comments only for non-obvious WHY.** The codebase uses sparse comments — most
  of them flag a hidden constraint (e.g. "NWS rejects blank User-Agent") or a
  surprising choice. Don't add comments that restate what code does. Don't write
  multi-line doc blocks on internal items.

- **CSS selectors via `util::selectors!`.** When scraping HTML, declare selectors
  through the macro instead of `lazy_static` / inline `Selector::parse(...)`.
  The macro keeps the name and CSS string colocated in one statement.

- **HTML scraping isolation.** Each service that scrapes lives in its own
  `scraper.rs` (HTTP + parse) and exposes typed results to `mod.rs` (caching +
  service surface). `server.rs` never touches HTML.

## Gotchas

- **NOAA NWS rejects blank User-Agent.** The shared `reqwest::Client` in
  `main.rs::run_serve` sets one — always clone that client; never construct a
  fresh `reqwest::Client::new()` mid-call.

- **`nutrition.sa.ucsc.edu` (FoodPro) is intermittently down.** Treat upstream
  HTTP errors as expected; surface them as text rather than 500-ing the tool.

- **`events.ucsc.edu` is WordPress + Tribe Events plugin** at
  `/wp-json/tribe/events/v1/events`. It is *not* Localist, despite earlier campus
  pages suggesting that. Eventbrite is a separate API for community events; both
  tools should typically be called together for full coverage.

- **CruzID auth is browser-driven.** `auth/browser.rs` drives Chrome via CDP
  (chromiumoxide). Headless servers can't run the `login` tool — use
  `slug-mcp export-token` locally and the `authenticate` tool remotely.

- **Stdio mode owns stdout.** Anything written to stdout in stdio transport
  corrupts MCP framing. All logs/diagnostics go to stderr. Tracing is configured
  to write to `std::io::stderr` for this reason.

- **Per-session auth state for SSE.** SSE clients call `authenticate` with a
  portable token; this is stored per-session in `SlugMcpServer.session_auth`
  (an `Arc<RwLock<Option<SessionData>>>`). Stdio mode falls back to the
  on-disk encrypted session. See `get_active_session` in `server.rs`.

## Adding a new tool: checklist

1. Decide if it fits an existing service module. If not, add a new `src/<name>/`
   directory with `mod.rs` (and `scraper.rs` if HTTP+parse is non-trivial).
2. Write the `Request` struct (`#[derive(Debug, Deserialize, JsonSchema)]`) with
   doc comments — those become the schema descriptions the model sees.
3. Implement the service method using `cache.get_or_fetch(key, ttl_secs, ...)`.
4. If it needs an API key: add `Option<String>` field to `Config` in
   `config.rs`, plumb it through `main.rs` to the service constructor, and
   return a "not configured" string on `None`.
5. In `server.rs`: add `Arc<NewService>` to both `ServiceContext` and
   `SlugMcpServer`, and copy it in `SlugMcpServer::new`.
6. Add the `#[tool(description = "...")]` handler **inside `define_tools! { ... }`**.
   Auth-only tools go in the `$extra` branch.
7. `cargo clippy` and `cargo test`. Sanity-check by running `cargo run -- serve`
   and listing tools from a connected client (or eyeball the list in
   `tool_router`).

## Where to look

- **Canonical service example:** [src/dining/mod.rs](src/dining/mod.rs) +
  [src/dining/scraper.rs](src/dining/scraper.rs).
- **Tool registration patterns:** the `define_tools!` macro body in
  [src/server.rs](src/server.rs).
- **Auth flow:** [src/auth/mod.rs](src/auth/mod.rs) for the `AuthManager`,
  [src/auth/browser.rs](src/auth/browser.rs) for the CDP login dance,
  [src/auth/token.rs](src/auth/token.rs) for the portable token format.
- **Prior design notes:** [docs/plans/](docs/plans/) holds the original
  design and dining-scraper specs. Use them for historical context, not as
  current API reference — code wins on conflicts.
- **Deploy:** [deploy.sh](deploy.sh) and [.gitlab-ci.yml](.gitlab-ci.yml).
  Hosted at `https://2262-cse115b-02.be.ucsc.edu/mcp` on UCSC ITS infra.
