use std::path::PathBuf;

use anyhow::Result;
use directories::ProjectDirs;

#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub sse_port: u16,
    pub eventbrite_api_key: Option<String>,
    pub bustime_api_key: Option<String>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let data_dir = ProjectDirs::from("edu", "ucsc", "slug-mcp")
            .map(|p| p.data_dir().to_path_buf())
            .unwrap_or_else(|| {
                dirs_fallback().unwrap_or_else(|| PathBuf::from(".slug-mcp"))
            });

        std::fs::create_dir_all(&data_dir)?;

        Ok(Self {
            data_dir,
            sse_port: std::env::var("SLUG_MCP_SSE_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3000),
            eventbrite_api_key: std::env::var("SLUG_MCP_EVENTBRITE_KEY").ok(),
            bustime_api_key: std::env::var("SLUG_MCP_BUSTIME_KEY").ok(),
        })
    }

    #[cfg(feature = "auth")]
    pub fn session_path(&self) -> PathBuf {
        self.data_dir.join("session.enc")
    }
}

fn dirs_fallback() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".slug-mcp"))
}

mod dirs {
    use std::path::PathBuf;
    pub fn home_dir() -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}
