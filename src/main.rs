use std::sync::Arc;
#[cfg(feature = "auth")]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(feature = "auth")]
use anyhow::Context;
use anyhow::Result;
use clap::{Parser, Subcommand};
use rmcp::ServiceExt;

mod academics;
#[cfg(feature = "auth")]
mod auth;
mod cache;
mod classrooms;
mod config;
mod degrees;
mod dining;
mod events;
mod library;
mod recreation;
mod server;
mod transit;

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

async fn run_serve(sse: bool, port: u16) -> Result<()> {
    let config = Arc::new(config::Config::load()?);
    let cache = Arc::new(cache::CacheStore::new(10_000));

    let http = reqwest::Client::new();
    #[cfg(feature = "auth")]
    let auth = Arc::new(auth::AuthManager::new(config.session_path()));
    let bustime_key = config.bustime_api_key.clone();
    let ctx = server::ServiceContext {
        config,
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
    };

    // Pre-warm dining menu cache daily at 5 AM Pacific
    let _refresher = dining::start_cache_refresher(http, ctx.cache.clone());

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
    let sse_config = StreamableHttpServerConfig {
        stateful_mode: true,
        ..Default::default()
    };

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
