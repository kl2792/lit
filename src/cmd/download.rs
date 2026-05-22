/// `lit download <id>` -- Download PDF or arXiv LaTeX source.
///
/// Default: find open-access PDF via Unpaywall and print URL.
/// `--source`: download arXiv LaTeX source tarball, extract, write source.yaml.
/// `--url-only`: print PDF URL without downloading.

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
) -> Result<(), Box<dyn std::error::Error>> {
    if source {
        return run_source(ctx, input, dir_override).await;
    }
    run_pdf(ctx, input, url_only).await
}

/// Find open-access PDF via Unpaywall, with S2 and OpenAlex fallbacks.
/// When a PDF is found (open-access or via EZProxy), downloads it to etc/pdf/<citekey>/.
async fn run_pdf(ctx: &Context, input: &str, url_only: bool) -> Result<(), Box<dyn std::error::Error>> {
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

    let pdf_url = uw_pdf.or(s2_pdf).or(oa_pdf);

    let cookie_path = find_cookie_file();
    let ez_url = if cookie_path.is_some() && !doi.is_empty() {
        Some(format!("https://doi-org.ezproxy.cul.columbia.edu/{}", doi))
    } else {
        None
    };

    if url_only {
        if let Some(ref pdf) = pdf_url {
            println!("{}", pdf);
        } else if let Some(ref ez) = ez_url {
            println!("{}", ez);
        } else {
            return Err("No open-access PDF found".into());
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

    // Try to download the PDF.
    let bytes = if let Some(ref url) = pdf_url {
        format::info(&format!("Fetching open-access PDF: {}", url));
        fetch_pdf_bytes(url, None).await
    } else {
        None
    };

    let bytes = if bytes.is_none() {
        match (&ez_url, &cookie_path) {
            (Some(ez), Some(cp)) => {
                format::info("Trying EZProxy...");
                fetch_pdf_bytes(ez, Some(cp)).await
            }
            _ => None,
        }
    } else {
        bytes
    };

    match bytes {
        Some(data) => {
            let dir_name = {
                let slug = generate_dir_name(&meta);
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
            let yaml = build_doi_source_yaml(&meta, &doi, &today);
            std::fs::write(dir_name.join("source.yaml"), &yaml)?;
            let kb = data.len() / 1024;
            println!("Saved: {} ({}KB)", dir_name.display(), kb);
        }
        None => {
            if let Some(ref pdf) = pdf_url {
                println!("PDF: {}", pdf);
            } else {
                println!("No open-access PDF found");
            }
            if let Some(ref ez) = ez_url {
                println!("EZProxy: {} [session active]", ez);
            }
        }
    }

    Ok(())
}

/// Fetch URL as bytes, returning Some only if the response is a valid PDF.
/// When cookie_path is given, delegates to curl for cookie-authenticated requests.
async fn fetch_pdf_bytes(url: &str, cookie_path: Option<&std::path::Path>) -> Option<Vec<u8>> {
    if let Some(cp) = cookie_path {
        let tmp = format!("/tmp/lit_dl_{}.pdf", std::process::id());
        let status = Command::new("curl")
            .args(["-sL", "--max-time", "60", "-b", cp.to_str()?, "-A",
                   "Mozilla/5.0", "-H", "Accept: application/pdf,*/*",
                   url, "-o", &tmp])
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }
        let data = std::fs::read(&tmp).ok()?;
        let _ = std::fs::remove_file(&tmp);
        return if data.starts_with(b"%PDF") { Some(data) } else { None };
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(DOWNLOAD_TIMEOUT_SECS))
        .user_agent("Mozilla/5.0")
        .build()
        .ok()?;
    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let bytes = resp.bytes().await.ok()?;
    if bytes.starts_with(b"%PDF") { Some(bytes.to_vec()) } else { None }
}

fn build_doi_source_yaml(paper: &PaperResult, doi: &str, retrieved: &str) -> String {
    let authors_str = paper.authors.join(" and ").replace('"', "\\\"");
    let title = paper.title.replace('"', "\\\"");
    format!(
        "title: \"{}\"\nauthors: \"{}\"\nyear: {}\ndoi: \"{}\"\nretrieved: \"{}\"\n",
        title, authors_str, paper.year, doi, retrieved
    )
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
