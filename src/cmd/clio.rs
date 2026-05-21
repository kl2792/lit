/// `lit clio` — Columbia Clio catalog operations.
///
/// Subcommands:
/// - `auth` — check EZProxy cookie status
/// - `sync [--check]` — download and index Columbia catalog

use std::path::Path;

use crate::api::clio as clio_api;

/// Report EZProxy cookie status from a Netscape-format cookies file.
///
/// Counts valid cookies, finds expiry range. If the file does not exist, prints
/// instructions for how to create it.
pub fn run_auth(
    _clio_db_path: &Path,
    cookie_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if !cookie_path.exists() {
        println!("No EZProxy session found.");
        println!("Run: python3 bin/clio-auth.py");
        return Ok(());
    }

    let content = std::fs::read_to_string(cookie_path)?;

    let mut count = 0usize;
    let mut min_expiry: Option<u64> = None;
    let mut max_expiry: Option<u64> = None;

    for line in content.lines() {
        let line = line.trim();
        // Skip comments and blank lines
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        count += 1;

        // Netscape cookie format: tab-separated, field 5 (0-indexed) is expiry epoch
        let fields: Vec<&str> = line.splitn(7, '\t').collect();
        if fields.len() >= 6 {
            if let Ok(expiry) = fields[4].parse::<u64>() {
                min_expiry = Some(min_expiry.map_or(expiry, |m: u64| m.min(expiry)));
                max_expiry = Some(max_expiry.map_or(expiry, |m: u64| m.max(expiry)));
            }
        }
    }

    println!("EZProxy session: {} cookie(s) found", count);
    if let (Some(min_e), Some(max_e)) = (min_expiry, max_expiry) {
        println!("Expiry: {} – {}", epoch_to_date(min_e), epoch_to_date(max_e));
    }
    println!("Cookie file: {}", cookie_path.display());

    Ok(())
}

/// Download and index the Columbia Clio catalog, or report current index status.
///
/// The base URL serves an index page listing `extract-NNN.xml.gz` files.
/// Each file is downloaded, parsed, and inserted into the local FTS5 index.
pub async fn run_sync(
    clio_db_path: &Path,
    check_only: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    const BASE_URL: &str =
        "https://lito.cul.columbia.edu/extracts/ColumbiaLibraryCatalog/full/";

    if check_only {
        if !clio_db_path.exists() {
            println!("clio.db not found — run `lit clio sync` to build the index.");
            return Ok(());
        }
        let conn = rusqlite::Connection::open(clio_db_path)?;
        let last_sync: Option<String> = conn
            .query_row(
                "SELECT value FROM clio_meta WHERE key='last_sync'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap_or(None);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM clio_fts", [], |row| row.get(0))
            .unwrap_or(0);
        println!(
            "Clio index: {} records",
            count
        );
        if let Some(ref date) = last_sync {
            println!("Last sync: {}", date);
        } else {
            println!("Last sync: never");
        }
        return Ok(());
    }

    // Create/open DB and init schema
    if let Some(parent) = clio_db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = rusqlite::Connection::open(clio_db_path)?;
    clio_api::init_clio_db(&conn)?;

    // Fetch index page to discover extract filenames
    let client = reqwest::Client::builder()
        .user_agent("lit/1.0")
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    println!("Fetching file index from {}", BASE_URL);
    let index_html = client.get(BASE_URL).send().await?.text().await?;
    let filenames = extract_gz_filenames(&index_html);

    if filenames.is_empty() {
        return Err("No extract files found at base URL — check the URL or network connection".into());
    }

    println!("Found {} extract files", filenames.len());

    let mut total_records: i64 = 0;

    for (i, filename) in filenames.iter().enumerate() {
        let url = format!("{}{}", BASE_URL, filename);
        eprint!("[{}/{}] Downloading {}...", i + 1, filenames.len(), filename);

        let bytes = match client.get(&url).send().await {
            Ok(resp) => resp.bytes().await?,
            Err(e) => {
                eprintln!(" ERROR: {}", e);
                continue;
            }
        };

        let records = match clio_api::parse_marcxml_gz(&bytes) {
            Ok(r) => r,
            Err(e) => {
                eprintln!(" parse error: {}", e);
                continue;
            }
        };

        let n = records.len();
        // Insert in a transaction per file for performance
        conn.execute("BEGIN", [])?;
        if let Err(e) = clio_api::insert_batch(&conn, &records) {
            eprintln!(" insert error: {}", e);
            conn.execute("ROLLBACK", [])?;
            continue;
        }
        conn.execute("COMMIT", [])?;

        total_records += n as i64;
        eprintln!(" {} records", n);
    }

    // Record sync metadata
    let today = today_string();
    conn.execute(
        "INSERT OR REPLACE INTO clio_meta VALUES ('last_sync', ?1)",
        rusqlite::params![today],
    )?;

    println!("Sync complete: {} total records indexed", total_records);
    println!("Last sync: {}", today);

    Ok(())
}

// --- Helpers ---

/// Extract `extract-NNN.xml.gz` filenames from an HTML index page.
fn extract_gz_filenames(html: &str) -> Vec<String> {
    let mut filenames = Vec::new();
    let mut pos = 0;
    while let Some(start) = html[pos..].find("href=\"") {
        let start = pos + start + 6;
        if let Some(end) = html[start..].find('"') {
            let href = &html[start..start + end];
            if href.ends_with(".xml.gz") {
                // Strip path prefix — only keep the filename
                let name = href.rsplit('/').next().unwrap_or(href);
                filenames.push(name.to_string());
            }
            pos = start + end + 1;
        } else {
            break;
        }
    }
    filenames
}

/// Format a Unix timestamp as YYYY-MM-DD (UTC, approximate via division).
fn epoch_to_date(epoch: u64) -> String {
    let days = epoch / 86400;
    let (year, month, day) = days_to_ymd(days);
    format!("{:04}-{:02}-{:02}", year, month, day)
}

fn today_string() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    epoch_to_date(secs)
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

// Use the optional extension from rusqlite
use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_gz_filenames_basic() {
        let html = r#"<a href="extract-001.xml.gz">extract-001.xml.gz</a>
                      <a href="extract-002.xml.gz">extract-002.xml.gz</a>
                      <a href="README.txt">README</a>"#;
        let names = extract_gz_filenames(html);
        assert_eq!(names, vec!["extract-001.xml.gz", "extract-002.xml.gz"]);
    }

    #[test]
    fn test_extract_gz_filenames_with_paths() {
        let html = r#"<a href="/extracts/full/extract-010.xml.gz">file</a>"#;
        let names = extract_gz_filenames(html);
        assert_eq!(names, vec!["extract-010.xml.gz"]);
    }

    #[test]
    fn test_epoch_to_date_known() {
        // 2026-05-21 = day 20594 from epoch
        // 20594 * 86400 = 1779321600
        assert_eq!(epoch_to_date(1779321600), "2026-05-21");
    }

    #[test]
    fn test_today_string_format() {
        let s = today_string();
        assert_eq!(s.len(), 10);
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[7..8], "-");
    }
}
