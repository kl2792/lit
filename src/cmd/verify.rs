/// `lit verify <bib_file> [jobs]` -- Verify .bib entries against multiple APIs.
///
/// Parses the .bib file, skips `% lit:skip`-annotated entries, and checks each
/// entry against CrossRef (DOI), arXiv (eprint), OpenAlex (title search),
/// Semantic Scholar (title+author search), and OpenLibrary (books).
///
/// Reports mismatches in year, author, and title. Suggests missing DOIs.
/// Exits with non-zero status if any entries are UNKNOWN or MISMATCH.

use std::path::Path;
use std::sync::Arc;

use regex::Regex;

use super::Context;
use crate::api;
use crate::bibtex;
use crate::db;
use crate::format;
use crate::http;

/// Parsed bib entry for verification.
struct BibEntry {
    entry_type: String,
    key: String,
    title: String,
    author: String,
    year: String,
    doi: String,
    eprint: String,
}

/// Verification outcome for a single entry.
#[derive(Debug, Clone)]
enum Status {
    Ok,
    Mismatch,
    Tentative,
    Book,
    Unknown,
}

/// Result of verifying one entry.
#[derive(Debug, Clone)]
struct VerifyResult {
    index: usize,
    status: Status,
    source: String,
    key: String,
    title: String,
    issues: Vec<String>,
}

pub async fn run(ctx: &Context, bib_file: &Path, jobs: usize) -> Result<(), Box<dyn std::error::Error>> {
    if !bib_file.exists() {
        return Err(format!("File not found: {}", bib_file.display()).into());
    }

    format::info(&format!("Verifying entries in: {}", bib_file.display()));

    let content = std::fs::read_to_string(bib_file)?;
    let entries = parse_entries(&content);

    let total = entries.len();
    println!("Found {} entries to verify", total);
    println!();

    // Count manual skips
    let manual = content.matches("% lit:skip").count();

    format::info(&format!("Verifying entries (parallel={})...", jobs));
    println!();

    // Verify entries using tokio tasks with a semaphore for concurrency control
    let client = Arc::new(ctx.client());
    let results = verify_parallel(&entries, jobs, &client).await;

    // Tally and display results
    let mut ok_count = 0usize;
    let mut mismatch_count = 0usize;
    let mut unknown_count = 0usize;
    let mut book_count = 0usize;

    for r in &results {
        match r.status {
            Status::Ok | Status::Tentative => {
                ok_count += 1;
                if !r.issues.is_empty() {
                    println!(
                        "  ! {} [{}]: {}",
                        r.key,
                        r.source,
                        r.issues.join(" ")
                    );
                }
            }
            Status::Mismatch => {
                mismatch_count += 1;
                println!(
                    "  ! {} [{}]: {}",
                    r.key,
                    r.source,
                    r.issues.join(" ")
                );
            }
            Status::Book => {
                book_count += 1;
                println!("  B {} [{}]: {}", r.key, r.source, r.title);
            }
            Status::Unknown => {
                unknown_count += 1;
                println!("  x {}: {}", r.key, r.title);
            }
        }
    }

    println!();
    let verified = ok_count + manual;
    println!(
        "Total: {} | OK: {} (auto:{} manual:{}) | Mismatch: {} | Books: {} | Not found: {}",
        total, verified, ok_count, manual, mismatch_count, book_count, unknown_count
    );

    if unknown_count > 0 || mismatch_count > 0 {
        Err("Verification found issues".into())
    } else {
        Ok(())
    }
}

/// Parse bib entries from file content, skipping those preceded by `% lit:skip`.
///
/// Delegates to `bibtex::parse_bib_file` for robust parsing (handles nested braces,
/// multi-line values, etc.), then extracts the fields needed for verification.
fn parse_entries(content: &str) -> Vec<BibEntry> {
    let braces_re = Regex::new(r"[{}]").unwrap();
    let bib_entries = bibtex::parse_bib_file(content);

    bib_entries
        .into_iter()
        .filter_map(|be| {
            let title = be
                .get_field("title")
                .map(|t| braces_re.replace_all(t, "").to_string())
                .unwrap_or_default();

            if title.is_empty() {
                return None;
            }

            // Extract last name of first author
            let author = be
                .get_field("author")
                .map(|a| {
                    let first = a.split(" and ").next().unwrap_or("");
                    let part = first.split(',').next().unwrap_or("");
                    part.split_whitespace()
                        .last()
                        .unwrap_or("")
                        .to_string()
                })
                .unwrap_or_default();

            let year = be
                .get_field("year")
                .unwrap_or("")
                .to_string();
            let doi = be
                .get_field("doi")
                .unwrap_or("")
                .to_string();
            let eprint = be
                .get_field("eprint")
                .unwrap_or("")
                .to_string();

            Some(BibEntry {
                entry_type: be.entry_type,
                key: be.key,
                title,
                author,
                year,
                doi,
                eprint,
            })
        })
        .collect()
}

/// Run verification in parallel using tokio tasks with a semaphore.
async fn verify_parallel(
    entries: &[BibEntry],
    jobs: usize,
    client: &Arc<http::Client>,
) -> Vec<VerifyResult> {
    let semaphore = Arc::new(tokio::sync::Semaphore::new(jobs.max(1)));
    let mut handles = Vec::with_capacity(entries.len());

    for (idx, entry) in entries.iter().enumerate() {
        let sem = Arc::clone(&semaphore);
        let client = Arc::clone(client);
        // Copy entry data into owned types for the spawned task
        let entry_type = entry.entry_type.clone();
        let key = entry.key.clone();
        let title = entry.title.clone();
        let author = entry.author.clone();
        let year = entry.year.clone();
        let doi = entry.doi.clone();
        let eprint = entry.eprint.clone();

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let entry = BibEntry {
                entry_type,
                key,
                title,
                author,
                year,
                doi,
                eprint,
            };
            verify_single(&client, &entry, idx).await
        }));
    }

    let mut results = Vec::with_capacity(handles.len());
    for handle in handles {
        results.push(handle.await.unwrap());
    }
    results.sort_by_key(|r| r.index);
    results
}

/// Verify a single bib entry against APIs.
async fn verify_single(client: &http::Client, entry: &BibEntry, index: usize) -> VerifyResult {
    let mut status = Status::Unknown;
    let mut source = String::new();
    let mut found_title = String::new();
    let mut found_year = String::new();
    let mut found_author = String::new();
    let mut issues: Vec<String> = Vec::new();
    let is_book = entry.entry_type == "book";

    // 1. Check CrossRef if DOI present
    if !entry.doi.is_empty() {
        let url = api::crossref::doi_url(&entry.doi);
        let cache_key = db::Db::cache_key("verify_doi", &entry.doi);
        if let Ok(body) = client.get_cached(&cache_key, &url, db::TTL_SEARCH).await {
            if body.contains("\"title\"") {
                if let Ok(result) = api::crossref::parse_doi(&body) {
                    found_title = result.title;
                    found_year = result.year;
                    found_author = api::first_author_lastname(&result.authors);
                    status = Status::Ok;
                    source = "DOI".to_string();
                }
            }
        }
    }

    // 2. Check arXiv if eprint present
    if !entry.eprint.is_empty() && matches!(status, Status::Unknown) {
        let url = api::arxiv::query_url(&entry.eprint);
        let cache_key = db::Db::cache_key("verify_arxiv", &entry.eprint);
        if let Ok(body) = client.get_cached(&cache_key, &url, db::TTL_SEARCH).await {
            if body.contains("<entry>") {
                if let Ok(result) = api::arxiv::parse_entry(&body) {
                    found_title = result.title;
                    found_year = result.year;
                    found_author = api::first_author_lastname(&result.authors);
                    status = Status::Ok;
                    source = "arXiv".to_string();
                }
            }
        }
    }

    // 3. Search OpenAlex by title
    if !entry.title.is_empty() && matches!(status, Status::Unknown) {
        let search_title = if entry.title.len() > 60 {
            // Truncate at last space boundary before 60 chars
            let truncated = format::safe_truncate(&entry.title, 60);
            match truncated.rfind(' ') {
                Some(pos) => truncated[..pos].to_string(),
                None => truncated.to_string(),
            }
        } else {
            entry.title.clone()
        };

        let url = api::openalex::title_search_url(&search_title, 5);
        let cache_key = db::Db::cache_key("verify_oa", &entry.title);
        if let Ok(body) = client.get_cached(&cache_key, &url, db::TTL_SEARCH).await {
            if body.contains("\"results\"") {
                if let Ok(results) = api::openalex::parse_search(&body) {
                    if let Some((best, best_author_match)) =
                        find_best_openalex_match(&results, entry)
                    {
                        found_title = best.title.clone();
                        found_year = best.year.clone();
                        found_author = api::first_author_lastname(&best.authors);

                        // Check for DOI suggestion
                        if let Some(ref found_doi) = best.doi {
                            if entry.doi.is_empty() && best_author_match {
                                let year_close = years_within(
                                    &entry.year,
                                    &found_year,
                                    3,
                                );
                                if year_close {
                                    let clean_doi = found_doi
                                        .replace("https://doi.org/", "");
                                    let is_proceedings = entry.entry_type
                                        == "inproceedings"
                                        || entry.entry_type == "conference";
                                    if clean_doi.to_lowercase().contains("arxiv") {
                                        if !is_proceedings {
                                            issues.push(format!(
                                                "arxiv-doi:{}",
                                                clean_doi
                                            ));
                                        }
                                    } else {
                                        issues.push(format!(
                                            "add-doi:{}",
                                            clean_doi
                                        ));
                                    }
                                }
                            }
                        }

                        if !found_title.is_empty() {
                            if best_author_match {
                                status = Status::Ok;
                                source = "OpenAlex".to_string();
                            } else {
                                status = Status::Tentative;
                                source = "OpenAlex".to_string();
                            }
                        }
                    }
                }
            }
        }
    }

    // 4. Search Semantic Scholar
    if !entry.title.is_empty()
        && matches!(status, Status::Unknown | Status::Tentative)
    {
        let search_query = format!(
            "{} {}",
            format::safe_truncate(&entry.title, 80),
            entry.author
        );
        let url = api::semantic_scholar::search_url(&search_query, 5);
        let cache_key = db::Db::cache_key("verify_ss", &entry.title);
        if let Ok(body) = client.get_cached(&cache_key, &url, db::TTL_SEARCH).await {
            if body.contains("\"data\"") {
                if let Ok(results) = api::semantic_scholar::parse_search(&body) {
                    if let Some(matched) = find_ss_match(&results, entry) {
                        found_title = matched.title.clone();
                        found_year = matched.year.clone();
                        found_author = api::first_author_lastname(&matched.authors);
                        status = Status::Ok;
                        source = "SemanticScholar".to_string();
                    }
                }
            }
        }
    }

    // 5. Search OpenLibrary for books
    if is_book && matches!(status, Status::Unknown) {
        let search_query = format!(
            "{} {}",
            format::safe_truncate(&entry.title, 50),
            entry.author
        );
        let url = api::openlibrary::search_url(&search_query, 5);
        let cache_key = db::Db::cache_key("verify_book", &entry.title);
        if let Ok(body) = client.get_cached(&cache_key, &url, db::TTL_SEARCH).await {
            if body.contains("\"docs\"") {
                if let Ok(results) = api::openlibrary::parse_search(&body) {
                    let title_norm = normalize_title(&entry.title);
                    for p in &results {
                        let pt_norm = normalize_title(&p.title);
                        let cmp_len = 20.min(title_norm.len()).min(pt_norm.len());
                        if cmp_len > 0
                            && title_norm[..cmp_len] == pt_norm[..cmp_len]
                        {
                            found_title = p.title.clone();
                            found_year = p.year.clone();
                            found_author = api::first_author_lastname(&p.authors);
                            status = Status::Book;
                            source = "OpenLibrary".to_string();
                            break;
                        }
                    }
                }
            }
        }
    }

    // Compare metadata for OK/TENTATIVE entries
    if matches!(status, Status::Ok | Status::Tentative) {
        // Year mismatch
        if !entry.year.is_empty()
            && !found_year.is_empty()
            && entry.year != found_year
        {
            issues.push(format!("year:{}->{}", entry.year, found_year));
        }

        // Author mismatch
        if !entry.author.is_empty() && !found_author.is_empty() {
            let a_norm = normalize_author(&entry.author);
            let f_norm = normalize_author(&found_author);
            let mismatch = if a_norm.len() >= 4 {
                a_norm.get(..4) != f_norm.get(..4)
            } else {
                a_norm != f_norm
            };
            if mismatch {
                issues.push(format!(
                    "author:{}->{}",
                    entry.author, found_author
                ));
            }
        }

        // Title mismatch
        if !entry.title.is_empty() && !found_title.is_empty() {
            let t_norm = normalize_title(&entry.title);
            let f_norm = normalize_title(&found_title);
            let t_prefix: String = t_norm.chars().take(30).collect();
            let f_prefix: String = f_norm.chars().take(30).collect();
            if t_prefix != f_prefix {
                issues.push("title-mismatch".to_string());
            }
        }

        // Promote to MISMATCH if real issues exist
        let real_issues = issues
            .iter()
            .any(|i| !i.starts_with("add-doi:") && !i.starts_with("arxiv-doi:"));
        if real_issues && matches!(status, Status::Ok) {
            status = Status::Mismatch;
        }
    }

    let display_title = format::safe_truncate(&entry.title, 50).to_string();

    VerifyResult {
        index,
        status,
        source,
        key: entry.key.clone(),
        title: display_title,
        issues,
    }
}

/// Normalize a title for fuzzy matching: lowercase, strip non-alphanumeric.
fn normalize_title(title: &str) -> String {
    title
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Normalize an author name for matching: lowercase, strip non-alpha.
fn normalize_author(author: &str) -> String {
    author
        .chars()
        .filter(|c| c.is_alphabetic())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Compute the length of the common prefix between two strings.
fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars()
        .zip(b.chars())
        .take_while(|(x, y)| x == y)
        .count()
}

/// Check if author matches (first 4 chars of normalized last name, or exact for short names).
fn author_matches(bib_author: &str, found_names: &str) -> bool {
    let a_norm = normalize_author(bib_author);
    if a_norm.is_empty() {
        return true;
    }
    let names_lower = found_names.to_lowercase();
    if a_norm.len() >= 4 {
        names_lower.contains(&a_norm[..4])
    } else {
        let padded = format!(" {} ", names_lower);
        padded.contains(&format!(" {} ", a_norm))
    }
}

/// Check if two year strings are within `range` of each other.
fn years_within(a: &str, b: &str, range: i32) -> bool {
    match (a.parse::<i32>(), b.parse::<i32>()) {
        (Ok(ya), Ok(yb)) => (ya - yb).abs() <= range,
        _ => false,
    }
}

/// Find the best fuzzy match from OpenAlex results.
/// Returns (best_result, author_matched) or None if no good match.
fn find_best_openalex_match<'a>(
    results: &'a [api::PaperResult],
    entry: &BibEntry,
) -> Option<(&'a api::PaperResult, bool)> {
    let title_norm = normalize_title(&entry.title);
    let author_norm = normalize_author(&entry.author);

    let mut best: Option<(&api::PaperResult, usize, bool)> = None;

    for p in results {
        let pt_norm = normalize_title(&p.title);
        let prefix_len = common_prefix_len(&title_norm, &pt_norm);

        // Build author string from result
        let p_auth_names = p.authors.join(" ");
        let has_author_match = if author_norm.is_empty() {
            true
        } else {
            author_matches(&entry.author, &p_auth_names)
        };

        let mut score = prefix_len;
        let title_match_ok = prefix_len >= 15
            || (prefix_len >= 10
                && !title_norm.is_empty()
                && (prefix_len as f64) >= (title_norm.len() as f64) * 0.8);

        if title_match_ok {
            if !entry.year.is_empty() && p.year == entry.year {
                score += 10;
            }
            if has_author_match {
                score += 15;
            }
        }

        let dominated = match best {
            Some((_, bs, _)) => score <= bs,
            None => false,
        };

        if !dominated {
            best = Some((p, score, has_author_match));
        }
    }

    let (best_paper, best_score, best_author_match) = best?;

    if (best_score >= 20 && best_author_match) || best_score >= 35 {
        Some((best_paper, best_author_match))
    } else {
        None
    }
}

/// Find a Semantic Scholar fuzzy match.
fn find_ss_match<'a>(
    results: &'a [api::PaperResult],
    entry: &BibEntry,
) -> Option<&'a api::PaperResult> {
    let title_norm = normalize_title(&entry.title);

    for p in results {
        let pt_norm = normalize_title(&p.title);
        let prefix_len = common_prefix_len(&title_norm, &pt_norm);

        let p_auth_names = p.authors.join(" ");
        let has_author_match = if entry.author.is_empty() {
            true
        } else {
            author_matches(&entry.author, &p_auth_names)
        };

        let title_match_ok = prefix_len >= 15
            || (prefix_len >= 10
                && !title_norm.is_empty()
                && (prefix_len as f64) >= (title_norm.len() as f64) * 0.8);

        if title_match_ok && has_author_match {
            return Some(p);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_title() {
        assert_eq!(
            normalize_title("Attention Is All You Need!"),
            "attentionisallyouneed"
        );
    }

    #[test]
    fn test_normalize_author() {
        assert_eq!(normalize_author("Vaswani"), "vaswani");
        assert_eq!(normalize_author("O'Brien"), "obrien");
    }

    #[test]
    fn test_common_prefix_len() {
        assert_eq!(common_prefix_len("abcdef", "abcdxy"), 4);
        assert_eq!(common_prefix_len("xyz", "abc"), 0);
        assert_eq!(common_prefix_len("same", "same"), 4);
    }

    #[test]
    fn test_author_matches_long_name() {
        assert!(author_matches("Vaswani", "ashish vaswani et al"));
        assert!(!author_matches("Vaswani", "john smith"));
    }

    #[test]
    fn test_author_matches_short_name() {
        assert!(author_matches("Ho", "jonathan ho et al"));
    }

    #[test]
    fn test_author_matches_empty() {
        assert!(author_matches("", "any author"));
    }

    #[test]
    fn test_years_within() {
        assert!(years_within("2020", "2021", 3));
        assert!(years_within("2020", "2023", 3));
        assert!(!years_within("2020", "2024", 3));
        assert!(!years_within("abc", "2020", 3));
    }

    #[test]
    fn test_parse_entries_basic() {
        let content = r#"
@article{vaswani2017attention,
  title = {Attention Is All You Need},
  author = {Vaswani, Ashish and Shazeer, Noam},
  year = {2017},
  eprint = {1706.03762},
}
"#;
        let entries = parse_entries(content);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "vaswani2017attention");
        assert_eq!(entries[0].title, "Attention Is All You Need");
        assert_eq!(entries[0].author, "Vaswani");
        assert_eq!(entries[0].year, "2017");
        assert_eq!(entries[0].eprint, "1706.03762");
    }

    #[test]
    fn test_parse_entries_skip() {
        let content = r#"
% lit:skip
@article{skipped2020,
  title = {Should Be Skipped},
  author = {Nobody},
  year = {2020},
}

@article{kept2021,
  title = {Should Be Kept},
  author = {Somebody},
  year = {2021},
}
"#;
        let entries = parse_entries(content);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "kept2021");
    }

    #[test]
    fn test_parse_entries_with_doi() {
        let content = r#"
@inproceedings{karimi2021algorithmic,
  title = {Algorithmic Recourse Under Imperfect Causal Knowledge},
  author = {Karimi, Amir-Hossein},
  year = {2021},
  doi = {10.1145/3442188.3445899},
}
"#;
        let entries = parse_entries(content);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].doi, "10.1145/3442188.3445899");
        assert_eq!(entries[0].entry_type, "inproceedings");
    }
}
