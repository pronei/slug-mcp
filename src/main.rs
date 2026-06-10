use std::sync::Arc;
#[cfg(feature = "auth")]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(feature = "auth")]
use anyhow::Context;
use anyhow::Result;
use clap::{Parser, Subcommand};
use rmcp::ServiceExt;

mod academics;
mod air_forecast;
mod air_quality;
mod astronomy;
#[cfg(feature = "auth")]
mod auth;
mod beach_water;
mod biodiversity;
mod buoy;
mod cache;
mod classrooms;
mod climbing;
mod config;
mod degrees;
mod dining;
mod earthquakes;
mod events;
mod fire;
mod library;
mod marine;
mod nps;
mod ocean;
mod outdoors;
mod recreation;
mod server;
mod space_weather;
mod tides;
mod traffic;
mod transit;
mod usgs_water;
mod util;
mod wave_buoy;
mod weather;

#[derive(Parser)]
#[command(name = "slug-mcp", about = "MCP server for UCSC campus services")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the MCP server (default if no subcommand given)
    Serve {
        /// Run as an SSE server instead of stdio
        #[arg(long)]
        sse: bool,
        /// Port for the SSE server
        #[arg(long, default_value_t = 3000)]
        port: u16,
    },
    /// Login locally via browser and print a portable auth token to stdout.
    /// Use this token with the `authenticate` tool on a remote SSE server.
    #[cfg(feature = "auth")]
    ExportToken,
    /// Interactive live check of the auth flow — login, meal balance, and an
    /// S&E study-room booking on a given date.
    #[cfg(feature = "auth")]
    VerifyAuth {
        /// Date to book (YYYY-MM-DD).
        #[arg(long, default_value = "2026-06-12")]
        date: String,
        /// Actually submit the booking (otherwise stops after showing availability).
        #[arg(long)]
        book: bool,
        /// Space ID to book (default: first room with the longest open block).
        #[arg(long)]
        space_id: Option<u32>,
        /// Booking start time, e.g. "14:00" (default: earliest available).
        #[arg(long)]
        start: Option<String>,
        /// Booking end time, e.g. "16:00" (default: start + up to 2h contiguous).
        #[arg(long)]
        end: Option<String>,
        /// Group name for the booking form, if the room requires one.
        #[arg(long)]
        group: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command.unwrap_or(Command::Serve {
        sse: false,
        port: 3000,
    }) {
        #[cfg(feature = "auth")]
        Command::ExportToken => run_export_token().await,
        #[cfg(feature = "auth")]
        Command::VerifyAuth {
            date,
            book,
            space_id,
            start,
            end,
            group,
        } => {
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
                .with_writer(std::io::stderr)
                .init();
            run_verify_auth(date, book, space_id, start, end, group).await
        }
        Command::Serve { sse, port } => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::from_default_env()
                        .add_directive(tracing::Level::INFO.into()),
                )
                .with_writer(std::io::stderr)
                .init();

            run_serve(sse, port).await
        }
    }
}

#[cfg(feature = "auth")]
async fn run_export_token() -> Result<()> {
    // Minimal logging — only errors, since stdout is for the token
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("error"))
        .with_writer(std::io::stderr)
        .init();

    eprintln!("Opening browser for UCSC login...");
    eprintln!("Complete CruzID + Duo authentication in the browser window.");

    let cookies = auth::browser::login_via_browser()
        .await
        .context("browser login failed")?;

    let cookie_data =
        auth::browser::serialize_cookies(&cookies).context("failed to serialize cookies")?;

    let username =
        auth::browser::extract_username(&cookies).unwrap_or_else(|| "UCSC User".to_string());

    let expires_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
        + 8 * 3600; // 8 hours

    let session_data = auth::session::SessionData {
        cookies: cookie_data,
        username: username.clone(),
        expires_at,
    };

    let token = auth::token::encode_token(&session_data)?;

    eprintln!();
    eprintln!("Authenticated as {}. Token valid for 8 hours.", username);
    eprintln!("Pass this token to the `authenticate` tool on the remote server:");
    eprintln!();
    println!("{}", token);

    Ok(())
}

#[cfg(feature = "auth")]
async fn run_verify_auth(
    date: String,
    book: bool,
    space_override: Option<u32>,
    start_override: Option<String>,
    end_override: Option<String>,
    group: Option<String>,
) -> Result<()> {
    let config = Arc::new(config::Config::load()?);
    let cache = Arc::new(cache::CacheStore::new(1_000));
    let http = reqwest::Client::builder()
        .user_agent("slug-mcp/0.1 (+https://git.ucsc.edu/pmundra/slug-mcp; student project)")
        .gzip(true)
        .build()?;

    let auth = auth::AuthManager::new(config.session_path());

    // 1. Reuse a saved session if still valid, else open a browser to log in.
    let session = match auth.get_session()? {
        Some(s) => {
            eprintln!("✓ Reusing saved session for {}", s.username);
            s
        }
        None => {
            eprintln!("Opening Chrome for UCSC login — complete CruzID + Duo in the window…");
            let username = auth.login().await.context("browser login failed")?;
            eprintln!("✓ Logged in as {username}");
            auth.get_session()?
                .context("session not found after login")?
        }
    };

    let client = auth::build_authenticated_client(&session.cookies)?;

    // 2. Meal balance (exercises saml_aware_get + cbord scrape).
    eprintln!("\n── Meal balance ──");
    let dining = dining::DiningService::new(http.clone(), cache.clone());
    match dining.get_balance(&client, &session.username).await {
        Ok(result) => {
            println!("{}", result.balance.format());
            if let Some(snippet) = result.debug_snippet {
                eprintln!("⚠ balance parse failed; page text:\n{snippet}");
            }
        }
        Err(e) => eprintln!("✗ balance error: {e:#}"),
    }

    // 3. S&E availability for the target date.
    eprintln!("\n── S&E availability for {date} ──");
    let library = library::LibraryService::new(http.clone(), cache.clone());
    let availability = library
        .get_availability(Some("science"), Some(&date))
        .await
        .context("availability fetch failed")?;
    println!("{availability}");

    if !book {
        eprintln!("\n(--book not set; stopping before reservation)");
        return Ok(());
    }

    // 4. Resolve the room + window: explicit overrides win; otherwise pick the
    //    first room with the longest contiguous open block (cap 2h).
    let avail = library::scraper::scrape_availability(&client, 16578, &date).await?;
    let pick = match space_override {
        Some(sid) => avail
            .rooms
            .iter()
            .find(|r| r.space_id == Some(sid))
            .and_then(|r| {
                let (s, e) = match (&start_override, &end_override) {
                    (Some(s), Some(e)) => (s.clone(), e.clone()),
                    _ => best_window(&r.available_slots)?,
                };
                Some((sid, s, e, r.name.clone()))
            }),
        None => avail
            .rooms
            .iter()
            .filter_map(|r| {
                let sid = r.space_id?;
                let (s, e) = match (&start_override, &end_override) {
                    (Some(s), Some(e)) => {
                        // Room must have the whole window open as a contiguous
                        // chain of slots, not merely the starting slot.
                        window_is_open(&r.available_slots, s, e).then_some(())?;
                        (s.clone(), e.clone())
                    }
                    _ => best_window(&r.available_slots)?,
                };
                Some((sid, s, e, r.name.clone()))
            })
            .next(),
    };
    let Some((space_id, start, end, name)) = pick else {
        eprintln!("No bookable open slots match at S&E on {date}.");
        return Ok(());
    };

    eprintln!("\n── Booking {name} (space {space_id}) {date} {start}–{end} ──");
    let result = library
        .book(&client, space_id, &date, &start, &end, group.as_deref())
        .await
        .context("booking failed")?;
    println!("{result}");

    Ok(())
}

/// True if `[start, end)` is fully covered by a contiguous chain of slots.
#[cfg(feature = "auth")]
fn window_is_open(slots: &[library::scraper::TimeSlot], start: &str, end: &str) -> bool {
    let mut cursor = start.to_string();
    while cursor != end {
        match slots.iter().find(|t| t.start == cursor) {
            Some(t) => cursor = t.end.clone(),
            None => return false,
        }
    }
    true
}

/// Longest contiguous run of 30-min slots from the earliest available start,
/// capped at 2 hours. Returns ("HH:MM" start, "HH:MM" end).
#[cfg(feature = "auth")]
fn best_window(slots: &[library::scraper::TimeSlot]) -> Option<(String, String)> {
    let mut s: Vec<&library::scraper::TimeSlot> = slots.iter().collect();
    s.sort_by(|a, b| a.start.cmp(&b.start));
    let first = s.first()?;
    let mut end = first.end.clone();
    let mut count = 1;
    for w in s.windows(2) {
        if w[0].end == w[1].start && count < 4 {
            end = w[1].end.clone();
            count += 1;
        } else {
            break;
        }
    }
    Some((first.start.clone(), end))
}

async fn run_serve(sse: bool, port: u16) -> Result<()> {
    let config = Arc::new(config::Config::load()?);
    let cache = Arc::new(cache::CacheStore::new(10_000));

    // Shared HTTP client: gzip for smaller responses, explicit User-Agent so
    // public upstream APIs (notably NOAA NWS, which rejects blank UAs) can
    // identify us, and bounded timeouts so one hung upstream (Overpass,
    // ERDDAP, NDBC…) can't wedge a tool call forever. All services clone from
    // this single client.
    let http = reqwest::Client::builder()
        .user_agent("slug-mcp/0.1 (+https://git.ucsc.edu/pmundra/slug-mcp; student project)")
        .gzip(true)
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build HTTP client: {}", e))?;
    #[cfg(feature = "auth")]
    let auth = Arc::new(auth::AuthManager::new(config.session_path()));
    let bustime_key = config.bustime_api_key.clone();
    let firms_key = config.firms_map_key.clone();
    let ebird_key = config.ebird_api_key.clone();
    let airnow_key = config.airnow_api_key.clone();
    let nps_key = config.nps_api_key.clone();
    let biodiversity = Arc::new(biodiversity::BiodiversityService::new(
        http.clone(),
        cache.clone(),
        ebird_key,
    ));
    let ctx = server::ServiceContext {
        cache: cache.clone(),
        #[cfg(feature = "auth")]
        auth,
        degrees: Arc::new(degrees::DegreeService::new(http.clone(), cache.clone())),
        dining: Arc::new(dining::DiningService::new(http.clone(), cache.clone())),
        events: Arc::new(events::EventsService::new(http.clone(), cache.clone())),
        recreation: Arc::new(recreation::RecreationService::new(http.clone(), cache.clone())),
        library: Arc::new(library::LibraryService::new(http.clone(), cache.clone())),
        academics: Arc::new(academics::AcademicsService::new(http.clone(), cache.clone())),
        classrooms: Arc::new(classrooms::ClassroomService::new(http.clone(), cache.clone())),
        transit: Arc::new(transit::TransitService::new(http.clone(), cache.clone(), bustime_key)),
        weather: Arc::new(weather::WeatherService::new(http.clone(), cache.clone())),
        marine: Arc::new(marine::MarineService::new(http.clone(), cache.clone())),
        fire: Arc::new(fire::FireService::new(http.clone(), cache.clone(), firms_key)),
        traffic: Arc::new(traffic::TrafficService::new(http.clone(), cache.clone())),
        tides: Arc::new(tides::TidesService::new(http.clone(), cache.clone())),
        buoy: Arc::new(buoy::BuoyService::new(http.clone(), cache.clone())),
        wave_buoy: Arc::new(wave_buoy::WaveBuoyService::new(http.clone(), cache.clone())),
        usgs_water: Arc::new(usgs_water::UsgsWaterService::new(http.clone(), cache.clone())),
        biodiversity: biodiversity.clone(),
        air_quality: Arc::new(air_quality::AirQualityService::new(
            http.clone(),
            cache.clone(),
            airnow_key,
        )),
        astronomy: Arc::new(astronomy::AstronomyService::new(http.clone(), cache.clone())),
        space_weather: Arc::new(space_weather::SpaceWeatherService::new(http.clone(), cache.clone())),
        outdoors: Arc::new(outdoors::OutdoorsService::new(http.clone(), cache.clone())),
        climbing: Arc::new(climbing::ClimbingService::new(http.clone(), cache.clone())),
        earthquakes: Arc::new(earthquakes::EarthquakeService::new(http.clone(), cache.clone())),
        beach_water: Arc::new(beach_water::BeachWaterService::new(http.clone(), cache.clone())),
        nps: Arc::new(nps::NpsService::new(http.clone(), cache.clone(), nps_key)),
        air_forecast: Arc::new(air_forecast::AirForecastService::new(http.clone(), cache.clone())),
        ocean: Arc::new(ocean::OceanService::new(http.clone(), cache.clone(), biodiversity)),
    };

    // Pre-warm dining menu cache daily at 5 AM Pacific. The handle is watched
    // by a sibling task so an unexpected panic surfaces in logs instead of
    // silently leaving the cache un-refreshed.
    let refresher_handle = dining::start_cache_refresher(http, ctx.cache.clone());
    tokio::spawn(async move {
        match refresher_handle.await {
            Ok(()) => tracing::warn!("dining cache refresher exited (loop should be infinite)"),
            Err(e) => tracing::error!("dining cache refresher task failed: {}", e),
        }
    });

    if sse {
        run_sse(port, ctx).await
    } else {
        let server = server::SlugMcpServer::new(ctx);
        let service = server.serve(rmcp::transport::io::stdio()).await?;
        service.waiting().await?;
        Ok(())
    }
}

async fn run_sse(port: u16, ctx: server::ServiceContext) -> Result<()> {
    use rmcp::transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    };

    let session_manager = Arc::new(LocalSessionManager::default());
    // StreamableHttpServerConfig is #[non_exhaustive] as of rmcp 1.x — build
    // from Default and set the fields we care about.
    let mut sse_config = StreamableHttpServerConfig::default();
    sse_config.stateful_mode = true;

    let service = StreamableHttpService::new(
        move || Ok(server::SlugMcpServer::new(ctx.clone())),
        session_manager,
        sse_config,
    );

    let app = axum::Router::new().nest_service("/mcp", service);

    let listener = tokio::net::TcpListener::bind(format!("localhost:{}", port)).await?;
    tracing::info!("SSE server listening on http://localhost:{}/mcp", port);

    axum::serve(listener, app).await?;

    Ok(())
}
