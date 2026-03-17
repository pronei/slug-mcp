use std::sync::Arc;
use std::time::{Duration, Instant};

use moka::future::Cache;
use moka::Expiry;

/// Cached value with a per-entry TTL.
#[derive(Clone)]
struct CacheEntry {
    value: Arc<String>,
    ttl: Duration,
}

/// Expiry policy that uses each entry's stored TTL.
struct PerEntryExpiry;

impl Expiry<String, CacheEntry> for PerEntryExpiry {
    fn expire_after_create(
        &self,
        _key: &String,
        value: &CacheEntry,
        _created_at: Instant,
    ) -> Option<Duration> {
        Some(value.ttl)
    }
}

/// In-memory TTL cache backed by moka with per-entry expiration.
#[derive(Clone)]
pub struct CacheStore {
    inner: Cache<String, CacheEntry>,
}

impl CacheStore {
    /// Create a new in-memory cache with a max capacity.
    pub fn new(max_capacity: u64) -> Self {
        let inner = Cache::builder()
            .max_capacity(max_capacity)
            .expire_after(PerEntryExpiry)
            .build();
        Self { inner }
    }

    pub async fn get(&self, key: &str) -> Option<String> {
        self.inner.get(key).await.map(|e| (*e.value).clone())
    }

    pub async fn set(&self, key: &str, value: &str, ttl_secs: u64) {
        let entry = CacheEntry {
            value: Arc::new(value.to_string()),
            ttl: Duration::from_secs(ttl_secs),
        };
        self.inner.insert(key.to_string(), entry).await;
    }

    pub async fn invalidate(&self, key: &str) {
        self.inner.invalidate(key).await;
    }
}
