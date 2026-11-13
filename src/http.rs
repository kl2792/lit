/// HTTP client wrapper with caching support.
///
/// Wraps `reqwest::Client` and integrates with the SQLite-backed `Db`
/// for transparent request deduplication.

use crate::db::Db;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Maximum response body size: 50 MB.
const MAX_RESPONSE_BYTES: usize = 50 * 1024 * 1024;

#[cfg(test)]
#[path = "http/publisher_transport_tests.rs"]
pub(crate) mod publisher_transport_tests;

#[path = "http/pacing.rs"]
pub(crate) mod pacing;

pub struct Client {
    inner: reqwest::Client,
    db: Arc<Db>,
    no_cache: bool,
    patience: Option<Patience>,
}

/// Patient mode (ADR-012): Semantic Scholar requests wait out rate limits
/// for up to `budget` of retry sleep, paced across processes through
/// `pacing_file`, before the caller falls back.
#[derive(Clone, Debug, PartialEq)]
pub struct Patience {
    pub budget: Duration,
    pub pacing_file: PathBuf,
}

/// Patience budget when neither `--patient-budget` nor the `patient_budget`
/// setting names one.
pub const DEFAULT_PATIENCE_BUDGET: Duration = Duration::from_secs(300);

/// Spacing of patient Semantic Scholar requests across processes: the
/// documented 1 request per second for an API key, plus 0.1s for differences
/// in when requests leave each process (ADR-012).
pub const S2_REQUEST_INTERVAL: Duration = Duration::from_millis(1100);

/// Longest patient backoff before jitter, so retries keep coming once the
/// doubling passes a minute.
const PATIENT_BACKOFF_CAP: Duration = Duration::from_secs(60);

/// Shortest pacing wait worth a stderr line.
const PACING_NOTE_THRESHOLD: Duration = Duration::from_secs(2);

/// How one request retries.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Policy {
    max_attempts: usize,
    /// Longest total sleep across retries.
    budget: Duration,
    /// True for a patient Semantic Scholar request: capped backoff, paced
    /// attempts, and progress lines that name the source and budget.
    patient: bool,
}

/// The retry policy for `url`: patient for a Semantic Scholar request when
/// `patience` is set, otherwise `DEFAULT_MAX_ATTEMPTS` attempts under `budget`.
/// `attempts_override` is `$LIT_MAX_ATTEMPTS`; patient requests are limited
/// by their budget alone unless it is set.
fn policy_for(url: &str, patience: Option<&Patience>, budget: Duration, attempts_override: Option<usize>) -> Policy {
    match patience {
        Some(p) if is_semantic_scholar(url) => Policy {
            max_attempts: attempts_override.unwrap_or(usize::MAX),
            budget: p.budget,
            patient: true,
        },
        _ => Policy {
            max_attempts: attempts_override.unwrap_or(DEFAULT_MAX_ATTEMPTS),
            budget,
            patient: false,
        },
    }
}

fn is_semantic_scholar(url: &str) -> bool {
    url.contains("api.semanticscholar.org")
}

/// Uncached publisher response. The caller validates content and persists provenance.
#[derive(Debug)]
pub struct PublisherResponse {
    pub final_url: String,
    pub status: u16,
    pub mime: String,
    pub bytes: Vec<u8>,
}

// A publisher body failure retains headers that arrived, but carries no
// complete-body count or digest. Other HTTP clients keep their existing errors.
#[derive(Debug)]
struct PublisherResponseFailure {
    final_url: String,
    status: u16,
    mime: String,
    message: String,
}
impl std::fmt::Display for PublisherResponseFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.message) }
}
impl std::error::Error for PublisherResponseFailure {}
fn publisher_body_failure(response: &PublisherResponse, message: String) -> Box<dyn std::error::Error> {
    Box::new(PublisherResponseFailure { final_url: response.final_url.clone(), status: response.status, mime: response.mime.clone(), message })
}
pub(crate) fn publisher_failure_headers<'a>(error: &'a (dyn std::error::Error + 'static)) -> Option<(&'a str, u16, &'a str)> {
    error.downcast_ref::<PublisherResponseFailure>().map(|error| (error.final_url.as_str(), error.status, error.mime.as_str()))
}

/// Frontiers' article and artifact hosts. Exact names prevent suffix spoofing.
pub fn is_frontiers_url(url: &reqwest::Url) -> bool {
    url.scheme() == "https" && url.username().is_empty() && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && matches!(url.host_str(), Some("www.frontiersin.org" | "frontiersin.org" | "public-pages-files-2025.frontiersin.org"))
}

/// PMC's article repository and its image CDN, with exact HTTPS hosts.
pub fn is_pmc_repository_url(url: &reqwest::Url) -> bool {
    url.scheme() == "https" && url.username().is_empty() && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && (url.host_str() == Some("pmc.ncbi.nlm.nih.gov") && url.path().starts_with("/articles/")
            || url.host_str() == Some("cdn.ncbi.nlm.nih.gov") && url.path().starts_with("/pmc/blobs/"))
}

/// Public PMC Cloud media objects, confined to one article-version directory.
pub fn is_pmc_cloud_url(url: &reqwest::Url) -> bool {
    if url.scheme() != "https" || url.host_str() != Some("pmc-oa-opendata.s3.amazonaws.com")
        || !url.username().is_empty() || url.password().is_some() || url.port_or_known_default() != Some(443)
        || url.fragment().is_some() { return false; }
    let parts: Vec<_> = url.path().split('/').collect();
    if parts.len() != 3 || parts[0] != "" || parts[2].is_empty() || parts[2] == "." || parts[2] == ".."
        || !parts[2].bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')) { return false; }
    let Some((article, version)) = parts[1].strip_prefix("PMC").and_then(|v| v.split_once('.')) else { return false; };
    !article.is_empty() && article.bytes().all(|b| b.is_ascii_digit())
        && version.bytes().all(|b| b.is_ascii_digit()) && version.parse::<u64>().is_ok_and(|v| v > 0)
}

impl Client {
    /// Scoped binary transport, sharing the HTTP retry budget. Redirects stay
    /// on explicit publisher hosts; denied pages remain responses for capture.
    pub async fn get_publisher(&self, url: &str) -> Result<PublisherResponse, Box<dyn std::error::Error>> {
        #[cfg(test)]
        if let Some(client) = publisher_transport_tests::current_client() {
            return publisher_get(&client, url, 1).await;
        }
        let client = publisher_client_builder(Duration::from_secs(30)).build()?;
        publisher_get(&client, url, attempts_override().unwrap_or(DEFAULT_MAX_ATTEMPTS)).await
    }
    /// Create a new HTTP client backed by the given database.
    ///
    /// When `no_cache` is true, all requests bypass the cache (no reads or writes).
    /// With `patience`, Semantic Scholar requests follow ADR-012.
    /// Timeout defaults to 15 seconds, overridden by `$CURL_TIMEOUT` env var.
    pub fn new(db: Arc<Db>, no_cache: bool, patience: Option<Patience>) -> Self {
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
            patience,
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
        self.get_cached_within(cache_key, url, ttl, RETRY_BUDGET).await
    }

    /// `get_cached` with at most `budget` of total sleep across retries.
    pub async fn get_cached_within(
        &self,
        cache_key: &str,
        url: &str,
        ttl: u64,
        budget: Duration,
    ) -> Result<String, Box<dyn std::error::Error>> {
        if !self.no_cache {
            if let Some(cached) = self.db.cache_get(cache_key, ttl) {
                return Ok(cached);
            }
        }

        let body = self.get_within(url, budget).await?;
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
    /// A 429 that outlasts the retries reports the attempt count, the server's
    /// Retry-After, and a missing API key variable (see `key_spec`).
    /// Sends the host's API key header when its variable is set (`auth_header`).
    /// Rejects responses larger than 50 MB.
    ///
    /// Timeouts, connection errors, 429 and 5xx are retried under `with_retries`.
    pub async fn get(&self, url: &str) -> Result<String, Box<dyn std::error::Error>> {
        self.get_within(url, RETRY_BUDGET).await
    }

    /// `get` with at most `budget` of total sleep across retries, except that
    /// a patient Semantic Scholar request uses the patience budget instead.
    async fn get_within(&self, url: &str, budget: Duration) -> Result<String, Box<dyn std::error::Error>> {
        let policy = policy_for(url, self.patience.as_ref(), budget, attempts_override());
        let pacing_file = self.patience.as_ref().filter(|_| policy.patient).map(|p| p.pacing_file.as_path());
        let outcome = with_retries(
            &policy,
            || async move {
                if let Some(file) = pacing_file {
                    pace(file).await;
                }
                self.attempt(url).await
            },
            tokio::time::sleep,
            |line| eprintln!("{}", line),
        )
        .await;
        match outcome {
            Ok(result) => result,
            Err((Transient::RateLimited, attempts, retry_after)) => {
                let has_key = auth_header(url, env_var).is_some();
                Err(rate_limit_message(url, attempts, has_key, retry_after).into())
            }
            Err((Transient::Status(status), _, _)) => Err(format!("HTTP {} for {}", status, url).into()),
            Err((Transient::Transport(e), _, _)) => Err(e.into()),
        }
    }

    /// One GET: a final result, or a transient failure with the server's
    /// Retry-After hint when it sent one.
    async fn attempt(&self, url: &str) -> Attempt<Result<String, Box<dyn std::error::Error>>, Transient> {
        let mut req = self.inner.get(url);
        if let Some((name, value)) = auth_header(url, env_var) {
            req = req.header(name, value);
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

fn publisher_client_builder(timeout: Duration) -> reqwest::ClientBuilder {
    reqwest::Client::builder().use_rustls_tls().timeout(timeout).user_agent("lit/1.0")
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 10
                || attempt.previous().first().is_some_and(|original| is_pmc_cloud_url(original) && original != attempt.url())
                || !(is_frontiers_url(attempt.url()) || is_pmc_repository_url(attempt.url()) || is_pmc_cloud_url(attempt.url())) {
                attempt.error("publisher redirect outside supported scope")
            } else { attempt.follow() }
        }))
}

async fn publisher_get(client: &reqwest::Client, url: &str, attempts: usize) -> Result<PublisherResponse, Box<dyn std::error::Error>> {
    let parsed = reqwest::Url::parse(url)?;
    if !(is_frontiers_url(&parsed) || is_pmc_repository_url(&parsed) || is_pmc_cloud_url(&parsed)) {
        return Err(format!("unsupported publisher URL: {url}").into());
    }
    let last_response = std::cell::RefCell::new(None);
    let policy = Policy { max_attempts: attempts, budget: RETRY_BUDGET, patient: false };
    let result = with_retries(&policy, || async {
        // Metadata belongs to the current attempt. A later connection failure
        // must not masquerade as the preceding attempt's HTTP response.
        last_response.replace(None);
        let resp = match client.get(url).send().await {
            Ok(resp) => resp,
            Err(err) if err.is_timeout() || err.is_connect() => return Attempt::Retry { err: Transient::Transport(err), retry_after: None },
            Err(err) => return Attempt::Done(Err(err.into())),
        };
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS || resp.status().is_server_error() {
            let retry_after = resp.headers().get(reqwest::header::RETRY_AFTER).and_then(|v| v.to_str().ok()).and_then(parse_retry_after);
            last_response.replace(Some(PublisherResponseFailure {
                final_url: resp.url().to_string(), status: resp.status().as_u16(),
                mime: resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string(),
                message: String::new(),
            }));
            return Attempt::Retry { err: Transient::Status(resp.status()), retry_after };
        }
        Attempt::Done(read_publisher(resp).await)
    }, tokio::time::sleep, |line| eprintln!("{}", line)).await;
    match result {
        Ok(response) => response,
        Err((error, attempts, _)) => {
            let message = format!("publisher retrieval {url} failed after {attempts} attempts: {error}");
            if let Some(mut response) = last_response.into_inner() {
                response.message = message;
                Err(Box::new(response))
            } else { Err(message.into()) }
        }
    }
}

async fn read_publisher(mut resp: reqwest::Response) -> Result<PublisherResponse, Box<dyn std::error::Error>> {
    let mut result = PublisherResponse {
        final_url: resp.url().to_string(), status: resp.status().as_u16(),
        mime: resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string(),
        bytes: Vec::new(),
    };
    if resp.content_length().is_some_and(|n| n > MAX_RESPONSE_BYTES as u64) {
        return Err(publisher_body_failure(&result, format!("publisher response exceeds 50 MB for {}", result.final_url)));
    }
    while let Some(chunk) = resp.chunk().await.map_err(|error| {
        publisher_body_failure(&result, format!("publisher body {} for {}: {error}", if error.is_timeout() { "timeout" } else { "read failed" }, result.final_url))
    })? {
        if result.bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(publisher_body_failure(&result, format!("publisher response exceeds 50 MB for {}", result.final_url)));
        }
        result.bytes.extend_from_slice(&chunk);
    }
    Ok(result)
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
pub const RETRY_BUDGET: Duration = Duration::from_secs(60);

/// Retry budget for a request whose caller has another source to try, such as
/// one tier of the search cascade: the 2s and 4s backoffs fit with any jitter
/// (at most 3s + 6s), and a longer Retry-After ends the retries at once, so
/// the next source answers within about ten seconds.
pub const FALLBACK_RETRY_BUDGET: Duration = Duration::from_secs(9);

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

/// Run `attempt` up to `policy.max_attempts` times, sleeping between
/// transient failures and passing one progress line per retry to `note`.
///
/// The wait before retry `k` (0-based) is the server's Retry-After when given,
/// otherwise `2^(k+1)` seconds plus up to 50% jitter (2s, 4s, 8s, 16s, ...),
/// the doubling capped at 60s for a patient policy.
/// Gives up early rather than let total sleep exceed `policy.budget`.
/// Returns the first `Done` value, or the last transient error, the number
/// of attempts made, and the last attempt's Retry-After.
async fn with_retries<T, E, A, AFut, S, SFut, N>(
    policy: &Policy,
    mut attempt: A,
    mut sleep: S,
    mut note: N,
) -> Result<T, (E, usize, Option<Duration>)>
where
    E: std::fmt::Display,
    A: FnMut() -> AFut,
    AFut: std::future::Future<Output = Attempt<T, E>>,
    S: FnMut(Duration) -> SFut,
    SFut: std::future::Future<Output = ()>,
    N: FnMut(String),
{
    let cap = policy.patient.then_some(PATIENT_BACKOFF_CAP);
    let mut slept = Duration::ZERO;
    let mut made = 0;
    loop {
        made += 1;
        let (err, retry_after) = match attempt().await {
            Attempt::Done(value) => return Ok(value),
            Attempt::Retry { err, retry_after } => (err, retry_after),
        };
        let wait = retry_after.unwrap_or_else(|| backoff(made - 1, jitter_fraction(), cap));
        if made >= policy.max_attempts || slept + wait > policy.budget {
            return Err((err, made, retry_after));
        }
        note(retry_note(policy, &err, made, wait, slept + wait));
        sleep(wait).await;
        slept += wait;
    }
}

/// The progress line for a retry after attempt `made`, waiting `wait`, with
/// `used` of the budget slept once the wait ends.
fn retry_note(policy: &Policy, err: &dyn std::fmt::Display, made: usize, wait: Duration, used: Duration) -> String {
    if policy.patient {
        format!(
            "note: Semantic Scholar: {} (attempt {}), retrying in {:.1}s ({:.0}s of {}s patience used)",
            err, made, wait.as_secs_f64(), used.as_secs_f64(), policy.budget.as_secs()
        )
    } else {
        format!(
            "note: {} (attempt {}/{}), retrying in {:.1}s...",
            err, made, policy.max_attempts, wait.as_secs_f64()
        )
    }
}

/// Exponential backoff for retry `k`: `2^(k+1)` seconds, at most `cap`,
/// scaled by `1 + jitter/2` with `jitter` in `[0, 1)`. The exponent is capped
/// so the shift cannot overflow.
fn backoff(k: usize, jitter: f64, cap: Option<Duration>) -> Duration {
    let base = Duration::from_secs(2u64 << k.min(16));
    cap.map_or(base, |c| base.min(c)).mul_f64(1.0 + jitter / 2.0)
}

/// `$LIT_MAX_ATTEMPTS` when it is a positive integer.
fn attempts_override() -> Option<usize> {
    std::env::var("LIT_MAX_ATTEMPTS").ok().and_then(|v| v.parse::<usize>().ok()).filter(|&n| n > 0)
}

/// Wait for this process's Semantic Scholar request slot in `file`
/// (ADR-012), noting waits of 2s or more. A pacing file that cannot be used
/// is reported and the request goes unpaced.
async fn pace(file: &std::path::Path) {
    match pacing::reserve(file, S2_REQUEST_INTERVAL) {
        Ok(wait) => {
            if let Some(line) = pacing_note(wait) {
                eprintln!("{}", line);
            }
            tokio::time::sleep(wait).await;
        }
        Err(e) => eprintln!("note: Semantic Scholar pacing unavailable ({}: {}); requesting unpaced", file.display(), e),
    }
}

/// The stderr line for a pacing wait, when it is long enough to report.
fn pacing_note(wait: Duration) -> Option<String> {
    (wait >= PACING_NOTE_THRESHOLD).then(|| {
        format!("note: Semantic Scholar: waiting {:.1}s for a request slot shared with other lit processes", wait.as_secs_f64())
    })
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

/// How an API host takes a key: the environment variable holding it, the
/// request header carrying it, and the prefix before the key in that header.
struct KeySpec {
    env: &'static str,
    header: &'static str,
    prefix: &'static str,
}

/// The key scheme for `url`'s API host, if it takes one.
fn key_spec(url: &str) -> Option<KeySpec> {
    if is_semantic_scholar(url) {
        Some(KeySpec { env: "S2_API_KEY", header: "x-api-key", prefix: "" })
    } else if url.contains("api.openalex.org") {
        Some(KeySpec { env: "OPENALEX_API_KEY", header: "authorization", prefix: "Bearer " })
    } else {
        None
    }
}

/// The auth header (name, value) for `url`, with the key read through `env`;
/// nothing when the host takes no key or its variable is unset or empty.
fn auth_header(url: &str, env: impl Fn(&str) -> Option<String>) -> Option<(&'static str, String)> {
    let spec = key_spec(url)?;
    let key = env(spec.env).filter(|k| !k.is_empty())?;
    Some((spec.header, format!("{}{}", spec.prefix, key)))
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Error text for a request still rate-limited after `attempts` tries. Names
/// the server's last Retry-After when sent, and the missing key variable for a
/// keyed API host requested without one.
fn rate_limit_message(url: &str, attempts: usize, has_key: bool, retry_after: Option<Duration>) -> String {
    let wait = retry_after.map_or(String::new(), |d| format!("; retry after {}s", d.as_secs()));
    let hint = match key_spec(url) {
        Some(spec) if !has_key => format!(" ({} is not set; requests share the anonymous rate limit)", spec.env),
        _ => String::new(),
    };
    format!("{} for {} after {} attempts{}{}", RATE_LIMITED, url, attempts, wait, hint)
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
    use super::{auth_header, backoff, is_rate_limited, pacing_note, parse_retry_after, policy_for, rate_limit_message, retry_note, with_retries, Attempt, Patience, Policy, DEFAULT_MAX_ATTEMPTS, DEFAULT_PATIENCE_BUDGET, FALLBACK_RETRY_BUDGET, RETRY_BUDGET};
    use std::cell::RefCell;
    use std::time::Duration;
    #[test]
    fn publisher_scope_rejects_host_spoofing_credentials_and_downgrade() {
        for url in ["http://www.frontiersin.org/a", "https://www.frontiersin.org.evil.test/a", "https://user@www.frontiersin.org/a", "https://www.frontiersin.org:444/a", "file:///a"] {
            assert!(!super::is_frontiers_url(&reqwest::Url::parse(url).unwrap()), "{url}");
        }
        assert!(super::is_frontiers_url(&reqwest::Url::parse("https://www.frontiersin.org/a").unwrap()));
    }

    /// Run `with_retries` over a scripted sequence of attempt outcomes,
    /// recording every sleep instead of sleeping.
    async fn drive(
        max_attempts: usize,
        script: Vec<Attempt<&'static str, &'static str>>,
    ) -> (Result<&'static str, (&'static str, usize, Option<Duration>)>, Vec<Duration>) {
        drive_within(RETRY_BUDGET, max_attempts, script).await
    }

    /// `drive` under total sleep budget `budget`.
    async fn drive_within(
        budget: Duration,
        max_attempts: usize,
        script: Vec<Attempt<&'static str, &'static str>>,
    ) -> (Result<&'static str, (&'static str, usize, Option<Duration>)>, Vec<Duration>) {
        let (result, sleeps, _) = drive_policy(Policy { max_attempts, budget, patient: false }, script).await;
        (result, sleeps)
    }

    /// Run `with_retries` under `policy` over a scripted sequence of attempt
    /// outcomes, recording every sleep (the mocked clock) and progress line.
    async fn drive_policy(
        policy: Policy,
        script: Vec<Attempt<&'static str, &'static str>>,
    ) -> (Result<&'static str, (&'static str, usize, Option<Duration>)>, Vec<Duration>, Vec<String>) {
        let script = RefCell::new(script.into_iter());
        let sleeps = RefCell::new(Vec::new());
        let notes = RefCell::new(Vec::new());
        let result = with_retries(
            &policy,
            || std::future::ready(script.borrow_mut().next().expect("script exhausted")),
            |d| {
                sleeps.borrow_mut().push(d);
                std::future::ready(())
            },
            |line| notes.borrow_mut().push(line),
        )
        .await;
        (result, sleeps.into_inner(), notes.into_inner())
    }

    const S2_SEARCH: &str = "https://api.semanticscholar.org/graph/v1/paper/search?query=x";

    fn patience() -> Patience {
        Patience { budget: DEFAULT_PATIENCE_BUDGET, pacing_file: "unused".into() }
    }

    fn patient_policy() -> Policy {
        policy_for(S2_SEARCH, Some(&patience()), FALLBACK_RETRY_BUDGET, None)
    }

    #[test]
    fn without_patience_every_request_keeps_its_caller_budget_and_five_attempts() {
        for url in [S2_SEARCH, "https://api.openalex.org/works?search=x"] {
            for budget in [FALLBACK_RETRY_BUDGET, RETRY_BUDGET] {
                assert_eq!(
                    policy_for(url, None, budget, None),
                    Policy { max_attempts: DEFAULT_MAX_ATTEMPTS, budget, patient: false }
                );
            }
        }
        assert_eq!(policy_for(S2_SEARCH, None, RETRY_BUDGET, Some(2)).max_attempts, 2);
    }

    #[test]
    fn patience_applies_to_semantic_scholar_only() {
        let p = Patience { budget: Duration::from_secs(120), pacing_file: "f".into() };
        assert_eq!(
            policy_for(S2_SEARCH, Some(&p), FALLBACK_RETRY_BUDGET, None),
            Policy { max_attempts: usize::MAX, budget: Duration::from_secs(120), patient: true }
        );
        let s2_cites = "https://api.semanticscholar.org/graph/v1/paper/ARXIV:1/citations?offset=0";
        assert!(policy_for(s2_cites, Some(&p), RETRY_BUDGET, None).patient);
        for url in ["https://api.openalex.org/works?search=x", "https://export.arxiv.org/api/query?search_query=x"] {
            assert_eq!(
                policy_for(url, Some(&p), FALLBACK_RETRY_BUDGET, None),
                Policy { max_attempts: DEFAULT_MAX_ATTEMPTS, budget: FALLBACK_RETRY_BUDGET, patient: false },
                "{}", url
            );
        }
        assert_eq!(policy_for(S2_SEARCH, Some(&p), RETRY_BUDGET, Some(3)).max_attempts, 3);
    }

    #[test]
    fn patient_backoff_doubles_to_a_sixty_second_cap_with_bounded_jitter() {
        let cap = Some(Duration::from_secs(60));
        let bases = [2, 4, 8, 16, 32, 60, 60, 60];
        for (k, base) in bases.iter().enumerate() {
            let base = Duration::from_secs(*base);
            assert_eq!(backoff(k, 0.0, cap), base, "retry {}", k);
            assert!(backoff(k, 0.999, cap) < base.mul_f64(1.5));
        }
        assert_eq!(backoff(40, 0.0, cap), Duration::from_secs(60), "no overflow far out");
        assert_eq!(backoff(5, 0.0, None), Duration::from_secs(64), "default backoff is uncapped");
    }

    #[tokio::test]
    async fn patient_retries_continue_past_five_attempts_until_the_budget_is_spent() {
        let (result, sleeps, _) = drive_policy(patient_policy(), (0..100).map(|_| transient(None)).collect()).await;
        let (err, made, _) = result.unwrap_err();
        assert_eq!(err, "rate limited");
        assert!(made > DEFAULT_MAX_ATTEMPTS, "made {} attempts", made);
        let total: Duration = sleeps.iter().sum();
        assert!(total <= DEFAULT_PATIENCE_BUDGET, "slept {:?}", total);
        // The refused wait, under 90s (the 60s cap plus 50% jitter), would have passed the budget.
        assert!(total + Duration::from_secs(90) > DEFAULT_PATIENCE_BUDGET, "gave up early at {:?}", total);
        for (k, d) in sleeps.iter().enumerate() {
            let base = Duration::from_secs((2u64 << k.min(16)).min(60));
            assert!(*d >= base && *d < base.mul_f64(1.5), "sleep {} = {:?}", k, d);
        }
    }

    #[tokio::test]
    async fn patient_success_after_retries_returns_the_body() {
        let script = vec![transient(None), transient(None), transient(None), transient(None), transient(None), Attempt::Done("body")];
        let (result, sleeps, notes) = drive_policy(patient_policy(), script).await;
        assert_eq!(result, Ok("body"));
        assert_eq!(sleeps.len(), 5);
        assert_eq!(notes.len(), 5, "one progress line per retry");
    }

    #[tokio::test]
    async fn retry_after_takes_precedence_over_patient_backoff_and_its_cap() {
        let (result, sleeps, _) = drive_policy(patient_policy(), vec![transient(Some(7)), transient(Some(120)), Attempt::Done("body")]).await;
        assert_eq!(result, Ok("body"));
        assert_eq!(sleeps, vec![Duration::from_secs(7), Duration::from_secs(120)]);
    }

    #[tokio::test]
    async fn patient_retry_after_beyond_the_remaining_budget_gives_up_at_once() {
        let (result, sleeps, _) = drive_policy(patient_policy(), vec![transient(Some(200)), transient(Some(101))]).await;
        assert_eq!(result, Err(("rate limited", 2, Some(Duration::from_secs(101)))));
        assert_eq!(sleeps, vec![Duration::from_secs(200)], "200s fits, 200s + 101s does not");
    }

    #[tokio::test]
    async fn patient_progress_lines_name_the_source_attempt_wait_and_budget_used() {
        let (_, sleeps, notes) = drive_policy(patient_policy(), vec![transient(Some(7)), transient(Some(30)), Attempt::Done("body")]).await;
        assert_eq!(sleeps.len(), 2);
        assert_eq!(notes, vec![
            "note: Semantic Scholar: rate limited (attempt 1), retrying in 7.0s (7s of 300s patience used)".to_string(),
            "note: Semantic Scholar: rate limited (attempt 2), retrying in 30.0s (37s of 300s patience used)".to_string(),
        ]);
    }

    #[test]
    fn default_progress_line_is_unchanged() {
        let policy = Policy { max_attempts: 5, budget: RETRY_BUDGET, patient: false };
        assert_eq!(
            retry_note(&policy, &"rate limited", 2, Duration::from_millis(4300), Duration::from_secs(7)),
            "note: rate limited (attempt 2/5), retrying in 4.3s..."
        );
    }

    #[tokio::test]
    async fn default_policy_prints_one_line_per_retry() {
        let (_, _, notes) = drive_policy(
            Policy { max_attempts: 5, budget: RETRY_BUDGET, patient: false },
            vec![transient(Some(3)), Attempt::Done("body")],
        ).await;
        assert_eq!(notes, vec!["note: rate limited (attempt 1/5), retrying in 3.0s...".to_string()]);
    }

    #[test]
    fn pacing_waits_of_two_seconds_or_more_are_reported() {
        assert_eq!(pacing_note(Duration::from_millis(1100)), None);
        assert_eq!(
            pacing_note(Duration::from_millis(3300)).as_deref(),
            Some("note: Semantic Scholar: waiting 3.3s for a request slot shared with other lit processes")
        );
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
        assert_eq!(result, Err(("rate limited", 1, Some(RETRY_BUDGET + Duration::from_secs(1)))));
        assert!(sleeps.is_empty());
    }

    #[tokio::test]
    async fn backoff_doubles_from_two_seconds_with_bounded_jitter_and_total() {
        let (result, sleeps) = drive(5, (0..5).map(|_| transient(None)).collect()).await;
        assert_eq!(result, Err(("rate limited", 5, None)));
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
    async fn fallback_budget_gives_up_within_ten_seconds_and_honors_short_retry_after() {
        let (result, sleeps) = drive_within(FALLBACK_RETRY_BUDGET, 5, (0..5).map(|_| transient(None)).collect()).await;
        assert!(result.is_err());
        assert!(FALLBACK_RETRY_BUDGET <= Duration::from_secs(10));
        assert!(sleeps.iter().sum::<Duration>() <= FALLBACK_RETRY_BUDGET, "{:?}", sleeps);
        let (result, sleeps) = drive_within(FALLBACK_RETRY_BUDGET, 5, vec![transient(Some(3)), Attempt::Done("body")]).await;
        assert_eq!((result, sleeps), (Ok("body"), vec![Duration::from_secs(3)]));
        let (result, sleeps) = drive_within(FALLBACK_RETRY_BUDGET, 5, vec![transient(Some(30))]).await;
        assert_eq!(result, Err(("rate limited", 1, Some(Duration::from_secs(30)))));
        assert!(sleeps.is_empty(), "a longer Retry-After falls back at once");
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

    /// An environment holding only `var=value`.
    fn env_with(var: &'static str, value: &'static str) -> impl Fn(&str) -> Option<String> {
        move |v| (v == var).then(|| value.to_string())
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn s2_key_goes_on_every_semantic_scholar_api_url_only() {
        let env = env_with("S2_API_KEY", "k");
        for url in [
            "https://api.semanticscholar.org/graph/v1/paper/search?query=x",
            "https://api.semanticscholar.org/graph/v1/paper/ARXIV:2408.01416/references?offset=0",
            "https://api.semanticscholar.org/graph/v1/paper/DOI:10.1/x/citations?offset=1000",
        ] {
            assert_eq!(auth_header(url, &env), Some(("x-api-key", "k".to_string())), "url: {}", url);
        }
        assert_eq!(auth_header("https://api.openalex.org/works?search=x", &env), None);
        assert_eq!(auth_header("https://api.semanticscholar.org/graph/v1/paper/search", no_env), None);
    }

    #[test]
    fn openalex_key_goes_as_bearer_token_on_openalex_api_urls_only() {
        let env = env_with("OPENALEX_API_KEY", "k");
        assert_eq!(
            auth_header("https://api.openalex.org/works?search=x", &env),
            Some(("authorization", "Bearer k".to_string()))
        );
        assert_eq!(auth_header("https://api.semanticscholar.org/graph/v1/paper/search", &env), None);
        assert_eq!(auth_header("https://export.arxiv.org/api/query?id_list=1", &env), None);
        assert_eq!(auth_header("https://api.openalex.org/works", no_env), None);
    }

    #[test]
    fn empty_key_sends_no_header() {
        assert_eq!(auth_header("https://api.openalex.org/works", env_with("OPENALEX_API_KEY", "")), None);
        assert_eq!(auth_header("https://api.semanticscholar.org/graph/v1/paper/x", env_with("S2_API_KEY", "")), None);
    }

    #[test]
    fn s2_rate_limit_without_a_key_says_so() {
        let url = "https://api.semanticscholar.org/graph/v1/paper/search";
        assert!(rate_limit_message(url, 4, false, None).contains("S2_API_KEY is not set"));
        assert!(!rate_limit_message(url, 4, true, None).contains("S2_API_KEY"));
        assert!(!rate_limit_message("https://api.openalex.org/works", 4, false, None).contains("S2_API_KEY"));
    }

    #[test]
    fn openalex_rate_limit_without_a_key_says_so() {
        let url = "https://api.openalex.org/works?search=x";
        assert!(rate_limit_message(url, 1, false, None).contains("OPENALEX_API_KEY is not set"));
        assert!(!rate_limit_message(url, 1, true, None).contains("OPENALEX_API_KEY"));
        assert!(!rate_limit_message("https://export.arxiv.org/api/query", 1, false, None).contains("API_KEY"));
    }

    #[test]
    fn rate_limit_message_reports_the_server_requested_wait() {
        let url = "https://api.openalex.org/works";
        let msg = rate_limit_message(url, 1, false, Some(Duration::from_secs(35714)));
        assert!(msg.contains("retry after 35714s"), "{}", msg);
        assert!(!rate_limit_message(url, 4, false, None).contains("retry after"));
    }

    #[tokio::test]
    async fn exhausted_retries_return_the_last_retry_after() {
        let (result, _) = drive(5, vec![transient(Some(35714))]).await;
        assert_eq!(result, Err(("rate limited", 1, Some(Duration::from_secs(35714)))));
        let (result, _) = drive(2, vec![transient(None), transient(None)]).await;
        assert_eq!(result, Err(("rate limited", 2, None)));
    }

    #[test]
    fn is_rate_limited_recognizes_exhausted_429_retries_only() {
        assert!(is_rate_limited(&rate_limit_message("https://api.openalex.org/works", 4, false, None)));
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
