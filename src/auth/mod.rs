pub mod browser;
pub mod session;
pub mod token;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use reqwest::cookie::Jar;

use session::SessionData;

/// How long we consider a session valid.
const SESSION_DURATION: Duration = Duration::from_secs(8 * 3600); // 8 hours

pub struct AuthStatus {
    pub authenticated: bool,
    pub username: Option<String>,
    pub expires_in: Option<Duration>,
}

impl AuthStatus {
    pub fn format(&self) -> String {
        if self.authenticated {
            let expires = self
                .expires_in
                .map(|d| {
                    let hours = d.as_secs() / 3600;
                    let mins = (d.as_secs() % 3600) / 60;
                    format!("{}h {}m", hours, mins)
                })
                .unwrap_or_else(|| "unknown".to_string());

            format!(
                "Authenticated as **{}** (expires in {})",
                self.username.as_deref().unwrap_or("unknown"),
                expires
            )
        } else {
            "Not authenticated. Use the `login` tool to sign in with your UCSC credentials."
                .to_string()
        }
    }
}

pub struct AuthManager {
    session_path: PathBuf,
}

impl AuthManager {
    pub fn new(session_path: PathBuf) -> Self {
        Self { session_path }
    }

    /// Login by launching a Chrome window for UCSC Shibboleth SSO.
    ///
    /// Opens a browser via Chrome DevTools Protocol, navigates to the GET
    /// login page (which triggers SAML), waits for the user to complete
    /// CruzID + Duo authentication, then extracts all cookies (including
    /// session-only cookies) via CDP.
    pub async fn login(&self) -> Result<String> {
        let cookies = browser::login_via_browser().await?;

        let cookie_data =
            browser::serialize_cookies(&cookies).context("failed to serialize cookies")?;

        let expires_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
            + SESSION_DURATION.as_secs() as i64;

        let username =
            browser::extract_username(&cookies).unwrap_or_else(|| "UCSC User".to_string());

        let session_data = SessionData {
            cookies: cookie_data,
            username: username.clone(),
            expires_at,
        };

        session::save_session(&self.session_path, &session_data)?;
        tracing::info!("Logged in as {}", username);
        Ok(username)
    }

    pub fn check_auth(&self) -> Result<AuthStatus> {
        match session::load_session(&self.session_path)? {
            Some(data) => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                let remaining = (data.expires_at - now).max(0) as u64;

                Ok(AuthStatus {
                    authenticated: true,
                    username: Some(data.username),
                    expires_in: Some(Duration::from_secs(remaining)),
                })
            }
            None => Ok(AuthStatus {
                authenticated: false,
                username: None,
                expires_in: None,
            }),
        }
    }

    pub fn get_session(&self) -> Result<Option<SessionData>> {
        session::load_session(&self.session_path)
    }
}

/// Build a `reqwest::Client` pre-loaded with cookies for authenticated requests.
///
/// The client's cookie jar contains all cookies captured during the browser
/// login session. These include IdP cookies (login.ucsc.edu), SP cookies
/// (get.cbord.com), and any intermediate cookies from the SAML flow.
pub fn build_authenticated_client(cookie_data: &str) -> Result<reqwest::Client> {
    let cookies = browser::deserialize_cookies(cookie_data)?;
    let jar = Jar::default();

    for cookie in &cookies {
        // Add each cookie to its correct domain
        let scheme = if cookie.secure { "https" } else { "http" };
        let domain = cookie.domain.trim_start_matches('.');
        let url_str = format!("{}://{}{}", scheme, domain, cookie.path);
        if let Ok(url) = url_str.parse() {
            let cookie_str = format!(
                "{}={}; Domain={}; Path={}{}{}",
                cookie.name,
                cookie.value,
                cookie.domain,
                cookie.path,
                if cookie.secure { "; Secure" } else { "" },
                if cookie.http_only {
                    "; HttpOnly"
                } else {
                    ""
                },
            );
            jar.add_cookie_str(&cookie_str, &url);
        }
    }

    reqwest::Client::builder()
        .cookie_provider(Arc::new(jar))
        .redirect(reqwest::redirect::Policy::limited(20))
        .build()
        .context("failed to build authenticated HTTP client")
}

/// Make an authenticated GET request that follows SAML POST binding forms.
///
/// SAML POST binding uses an intermediate HTML form with hidden fields
/// (`SAMLResponse`, `RelayState`) that browsers auto-submit via JavaScript.
/// Since `reqwest` doesn't execute JS, we parse the form and POST manually.
pub async fn saml_aware_get(client: &reqwest::Client, url: &str) -> Result<SamlResponse> {
    let mut resp = client.get(url).send().await.context("request failed")?;

    // Follow up to 5 SAML POST binding forms
    for _ in 0..5 {
        let status = resp.status();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        if !content_type.contains("text/html") {
            return Ok(SamlResponse {
                status,
                body: resp.text().await.unwrap_or_default(),
            });
        }

        let body = resp.text().await.context("failed to read response body")?;

        if let Some((action_url, fields)) = parse_saml_form(&body) {
            tracing::debug!("Following SAML POST binding to {}", action_url);
            resp = client
                .post(&action_url)
                .form(&fields)
                .send()
                .await
                .context("SAML POST binding failed")?;
        } else {
            return Ok(SamlResponse { status, body });
        }
    }

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    Ok(SamlResponse { status, body })
}

/// Response from a SAML-aware request.
pub struct SamlResponse {
    pub status: reqwest::StatusCode,
    pub body: String,
}

/// Parse a SAML POST binding auto-submit form from HTML.
fn parse_saml_form(html: &str) -> Option<(String, Vec<(String, String)>)> {
    use std::sync::LazyLock;

    use scraper::{Html, Selector};

    static FORM_SEL: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("form").expect("hardcoded selector"));
    static INPUT_SEL: LazyLock<Selector> = LazyLock::new(|| {
        Selector::parse("input[type=\"hidden\"]").expect("hardcoded selector")
    });

    let document = Html::parse_document(html);

    for form in document.select(&FORM_SEL) {
        let mut fields = Vec::new();
        let mut has_saml_field = false;

        for input in form.select(&INPUT_SEL) {
            let name = input.value().attr("name").unwrap_or_default().to_string();
            let value = input.value().attr("value").unwrap_or_default().to_string();

            if name == "SAMLResponse" || name == "SAMLRequest" {
                has_saml_field = true;
            }

            if !name.is_empty() {
                fields.push((name, value));
            }
        }

        if has_saml_field {
            let action = form
                .value()
                .attr("action")
                .unwrap_or_default()
                .to_string();

            if !action.is_empty() {
                return Some((action, fields));
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_saml_form() {
        let html = r#"
        <html><body>
        <form method="POST" action="https://get.cbord.com/ucsc/Shibboleth.sso/SAML2/POST">
            <input type="hidden" name="SAMLResponse" value="PHNhbWw..." />
            <input type="hidden" name="RelayState" value="https://get.cbord.com/ucsc/full/login.php" />
            <noscript><button type="submit">Continue</button></noscript>
        </form>
        </body></html>"#;

        let result = parse_saml_form(html);
        assert!(result.is_some());

        let (action, fields) = result.unwrap();
        assert_eq!(
            action,
            "https://get.cbord.com/ucsc/Shibboleth.sso/SAML2/POST"
        );
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].0, "SAMLResponse");
        assert_eq!(fields[1].0, "RelayState");
    }

    #[test]
    fn test_parse_saml_form_no_saml() {
        let html = r#"<html><body><form action="/login"><input type="text" name="user" /></form></body></html>"#;
        assert!(parse_saml_form(html).is_none());
    }
}
