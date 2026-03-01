/// `lit source <arxiv_id> [dir]` -- Download arXiv LaTeX source tarball.
///
/// Looks up paper metadata from the arXiv API, creates a named directory
/// (e.g. `harutyunyan2019_hca/`), downloads and extracts the source tarball,
/// writes a `source.yaml` with metadata, and cleans up the tarball.
///
/// If `[dir]` is provided, it overrides the auto-generated directory name.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::Context;
use crate::api::arxiv;
use crate::api::PaperResult;
use crate::detect::normalize_arxiv;
use crate::format;

/// Download timeout for source tarballs (seconds). Tarballs can be large,
/// so we use a longer timeout than the default 15s HTTP client.
const DOWNLOAD_TIMEOUT_SECS: u64 = 60;

/// Words to skip when generating the directory slug from the title.
const SKIP_WORDS: &[&str] = &[
    "the", "a", "an", "of", "and", "in", "on", "for", "to", "how", "what", "why", "when",
    "with", "from", "by", "is", "are", "at", "its",
];

pub fn run(ctx: &Context, input: &str, dir_override: Option<&Path>) -> Result<(), Box<dyn std::error::Error>> {
    let arxiv_id = normalize_arxiv(input);
    let url = format!("https://arxiv.org/e-print/{}", arxiv_id);

    // Step 1: Look up paper metadata from arXiv API.
    format::info(&format!("Looking up metadata for arXiv:{}", arxiv_id));
    let paper = fetch_metadata(ctx, &arxiv_id)?;

    // Step 2: Determine output directory.
    // Default to etc/pdf/{key} relative to project root (parent of parent of exe).
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

    // Step 3: Download the tarball into the directory.
    let safe_id = arxiv_id.replace('/', "_");
    let tarball = dir_name.join(format!("{}.tar.gz", safe_id));

    format::info(&format!("Downloading arXiv source: {}", arxiv_id));
    let download_client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(DOWNLOAD_TIMEOUT_SECS))
        .user_agent("lit/1.0")
        .build()?;

    let resp = download_client.get(&url).send()?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {} for {}", resp.status(), url).into());
    }
    let bytes = resp.bytes()?;
    std::fs::write(&tarball, &bytes)?;

    if !tarball.exists() {
        return Err("Download failed".into());
    }

    // Step 4: Extract the tarball.
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
            // Try gunzip in case it's a gzipped single file, not a tar archive
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

    // Step 5: Write source.yaml.
    let today = today_string();
    let yaml = build_source_yaml(&paper, &arxiv_id, &today);
    let yaml_path = dir_name.join("source.yaml");
    std::fs::write(&yaml_path, &yaml)?;
    format::info(&format!("Wrote {}", yaml_path.display()));

    // Step 6: Clean up the tarball (if it still exists after extraction).
    if tarball.exists() {
        std::fs::remove_file(&tarball)?;
        format::info("Cleaned up tarball");
    }

    // Summary
    println!("Title: {}", paper.title);
    let first_author = paper.authors.first().map(|s| s.as_str()).unwrap_or("?");
    println!("Authors: {} et al.", first_author);
    println!("Year: {}", paper.year);
    println!("Directory: {}", dir_name.display());

    Ok(())
}

/// Default output directory: `etc/pdf/` relative to the project root.
///
/// Walks up from cwd looking for a directory containing `etc/pdf/`.
/// Falls back to `./etc/pdf/` if no project root is found.
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

/// Fetch paper metadata from the arXiv API.
fn fetch_metadata(ctx: &Context, arxiv_id: &str) -> Result<PaperResult, Box<dyn std::error::Error>> {
    let url = arxiv::query_url(arxiv_id);
    let client = ctx.client();
    let cache_key = crate::cache::Cache::key("arxiv", arxiv_id);
    let body = client.get_cached_deferred(&cache_key, &url, crate::cache::TTL_DOI)?;
    let result = arxiv::parse_entry(&body)?;
    client.cache_set(&cache_key, &body);
    Ok(result)
}

/// Generate a directory name from paper metadata.
///
/// Format: `{lastname}{year}{slug}` (bibtex key style, no separators) where:
/// - `lastname` is the lowercased last name of the first author (non-alpha stripped)
/// - `year` is the 4-digit year
/// - `slug` is the first significant word from the title (lowercase, non-alpha stripped)
///
/// Examples: `harutyunyan2019hindsight`, `schulman2017proximal`, `mesnard2023quantile`
fn generate_dir_name(paper: &PaperResult) -> String {
    let lastname = extract_lastname(&paper.authors);
    let slug = extract_title_slug(&paper.title);

    format!("{}{}{}", lastname, paper.year, slug)
}

/// Extract the lowercased last name of the first author.
fn extract_lastname(authors: &[String]) -> String {
    if let Some(first) = authors.first() {
        let name = first.trim();
        let last = name.split_whitespace().last().unwrap_or("unknown");
        last.chars()
            .filter(|c| c.is_alphabetic())
            .collect::<String>()
            .to_lowercase()
    } else {
        "unknown".to_string()
    }
}

/// Extract a short slug from the title: first significant word, lowercased.
fn extract_title_slug(title: &str) -> String {
    title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .find(|w| !SKIP_WORDS.contains(&w.as_str()) && w.len() > 2)
        .unwrap_or_default()
}

/// Build the source.yaml content.
fn build_source_yaml(paper: &PaperResult, arxiv_id: &str, retrieved: &str) -> String {
    // Format authors as "Last, First and Last, First and ..."
    let authors_str = paper.authors.join(" and ");
    // Escape double quotes in title/authors for YAML safety
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

/// Get today's date as YYYY-MM-DD.
fn today_string() -> String {
    // Use UNIX date formatting via std::time. For simplicity, compute from
    // SystemTime since we don't have chrono as a dependency.
    let now = std::time::SystemTime::now();
    let since_epoch = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Simple date calculation (no leap second precision needed)
    let days = since_epoch / 86400;
    let (year, month, day) = days_to_ymd(days);
    format!("{:04}-{:02}-{:02}", year, month, day)
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
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
        // 2026-03-01 = day 20513 since epoch
        // 2026-01-01 = day 20454
        // Jan has 31, Feb has 28 in 2026: 20454 + 31 + 28 = 20513
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
