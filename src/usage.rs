//! Usage analytics: Prometheus counters for tool calls and sessions, plus one
//! log line per call. Fly scrapes `/metrics` from a separate internal port.

use std::time::Duration;

use anyhow::Result;
use metrics::{counter, describe_counter, describe_histogram, histogram};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};

const TOOL_CALLS: &str = "slug_mcp_tool_calls_total";
const TOOL_CALL_DURATION: &str = "slug_mcp_tool_call_duration_seconds";
const SESSIONS: &str = "slug_mcp_sessions_total";

/// Label for tool names the router doesn't know, so scanners can't mint series.
pub const UNKNOWN_TOOL: &str = "unknown";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    ToolError,
    ProtocolError,
}

impl Outcome {
    const ALL: [Outcome; 3] = [Outcome::Ok, Outcome::ToolError, Outcome::ProtocolError];

    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::ToolError => "tool_error",
            Outcome::ProtocolError => "protocol_error",
        }
    }
}

// Matched as substrings of the lowercased name, in order: "claude-code" must
// come before the "claude" catch-all for claude.ai and Claude Desktop.
const CLIENTS: &[(&str, &str)] = &[
    ("claude-code", "claude-code"),
    ("claude", "claude-ai"),
    ("codex", "codex"),
    ("openai", "chatgpt"),
    ("chatgpt", "chatgpt"),
    ("cursor", "cursor"),
    ("visual studio code", "vscode"),
    ("vscode", "vscode"),
    ("gemini", "gemini"),
    ("windsurf", "windsurf"),
    ("inspector", "mcp-inspector"),
    ("goose", "goose"),
    ("rmcp", "rmcp"),
];

/// Clamp a self-reported client name to a fixed label set; the raw name is
/// free text and would otherwise be unbounded label cardinality.
pub fn client_label(name: Option<&str>) -> &'static str {
    let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) else {
        return "unknown";
    };
    let name = name.to_lowercase();
    CLIENTS
        .iter()
        .find(|(needle, _)| name.contains(needle))
        .map_or("other", |(_, label)| label)
}

fn client_labels() -> impl Iterator<Item = &'static str> {
    let mut labels: Vec<&'static str> = CLIENTS.iter().map(|(_, label)| *label).collect();
    labels.extend(["unknown", "other"]);
    labels.sort_unstable();
    labels.dedup();
    labels.into_iter()
}

/// Install the global recorder. Without it (stdio mode) the macros are no-ops.
pub fn install(tool_names: &[String]) -> Result<PrometheusHandle> {
    let handle = PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full(TOOL_CALL_DURATION.into()),
            &[0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0],
        )?
        .install_recorder()?;

    describe_counter!(TOOL_CALLS, "MCP tools/call requests by tool and outcome.");
    describe_histogram!(
        TOOL_CALL_DURATION,
        metrics::Unit::Seconds,
        "MCP tools/call latency by tool."
    );
    describe_counter!(SESSIONS, "MCP initialize handshakes by client.");

    // A series that first appears at 1 loses that call to increase(): Prometheus
    // has no earlier sample to diff against. Starting every series at 0 makes
    // the first call after a deploy count.
    for tool in tool_names.iter().map(String::as_str).chain([UNKNOWN_TOOL]) {
        for outcome in Outcome::ALL {
            counter!(TOOL_CALLS, "tool" => tool.to_owned(), "outcome" => outcome.as_str())
                .increment(0);
        }
    }
    for client in client_labels() {
        counter!(SESSIONS, "client" => client).increment(0);
    }

    // install_recorder spawns nothing, and histogram buckets only drain on upkeep.
    let upkeep = handle.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tick.tick().await;
            upkeep.run_upkeep();
        }
    });

    Ok(handle)
}

/// Serve `/metrics` on its own listener so it stays off the public service.
pub async fn serve(listener: tokio::net::TcpListener, handle: PrometheusHandle) -> Result<()> {
    let app = axum::Router::new().route(
        "/metrics",
        axum::routing::get(move || {
            let handle = handle.clone();
            async move {
                (
                    [(
                        axum::http::header::CONTENT_TYPE,
                        "text/plain; version=0.0.4; charset=utf-8",
                    )],
                    handle.render(),
                )
            }
        }),
    );
    axum::serve(listener, app).await?;
    Ok(())
}

pub fn record_tool_call(
    tool: &str,
    known: bool,
    client: Option<&str>,
    outcome: Outcome,
    elapsed: Duration,
) {
    let label = if known { tool } else { UNKNOWN_TOOL };
    let client_label = client_label(client);
    counter!(TOOL_CALLS, "tool" => label.to_owned(), "outcome" => outcome.as_str()).increment(1);
    histogram!(TOOL_CALL_DURATION, "tool" => label.to_owned()).record(elapsed.as_secs_f64());

    let raw_client: String = client.unwrap_or("-").chars().take(64).collect();
    let tool: String = tool.chars().take(64).collect();
    tracing::info!(
        target: "slug_mcp::usage",
        %tool,
        client = %client_label,
        %raw_client,
        outcome = outcome.as_str(),
        ms = elapsed.as_millis() as u64,
        "tool call"
    );
}

pub fn record_session(client: &str) {
    counter!(SESSIONS, "client" => client_label(Some(client))).increment(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_names_clamp_to_known_labels() {
        assert_eq!(client_label(Some("claude-code")), "claude-code");
        assert_eq!(client_label(Some("claude-ai")), "claude-ai");
        assert_eq!(client_label(Some("Claude Desktop")), "claude-ai");
        assert_eq!(client_label(Some("openai-mcp")), "chatgpt");
        assert_eq!(client_label(Some("codex-mcp-client")), "codex");
        assert_eq!(client_label(Some("Visual Studio Code")), "vscode");
        assert_eq!(client_label(Some("inspector-client")), "mcp-inspector");
        assert_eq!(client_label(Some("rmcp")), "rmcp");
        assert_eq!(client_label(Some("  ")), "unknown");
        assert_eq!(client_label(None), "unknown");
        assert_eq!(client_label(Some("my-homebrew-agent")), "other");
    }

    #[test]
    fn every_client_label_is_preregistered() {
        let labels: Vec<_> = client_labels().collect();
        for name in ["claude-code", "openai-mcp", "", "my-homebrew-agent"] {
            let label = client_label(Some(name));
            assert!(labels.contains(&label), "{label}");
        }
        assert_eq!(labels.iter().filter(|l| **l == "vscode").count(), 1);
    }
}
