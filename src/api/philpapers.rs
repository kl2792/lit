use super::{urlencode, PaperResult};
use serde_json::Value;

// -- ID extraction -----------------------------------------------------------

/// Extract the PhilPapers entry ID from a rec URL.
///
/// `https://philpapers.org/rec/ANDCIT-6` → `ANDCIT-6`
pub fn extract_id(url: &str) -> Option<String> {
    url.split("/rec/")
        .nth(1)
        .map(|s| s.trim_end_matches('/').to_string())
}

// -- URL builders ------------------------------------------------------------

/// BibTeX export URL for a single entry.
///
/// PhilPapers serves BibTeX at `{rec_url}.bib` (same pattern as DBLP).
pub fn bib_url(id: &str) -> String {
    format!("https://philpapers.org/rec/{}.bib", id)
}

/// JSON API search URL.
pub fn search_url(query: &str, limit: usize) -> String {
    format!(
        "https://philpapers.org/api/search?q={}&format=json&limit={}",
        urlencode(query),
        limit
    )
}

// -- Parsers -----------------------------------------------------------------

/// Parse a single BibTeX entry fetched from PhilPapers into a `PaperResult`.
///
/// PhilPapers BibTeX uses standard fields: `title`, `author`, `year`,
/// `journal`/`booktitle`, `doi`. Authors are comma-and-and-separated.
pub fn parse_bib_entry(bib: &str) -> PaperResult {
    let entries = crate::bibtex::parse_bib_file(bib);
    let Some(entry) = entries.first() else {
        return PaperResult::default();
    };

    let title = entry.get_field("title").unwrap_or("").to_string();
    let year = entry.get_field("year").unwrap_or("?").to_string();
    let doi = entry.get_field("doi").map(str::to_string);

    let author_raw = entry.get_field("author").unwrap_or("");
    let authors: Vec<String> = author_raw
        .split(" and ")
        .map(|a| {
            let trimmed = a.trim();
            // "Last, First" → "First Last"
            if let Some((last, first)) = trimmed.split_once(',') {
                format!("{} {}", first.trim(), last.trim())
            } else {
                trimmed.to_string()
            }
        })
        .filter(|s| !s.is_empty())
        .collect();

    let venue = entry
        .get_field("journal")
        .or_else(|| entry.get_field("booktitle"))
        .map(str::to_string);

    PaperResult {
        title,
        authors,
        year,
        doi,
        venue,
        ..Default::default()
    }
}

/// Parse PhilPapers JSON search results.
///
/// PhilPapers returns a top-level array of entry objects.
/// Each entry has: `eId`, `title`, `authorsStr`, `pubYear`, `pub` (venue).
pub fn parse_search(body: &str) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>> {
    let data: Value = serde_json::from_str(body)?;
    let entries = match data.as_array() {
        Some(arr) => arr.clone(),
        None => {
            // Some versions wrap in {"entries": [...]}
            data["entries"]
                .as_array()
                .cloned()
                .ok_or("expected JSON array or {entries: [...]}")?
        }
    };

    let mut results = Vec::with_capacity(entries.len());

    for entry in &entries {
        let title = entry["title"].as_str().unwrap_or("N/A").to_string();
        let year = entry["pubYear"]
            .as_str()
            .or_else(|| entry["year"].as_str())
            .unwrap_or("?")
            .to_string();

        let authors = parse_authors_field(entry);

        let venue = entry["pub"]
            .as_str()
            .or_else(|| entry["journal"].as_str())
            .map(str::to_string);

        let doi = entry["doi"].as_str().map(str::to_string);

        let id = entry["eId"].as_str().unwrap_or("").to_string();
        let pdf_url = if !id.is_empty() {
            Some(format!("https://philpapers.org/rec/{}", id))
        } else {
            None
        };

        results.push(PaperResult {
            title,
            authors,
            year,
            doi,
            venue,
            pdf_url,
            ..Default::default()
        });
    }

    Ok(results)
}

/// Parse the authors field, which may be:
/// - `authorsStr`: a pre-formatted string like "Anderson, J., Smith, B."
/// - `authors`: an array of objects with `name` field
fn parse_authors_field(entry: &Value) -> Vec<String> {
    // Prefer structured `authors` array
    if let Some(arr) = entry["authors"].as_array() {
        return arr
            .iter()
            .filter_map(|a| {
                a["name"].as_str().map(|s| {
                    // "Last, First" → "First Last"
                    if let Some((last, first)) = s.split_once(',') {
                        format!("{} {}", first.trim(), last.trim())
                    } else {
                        s.to_string()
                    }
                })
            })
            .collect();
    }

    // Fall back to authorsStr
    if let Some(s) = entry["authorsStr"].as_str() {
        return s
            .split(", ")
            .map(|a| a.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    vec![]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_id_standard() {
        assert_eq!(
            extract_id("https://philpapers.org/rec/ANDCIT-6"),
            Some("ANDCIT-6".to_string())
        );
    }

    #[test]
    fn extract_id_trailing_slash() {
        assert_eq!(
            extract_id("https://philpapers.org/rec/ANDCIT-6/"),
            Some("ANDCIT-6".to_string())
        );
    }

    #[test]
    fn extract_id_no_rec_path() {
        assert_eq!(extract_id("https://philpapers.org/"), None);
    }

    #[test]
    fn bib_url_format() {
        assert_eq!(
            bib_url("ANDCIT-6"),
            "https://philpapers.org/rec/ANDCIT-6.bib"
        );
    }

    #[test]
    fn search_url_encodes_query() {
        let url = search_url("causal inference", 5);
        assert!(url.contains("causal%20inference"));
        assert!(url.contains("limit=5"));
        assert!(url.contains("format=json"));
    }

    #[test]
    fn parse_bib_entry_standard() {
        let bib = r#"@article{AndersonCIT2020,
  title = {Causation in the Law},
  author = {Anderson, James and Smith, Barbara},
  year = {2020},
  journal = {Legal Theory},
  doi = {10.1017/S1352325220000123},
}"#;
        let result = parse_bib_entry(bib);
        assert_eq!(result.title, "Causation in the Law");
        assert_eq!(result.year, "2020");
        assert_eq!(result.authors, vec!["James Anderson", "Barbara Smith"]);
        assert_eq!(result.venue.as_deref(), Some("Legal Theory"));
        assert_eq!(result.doi.as_deref(), Some("10.1017/S1352325220000123"));
    }

    #[test]
    fn parse_bib_entry_missing_fields() {
        let bib = "@article{Key2020, title = {Minimal},}";
        let result = parse_bib_entry(bib);
        assert_eq!(result.title, "Minimal");
        assert!(result.authors.is_empty());
        assert!(result.doi.is_none());
    }

    #[test]
    fn parse_search_array_format() {
        let body = r#"[
            {
                "eId": "ANDCIT-6",
                "title": "Causation in the Law",
                "pubYear": "2020",
                "pub": "Legal Theory",
                "doi": "10.1017/example",
                "authors": [
                    {"name": "Anderson, James"},
                    {"name": "Smith, Barbara"}
                ]
            }
        ]"#;
        let results = parse_search(body).unwrap();
        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert_eq!(r.title, "Causation in the Law");
        assert_eq!(r.year, "2020");
        assert_eq!(r.venue.as_deref(), Some("Legal Theory"));
        assert_eq!(r.doi.as_deref(), Some("10.1017/example"));
        assert_eq!(r.pdf_url.as_deref(), Some("https://philpapers.org/rec/ANDCIT-6"));
        assert_eq!(r.authors, vec!["James Anderson", "Barbara Smith"]);
    }

    #[test]
    fn parse_search_wrapped_format() {
        let body = r#"{"entries": [{"title": "Test", "pubYear": "2021"}]}"#;
        let results = parse_search(body).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Test");
    }

    #[test]
    fn parse_search_authorsstr_fallback() {
        let body = r#"[{"title": "Test", "pubYear": "2021", "authorsStr": "Pearl, Judea, Mackenzie, Dana"}]"#;
        let results = parse_search(body).unwrap();
        assert!(!results[0].authors.is_empty());
    }

    #[test]
    fn parse_search_empty() {
        let results = parse_search("[]").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn parse_authors_structured() {
        let entry = serde_json::json!({
            "authors": [{"name": "Pearl, Judea"}, {"name": "Mackenzie, Dana"}]
        });
        let authors = parse_authors_field(&entry);
        assert_eq!(authors, vec!["Judea Pearl", "Dana Mackenzie"]);
    }

    #[test]
    fn parse_authors_already_firstlast() {
        let entry = serde_json::json!({
            "authors": [{"name": "Judea Pearl"}]
        });
        let authors = parse_authors_field(&entry);
        assert_eq!(authors, vec!["Judea Pearl"]);
    }
}
