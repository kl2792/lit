/// `lit download <id>` -- Download PDF or arXiv LaTeX source.
///
/// Default: find open-access PDF via Unpaywall and print URL.
/// `--source`: download arXiv LaTeX source tarball, extract, write source.yaml.
/// `--url-only`: print the candidate URLs, in tier order, without downloading.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::artifact::{claim_dir, classify, yaml_paper_ids, Claim, Target};
use super::check::{is_confirmed, yaml_field};
use super::metadata::{arxiv_metadata, Metadata, MetadataFetch};
use super::Context;
use crate::api::{extract_last_name, unpaywall, PaperResult};
use crate::citekey::SKIP_WORDS;
use crate::db;
use crate::detect::normalize_arxiv;
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
        run_source(ctx, input, dir_override).await?;
    } else {
        run_pdf(ctx, input, url_only, citekey).await?;
    }
    Ok(())
}

/// Fetch `id` into its artifact directory, an arXiv preprint's LaTeX source
/// or a DOI's PDF, and return that directory. Fails when nothing arrived.
pub(crate) async fn fetch(ctx: &Context, id: &Target) -> Result<PathBuf, Box<dyn std::error::Error>> {
    match id {
        Target::Arxiv(arxiv_id) => run_source(ctx, arxiv_id, None).await,
        Target::Doi(doi) => Ok(run_doi_pdf(ctx, doi, false, None).await?.ok_or("a DOI fetch listed URLs instead of saving")?),
    }
}

/// Download a PDF to etc/pdf/<citekey>/, from arXiv for an arXiv preprint and
/// through the DOI tiers otherwise. Returns the directory written, or `None`
/// when `url_only` printed the candidate URLs instead.
async fn run_pdf(ctx: &Context, input: &str, url_only: bool, citekey: Option<&str>) -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
    match classify(input) {
        Target::Arxiv(id) => run_arxiv_pdf(ctx, &id, url_only, citekey).await,
        Target::Doi(doi) => run_doi_pdf(ctx, &doi, url_only, citekey).await,
    }
}

/// Download an arXiv preprint's PDF from arXiv, with metadata from the arXiv
/// API or its fallbacks. Missing metadata does not stop the download.
async fn run_arxiv_pdf(ctx: &Context, arxiv_id: &str, url_only: bool, citekey: Option<&str>) -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
    let tiers = arxiv_tiers(arxiv_id);
    if url_only {
        for url in tier_urls(&tiers) {
            println!("{}", url);
        }
        return Ok(None);
    }

    let meta = fetch_metadata(&ctx.client(), arxiv_id).await;
    if let Some(m) = &meta {
        format::info(&format!("Title: {}", m.paper.title));
    }

    let (data, delivered_by) = fetch_tiers(&tiers)
        .await
        .map_err(|failures| failure_report(&failures, Some(&tiers[0].url), None, None).join("\n"))?;
    let yaml = build_source_yaml(meta.as_ref(), arxiv_id, citekey, Some(&delivered_by), &today_string());
    let id = Target::Arxiv(arxiv_id.to_string());
    save_pdf(pdf_dir(citekey, || arxiv_slug(meta.as_ref(), arxiv_id), &id)?, &data, yaml).map(Some)
}

/// Find open-access PDF via Unpaywall, with S2 and OpenAlex fallbacks.
/// Resolves in tier order (open access, Clio, EZProxy) and downloads the first
/// tier that delivers bytes to etc/pdf/<citekey>/. Returns the directory
/// written, or `None` when `url_only` printed the candidate URLs instead.
async fn run_doi_pdf(ctx: &Context, doi: &str, url_only: bool, citekey: Option<&str>) -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
    use crate::api::{openalex as oa_api, semantic_scholar as s2_api};

    let doi = doi.to_string();
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
    // Whether any provider answered for this DOI, recorded before the records
    // themselves are consumed, so the artifact can state it rather than leave
    // `check` to guess from a missing field later.
    let uw_answered = uw_result.is_some();
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
        return Ok(None);
    }

    format::info(&format!("Title: {}", title));

    // Semantic Scholar's record when it answered, otherwise one assembled from
    // what the other lookups returned, which is metadata only if one did.
    let confirmed = s2_result.is_some() || uw_answered;
    let meta = s2_result.unwrap_or_else(|| PaperResult {
        title: title.clone(),
        doi: if doi.is_empty() { None } else { Some(doi.clone()) },
        ..Default::default()
    });

    let (data, delivered_by) = fetch_tiers(&tiers).await.map_err(|failures| {
        failure_report(&failures, pdf_url.as_deref(), clio_url.as_deref(), ez_url.as_deref()).join("\n")
    })?;
    // Provenance is the tier that produced these bytes, not the first tier
    // that merely had a candidate URL.
    let yaml = build_doi_source_yaml(&meta, &doi, citekey, Some(delivered_by.as_str()), &today_string(), confirmed);
    let id = Target::Doi(doi.clone());
    save_pdf(pdf_dir(citekey, || generate_dir_name(&meta), &id)?, &data, yaml).map(Some)
}

/// Fetch the tiers in order with curl.
///
/// A tier is tried whenever the ones before it returned no bytes, so a dead
/// link in one tier cannot disable a later one.
async fn fetch_tiers(tiers: &[Tier]) -> Result<(Vec<u8>, String), Vec<FetchError>> {
    fetch_first_success(tiers, |url, cookies| async move {
        fetch_pdf_via_curl(&url, cookies.as_deref()).await
    })
    .await
}

/// The directory a PDF download of `id` writes: etc/pdf/<citekey> when the
/// caller names a citekey, else the one `claim_dir` picks from `slug`.
fn pdf_dir(citekey: Option<&str>, slug: impl FnOnce() -> String, id: &Target) -> Result<Claim, Box<dyn std::error::Error>> {
    let base = crate::paths::artifact_dir()?;
    Ok(match citekey {
        Some(key) => Claim::at(&base.join(key))?,
        None => claim_dir(&base, &slug(), id)?,
    })
}

/// Write `paper.pdf`, its `pdftotext` extraction, and `source.yaml` to the
/// claimed directory, and return it.
fn save_pdf(claim: Claim, data: &[u8], yaml: String) -> Result<PathBuf, Box<dyn std::error::Error>> {
    write_pdf(claim.dir(), data)?;
    write_source_yaml(claim.dir(), yaml)?;
    format::info(&format!("Saved: {} ({}KB)", claim.dir().display(), data.len() / 1024));
    Ok(claim.dir().to_path_buf())
}

/// Write `data` as `dir/paper.pdf` and its `pdftotext` extraction as
/// `dir/paper.txt`; a failed extraction leaves the PDF for `lit read` to retry.
fn write_pdf(dir: &Path, data: &[u8]) -> std::io::Result<()> {
    let pdf_path = dir.join("paper.pdf");
    std::fs::write(&pdf_path, data)?;
    let _ = Command::new("pdftotext").arg(&pdf_path).arg(dir.join("paper.txt")).status();
    Ok(())
}

/// Write `yaml` as `dir/source.yaml`, merged with the record already there.
fn write_source_yaml(dir: &Path, yaml: String) -> std::io::Result<PathBuf> {
    let path = dir.join("source.yaml");
    let old = std::fs::read_to_string(&path).ok();
    std::fs::write(&path, merge_source_yaml(old.as_deref(), yaml))?;
    Ok(path)
}

/// The arXiv identifier `--source` fetches: the one `classify` finds, so an
/// arXiv DOI works here as it does for the PDF, and otherwise the input read as
/// an arXiv identifier, since `--source` accepts nothing else.
fn source_arxiv_id(input: &str) -> String {
    match classify(input) {
        Target::Arxiv(id) => id,
        Target::Doi(_) => normalize_arxiv(input),
    }
}

/// The tiers for an arXiv preprint: arXiv's own PDF, which is open access.
fn arxiv_tiers(arxiv_id: &str) -> Vec<Tier> {
    vec![Tier {
        label: "arXiv PDF",
        url: format!("https://arxiv.org/pdf/{}", arxiv_id),
        cookies: None,
    }]
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

/// The lines of the error a download fails with when no tier delivered a PDF.
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

/// Build `source.yaml` for a DOI download.
///
/// `confirmed` says whether `paper` is a provider's record for this DOI rather
/// than a stand-in assembled from the DOI itself. `lit check` reads the flag
/// instead of re-deriving the answer from a missing title.
fn build_doi_source_yaml(
    paper: &PaperResult,
    doi: &str,
    bibtex_key: Option<&str>,
    source_url: Option<&str>,
    retrieved: &str,
    confirmed: bool,
) -> String {
    let authors_str = paper.authors.join(" and ").replace('"', "\\\"");
    let title = paper.title.replace('"', "\\\"");
    let mut yaml = format!("title: \"{}\"\nauthors: \"{}\"\nyear: {}\ndoi: \"{}\"\n", title, authors_str, paper.year, doi);
    push_provenance(&mut yaml, bibtex_key, source_url);
    if confirmed {
        yaml.push_str("metadata_confirmed: true\n");
    }
    yaml.push_str(&format!("retrieved: \"{}\"\n", retrieved));
    yaml
}

/// Append the citekey and the URL that delivered the bytes, when known.
fn push_provenance(yaml: &mut String, bibtex_key: Option<&str>, source_url: Option<&str>) {
    if let Some(key) = bibtex_key {
        yaml.push_str(&format!("bibtex_key: \"{}\"\n", key.replace('"', "\\\"")));
    }
    if let Some(url) = source_url {
        yaml.push_str(&format!("source_url: \"{}\"\n", url.replace('"', "\\\"")));
    }
}

/// Download an arXiv preprint's LaTeX source into its artifact directory and
/// return that directory.
async fn run_source(
    ctx: &Context,
    input: &str,
    dir_override: Option<&Path>,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let arxiv_id = source_arxiv_id(input);

    let (meta, bytes) = fetch_source(&ctx.client(), &arxiv_id, download_bytes).await?;

    let claim = match dir_override {
        Some(dir) => Claim::at(dir)?,
        None => claim_dir(
            &crate::paths::artifact_dir()?,
            &arxiv_slug(meta.as_ref(), &arxiv_id),
            &Target::Arxiv(arxiv_id.clone()),
        )?,
    };
    format::info(&format!("Output directory: {}", claim.dir().display()));

    // source.yaml is written only after the e-print is in place, so a failed
    // fetch never leaves a directory that holds metadata and nothing to read.
    place_source(claim.dir(), &arxiv_id, &bytes)?;
    let yaml = build_source_yaml(meta.as_ref(), &arxiv_id, None, Some(&eprint_url(&arxiv_id)), &today_string());
    let yaml_path = write_source_yaml(claim.dir(), yaml)?;
    format::info(&format!("Wrote {}", yaml_path.display()));

    if let Some(Metadata { paper, .. }) = &meta {
        let first_author = paper.authors.first().map(|s| s.as_str()).unwrap_or("?");
        format::info(&format!("Title: {}\nAuthors: {} et al.\nYear: {}", paper.title, first_author, paper.year));
    }
    Ok(claim.dir().to_path_buf())
}

/// Unpack an arXiv e-print into a staging sibling of `dir`, then move its
/// entries into `dir`, replacing same-named ones.
///
/// The staging directory carries this process's id, so two processes
/// fetching one paper never share or delete each other's files. A failed
/// unpack deletes only the staging directory: `dir`, with its `source.yaml`
/// and any hand-attached files, is untouched. Files in `dir` that the e-print
/// does not contain are kept.
fn place_source(dir: &Path, arxiv_id: &str, bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let staging = super::misc::fresh_staging(dir);
    std::fs::create_dir(&staging)?;
    let result = unpack_source(&staging, arxiv_id, bytes).and_then(|()| Ok(merge_dir(&staging, dir)?));
    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// Move every entry of `from` into `to`, replacing an entry of the same name.
fn merge_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        match std::fs::symlink_metadata(&target) {
            Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&target)?,
            Ok(_) => std::fs::remove_file(&target)?,
            Err(_) => {}
        }
        std::fs::rename(entry.path(), &target)?;
    }
    Ok(())
}

/// Unpack an arXiv e-print into `dir`.
///
/// arXiv serves a gzipped tarball for a multi-file submission, a gzipped
/// single file for a one-file submission (saved as `<id>.tex`), and the PDF
/// itself for a PDF-only submission (saved as `paper.pdf` and `paper.txt`).
/// Any other payload is an error, so no metadata-only directory results.
///
/// The payload is classified by content rather than by whether `tar`
/// succeeds, because bsdtar (macOS) reads plain text as an mtree spec and
/// "extracts" junk files from a gzipped single .tex file.
fn unpack_source(dir: &Path, arxiv_id: &str, bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let staged = dir.join(format!("{}.eprint", arxiv_id.replace('/', "_")));
    std::fs::write(&staged, bytes)?;
    let result = unpack_staged(dir, &staged, arxiv_id, bytes);
    if staged.exists() {
        std::fs::remove_file(&staged)?;
    }
    result
}

/// `unpack_source` for an e-print already written to `staged`.
fn unpack_staged(dir: &Path, staged: &Path, arxiv_id: &str, bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let gunzipped = Command::new("gzip")
        .arg("-dc")
        .arg(staged)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| out.stdout);
    let payload = gunzipped.as_deref().unwrap_or(bytes);

    if payload.starts_with(b"%PDF") {
        format::info("e-print is a PDF (PDF-only submission)");
        return Ok(write_pdf(dir, payload)?);
    }
    if is_tar(payload) {
        format::info("Extracting source tarball...");
        std::fs::write(staged, payload)?;
        let status = Command::new("tar").arg("-xf").arg(staged).arg("-C").arg(dir).status()?;
        if !status.success() {
            return Err(format!("tar exited with {} on the e-print for {}", status, arxiv_id).into());
        }
        return Ok(());
    }
    if gunzipped.is_some() {
        format::info("e-print is a single gzipped file; saving it as .tex");
        std::fs::write(dir.join(format!("{}.tex", arxiv_id.replace('/', "_"))), payload)?;
        return Ok(());
    }
    Err(format!("arXiv e-print for {} is neither a PDF, a tarball nor a gzipped file", arxiv_id).into())
}

/// Whether `payload` starts with a tar header: its checksum field equals the
/// byte sum of the header with that field read as spaces. Every tar format
/// has this checksum, including pre-POSIX (v7) archives without the `ustar`
/// magic, while text such as a single .tex file has odds of matching it that
/// are negligible.
fn is_tar(payload: &[u8]) -> bool {
    let Some(header) = payload.get(..512) else { return false };
    let field = String::from_utf8_lossy(&header[148..156]);
    let Ok(recorded) = u32::from_str_radix(field.trim_matches(|c: char| c == '\0' || c == ' '), 8) else {
        return false;
    };
    let sum: u32 = header
        .iter()
        .enumerate()
        .map(|(i, &b)| if (148..156).contains(&i) { u32::from(b' ') } else { u32::from(b) })
        .sum();
    recorded == sum
}

/// The keys of `source.yaml` that describe the fetch that produced the bytes
/// on disk, rather than the paper.
const FETCH_KEYS: [&str; 2] = ["source_url", "retrieved"];

/// The `source.yaml` to write over `old`, if one exists.
///
/// When both records name the same paper, a record without provider-confirmed
/// metadata does not replace one with it, and the old record's `bibtex_key`
/// carries over when the kept record has none, so a refetch never loses what
/// an earlier fetch or `lit check --fix` wrote. The fetch keys always come
/// from `new`, since the bytes now on disk are the ones this fetch delivered.
fn merge_source_yaml(old: Option<&str>, new: String) -> String {
    let Some(old) = old.filter(|old| {
        let new_ids = yaml_paper_ids(&new);
        yaml_paper_ids(old).iter().any(|id| new_ids.contains(id))
    }) else {
        return new;
    };
    let is_fetch_line = |line: &&str| line.split_once(':').is_some_and(|(key, _)| FETCH_KEYS.contains(&key.trim()));
    let mut merged = if is_confirmed(old) && !is_confirmed(&new) {
        let kept = old.lines().filter(|line| !is_fetch_line(line));
        kept.chain(new.lines().filter(is_fetch_line)).map(|line| format!("{}\n", line)).collect()
    } else {
        new.clone()
    };
    if yaml_field(&merged, "bibtex_key").is_none() {
        if let Some(key) = yaml_field(old, "bibtex_key").or_else(|| yaml_field(&new, "bibtex_key")) {
            merged.push_str(&format!("bibtex_key: \"{}\"\n", key));
        }
    }
    merged
}

// -- Helpers (moved from source.rs) -------------------------------------------

/// arXiv's LaTeX source endpoint for `arxiv_id`.
fn eprint_url(arxiv_id: &str) -> String {
    format!("https://arxiv.org/e-print/{}", arxiv_id)
}

/// Resolve metadata, then fetch the e-print with `get_bytes` whether or not
/// metadata resolved: the e-print host is separate from the metadata APIs and
/// often still serves while they rate-limit.
async fn fetch_source<F, G, Fut>(
    f: &F,
    arxiv_id: &str,
    get_bytes: G,
) -> Result<(Option<Metadata>, Vec<u8>), Box<dyn std::error::Error>>
where
    F: MetadataFetch,
    G: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, Box<dyn std::error::Error>>>,
{
    let meta = fetch_metadata(f, arxiv_id).await;
    format::info(&format!("Downloading arXiv source: {}", arxiv_id));
    let bytes = get_bytes(eprint_url(arxiv_id)).await?;
    Ok((meta, bytes))
}

/// GET `url` and return its body bytes; non-2xx is an error.
async fn download_bytes(url: String) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .timeout(Duration::from_secs(DOWNLOAD_TIMEOUT_SECS))
        .user_agent("lit/1.0")
        .build()?;
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {} for {}", resp.status(), url).into());
    }
    Ok(resp.bytes().await?.to_vec())
}

/// Metadata for `arxiv_id` from the arXiv API or its fallbacks, or `None`
/// (with a warning) when every source fails, so the caller can still download.
async fn fetch_metadata<F: MetadataFetch>(f: &F, arxiv_id: &str) -> Option<Metadata> {
    format::info(&format!("Looking up metadata for arXiv:{}", arxiv_id));
    match arxiv_metadata(f, arxiv_id).await {
        Ok(m) => Some(m),
        Err(e) => {
            format::warn(&format!("warning: {}; downloading without metadata", e));
            None
        }
    }
}

/// Artifact directory name: `<lastname><year><titleword>` from metadata, or
/// the arXiv id (slashes replaced) when no metadata is available.
fn arxiv_slug(meta: Option<&Metadata>, arxiv_id: &str) -> String {
    match meta {
        Some(m) => generate_dir_name(&m.paper),
        None => arxiv_id.replace('/', "_"),
    }
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

/// Build `source.yaml` for an arXiv download.
///
/// With `meta`, the record is a provider's answer for `arxiv_id`, so the
/// artifact is confirmed and names that provider. Without it, only the id and
/// provenance are written, so nothing bibliographic is invented and
/// `lit check --fix` can fill the record later.
fn build_source_yaml(
    meta: Option<&Metadata>,
    arxiv_id: &str,
    bibtex_key: Option<&str>,
    source_url: Option<&str>,
    retrieved: &str,
) -> String {
    let mut yaml = String::new();
    if let Some(Metadata { paper, .. }) = meta {
        yaml.push_str(&format!("title: \"{}\"\n", paper.title.replace('"', "\\\"")));
        yaml.push_str(&format!("authors: \"{}\"\n", paper.authors.join(" and ").replace('"', "\\\"")));
        yaml.push_str(&format!("year: {}\n", paper.year));
    }
    yaml.push_str(&format!("arxiv: \"{}\"\n", arxiv_id));
    push_provenance(&mut yaml, bibtex_key, source_url);
    if let Some(m) = meta {
        yaml.push_str("metadata_confirmed: true\n");
        yaml.push_str(&format!("metadata_source: \"{}\"\n", m.source.id()));
    }
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
    use crate::cmd::metadata::MetadataSource;
    use crate::cmd::metadata::tests::MockMetadataFetch;
    use std::cell::RefCell;

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
        let meta = Metadata { paper, source: MetadataSource::Arxiv };
        let yaml = build_source_yaml(Some(&meta), "1912.02503", None, None, "2026-03-01");
        assert!(yaml.contains("title: \"Hindsight Credit Assignment\""));
        assert!(yaml.contains("authors: \"Anna Harutyunyan and Will Dabney\""));
        assert!(yaml.contains("year: 2019"));
        assert!(yaml.contains("arxiv: \"1912.02503\""));
        assert!(yaml.contains("retrieved: \"2026-03-01\""));
        // A provider's record for this id, so `check` need never ask again.
        assert!(yaml.contains("metadata_confirmed: true"));
        assert!(yaml.contains("metadata_source: \"arxiv\""));
        assert!(!yaml.contains("bibtex_key") && !yaml.contains("source_url"));
    }

    #[test]
    fn source_yaml_names_the_fallback_that_supplied_metadata() {
        let paper = PaperResult { title: "A Paper".to_string(), year: "2026".to_string(), ..Default::default() };
        let meta = Metadata { paper, source: MetadataSource::SemanticScholar };
        let yaml = build_source_yaml(Some(&meta), "2609.39243", None, None, "2026-10-05");
        assert!(yaml.contains("metadata_source: \"semantic_scholar\""));
        assert!(yaml.contains("metadata_confirmed: true"));
    }

    #[test]
    fn source_yaml_without_metadata_records_only_the_id() {
        // No provider answered: nothing bibliographic may be written, and the
        // artifact must not claim confirmation, so `check --fix` can fill it later.
        let yaml = build_source_yaml(None, "2609.39243", None, Some("https://arxiv.org/e-print/2609.39243"), "2026-10-05");
        assert!(yaml.contains("arxiv: \"2609.39243\""));
        assert!(yaml.contains("source_url: \"https://arxiv.org/e-print/2609.39243\""));
        for absent in ["title:", "authors:", "year:", "metadata_confirmed", "metadata_source"] {
            assert!(!yaml.contains(absent), "unexpected {} in {}", absent, yaml);
        }
    }

    #[test]
    fn arxiv_slug_falls_back_to_the_id_without_metadata() {
        assert_eq!(arxiv_slug(None, "2609.39243"), "2609.39243");
        assert_eq!(arxiv_slug(None, "hep-th/9901001"), "hep-th_9901001");
        let paper = PaperResult {
            title: "Hindsight Credit Assignment".to_string(),
            authors: vec!["Anna Harutyunyan".to_string()],
            year: "2019".to_string(),
            ..Default::default()
        };
        let meta = Metadata { paper, source: MetadataSource::OpenAlex };
        assert_eq!(arxiv_slug(Some(&meta), "1912.02503"), "harutyunyan2019hindsight");
    }

    /// Byte fetcher that records each URL and returns a fixed tarball.
    fn recording_bytes(urls: &RefCell<Vec<String>>) -> impl FnOnce(String) -> std::future::Ready<Result<Vec<u8>, Box<dyn std::error::Error>>> + '_ {
        move |url| {
            urls.borrow_mut().push(url);
            std::future::ready(Ok(b"tarball".to_vec()))
        }
    }

    const ARXIV_API: &str = "export.arxiv.org/api/query";
    const S2_API: &str = "api.semanticscholar.org";

    #[tokio::test]
    async fn source_download_proceeds_after_fallback_metadata() {
        let f = MockMetadataFetch::new(vec![
            (ARXIV_API, Err("HTTP 429 Too Many Requests".to_string())),
            (S2_API, Ok(r#"{"title": "From S2", "authors": [{"name": "Jane Doe"}], "year": 2026}"#.to_string())),
        ]);
        let urls = RefCell::new(Vec::new());
        let (meta, bytes) = fetch_source(&f, "2609.39243", recording_bytes(&urls)).await.unwrap();
        assert_eq!(meta.unwrap().source, MetadataSource::SemanticScholar);
        assert_eq!(bytes, b"tarball");
        assert_eq!(*urls.borrow(), vec!["https://arxiv.org/e-print/2609.39243"]);
    }

    #[tokio::test]
    async fn source_download_proceeds_when_no_metadata_source_answers() {
        let f = MockMetadataFetch::new(vec![]); // every lookup fails
        let urls = RefCell::new(Vec::new());
        let (meta, _) = fetch_source(&f, "2609.39243", recording_bytes(&urls)).await.unwrap();
        assert!(meta.is_none());
        assert_eq!(*urls.borrow(), vec!["https://arxiv.org/e-print/2609.39243"]);
    }

    #[test]
    fn test_build_source_yaml_records_citekey_and_delivering_url() {
        let paper = PaperResult { title: "A Paper".to_string(), year: "2025".to_string(), ..Default::default() };
        let meta = Metadata { paper, source: MetadataSource::Arxiv };
        let yaml = build_source_yaml(
            Some(&meta),
            "2510.24941",
            Some("zhao2025can"),
            Some("https://arxiv.org/pdf/2510.24941"),
            "2026-03-01",
        );
        assert!(yaml.contains("bibtex_key: \"zhao2025can\""));
        assert!(yaml.contains("source_url: \"https://arxiv.org/pdf/2510.24941\""));
    }

    #[test]
    fn a_doi_artifact_records_whether_a_provider_supplied_its_metadata() {
        let paper = PaperResult {
            title: "A Paper".to_string(),
            authors: vec!["Jane Doe".to_string()],
            year: "2024".to_string(),
            ..Default::default()
        };
        let confirmed = build_doi_source_yaml(&paper, "10.1234/test", None, None, "2026-03-01", true);
        assert!(confirmed.contains("metadata_confirmed: true"));

        // A synthetic record built from the DOI alone confirms nothing, so the
        // flag must be absent rather than asserted.
        let synthetic = build_doi_source_yaml(&paper, "10.1234/test", None, None, "2026-03-01", false);
        assert!(!synthetic.contains("metadata_confirmed"));
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
            true,
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
    fn test_source_arxiv_id_accepts_an_arxiv_doi() {
        assert_eq!(source_arxiv_id("10.48550/arXiv.2510.24941"), "2510.24941");
        assert_eq!(source_arxiv_id("https://doi.org/10.48550/arXiv.2510.24941v2"), "2510.24941");
    }

    #[test]
    fn test_source_arxiv_id_keeps_normalizing_other_inputs() {
        assert_eq!(source_arxiv_id("2006.11239v2"), "2006.11239");
        assert_eq!(source_arxiv_id("https://arxiv.org/pdf/2006.11239.pdf"), "2006.11239");
    }

    #[test]
    fn test_arxiv_tiers_are_arxiv_only() {
        assert_eq!(tier_urls(&arxiv_tiers("2510.24941")), vec!["https://arxiv.org/pdf/2510.24941"]);
        assert_eq!(tier_urls(&arxiv_tiers("hep-th/9901001")), vec!["https://arxiv.org/pdf/hep-th/9901001"]);
        assert!(arxiv_tiers("2510.24941").iter().all(|t| t.cookies.is_none()), "arXiv needs no session cookie");
    }

    /// Entry names in `dir`, sorted.
    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Run `program args` in `dir` and assert it succeeds.
    fn run_in(dir: &Path, program: &str, args: &[&str]) {
        let status = Command::new(program).args(args).current_dir(dir).status().unwrap();
        assert!(status.success(), "{} {:?} failed", program, args);
    }

    #[test]
    fn unpack_source_extracts_a_tarball() {
        let work = tempfile::tempdir().unwrap();
        std::fs::write(work.path().join("main.tex"), "\\documentclass{article}\n").unwrap();
        run_in(work.path(), "tar", &["-czf", "e.tgz", "main.tex"]);
        let bytes = std::fs::read(work.path().join("e.tgz")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        unpack_source(dir.path(), "2502.11447", &bytes).unwrap();
        assert_eq!(entries(dir.path()), vec!["main.tex"]);
    }

    #[test]
    fn unpack_source_saves_a_gzipped_single_file_as_tex() {
        let work = tempfile::tempdir().unwrap();
        std::fs::write(work.path().join("only"), "\\documentclass{article}\n").unwrap();
        run_in(work.path(), "gzip", &["only"]);
        let bytes = std::fs::read(work.path().join("only.gz")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        unpack_source(dir.path(), "hep-th/9901001", &bytes).unwrap();
        assert_eq!(entries(dir.path()), vec!["hep-th_9901001.tex"]);
    }

    #[test]
    fn unpack_source_saves_a_pdf_payload_as_paper_pdf() {
        // A PDF-only submission's e-print is the PDF; it used to fail tar and
        // gunzip and leave a directory holding only source.yaml.
        let dir = tempfile::tempdir().unwrap();
        unpack_source(dir.path(), "2606.21078", b"%PDF-1.5\nnot really a pdf\n").unwrap();
        assert!(entries(dir.path()).contains(&"paper.pdf".to_string()));
    }

    #[test]
    fn unpack_source_rejects_an_unrecognized_payload_and_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let err = unpack_source(dir.path(), "2606.21078", b"<html>rate limited</html>").unwrap_err();
        assert!(err.to_string().contains("2606.21078"));
        assert!(entries(dir.path()).is_empty());
    }

    #[test]
    fn merge_source_yaml_keeps_confirmed_metadata_and_takes_the_new_provenance() {
        // The bytes now on disk came from this fetch, so its source_url and
        // date are the provenance even when the old metadata is kept.
        let old = "title: \"T\"\narxiv: \"2606.21078\"\nbibtex_key: \"ahmed2026validation\"\nsource_url: \"https://example.org/old.pdf\"\nmetadata_confirmed: true\nretrieved: \"2025-01-01\"\n";
        let new = build_source_yaml(None, "2606.21078", None, Some("https://arxiv.org/e-print/2606.21078"), "2026-10-13");
        let merged = merge_source_yaml(Some(old), new);
        for (key, value) in [
            ("title", "T"),
            ("bibtex_key", "ahmed2026validation"),
            ("metadata_confirmed", "true"),
            ("source_url", "https://arxiv.org/e-print/2606.21078"),
            ("retrieved", "2026-10-13"),
        ] {
            assert_eq!(yaml_field(&merged, key).as_deref(), Some(value), "{} in {:?}", key, merged);
        }
        assert_eq!(merged.matches("source_url").count(), 1, "{:?}", merged);
    }

    /// A tar archive in the pre-POSIX (v7) format, which has no `ustar` magic,
    /// holding `name` with `content`.
    fn v7_tar(name: &str, content: &[u8]) -> Vec<u8> {
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..108].copy_from_slice(b"0000644\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        header[124..136].copy_from_slice(format!("{:011o}\0", content.len()).as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        header[148..156].copy_from_slice(b"        ");
        header[156] = b'0';
        let sum: u32 = header.iter().map(|&b| b as u32).sum();
        header[148..156].copy_from_slice(format!("{:06o}\0 ", sum).as_bytes());
        let mut tar = header.to_vec();
        tar.extend_from_slice(content);
        tar.resize(512 + content.len().div_ceil(512) * 512 + 1024, 0);
        tar
    }

    #[test]
    fn unpack_source_extracts_a_v7_tarball_plain_or_gzipped() {
        let tar = v7_tar("main.tex", b"\\documentclass{article}\n");
        let work = tempfile::tempdir().unwrap();
        std::fs::write(work.path().join("e.tar"), &tar).unwrap();
        run_in(work.path(), "gzip", &["-k", "e.tar"]);
        for bytes in [tar.clone(), std::fs::read(work.path().join("e.tar.gz")).unwrap()] {
            let dir = tempfile::tempdir().unwrap();
            unpack_source(dir.path(), "hep-th/9901001", &bytes).unwrap();
            assert_eq!(entries(dir.path()), vec!["main.tex"]);
            assert_eq!(std::fs::read(dir.path().join("main.tex")).unwrap(), b"\\documentclass{article}\n");
        }
    }

    /// `base/name` holding `source.yaml` and a hand-attached `paper.pdf`.
    fn attached_artifact(base: &Path, name: &str) -> PathBuf {
        let dir = base.join(name);
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("source.yaml"), "arxiv: \"2502.11447\"\nmetadata_confirmed: true\n").unwrap();
        std::fs::write(dir.join("paper.pdf"), "%PDF hand-attached").unwrap();
        dir
    }

    #[test]
    fn place_source_failure_leaves_an_existing_directory_intact() {
        let base = tempfile::tempdir().unwrap();
        let dir = attached_artifact(base.path(), "wang2025does");
        assert!(place_source(&dir, "2502.11447", b"<html>rate limited</html>").is_err());
        assert_eq!(entries(&dir), vec!["paper.pdf", "source.yaml"]);
        assert_eq!(std::fs::read_to_string(dir.join("paper.pdf")).unwrap(), "%PDF hand-attached");
        assert_eq!(
            std::fs::read_to_string(dir.join("source.yaml")).unwrap(),
            "arxiv: \"2502.11447\"\nmetadata_confirmed: true\n"
        );
        assert_eq!(entries(base.path()), vec!["wang2025does"]);
    }

    #[test]
    fn place_source_failure_removes_a_newly_claimed_directory() {
        let base = tempfile::tempdir().unwrap();
        let claim = claim_dir(base.path(), "wang2025does", &Target::Arxiv("2502.11447".into())).unwrap();
        assert!(place_source(claim.dir(), "2502.11447", b"<html>rate limited</html>").is_err());
        drop(claim);
        assert!(entries(base.path()).is_empty());
    }

    #[test]
    fn place_source_merges_the_e_print_into_the_directory_and_keeps_user_files() {
        let base = tempfile::tempdir().unwrap();
        let dir = attached_artifact(base.path(), "wang2025does");
        std::fs::create_dir(dir.join("figures")).unwrap();
        std::fs::write(dir.join("figures/old.png"), "old").unwrap();
        let work = tempfile::tempdir().unwrap();
        std::fs::create_dir(work.path().join("figures")).unwrap();
        std::fs::write(work.path().join("figures/new.png"), "new").unwrap();
        std::fs::write(work.path().join("main.tex"), "\\documentclass{article}\n").unwrap();
        run_in(work.path(), "tar", &["-czf", "e.tgz", "main.tex", "figures"]);
        place_source(&dir, "2502.11447", &std::fs::read(work.path().join("e.tgz")).unwrap()).unwrap();
        assert_eq!(entries(&dir), vec!["figures", "main.tex", "paper.pdf", "source.yaml"]);
        assert_eq!(entries(&dir.join("figures")), vec!["new.png"]);
        assert_eq!(std::fs::read_to_string(dir.join("paper.pdf")).unwrap(), "%PDF hand-attached");
        assert_eq!(entries(base.path()), vec!["wang2025does"]);
    }

    #[test]
    fn merge_source_yaml_takes_new_metadata_and_carries_the_citekey() {
        let old = "arxiv: \"2606.21078\"\nbibtex_key: \"ahmed2026validation\"\n";
        let paper = PaperResult { title: "Validation".to_string(), year: "2026".to_string(), ..Default::default() };
        let meta = Metadata { paper, source: MetadataSource::Arxiv };
        let new = build_source_yaml(Some(&meta), "2606.21078", None, None, "2026-10-13");
        let merged = merge_source_yaml(Some(old), new.clone());
        assert!(merged.starts_with(&new));
        assert_eq!(yaml_field(&merged, "bibtex_key").as_deref(), Some("ahmed2026validation"));
        assert_eq!(merge_source_yaml(None, new.clone()), new);
    }

    #[test]
    fn merge_source_yaml_ignores_a_record_for_another_paper() {
        // An explicit --citekey may point at a directory holding another
        // paper; its record must not survive into this paper's.
        let old = "title: \"Other\"\narxiv: \"2506.21812\"\nbibtex_key: \"palikhe2025towards\"\nmetadata_confirmed: true\n";
        let new = build_source_yaml(None, "2509.21631", None, None, "2026-10-13");
        assert_eq!(merge_source_yaml(Some(old), new.clone()), new);
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
