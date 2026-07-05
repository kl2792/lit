//! ADR-001 change 1: sanitize BibTeX at the emission choke points.
//!
//! Covers the `lit::sanitize` module plus the `bibtex::upsert_to_file` /
//! `bibtex::append_to_file` choke points (the latter is the `-b/--bib` path).

use lit::sanitize::sanitize_bibtex;

fn entry(fields: &str) -> String {
    format!("@article{{test2020key,\n{}\n}}", fields)
}

#[test]
fn crossref_fixture_decodes_amp_and_normalizes_month() {
    // A CrossRef-shaped fixture with `&amp;` in the title and `month = {June}`.
    let input = entry(
        "  title = {Causality &amp; Counterfactuals},\n  author = {A B},\n  month = {June},\n  year = {2020}",
    );
    let out = sanitize_bibtex(&input);
    assert!(out.text.contains(r"Causality \& Counterfactuals"), "got: {}", out.text);
    assert!(!out.text.contains("&amp;"));
    assert!(out.text.contains("month = jun"), "got: {}", out.text);
    assert!(!out.text.contains("{June}"));
    assert!(out.changed);
}

#[test]
fn numeric_entities_decoded_then_escaped() {
    let input = entry("  title = {A &#38; B &#x26; C},\n  year = {2020}");
    let out = sanitize_bibtex(&input);
    assert!(out.text.contains(r"A \& B \& C"), "got: {}", out.text);
    assert!(!out.text.contains("&#"));
}

#[test]
fn lt_gt_entities_become_textless_textgreater() {
    let input = entry("  title = {p &lt; 0.05 &gt; q},\n  year = {2020}");
    let out = sanitize_bibtex(&input);
    assert!(out.text.contains(r"\textless"), "got: {}", out.text);
    assert!(out.text.contains(r"\textgreater"), "got: {}", out.text);
    assert!(!out.text.contains("&lt;"));
    assert!(!out.text.contains("&gt;"));
}

#[test]
fn abbreviation_months_normalize() {
    // `Sept` and `Sep.` are common abbreviations that must map to the `sep` macro.
    let input = entry("  title = {T},\n  month = {Sept},\n  year = {2020}");
    let out = sanitize_bibtex(&input);
    assert!(out.text.contains("month = sep"), "got: {}", out.text);

    let input2 = entry("  title = {T},\n  month = {Sep.},\n  year = {2020}");
    let out2 = sanitize_bibtex(&input2);
    assert!(out2.text.contains("month = sep"), "got: {}", out2.text);
}

#[test]
fn unicode_dashes_normalize_to_latex_dashes() {
    // En dash in pages -> `--`; em dash in title -> `---`.
    // (Observed in the wild: the 2026-06-09 AAAI add emitted pages={26841–26850}.)
    let input = entry("  title = {Causes \u{2014} and Effects},\n  pages = {26841\u{2013}26850},\n  year = {2020}");
    let out = sanitize_bibtex(&input);
    assert!(out.text.contains("26841--26850"), "got: {}", out.text);
    assert!(out.text.contains("Causes --- and Effects"), "got: {}", out.text);
    assert!(!out.text.contains('\u{2013}'));
    assert!(!out.text.contains('\u{2014}'));
}

#[test]
fn url_doi_fields_and_url_spans_are_exempt() {
    let input = entry(
        "  title = {T},\n  url = {https://example.com/?a=1&b=2},\n  doi = {10.1000/a&b},\n  howpublished = {See \\url{https://example.com/?c=3&d=4} for details},\n  year = {2020}",
    );
    let out = sanitize_bibtex(&input);
    assert!(out.text.contains("https://example.com/?a=1&b=2"), "url field must survive: {}", out.text);
    assert!(out.text.contains("10.1000/a&b"), "doi field must survive: {}", out.text);
    assert!(out.text.contains(r"\url{https://example.com/?c=3&d=4}"), "url span must survive: {}", out.text);
    assert!(!out.text.contains(r"a=1\&"));
    assert!(!out.text.contains(r"10.1000/a\&"));
    assert!(!out.text.contains(r"c=3\&"));
}

#[test]
fn unmappable_month_passes_through_with_warning() {
    let input = entry("  title = {T},\n  month = {June 2020},\n  year = {2020}");
    let out = sanitize_bibtex(&input);
    assert!(out.text.contains("month = {June 2020}"), "got: {}", out.text);
    assert!(
        out.warnings.iter().any(|w| w.contains("month")),
        "expected a month warning, got: {:?}",
        out.warnings
    );
}

#[test]
fn sanitize_is_idempotent() {
    let input = entry(
        "  title = {A &amp; B \u{2014} C &lt;D&gt;},\n  pages = {1\u{2013}2},\n  month = {June},\n  year = {2020}",
    );
    let once = sanitize_bibtex(&input);
    let twice = sanitize_bibtex(&once.text);
    assert_eq!(once.text, twice.text);
    assert!(!twice.changed, "second sanitize must be a no-op");
}

#[test]
fn already_escaped_ampersand_not_double_escaped() {
    let input = entry("  title = {A \\& B},\n  year = {2020}");
    let out = sanitize_bibtex(&input);
    assert!(out.text.contains(r"A \& B"), "got: {}", out.text);
    assert!(!out.text.contains(r"\\&"));
}

#[test]
fn upsert_to_file_sanitizes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("refs.bib");
    let raw = entry("  title = {A &amp; B},\n  author = {X},\n  month = {June},\n  year = {2020}");
    lit::bibtex::upsert_to_file(&path, &raw, false).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains(r"A \& B"), "got: {}", content);
    assert!(content.contains("month = jun"), "got: {}", content);
    assert!(!content.contains("&amp;"));
}

#[test]
fn append_to_file_sanitizes_bib_flag_path() {
    // `-b/--bib` appends via bibtex::append_to_file (cmd/mod.rs handle_bib);
    // sanitizing here covers that second emission path by construction.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("refs.bib");
    let raw = entry("  title = {A &amp; B},\n  author = {X},\n  pages = {1\u{2013}2},\n  year = {2020}");
    lit::bibtex::append_to_file(&path, &raw).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains(r"A \& B"), "got: {}", content);
    assert!(content.contains("1--2"), "got: {}", content);
    assert!(!content.contains("&amp;"));
}
