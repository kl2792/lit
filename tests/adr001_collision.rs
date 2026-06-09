//! ADR-001 change 3: collision guard on upsert.
//!
//! The guard lives in `bibtex::upsert_to_file`, the shared choke point, so
//! `lit add` (add.rs) and `lit misc` (misc.rs) are covered uniformly. The
//! `add` path is exercised here at the choke point (add's network fetch is
//! not reproducible offline); the `misc` path is exercised end-to-end.

use lit::bibtex::upsert_to_file;
use lit::cmd::misc::{run_data, MiscParams};

const EXISTING: &str = "@misc{maiti2025counterfactual,\n  title = {Counterfactual Reasoning Tech Report},\n  author = {Aurghya Maiti},\n  year = {2025}\n}";

fn write_existing(path: &std::path::Path) {
    std::fs::write(path, format!("{}\n", EXISTING)).unwrap();
}

#[test]
fn upsert_same_key_different_title_aborts_with_both_titles() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("refs.bib");
    write_existing(&path);

    let incoming = "@article{maiti2025counterfactual,\n  title = {Counterfactual Graphical Tiers},\n  author = {Aurghya Maiti},\n  year = {2025}\n}";
    let err = upsert_to_file(&path, incoming, false).unwrap_err().to_string();
    assert!(err.contains("Counterfactual Reasoning Tech Report"), "err: {}", err);
    assert!(err.contains("Counterfactual Graphical Tiers"), "err: {}", err);
    assert!(err.contains("--key"), "err must hint at --key disambiguation: {}", err);

    // Nothing clobbered.
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("Counterfactual Reasoning Tech Report"));
    assert!(!content.contains("Counterfactual Graphical Tiers"));
}

#[test]
fn upsert_same_key_same_title_passes() {
    // Same-paper refresh (WORKFLOWS.md 3b): case- and whitespace-insensitive match.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("refs.bib");
    write_existing(&path);

    let incoming = "@article{maiti2025counterfactual,\n  title = {Counterfactual  reasoning tech REPORT},\n  author = {Aurghya Maiti},\n  year = {2025},\n  journal = {AAAI}\n}";
    upsert_to_file(&path, incoming, false).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("journal = {AAAI}"), "got: {}", content);
}

#[test]
fn upsert_force_overrides_guard() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("refs.bib");
    write_existing(&path);

    let incoming = "@article{maiti2025counterfactual,\n  title = {Counterfactual Graphical Tiers},\n  author = {Aurghya Maiti},\n  year = {2025}\n}";
    upsert_to_file(&path, incoming, true).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("Counterfactual Graphical Tiers"));
    assert!(!content.contains("Counterfactual Reasoning Tech Report"));
}

#[test]
fn misc_collision_aborts_and_force_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("refs.bib");
    write_existing(&path);

    let params = MiscParams {
        citekey: "maiti2025counterfactual".into(),
        title: "A Totally Different Working Paper".into(),
        authors: vec!["Aurghya Maiti".into()],
        year: "2025".into(),
        howpublished: None,
        note: None,
    };
    let err = run_data(&params, &path, false).unwrap_err().to_string();
    assert!(err.contains("Counterfactual Reasoning Tech Report"), "err: {}", err);
    assert!(err.contains("A Totally Different Working Paper"), "err: {}", err);

    run_data(&params, &path, true).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("A Totally Different Working Paper"));
}

#[test]
fn upsert_over_legacy_unsanitized_title_passes() {
    // A legacy entry written before the sanitize pass may still contain
    // `&amp;`; a same-paper refresh arrives sanitized (`\&`). The guard
    // must treat these as the same title.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("refs.bib");
    std::fs::write(
        &path,
        "@article{k2020x,\n  title = {A &amp; B},\n  author = {X},\n  year = {2020}\n}\n",
    )
    .unwrap();
    let incoming = "@article{k2020x,\n  title = {A \\& B},\n  author = {X},\n  year = {2020},\n  journal = {J}\n}";
    upsert_to_file(&path, incoming, false).unwrap();
    assert!(std::fs::read_to_string(&path).unwrap().contains("journal = {J}"));
}
