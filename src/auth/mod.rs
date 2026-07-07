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
                if cookie.http_only { "; HttpOnly" } else { "" },
            );
            jar.add_cookie_str(&cookie_str, &url);
        }
    }

    reqwest::Client::builder()
        .cookie_provider(Arc::new(jar))
        .redirect(reqwest::redirect::Policy::limited(20))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .context("failed to build authenticated HTTP client")
}

/// Make an authenticated GET request that follows SAML POST binding forms
/// and Shibboleth attribute-release consent pages.
///
/// SAML POST binding uses an intermediate HTML form with hidden fields
/// (`SAMLResponse`, `RelayState`) that browsers auto-submit via JavaScript.
/// Since `reqwest` doesn't execute JS, we parse the form and POST manually.
///
/// The UCSC IdP may also interpose an "Information Release" consent page the
/// first time a service provider asks for attributes. That form has no SAML
/// fields — just `_shib_idp_consentOptions` radios and an `_eventId_proceed`
/// submit. We accept it with "remember consent" so subsequent flows (LibCal,
/// cbord, …) skip the page for the rest of the IdP session.
pub async fn saml_aware_get(client: &reqwest::Client, url: &str) -> Result<SamlResponse> {
    let resp = client.get(url).send().await.context("request failed")?;
    let start = SamlResponse {
        status: resp.status(),
        final_url: resp.url().clone(),
        body: resp.text().await.context("failed to read response body")?,
    };
    saml_continue(client, start).await
}

/// Continue a SAML/consent chain from an already-received response.
///
/// Useful when an AJAX endpoint gets intercepted by SSO mid-flight (reqwest
/// follows the redirects, so the caller ends up holding the IdP page body
/// instead of the JSON it asked for). Pass that response here; it auto-submits
/// SAML POST binding forms and consent pages until it lands on a page that is
/// neither — typically back on the service provider, authenticated.
pub async fn saml_continue(
    client: &reqwest::Client,
    mut current: SamlResponse,
) -> Result<SamlResponse> {
    // Follow up to 8 intermediate hops (SAML POST forms + consent pages)
    for _ in 0..8 {
        let next_post = parse_saml_form(&current.body)
            .map(|(action, fields)| ("SAML POST binding", action, fields))
            .or_else(|| {
                parse_consent_form(&current.body)
                    .map(|(action, fields)| ("IdP consent accept", action, fields))
            });

        let Some((kind, action_url, fields)) = next_post else {
            return Ok(current);
        };

        // Form actions are usually absolute for SAML, but the consent
        // form's is relative — resolve against the page that served it.
        let action_abs = current
            .final_url
            .join(&action_url)
            .context("invalid intermediate form action URL")?;
        tracing::debug!("Following {kind} to {action_abs}");
        let resp = client
            .post(action_abs)
            .form(&fields)
            .send()
            .await
            .with_context(|| format!("{kind} POST failed"))?;
        current = SamlResponse {
            status: resp.status(),
            final_url: resp.url().clone(),
            body: resp.text().await.context("failed to read response body")?,
        };
    }

    Ok(current)
}

/// Response from a SAML-aware request.
pub struct SamlResponse {
    pub status: reqwest::StatusCode,
    /// URL of the final response in the redirect/SAML chain — tells the
    /// caller where it actually landed (e.g. back on the SP vs. stuck on
    /// the IdP login page).
    pub final_url: reqwest::Url,
    pub body: String,
}

/// Parse a SAML POST binding auto-submit form from HTML.
fn parse_saml_form(html: &str) -> Option<(String, Vec<(String, String)>)> {
    use std::sync::LazyLock;

    use scraper::{Html, Selector};

    static FORM_SEL: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("form").expect("hardcoded selector"));
    static INPUT_SEL: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("input[type=\"hidden\"]").expect("hardcoded selector"));

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
            let action = form.value().attr("action").unwrap_or_default().to_string();

            if !action.is_empty() {
                return Some((action, fields));
            }
        }
    }

    None
}

/// Parse a Shibboleth IdP attribute-release consent form.
///
/// Recognized by a form action under `/idp/profile/` containing an
/// `_eventId_proceed` submit input. Returns the form action and the fields
/// for an "Accept" submission: all hidden inputs (e.g. `csrf_token` on newer
/// IdPs), every pre-checked radio/checkbox — except we force the consent
/// option to `_shib_idp_rememberConsent` when that radio group is present —
/// and the `_eventId_proceed` submit itself.
fn parse_consent_form(html: &str) -> Option<(String, Vec<(String, String)>)> {
    use std::sync::LazyLock;

    use scraper::{Html, Selector};

    static FORM_SEL: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("form").expect("hardcoded selector"));
    static INPUT_SEL: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("input").expect("hardcoded selector"));

    let document = Html::parse_document(html);

    for form in document.select(&FORM_SEL) {
        let action = form.value().attr("action").unwrap_or_default();
        if !action.contains("/idp/profile/") {
            continue;
        }

        let mut fields: Vec<(String, String)> = Vec::new();
        let mut has_proceed = false;
        let mut consent_radio_seen = false;

        for input in form.select(&INPUT_SEL) {
            let v = input.value();
            let name = v.attr("name").unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let value = v.attr("value").unwrap_or_default();
            match v.attr("type").unwrap_or("text") {
                "hidden" => fields.push((name.to_string(), value.to_string())),
                "radio" if name == "_shib_idp_consentOptions" => {
                    // Pick "remember consent" regardless of the page default so
                    // the IdP stores the decision and later SP flows skip the page.
                    if !consent_radio_seen && value == "_shib_idp_rememberConsent" {
                        consent_radio_seen = true;
                        fields.push((name.to_string(), value.to_string()));
                    }
                }
                "radio" | "checkbox" => {
                    if v.attr("checked").is_some() {
                        fields.push((name.to_string(), value.to_string()));
                    }
                }
                "submit" if name == "_eventId_proceed" => {
                    has_proceed = true;
                    fields.push((name.to_string(), value.to_string()));
                }
                _ => {}
            }
        }

        if has_proceed {
            return Some((action.to_string(), fields));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDP_CONSENT_FIXTURE: &str = include_str!("fixtures/idp_consent.html");

    #[test]
    fn test_parse_consent_form_fixture() {
        let (action, fields) = parse_consent_form(IDP_CONSENT_FIXTURE).expect("consent form");
        assert_eq!(action, "/idp/profile/SAML2/Redirect/SSO?execution=e4s1");
        assert!(
            fields
                .iter()
                .any(|(n, v)| n == "_shib_idp_consentOptions" && v == "_shib_idp_rememberConsent")
        );
        assert!(fields.iter().any(|(n, _)| n == "_eventId_proceed"));
        // Reject button must NOT be submitted
        assert!(
            !fields
                .iter()
                .any(|(n, _)| n == "_eventId_AttributeReleaseRejected")
        );
        // Exactly one consent option
        assert_eq!(
            fields
                .iter()
                .filter(|(n, _)| n == "_shib_idp_consentOptions")
                .count(),
            1
        );
    }

    #[test]
    fn test_parse_consent_form_ignores_saml_and_plain_forms() {
        // A SAML POST binding form must not be misidentified as consent.
        let saml = r#"<form action="https://get.cbord.com/ucsc/Shibboleth.sso/SAML2/POST">
            <input type="hidden" name="SAMLResponse" value="x"/></form>"#;
        assert!(parse_consent_form(saml).is_none());
        let plain = r#"<form action="/login"><input type="text" name="user"/></form>"#;
        assert!(parse_consent_form(plain).is_none());
    }

    #[test]
    fn test_consent_form_round_trips_hidden_csrf() {
        // Newer Shibboleth IdPs add a csrf_token hidden input — must round-trip.
        let html = r#"<form action="/idp/profile/SAML2/Redirect/SSO?execution=e2s2" method="post">
            <input type="hidden" name="csrf_token" value="tok123"/>
            <input type="radio" name="_shib_idp_consentOptions" value="_shib_idp_rememberConsent" checked/>
            <input type="submit" name="_eventId_proceed" value="Accept"/>
        </form>"#;
        let (_, fields) = parse_consent_form(html).unwrap();
        assert!(
            fields
                .iter()
                .any(|(n, v)| n == "csrf_token" && v == "tok123")
        );
    }

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
