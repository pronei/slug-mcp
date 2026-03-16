use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use aes_gcm::aead::{Aead, OsRng};
use aes_gcm::{AeadCore, Aes256Gcm, Key, KeyInit};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionData {
    pub cookies: String,
    pub username: String,
    pub expires_at: i64,
}

impl SessionData {
    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        now >= self.expires_at
    }
}

pub fn save_session(path: &Path, data: &SessionData) -> Result<()> {
    let json = serde_json::to_vec(data)?;
    let key = derive_key();
    let cipher = Aes256Gcm::new(&key);
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);

    let ciphertext = cipher
        .encrypt(&nonce, json.as_slice())
        .map_err(|e| anyhow::anyhow!("encryption failed: {}", e))?;

    // Prepend nonce (12 bytes) to ciphertext
    let mut output = Vec::with_capacity(12 + ciphertext.len());
    output.extend_from_slice(&nonce);
    output.extend_from_slice(&ciphertext);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, output).context("failed to write session file")?;

    Ok(())
}

pub fn load_session(path: &Path) -> Result<Option<SessionData>> {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    if data.len() < 12 {
        return Ok(None);
    }

    let (nonce_bytes, ciphertext) = data.split_at(12);
    let nonce = aes_gcm::Nonce::from_slice(nonce_bytes);
    let key = derive_key();
    let cipher = Aes256Gcm::new(&key);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| anyhow::anyhow!("session decryption failed - file may be corrupted"))?;

    let session: SessionData = serde_json::from_slice(&plaintext)?;

    if session.is_expired() {
        let _ = std::fs::remove_file(path);
        return Ok(None);
    }

    Ok(Some(session))
}

pub fn clear_session(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn derive_key() -> Key<Aes256Gcm> {
    let hostname = hostname::get()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let salt = b"slug-mcp-session-v1";

    let mut hasher = Sha256::new();
    hasher.update(hostname.as_bytes());
    hasher.update(salt);
    let hash = hasher.finalize();

    *Key::<Aes256Gcm>::from_slice(&hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_session_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test-session.enc");

        let data = SessionData {
            cookies: "session_id=abc123".to_string(),
            username: "testuser".to_string(),
            expires_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64
                + 3600,
        };

        save_session(&path, &data).unwrap();
        let loaded = load_session(&path).unwrap().unwrap();

        assert_eq!(loaded.username, "testuser");
        assert_eq!(loaded.cookies, "session_id=abc123");
        assert!(!loaded.is_expired());
    }

    #[test]
    fn test_expired_session_returns_none() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test-session.enc");

        let data = SessionData {
            cookies: "old".to_string(),
            username: "old_user".to_string(),
            expires_at: 0, // expired
        };

        save_session(&path, &data).unwrap();
        let loaded = load_session(&path).unwrap();

        assert!(loaded.is_none());
    }

    #[test]
    fn test_missing_session_returns_none() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.enc");
        let loaded = load_session(&path).unwrap();
        assert!(loaded.is_none());
    }
}
