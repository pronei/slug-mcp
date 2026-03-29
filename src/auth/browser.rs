use std::time::Duration;

use anyhow::{bail, Context, Result};
use chromiumoxide::browser::{Browser, BrowserConfig};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

/// URL that triggers the SAML login flow via GET's SP.
const LOGIN_ENTRY_URL: &str = "https://get.cbord.com/ucsc/full/login.php";

/// URL pattern indicating successful authentication (back on GET site).
const AUTH_SUCCESS_PATTERN: &str = "get.cbord.com/ucsc/full/";

/// How long to wait for the user to complete authentication.
const AUTH_TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes

/// How often to check if auth completed.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Simplified cookie for serialization/storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
}

/// Launch a visible Chrome window, navigate to UCSC login, wait for auth,
/// then extract all cookies via Chrome DevTools Protocol.
///
/// Returns all cookies from the browser session (including session-only
/// cookies that aren't written to disk).
pub async fn login_via_browser() -> Result<Vec<StoredCookie>> {
    // Launch Chrome with UI (not headless) so user can interact
    let config = BrowserConfig::builder()
        .with_head()
        .build()
        .map_err(|e| anyhow::anyhow!("failed to configure browser: {}", e))?;

    let (browser, mut handler) = Browser::launch(config)
        .await
        .context("failed to launch Chrome. Is Chrome/Chromium installed?")?;

    // Spawn the CDP event handler
    let handler_task = tokio::spawn(async move {
        while let Some(_event) = handler.next().await {}
    });

    let result = do_login_flow(&browser).await;

    // Clean up
    drop(browser);
    handler_task.abort();

    result
}

async fn do_login_flow(browser: &Browser) -> Result<Vec<StoredCookie>> {
    let page = browser
        .new_page(LOGIN_ENTRY_URL)
        .await
        .context("failed to open login page")?;

    tracing::info!("Browser opened for UCSC login. Waiting for authentication...");

    // Poll until the URL indicates successful auth (back on GET site)
    let deadline = tokio::time::Instant::now() + AUTH_TIMEOUT;

    loop {
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "Login timed out after {} seconds. Please try again.",
                AUTH_TIMEOUT.as_secs()
            );
        }

        tokio::time::sleep(POLL_INTERVAL).await;

        // Check current URL
        let current_url = match page.url().await {
            Ok(Some(url)) => url.to_string(),
            _ => continue,
        };

        tracing::debug!("Current URL: {}", current_url);

        // Check if we're back on the GET site (auth completed)
        if current_url.contains(AUTH_SUCCESS_PATTERN) && !current_url.contains("login.php") {
            tracing::info!("Authentication completed! Extracting cookies...");

            // Get ALL cookies from the browser (includes session cookies)
            let cdp_cookies = page
                .get_cookies()
                .await
                .context("failed to extract cookies from browser")?;

            let cookies: Vec<StoredCookie> = cdp_cookies
                .into_iter()
                .map(|c| StoredCookie {
                    name: c.name,
                    value: c.value,
                    domain: c.domain,
                    path: c.path,
                    secure: c.secure,
                    http_only: c.http_only,
                })
                .collect();

            tracing::info!("Captured {} cookies from browser session", cookies.len());
            return Ok(cookies);
        }
    }
}

/// Serialize cookies to a storable string format (JSON array).
pub fn serialize_cookies(cookies: &[StoredCookie]) -> Result<String> {
    serde_json::to_string(cookies).context("failed to serialize cookies")
}

/// Deserialize cookies from stored string format.
pub fn deserialize_cookies(data: &str) -> Result<Vec<StoredCookie>> {
    serde_json::from_str(data).context("failed to deserialize cookies")
}

/// Extract a username if any cookie contains one (best-effort).
pub fn extract_username(cookies: &[StoredCookie]) -> Option<String> {
    for cookie in cookies {
        if cookie.name.contains("username") && !cookie.value.is_empty() {
            return Some(cookie.value.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_deserialize_cookies() {
        let cookies = vec![StoredCookie {
            name: "JSESSIONID".to_string(),
            value: "abc123".to_string(),
            domain: "login.ucsc.edu".to_string(),
            path: "/".to_string(),
            secure: true,
            http_only: true,
        }];

        let serialized = serialize_cookies(&cookies).unwrap();
        let deserialized = deserialize_cookies(&serialized).unwrap();

        assert_eq!(deserialized.len(), 1);
        assert_eq!(deserialized[0].name, "JSESSIONID");
        assert_eq!(deserialized[0].value, "abc123");
    }

    #[test]
    fn test_extract_username_not_found() {
        let cookies = vec![StoredCookie {
            name: "JSESSIONID".to_string(),
            value: "abc".to_string(),
            domain: "example.com".to_string(),
            path: "/".to_string(),
            secure: false,
            http_only: false,
        }];
        assert!(extract_username(&cookies).is_none());
    }
}
