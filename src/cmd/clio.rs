/// `lit clio` — Columbia Clio catalog operations.
///
/// Subcommands:
/// - `auth` — check EZProxy cookie status
/// - `sync [--check]` — download and index Columbia catalog

use std::path::Path;

use crate::api::clio as clio_api;

/// Number of files to download+parse concurrently.
/// lito.cul.columbia.edu tolerates ~3 concurrent connections.
const PARALLEL: usize = 3;
/// Retries per file on network error.
const RETRIES: usize = 3;

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
/// Completed files are recorded in clio_meta so interrupted syncs can resume.
///
/// Guards against accidental double-runs: if a full sync completed within the last
/// 30 days, prints a message and exits unless `--force` is passed.
pub async fn run_sync(
    clio_db_path: &Path,
    check_only: bool,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    const BASE_URL: &str =
        "https://lito.cul.columbia.edu/extracts/ColumbiaLibraryCatalog/full/";
    const SYNC_INTERVAL_DAYS: u64 = 30;

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
        let files_done: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM clio_meta WHERE key LIKE 'file:%'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        println!("Clio index: {} records ({} files indexed)", count, files_done);
        match last_sync {
            Some(ref date) => println!("Last sync: {}", date),
            None if files_done > 0 => println!("Last sync: in progress ({} files done)", files_done),
            None => println!("Last sync: never"),
        }
        return Ok(());
    }

    // Create/open DB and init schema
    if let Some(parent) = clio_db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = rusqlite::Connection::open(clio_db_path)?;
    clio_api::init_clio_db(&conn)?;

    // Guard: skip if synced within the last 30 days (unless --force)
    if !force {
        let last_sync: Option<String> = conn
            .query_row(
                "SELECT value FROM clio_meta WHERE key='last_sync'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap_or(None);
        if let Some(ref date) = last_sync {
            println!("Already synced on {}. Columbia updates monthly.", date);
            println!("Run `lit clio sync --force` to re-sync.");
            return Ok(());
        }
    }

    // --force: clear existing data and per-file progress
    if force {
        conn.execute_batch(
            "DELETE FROM clio_fts;
             DELETE FROM clio_meta WHERE key LIKE 'file:%';
             DELETE FROM clio_meta WHERE key='last_sync';",
        )?;
        println!("Cleared existing index.");
    }

    // Fetch index page to discover extract filenames
    let client = reqwest::Client::builder()
        .user_agent("lit/1.0")
        .timeout(std::time::Duration::from_secs(120))
        .build()?;

    println!("Fetching file index from {}", BASE_URL);
    let index_html = client.get(BASE_URL).send().await?.text().await?;
    let filenames = extract_gz_filenames(&index_html);

    if filenames.is_empty() {
        return Err("No extract files found at base URL — check the URL or network connection".into());
    }

    println!("Found {} extract files", filenames.len());

    // Count already-done files (for resume after interruption)
    let already_done: std::collections::HashSet<String> = {
        let mut stmt = conn.prepare(
            "SELECT SUBSTR(key, 6) FROM clio_meta WHERE key LIKE 'file:%'"
        )?;
        stmt.query_map([], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect()
    };

    if !already_done.is_empty() {
        println!("Resuming: {} files already done, skipping.", already_done.len());
    }

    let mut total_records: i64 = conn
        .query_row("SELECT COUNT(*) FROM clio_fts", [], |row| row.get(0))
        .unwrap_or(0);

    let todo: Vec<(usize, String)> = filenames
        .iter()
        .enumerate()
        .filter(|(_, f)| !already_done.contains(*f))
        .map(|(i, f)| (i, f.clone()))
        .collect();

    let total = filenames.len();
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(PARALLEL));
    let (tx, mut rx) =
        tokio::sync::mpsc::channel::<(usize, String, Result<Vec<clio_api::ClioRecord>, String>)>(
            PARALLEL * 2,
        );

    // Spawn all download+parse tasks, bounded by semaphore
    for (i, filename) in todo {
        let sem = sem.clone();
        let tx = tx.clone();
        let url = format!("{}{}", BASE_URL, filename);
        let client = client.clone();

        tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let mut last_err = String::new();
            let mut result = Err(String::new());
            for attempt in 0..RETRIES {
                if attempt > 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(2u64.pow(attempt as u32))).await;
                }
                match client.get(&url).send().await {
                    Err(e) => { last_err = e.to_string(); continue; }
                    Ok(resp) => match resp.bytes().await {
                        Err(e) => { last_err = e.to_string(); continue; }
                        Ok(bytes) => {
                            result = tokio::task::spawn_blocking(move || {
                                clio_api::parse_marcxml_gz(&bytes).map_err(|e| e.to_string())
                            })
                            .await
                            .map_err(|e| e.to_string())
                            .and_then(|r| r);
                            if result.is_ok() { break; }
                            if let Err(ref e) = result { last_err = e.clone(); }
                        }
                    }
                }
            }
            let result = if result.is_err() { Err(last_err) } else { result };
            let _ = tx.send((i, filename, result)).await;
        });
    }
    drop(tx); // close sender so receiver terminates when all tasks finish

    // Insert results as they arrive (sequential writes to SQLite)
    while let Some((i, filename, result)) = rx.recv().await {
        match result {
            Ok(records) => {
                let n = records.len();
                eprint!("[{}/{}] {}... ", i + 1, total, filename);
                conn.execute("BEGIN", [])?;
                if let Err(e) = clio_api::insert_batch(&conn, &records) {
                    eprintln!("insert error: {}", e);
                    conn.execute("ROLLBACK", [])?;
                    continue;
                }
                conn.execute(
                    "INSERT OR REPLACE INTO clio_meta VALUES (?1, ?2)",
                    rusqlite::params![format!("file:{}", filename), n.to_string()],
                )?;
                conn.execute("COMMIT", [])?;
                total_records += n as i64;
                eprintln!("{} records (total: {})", n, total_records);
            }
            Err(e) => eprintln!("[{}/{}] {} ERROR: {}", i + 1, total, filename, e),
        }
    }

    // Mark full sync complete
    let today = today_string();
    conn.execute(
        "INSERT OR REPLACE INTO clio_meta VALUES ('last_sync', ?1)",
        rusqlite::params![today],
    )?;

    println!("Sync complete: {} total records indexed", total_records);
    println!("Last sync: {}", today);
    let _ = SYNC_INTERVAL_DAYS;

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
