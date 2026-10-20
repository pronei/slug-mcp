//! Offline integration tests: the real binary, a real MCP client, no network.
//!
//! Everything here is deterministic. Tools are only called on paths that
//! return before any upstream request (argument validation, missing API keys).

mod common;

use common::{
    EXPECTED_TOOL_COUNT, HttpServer, StdioServer, error_code, is_tool_error, text_of, tools_by_name,
};
use rmcp::model::ErrorCode;
use serde_json::json;

#[tokio::test]
async fn stdio_initialize_reports_identity_and_instructions() {
    let server = StdioServer::spawn(false).await;
    let info = server.client.peer_info().expect("initialize result");
    let identity = info.server_info.as_ref().expect("serverInfo");

    assert_eq!(identity.name, "slug-mcp");
    assert_eq!(identity.version, env!("CARGO_PKG_VERSION"));
    assert!(
        info.capabilities.tools.is_some(),
        "tools capability missing"
    );
    let instructions = info.instructions.as_deref().unwrap_or_default();
    assert!(
        instructions.contains("UCSC") && instructions.contains("Santa Cruz"),
        "instructions should orient the model: {instructions:?}"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn stdio_lists_every_tool_with_a_description_and_object_schema() {
    let server = StdioServer::spawn(false).await;
    let tools = server.client.list_all_tools().await.expect("tools/list");
    let names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
    let by_name = tools_by_name(tools);

    assert_eq!(
        by_name.len(),
        names.len(),
        "duplicate tool names: {names:?}"
    );
    assert_eq!(
        by_name.len(),
        EXPECTED_TOOL_COUNT,
        "tool count changed — update EXPECTED_TOOL_COUNT and the counts in README/AGENTS.md: {names:?}"
    );

    let mut problems = Vec::new();
    for (name, tool) in &by_name {
        let valid_name = !name.is_empty()
            && name.len() <= 64
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !valid_name {
            problems.push(format!("{name}: name is not snake_case ascii ≤ 64 chars"));
        }
        if tool.description.as_deref().unwrap_or("").trim().is_empty() {
            problems.push(format!("{name}: empty description"));
        }
        let schema = &tool.input_schema;
        if schema.get("type").and_then(|v| v.as_str()) != Some("object") {
            problems.push(format!("{name}: input schema is not an object: {schema:?}"));
        }
        // Doc comments on request fields become the descriptions the model
        // reads; an undocumented parameter is a usability bug.
        if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
            for (prop, def) in props {
                let documented = def
                    .get("description")
                    .and_then(|d| d.as_str())
                    .is_some_and(|d| !d.trim().is_empty());
                if !documented {
                    problems.push(format!("{name}.{prop}: parameter has no description"));
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "tool surface problems:\n{}",
        problems.join("\n")
    );

    server.shutdown().await;
}

#[tokio::test]
async fn unknown_tool_is_a_protocol_error() {
    let server = StdioServer::spawn(false).await;
    let err = server
        .call("no_such_tool", json!({}))
        .await
        .expect_err("unknown tool must not succeed");
    assert_eq!(error_code(&err), Some(ErrorCode::INVALID_PARAMS), "{err}");
    server.shutdown().await;
}

/// rmcp reports argument deserialization failures as tool results with
/// `isError`, so the model sees the expected type and can fix its call.
#[tokio::test]
async fn malformed_arguments_are_explained_as_tool_errors() {
    let server = StdioServer::spawn(false).await;
    let result = server
        .call("get_tides", json!({ "days": "many" }))
        .await
        .expect("deserialization failures are tool results, not protocol errors");
    assert!(is_tool_error(&result), "isError should be set: {result:?}");
    let text = text_of(&result);
    assert!(
        text.contains("expected u32") || text.contains("invalid type"),
        "message should explain the type mismatch: {text}"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn caller_mistakes_are_tool_level_errors_with_guidance() {
    let server = StdioServer::spawn(false).await;
    let result = server
        .call("search_outdoors", json!({ "category": "bogus" }))
        .await
        .expect("validation failures are tool results, not protocol errors");
    assert!(is_tool_error(&result), "isError should be set");
    let text = text_of(&result);
    assert!(
        text.contains("bogus") && text.contains("trails"),
        "message should name the bad value and list valid ones: {text}"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn dining_date_validation_happens_before_any_fetch() {
    let server = StdioServer::spawn(false).await;
    let result = server
        .call("get_dining_menu", json!({ "date": "tomorrow" }))
        .await
        .expect("bad dates are explained, not thrown");
    assert!(!is_tool_error(&result));
    let text = text_of(&result);
    assert!(text.contains("YYYY-MM-DD"), "{text}");
    server.shutdown().await;
}

#[tokio::test]
async fn missing_api_keys_degrade_to_instructions() {
    let server = StdioServer::spawn(false).await;
    let cases = [
        (
            "get_bus_predictions",
            json!({ "stop": "Science Hill" }),
            "SLUG_MCP_BUSTIME_KEY",
        ),
        (
            "get_service_alerts",
            json!({ "route": "18" }),
            "SLUG_MCP_BUSTIME_KEY",
        ),
        ("get_air_quality", json!({}), "AIRNOW_API_KEY"),
        ("get_fire_detections", json!({}), "SLUG_MCP_FIRMS_KEY"),
        (
            "get_national_park_info",
            json!({ "park_code": "pinn" }),
            "NPS_API_KEY",
        ),
        ("search_bird_observations", json!({}), "EBIRD_API_KEY"),
        ("get_bird_hotspots", json!({}), "EBIRD_API_KEY"),
        (
            "find_nearest_bird_sighting",
            json!({ "species": "Great Horned Owl" }),
            "EBIRD_API_KEY",
        ),
        (
            "get_historic_bird_observations",
            json!({ "year": 2025, "month": 9, "day": 3 }),
            "EBIRD_API_KEY",
        ),
    ];
    for (tool, args, env_var) in cases {
        let result = server
            .call(tool, args)
            .await
            .unwrap_or_else(|e| panic!("{tool} without a key must not error: {e}"));
        assert!(!is_tool_error(&result), "{tool}: isError set without a key");
        let text = text_of(&result);
        assert!(
            text.contains(env_var),
            "{tool}: should tell the caller to set {env_var}: {text}"
        );
    }
    server.shutdown().await;
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn check_auth_reports_logged_out_with_a_fresh_data_dir() {
    let server = StdioServer::spawn(false).await;
    let result = server
        .call("check_auth", json!({}))
        .await
        .expect("check_auth reads the session file only");
    assert!(!is_tool_error(&result));
    assert!(!text_of(&result).trim().is_empty());
    server.shutdown().await;
}

#[tokio::test]
async fn http_serves_healthz_and_the_same_tool_surface() {
    let server = HttpServer::spawn(false).await;

    let body = reqwest::get(format!("http://127.0.0.1:{}/healthz", server.port))
        .await
        .expect("GET /healthz")
        .text()
        .await
        .expect("body");
    assert_eq!(body, "ok");

    let client = server.connect().await;
    let info = client.peer_info().expect("initialize result");
    assert_eq!(
        info.server_info.as_ref().expect("serverInfo").name,
        "slug-mcp"
    );
    let tools = client.list_all_tools().await.expect("tools/list over HTTP");
    assert_eq!(tools.len(), EXPECTED_TOOL_COUNT);

    let result = common::call_tool(&client, "search_outdoors", json!({ "category": "bogus" }))
        .await
        .expect("tool call over HTTP");
    assert!(is_tool_error(&result));

    let _ = client.cancel().await;
    server.shutdown().await;
}

/// DNS-rebinding protection: rmcp only answers `/mcp` for Host headers in the
/// allow-list (loopback by default; `SLUG_MCP_ALLOWED_HOSTS` in production).
#[tokio::test]
async fn http_rejects_requests_with_an_unlisted_host_header() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let server = HttpServer::spawn(false).await;
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", server.port))
        .await
        .expect("connect");
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#;
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: evil.example.com\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.expect("read");
    let status_line = String::from_utf8_lossy(&response)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    assert!(
        status_line.contains(" 403"),
        "unlisted Host should be refused with 403, got: {status_line}"
    );
    server.shutdown().await;
}

// ─── 2026-07-28 spec conformance ───

/// Clients read these hints to decide which calls need the user's confirmation.
/// An omitted hint means "may write, may destroy", so every tool states them
/// explicitly, and only the auth tools may change any state.
#[tokio::test]
async fn every_tool_has_a_title_and_explicit_behavior_hints() {
    let server = HttpServer::spawn(false).await;
    let client = server.connect().await;
    let tools = tools_by_name(client.list_all_tools().await.expect("tools/list over HTTP"));

    let mut problems = Vec::new();
    let mut state_changing = Vec::new();
    for (name, tool) in &tools {
        if tool.title.as_deref().is_none_or(|t| t.trim().is_empty()) {
            problems.push(format!("{name}: no title"));
        }
        let Some(hints) = &tool.annotations else {
            problems.push(format!("{name}: no annotations"));
            continue;
        };
        if hints.open_world_hint.is_none() {
            problems.push(format!("{name}: open_world_hint unset"));
        }
        match hints.read_only_hint {
            None => problems.push(format!("{name}: read_only_hint unset")),
            Some(true) => {}
            Some(false) => {
                state_changing.push(name.as_str());
                if hints.destructive_hint.is_none() || hints.idempotent_hint.is_none() {
                    problems.push(format!(
                        "{name}: changes state but leaves destructive/idempotent hints unset"
                    ));
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "tool annotation problems:\n{}",
        problems.join("\n")
    );
    let expected: &[&str] = if cfg!(feature = "auth") {
        &["authenticate", "login"]
    } else {
        &[]
    };
    assert_eq!(state_changing, expected, "only the auth tools change state");

    let _ = client.cancel().await;
    server.shutdown().await;
}

#[tokio::test]
async fn initialize_reports_a_display_title_description_and_website() {
    let server = StdioServer::spawn(false).await;
    let info = server.client.peer_info().expect("initialize result");
    let identity = info.server_info.as_ref().expect("serverInfo");

    assert_eq!(identity.title.as_deref(), Some("Slug MCP"));
    assert!(
        identity
            .description
            .as_deref()
            .is_some_and(|d| d.contains("Santa Cruz")),
        "description: {:?}",
        identity.description
    );
    assert_eq!(
        identity.website_url.as_deref(),
        Some("https://github.com/pronei/slug-mcp")
    );
    server.shutdown().await;
}

/// A 2026-07-28 client has no session: each POST carries its protocol version
/// in `_meta`. Built by hand so the wire format is what's under test.
#[tokio::test]
async fn http_2026_07_28_tools_list_is_json_with_day_long_public_cache_hints() {
    let server = HttpServer::spawn(false).await;
    let response = reqwest::Client::new()
        .post(server.mcp_url())
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", "tools/list")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": { "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": { "name": "mcp-protocol-test", "version": "0" },
                "io.modelcontextprotocol/clientCapabilities": {}
            } }
        }))
        .send()
        .await
        .expect("POST tools/list");

    assert_eq!(response.status(), 200);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.starts_with("application/json"),
        "json_response should answer without SSE framing, got {content_type}"
    );
    let body: serde_json::Value = response.json().await.expect("JSON-RPC body");
    let result = &body["result"];
    assert_eq!(result["ttlMs"], 86_400_000, "{body}");
    assert_eq!(result["cacheScope"], "public", "{body}");
    let tools = result["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), EXPECTED_TOOL_COUNT);
    for tool in tools {
        assert!(
            tool["annotations"]["readOnlyHint"].is_boolean() && tool["title"].is_string(),
            "title and readOnlyHint should be on the wire: {tool}"
        );
    }
    server.shutdown().await;
}

/// Earlier protocol versions have no cache-hint fields, so none are sent.
#[tokio::test]
async fn legacy_tools_list_has_no_cache_hints() {
    let server = StdioServer::spawn(false).await;
    let result = server.client.list_tools(None).await.expect("tools/list");
    assert_eq!(result.tools.len(), EXPECTED_TOOL_COUNT);
    assert_eq!(result.ttl_ms, None);
    assert_eq!(result.cache_scope, None);
    server.shutdown().await;
}

async fn post_initialize(server: &HttpServer, origin: Option<&str>) -> reqwest::Response {
    let mut request = reqwest::Client::new()
        .post(server.mcp_url())
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "mcp-protocol-test", "version": "0" }
            }
        }));
    if let Some(origin) = origin {
        request = request.header("Origin", origin);
    }
    request.send().await.expect("POST /mcp")
}

/// The spec's Origin rule: a page on another site gets 403. Native clients
/// send no Origin and local browser tools use loopback origins; both pass.
#[tokio::test]
async fn http_refuses_cross_site_origins() {
    let server = HttpServer::spawn(false).await;
    for origin in [
        "https://evil.example.com",
        "http://localhost.evil.example.com",
        "null",
    ] {
        let status = post_initialize(&server, Some(origin)).await.status();
        assert_eq!(status, 403, "Origin {origin} should be refused");
    }
    for origin in [
        None,
        Some("http://localhost:6274"),
        Some("http://127.0.0.1:5173"),
    ] {
        let status = post_initialize(&server, origin).await.status();
        assert_eq!(status, 200, "Origin {origin:?} should be accepted");
    }
    server.shutdown().await;
}

#[tokio::test]
async fn http_rejects_request_bodies_over_64_kib() {
    let server = HttpServer::spawn(false).await;
    let response = reqwest::Client::new()
        .post(server.mcp_url())
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "x".repeat(64 * 1024), "version": "0" }
            }
        }))
        .send()
        .await
        .expect("POST /mcp");
    assert_eq!(response.status(), 413);
    server.shutdown().await;
}
