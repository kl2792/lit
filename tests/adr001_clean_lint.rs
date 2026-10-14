//! ADR-001 change 1 (lint half): `lit clean` flags `&amp;`, Unicode dashes,
//! and non-macro `month` values; `--apply` rewrites all three.

use lit::cmd::clean;

const FIXTURE: &str = "@article{amp2020entry,\n  title = {A &amp; B},\n  author = {X},\n  year = {2020}\n}\n\n@article{dash2021entry,\n  title = {C},\n  author = {Y},\n  year = {2021},\n  pages = {1\u{2013}2}\n}\n\n@article{month2022entry,\n  title = {D},\n  author = {Z},\n  year = {2022},\n  month = {June}\n}\n\n@article{clean2023entry,\n  title = {Clean},\n  author = {W},\n  year = {2023}\n}\n\n@article{june2024title,\n  title = {The June Uprising},\n  author = {V},\n  year = {2024}\n}\n";

fn lint_keys(report: &clean::CleanReport) -> Vec<String> {
    report.lint.iter().map(|(k, _)| k.clone()).collect()
}

#[test]
fn lint_flags_amp_dash_and_month_leaving_clean_entries_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("refs.bib");
    std::fs::write(&path, FIXTURE).unwrap();

    let report = clean::run(&path, false, false, &[]).unwrap();
    let keys = lint_keys(&report);
    assert!(keys.contains(&"amp2020entry".to_string()), "lint: {:?}", report.lint);
    assert!(keys.contains(&"dash2021entry".to_string()), "lint: {:?}", report.lint);
    assert!(keys.contains(&"month2022entry".to_string()), "lint: {:?}", report.lint);
    assert!(!keys.contains(&"clean2023entry".to_string()), "lint: {:?}", report.lint);
    assert!(!keys.contains(&"june2024title".to_string()), "a title containing the word June is not a month problem: {:?}", report.lint);

    // Dry run: file untouched.
    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content, FIXTURE);
}

#[test]
fn lint_apply_rewrites_all_three_findings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("refs.bib");
    std::fs::write(&path, FIXTURE).unwrap();

    let report = clean::run(&path, true, false, &[]).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains(r"A \& B"), "got: {}", content);
    assert!(!content.contains("&amp;"));
    assert!(content.contains("1--2"), "got: {}", content);
    assert!(!content.contains('\u{2013}'));
    assert!(content.contains("month = jun"), "got: {}", content);
    assert!(content.contains("The June Uprising"), "clean title must survive: {}", content);
    assert!(content.contains("clean2023entry"));

    let fixed = &report.lint_fixed;
    assert!(fixed.contains(&"amp2020entry".to_string()), "fixed: {:?}", fixed);
    assert!(fixed.contains(&"dash2021entry".to_string()), "fixed: {:?}", fixed);
    assert!(fixed.contains(&"month2022entry".to_string()), "fixed: {:?}", fixed);
}
