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

    /// Check cache for `key`, deserialize as `T`. On miss or deser failure,
    /// call `fetch`, serialize the result back into the cache, and return it.
    pub async fn get_or_fetch<T, F, Fut>(
        &self,
        key: &str,
        ttl_secs: u64,
        fetch: F,
    ) -> anyhow::Result<T>
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>>,
    {
        if let Some(cached) = self.get(key).await {
            match serde_json::from_str::<T>(&cached) {
                Ok(val) => return Ok(val),
                Err(e) => tracing::warn!("Cache deser failed for {}: {}", key, e),
            }
        }

        let val = fetch().await?;

        if let Ok(json) = serde_json::to_string(&val) {
            self.set(key, &json, ttl_secs).await;
        }

        Ok(val)
    }
}
