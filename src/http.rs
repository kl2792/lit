/// HTTP client wrapper with caching support.
///
/// Wraps `reqwest::Client` and integrates with the SQLite-backed `Db`
/// for transparent request deduplication.

use crate::db::Db;
use std::sync::Arc;
use std::time::Duration;

/// Maximum response body size: 50 MB.
const MAX_RESPONSE_BYTES: usize = 50 * 1024 * 1024;

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
        let timeout_secs: u64 = std::env::var("CURL_TIMEOUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(15);

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
        if !self.no_cache {
            if let Some(cached) = self.db.cache_get(cache_key, ttl) {
                return Ok(cached);
            }
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
        if !self.no_cache {
            if let Some(cached) = self.db.cache_get(cache_key, ttl) {
                return Ok(cached);
            }
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
    /// Automatically adds `x-api-key` header for Semantic Scholar if `$S2_API_KEY` is set.
    /// Rejects responses larger than 50 MB.
    pub async fn get(&self, url: &str) -> Result<String, Box<dyn std::error::Error>> {
        let max_attempts = 60;
        for attempt in 0..max_attempts {
            let mut req = self.inner.get(url);
            if url.contains("semanticscholar.org") {
                if let Ok(key) = std::env::var("S2_API_KEY") {
                    req = req.header("x-api-key", key);
                }
            }
            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) if e.is_timeout() || e.is_connect() => {
                    if attempt < max_attempts - 1 {
                        eprintln!("note: {} (attempt {}/{}), retrying in 1s...",
                            if e.is_timeout() { "timeout" } else { "connection error" },
                            attempt + 1, max_attempts);
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                    return Err(e.into());
                }
                Err(e) => return Err(e.into()),
            };
            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                if attempt < max_attempts - 1 {
                    let wait = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(1)
                        .min(5); // cap wait at 5s per retry
                    eprintln!("note: rate limited (attempt {}/{}), retrying in {}s...",
                        attempt + 1, max_attempts, wait);
                    tokio::time::sleep(Duration::from_secs(wait)).await;
                    continue;
                }
                return Err(format!("HTTP 429 Too Many Requests for {} (retried {}x over ~60s)", url, max_attempts).into());
            }
            if resp.status().is_server_error() && attempt < max_attempts - 1 {
                eprintln!("note: HTTP {} (attempt {}/{}), retrying in 1s...",
                    resp.status(), attempt + 1, max_attempts);
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            if !resp.status().is_success() {
                return Err(format!("HTTP {} for {}", resp.status(), url).into());
            }
            // Check Content-Length header if present
            if let Some(len) = resp.content_length() {
                if len as usize > MAX_RESPONSE_BYTES {
                    return Err(format!(
                        "Response too large ({} bytes) for {}",
                        len, url
                    )
                    .into());
                }
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
