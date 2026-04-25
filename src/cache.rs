use std::any::Any;
use std::sync::Arc;
use std::time::{Duration, Instant};

use moka::Expiry;
use moka::future::Cache;

/// Cached value with a per-entry TTL. The value is type-erased so a single
/// `CacheStore` can hold any `Send + Sync + 'static` type — callers `get<T>`
/// downcasts back to the concrete type. No JSON round-trip.
#[derive(Clone)]
struct CacheEntry {
    value: Arc<dyn Any + Send + Sync>,
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

/// In-memory TTL cache with per-entry expiration. Backed by moka with
/// type-erased `Arc<dyn Any>` storage so callers can cache any concrete type
/// without serialization overhead.
#[derive(Clone)]
pub struct CacheStore {
    inner: Cache<String, CacheEntry>,
}

impl CacheStore {
    pub fn new(max_capacity: u64) -> Self {
        let inner = Cache::builder()
            .max_capacity(max_capacity)
            .expire_after(PerEntryExpiry)
            .build();
        Self { inner }
    }

    /// Get a cloned value for `key` if present and the stored type matches `T`.
    /// A type mismatch is treated as a miss; pair this with `get_or_fetch` and
    /// the next call will refetch with the correct type.
    pub async fn get<T: Clone + Send + Sync + 'static>(&self, key: &str) -> Option<T> {
        let arc = self.get_arc::<T>(key).await?;
        Some((*arc).clone())
    }

    /// Get the `Arc` directly — avoids one clone for read-only access of large
    /// cached values. Most callers should use `get` instead.
    pub async fn get_arc<T: Send + Sync + 'static>(&self, key: &str) -> Option<Arc<T>> {
        let entry = self.inner.get(key).await?;
        Arc::downcast::<T>(entry.value).ok()
    }

    /// Insert `value` under `key` with the given TTL.
    pub async fn set<T: Send + Sync + 'static>(&self, key: &str, value: T, ttl: Duration) {
        self.set_arc(key, Arc::new(value), ttl).await;
    }

    /// Insert an already-shared `Arc<T>` — useful when the caller wants to
    /// retain its own reference without an extra clone of `T`.
    pub async fn set_arc<T: Send + Sync + 'static>(
        &self,
        key: &str,
        value: Arc<T>,
        ttl: Duration,
    ) {
        let entry = CacheEntry {
            value: value as Arc<dyn Any + Send + Sync>,
            ttl,
        };
        self.inner.insert(key.to_string(), entry).await;
    }

    pub async fn invalidate(&self, key: &str) {
        self.inner.invalidate(key).await;
    }

    /// Cache-aside fetch: return cached `T` (cloned), or invoke `fetch` and
    /// cache the result before returning it. Type mismatches in the cache are
    /// treated as misses.
    pub async fn get_or_fetch<T, F, Fut>(
        &self,
        key: &str,
        ttl_secs: u64,
        fetch: F,
    ) -> anyhow::Result<T>
    where
        T: Clone + Send + Sync + 'static,
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>>,
    {
        if let Some(cached) = self.get::<T>(key).await {
            return Ok(cached);
        }
        let value = fetch().await?;
        let arc = Arc::new(value);
        self.set_arc(key, Arc::clone(&arc), Duration::from_secs(ttl_secs))
            .await;
        Ok((*arc).clone())
    }
}
