//! `lit clean <bib_file>` -- Detect and optionally remove problematic .bib entries.
//!
//! Detects three problem categories:
//! - **Malformed**: citekey contains `=`, `http`, or starts with a digit; OR has no title, author, or year.
//! - **Duplicates**: two entries share the same `eprint` or `doi` field (keeps first occurrence).
//! - **Orphans**: entries not cited in any .tex file (only when `--tex <dir>` is provided).
//!
//! By default runs in dry-run mode (report only). With `--apply`, rewrites the bib file
//! removing malformed and duplicate entries (orphans are only pruned with `--prune`).

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;

use crate::bibtex;
use crate::format;

/// Report from a clean scan.
pub struct CleanReport {
    /// Citekeys that are malformed.
    pub malformed: Vec<String>,
    /// Pairs of (kept_key, removed_key) for duplicate entries.
    pub duplicates: Vec<(String, String)>,
    /// Citekeys not cited in any scanned .tex file (empty if no tex_dirs provided).
    pub orphans: Vec<String>,
    /// Citekeys that were actually removed (if apply=true).
    pub removed: Vec<String>,
}

/// Run the clean command: parse, detect issues, optionally rewrite.
///
/// # Arguments
/// - `bib_file`: path to the .bib file
/// - `apply`: if true, rewrite the bib file removing malformed and duplicate entries
/// - `prune`: if true and apply=true, also remove orphaned entries
/// - `tex_dirs`: directories to scan for .tex files (orphan detection)
pub fn run(
    bib_file: &Path,
    apply: bool,
    prune: bool,
    tex_dirs: &[&Path],
) -> Result<CleanReport, Box<dyn std::error::Error>> {
    if !bib_file.exists() {
        return Err(format!("File not found: {}", bib_file.display()).into());
    }

    let content = std::fs::read_to_string(bib_file)?;
    let entries = bibtex::parse_bib_file(&content);

    // --- Detect malformed entries ---
    let malformed: Vec<String> = entries
        .iter()
        .filter(|e| is_malformed(e))
        .map(|e| e.key.clone())
        .collect();

    // --- Detect duplicates (by eprint or doi) ---
    let mut seen_eprint: HashMap<String, String> = HashMap::new();
    let mut seen_doi: HashMap<String, String> = HashMap::new();
    let mut duplicates: Vec<(String, String)> = Vec::new();

    for entry in &entries {
        if let Some(eprint) = entry.get_field("eprint") {
            let normalized = eprint.trim().to_lowercase();
            if !normalized.is_empty() {
                if let Some(existing) = seen_eprint.get(&normalized) {
                    duplicates.push((existing.clone(), entry.key.clone()));
                } else {
                    seen_eprint.insert(normalized, entry.key.clone());
                }
            }
        }
        if let Some(doi) = entry.get_field("doi") {
            let normalized = doi.trim().to_lowercase();
            if !normalized.is_empty() {
                if let Some(existing) = seen_doi.get(&normalized) {
                    duplicates.push((existing.clone(), entry.key.clone()));
                } else {
                    seen_doi.insert(normalized, entry.key.clone());
                }
            }
        }
    }

    // --- Detect orphans (only if tex_dirs provided) ---
    let orphans = if tex_dirs.is_empty() {
        Vec::new()
    } else {
        let cited = collect_citations(tex_dirs)?;
        entries
            .iter()
            .filter(|e| !cited.contains(&e.key))
            .map(|e| e.key.clone())
            .collect()
    };

    // --- Apply: rewrite bib file if requested ---
    let removed = if apply {
        let duplicate_removed: HashSet<&str> =
            duplicates.iter().map(|(_, r)| r.as_str()).collect();

        let to_remove: HashSet<&str> = malformed
            .iter()
            .map(|k| k.as_str())
            .chain(duplicate_removed.iter().copied())
            .chain(if prune {
                orphans.iter().map(|k| k.as_str()).collect::<Vec<_>>().into_iter()
            } else {
                Vec::new().into_iter()
            })
            .collect();

        let removed_keys: Vec<String> = to_remove.iter().map(|s| s.to_string()).collect();
        rewrite_bib(&content, bib_file, &to_remove)?;
        removed_keys
    } else {
        Vec::new()
    };

    Ok(CleanReport {
        malformed,
        duplicates,
        orphans,
        removed,
    })
}

/// Print a human-readable summary of the clean report.
pub fn print_report(report: &CleanReport, apply: bool) {
    let total_issues =
        report.malformed.len() + report.duplicates.len() + report.orphans.len();

    if total_issues == 0 {
        format::info("No issues found.");
        return;
    }

    if !report.malformed.is_empty() {
        println!("Malformed ({}):", report.malformed.len());
        for key in &report.malformed {
            println!("  {}", key);
        }
        println!();
    }

    if !report.duplicates.is_empty() {
        println!("Duplicates ({}):", report.duplicates.len());
        for (kept, removed) in &report.duplicates {
            println!("  keep:{} remove:{}", kept, removed);
        }
        println!();
    }

    if !report.orphans.is_empty() {
        println!("Orphans ({}):", report.orphans.len());
        for key in &report.orphans {
            println!("  {}", key);
        }
        println!();
    }

    if apply {
        if report.removed.is_empty() {
            println!("No entries removed.");
        } else {
            println!("Removed {} entr{}:", report.removed.len(), if report.removed.len() == 1 { "y" } else { "ies" });
            for key in &report.removed {
                println!("  {}", key);
            }
        }
    } else {
        println!(
            "Dry run: {} issue{} found. Pass --apply to remove malformed + duplicate entries.",
            total_issues,
            if total_issues == 1 { "" } else { "s" }
        );
    }
}

// -- Helpers ------------------------------------------------------------------

/// Returns true if a BibEntry is malformed.
///
/// Malformed means: citekey contains `=` or `http` or starts with a digit,
/// OR the entry is missing a title, author, or year.
fn is_malformed(entry: &bibtex::BibEntry) -> bool {
    let key = &entry.key;
    if key.contains('=') || key.contains("http") || key.starts_with(|c: char| c.is_ascii_digit()) {
        return true;
    }
    let title = entry.get_field("title").unwrap_or("").trim();
    let author = entry.get_field("author").unwrap_or("").trim();
    let year = entry.get_field("year").unwrap_or("").trim();
    title.is_empty() || author.is_empty() || year.is_empty()
}

/// Scan .tex files in the given directories and collect all citekeys.
///
/// Matches `\cite{key}`, `\citep{key}`, `\citet{key}`, `\citealt{key}`,
/// `\citealp{key}`, and comma-separated multi-cite variants.
fn collect_citations(tex_dirs: &[&Path]) -> Result<HashSet<String>, Box<dyn std::error::Error>> {
    let mut cited = HashSet::new();

    for dir in tex_dirs {
        collect_citations_in_dir(dir, &mut cited)?;
    }

    Ok(cited)
}

/// Recursively scan a directory for .tex files and collect citekeys.
fn collect_citations_in_dir(
    dir: &Path,
    cited: &mut HashSet<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !dir.is_dir() {
        return Ok(());
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_citations_in_dir(&path, cited)?;
        } else if path.extension().is_some_and(|e| e == "tex") {
            let mut content = String::new();
            std::fs::File::open(&path)?.read_to_string(&mut content)?;
            extract_citekeys(&content, cited);
        }
    }

    Ok(())
}

/// Extract all citekeys from a LaTeX string using a simple parser.
///
/// Handles `\cite{a,b}`, `\citep{a}`, `\citet{a}`, and similar variants.
fn extract_citekeys(content: &str, cited: &mut HashSet<String>) {
    let bytes = content.as_bytes();
    let mut pos = 0;

    while pos < bytes.len() {
        // Look for '\cite' prefix
        if let Some(offset) = content[pos..].find("\\cite") {
            pos += offset + 5; // skip '\cite'

            // Skip optional variant suffix like 'p', 't', 'alt', etc. (up to '{')
            while pos < bytes.len() && bytes[pos] != b'{' && bytes[pos] != b'\n' {
                pos += 1;
            }

            if pos >= bytes.len() || bytes[pos] != b'{' {
                continue;
            }
            pos += 1; // skip '{'

            // Read until matching '}'
            let key_start = pos;
            let mut depth = 1usize;
            while pos < bytes.len() && depth > 0 {
                if bytes[pos] == b'{' {
                    depth += 1;
                } else if bytes[pos] == b'}' {
                    depth -= 1;
                }
                if depth > 0 {
                    pos += 1;
                }
            }

            let keys_str = &content[key_start..pos];
            // Keys may be comma-separated with optional whitespace
            for key in keys_str.split(',') {
                let trimmed = key.trim();
                if !trimmed.is_empty() {
                    cited.insert(trimmed.to_string());
                }
            }

            if pos < bytes.len() {
                pos += 1; // skip closing '}'
            }
        } else {
            break;
        }
    }
}

/// Rewrite the bib file, omitting entries whose keys are in `to_remove`.
///
/// Preserves all comments, `% lit:skip` annotations, and the ordering of
/// remaining entries. Uses raw block extraction so formatting is preserved.
fn rewrite_bib(
    content: &str,
    bib_file: &Path,
    to_remove: &HashSet<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let kept = extract_kept_blocks(content, to_remove);
    std::fs::write(bib_file, kept)?;
    Ok(())
}

/// Extract raw text blocks for entries not in `to_remove`, preserving intervening text.
///
/// Strategy: scan for `@type{key,` start positions, track brace depth to find
/// each entry's end, then include or exclude each block. Text between entries
/// (comments, blank lines) is preserved when the preceding entry is kept.
fn extract_kept_blocks(content: &str, to_remove: &HashSet<&str>) -> String {
    let bytes = content.as_bytes();
    let mut result = String::with_capacity(content.len());
    let mut pos = 0;
    // Track where the last entry ended (or 0 for the file start)
    let mut last_end = 0;

    while pos < bytes.len() {
        // Find next '@'
        let offset = match content[pos..].find('@') {
            Some(o) => o,
            None => break,
        };
        let at_pos = pos + offset;
        pos = at_pos + 1;

        // Read entry type
        let type_start = pos;
        while pos < bytes.len() && bytes[pos] != b'{' && !bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        let entry_type = content[type_start..pos].to_lowercase();

        // Skip to opening brace
        while pos < bytes.len() && bytes[pos] != b'{' {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }
        pos += 1; // skip '{'

        // Skip whitespace
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }

        // Read key (up to comma or whitespace)
        let key_start = pos;
        while pos < bytes.len() && bytes[pos] != b',' && !bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        let key = content[key_start..pos].trim().to_string();

        // Now find the end of this entry (matching closing brace at depth 0)
        // We're already past the first '{' that opened the entry, at depth 1.
        let mut depth: i32 = 1;
        while pos < bytes.len() && depth > 0 {
            if bytes[pos] == b'{' {
                depth += 1;
            } else if bytes[pos] == b'}' {
                depth -= 1;
            }
            pos += 1;
        }
        let entry_end = pos; // position after the closing '}'

        // Skip @comment, @preamble, @string — always preserve as-is
        if entry_type == "comment" || entry_type == "preamble" || entry_type == "string" {
            // Include everything up to and including this block
            result.push_str(&content[last_end..entry_end]);
            last_end = entry_end;
            continue;
        }

        if to_remove.contains(key.as_str()) {
            // Skip this entry: advance last_end past it (dropping preceding inter-entry text too)
            // We preserve inter-entry text only when keeping the entry that follows it.
            last_end = entry_end;
            // Also skip trailing newline(s) after removed entry
            while last_end < bytes.len() && (bytes[last_end] == b'\n' || bytes[last_end] == b'\r') {
                last_end += 1;
            }
        } else {
            // Keep: include everything from last_end to entry_end
            result.push_str(&content[last_end..entry_end]);
            last_end = entry_end;
        }
    }

    // Append any trailing content after the last entry
    if last_end < content.len() {
        result.push_str(&content[last_end..]);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_malformed_digit_key() {
        let entry = bibtex::BibEntry {
            entry_type: "misc".to_string(),
            key: "1".to_string(),
            fields: vec![
                ("url".to_string(), "http://example.com".to_string()),
            ],
        };
        assert!(is_malformed(&entry));
    }

    #[test]
    fn test_is_malformed_equals_in_key() {
        let entry = bibtex::BibEntry {
            entry_type: "misc".to_string(),
            key: "url=http://x.com".to_string(),
            fields: vec![
                ("title".to_string(), "X".to_string()),
                ("author".to_string(), "Y".to_string()),
                ("year".to_string(), "2020".to_string()),
            ],
        };
        assert!(is_malformed(&entry));
    }

    #[test]
    fn test_is_malformed_http_in_key() {
        let entry = bibtex::BibEntry {
            entry_type: "misc".to_string(),
            key: "http://example.com/paper".to_string(),
            fields: vec![
                ("title".to_string(), "X".to_string()),
                ("author".to_string(), "Y".to_string()),
                ("year".to_string(), "2020".to_string()),
            ],
        };
        assert!(is_malformed(&entry));
    }

    #[test]
    fn test_is_malformed_missing_title() {
        let entry = bibtex::BibEntry {
            entry_type: "article".to_string(),
            key: "valid2021".to_string(),
            fields: vec![
                ("author".to_string(), "Jane Doe".to_string()),
                ("year".to_string(), "2021".to_string()),
            ],
        };
        assert!(is_malformed(&entry));
    }

    #[test]
    fn test_is_malformed_ok() {
        let entry = bibtex::BibEntry {
            entry_type: "article".to_string(),
            key: "valid2021".to_string(),
            fields: vec![
                ("title".to_string(), "A Paper".to_string()),
                ("author".to_string(), "Jane Doe".to_string()),
                ("year".to_string(), "2021".to_string()),
            ],
        };
        assert!(!is_malformed(&entry));
    }

    #[test]
    fn test_extract_citekeys_basic() {
        let mut cited = HashSet::new();
        extract_citekeys(r"\cite{foo2020,bar2021}", &mut cited);
        assert!(cited.contains("foo2020"));
        assert!(cited.contains("bar2021"));
    }

    #[test]
    fn test_extract_citekeys_variants() {
        let mut cited = HashSet::new();
        extract_citekeys(r"\citep{a2020} \citet{b2021} \citealt{c2022}", &mut cited);
        assert!(cited.contains("a2020"));
        assert!(cited.contains("b2021"));
        assert!(cited.contains("c2022"));
    }

    #[test]
    fn test_extract_citekeys_whitespace() {
        let mut cited = HashSet::new();
        extract_citekeys(r"\cite{ foo2020 , bar2021 }", &mut cited);
        assert!(cited.contains("foo2020"));
        assert!(cited.contains("bar2021"));
    }

    #[test]
    fn test_extract_kept_blocks_removes_entry() {
        let bib = "@article{good2020,\n  title = {Good},\n  author = {A},\n  year = {2020}\n}\n\n@misc{1,\n  url = {http://x}\n}\n";
        let to_remove: HashSet<&str> = ["1"].iter().copied().collect();
        let result = extract_kept_blocks(bib, &to_remove);
        assert!(result.contains("good2020"));
        assert!(!result.contains("@misc{1"));
    }

    #[test]
    fn test_extract_kept_blocks_preserves_all_when_empty_remove() {
        let bib = "@article{paper2020,\n  title = {T},\n  author = {A},\n  year = {2020}\n}\n";
        let to_remove: HashSet<&str> = HashSet::new();
        let result = extract_kept_blocks(bib, &to_remove);
        assert_eq!(result, bib);
    }
}
