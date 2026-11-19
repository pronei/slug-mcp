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

    // moka's default keeps the first write's deadline, so an overwrite of a
    // live key would still expire on the old schedule.
    fn expire_after_update(
        &self,
        _key: &String,
        value: &CacheEntry,
        _updated_at: Instant,
        _duration_until_expiry: Option<Duration>,
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
    pub async fn set_arc<T: Send + Sync + 'static>(&self, key: &str, value: Arc<T>, ttl: Duration) {
        let entry = CacheEntry {
            value: value as Arc<dyn Any + Send + Sync>,
            ttl,
        };
        self.inner.insert(key.to_string(), entry).await;
    }

    /// Evict a single key. Part of the cache API; no caller at present.
    #[allow(dead_code)]
    pub async fn invalidate(&self, key: &str) {
        self.inner.invalidate(key).await;
    }

    /// Cache-aside fetch with single-flight: return cached `T` (cloned), or
    /// invoke `fetch` and cache the result before returning it. Concurrent
    /// misses for the same key are coalesced — only one `fetch` runs and the
    /// rest await its result (moka's `or_try_insert_with`). Type mismatches in
    /// the cache are treated as misses.
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
        // Fast path: a type-correct hit avoids the entry machinery entirely.
        if let Some(cached) = self.get::<T>(key).await {
            return Ok(cached);
        }
        // A wrong-typed entry squatting the key would make the coalesced
        // insert below return it (and skip the fetch). All callers of a given
        // key use the same `T`, so this only happens across a `T`-for-key code
        // change; drop the stale entry so the fetch runs and re-types it.
        if self.inner.contains_key(key) {
            self.inner.invalidate(key).await;
        }

        let ttl = Duration::from_secs(ttl_secs);
        let entry = self
            .inner
            .entry(key.to_string())
            .or_try_insert_with(async move {
                match fetch().await {
                    Ok(value) => Ok(CacheEntry {
                        value: Arc::new(value) as Arc<dyn Any + Send + Sync>,
                        ttl,
                    }),
                    // Callers show this text to clients, and some adapters put
                    // API keys in the query string that reqwest errors print.
                    Err(e) => {
                        let chain = redact_url_queries(&format!("{e:#}"));
                        tracing::warn!(key, "cache fetch failed: {chain}");
                        Err(chain)
                    }
                }
            })
            .await
            .map_err(|chain: Arc<String>| anyhow::anyhow!("cache fetch failed: {chain}"))?;

        match Arc::downcast::<T>(entry.into_value().value) {
            Ok(arc) => Ok((*arc).clone()),
            // We invalidated any wrong-typed entry above and the coalesced
            // fetch inserts a `T`, so this is unreachable in practice. Surface
            // an error rather than panic if the invariant ever breaks.
            Err(_) => Err(anyhow::anyhow!(
                "cache type mismatch for key '{key}' after fetch"
            )),
        }
    }
}

// Hosts and paths stay so the error still says which request failed.
fn redact_url_queries(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = ["http://", "https://"]
        .iter()
        .filter_map(|scheme| rest.find(scheme))
        .min()
    {
        let (before, from_url) = rest.split_at(start);
        out.push_str(before);
        let end = from_url
            .find(|c: char| c.is_whitespace() || matches!(c, ')' | '"' | '\'' | '<' | '>'))
            .unwrap_or(from_url.len());
        // Sentence punctuation after a URL (`…?a=b: cause`) isn't part of it.
        let url = from_url[..end].trim_end_matches([':', ',', ';', '.']);
        match url.find('?') {
            Some(q) => {
                out.push_str(&url[..q]);
                out.push_str("?<redacted>");
            }
            None => out.push_str(url),
        }
        out.push_str(&from_url[url.len()..end]);
        rest = &from_url[end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fetch_error(err: anyhow::Error) -> String {
        let cache = CacheStore::new(16);
        cache
            .get_or_fetch::<u32, _, _>("k", 60, || async { Err(err) })
            .await
            .unwrap_err()
            .to_string()
    }

    #[tokio::test]
    async fn fetch_error_keeps_the_whole_cause_chain() {
        let msg = fetch_error(anyhow::anyhow!("inner cause").context("outer")).await;
        assert!(msg.starts_with("cache fetch failed: "), "{msg}");
        assert!(msg.contains("outer"), "{msg}");
        assert!(msg.contains("inner cause"), "{msg}");
    }

    #[tokio::test]
    async fn fetch_error_hides_url_query_strings() {
        let err = anyhow::anyhow!(
            "error sending request for url (https://example.test/v1/parks?parkCode=pinn&api_key=SECRET123)"
        )
        .context("NPS API HTTP request failed");
        let msg = fetch_error(err).await;
        assert!(!msg.contains("SECRET123"), "{msg}");
        assert!(msg.contains("https://example.test/v1/parks"), "{msg}");
        assert!(msg.contains("NPS API HTTP request failed"), "{msg}");
    }

    #[test]
    fn redact_url_queries_table() {
        let cases = [
            ("no url here", "no url here"),
            (
                "see https://example.test/a/b for details",
                "see https://example.test/a/b for details",
            ),
            (
                "ends with https://example.test/a?key=K1",
                "ends with https://example.test/a?<redacted>",
            ),
            (
                "error sending request for url (https://example.test/a?a=b&key=K1)",
                "error sending request for url (https://example.test/a?<redacted>)",
            ),
            (
                "fetch https://example.test/a?key=K1: connection refused",
                "fetch https://example.test/a?<redacted>: connection refused",
            ),
            (
                "first http://one.test/x?key=K1, then https://two.test/y?token=K2 and https://three.test/z",
                "first http://one.test/x?<redacted>, then https://two.test/y?<redacted> and https://three.test/z",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(redact_url_queries(input), want, "input: {input}");
        }
    }

    #[tokio::test]
    async fn overwrite_with_longer_ttl_extends_expiry() {
        let cache = CacheStore::new(16);
        cache.set("k", 1u32, Duration::from_millis(400)).await;
        cache.set("k", 2u32, Duration::from_secs(30)).await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(cache.get::<u32>("k").await, Some(2));
    }

    #[tokio::test]
    async fn overwrite_with_shorter_ttl_shortens_expiry() {
        let cache = CacheStore::new(16);
        cache.set("k", 1u32, Duration::from_secs(30)).await;
        cache.set("k", 2u32, Duration::from_millis(200)).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(cache.get::<u32>("k").await, None);
    }
}
