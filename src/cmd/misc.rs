/// `mcp__lit__misc` — Insert a `@misc` BibTeX entry from a blog post, forum post,
/// or other unpublished work that has no arXiv ID or DOI.

use std::path::Path;

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
pub fn run_data(params: &MiscParams, bib_file: &Path) -> Result<AddResult, Box<dyn std::error::Error>> {
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

    crate::bibtex::upsert_to_file(bib_file, &bib_text)?;

    Ok(AddResult {
        entry_key: params.citekey.clone(),
        bib_text,
    })
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
        let result = run_data(&params, tmp.path()).unwrap();
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
        let result = run_data(&params, tmp.path()).unwrap();
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
        run_data(&params, tmp.path()).unwrap();
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
        let result = run_data(&params, tmp.path()).unwrap();
        assert!(result.bib_text.contains("note = {Accessed: 2024-01-01}"));
    }
}
