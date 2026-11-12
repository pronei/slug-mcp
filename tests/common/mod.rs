//! Shared harness for the integration tests: spawns the real `slug-mcp` binary
//! and talks to it as an MCP client, so the tests cover CLI parsing, transport
//! framing, the tool router, and the handlers end to end.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::time::Duration;

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ErrorCode};
use rmcp::service::{RoleClient, RunningService, ServiceError};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::Value;
use tempfile::TempDir;

/// Env vars that unlock keyed upstreams. The offline tests strip them so the
/// "key not configured" paths are exercised regardless of the developer's shell.
pub const API_KEY_VARS: &[&str] = &[
    "SLUG_MCP_BUSTIME_KEY",
    "SLUG_MCP_FIRMS_KEY",
    "AIRNOW_API_KEY",
    "EBIRD_API_KEY",
    "NPS_API_KEY",
];

/// Tools compiled into this build. Bump this (and the counts in README /
/// AGENTS.md) when a tool is added or removed.
#[cfg(feature = "auth")]
pub const EXPECTED_TOOL_COUNT: usize = 58;
#[cfg(not(feature = "auth"))]
pub const EXPECTED_TOOL_COUNT: usize = 54;

pub type Client = RunningService<RoleClient, ()>;

pub fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_slug-mcp")
}

/// A spawned stdio server plus the scratch HOME it writes its data dir into.
pub struct StdioServer {
    pub client: Client,
    _home: TempDir,
}

impl StdioServer {
    /// Spawn `slug-mcp serve` over stdio. `keep_keys` controls whether the
    /// caller's API-key env vars reach the child.
    pub async fn spawn(keep_keys: bool) -> Self {
        let home = tempfile::tempdir().expect("tempdir");
        let home_path = home.path().to_path_buf();
        let cmd = tokio::process::Command::new(binary()).configure(|cmd| {
            cmd.arg("serve");
            configure_env(cmd, &home_path, keep_keys);
        });
        let transport = TokioChildProcess::new(cmd).expect("spawn slug-mcp");
        let client = ().serve(transport).await.expect("MCP initialize over stdio");
        Self {
            client,
            _home: home,
        }
    }

    pub async fn call(&self, tool: &str, args: Value) -> Result<CallToolResult, ServiceError> {
        call_tool(&self.client, tool, args).await
    }

    pub async fn shutdown(self) {
        let _ = self.client.cancel().await;
    }
}

/// Point every data-dir lookup at a scratch HOME (macOS and XDG) and keep the
/// server's stderr quiet unless the developer asked for logs.
pub fn configure_env(cmd: &mut tokio::process::Command, home: &std::path::Path, keep_keys: bool) {
    cmd.env("HOME", home)
        .env("XDG_DATA_HOME", home.join("share"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"));
    // main.rs appends a global INFO directive after RUST_LOG, so a bare level
    // can't go below INFO; per-crate directives can.
    if std::env::var_os("RUST_LOG").is_none() {
        cmd.env("RUST_LOG", "slug_mcp=warn,rmcp=warn,html5ever=error");
    }
    // A developer's production allow-lists would make rmcp 403 our loopback
    // Host and Origin headers.
    cmd.env_remove("SLUG_MCP_ALLOWED_HOSTS");
    cmd.env_remove("SLUG_MCP_ALLOWED_ORIGINS");
    if !keep_keys {
        for var in API_KEY_VARS {
            cmd.env_remove(var);
        }
    }
}

pub async fn call_tool(
    client: &Client,
    tool: &str,
    args: Value,
) -> Result<CallToolResult, ServiceError> {
    let arguments = match args {
        Value::Object(map) => Some(map),
        Value::Null => None,
        other => panic!("tool arguments must be a JSON object, got {other}"),
    };
    let mut params = CallToolRequestParams::new(tool.to_string());
    params.arguments = arguments;
    client.call_tool(params).await
}

/// Concatenated text of every text content block.
pub fn text_of(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn is_tool_error(result: &CallToolResult) -> bool {
    result.is_error.unwrap_or(false)
}

/// The JSON-RPC error code of a protocol-level failure, if that is what `err` is.
pub fn error_code(err: &ServiceError) -> Option<ErrorCode> {
    match err {
        ServiceError::McpError(data) => Some(data.code),
        _ => None,
    }
}

/// Deterministic map of tool name → tool, for assertions and for the canary
/// to confirm it covers every public tool.
pub fn tools_by_name(tools: Vec<rmcp::model::Tool>) -> BTreeMap<String, rmcp::model::Tool> {
    tools.into_iter().map(|t| (t.name.to_string(), t)).collect()
}

/// An MCP client session with the Streamable HTTP server at `url`.
pub async fn connect_http(url: &str) -> Client {
    let transport = rmcp::transport::StreamableHttpClientTransport::from_uri(url.to_string());
    ().serve(transport)
        .await
        .unwrap_or_else(|e| panic!("MCP initialize over Streamable HTTP at {url}: {e}"))
}

/// A free loopback port. There is a small race between release and reuse,
/// which is acceptable for a test harness.
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind 127.0.0.1:0")
        .local_addr()
        .expect("local addr")
        .port()
}

/// Poll `/healthz` until it answers (`true`) or the child exits (`false`). The
/// first launch of a freshly linked binary can be slow on macOS, hence 60 s.
async fn wait_for_healthz(child: &mut tokio::process::Child, port: u16) -> bool {
    let healthz = format!("http://127.0.0.1:{port}/healthz");
    let http = reqwest::Client::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        if let Ok(resp) = http.get(&healthz).send().await
            && resp.status().is_success()
        {
            return true;
        }
        if let Ok(Some(_)) = child.try_wait() {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("slug-mcp --sse did not answer /healthz on port {port} within 60s");
}

/// `slug-mcp serve --sse` on a free port, ready once `/healthz` answers.
pub struct HttpServer {
    pub port: u16,
    child: tokio::process::Child,
    _home: TempDir,
}

impl HttpServer {
    pub async fn spawn(keep_keys: bool) -> Self {
        let home = tempfile::tempdir().expect("tempdir");
        // Another test can grab the port between `free_port` and the bind, so a
        // server that exits before answering is retried on a fresh port.
        for _ in 0..3 {
            let port = free_port();
            let mut cmd = tokio::process::Command::new(binary());
            cmd.args(["serve", "--sse", "--port", &port.to_string()]);
            configure_env(&mut cmd, home.path(), keep_keys);
            cmd.kill_on_drop(true);
            let mut child = cmd.spawn().expect("spawn slug-mcp --sse");
            if wait_for_healthz(&mut child, port).await {
                return Self {
                    port,
                    child,
                    _home: home,
                };
            }
        }
        panic!("slug-mcp --sse exited before answering /healthz, three times");
    }

    pub fn mcp_url(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.port)
    }

    pub async fn connect(&self) -> Client {
        connect_http(&self.mcp_url()).await
    }

    pub async fn shutdown(mut self) {
        let _ = self.child.kill().await;
    }
}
