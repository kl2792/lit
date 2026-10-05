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
            .use_rustls_tls()
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
    /// A 429 that outlasts the retries reports the attempt count (and a missing
    /// `$S2_API_KEY` for Semantic Scholar URLs).
    /// Automatically adds `x-api-key` header for Semantic Scholar if `$S2_API_KEY` is set.
    /// Rejects responses larger than 50 MB.
    ///
    /// Timeouts, connection errors, 429 and 5xx are retried under `with_retries`.
    pub async fn get(&self, url: &str) -> Result<String, Box<dyn std::error::Error>> {
        let max_attempts = std::env::var("LIT_MAX_ATTEMPTS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(DEFAULT_MAX_ATTEMPTS);
        let outcome = with_retries(
            max_attempts,
            RETRY_BUDGET,
            || self.attempt(url),
            tokio::time::sleep,
        )
        .await;
        match outcome {
            Ok(result) => result,
            Err((Transient::RateLimited, attempts)) => {
                let has_key = std::env::var("S2_API_KEY").is_ok_and(|k| !k.is_empty());
                Err(rate_limit_message(url, attempts, has_key).into())
            }
            Err((Transient::Status(status), _)) => Err(format!("HTTP {} for {}", status, url).into()),
            Err((Transient::Transport(e), _)) => Err(e.into()),
        }
    }

    /// One GET: a final result, or a transient failure with the server's
    /// Retry-After hint when it sent one.
    async fn attempt(&self, url: &str) -> Attempt<Result<String, Box<dyn std::error::Error>>, Transient> {
        let mut req = self.inner.get(url);
        if let Some(key) = s2_api_key(url, std::env::var("S2_API_KEY").ok()) {
            req = req.header("x-api-key", key);
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) if e.is_timeout() || e.is_connect() => {
                return Attempt::Retry { err: Transient::Transport(e), retry_after: None };
            }
            Err(e) => return Attempt::Done(Err(e.into())),
        };
        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_retry_after);
            let err = if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                Transient::RateLimited
            } else {
                Transient::Status(status)
            };
            return Attempt::Retry { err, retry_after };
        }
        if !status.is_success() {
            return Attempt::Done(Err(format!("HTTP {} for {}", status, url).into()));
        }
        Attempt::Done(read_body(resp, url).await)
    }
}

/// Read a success response's body, rejecting anything over `MAX_RESPONSE_BYTES`.
async fn read_body(resp: reqwest::Response, url: &str) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(len) = resp.content_length() {
        if len as usize > MAX_RESPONSE_BYTES {
            return Err(format!("Response too large ({} bytes) for {}", len, url).into());
        }
    }
    let body = resp.text().await?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(format!("Response too large ({} bytes) for {}", body.len(), url).into());
    }
    Ok(body)
}

/// Attempts per request unless `$LIT_MAX_ATTEMPTS` overrides it.
const DEFAULT_MAX_ATTEMPTS: usize = 5;

/// Longest total sleep across one request's retries. A provider that stays
/// unavailable longer is treated as down, so callers can fall back elsewhere.
const RETRY_BUDGET: Duration = Duration::from_secs(60);

/// A failure worth retrying.
enum Transient {
    RateLimited,
    Status(reqwest::StatusCode),
    Transport(reqwest::Error),
}

impl std::fmt::Display for Transient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Transient::RateLimited => write!(f, "rate limited"),
            Transient::Status(s) => write!(f, "HTTP {}", s),
            Transient::Transport(e) if e.is_timeout() => write!(f, "timeout"),
            Transient::Transport(_) => write!(f, "connection error"),
        }
    }
}

/// What one attempt produced: a final value, or a transient failure `err`
/// with the server's requested delay, if any.
#[derive(Debug, PartialEq)]
enum Attempt<T, E> {
    Done(T),
    Retry { err: E, retry_after: Option<Duration> },
}

/// Run `attempt` up to `max_attempts` times, sleeping between transient failures.
///
/// The wait before retry `k` (0-based) is the server's Retry-After when given,
/// otherwise `2^(k+1)` seconds plus up to 50% jitter (2s, 4s, 8s, 16s, ...).
/// Gives up early rather than let total sleep exceed `budget`.
/// Returns the first `Done` value, or the last transient error and the number
/// of attempts made.
async fn with_retries<T, E, A, AFut, S, SFut>(
    max_attempts: usize,
    budget: Duration,
    mut attempt: A,
    mut sleep: S,
) -> Result<T, (E, usize)>
where
    E: std::fmt::Display,
    A: FnMut() -> AFut,
    AFut: std::future::Future<Output = Attempt<T, E>>,
    S: FnMut(Duration) -> SFut,
    SFut: std::future::Future<Output = ()>,
{
    let mut slept = Duration::ZERO;
    let mut made = 0;
    loop {
        made += 1;
        let (err, retry_after) = match attempt().await {
            Attempt::Done(value) => return Ok(value),
            Attempt::Retry { err, retry_after } => (err, retry_after),
        };
        let wait = retry_after.unwrap_or_else(|| backoff(made - 1, jitter_fraction()));
        if made >= max_attempts || slept + wait > budget {
            return Err((err, made));
        }
        eprintln!(
            "note: {} (attempt {}/{}), retrying in {:.1}s...",
            err, made, max_attempts, wait.as_secs_f64()
        );
        sleep(wait).await;
        slept += wait;
    }
}

/// Exponential backoff for retry `k`: `2^(k+1)` seconds scaled by `1 + jitter/2`,
/// `jitter` in `[0, 1)`. The exponent is capped so the shift cannot overflow.
fn backoff(k: usize, jitter: f64) -> Duration {
    Duration::from_secs(2u64 << k.min(16)).mul_f64(1.0 + jitter / 2.0)
}

/// A jitter fraction in `[0, 1)` from the clock's sub-second nanoseconds,
/// enough to de-synchronize concurrent retries without a RNG dependency.
fn jitter_fraction() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    f64::from(nanos % 1_000_000) / 1_000_000.0
}

/// Parse a Retry-After value in delay-seconds form. The HTTP-date form is
/// ignored, which falls back to exponential backoff.
fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

fn is_s2_api(url: &str) -> bool {
    url.contains("api.semanticscholar.org")
}

/// The `x-api-key` value for `url`: the S2 key (`$S2_API_KEY`, passed in as
/// `key`) on Semantic Scholar API URLs, nothing elsewhere or when unset/empty.
fn s2_api_key(url: &str, key: Option<String>) -> Option<String> {
    key.filter(|k| is_s2_api(url) && !k.is_empty())
}

/// Error text for a request still rate-limited after `attempts` tries; for
/// Semantic Scholar without a key, names the missing `S2_API_KEY`.
fn rate_limit_message(url: &str, attempts: usize, has_key: bool) -> String {
    let hint = if is_s2_api(url) && !has_key {
        " (S2_API_KEY is not set; requests share the anonymous rate limit)"
    } else {
        ""
    };
    format!("{} for {} after {} attempts{}", RATE_LIMITED, url, attempts, hint)
}

const RATE_LIMITED: &str = "HTTP 429 Too Many Requests";

/// True when `err` is the error for a request still rate-limited after all
/// retries, as opposed to an absent record or another failure.
pub fn is_rate_limited(err: &str) -> bool {
    err.starts_with(RATE_LIMITED)
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

#[cfg(test)]
mod tests {
    use super::{is_rate_limited, parse_retry_after, rate_limit_message, s2_api_key, with_retries, Attempt, RETRY_BUDGET};
    use std::cell::RefCell;
    use std::time::Duration;

    /// Run `with_retries` over a scripted sequence of attempt outcomes,
    /// recording every sleep instead of sleeping.
    async fn drive(
        max_attempts: usize,
        script: Vec<Attempt<&'static str, &'static str>>,
    ) -> (Result<&'static str, (&'static str, usize)>, Vec<Duration>) {
        let script = RefCell::new(script.into_iter());
        let sleeps = RefCell::new(Vec::new());
        let result = with_retries(
            max_attempts,
            RETRY_BUDGET,
            || std::future::ready(script.borrow_mut().next().expect("script exhausted")),
            |d| {
                sleeps.borrow_mut().push(d);
                std::future::ready(())
            },
        )
        .await;
        (result, sleeps.into_inner())
    }

    fn transient(retry_after: Option<u64>) -> Attempt<&'static str, &'static str> {
        Attempt::Retry { err: "rate limited", retry_after: retry_after.map(Duration::from_secs) }
    }

    #[tokio::test]
    async fn retry_after_header_sets_the_wait() {
        let (result, sleeps) = drive(5, vec![transient(Some(7)), Attempt::Done("body")]).await;
        assert_eq!(result, Ok("body"));
        assert_eq!(sleeps, vec![Duration::from_secs(7)]);
    }

    #[tokio::test]
    async fn retry_after_beyond_the_budget_gives_up_without_sleeping() {
        let (result, sleeps) = drive(5, vec![transient(Some(RETRY_BUDGET.as_secs() + 1))]).await;
        assert_eq!(result, Err(("rate limited", 1)));
        assert!(sleeps.is_empty());
    }

    #[tokio::test]
    async fn backoff_doubles_from_two_seconds_with_bounded_jitter_and_total() {
        let (result, sleeps) = drive(5, (0..5).map(|_| transient(None)).collect()).await;
        assert_eq!(result, Err(("rate limited", 5)));
        assert_eq!(sleeps.len(), 4, "no sleep after the final attempt");
        for (k, d) in sleeps.iter().enumerate() {
            let base = Duration::from_secs(2 << k); // 2, 4, 8, 16
            assert!(*d >= base && *d < base.mul_f64(1.5), "sleep {} = {:?}", k, d);
        }
        assert!(sleeps.iter().sum::<Duration>() <= RETRY_BUDGET);
    }

    #[tokio::test]
    async fn total_sleep_never_exceeds_the_budget_however_many_attempts() {
        let (result, sleeps) = drive(50, (0..50).map(|_| transient(None)).collect()).await;
        assert!(result.is_err());
        assert!(sleeps.iter().sum::<Duration>() <= RETRY_BUDGET);
    }

    #[tokio::test]
    async fn success_on_first_attempt_never_sleeps() {
        let (result, sleeps) = drive(5, vec![Attempt::Done("body")]).await;
        assert_eq!(result, Ok("body"));
        assert!(sleeps.is_empty());
    }

    #[test]
    fn retry_after_parses_delay_seconds_only() {
        assert_eq!(parse_retry_after("7"), Some(Duration::from_secs(7)));
        assert_eq!(parse_retry_after(" 30 "), Some(Duration::from_secs(30)));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT"), None);
    }

    #[test]
    fn s2_key_goes_on_every_semantic_scholar_api_url_only() {
        let key = Some("k".to_string());
        for url in [
            "https://api.semanticscholar.org/graph/v1/paper/search?query=x",
            "https://api.semanticscholar.org/graph/v1/paper/ARXIV:2408.01416/references?offset=0",
            "https://api.semanticscholar.org/graph/v1/paper/DOI:10.1/x/citations?offset=1000",
        ] {
            assert_eq!(s2_api_key(url, key.clone()).as_deref(), Some("k"), "url: {}", url);
        }
        assert_eq!(s2_api_key("https://api.openalex.org/works?search=x", key), None);
        assert_eq!(s2_api_key("https://api.semanticscholar.org/graph/v1/paper/search", None), None);
    }

    #[test]
    fn s2_rate_limit_without_a_key_says_so() {
        let url = "https://api.semanticscholar.org/graph/v1/paper/search";
        assert!(rate_limit_message(url, 4, false).contains("S2_API_KEY is not set"));
        assert!(!rate_limit_message(url, 4, true).contains("S2_API_KEY"));
        assert!(!rate_limit_message("https://api.openalex.org/works", 4, false).contains("S2_API_KEY"));
    }

    #[test]
    fn is_rate_limited_recognizes_exhausted_429_retries_only() {
        assert!(is_rate_limited(&rate_limit_message("https://api.openalex.org/works", 4, false)));
        assert!(!is_rate_limited("HTTP 503 for https://api.openalex.org/works"));
        assert!(!is_rate_limited("error sending request"));
    }

    #[test]
    fn test_rustls_client_builds() {
        // Guards the sandbox fix: TLS must come from rustls with bundled webpki
        // roots. macOS SecureTransport (reqwest default-tls) needs keychain
        // access and fails with OSStatus -26276 inside seatbelt sandboxes.
        // `use_rustls_tls()` only compiles while a rustls-tls feature is on.
        reqwest::Client::builder()
            .use_rustls_tls()
            .build()
            .expect("rustls-backed client should build without keychain access");
    }
}
