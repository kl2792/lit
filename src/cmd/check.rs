/// `lit check [--fix]` -- Validate DB<->filesystem consistency.
///
/// Two checks:
/// 1. DB->FS: papers with non-null local_path must have the directory on disk.
/// 2. FS->DB: etc/pdf/*/ dirs with source.yaml should have a corresponding paper in DB.

use std::path::{Path, PathBuf};

use super::Context;
use crate::db::PaperRow;
use crate::format;

fn yaml_field(content: &str, wanted: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let (key, value) = line.trim().split_once(':')?;
        if key.trim() != wanted {
            return None;
        }
        Some(value.trim().trim_matches('"').to_string())
    })
}

/// The `source.yaml` key recording that a provider confirmed this metadata.
///
/// An artifact-producing command that resolved the work through CrossRef, arXiv
/// or Open Library writes it; one built from a bibliography entry, a filename
/// or hand-entered text does not.
const CONFIRMED_KEY: &str = "metadata_confirmed";

/// Whether a provider ever confirmed this artifact's metadata.
///
/// `title: "unknown"` used to stand in for this, which conflated "nothing is
/// known" with "nothing was checked": an artifact with a present but incomplete
/// record was therefore never completed. The absence of the key means
/// unconfirmed, so artifacts written before it existed are looked up once.
fn is_confirmed(content: &str) -> bool {
    yaml_field(content, CONFIRMED_KEY).is_some_and(|value| value == "true")
}

fn paper_from_bib_entry(entry: &crate::bibtex::BibEntry, local_path: &str) -> Option<PaperRow> {
    let title = entry.get_field("title")?.to_string();
    let authors = entry.get_field("author").unwrap_or_default();
    let authors_json = serde_json::to_string(
        &authors.split(" and ").map(str::trim).collect::<Vec<_>>(),
    )
    .ok()?;
    Some(PaperRow {
        entry_type: Some(entry.entry_type.clone()),
        title,
        authors: authors_json,
        year: entry.get_field("year").map(str::to_string),
        doi: entry.get_field("doi").map(str::to_string),
        arxiv_id: entry.get_field("arxiv").map(str::to_string),
        isbn: entry.get_field("isbn").map(str::to_string),
        journal: entry.get_field("journal").map(str::to_string),
        booktitle: entry.get_field("booktitle").map(str::to_string),
        publisher: entry.get_field("publisher").map(str::to_string),
        volume: entry.get_field("volume").map(str::to_string),
        number: entry.get_field("number").map(str::to_string),
        pages: entry.get_field("pages").map(str::to_string),
        url: entry.get_field("url").map(str::to_string),
        local_path: Some(local_path.to_string()),
        ..Default::default()
    })
}

fn merge_bib_fallback(
    mut paper: PaperRow,
    entry: &crate::bibtex::BibEntry,
    local_path: &str,
) -> PaperRow {
    let Some(fallback) = paper_from_bib_entry(entry, local_path) else {
        return paper;
    };
    if paper.title == "unknown" { paper.title = fallback.title; }
    if paper.authors == "[]" { paper.authors = fallback.authors; }
    macro_rules! fill {
        ($field:ident) => { if paper.$field.is_none() { paper.$field = fallback.$field; } };
    }
    fill!(year); fill!(doi); fill!(arxiv_id); fill!(isbn); fill!(journal);
    fill!(booktitle); fill!(publisher); fill!(volume); fill!(number); fill!(pages); fill!(url);
    paper.entry_type = paper.entry_type.or(fallback.entry_type);
    paper
}

/// Fill remaining fields from the identifier-specific lookup.
///
/// The client is a parameter, and every lookup goes through the response cache,
/// so a caller can seed the cache and exercise this path without a network.
async fn fill_identifier_fallback(client: &crate::http::Client, mut paper: PaperRow) -> PaperRow {
    let fetched = if let Some(doi) = paper.doi.as_deref() {
        let url = crate::api::crossref::doi_url(doi);
        client
            .get_cached(&crate::db::Db::cache_key("doi", doi), &url, crate::db::TTL_DOI)
            .await
            .ok()
            .and_then(|body| crate::api::crossref::parse_doi(&body).ok())
    } else if let Some(arxiv_id) = paper.arxiv_id.as_deref() {
        let url = crate::api::arxiv::query_url(arxiv_id);
        client
            .get_cached(&crate::db::Db::cache_key("arxiv", arxiv_id), &url, crate::db::TTL_DOI)
            .await
            .ok()
            .and_then(|body| crate::api::arxiv::parse_entry(&body).ok())
    } else if let Some(isbn) = paper.isbn.as_deref() {
        // Same cache key as `lookup_isbn_data`, so the two share one response.
        let url = crate::api::openlibrary::isbn_url(isbn);
        client
            .get_cached(&format!("isbn_{}", isbn), &url, crate::db::TTL_DOI)
            .await
            .ok()
            .and_then(|body| crate::api::openlibrary::parse_isbn(&body).ok())
    } else {
        None
    };
    if let Some(result) = fetched {
        let fallback = PaperRow::from(&result);
        if paper.title == "unknown" { paper.title = fallback.title; }
        if paper.authors == "[]" { paper.authors = fallback.authors; }
        macro_rules! fill {
            ($field:ident) => { if paper.$field.is_none() { paper.$field = fallback.$field; } };
        }
        fill!(year); fill!(doi); fill!(arxiv_id); fill!(isbn); fill!(journal);
        fill!(url); fill!(pdf_url); fill!(r#abstract); fill!(categories);
    }
    paper
}

/// Parse a source.yaml file into a PaperRow.
///
/// The YAML format is simple key-value pairs (no nesting). Known keys:
/// title, authors/author, year, arxiv, doi, journal, volume, number, pages,
/// url, publisher, booktitle, retrieved, bibtex_key, note.
pub fn parse_source_yaml(content: &str, local_path: &str) -> PaperRow {
    let mut title = None;
    let mut authors = None;
    let mut year = None;
    let mut arxiv_id = None;
    let mut doi = None;
    let mut isbn = None;
    let mut journal = None;
    let mut volume = None;
    let mut number = None;
    let mut pages = None;
    let mut url = None;
    let mut publisher = None;
    let mut booktitle = None;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        // Strip surrounding quotes
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value);

        match key {
            "title" => title = Some(value.to_string()),
            "authors" | "author" => authors = Some(value.to_string()),
            "year" => year = Some(value.to_string()),
            "arxiv" => arxiv_id = Some(value.to_string()),
            "doi" => doi = Some(value.to_string()),
            "isbn" => isbn = Some(value.to_string()),
            "journal" => journal = Some(value.to_string()),
            "volume" => volume = Some(value.to_string()),
            "number" => number = Some(value.to_string()),
            "pages" => pages = Some(value.to_string()),
            "url" => url = Some(value.to_string()),
            "publisher" => publisher = Some(value.to_string()),
            "booktitle" => booktitle = Some(value.to_string()),
            _ => {} // ignore retrieved, bibtex_key, note, etc.
        }
    }

    // Convert "Last, First and Last, First" author string to JSON array
    let authors_json = match &authors {
        Some(a) => {
            let names: Vec<&str> = a.split(" and ").map(|s| s.trim()).collect();
            serde_json::to_string(&names).unwrap_or_else(|_| format!("[\"{}\"]", a))
        }
        None => "[]".to_string(),
    };

    PaperRow {
        title: title.unwrap_or_else(|| "unknown".to_string()),
        authors: authors_json,
        year,
        arxiv_id,
        doi,
        isbn,
        journal,
        volume,
        number,
        pages,
        url: url.clone(),
        publisher,
        booktitle,
        local_path: Some(local_path.to_string()),
        ..Default::default()
    }
}

/// Where the machine-readable unresolved set lives, relative to the root.
pub const UNRESOLVED_REPORT: &str = ".lit/unresolved-artifacts.json";

/// Dedup key for an artifact directory.
///
/// Built from the leaf name rather than from `strip_prefix(root)`, so the value
/// stored in `local_path` is the same whichever directory the run started in
/// and whichever spelling of the root produced the path.
fn artifact_key(dir: &Path) -> String {
    format!("etc/pdf/{}", dir.file_name().unwrap_or_default().to_string_lossy())
}

/// One artifact's outcome under `--fix`.
enum Outcome {
    /// Reconciled; the paper row now points at this artifact.
    Fixed(i64),
    /// Left untouched; the record names the path and what is missing.
    Unresolved(serde_json::Value),
}

/// An artifact that could not be processed at all.
///
/// `reason` classifies the failure for the report; `error` is propagated once
/// the report has been written.
struct ArtifactError {
    reason: &'static str,
    error: Box<dyn std::error::Error>,
}

fn unresolved_record(
    path: &str,
    candidate: Option<&str>,
    reason: &str,
    missing: &[&str],
    detail: Option<String>,
) -> serde_json::Value {
    serde_json::json!({
        "path": path,
        "candidate": candidate,
        "reason": reason,
        "missing": missing,
        "detail": detail,
    })
}

/// Which citekey candidates the bibliography can answer for.
///
/// Built once per run, so matching an artifact is a hash lookup rather than a
/// scan of every entry, and a key that appears twice is visible as such.
fn index_bib(entries: &[crate::bibtex::BibEntry]) -> std::collections::HashMap<&str, Vec<usize>> {
    let mut index: std::collections::HashMap<&str, Vec<usize>> = std::collections::HashMap::new();
    for (position, entry) in entries.iter().enumerate() {
        index.entry(entry.key.as_str()).or_default().push(position);
    }
    index
}

/// True when nothing identifies the paper: no title and no stable identifier.
///
/// Such a row would make later deduplication unsafe, so the ADR leaves the
/// artifact alone and reports it instead.
fn is_unidentified(paper: &PaperRow) -> bool {
    paper.title == "unknown"
        && paper.doi.is_none()
        && paper.arxiv_id.is_none()
        && paper.isbn.is_none()
}

/// Identifier fields the artifact still lacks, in the order the ADR names them.
fn missing_identifiers(paper: &PaperRow) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if paper.title == "unknown" {
        missing.push("title");
    }
    if paper.doi.is_none() {
        missing.push("doi");
    }
    if paper.arxiv_id.is_none() {
        missing.push("arxiv");
    }
    if paper.isbn.is_none() {
        missing.push("isbn");
    }
    missing
}

/// The stdout of one run.
///
/// Under `--json` the object is the whole of stdout, so the output parses.
/// Prose status goes to stdout only in the non-JSON case; per-artifact detail
/// goes to stderr in both.
fn status_lines(
    json: bool,
    fix: bool,
    issues: usize,
    fixed: usize,
    unresolved: &[serde_json::Value],
) -> Vec<String> {
    if json {
        return vec![serde_json::json!({
            "issues": issues,
            "fixed": fixed,
            "unresolved": unresolved,
        })
        .to_string()];
    }
    let line = if issues == 0 {
        "check: all consistent".to_string()
    } else if fix {
        format!("check: fixed {} of {} issues", fixed, issues)
    } else {
        format!("check: {} issues found (run with --fix to repair)", issues)
    };
    vec![line]
}

/// Persist the unresolved set, or remove a report that no longer applies.
fn write_unresolved_report(
    root: &Path,
    unresolved: &[serde_json::Value],
) -> Result<(), Box<dyn std::error::Error>> {
    let path = root.join(UNRESOLVED_REPORT);
    if unresolved.is_empty() {
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(unresolved)?)?;
    Ok(())
}

/// Reconcile one artifact directory against the database.
///
/// `assigned` maps paper id to the artifact that already claims it, so a second
/// artifact resolving to the same row is reported instead of stealing it.
async fn reconcile_artifact(
    ctx: &Context,
    client: &crate::http::Client,
    dir_path: &Path,
    rel_path: &str,
    bib: Option<(&[crate::bibtex::BibEntry], &std::collections::HashMap<&str, Vec<usize>>)>,
    assigned: &std::collections::HashMap<i64, String>,
) -> Result<Outcome, ArtifactError> {
    let content = std::fs::read_to_string(dir_path.join("source.yaml")).map_err(|e| ArtifactError {
        reason: "read_failed",
        error: e.into(),
    })?;
    let mut paper = parse_source_yaml(&content, rel_path);
    let candidate = yaml_field(&content, "bibtex_key")
        .or_else(|| dir_path.file_name().map(|name| name.to_string_lossy().to_string()));

    if let (Some((entries, index)), Some(key)) = (bib, candidate.as_deref()) {
        match index.get(key).map(Vec::as_slice) {
            Some([position]) => paper = merge_bib_fallback(paper, &entries[*position], rel_path),
            // Picking one of several entries with the same key would record a
            // coin flip as a fact, so the bibliography is repaired by hand.
            Some(positions) => {
                return Ok(Outcome::Unresolved(unresolved_record(
                    rel_path,
                    candidate.as_deref(),
                    "ambiguous_citekey",
                    &["bibtex_key"],
                    Some(format!("{} bibliography entries share the key '{}'", positions.len(), key)),
                )));
            }
            None => {}
        }
    }

    if !is_confirmed(&content) {
        paper = fill_identifier_fallback(client, paper).await;
    }

    if is_unidentified(&paper) {
        return Ok(Outcome::Unresolved(unresolved_record(
            rel_path,
            candidate.as_deref(),
            "no_identifier",
            &missing_identifiers(&paper),
            None,
        )));
    }

    // One transaction: a paper row committed without its local_path is exactly
    // the half-written state this reconciler exists to repair.
    let claim = ctx
        .db
        .upsert_paper_with_local_path(&paper, Some("source_yaml"), rel_path, |id| {
            assigned.get(&id).is_none_or(|claimed| claimed == rel_path)
        })
        .map_err(|e| ArtifactError { reason: "reconcile_failed", error: e.into() })?;

    match claim {
        crate::db::PathClaim::Taken(id) => Ok(Outcome::Fixed(id)),
        crate::db::PathClaim::Rejected(id) => Ok(Outcome::Unresolved(unresolved_record(
            rel_path,
            candidate.as_deref(),
            "local_path_conflict",
            &[],
            Some(format!(
                "paper id={} already points at {}",
                id,
                assigned.get(&id).map(String::as_str).unwrap_or("another artifact")
            )),
        ))),
    }
}

pub async fn run(
    ctx: &Context,
    fix: bool,
    bib_file: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    run_in(ctx, fix, bib_file, &crate::paths::project_root()?).await
}

/// `run` against an explicit project root.
///
/// Every artifact is attempted, and the unresolved report is written before any
/// error propagates, so the filesystem never disagrees with rows the run has
/// already committed.
pub async fn run_in(
    ctx: &Context,
    fix: bool,
    bib_file: Option<&Path>,
    root: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let pdf_dir = root.join("etc/pdf");
    let mut issues = 0usize;
    let mut fixed = 0usize;
    let bib_entries = bib_file.or(ctx.bib_file.as_deref()).and_then(|path| {
        std::fs::read_to_string(path)
            .ok()
            .map(|content| crate::bibtex::parse_bib_file(&content))
    });
    let bib_index = bib_entries.as_deref().map(index_bib);
    let bib = match (bib_entries.as_deref(), bib_index.as_ref()) {
        (Some(entries), Some(index)) => Some((entries, index)),
        _ => None,
    };
    let client = ctx.client();
    let mut unresolved: Vec<serde_json::Value> = Vec::new();
    let mut failure: Option<Box<dyn std::error::Error>> = None;

    // --- Check 1: DB -> FS consistency ---
    let stale = ctx.db.papers_with_local_path()?;
    let mut assigned: std::collections::HashMap<i64, String> = stale.iter().cloned().collect();
    for (id, path) in &stale {
        let full = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            root.join(path)
        };
        if !full.is_dir() {
            issues += 1;
            eprintln!("missing on disk: {} (paper id={})", path, id);
            if fix {
                match ctx.db.clear_local_path(*id) {
                    Ok(()) => {
                        assigned.remove(id);
                        fixed += 1;
                        format::info(&format!("  fixed: cleared local_path for id={}", id));
                    }
                    Err(e) => {
                        format::warn(&format!("  failed: {} (clear_local_path): {}", path, e));
                        failure.get_or_insert(e.into());
                    }
                }
            }
        }
    }

    // --- Check 2: FS -> DB consistency ---
    if pdf_dir.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(&pdf_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let dir_path = entry.path();
            if !dir_path.join("source.yaml").exists() {
                continue;
            }
            let rel_path = artifact_key(&dir_path);

            if ctx.db.has_paper_with_local_path(&rel_path)? {
                continue;
            }
            issues += 1;
            eprintln!("not in DB: {}", rel_path);
            if !fix {
                continue;
            }

            match reconcile_artifact(ctx, &client, &dir_path, &rel_path, bib, &assigned).await {
                Ok(Outcome::Fixed(id)) => {
                    assigned.insert(id, rel_path.clone());
                    fixed += 1;
                    format::info(&format!("  fixed: upserted as id={}", id));
                }
                Ok(Outcome::Unresolved(record)) => {
                    format::warn(&format!(
                        "  skipped: {} ({})",
                        rel_path,
                        record["reason"].as_str().unwrap_or_default()
                    ));
                    unresolved.push(record);
                }
                Err(ArtifactError { reason, error }) => {
                    format::warn(&format!("  failed: {} ({}): {}", rel_path, reason, error));
                    unresolved.push(unresolved_record(
                        &rel_path,
                        None,
                        reason,
                        &[],
                        Some(error.to_string()),
                    ));
                    failure.get_or_insert(error);
                }
            }
        }
    }

    if fix {
        write_unresolved_report(root, &unresolved)?;
    }
    for line in status_lines(ctx.json, fix, issues, fixed, &unresolved) {
        println!("{}", line);
    }
    if !ctx.json && !unresolved.is_empty() {
        eprintln!(
            "unresolved artifacts: {} (report: {})",
            unresolved.len(),
            root.join(UNRESOLVED_REPORT).display()
        );
    }

    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Check for cross-source field conflicts using paper_claims.
///
/// For each paper, finds fields where different sources disagree on the value,
/// and reports all (value, source) pairs for each conflicting field.
pub fn run_conflicts(ctx: &Context) -> Result<(), Box<dyn std::error::Error>> {
    let conflicts = ctx.db.find_claim_conflicts()?;

    if conflicts.is_empty() {
        println!("No cross-source conflicts found");
        return Ok(());
    }

    let mut current_paper: Option<i64> = None;
    let mut paper_count = 0;

    for (paper_id, title, field, claims) in &conflicts {
        if current_paper != Some(*paper_id) {
            current_paper = Some(*paper_id);
            paper_count += 1;
            eprintln!("conflict: id={} {:?}", paper_id, truncate_str(title, 60));
        }
        for (value, source) in claims {
            eprintln!("  {}: {} = {:?}", field, source, truncate_str(value, 60));
        }
    }

    println!("{} papers with conflicts", paper_count);

    Ok(())
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

/// Rebuild the database from source.yaml files.
///
/// Creates a new DB at `{db_path}.new`, scans `etc/pdf/**/source.yaml`,
/// upserts each, then atomically replaces the old DB.
pub fn rebuild(db_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let root = crate::paths::project_root()?;
    let pdf_dir = root.join("etc/pdf");

    if !pdf_dir.is_dir() {
        return Err(format!("etc/pdf/ not found at {}", pdf_dir.display()).into());
    }

    let new_path = db_path.with_extension("db.new");
    let bak_path = db_path.with_extension("db.bak");

    // Clean up any leftover .new file
    if new_path.exists() {
        std::fs::remove_file(&new_path)?;
    }

    // Collect all source.yaml files
    let mut yaml_files: Vec<PathBuf> = Vec::new();
    collect_source_yamls(&pdf_dir, &mut yaml_files);
    yaml_files.sort();

    let total = yaml_files.len();
    if total == 0 {
        return Err("no source.yaml files found".into());
    }

    // Create new DB and populate
    let new_db = crate::db::Db::open(&new_path)?;
    new_db.begin_bulk()?;

    let mut count = 0;
    for yaml_path in &yaml_files {
        let dir_path = yaml_path.parent().unwrap();
        let rel_path = artifact_key(dir_path);

        let content = match std::fs::read_to_string(yaml_path) {
            Ok(c) => c,
            Err(e) => {
                format::warn(&format!("  skip {}: {}", rel_path, e));
                continue;
            }
        };

        let paper = parse_source_yaml(&content, &rel_path);
        if is_unidentified(&paper) {
            continue; // skip empty/placeholder entries
        }

        match new_db.upsert_paper(&paper, Some("source_yaml")) {
            Ok(id) => {
                new_db.set_local_path(id, &rel_path)?;
                count += 1;
            }
            Err(e) => {
                format::warn(&format!("  skip {}: {}", rel_path, e));
            }
        }
    }

    new_db.end_bulk()?;
    drop(new_db);

    // Atomic swap
    if db_path.exists() {
        std::fs::rename(db_path, &bak_path)?;
    }
    std::fs::rename(&new_path, db_path)?;

    println!("rebuilt {}/{} papers", count, total);
    if bak_path.exists() {
        println!("backup: {}", bak_path.display());
    }

    Ok(())
}

/// Recursively collect source.yaml files under a directory.
fn collect_source_yamls(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Check for source.yaml in this subdir
            let yaml = path.join("source.yaml");
            if yaml.is_file() {
                out.push(yaml);
            }
            // Don't recurse further — source.yaml is always one level deep
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_source_yaml_basic() {
        let yaml = r#"title: "Proximal policy optimization algorithms"
authors: "Schulman, John and Wolski, Filip"
year: 2017
arxiv: "1707.06347"
retrieved: "unknown"
"#;
        let paper = parse_source_yaml(yaml, "etc/pdf/schulman2017ppo");
        assert_eq!(paper.title, "Proximal policy optimization algorithms");
        assert_eq!(paper.arxiv_id, Some("1707.06347".to_string()));
        assert_eq!(paper.year, Some("2017".to_string()));
        assert_eq!(paper.local_path, Some("etc/pdf/schulman2017ppo".to_string()));

        // Authors should be a JSON array
        let authors: Vec<String> = serde_json::from_str(&paper.authors).unwrap();
        assert_eq!(authors, vec!["Schulman, John", "Wolski, Filip"]);
    }

    #[test]
    fn test_parse_source_yaml_unquoted() {
        let yaml = "title: Causation\nauthor: David Lewis\nyear: 1973\n";
        let paper = parse_source_yaml(yaml, "etc/pdf/lewis1973causation");
        assert_eq!(paper.title, "Causation");
        let authors: Vec<String> = serde_json::from_str(&paper.authors).unwrap();
        assert_eq!(authors, vec!["David Lewis"]);
    }

    #[test]
    fn test_parse_source_yaml_doi() {
        let yaml = r#"title: "Probabilities of Causation"
authors: "Judea Pearl"
year: 1999
doi: "10.1023/a:1005233831499"
retrieved: "unknown"
"#;
        let paper = parse_source_yaml(yaml, "etc/pdf/pearl1999probabilities");
        assert_eq!(paper.doi, Some("10.1023/a:1005233831499".to_string()));
    }

    #[test]
    fn test_parse_source_yaml_unknown_title() {
        let yaml = "title: \"unknown\"\nretrieved: \"unknown\"\n";
        let paper = parse_source_yaml(yaml, "etc/pdf/foo");
        assert_eq!(paper.title, "unknown");
        assert!(paper.doi.is_none());
        assert!(paper.arxiv_id.is_none());
    }

    #[test]
    fn test_parse_source_yaml_extended() {
        let yaml = r#"title: "Causation"
author: "David Lewis"
year: 1973
journal: "The Journal of Philosophy"
volume: 70
number: 17
pages: "556-567"
url: "https://www.jstor.org/stable/2025310"
bibtex_key: lewis1973causation
"#;
        let paper = parse_source_yaml(yaml, "etc/pdf/lewis1973causation");
        assert_eq!(paper.journal, Some("The Journal of Philosophy".to_string()));
        assert_eq!(paper.volume, Some("70".to_string()));
        assert_eq!(paper.number, Some("17".to_string()));
        assert_eq!(paper.pages, Some("556-567".to_string()));
        assert_eq!(
            paper.url,
            Some("https://www.jstor.org/stable/2025310".to_string())
        );
    }

    #[test]
    fn test_truncate_str() {
        assert_eq!(truncate_str("short", 10), "short");
        assert_eq!(truncate_str("a long string here", 10), "a long str...");
    }

    #[test]
    fn test_bib_fallback_fills_missing_source_metadata() {
        let source = "bibtex_key: \"reinertsen2009flow\"\nyear: 2009\n";
        let entry = crate::bibtex::parse_bib_file(
            "@book{reinertsen2009flow, title = {Product Flow}, author = {Donald G. Reinertsen}, doi = {10.1/x}}",
        ).pop().unwrap();
        let paper = parse_source_yaml(source, "etc/pdf/reinertsen2009flow");
        let merged = merge_bib_fallback(paper, &entry, "etc/pdf/reinertsen2009flow");
        assert_eq!(merged.title, "Product Flow");
        assert_eq!(merged.doi.as_deref(), Some("10.1/x"));
        assert!(merged.authors.contains("Reinertsen"));
    }

    #[test]
    fn test_yaml_field_reads_provenance_key() {
        assert_eq!(yaml_field("bibtex_key: \"paper2026x\"\n", "bibtex_key"), Some("paper2026x".into()));
        assert_eq!(yaml_field("title: \"Paper\"\n", "bibtex_key"), None);
    }

    #[test]
    fn test_parse_source_yaml_isbn() {
        // L13: a book's identifier must survive the round trip to the DB.
        let yaml = "title: \"Causality\"\nisbn: \"9780521895606\"\n";
        let paper = parse_source_yaml(yaml, "etc/pdf/pearl2009causality");
        assert_eq!(paper.isbn, Some("9780521895606".to_string()));
    }

    // -- Reconciler tests (ADR-002 "Required tests, not yet written") ---------
    //
    // These drive `run_in`, which takes the project root explicitly, so no test
    // depends on the process working directory. They live here rather than in
    // `tests/` because `tokio` is a normal dependency, not a dev-dependency,
    // and an integration test therefore cannot drive an async entry point.

    use std::sync::Arc;

    /// A reconciler fixture: an empty in-memory DB and an empty artifact root.
    struct Fixture {
        tmp: tempfile::TempDir,
        ctx: Context,
    }

    impl Fixture {
        fn new() -> Self {
            Fixture {
                tmp: tempfile::tempdir().unwrap(),
                ctx: Context {
                    verbose: false,
                    bib_file: None,
                    bib_stdout: false,
                    json: false,
                    no_cache: false,
                    db: Arc::new(crate::db::Db::open_in_memory().unwrap()),
                },
            }
        }

        fn root(&self) -> &Path {
            self.tmp.path()
        }

        /// Create `etc/pdf/<name>/source.yaml` with the given body.
        fn artifact(&self, name: &str, yaml: &str) -> PathBuf {
            let dir = self.root().join("etc/pdf").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("source.yaml"), yaml).unwrap();
            dir
        }

        /// Create an artifact whose `source.yaml` cannot be read, so the loop
        /// raises an error at that entry and no earlier.
        fn unreadable_artifact(&self, name: &str) {
            std::fs::create_dir_all(self.root().join("etc/pdf").join(name).join("source.yaml")).unwrap();
        }

        fn bib(&self, content: &str) -> PathBuf {
            let path = self.root().join("refs.bib");
            std::fs::write(&path, content).unwrap();
            path
        }

        async fn fix(&self, bib: Option<&Path>) -> Result<(), Box<dyn std::error::Error>> {
            run_in(&self.ctx, true, bib, self.root()).await
        }

        fn local_paths(&self) -> Vec<String> {
            let mut paths: Vec<String> = self
                .ctx
                .db
                .papers_with_local_path()
                .unwrap()
                .into_iter()
                .map(|(_, path)| path)
                .collect();
            paths.sort();
            paths
        }

        fn paper_count(&self) -> i64 {
            self.ctx.db.db_stats().unwrap().paper_count
        }

        fn report(&self) -> Option<Vec<u8>> {
            std::fs::read(self.root().join(UNRESOLVED_REPORT)).ok()
        }

        fn report_records(&self) -> Vec<serde_json::Value> {
            serde_json::from_slice(&self.report().expect("report must exist")).unwrap()
        }
    }

    /// A fully confirmed artifact, so reconciling it makes no provider call.
    const LEWIS_YAML: &str = "title: \"Causation\"\nauthors: \"David Lewis\"\nyear: 1973\ndoi: \"10.2307/2025310\"\nmetadata_confirmed: true\n";

    /// A CrossRef body for `LEWIS_DOI`, as the response cache stores it.
    const LEWIS_CROSSREF: &str = r#"{"message":{"title":["Causation"],"published":{"date-parts":[[1973]]},"container-title":["The Journal of Philosophy"],"DOI":"10.2307/2025310","author":[{"given":"David","family":"Lewis"}]}}"#;

    const LEWIS_DOI: &str = "10.2307/2025310";

    impl Fixture {
        /// Seed the response cache so the identifier lookup resolves offline.
        fn seed_doi_lookup(&self, doi: &str, body: &str) {
            self.ctx.db.cache_set(
                &crate::db::Db::cache_key("doi", doi),
                &crate::api::crossref::doi_url(doi),
                body,
            );
        }

        fn only_paper(&self) -> PaperRow {
            let mut found = self.ctx.db.search_local("Causation", 10).unwrap();
            assert_eq!(found.len(), 1, "expected exactly one paper");
            found.pop().unwrap()
        }
    }

    #[tokio::test]
    async fn an_unconfirmed_artifact_gets_its_remaining_fields_from_the_provider() {
        // L14/D7: the old guard fired only on a `unknown` title, so a titled
        // artifact with a bare DOI kept its empty year and journal forever.
        let f = Fixture::new();
        f.seed_doi_lookup(LEWIS_DOI, LEWIS_CROSSREF);
        f.artifact(
            "lewis1973causation",
            "title: \"Causation\"\nauthors: \"David Lewis\"\ndoi: \"10.2307/2025310\"\n",
        );

        f.fix(None).await.unwrap();

        let paper = f.only_paper();
        assert_eq!(paper.year.as_deref(), Some("1973"));
        assert_eq!(paper.journal.as_deref(), Some("The Journal of Philosophy"));
    }

    #[tokio::test]
    async fn a_confirmed_artifact_is_not_looked_up_again() {
        // The guard exists because a cold cache costs one network call per
        // artifact; `metadata_confirmed` is what the guard now reads.
        let f = Fixture::new();
        f.seed_doi_lookup(LEWIS_DOI, LEWIS_CROSSREF);
        f.artifact(
            "lewis1973causation",
            "title: \"Causation\"\nauthors: \"David Lewis\"\ndoi: \"10.2307/2025310\"\nmetadata_confirmed: true\n",
        );

        f.fix(None).await.unwrap();

        let paper = f.only_paper();
        assert!(paper.year.is_none(), "a confirmed artifact must not be re-fetched");
        assert!(paper.journal.is_none(), "a confirmed artifact must not be re-fetched");
    }

    #[test]
    fn metadata_confirmed_is_a_provenance_flag_not_a_bibliographic_field() {
        let yaml = "title: \"Causation\"\nmetadata_confirmed: true\n";
        assert_eq!(yaml_field(yaml, "metadata_confirmed"), Some("true".into()));
        assert!(!is_confirmed("title: \"Causation\"\n"));
        assert!(is_confirmed(yaml));
        // It describes the file's provenance, so no column changes because of it.
        assert_eq!(parse_source_yaml(yaml, "etc/pdf/x").title, "Causation");
    }

    #[tokio::test]
    async fn fix_twice_leaves_papers_paths_and_report_identical() {
        let f = Fixture::new();
        f.artifact("lewis1973causation", LEWIS_YAML);
        f.artifact("mystery", "retrieved: \"2026-01-01\"\n");

        f.fix(None).await.unwrap();
        let (count, paths, report) = (f.paper_count(), f.local_paths(), f.report());
        assert_eq!(paths, vec!["etc/pdf/lewis1973causation".to_string()]);
        assert!(report.is_some(), "the unresolved artifact must be reported");

        f.fix(None).await.unwrap();
        assert_eq!(f.paper_count(), count, "a second run must not create papers");
        assert_eq!(f.local_paths(), paths, "local_path values must not move");
        assert_eq!(f.report(), report, "the report must be byte-identical");
    }

    #[tokio::test]
    async fn two_artifacts_sharing_a_doi_do_not_flip_the_row_pointer() {
        // L9: the second artifact resolves to the row the first one claimed.
        let f = Fixture::new();
        f.artifact("alpha", "title: \"Shared Work\"\nauthors: \"A Author\"\ndoi: \"10.1/shared\"\nmetadata_confirmed: true\n");
        f.artifact("beta", "title: \"Shared Work\"\nauthors: \"A Author\"\ndoi: \"10.1/shared\"\nmetadata_confirmed: true\n");

        f.fix(None).await.unwrap();
        assert_eq!(f.paper_count(), 1, "one DOI is one paper");
        assert_eq!(f.local_paths(), vec!["etc/pdf/alpha".to_string()]);
        let first = f.report_records();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0]["path"], "etc/pdf/beta");
        assert_eq!(first[0]["reason"], "local_path_conflict");

        f.fix(None).await.unwrap();
        assert_eq!(f.local_paths(), vec!["etc/pdf/alpha".to_string()], "the pointer must not alternate");
        assert_eq!(f.report_records(), first);
    }

    #[tokio::test]
    async fn a_rejected_artifact_leaves_the_row_it_could_not_claim_untouched() {
        // L8: the upsert and the local_path write are one transaction, so an
        // artifact that loses the claim rolls its own metadata back instead of
        // editing a row it was not allowed to point at.
        let f = Fixture::new();
        f.artifact("alpha", "title: \"Alpha Title\"\nauthors: \"A Author\"\ndoi: \"10.1/shared\"\nmetadata_confirmed: true\n");
        f.artifact("beta", "title: \"Beta Title\"\nauthors: \"B Author\"\ndoi: \"10.1/shared\"\nmetadata_confirmed: true\n");

        f.fix(None).await.unwrap();

        assert_eq!(f.paper_count(), 1);
        assert_eq!(f.ctx.db.search_local("Beta", 10).unwrap().len(), 0, "the rejected write must roll back");
        let kept = f.ctx.db.search_local("Alpha", 10).unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].local_path.as_deref(), Some("etc/pdf/alpha"));
    }

    #[tokio::test]
    async fn stored_path_does_not_depend_on_how_the_root_is_spelled() {
        // L10: the dedup key comes from the artifact directory name, so an
        // equivalent spelling of the root cannot produce a second paper.
        let f = Fixture::new();
        f.artifact("lewis1973causation", LEWIS_YAML);

        f.fix(None).await.unwrap();
        assert_eq!(f.local_paths(), vec!["etc/pdf/lewis1973causation".to_string()]);

        let detour = f.root().join("etc").join("..");
        run_in(&f.ctx, true, None, &detour).await.unwrap();
        assert_eq!(f.paper_count(), 1, "the same tree under another spelling is the same paper");
        assert_eq!(f.local_paths(), vec!["etc/pdf/lewis1973causation".to_string()]);
    }

    #[tokio::test]
    async fn bibtex_key_beats_the_directory_name() {
        let f = Fixture::new();
        f.artifact("dirname2020x", "bibtex_key: \"yamlkey2020x\"\n");
        let bib = f.bib(concat!(
            "@article{yamlkey2020x, title = {Yamlwins}, author = {A Author}, year = {2020}}\n",
            "@article{dirname2020x, title = {Dirnamewins}, author = {B Author}, year = {2020}}\n",
        ));

        f.fix(Some(&bib)).await.unwrap();
        let found = f.ctx.db.search_local("Yamlwins", 10).unwrap();
        assert_eq!(found.len(), 1, "the bibtex_key entry must win");
        assert_eq!(found[0].local_path.as_deref(), Some("etc/pdf/dirname2020x"));
    }

    #[tokio::test]
    async fn directory_name_is_used_only_when_bibtex_key_is_absent() {
        let f = Fixture::new();
        f.artifact("dirname2020x", "retrieved: \"2026-01-01\"\n");
        let bib = f.bib(concat!(
            "@article{yamlkey2020x, title = {Yamlwins}, author = {A Author}, year = {2020}}\n",
            "@article{dirname2020x, title = {Dirnamewins}, author = {B Author}, year = {2020}}\n",
        ));

        f.fix(Some(&bib)).await.unwrap();
        let found = f.ctx.db.search_local("Dirnamewins", 10).unwrap();
        assert_eq!(found.len(), 1, "the directory name is the fallback candidate");
        assert_eq!(found[0].local_path.as_deref(), Some("etc/pdf/dirname2020x"));
    }

    #[tokio::test]
    async fn duplicate_bibliography_keys_reach_the_report() {
        // L12: taking the first of two entries silently picks a coin flip.
        let f = Fixture::new();
        f.artifact("dup2020x", "title: \"From The Artifact\"\nauthors: \"A Author\"\n");
        let bib = f.bib(concat!(
            "@article{dup2020x, title = {First Copy}, author = {A Author}, year = {2020}}\n",
            "@article{dup2020x, title = {Second Copy}, author = {B Author}, year = {2021}}\n",
        ));

        f.fix(Some(&bib)).await.unwrap();
        assert_eq!(f.paper_count(), 0, "an ambiguous artifact must stay unwritten");
        let records = f.report_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["path"], "etc/pdf/dup2020x");
        assert_eq!(records[0]["candidate"], "dup2020x");
        assert_eq!(records[0]["reason"], "ambiguous_citekey");
    }

    #[tokio::test]
    async fn report_has_a_fixed_shape() {
        let f = Fixture::new();
        f.artifact("mystery", "retrieved: \"2026-01-01\"\n");

        f.fix(None).await.unwrap();
        let records = f.report_records();
        assert_eq!(records.len(), 1);
        let record = &records[0];
        let mut keys: Vec<&String> = record.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, vec!["candidate", "detail", "missing", "path", "reason"]);
        assert_eq!(record["path"], "etc/pdf/mystery");
        assert_eq!(record["reason"], "no_identifier");
        assert_eq!(record["missing"], serde_json::json!(["title", "doi", "arxiv", "isbn"]));
    }

    #[tokio::test]
    async fn report_is_deleted_once_the_set_empties() {
        let f = Fixture::new();
        f.artifact("mystery", "retrieved: \"2026-01-01\"\n");
        f.fix(None).await.unwrap();
        assert!(f.report().is_some());

        std::fs::remove_dir_all(f.root().join("etc/pdf/mystery")).unwrap();
        f.fix(None).await.unwrap();
        assert!(f.report().is_none(), "a stale report claims work that no longer exists");
    }

    #[tokio::test]
    async fn report_survives_an_error_raised_by_a_later_artifact() {
        // L7: the rows are already committed when the error arrives, so losing
        // the report leaves the filesystem and the database disagreeing.
        let f = Fixture::new();
        f.artifact("aaa-mystery", "retrieved: \"2026-01-01\"\n");
        f.artifact("bbb-lewis", LEWIS_YAML);
        f.unreadable_artifact("zzz-broken");

        let result = f.fix(None).await;
        assert!(result.is_err(), "the unreadable artifact must surface as an error");
        let records = f.report_records();
        assert_eq!(records.len(), 2, "records: {:?}", records);
        assert_eq!(records[0]["path"], "etc/pdf/aaa-mystery");
        assert_eq!(records[1]["reason"], "read_failed");
        assert_eq!(
            f.local_paths(),
            vec!["etc/pdf/bbb-lewis".to_string()],
            "artifacts before the error must still be reconciled"
        );
    }

    #[test]
    fn json_mode_prints_only_json() {
        // L11: a prose line next to the object makes the output unparseable.
        let record = serde_json::json!({"path": "etc/pdf/x"});
        let lines = status_lines(true, true, 3, 1, &[record]);
        assert_eq!(lines.len(), 1);
        serde_json::from_str::<serde_json::Value>(&lines[0]).expect("stdout must parse as JSON");
    }

    #[test]
    fn prose_mode_reports_counts() {
        let lines = status_lines(false, true, 3, 1, &[]);
        assert_eq!(lines, vec!["check: fixed 1 of 3 issues".to_string()]);
        assert_eq!(status_lines(false, false, 0, 0, &[]), vec!["check: all consistent".to_string()]);
    }

    #[test]
    fn artifact_key_ignores_everything_above_the_directory() {
        assert_eq!(artifact_key(Path::new("/a/b/etc/pdf/x")), "etc/pdf/x");
        assert_eq!(artifact_key(Path::new("./etc/pdf/x")), "etc/pdf/x");
    }
}
