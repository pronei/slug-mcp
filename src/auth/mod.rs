pub mod cas;
pub mod session;

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;

use session::SessionData;

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
            "Not authenticated. Use the `login` tool to sign in with your UCSC credentials.".to_string()
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

    pub async fn login(&self) -> Result<String> {
        let session_data = cas::perform_login().await?;
        let username = session_data.username.clone();
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
