//! `lit check [--fix]` -- Validate DB<->filesystem consistency.
//!
//! Two checks:
//! 1. DB->FS: papers with non-null local_path must have the directory on disk.
//! 2. FS->DB: paper storage dirs with source.yaml should have a corresponding paper in DB.

use std::path::{Path, PathBuf};

use super::Context;
use crate::db::PaperRow;
use crate::format;

/// Resolve the paper storage directory.
///
/// Delegates to `crate::find_pdf_base()`.
fn pdf_base() -> Result<PathBuf, Box<dyn std::error::Error>> {
    crate::find_pdf_base()
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

/// Check DB-to-filesystem consistency and optionally fix issues.
pub async fn run(ctx: &Context, fix: bool) -> Result<(), Box<dyn std::error::Error>> {
    let pdf_dir = pdf_base()?;
    let mut issues = 0;

    // --- Check 1: DB -> FS consistency ---
    let stale = ctx.db.papers_with_local_path()?;
    for (id, path) in &stale {
        let full = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            pdf_dir.join(Path::new(path).file_name().unwrap_or_default())
        };
        if !full.is_dir() {
            issues += 1;
            eprintln!("missing on disk: {} (paper id={})", path, id);
            if fix {
                ctx.db.clear_local_path(*id)?;
                format::info(&format!("  fixed: cleared local_path for id={}", id));
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
            let yaml_path = dir_path.join("source.yaml");
            if !yaml_path.exists() {
                continue;
            }

            // Use absolute path as local_path
            let rel_path = dir_path.to_string_lossy().to_string();

            let has_paper = ctx.db.has_paper_with_local_path(&rel_path)?;
            if !has_paper {
                issues += 1;
                eprintln!("not in DB: {}", rel_path);
                if fix {
                    let content = std::fs::read_to_string(&yaml_path)?;
                    let paper = parse_source_yaml(&content, &rel_path);
                    if paper.title == "unknown"
                        && paper.doi.is_none()
                        && paper.arxiv_id.is_none()
                    {
                        format::warn(&format!(
                            "  skipped: {} (no title/doi/arxiv)",
                            rel_path
                        ));
                        continue;
                    }
                    let id = ctx.db.upsert_paper(&paper, Some("source_yaml"))?;
                    ctx.db.set_local_path(id, &rel_path)?;
                    format::info(&format!("  fixed: upserted as id={}", id));
                }
            }
        }
    }

    if issues == 0 {
        println!("check: all consistent");
    } else if fix {
        println!("check: fixed {} issues", issues);
    } else {
        println!("check: {} issues found (run with --fix to repair)", issues);
    }

    Ok(())
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
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}...")
    }
}

/// Rebuild the database from source.yaml files.
///
/// Creates a new DB at `{db_path}.new`, scans the paper storage directory for
/// `source.yaml` files, upserts each, then atomically replaces the old DB.
pub fn rebuild(db_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let pdf_dir = pdf_base()?;

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
        let rel_path = dir_path
            .to_string_lossy()
            .to_string();

        let content = match std::fs::read_to_string(yaml_path) {
            Ok(c) => c,
            Err(e) => {
                format::warn(&format!("  skip {}: {}", rel_path, e));
                continue;
            }
        };

        let paper = parse_source_yaml(&content, &rel_path);
        if paper.title == "unknown" && paper.doi.is_none() && paper.arxiv_id.is_none() {
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
}
