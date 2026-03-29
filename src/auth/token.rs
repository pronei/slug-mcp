use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;

use super::session::SessionData;

/// Encode a session into a portable base64 token for transfer to a remote server.
pub fn encode_token(data: &SessionData) -> Result<String> {
    let json = serde_json::to_vec(data).context("failed to serialize session")?;
    Ok(STANDARD.encode(&json))
}

/// Decode a base64 token back into session data.
///
/// Returns an error if the token is malformed or the session has expired.
pub fn decode_token(token: &str) -> Result<SessionData> {
    let bytes = STANDARD
        .decode(token.trim())
        .context("invalid token encoding — expected base64")?;

    let data: SessionData =
        serde_json::from_slice(&bytes).context("invalid token format — expected session JSON")?;

    if data.is_expired() {
        bail!("token has expired — run `slug-mcp export-token` again");
    }

    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn test_roundtrip() {
        let data = SessionData {
            cookies: r#"[{"name":"sid","value":"abc","domain":".ucsc.edu","path":"/","secure":true,"http_only":true}]"#.to_string(),
            username: "testslug".to_string(),
            expires_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64
                + 3600,
        };

        let token = encode_token(&data).unwrap();
        let decoded = decode_token(&token).unwrap();

        assert_eq!(decoded.username, "testslug");
        assert_eq!(decoded.cookies, data.cookies);
        assert!(!decoded.is_expired());
    }

    #[test]
    fn test_expired_token_rejected() {
        let data = SessionData {
            cookies: "[]".to_string(),
            username: "old".to_string(),
            expires_at: 0, // long expired
        };

        let token = encode_token(&data).unwrap();
        let result = decode_token(&token);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("expired"));
    }

    #[test]
    fn test_garbage_token_rejected() {
        assert!(decode_token("not-valid-base64!!!").is_err());
    }

    #[test]
    fn test_wrong_json_rejected() {
        let token = STANDARD.encode(b"{\"wrong\": true}");
        assert!(decode_token(&token).is_err());
    }
}
