//! ADR-002 provenance decision, `lit attach` half.
//!
//! An artifact is self-describing: exactly one `bibtex_key`, a `source_url`
//! and every stable identifier the bibliography entry carries. Metadata is
//! never invented, and a failed write never leaves a half-written directory.

use lit::cmd::misc::attach_pdf_data;

/// Minimal valid PDF that pdftotext can extract ("Hello ADR World").
const MINI_PDF: &[u8] = b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >> endobj\n4 0 obj << /Length 46 >>\nstream\nBT /F1 24 Tf 72 720 Td (Hello ADR World) Tj ET\nendstream\nendobj\n5 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj\ntrailer << /Root 1 0 R >>\n";

struct Fixture {
    _tmp: tempfile::TempDir,
    root: std::path::PathBuf,
    bib: std::path::PathBuf,
    pdf: std::path::PathBuf,
}

fn fixture(bib_text: &str) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("etc-pdf");
    std::fs::create_dir_all(&root).unwrap();
    let bib = tmp.path().join("refs.bib");
    std::fs::write(&bib, bib_text).unwrap();
    let pdf = tmp.path().join("input.pdf");
    std::fs::write(&pdf, MINI_PDF).unwrap();
    Fixture { _tmp: tmp, root, bib, pdf }
}

const FULL_ENTRY: &str = "@book{pearl2009causality,\n  title = {Causality},\n  author = {Judea Pearl},\n  year = {2009},\n  doi = {10.1017/x},\n  isbn = {9780521895606},\n  url = {https://example.org/causality}\n}\n";

fn yaml_lines_with_key<'a>(yaml: &'a str, key: &str) -> Vec<&'a str> {
    let prefix = format!("{}:", key);
    yaml.lines().filter(|line| line.trim_start().starts_with(&prefix)).collect()
}

#[test]
fn attach_writes_exactly_one_bibtex_key() {
    // L5: build_misc_source_yaml and the provenance block must not both write it.
    let f = fixture(FULL_ENTRY);
    let dir = attach_pdf_data("pearl2009causality", &f.bib, f.pdf.to_str().unwrap(), false, &f.root).unwrap();
    let yaml = std::fs::read_to_string(dir.join("source.yaml")).unwrap();
    assert_eq!(
        yaml_lines_with_key(&yaml, "bibtex_key"),
        vec!["bibtex_key: \"pearl2009causality\""],
        "yaml: {}",
        yaml
    );
}

#[test]
fn attach_records_source_url_and_every_identifier() {
    let f = fixture(FULL_ENTRY);
    let dir = attach_pdf_data("pearl2009causality", &f.bib, f.pdf.to_str().unwrap(), false, &f.root).unwrap();
    let yaml = std::fs::read_to_string(dir.join("source.yaml")).unwrap();
    assert_eq!(yaml_lines_with_key(&yaml, "doi"), vec!["doi: \"10.1017/x\""], "yaml: {}", yaml);
    assert_eq!(yaml_lines_with_key(&yaml, "isbn"), vec!["isbn: \"9780521895606\""], "yaml: {}", yaml);
    assert_eq!(yaml_lines_with_key(&yaml, "source_url").len(), 1, "yaml: {}", yaml);
    assert!(yaml.contains("title: \"Causality\""), "yaml: {}", yaml);
}

#[test]
fn attach_fails_when_entry_lacks_author() {
    // L6: the ADR forbids inventing an author, so the attach must not proceed.
    let f = fixture("@misc{noauthor2020x,\n  title = {A Title Alone},\n  year = {2020}\n}\n");
    let err = attach_pdf_data("noauthor2020x", &f.bib, f.pdf.to_str().unwrap(), false, &f.root)
        .unwrap_err()
        .to_string();
    assert!(err.contains("author"), "error must name the missing field, got: {}", err);
    assert!(!f.root.join("noauthor2020x").exists(), "no artifact on failure");
}

#[test]
fn attach_fails_when_entry_lacks_title() {
    let f = fixture("@misc{notitle2020x,\n  author = {Alice Smith},\n  year = {2020}\n}\n");
    let err = attach_pdf_data("notitle2020x", &f.bib, f.pdf.to_str().unwrap(), false, &f.root)
        .unwrap_err()
        .to_string();
    assert!(err.contains("title"), "error must name the missing field, got: {}", err);
    assert!(!f.root.join("notitle2020x").exists(), "no artifact on failure");
}

#[test]
fn attach_never_writes_unknown_or_question_mark() {
    // A missing year must be omitted, not spelled "?".
    let f = fixture("@misc{noyear2020x,\n  title = {Undated Note},\n  author = {Alice Smith}\n}\n");
    let dir = attach_pdf_data("noyear2020x", &f.bib, f.pdf.to_str().unwrap(), false, &f.root).unwrap();
    let yaml = std::fs::read_to_string(dir.join("source.yaml")).unwrap();
    assert!(yaml_lines_with_key(&yaml, "year").is_empty(), "yaml: {}", yaml);
    assert!(!yaml.contains("unknown"), "yaml: {}", yaml);
}

#[test]
fn attach_force_replaces_stale_extracted_text() {
    // L15: paper.txt describes the previous PDF until it is removed.
    let f = fixture(FULL_ENTRY);
    let dir = attach_pdf_data("pearl2009causality", &f.bib, f.pdf.to_str().unwrap(), false, &f.root).unwrap();
    std::fs::write(dir.join("paper.txt"), "STALE TEXT FROM THE PREVIOUS PDF").unwrap();

    attach_pdf_data("pearl2009causality", &f.bib, f.pdf.to_str().unwrap(), true, &f.root).unwrap();
    let txt = std::fs::read_to_string(dir.join("paper.txt")).unwrap_or_default();
    assert!(!txt.contains("STALE"), "stale text survived --force: {}", txt);
}

#[test]
fn attach_refuses_existing_directory_without_force() {
    let f = fixture(FULL_ENTRY);
    std::fs::create_dir_all(f.root.join("pearl2009causality")).unwrap();
    let err = attach_pdf_data("pearl2009causality", &f.bib, f.pdf.to_str().unwrap(), false, &f.root)
        .unwrap_err()
        .to_string();
    assert!(err.contains("--force"), "got: {}", err);
}

#[cfg(unix)]
#[test]
fn attach_failure_leaves_the_previous_artifact_intact() {
    // L16: a failed write must not leave a half-written directory. Making the
    // artifact root read-only fails the write after the entry validated.
    use std::os::unix::fs::PermissionsExt;
    let f = fixture(FULL_ENTRY);
    let dir = attach_pdf_data("pearl2009causality", &f.bib, f.pdf.to_str().unwrap(), false, &f.root).unwrap();
    let before_pdf = std::fs::read(dir.join("paper.pdf")).unwrap();
    let before_yaml = std::fs::read_to_string(dir.join("source.yaml")).unwrap();

    let mut perms = std::fs::metadata(&f.root).unwrap().permissions();
    perms.set_mode(0o555);
    std::fs::set_permissions(&f.root, perms).unwrap();

    let result = attach_pdf_data("pearl2009causality", &f.bib, f.pdf.to_str().unwrap(), true, &f.root);

    let mut perms = std::fs::metadata(&f.root).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&f.root, perms).unwrap();

    assert!(result.is_err(), "a read-only root must fail the write");
    assert_eq!(std::fs::read(dir.join("paper.pdf")).unwrap(), before_pdf, "pdf must survive");
    assert_eq!(
        std::fs::read_to_string(dir.join("source.yaml")).unwrap(),
        before_yaml,
        "source.yaml must survive"
    );
    let leftovers: Vec<_> = std::fs::read_dir(&f.root)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| name != "pearl2009causality")
        .collect();
    assert!(leftovers.is_empty(), "no staging leftovers, found: {:?}", leftovers);
}
