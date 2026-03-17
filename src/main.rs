use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use rmcp::ServiceExt;

mod auth;
mod cache;
mod config;
mod dining;
mod events;
mod server;

#[derive(Parser)]
#[command(name = "slug-mcp", about = "MCP server for UCSC campus services")]
struct Cli {
    /// Run as an SSE server instead of stdio
    #[arg(long)]
    sse: bool,

    /// Port for the SSE server
    #[arg(long, default_value_t = 3000)]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let config = Arc::new(config::Config::load()?);
    let cache = Arc::new(cache::CacheStore::new(10_000));

    let http = reqwest::Client::new();
    let auth = Arc::new(auth::AuthManager::new(config.session_path()));
    let dining = Arc::new(dining::DiningService::new(http.clone(), cache.clone()));
    let events = Arc::new(events::EventsService::new(http, cache.clone()));

    if cli.sse {
        run_sse(cli.port, config, cache, auth, dining, events).await
    } else {
        let server = server::SlugMcpServer::new(config, cache, auth, dining, events);
        let service = server.serve(rmcp::transport::io::stdio()).await?;
        service.waiting().await?;
        Ok(())
    }
}

async fn run_sse(
    port: u16,
    config: Arc<config::Config>,
    cache: Arc<cache::CacheStore>,
    auth: Arc<auth::AuthManager>,
    dining: Arc<dining::DiningService>,
    events: Arc<events::EventsService>,
) -> Result<()> {
    use rmcp::transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    };

    let session_manager = Arc::new(LocalSessionManager::default());
    let sse_config = StreamableHttpServerConfig {
        stateful_mode: true,
        ..Default::default()
    };

    let service = StreamableHttpService::new(
        move || {
            Ok(server::SlugMcpServer::new(
                config.clone(),
                cache.clone(),
                auth.clone(),
                dining.clone(),
                events.clone(),
            ))
        },
        session_manager,
        sse_config,
    );

    let app = axum::Router::new().nest_service("/mcp", service);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
    tracing::info!("SSE server listening on http://0.0.0.0:{}/mcp", port);

    axum::serve(listener, app).await?;

    Ok(())
}
