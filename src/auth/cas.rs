use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use axum::extract::Query;
use axum::response::Html;
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use tokio::sync::oneshot;

use super::session::SessionData;

const CAS_LOGIN_URL: &str = "https://login.ucsc.edu/cas/login";
const CAS_VALIDATE_URL: &str = "https://login.ucsc.edu/cas/serviceValidate";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes
const SESSION_DURATION: Duration = Duration::from_secs(8 * 3600); // 8 hours

#[derive(Deserialize)]
struct CallbackParams {
    ticket: String,
}

pub async fn perform_login() -> Result<SessionData> {
    // Bind to ephemeral port
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to bind callback server")?;
    let addr: SocketAddr = listener.local_addr()?;
    let port = addr.port();
    let service_url = format!("http://localhost:{}/callback", port);

    // Channel to receive the ticket
    let (tx, rx) = oneshot::channel::<String>();
    let tx = std::sync::Arc::new(tokio::sync::Mutex::new(Some(tx)));

    let tx_clone = tx.clone();
    let app = Router::new().route(
        "/callback",
        get(move |Query(params): Query<CallbackParams>| {
            let tx = tx_clone.clone();
            async move {
                if let Some(sender) = tx.lock().await.take() {
                    let _ = sender.send(params.ticket);
                }
                Html(
                    r#"<!DOCTYPE html>
<html>
<head><title>UCSC Login Successful</title></head>
<body style="font-family: system-ui; text-align: center; padding: 60px;">
    <h1>Login Successful!</h1>
    <p>You can close this tab and return to your terminal.</p>
    <script>setTimeout(() => window.close(), 3000);</script>
</body>
</html>"#,
                )
            }
        }),
    );

    // Start server in background
    let server_handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    // Open browser
    let cas_url = format!("{}?service={}", CAS_LOGIN_URL, urlencoding::encode(&service_url));
    tracing::info!("Opening browser for CAS login...");
    open::that(&cas_url).context("failed to open browser")?;

    // Wait for ticket
    let ticket = tokio::time::timeout(LOGIN_TIMEOUT, rx)
        .await
        .map_err(|_| anyhow::anyhow!("login timed out after 5 minutes"))?
        .map_err(|_| anyhow::anyhow!("callback channel closed unexpectedly"))?;

    // Shut down callback server
    server_handle.abort();

    // Validate ticket with CAS
    let username = validate_ticket(&ticket, &service_url).await?;

    let expires_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
        + SESSION_DURATION.as_secs() as i64;

    Ok(SessionData {
        cookies: String::new(), // Will be populated when making authenticated requests
        username,
        expires_at,
    })
}

async fn validate_ticket(ticket: &str, service_url: &str) -> Result<String> {
    let client = reqwest::Client::new();
    let resp = client
        .get(CAS_VALIDATE_URL)
        .query(&[("ticket", ticket), ("service", service_url)])
        .send()
        .await
        .context("failed to validate CAS ticket")?;

    let xml = resp.text().await.context("failed to read CAS response")?;

    // Parse the CAS XML response to extract username
    // Successful response contains <cas:user>username</cas:user>
    // Failed response contains <cas:authenticationFailure>
    if xml.contains("authenticationFailure") {
        bail!("CAS authentication failed. The ticket may have expired.");
    }

    extract_cas_user(&xml)
        .ok_or_else(|| anyhow::anyhow!("could not extract username from CAS response"))
}

fn extract_cas_user(xml: &str) -> Option<String> {
    // Look for <cas:user>...</cas:user> pattern
    let start_tag = "<cas:user>";
    let end_tag = "</cas:user>";

    let start = xml.find(start_tag)? + start_tag.len();
    let end = xml[start..].find(end_tag)? + start;

    let username = xml[start..end].trim().to_string();
    if username.is_empty() {
        None
    } else {
        Some(username)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_cas_user_success() {
        let xml = r#"<cas:serviceResponse xmlns:cas='http://www.yale.edu/tp/cas'>
            <cas:authenticationSuccess>
                <cas:user>jsmith</cas:user>
            </cas:authenticationSuccess>
        </cas:serviceResponse>"#;

        assert_eq!(extract_cas_user(xml), Some("jsmith".to_string()));
    }

    #[test]
    fn test_extract_cas_user_failure() {
        let xml = r#"<cas:serviceResponse xmlns:cas='http://www.yale.edu/tp/cas'>
            <cas:authenticationFailure code='INVALID_TICKET'>
                Ticket not recognized
            </cas:authenticationFailure>
        </cas:serviceResponse>"#;

        assert_eq!(extract_cas_user(xml), None);
    }
}
