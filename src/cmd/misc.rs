/// `lit misc` — Insert a `@misc` BibTeX entry from a blog post, forum post,
/// tech report, or other work that has no arXiv ID or DOI.
///
/// With `--pdf <path-or-url>` (ADR-001 change 2), also ingests the artifact:
/// creates `etc/pdf/<citekey>/` with `paper.pdf`, `source.yaml`, and
/// `paper.txt` before writing the bib entry (artifact first, bib last).

use std::path::{Path, PathBuf};

pub use super::add::AddResult;

/// Parameters for a `@misc` BibTeX entry.
pub struct MiscParams {
    /// BibTeX citation key (e.g. "chan2022causal").
    pub citekey: String,
    /// Full title of the work.
    pub title: String,
    /// Author names in "First Last" format.
    pub authors: Vec<String>,
    /// Publication year.
    pub year: String,
    /// Where the work is published (e.g. `\url{https://...}`).
    pub howpublished: Option<String>,
    /// Optional note field.
    pub note: Option<String>,
}

/// Generate a `@misc` BibTeX entry, upsert it to a .bib file, and return the result.
pub fn run_data(params: &MiscParams, bib_file: &Path, force: bool) -> Result<AddResult, Box<dyn std::error::Error>> {
    let author_str = params.authors.join(" and ");
    let mut fields = vec![
        format!("  title = {{{}}}", params.title),
        format!("  author = {{{}}}", author_str),
        format!("  year = {{{}}}", params.year),
    ];
    if let Some(ref hp) = params.howpublished {
        fields.push(format!("  howpublished = {{{}}}", hp));
    }
    if let Some(ref note) = params.note {
        fields.push(format!("  note = {{{}}}", note));
    }

    let bib_text = format!("@misc{{{},\n{},\n}}", params.citekey, fields.join(",\n"));

    // upsert_to_file returns the sanitized text as written, so the printed
    // and JSON-emitted entry always matches the file.
    let bib_text = crate::bibtex::upsert_to_file(bib_file, &bib_text, force)?;

    Ok(AddResult {
        entry_key: params.citekey.clone(),
        bib_text,
    })
}

/// Ingest a PDF artifact (local path or URL) and then write the bib entry.
///
/// Order is artifact first, bib entry last. On download or validation failure
/// nothing is written: no directory, no bib entry. `%PDF` magic bytes are
/// validated on both paths. An existing `etc/pdf/<citekey>/` directory is an
/// error unless `force` is set.
pub fn run_pdf_data(
    params: &MiscParams,
    bib_file: &Path,
    pdf: &str,
    force: bool,
    pdf_root: &Path,
) -> Result<AddResult, Box<dyn std::error::Error>> {
    let dir = pdf_root.join(&params.citekey);
    let dir_existed = dir.exists();
    if dir_existed && !force {
        return Err(format!(
            "{} already exists; rerun with --force to overwrite",
            dir.display()
        )
        .into());
    }

    let is_url = pdf.starts_with("http://") || pdf.starts_with("https://");
    let bytes = if is_url {
        fetch_pdf_bytes_via_curl(pdf)
            .map_err(|e| format!("{}\nhint: download in browser, rerun with the local path", e))?
    } else {
        std::fs::read(pdf).map_err(|e| format!("could not read {}: {}", pdf, e))?
    };
    if !bytes.starts_with(b"%PDF") {
        let msg = format!("{} is not a PDF (missing %PDF magic bytes)", pdf);
        return Err(if is_url {
            format!("{}\nhint: download in browser, rerun with the local path", msg).into()
        } else {
            msg.into()
        });
    }

    let result = write_artifact_then_bib(params, bib_file, &dir, &bytes, force);
    if result.is_err() && !dir_existed {
        // Restore nothing-written semantics for directories created this run.
        // NotFound is fine: the failure may predate the create_dir_all.
        if let Err(e) = std::fs::remove_dir_all(&dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            crate::format::warn(&format!(
                "rollback failed: {} left behind: {}",
                dir.display(),
                e
            ));
        }
    }
    result
}

/// Write `paper.pdf`, `source.yaml`, `paper.txt`, then the bib entry.
fn write_artifact_then_bib(
    params: &MiscParams,
    bib_file: &Path,
    dir: &Path,
    bytes: &[u8],
    force: bool,
) -> Result<AddResult, Box<dyn std::error::Error>> {
    write_artifact(params, dir, bytes, "")?;
    run_data(params, bib_file, force)
}

/// Read a required BibTeX field, or fail.
///
/// ADR-002 forbids inventing an author, a title or an identifier, so an entry
/// that lacks one is a defect in the bibliography and stops the attach rather
/// than being papered over with a placeholder.
fn required_field<'a>(entry: &'a crate::bibtex::BibEntry, field: &str) -> Result<&'a str, String> {
    entry
        .get_field(field)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            format!(
                "bibliography entry '{}' has no {}; lit will not invent one, fix the entry first",
                entry.key, field
            )
        })
}

/// Write an artifact for a citekey that already exists in a bibliography.
/// Unlike `run_pdf_data`, this never creates or replaces a BibTeX entry.
pub fn attach_pdf_data(
    citekey: &str,
    bib_file: &Path,
    pdf: &str,
    force: bool,
    pdf_root: &Path,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(bib_file)?;
    let entry = crate::bibtex::parse_bib_file(&content)
        .into_iter()
        .find(|entry| entry.key == citekey)
        .ok_or_else(|| format!("no bibliography entry found for citekey '{}'", citekey))?;
    let title = required_field(&entry, "title")?.to_string();
    let authors = required_field(&entry, "author")?
        .split(" and ")
        .map(str::trim)
        .map(str::to_string)
        .collect();
    // A missing year is omitted rather than fabricated; see build_misc_source_yaml.
    let year = entry.get_field("year").unwrap_or_default().trim().to_string();
    let params = MiscParams {
        citekey: citekey.to_string(),
        title,
        authors,
        year,
        howpublished: entry.get_field("url").map(str::to_string),
        note: Some(format!("attached to existing bibliography entry {}", citekey)),
    };
    let dir = pdf_root.join(citekey);
    if dir.exists() && !force {
        return Err(format!("{} already exists; rerun with --force to overwrite", dir.display()).into());
    }
    let bytes = if pdf.starts_with("http://") || pdf.starts_with("https://") {
        fetch_pdf_bytes_via_curl(pdf).map_err(std::io::Error::other)?
    } else {
        std::fs::read(pdf)?
    };
    if !bytes.starts_with(b"%PDF") {
        return Err(format!("{} is not a PDF (missing %PDF magic bytes)", pdf).into());
    }
    // bibtex_key is already the first line of the generated source.yaml, so the
    // provenance block carries only what the generator does not know.
    let mut provenance = format!("source_url: \"{}\"\n", pdf.replace('"', "\\\""));
    for field in ["doi", "isbn", "arxiv"] {
        if let Some(value) = entry.get_field(field) {
            provenance.push_str(&format!("{}: \"{}\"\n", field, value.replace('"', "\\\"")));
        }
    }
    write_artifact(&params, &dir, &bytes, &provenance)?;
    Ok(dir)
}

/// Build the artifact in a sibling staging directory, then move it into place.
///
/// Every file is written before anything replaces the target, so a failure
/// leaves either the previous artifact or nothing at all, never a directory
/// whose `paper.pdf`, `source.yaml` and `paper.txt` describe different works.
/// Staging inside `dir`'s parent keeps the move within one filesystem.
fn write_artifact(
    params: &MiscParams,
    dir: &Path,
    bytes: &[u8],
    extra_yaml: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let staging = staging_path(dir);
    let _ = std::fs::remove_dir_all(&staging);
    let result = fill_staging(params, &staging, bytes, extra_yaml)
        .and_then(|()| publish(&staging, dir).map_err(Into::into));
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

/// Staging directory for `dir`, a dot-prefixed sibling so scans of `etc/pdf/`
/// that look for `source.yaml` never pick a half-written artifact up.
fn staging_path(dir: &Path) -> PathBuf {
    let name = dir.file_name().unwrap_or_default().to_string_lossy().to_string();
    let parent = dir.parent().unwrap_or(Path::new("."));
    parent.join(format!(".lit-staging-{}-{}", name, std::process::id()))
}

fn fill_staging(
    params: &MiscParams,
    staging: &Path,
    bytes: &[u8],
    extra_yaml: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(staging)?;
    std::fs::write(staging.join("paper.pdf"), bytes)?;
    let mut yaml = build_misc_source_yaml(params, &super::today_string());
    yaml.push_str(extra_yaml);
    std::fs::write(staging.join("source.yaml"), &yaml)?;
    if let Err(e) = super::read::ensure_text(staging) {
        crate::format::warn(&format!("text extraction failed: {}", e));
    }
    Ok(())
}

/// Move `staging` onto `dir`, keeping one complete copy at every instant.
///
/// The previous artifact steps aside before the new one lands and is deleted
/// only once the move succeeded, so a failed rename restores it.
fn publish(staging: &Path, dir: &Path) -> std::io::Result<()> {
    if !dir.exists() {
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        return std::fs::rename(staging, dir);
    }
    // Appended, not `with_extension`, so a citekey containing a dot cannot
    // make two artifacts share one backup name.
    let backup = PathBuf::from(format!("{}.old", staging.display()));
    let _ = std::fs::remove_dir_all(&backup);
    std::fs::rename(dir, &backup)?;
    match std::fs::rename(staging, dir) {
        Ok(()) => {
            let _ = std::fs::remove_dir_all(&backup);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::rename(&backup, dir);
            Err(e)
        }
    }
}

/// Identifier-less `source.yaml` builder, following the `download.rs`
/// conventions (`build_doi_source_yaml`/`build_source_yaml`): quoted
/// title/authors, bare year, quoted retrieved date; misc extras
/// (howpublished, note) included when present.
fn build_misc_source_yaml(params: &MiscParams, retrieved: &str) -> String {
    let title = params.title.replace('"', "\\\"");
    let authors = params.authors.join(" and ").replace('"', "\\\"");
    let mut yaml = String::new();
    yaml.push_str(&format!("bibtex_key: \"{}\"\n", params.citekey.replace('"', "\\\"")));
    yaml.push_str(&format!("title: \"{}\"\n", title));
    yaml.push_str(&format!("authors: \"{}\"\n", authors));
    // An empty year means the source had none; a placeholder would be invention.
    if !params.year.is_empty() {
        yaml.push_str(&format!("year: {}\n", params.year));
    }
    if let Some(ref hp) = params.howpublished {
        yaml.push_str(&format!("howpublished: \"{}\"\n", hp.replace('"', "\\\"")));
    }
    if let Some(ref note) = params.note {
        yaml.push_str(&format!("note: \"{}\"\n", note.replace('"', "\\\"")));
    }
    yaml.push_str(&format!("retrieved: \"{}\"\n", retrieved));
    yaml
}

/// Fetch a URL as bytes via curl (the convention for all PDF fetches).
pub(crate) fn fetch_pdf_bytes_via_curl(url: &str) -> Result<Vec<u8>, String> {
    let tmp = std::env::temp_dir().join(format!("lit_misc_dl_{}.pdf", std::process::id()));
    let status = std::process::Command::new("curl")
        .args([
            "-sfL",
            "--max-time",
            "60",
            "-A",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36",
            "-H",
            "Accept: application/pdf,*/*",
            url,
            "-o",
        ])
        .arg(&tmp)
        .status()
        .map_err(|e| format!("failed to run curl: {}", e))?;
    let result = if status.success() {
        std::fs::read(&tmp).map_err(|e| format!("download failed: {}", e))
    } else {
        Err(format!("download failed for {} (curl {})", url, status))
    };
    let _ = std::fs::remove_file(&tmp);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::NamedTempFile;

    #[test]
    fn test_misc_basic_generates_correct_bibtex() {
        // run_data should produce a @misc entry with all required fields.
        let tmp = NamedTempFile::new().unwrap();
        let params = MiscParams {
            citekey: "chan2022causal".into(),
            title: "Causal Scrubbing".into(),
            authors: vec!["Lawrence Chan".into(), "Buck Shlegeris".into()],
            year: "2022".into(),
            howpublished: Some(r"\url{https://example.com}".into()),
            note: None,
        };
        let result = run_data(&params, tmp.path(), false).unwrap();
        assert_eq!(result.entry_key, "chan2022causal");
        assert!(result.bib_text.starts_with("@misc{chan2022causal,"));
        assert!(result.bib_text.contains("title = {Causal Scrubbing}"));
        assert!(result.bib_text.contains("author = {Lawrence Chan and Buck Shlegeris}"));
        assert!(result.bib_text.contains("year = {2022}"));
        assert!(result.bib_text.contains(r"howpublished = {\url{https://example.com}}"));
        assert!(!result.bib_text.contains("note"));
    }

    #[test]
    fn test_misc_omits_optional_fields_when_none() {
        // run_data should omit howpublished and note when not provided.
        let tmp = NamedTempFile::new().unwrap();
        let params = MiscParams {
            citekey: "smith2023blog".into(),
            title: "A Blog Post".into(),
            authors: vec!["Alice Smith".into()],
            year: "2023".into(),
            howpublished: None,
            note: None,
        };
        let result = run_data(&params, tmp.path(), false).unwrap();
        assert!(!result.bib_text.contains("howpublished"));
        assert!(!result.bib_text.contains("note"));
    }

    #[test]
    fn test_misc_writes_to_bib_file() {
        // run_data should upsert the entry to the bib file on disk.
        let tmp = NamedTempFile::new().unwrap();
        let params = MiscParams {
            citekey: "test2024post".into(),
            title: "Test Post".into(),
            authors: vec!["Tester".into()],
            year: "2024".into(),
            howpublished: None,
            note: None,
        };
        run_data(&params, tmp.path(), false).unwrap();
        let contents = fs::read_to_string(tmp.path()).unwrap();
        assert!(contents.contains("@misc{test2024post,"));
    }

    #[test]
    fn test_misc_includes_note_when_provided() {
        // run_data should include the note field when it is Some.
        let tmp = NamedTempFile::new().unwrap();
        let params = MiscParams {
            citekey: "foo2021bar".into(),
            title: "Foo Bar".into(),
            authors: vec!["Foo Author".into()],
            year: "2021".into(),
            howpublished: None,
            note: Some("Accessed: 2024-01-01".into()),
        };
        let result = run_data(&params, tmp.path(), false).unwrap();
        assert!(result.bib_text.contains("note = {Accessed: 2024-01-01}"));
    }

    #[test]
    fn test_bib_text_matches_file_after_sanitize() {
        // The returned (printed / JSON-emitted) bib_text must be the
        // sanitized text as written to the file, not the raw input.
        let tmp = NamedTempFile::new().unwrap();
        let params = MiscParams {
            citekey: "tom2020jerry".into(),
            title: "Tom &amp; Jerry".into(),
            authors: vec!["Hanna Barbera".into()],
            year: "2020".into(),
            howpublished: None,
            note: None,
        };
        let result = run_data(&params, tmp.path(), false).unwrap();
        assert!(result.bib_text.contains(r"Tom \& Jerry"), "got: {}", result.bib_text);
        let contents = fs::read_to_string(tmp.path()).unwrap();
        assert_eq!(contents.trim(), result.bib_text);
    }
}
