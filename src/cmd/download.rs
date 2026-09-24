/// `lit download <id>` -- Download PDF or arXiv LaTeX source.
///
/// Default: find open-access PDF via Unpaywall and print URL.
/// `--source`: download arXiv LaTeX source tarball, extract, write source.yaml.
/// `--url-only`: print the candidate URLs, in tier order, without downloading.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::Context;
use crate::api::arxiv;
use crate::api::{extract_last_name, unpaywall, PaperResult};
use crate::citekey::SKIP_WORDS;
use crate::db;
use crate::detect::{normalize_arxiv, normalize_doi};
use crate::format;

/// Download timeout for source tarballs (seconds).
const DOWNLOAD_TIMEOUT_SECS: u64 = 60;

pub async fn run(
    ctx: &Context,
    input: &str,
    source: bool,
    url_only: bool,
    dir_override: Option<&Path>,
    citekey: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    if source {
        return run_source(ctx, input, dir_override).await;
    }
    run_pdf(ctx, input, url_only, citekey).await
}

/// Find open-access PDF via Unpaywall, with S2 and OpenAlex fallbacks.
/// Resolves in tier order (open access, Clio, EZProxy) and downloads the first
/// tier that delivers bytes to etc/pdf/<citekey>/.
async fn run_pdf(ctx: &Context, input: &str, url_only: bool, citekey: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    use crate::api::{openalex as oa_api, semantic_scholar as s2_api};

    let doi = normalize_doi(input);
    let client = ctx.client();

    let uw_key = db::Db::cache_key("unpaywall", &doi);
    let uw_url = unpaywall::pdf_url(&doi);
    let s2_key = db::Db::cache_key("s2_paper_doi", &doi);
    let s2_url = s2_api::paper_url(&format!("DOI:{}", doi));
    let oa_key = db::Db::cache_key("oa_work", &doi);
    let oa_url = oa_api::work_by_doi_url(&doi);

    let (uw_body, s2_body, oa_body) = tokio::join!(
        client.get_cached(&uw_key, &uw_url, db::TTL_DOI),
        client.get_cached(&s2_key, &s2_url, db::TTL_DOI),
        client.get_cached(&oa_key, &oa_url, db::TTL_DOI),
    );

    let uw_result = uw_body.ok().and_then(|b| unpaywall::parse_response(&b).ok());
    let title = uw_result.as_ref().map(|r| r.title.clone()).unwrap_or_else(|| "N/A".to_string());
    let uw_pdf = uw_result.and_then(|r| r.pdf_url);

    let s2_result = s2_body.ok().and_then(|b| s2_api::parse_paper(&b).ok());
    let s2_pdf = s2_result.as_ref().and_then(|r| r.pdf_url.clone());

    let oa_result = oa_body.ok().and_then(|b| oa_api::parse_work(&b).ok());
    let oa_pdf = oa_result.as_ref().and_then(|r| r.oa_url.clone());

    let pdf_url = [uw_pdf, s2_pdf, oa_pdf]
        .into_iter()
        .flatten()
        .find(|u| !u.is_empty());

    let cookie_path = find_cookie_file();
    let ez_url = if cookie_path.is_some() && !doi.is_empty() {
        Some(format!("https://doi-org.ezproxy.cul.columbia.edu/{}", doi))
    } else {
        None
    };

    let clio_url = lookup_clio_url(&doi);
    let tiers = build_tiers(
        pdf_url.as_deref(),
        clio_url.as_deref(),
        ez_url.as_deref(),
        cookie_path.as_deref(),
    );

    if url_only {
        if tiers.is_empty() {
            return Err("No open-access PDF found".into());
        }
        for url in tier_urls(&tiers) {
            println!("{}", url);
        }
        return Ok(());
    }

    println!("Title: {}", title);

    // Build a synthetic PaperResult for metadata (citekey generation).
    let meta = s2_result.unwrap_or_else(|| PaperResult {
        title: title.clone(),
        doi: if doi.is_empty() { None } else { Some(doi.clone()) },
        ..Default::default()
    });

    // A tier is tried whenever the ones before it returned no bytes, so a dead
    // link in one tier cannot disable a later one.
    let fetched = fetch_first_success(&tiers, |url, cookies| async move {
        fetch_pdf_via_curl(&url, cookies.as_deref()).await
    })
    .await;

    match fetched {
        Ok((data, delivered_by)) => {
            let dir_name = {
                let slug = citekey.map(|k| k.to_string()).unwrap_or_else(|| generate_dir_name(&meta));
                default_output_dir().join(slug)
            };
            std::fs::create_dir_all(&dir_name)?;
            let pdf_path = dir_name.join("paper.pdf");
            std::fs::write(&pdf_path, &data)?;
            let _ = Command::new("pdftotext")
                .arg(&pdf_path)
                .arg(dir_name.join("paper.txt"))
                .status();
            let today = today_string();
            // Provenance is the tier that produced these bytes, not the first tier
            // that merely had a candidate URL.
            let yaml = build_doi_source_yaml(&meta, &doi, citekey.as_deref(), Some(delivered_by.as_str()), &today);
            std::fs::write(dir_name.join("source.yaml"), &yaml)?;
            let kb = data.len() / 1024;
            println!("Saved: {} ({}KB)", dir_name.display(), kb);
        }
        Err(failures) => {
            for line in failure_report(&failures, pdf_url.as_deref(), clio_url.as_deref(), ez_url.as_deref()) {
                println!("{}", line);
            }
        }
    }

    Ok(())
}

/// One acquisition tier: a candidate URL and the cookie jar its fetch needs.
struct Tier {
    label: &'static str,
    url: String,
    cookies: Option<PathBuf>,
}

/// Build the acquisition tiers for one paper, in the order ADR-002 fixes:
/// open access, then the local Clio index, then EZProxy.
///
/// Both `--url-only` and the fetch path read this one list, so what the flag
/// reports cannot diverge from what the download tries.
fn build_tiers(
    pdf_url: Option<&str>,
    clio_url: Option<&str>,
    ez_url: Option<&str>,
    cookie_path: Option<&Path>,
) -> Vec<Tier> {
    let mut tiers = Vec::new();
    if let Some(url) = pdf_url {
        tiers.push(Tier { label: "open-access PDF", url: url.to_string(), cookies: None });
    }
    if let Some(url) = clio_url {
        tiers.push(Tier {
            label: "Clio URL",
            url: url.to_string(),
            cookies: cookie_path.map(|p| p.to_path_buf()),
        });
    }
    // EZProxy without a session cookie always fails, so it is not a candidate.
    if let (Some(url), Some(cp)) = (ez_url, cookie_path) {
        tiers.push(Tier {
            label: "EZProxy",
            url: url.to_string(),
            cookies: Some(cp.to_path_buf()),
        });
    }
    tiers
}

/// The candidate URLs, in tier order.
fn tier_urls(tiers: &[Tier]) -> Vec<String> {
    tiers.iter().map(|t| t.url.clone()).collect()
}

/// Try each tier in order, returning the bytes and the URL that produced them,
/// or every tier's failure in the order they occurred.
///
/// The failures are carried rather than counted, because what the caller has to
/// tell the user depends on which kind they are.
///
/// The fetcher is a parameter so the tier order can be tested without network access.
async fn fetch_first_success<F, Fut>(tiers: &[Tier], fetch: F) -> Result<(Vec<u8>, String), Vec<FetchError>>
where
    F: Fn(String, Option<PathBuf>) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, FetchError>>,
{
    let mut failures = Vec::new();
    for tier in tiers {
        format::info(&format!("Trying {}: {}", tier.label, tier.url));
        match fetch(tier.url.clone(), tier.cookies.clone()).await {
            Ok(data) => return Ok((data, tier.url.clone())),
            Err(e) => {
                format::warn(&e.to_string());
                failures.push(e);
            }
        }
    }
    Err(failures)
}

/// Look up a DOI's full-text URL in the local Clio index, if one is built.
fn lookup_clio_url(doi: &str) -> Option<String> {
    use crate::api::clio as clio_api;

    if doi.is_empty() {
        return None;
    }
    let clio_db = clio_api::default_clio_db_path();
    if !clio_db.exists() {
        return None;
    }
    let conn = rusqlite::Connection::open(&clio_db).ok()?;
    if clio_api::doi_index_stale(&conn) {
        format::warn("Clio DOI index missing; run `lit clio sync --force` to build it.");
        return None;
    }
    clio_api::lookup_doi_url(&conn, doi)
}

/// Why one tier delivered no PDF.
///
/// A server that answered with a login page and a fetch that never completed
/// are different facts, and only the first is evidence about the paper. The
/// `Option` this replaced collapsed them, so a local failure printed as
/// "No open-access PDF found".
#[derive(Debug)]
pub enum FetchError {
    /// The fetch completed and the bytes are not a PDF: a paywall interstitial,
    /// a login page, or an error document served with HTTP 200.
    NotPdf { url: String, bytes: usize },
    /// The fetch itself failed. `message` carries curl's stderr when curl ran.
    Transport { url: String, code: Option<i32>, message: String },
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::NotPdf { url, bytes } => write!(
                f,
                "{} answered with {} bytes that are not a PDF",
                url, bytes
            ),
            FetchError::Transport { url, code, message } => {
                let code = code.map(|c| c.to_string()).unwrap_or_else(|| "none".to_string());
                write!(f, "fetching {} failed (curl exit {}): {}", url, code, message)
            }
        }
    }
}

impl std::error::Error for FetchError {}

/// The curl arguments for one fetch.
///
/// The body arrives on stdout, so there is no output path and no directory that
/// has to be writable. `-s` suppresses the progress meter and `-S` keeps the
/// error message, which `-s` alone would also discard.
fn curl_args(url: &str, cookie_path: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = [
        "-sSL", "--max-time", "60",
        "-A", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36",
        "-H", "Accept: application/pdf,*/*",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Some(cp) = cookie_path {
        args.push("-b".to_string());
        args.push(cp.to_string());
    }
    args.push(url.to_string());
    args
}

/// Fetch a URL as bytes via curl.
///
/// The bytes are read from curl's stdout, never staged through a file: they are
/// buffered in memory either way, and a scratch path is one more thing that can
/// be unwritable. stderr is captured separately so it cannot enter the payload.
async fn fetch_pdf_via_curl(url: &str, cookie_path: Option<&std::path::Path>) -> Result<Vec<u8>, FetchError> {
    let transport = |code: Option<i32>, message: String| FetchError::Transport {
        url: url.to_string(),
        code,
        message,
    };
    let cookies = match cookie_path {
        Some(cp) => match cp.to_str() {
            Some(s) => Some(s.to_string()),
            None => {
                return Err(transport(None, format!("cookie jar path is not valid UTF-8: {}", cp.display())));
            }
        },
        None => None,
    };

    let output = Command::new("curl")
        .args(curl_args(url, cookies.as_deref()))
        .output()
        .map_err(|e| transport(None, format!("could not run curl: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(transport(output.status.code(), stderr));
    }
    if output.stdout.is_empty() {
        return Err(transport(output.status.code(), "curl succeeded but returned no bytes".to_string()));
    }
    if output.stdout.starts_with(b"%PDF") {
        Ok(output.stdout)
    } else {
        Err(FetchError::NotPdf { url: url.to_string(), bytes: output.stdout.len() })
    }
}

/// The lines `lit download` prints when no tier delivered a PDF.
///
/// "No open-access PDF found" is a claim about the paper, so it is made only
/// when every attempt reached a server and none of them returned a PDF. A
/// transport failure is reported as itself, with the cause curl gave.
fn failure_report(
    failures: &[FetchError],
    pdf_url: Option<&str>,
    clio_url: Option<&str>,
    ez_url: Option<&str>,
) -> Vec<String> {
    let mut lines = Vec::new();
    let mut transport_failed = false;
    for failure in failures {
        if let FetchError::Transport { .. } = failure {
            transport_failed = true;
            lines.push(format!("Fetch failed: {}", failure));
        }
    }
    if let Some(url) = pdf_url {
        lines.push(format!("PDF: {}", url));
    } else if !transport_failed {
        lines.push("No open-access PDF found".to_string());
    }
    if let Some(url) = clio_url {
        lines.push(format!("Clio: {}", url));
    }
    if let Some(url) = ez_url {
        lines.push(format!("EZProxy: {} [session active]", url));
    }
    lines
}

fn build_doi_source_yaml(
    paper: &PaperResult,
    doi: &str,
    bibtex_key: Option<&str>,
    source_url: Option<&str>,
    retrieved: &str,
) -> String {
    let authors_str = paper.authors.join(" and ").replace('"', "\\\"");
    let title = paper.title.replace('"', "\\\"");
    let mut yaml = format!("title: \"{}\"\nauthors: \"{}\"\nyear: {}\ndoi: \"{}\"\n", title, authors_str, paper.year, doi);
    if let Some(key) = bibtex_key {
        yaml.push_str(&format!("bibtex_key: \"{}\"\n", key.replace('"', "\\\"")));
    }
    if let Some(url) = source_url {
        yaml.push_str(&format!("source_url: \"{}\"\n", url.replace('"', "\\\"")));
    }
    yaml.push_str(&format!("retrieved: \"{}\"\n", retrieved));
    yaml
}

/// Download arXiv LaTeX source tarball.
async fn run_source(
    ctx: &Context,
    input: &str,
    dir_override: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let arxiv_id = normalize_arxiv(input);
    let url = format!("https://arxiv.org/e-print/{}", arxiv_id);

    format::info(&format!("Looking up metadata for arXiv:{}", arxiv_id));
    let paper = fetch_metadata(ctx, &arxiv_id).await?;

    let dir_name = match dir_override {
        Some(d) => d.to_path_buf(),
        None => {
            let slug = generate_dir_name(&paper);
            let base = default_output_dir();
            base.join(slug)
        }
    };

    format::info(&format!("Output directory: {}", dir_name.display()));
    std::fs::create_dir_all(&dir_name)?;

    let safe_id = arxiv_id.replace('/', "_");
    let tarball = dir_name.join(format!("{}.tar.gz", safe_id));

    format::info(&format!("Downloading arXiv source: {}", arxiv_id));
    let download_client = reqwest::Client::builder()
        .use_rustls_tls()
        .timeout(Duration::from_secs(DOWNLOAD_TIMEOUT_SECS))
        .user_agent("lit/1.0")
        .build()?;

    let resp = download_client.get(&url).send().await?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {} for {}", resp.status(), url).into());
    }
    let bytes = resp.bytes().await?;
    std::fs::write(&tarball, &bytes)?;

    if !tarball.exists() {
        return Err("Download failed".into());
    }

    format::info("Extracting source tarball...");
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(&dir_name)
        .status();

    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            format::warn(&format!("tar exited with status {}", s));
            format::info("Retrying as gzipped file...");
            let gunzip_status = Command::new("gunzip")
                .arg("-f")
                .arg(&tarball)
                .status();
            if let Ok(gs) = gunzip_status {
                if !gs.success() {
                    format::warn("gunzip also failed; tarball may be in an unexpected format");
                }
            }
        }
        Err(e) => return Err(format!("failed to run tar: {}", e).into()),
    }

    let today = today_string();
    let yaml = build_source_yaml(&paper, &arxiv_id, &today);
    let yaml_path = dir_name.join("source.yaml");
    std::fs::write(&yaml_path, &yaml)?;
    format::info(&format!("Wrote {}", yaml_path.display()));

    if tarball.exists() {
        std::fs::remove_file(&tarball)?;
        format::info("Cleaned up tarball");
    }

    println!("Title: {}", paper.title);
    let first_author = paper.authors.first().map(|s| s.as_str()).unwrap_or("?");
    println!("Authors: {} et al.", first_author);
    println!("Year: {}", paper.year);
    println!("Directory: {}", dir_name.display());

    Ok(())
}

// -- Helpers (moved from source.rs) -------------------------------------------

fn default_output_dir() -> PathBuf {
    if let Ok(cwd) = std::env::current_dir() {
        let mut dir = cwd.as_path();
        loop {
            let candidate = dir.join("etc/pdf");
            if candidate.is_dir() {
                return candidate;
            }
            match dir.parent() {
                Some(p) => dir = p,
                None => break,
            }
        }
    }
    PathBuf::from("etc/pdf")
}

async fn fetch_metadata(ctx: &Context, arxiv_id: &str) -> Result<PaperResult, Box<dyn std::error::Error>> {
    let url = arxiv::query_url(arxiv_id);
    let client = ctx.client();
    let cache_key = db::Db::cache_key("arxiv", arxiv_id);
    let body = client.get_cached_deferred(&cache_key, &url, db::TTL_DOI).await?;
    let result = arxiv::parse_entry(&body)?;
    client.cache_set(&cache_key, &url, &body);
    Ok(result)
}

fn generate_dir_name(paper: &PaperResult) -> String {
    let lastname = extract_lastname(&paper.authors);
    let slug = extract_title_slug(&paper.title);
    format!("{}{}{}", lastname, paper.year, slug)
}

fn extract_lastname(authors: &[String]) -> String {
    if let Some(first) = authors.first() {
        let last = extract_last_name(first.trim());
        last.chars()
            .filter(|c| c.is_alphabetic())
            .collect::<String>()
            .to_lowercase()
    } else {
        "unknown".to_string()
    }
}

fn extract_title_slug(title: &str) -> String {
    title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .find(|w| !SKIP_WORDS.contains(&w.as_str()) && w.len() > 2)
        .unwrap_or_default()
}

fn build_source_yaml(paper: &PaperResult, arxiv_id: &str, retrieved: &str) -> String {
    let authors_str = paper.authors.join(" and ");
    let title = paper.title.replace('"', "\\\"");
    let authors = authors_str.replace('"', "\\\"");

    let mut yaml = String::new();
    yaml.push_str(&format!("title: \"{}\"\n", title));
    yaml.push_str(&format!("authors: \"{}\"\n", authors));
    yaml.push_str(&format!("year: {}\n", paper.year));
    yaml.push_str(&format!("arxiv: \"{}\"\n", arxiv_id));
    yaml.push_str(&format!("retrieved: \"{}\"\n", retrieved));
    yaml
}

/// Walk up from cwd looking for `.cache/lit/clio/cookies.txt`.
///
/// Returns the path when found, or `None` when the filesystem root is reached.
fn find_cookie_file() -> Option<std::path::PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let mut dir = cwd.as_path();
    loop {
        let candidate = dir.join(".cache/lit/clio/cookies.txt");
        if candidate.exists() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
}

fn today_string() -> String {
    let now = std::time::SystemTime::now();
    let since_epoch = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = since_epoch / 86400;
    let (year, month, day) = days_to_ymd(days);
    format!("{:04}-{:02}-{:02}", year, month, day)
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_lastname_basic() {
        let authors = vec!["Jonathan Ho".to_string()];
        assert_eq!(extract_lastname(&authors), "ho");
    }

    #[test]
    fn test_extract_lastname_hyphen() {
        let authors = vec!["Amir-Hossein Karimi".to_string()];
        assert_eq!(extract_lastname(&authors), "karimi");
    }

    #[test]
    fn test_extract_lastname_empty() {
        let authors: Vec<String> = vec![];
        assert_eq!(extract_lastname(&authors), "unknown");
    }

    #[test]
    fn test_extract_title_slug_basic() {
        assert_eq!(extract_title_slug("Proximal Policy Optimization Algorithms"), "proximal");
    }

    #[test]
    fn test_extract_title_slug_skip_article() {
        assert_eq!(extract_title_slug("The Art of Reasoning"), "art");
    }

    #[test]
    fn test_extract_title_slug_all_skip() {
        assert_eq!(extract_title_slug("of the and in on"), "");
    }

    #[test]
    fn test_generate_dir_name() {
        let paper = PaperResult {
            title: "Hindsight Credit Assignment".to_string(),
            authors: vec!["Anna Harutyunyan".to_string()],
            year: "2019".to_string(),
            ..Default::default()
        };
        assert_eq!(generate_dir_name(&paper), "harutyunyan2019hindsight");
    }

    #[test]
    fn test_generate_dir_name_skip_articles() {
        let paper = PaperResult {
            title: "The Art of Something".to_string(),
            authors: vec!["John Smith".to_string()],
            year: "2021".to_string(),
            ..Default::default()
        };
        assert_eq!(generate_dir_name(&paper), "smith2021art");
    }

    #[test]
    fn test_generate_dir_name_no_slug() {
        let paper = PaperResult {
            title: "of the and".to_string(),
            authors: vec!["Jane Doe".to_string()],
            year: "2020".to_string(),
            ..Default::default()
        };
        assert_eq!(generate_dir_name(&paper), "doe2020");
    }

    #[test]
    fn test_build_source_yaml() {
        let paper = PaperResult {
            title: "Hindsight Credit Assignment".to_string(),
            authors: vec!["Anna Harutyunyan".to_string(), "Will Dabney".to_string()],
            year: "2019".to_string(),
            ..Default::default()
        };
        let yaml = build_source_yaml(&paper, "1912.02503", "2026-03-01");
        assert!(yaml.contains("title: \"Hindsight Credit Assignment\""));
        assert!(yaml.contains("authors: \"Anna Harutyunyan and Will Dabney\""));
        assert!(yaml.contains("year: 2019"));
        assert!(yaml.contains("arxiv: \"1912.02503\""));
        assert!(yaml.contains("retrieved: \"2026-03-01\""));
    }

    fn tier(label: &'static str, url: &str) -> Tier {
        Tier { label, url: url.to_string(), cookies: None }
    }

    /// Fetcher that records every URL it is asked for and succeeds only for `winner`.
    fn recording_fetcher<'a>(
        attempts: &'a std::sync::Mutex<Vec<String>>,
        winner: Option<&'a str>,
    ) -> impl Fn(String, Option<PathBuf>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<u8>, FetchError>> + 'a>> + 'a
    {
        move |url, _cookies| {
            Box::pin(async move {
                attempts.lock().unwrap().push(url.clone());
                match winner {
                    Some(w) if w == url => Ok(b"%PDF-1.4".to_vec()),
                    _ => Err(FetchError::NotPdf { url, bytes: 0 }),
                }
            })
        }
    }

    #[tokio::test]
    async fn test_fetch_first_success_reports_delivering_tier() {
        let attempts = std::sync::Mutex::new(Vec::new());
        let tiers = vec![
            tier("open-access", "https://oa.example.com/paper.pdf"),
            tier("EZProxy", "https://ez.example.com/paper.pdf"),
        ];
        let got = fetch_first_success(&tiers, recording_fetcher(&attempts, Some("https://ez.example.com/paper.pdf"))).await;
        let (_, source_url) = got.expect("EZProxy tier should deliver bytes");
        assert_eq!(source_url, "https://ez.example.com/paper.pdf");
    }

    #[tokio::test]
    async fn test_fetch_first_success_tries_clio_after_failed_open_access() {
        let attempts = std::sync::Mutex::new(Vec::new());
        let tiers = vec![
            tier("open-access", "https://oa.example.com/dead.pdf"),
            tier("Clio", "https://clio.example.com/paper.pdf"),
            tier("EZProxy", "https://ez.example.com/paper.pdf"),
        ];
        let got = fetch_first_success(&tiers, recording_fetcher(&attempts, Some("https://clio.example.com/paper.pdf"))).await;
        let (_, source_url) = got.expect("Clio tier should deliver bytes");
        assert_eq!(source_url, "https://clio.example.com/paper.pdf");
        assert_eq!(
            *attempts.lock().unwrap(),
            vec!["https://oa.example.com/dead.pdf", "https://clio.example.com/paper.pdf"],
            "a dead open-access link must not skip the Clio tier"
        );
    }

    #[tokio::test]
    async fn test_fetch_first_success_stops_at_first_delivering_tier() {
        let attempts = std::sync::Mutex::new(Vec::new());
        let tiers = vec![
            tier("open-access", "https://oa.example.com/paper.pdf"),
            tier("EZProxy", "https://ez.example.com/paper.pdf"),
        ];
        let got = fetch_first_success(&tiers, recording_fetcher(&attempts, Some("https://oa.example.com/paper.pdf"))).await;
        assert_eq!(got.unwrap().1, "https://oa.example.com/paper.pdf");
        assert_eq!(*attempts.lock().unwrap(), vec!["https://oa.example.com/paper.pdf"]);
    }

    #[tokio::test]
    async fn test_fetch_first_success_none_when_every_tier_fails() {
        let attempts = std::sync::Mutex::new(Vec::new());
        let tiers = vec![
            tier("open-access", "https://oa.example.com/dead.pdf"),
            tier("EZProxy", "https://ez.example.com/dead.pdf"),
        ];
        let got = fetch_first_success(&tiers, recording_fetcher(&attempts, None)).await;
        assert!(got.is_err());
        assert_eq!(attempts.lock().unwrap().len(), 2, "every tier must be tried");
    }

    #[tokio::test]
    async fn test_url_only_lists_exactly_the_urls_the_fetch_path_tries() {
        let cookies = PathBuf::from("/nonexistent/cookies.txt");
        let tiers = build_tiers(
            Some("https://oa.example.com/paper.pdf"),
            Some("https://clio.example.com/paper.pdf"),
            Some("https://ez.example.com/paper.pdf"),
            Some(&cookies),
        );

        let attempts = std::sync::Mutex::new(Vec::new());
        let got = fetch_first_success(&tiers, recording_fetcher(&attempts, None)).await;

        assert!(got.is_err());
        assert_eq!(
            tier_urls(&tiers),
            *attempts.lock().unwrap(),
            "--url-only must report the same candidates, in the same order, that the fetch path tries"
        );
    }

    #[test]
    fn test_build_tiers_orders_open_access_then_clio_then_ezproxy() {
        let cookies = PathBuf::from("/nonexistent/cookies.txt");
        let tiers = build_tiers(
            Some("https://oa.example.com/paper.pdf"),
            Some("https://clio.example.com/paper.pdf"),
            Some("https://ez.example.com/paper.pdf"),
            Some(&cookies),
        );
        assert_eq!(
            tier_urls(&tiers),
            vec![
                "https://oa.example.com/paper.pdf",
                "https://clio.example.com/paper.pdf",
                "https://ez.example.com/paper.pdf",
            ]
        );
    }

    #[test]
    fn test_build_tiers_omits_ezproxy_without_cookies() {
        let tiers = build_tiers(
            None,
            Some("https://clio.example.com/paper.pdf"),
            Some("https://ez.example.com/paper.pdf"),
            None,
        );
        assert_eq!(tier_urls(&tiers), vec!["https://clio.example.com/paper.pdf"]);
    }

    #[test]
    fn test_build_doi_source_yaml_records_given_source_url() {
        let paper = PaperResult {
            title: "A Paper".to_string(),
            authors: vec!["Jane Doe".to_string()],
            year: "2024".to_string(),
            ..Default::default()
        };
        let yaml = build_doi_source_yaml(
            &paper,
            "10.1234/test",
            Some("doe2024paper"),
            Some("https://ez.example.com/paper.pdf"),
            "2026-03-01",
        );
        assert!(yaml.contains("source_url: \"https://ez.example.com/paper.pdf\""));
        assert!(yaml.contains("bibtex_key: \"doe2024paper\""));
    }

    /// Write `bytes` into a fresh temporary directory and return the directory
    /// plus a `file://` URL naming the file, so a fetch can be exercised
    /// without network access.
    fn served_file(name: &str, bytes: &[u8]) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        let url = format!("file://{}", path.display());
        (dir, url)
    }

    /// Every entry under `dir`, sorted, as a comparable listing.
    fn listing(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn test_curl_args_name_no_output_file() {
        let args = curl_args("https://example.com/paper.pdf", Some("/cookies.txt"));
        for flag in ["-o", "--output", "-O", "--remote-name"] {
            assert!(!args.iter().any(|a| a == flag), "curl must not be told to write a file: {}", flag);
        }
        assert_eq!(args.last().unwrap(), "https://example.com/paper.pdf");
        assert!(args.windows(2).any(|w| w[0] == "-b" && w[1] == "/cookies.txt"));
    }

    #[tokio::test]
    async fn test_fetch_pdf_via_curl_returns_bytes_without_writing_a_file() {
        let (dir, url) = served_file("paper.pdf", b"%PDF-1.4 body");
        let before = listing(dir.path());

        let got = fetch_pdf_via_curl(&url, None).await.expect("a PDF must be delivered");

        assert_eq!(got, b"%PDF-1.4 body");
        assert_eq!(listing(dir.path()), before, "the fetch must create no file beside the source");
        assert!(
            !std::path::Path::new(&format!("/tmp/lit_dl_{}.pdf", std::process::id())).exists(),
            "the fetch must not stage bytes through a scratch file"
        );
    }

    #[tokio::test]
    async fn test_fetch_pdf_via_curl_reports_a_non_pdf_body_as_not_a_pdf() {
        let html = b"<html><body>Sign in to continue</body></html>";
        let (_dir, url) = served_file("login.html", html);

        let err = fetch_pdf_via_curl(&url, None).await.expect_err("HTML is not a PDF");

        match err {
            FetchError::NotPdf { bytes, .. } => assert_eq!(bytes, html.len()),
            other => panic!("a delivered non-PDF must not be reported as a transport failure: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_fetch_pdf_via_curl_preserves_the_transport_failure_cause() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("file://{}", dir.path().join("absent.pdf").display());

        let err = fetch_pdf_via_curl(&url, None).await.expect_err("an unreadable URL must fail");

        match err {
            FetchError::Transport { code, message, .. } => {
                assert_eq!(code, Some(37), "curl's exit code must survive");
                assert!(message.contains("absent.pdf"), "curl's stderr must survive: {}", message);
            }
            other => panic!("a failed fetch must not be reported as a delivered non-PDF: {:?}", other),
        }
    }

    #[test]
    fn test_failure_report_claims_no_open_access_pdf_only_when_that_happened() {
        let transport = vec![FetchError::Transport {
            url: "https://clio.example.com/paper.pdf".to_string(),
            code: Some(56),
            message: "Failure writing output".to_string(),
        }];
        let lines = failure_report(&transport, None, Some("https://clio.example.com/paper.pdf"), None);
        assert!(
            !lines.iter().any(|l| l.contains("No open-access PDF found")),
            "a transport failure says nothing about whether the paper is open access: {:?}",
            lines
        );
        assert!(lines.iter().any(|l| l.contains("56") && l.contains("Failure writing output")));
    }

    #[test]
    fn test_failure_report_claims_no_open_access_pdf_when_every_candidate_answered() {
        let not_pdf = vec![FetchError::NotPdf {
            url: "https://clio.example.com/paper.pdf".to_string(),
            bytes: 42,
        }];
        let lines = failure_report(&not_pdf, None, None, None);
        assert!(lines.iter().any(|l| l.contains("No open-access PDF found")), "{:?}", lines);
    }

    #[test]
    fn test_days_to_ymd_epoch() {
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
    }

    #[test]
    fn test_days_to_ymd_known_date() {
        assert_eq!(days_to_ymd(20513), (2026, 3, 1));
    }

    #[test]
    fn test_today_string_format() {
        let s = today_string();
        assert_eq!(s.len(), 10);
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[7..8], "-");
    }
}
