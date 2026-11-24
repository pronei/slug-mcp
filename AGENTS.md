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
cargo run -- serve --sse --metrics-port 9091 # plus Prometheus /metrics on :9091
cargo run -- export-token                    # browser login, prints portable token to stdout

cargo test                                   # unit tests + offline MCP protocol tests (no network)
cargo test --test live_canary -- --ignored   # live canaries, one per public tool (hits upstreams)
cargo clippy --all-targets -- -D warnings    # lints; CI is strict, on both feature sets
cargo fmt
cargo deny check                             # advisories/licenses/sources; same policy as CI
cargo llvm-cov nextest --profile ci          # coverage; HTML with `cargo llvm-cov report --html`
```

Logging: `RUST_LOG=slug_mcp=debug cargo run -- serve` (writes to stderr; stdout is
reserved for MCP framing on stdio transport, and for the token on `export-token`).
`main.rs` appends a global INFO directive after `RUST_LOG`, so a bare level such as
`RUST_LOG=warn` can't go below INFO; per-crate directives can
(`RUST_LOG=slug_mcp=warn,rmcp=warn`).

## Testing tiers

1. **Unit tests** (`mod tests` in each module, fixtures in `src/<service>/fixtures/`):
   parsers and formatters against saved upstream responses. Offline, sub-second.
2. **Offline integration** ([tests/mcp_protocol.rs](tests/mcp_protocol.rs)): spawns the
   built binary and drives it as a real MCP client over stdio and Streamable HTTP.
   Asserts the tool surface (count, names, titles and behaviour hints, schemas, every
   parameter documented), server identity, error semantics (`isError` for caller
   mistakes, -32602 for unknown tools), "key not configured" degradation, and the HTTP
   transport (`/healthz`, Host and Origin allow-lists, the body cap, answers to
   probing clients). Only calls code paths that return before any upstream request.
   Runs in CI.
3. **Live canaries** ([tests/live_canary.rs](tests/live_canary.rs), all `#[ignore]`):
   one test per public tool against its real upstream. Each asserts the headings and
   markers its formatter promises (`# Tide predictions`, `(recipe: `, …), that no raw
   HTML entity reaches the model, and, where data can go stale without erroring, a
   freshness check (the group-exercise schedule must be for the current quarter).
   Three tests chain ids (menu → nutrition, occupancy → schedule, requirements →
   progress). A failure's first line is `<tool>: <what broke> — adapter <file> ←
   <upstream>`, taken from the `ADAPTERS` table. A protocol-level failure is retried
   once after 15 s so a momentary blip isn't reported as drift.
   `SLUG_MCP_CANARY_URL=https://slug-mcp.fly.dev/mcp` points the same tests at a
   deployed server. `canary.yml` runs both targets daily and keeps one `canary`
   issue per target current.
4. **Coverage**: `cargo llvm-cov nextest --profile ci` (CI uploads to Codecov and
   the job summary). The spawned binary is instrumented too, so handler code in
   `server.rs` counts, not only the fixture-driven parsers.

When a tool is added or removed: bump `EXPECTED_TOOL_COUNT` in `tests/common/mod.rs`
(59 with the auth feature, 55 public), add an `ADAPTERS` row and a canary for it in
`tests/live_canary.rs` (`every_public_tool_has_a_canary` fails otherwise), and update
the tool counts in README / this file.

## Module layout

```
src/
├── main.rs        # CLI parsing, service wiring, transport setup
├── server.rs      # All MCP tool handlers (one big file by design — single source
│                  # of truth for the tool surface).
├── cache.rs       # Type-erased TTL cache (moka). No JSON round-trip.
├── config.rs      # Env var → Config struct. Optional API keys.
├── usage.rs       # Tool-call/session counters, /metrics listener, per-call log.
├── util.rs        # `now_pacific()`, `strip_html_tags`, `truncate`,
│                  # `degrees_to_compass`, `selectors!` macro.
├── util/fuzzy.rs  # FuzzyMatcher — case/whitespace-insensitive name matching.
├── auth/          # CDP-driven CruzID+Duo login; AES-256-GCM session storage;
│                  # portable token codec. All gated on `feature = "auth"`.
└── <service>/     # 28 service modules — see "The pattern" below.
```

Service modules: `academics`, `air_forecast`, `air_quality`, `astronomy`,
`beach_water`, `bike_share`, `biodiversity`, `buoy`, `classrooms`, `climbing`, `degrees`,
`dining`, `earthquakes`, `events`, `fire`, `library`, `marine`, `nps`, `ocean`,
`recreation`, `space_weather`, `summer`, `tides`, `traffic`,
`transit`, `usgs_water`, `wave_buoy`, `weather`.

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

1. `server.rs` → add an `Arc<FooService>` field to `ServiceContext`; `main.rs` →
   construct it in `run_serve`.
2. `server.rs` → add the matching field to `SlugMcpServer` and copy it across in
   `SlugMcpServer::new(ctx)`.
3. `server.rs` → add the `#[tool]` handler **inside the `define_tools! { ... }`
   macro body**, not in a sibling impl block. The macro exists so auth-only
   tools can be cfg-gated through `$extra` while keeping `#[tool_router]` happy.

## Conventions

- **rmcp 3.x API.** Tool handler params use `Parameters<T>` wrapping the request
  struct — *not* `#[tool(aggr)]` (older API). Tool results wrap
  `ContentBlock::text(...)`. `ServerHandler::call_tool` returns
  `CallToolResponse` (the router's result converts via `Into`).
  `ListToolsResult` carries cache hints (`ttlMs`, `cacheScope`): build it with
  `ListToolsResult::with_all_items(..)` and set them only for 2026-07-28+ peers
  (`context.protocol_version()`). `get_info` returns `ServerConfig` (the
  `ServerInfo` alias is deprecated), a builder over a `#[non_exhaustive]`
  struct, so don't construct it directly. Streamable HTTP config uses
  `legacy_session_mode` (sessions only for protocol versions before
  2026-07-28; newer clients are served statelessly per SEP-2567) and enforces
  an Origin allow-list (`SLUG_MCP_ALLOWED_ORIGINS`; requests without an Origin
  header pass).

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

- **`nutrition.sa.ucsc.edu` also sends the wrong intermediate certificate.**
  macOS and browsers repair the chain through AIA, so menus work on a laptop;
  OpenSSL on Linux doesn't, so the Docker image installs
  `certs/InCommon_ECC_OV_SSL_CA_3.crt`. Menus that fail only in production are
  this, not the parser.

- **Eventbrite blocks the hosted server.** Its CloudFront answers Fly's IPs with a
  405 CAPTCHA page, so `search_eventbrite_events` errors on
  `slug-mcp.fly.dev` while working from campus and home networks. Headers won't
  get past a CAPTCHA.

- **CHP sometimes serves a broken copy of `sa.xml`.** `media.chp.ca.gov` is one
  address behind a load balancer, and it can hand a caller a file that is cut
  off mid-tag and frozen hours in the past, as a 200, while other callers get
  the live feed (2026-11-19: 180,224 bytes, last modified 11:57 PM the night
  before). It looks host-specific and isn't: Fly and a laptop each had it at
  different times that day. Fetching again returns the same copy, so the
  adapter doesn't retry. It rejects cut-off and stale copies, shows the last
  good list for up to 30 minutes, and asks CHP nothing for five minutes. The
  `(details: …)` text in the tool output and in `fly logs` says what came back.

- **Overwriting a live cache key resets its expiry** to the new TTL.
  `PerEntryExpiry` overrides moka's `expire_after_update`, whose default keeps
  the first write's deadline.

- **`get_or_fetch` errors show the whole cause chain with URL query strings
  redacted**, since callers pass them to clients and reqwest errors print the
  request URL. A key in a query string is covered; a key in the URL path is
  not, so that adapter strips the URL itself (FIRMS uses
  `reqwest::Error::without_url`).

- **`events.ucsc.edu` is WordPress + Tribe Events plugin** at
  `/wp-json/tribe/events/v1/events`. It is *not* Localist, despite earlier campus
  pages suggesting that. Eventbrite is a separate API for community events; both
  tools should typically be called together for full coverage.

- **CruzID auth is browser-driven.** `auth/browser.rs` drives Chrome via CDP
  (chromiumoxide). Headless servers can't run the `login` tool — use
  `slug-mcp export-token` locally and the `authenticate` tool remotely.

- **BusTime and GTFS-RT use different stop-id namespaces.** BusTime `stpid`s
  (the stops catalog) never appear in the GTFS-RT trip_updates feed, so
  `get_bus_predictions` is BusTime-first; GTFS-RT only supplies vehicle
  positions/occupancy and system alerts. Real GTFS-RT predictions (and true
  per-route delays) need a static-GTFS `stops.txt`/`stop_times.txt` loader.

- **The public build exposes 55 tools, not 59.** The Dockerfile builds
  `--no-default-features`, which compiles out the four auth tools.

- **`docs/` is git-ignored.** Local design notes and audit write-ups live there
  but never ship; anything that must be tracked goes in README/AGENTS/CHANGELOG.

- **Stdio mode owns stdout.** Anything written to stdout in stdio transport
  corrupts MCP framing. All logs/diagnostics go to stderr. Tracing is configured
  to write to `std::io::stderr` for this reason.

- **Per-session auth state for SSE.** SSE clients call `authenticate` with a
  portable token; this is stored per-session in `SlugMcpServer.session_auth`
  (an `Arc<RwLock<Option<SessionData>>>`). Stdio mode falls back to the
  on-disk encrypted session. See `get_active_session` in `server.rs`.

- **rmcp turns argument deserialization failures into tool results**
  (`isError: true`, "failed to deserialize parameters: …"), not JSON-RPC -32602,
  so the model can correct its call. Only an unknown tool name is -32602.

- **Canary markers are part of a formatter's contract.** Each live canary asserts
  on the heading its tool renders (`# Santa Cruz Weather Forecast`, `## Schedule:`).
  Renaming a heading means updating the canary — on purpose: headings are what the
  model reads, so a silent rename is a behaviour change.

- **The local canary trusts the InCommon intermediate.** `nutrition.sa.ucsc.edu`
  sends the wrong intermediate certificate; macOS repairs the chain, OpenSSL on a
  Linux runner doesn't. `canary.yml` installs `certs/InCommon_ECC_OV_SSL_CA_3.crt`
  (the same file the Docker image on `deploy/flyio` installs) so dining canaries
  fail on drift, not TLS.

- **Dependabot's Docker bumps land on `main`.** Deploys fast-forward `deploy/flyio`
  from `main`, so a merged base-image bump ships with the next
  `git push origin main:deploy/flyio`, like any other change.

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
6. Add the `#[tool(title = "...", annotations(...), description = "...")]` handler
   **inside `define_tools! { ... }`**. Auth-only tools go in the `$extra` branch.
   Data tools take `annotations(read_only_hint = true, open_world_hint = true)`;
   a tool that changes state also sets `destructive_hint` and `idempotent_hint`.
   `tests/mcp_protocol.rs` fails when a tool leaves a hint or its title unset.
7. `cargo clippy` and `cargo test`. Sanity-check by running `cargo run -- serve`
   and listing tools from a connected client (or eyeball the list in
   `tool_router`).

## Deploy

Hosted on Fly.io as app `slug-mcp` (region `sjc`) at
`https://slug-mcp.fly.dev/mcp`. Pushes to the `deploy/flyio` branch on GitHub
deploy through [.github/workflows/fly-deploy.yml](.github/workflows/fly-deploy.yml);
pushes to `main` don't. To ship what's on `main`, fast-forward the deploy branch:

```bash
git push origin main:deploy/flyio
```

To deploy by hand instead:

```bash
fly deploy --remote-only --ha=false
```

- **`deploy/flyio` is long-lived.** Merge PRs from it with a merge commit, not a
  squash, so it can keep fast-forwarding to `main`; don't delete it after a merge.

- **One machine, always on.** The tool cache and legacy MCP sessions live in
  process memory. `fly.toml` turns auto-stop off, and `--ha=false` stops Fly
  from adding a second machine that would split sessions.
- **Public hostnames go in `SLUG_MCP_ALLOWED_HOSTS`** (set in `fly.toml`). rmcp
  answers 403 to any `/mcp` request whose Host header isn't listed; `/healthz`
  isn't checked. A custom domain needs adding there.
- **The container binds `--host 0.0.0.0`.** The default is `127.0.0.1` for local
  runs, which Fly's proxy can't reach.
- **API keys are Fly secrets** (`fly secrets list`), never `[env]` in `fly.toml`.
- **`/metrics` is on port 9091, not 3000.** `fly.toml`'s `[metrics]` section has
  Fly scrape it privately every 15 s; it isn't in `[http_service]`, so it isn't
  public. The metrics listener binds `[::]` when `--host` is `0.0.0.0`, because
  the scraper comes in over the machine's private IPv6 address.

## Usage metrics

`ServerHandler::call_tool` in `server.rs` records every tool call through
`usage.rs`; `initialize` records each legacy session. Fly keeps about 15 days
in the managed Prometheus behind [fly-metrics.net](https://fly-metrics.net)
(Explore → Prometheus), and the same PromQL works against
`https://api.fly.io/prometheus/personal/api/v1/query` with an
`Authorization: FlyV1 <token>` header (flyctl's `fm2_` token isn't a Bearer).

| Metric | Labels |
| --- | --- |
| `slug_mcp_tool_calls_total` | `tool`, `outcome` (`ok`, `tool_error`, `protocol_error`) |
| `slug_mcp_tool_call_duration_seconds` (histogram) | `tool` |
| `slug_mcp_sessions_total` | `client` |

- **Labels are bounded.** An unregistered tool name is counted as
  `tool="unknown"`, and client names are clamped by `CLIENTS` in `usage.rs`
  to a fixed list plus `other` and `unknown`. The per-call log line
  (`slug_mcp::usage`, "tool call") carries the raw client name: grep it for
  `client=other` and add new names to `CLIENTS`.
- **Sessions count `initialize` handshakes only.** 2026-07-28 clients are
  stateless and never send one; their name rides in each request's `_meta`
  and shows up in the log line, not in `slug_mcp_sessions_total`.
- **Every series starts at 0** after a deploy, so `increase()` counts the first
  call. Counters reset on each deploy; `increase()` handles that.

```promql
# Calls per tool per day
sum by (tool) (increase(slug_mcp_tool_calls_total[1d]))

# Calls per day, all tools
sum(increase(slug_mcp_tool_calls_total[1d]))

# Error rate per tool over a day (tool errors and protocol errors)
sum by (tool) (increase(slug_mcp_tool_calls_total{outcome!="ok"}[1d]))
  / sum by (tool) (increase(slug_mcp_tool_calls_total[1d]))

# p95 latency per tool over a day
histogram_quantile(0.95,
  sum by (tool, le) (increase(slug_mcp_tool_call_duration_seconds_bucket[1d])))

# Sessions per client per day
sum by (client) (increase(slug_mcp_sessions_total[1d]))
```

## Where to look

- **Canonical service example:** [src/dining/mod.rs](src/dining/mod.rs) +
  [src/dining/scraper.rs](src/dining/scraper.rs).
- **Tool registration patterns:** the `define_tools!` macro body in
  [src/server.rs](src/server.rs).
- **CI and checks:** [.github/workflows/ci.yml](.github/workflows/ci.yml) (fmt, clippy,
  tests, coverage, MSRV, cargo-deny), [.github/workflows/canary.yml](.github/workflows/canary.yml)
  (daily live canaries, local build and hosted server → one `canary` issue each), [.github/dependabot.yml](.github/dependabot.yml),
  [deny.toml](deny.toml), [.config/nextest.toml](.config/nextest.toml).
- **Auth flow:** [src/auth/mod.rs](src/auth/mod.rs) for the `AuthManager`,
  [src/auth/browser.rs](src/auth/browser.rs) for the CDP login dance,
  [src/auth/token.rs](src/auth/token.rs) for the portable token format.
- **Prior design notes:** [docs/plans/](docs/plans/) holds the original
  design and dining-scraper specs. Use them for historical context, not as
  current API reference — code wins on conflicts.
- **Deploy:** [Dockerfile](Dockerfile), [fly.toml](fly.toml), and
  [.github/workflows/fly-deploy.yml](.github/workflows/fly-deploy.yml); see
  [Deploy](#deploy).
