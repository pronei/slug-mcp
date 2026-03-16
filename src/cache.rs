use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use rusqlite::Connection;
use tokio::sync::Mutex;

pub struct CacheStore {
    conn: Mutex<Connection>,
}

impl CacheStore {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS cache_entries (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                expires_at INTEGER NOT NULL
            )",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub async fn get(&self, key: &str) -> Option<String> {
        let conn = self.conn.lock().await;
        let now = unix_now();
        conn.query_row(
            "SELECT value FROM cache_entries WHERE key = ?1 AND expires_at > ?2",
            rusqlite::params![key, now],
            |row| row.get(0),
        )
        .ok()
    }

    pub async fn set(&self, key: &str, value: &str, ttl_secs: u64) {
        let conn = self.conn.lock().await;
        let expires_at = unix_now() + ttl_secs as i64;
        let _ = conn.execute(
            "INSERT OR REPLACE INTO cache_entries (key, value, expires_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![key, value, expires_at],
        );
    }

    pub async fn invalidate(&self, key: &str) {
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "DELETE FROM cache_entries WHERE key = ?1",
            rusqlite::params![key],
        );
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
