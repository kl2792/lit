//! HTTP client wrapper with caching support.
//!
//! Wraps `reqwest::Client` and integrates with the SQLite-backed `Db`
//! for transparent request deduplication.

use crate::db::Db;
use std::sync::Arc;
use std::time::Duration;

/// Maximum response body size: 50 MB.
const MAX_RESPONSE_BYTES: usize = 50 * 1024 * 1024;

/// HTTP client with transparent SQLite-backed response caching.
pub struct Client {
    inner: reqwest::Client,
    db: Arc<Db>,
    no_cache: bool,
}

impl Client {
    /// Create a new HTTP client backed by the given database.
    ///
    /// When `no_cache` is true, all requests bypass the cache (no reads or writes).
    /// Timeout defaults to 15 seconds, overridden by `$CURL_TIMEOUT` env var.
    pub fn new(db: Arc<Db>, no_cache: bool) -> Self {
        let timeout_secs: u64 = crate::config::Config::get().timeout();

        let inner = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .user_agent("lit/1.0")
            .build()
            .expect("failed to build HTTP client");

        Client {
            inner,
            db,
            no_cache,
        }
    }

    /// Fetch a URL, checking the cache first.
    ///
    /// On cache miss (or when `no_cache` is set), performs an HTTP GET.
    /// When `no_cache` is false, stores the response body in the cache under
    /// `cache_key` and returns it. On cache hit (within `ttl` seconds),
    /// returns the cached value without making a request.
    pub async fn get_cached(
        &self,
        cache_key: &str,
        url: &str,
        ttl: u64,
    ) -> Result<String, Box<dyn std::error::Error>> {
        if !self.no_cache
            && let Some(cached) = self.db.cache_get(cache_key, ttl) {
                return Ok(cached);
            }

        let body = self.get(url).await?;
        if !self.no_cache && looks_valid(&body) {
            self.db.cache_set(cache_key, url, &body);
        }
        Ok(body)
    }

    /// Fetch a URL with cache read but deferred write.
    ///
    /// Returns the cached value if available. Otherwise fetches from the URL
    /// but does NOT write to cache. The caller should call `cache_set` after
    /// validating the response.
    pub async fn get_cached_deferred(
        &self,
        cache_key: &str,
        url: &str,
        ttl: u64,
    ) -> Result<String, Box<dyn std::error::Error>> {
        if !self.no_cache
            && let Some(cached) = self.db.cache_get(cache_key, ttl) {
                return Ok(cached);
            }
        self.get(url).await
    }

    /// Write a value to the cache (no-op when no_cache is set).
    pub fn cache_set(&self, cache_key: &str, url: &str, value: &str) {
        if !self.no_cache && !value.is_empty() {
            self.db.cache_set(cache_key, url, value);
        }
    }

    /// Perform an uncached HTTP GET request.
    ///
    /// Returns an error for non-success (non-2xx) HTTP status codes.
    /// For 429 (rate limited), includes the Retry-After hint if present.
    /// For Semantic Scholar: tries without API key first, cascades to API key on 429.
    /// Retries with exponential backoff (1s, 2s, 4s, ..., capped at 30s).
    /// Rejects responses larger than 50 MB.
    pub async fn get(&self, url: &str) -> Result<String, Box<dyn std::error::Error>> {
        let max_attempts = 10;
        let is_s2 = url.contains("semanticscholar.org");
        let s2_key = if is_s2 { crate::config::Config::get().s2_api_key().map(String::from) } else { None };
        let mut use_s2_key = false; // start without API key, cascade on 429
        let mut s2_key_failed = false; // true if API key returned 403, don't retry it
        for attempt in 0..max_attempts {
            let mut req = self.inner.get(url);
            if is_s2 && use_s2_key
                && let Some(ref key) = s2_key {
                    req = req.header("x-api-key", key);
                }
            let backoff = Duration::from_secs((1u64 << attempt).min(30));
            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) if e.is_timeout() || e.is_connect() => {
                    if attempt < max_attempts - 1 {
                        tokio::time::sleep(backoff).await;
                        continue;
                    }
                    return Err(e.into());
                }
                Err(e) => return Err(e.into()),
            };
            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                // Cascade to API key on first 429 for S2
                if is_s2 && !use_s2_key && s2_key.is_some() && !s2_key_failed {
                    use_s2_key = true;
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
                if attempt < max_attempts - 1 {
                    let wait = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .map(|v| v.min(30))
                        .unwrap_or(backoff.as_secs());
                    tokio::time::sleep(Duration::from_secs(wait)).await;
                    continue;
                }
                return Err(format!("HTTP 429 Too Many Requests for {} (retried {}x)", url, max_attempts).into());
            }
            // If S2 returns 403 with the API key, the key is likely expired/revoked.
            // Fall back to unauthenticated requests with normal rate-limit retries.
            if is_s2 && use_s2_key && resp.status() == reqwest::StatusCode::FORBIDDEN {
                use_s2_key = false;
                s2_key_failed = true;
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            if resp.status().is_server_error() && attempt < max_attempts - 1 {
                tokio::time::sleep(backoff).await;
                continue;
            }
            if !resp.status().is_success() {
                return Err(format!("HTTP {} for {}", resp.status(), url).into());
            }
            // Check Content-Length header if present
            if let Some(len) = resp.content_length()
                && len as usize > MAX_RESPONSE_BYTES {
                    return Err(format!(
                        "Response too large ({} bytes) for {}",
                        len, url
                    )
                    .into());
                }
            let body = resp.text().await?;
            if body.len() > MAX_RESPONSE_BYTES {
                return Err(format!(
                    "Response too large ({} bytes) for {}",
                    body.len(),
                    url
                )
                .into());
            }
            return Ok(body);
        }
        unreachable!()
    }
}

/// Check if a response body looks like valid content worth caching.
///
/// Rejects empty bodies and bodies that don't start with a JSON or XML marker.
fn looks_valid(body: &str) -> bool {
    let trimmed = body.trim_start();
    !trimmed.is_empty()
        && (trimmed.starts_with('{')
            || trimmed.starts_with('[')
            || trimmed.starts_with('<'))
}
