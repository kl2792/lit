//! ADR-001 change 2: `lit misc --pdf <path-or-url>`.
//!
//! Artifact first, bib entry last; %PDF validation on both paths; on failure
//! nothing is written (no directory, no bib entry) and the error carries the
//! browser-fallback hint.

use lit::cmd::misc::{run_pdf_data, MiscParams};

/// Minimal valid PDF that pdftotext can extract ("Hello ADR World").
const MINI_PDF: &[u8] = b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >> endobj\n4 0 obj << /Length 46 >>\nstream\nBT /F1 24 Tf 72 720 Td (Hello ADR World) Tj ET\nendstream\nendobj\n5 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj\ntrailer << /Root 1 0 R >>\n";

fn params(citekey: &str) -> MiscParams {
    MiscParams {
        citekey: citekey.into(),
        title: "Counterfactual Tiers Tech Report".into(),
        authors: vec!["Aurghya Maiti".into(), "Elias Bareinboim".into()],
        year: "2026".into(),
        howpublished: Some("Tech report R-125".into()),
        note: None,
    }
}

fn pdftotext_available() -> bool {
    std::process::Command::new("pdftotext")
        .arg("-v")
        .output()
        .is_ok()
}

#[test]
fn local_pdf_creates_artifact_then_bib_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("etc-pdf");
    std::fs::create_dir_all(&root).unwrap();
    let bib = tmp.path().join("refs.bib");
    let pdf_path = tmp.path().join("input.pdf");
    std::fs::write(&pdf_path, MINI_PDF).unwrap();

    let result = run_pdf_data(&params("maiti2026tier"), &bib, pdf_path.to_str().unwrap(), false, &root).unwrap();
    assert_eq!(result.entry_key, "maiti2026tier");

    let dir = root.join("maiti2026tier");
    assert!(dir.is_dir(), "etc/pdf/<citekey>/ must be created");
    let saved = std::fs::read(dir.join("paper.pdf")).unwrap();
    assert!(saved.starts_with(b"%PDF"));

    let yaml = std::fs::read_to_string(dir.join("source.yaml")).unwrap();
    assert!(yaml.contains("title: \"Counterfactual Tiers Tech Report\""), "yaml: {}", yaml);
    assert!(yaml.contains("authors: \"Aurghya Maiti and Elias Bareinboim\""), "yaml: {}", yaml);
    assert!(yaml.contains("year: 2026"), "yaml: {}", yaml);
    assert!(yaml.contains("retrieved: \""), "yaml: {}", yaml);

    if pdftotext_available() {
        let txt = std::fs::read_to_string(dir.join("paper.txt")).unwrap();
        assert!(txt.contains("Hello ADR World"), "txt: {}", txt);
    }

    let bib_content = std::fs::read_to_string(&bib).unwrap();
    assert!(bib_content.contains("@misc{maiti2026tier,"), "bib: {}", bib_content);
    assert!(bib_content.contains("howpublished = {Tech report R-125}"), "bib: {}", bib_content);
}

#[test]
fn non_pdf_local_file_rejected_nothing_written() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("etc-pdf");
    std::fs::create_dir_all(&root).unwrap();
    let bib = tmp.path().join("refs.bib");
    let not_pdf = tmp.path().join("input.pdf");
    std::fs::write(&not_pdf, b"<html>not a pdf</html>").unwrap();

    let err = run_pdf_data(&params("maiti2026tier"), &bib, not_pdf.to_str().unwrap(), false, &root);
    assert!(err.is_err());
    assert!(!root.join("maiti2026tier").exists(), "no directory on failure");
    assert!(!bib.exists(), "no bib entry on failure");
}

#[test]
fn url_failure_nothing_written_and_hint_present() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("etc-pdf");
    std::fs::create_dir_all(&root).unwrap();
    let bib = tmp.path().join("refs.bib");

    // Port 9 (discard) on localhost: connection refused, no network needed.
    let err = run_pdf_data(
        &params("maiti2026tier"),
        &bib,
        "http://127.0.0.1:9/paper.pdf",
        false,
        &root,
    )
    .unwrap_err()
    .to_string();

    assert!(
        err.contains("download in browser"),
        "error must carry the browser-fallback hint, got: {}",
        err
    );
    assert!(!root.join("maiti2026tier").exists(), "no directory on failure");
    assert!(!bib.exists(), "no bib entry on failure");
}

#[test]
fn bib_failure_rolls_back_created_directory() {
    // Failure in the final bib step (citekey collision without --force) must
    // remove the directory created this run and leave the bib untouched.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("etc-pdf");
    std::fs::create_dir_all(&root).unwrap();
    let bib = tmp.path().join("refs.bib");
    std::fs::write(
        &bib,
        "@misc{maiti2026tier,\n  title = {A Completely Different Paper},\n  year = {2026}\n}\n",
    )
    .unwrap();
    let original = std::fs::read_to_string(&bib).unwrap();
    let pdf_path = tmp.path().join("input.pdf");
    std::fs::write(&pdf_path, MINI_PDF).unwrap();

    let result = run_pdf_data(&params("maiti2026tier"), &bib, pdf_path.to_str().unwrap(), false, &root);
    assert!(result.is_err(), "collision must abort");
    assert!(!root.join("maiti2026tier").exists(), "created dir must be rolled back");
    assert_eq!(std::fs::read_to_string(&bib).unwrap(), original, "bib must be unchanged");
}

#[test]
fn existing_directory_errors_unless_force() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("etc-pdf");
    let dir = root.join("maiti2026tier");
    std::fs::create_dir_all(&dir).unwrap();
    let bib = tmp.path().join("refs.bib");
    let pdf_path = tmp.path().join("input.pdf");
    std::fs::write(&pdf_path, MINI_PDF).unwrap();

    let err = run_pdf_data(&params("maiti2026tier"), &bib, pdf_path.to_str().unwrap(), false, &root)
        .unwrap_err()
        .to_string();
    assert!(err.contains("--force"), "err must mention --force, got: {}", err);
    assert!(!bib.exists(), "no bib entry on failure");

    // --force overrides.
    run_pdf_data(&params("maiti2026tier"), &bib, pdf_path.to_str().unwrap(), true, &root).unwrap();
    assert!(dir.join("paper.pdf").exists());
    assert!(std::fs::read_to_string(&bib).unwrap().contains("@misc{maiti2026tier,"));
}
