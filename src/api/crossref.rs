use super::{urlencode, PaperResult};
use serde_json::Value;

/// Build URL for a general search query.
pub fn search_url(query: &str, limit: usize) -> String {
    format!(
        "https://api.crossref.org/works?query={}&rows={}&sort=relevance",
        urlencode(query),
        limit
    )
}

/// Build URL for looking up a single DOI.
pub fn doi_url(doi: &str) -> String {
    format!("https://api.crossref.org/works/{}", doi)
}

/// Build URL that returns raw BibTeX for a DOI (not JSON).
pub fn bibtex_url(doi: &str) -> String {
    format!(
        "https://api.crossref.org/works/{}/transform/application/x-bibtex",
        doi
    )
}

/// Parse response from the search endpoint.
pub fn parse_search(body: &str) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>> {
    let data: Value = serde_json::from_str(body)?;
    let items = data["message"]["items"]
        .as_array()
        .ok_or("missing message.items array")?;

    let mut results = Vec::with_capacity(items.len());

    for w in items {
        let title = w["title"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_str())
            .unwrap_or("N/A")
            .to_string();

        let year = extract_year(&w["published"]);

        let authors: Vec<String> = w["author"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|a| {
                        let given = a["given"].as_str().unwrap_or("");
                        let family = a["family"].as_str().unwrap_or("");
                        if family.is_empty() {
                            None
                        } else {
                            Some(format!("{} {}", given, family).trim().to_string())
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        let doi = w["DOI"].as_str().map(|s| s.to_string());

        let citations = w["is-referenced-by-count"].as_u64();

        results.push(PaperResult {
            title,
            authors,
            year,
            doi,
            citations,
            ..Default::default()
        });
    }

    Ok(results)
}

/// Parse response from the single-DOI endpoint.
pub fn parse_doi(body: &str) -> Result<PaperResult, Box<dyn std::error::Error>> {
    let data: Value = serde_json::from_str(body)?;
    let msg = &data["message"];

    let title = msg["title"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .unwrap_or("N/A")
        .to_string();

    let year = extract_year(&msg["published"]);

    let authors: Vec<String> = msg["author"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    let given = a["given"].as_str().unwrap_or("");
                    let family = a["family"].as_str().unwrap_or("");
                    if family.is_empty() {
                        None
                    } else {
                        Some(format!("{} {}", given, family).trim().to_string())
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let venue = msg["container-title"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let doi = msg["DOI"].as_str().map(|s| s.to_string());

    Ok(PaperResult {
        title,
        authors,
        year,
        doi,
        venue,
        ..Default::default()
    })
}

/// Extract year from a CrossRef `published` (or `created`) object.
///
/// Structure: `{ "date-parts": [[2021, 3, 15]] }`.
fn extract_year(published: &Value) -> String {
    published["date-parts"]
        .as_array()
        .and_then(|outer| outer.first())
        .and_then(|inner| inner.as_array())
        .and_then(|parts| parts.first())
        .and_then(|y| y.as_u64())
        .map(|y| y.to_string())
        .unwrap_or_else(|| "?".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_search_basic() {
        let body = r#"{
            "message": {
                "items": [
                    {
                        "title": ["Attention Is All You Need"],
                        "author": [
                            {"given": "Ashish", "family": "Vaswani"},
                            {"given": "Noam", "family": "Shazeer"}
                        ],
                        "published": {"date-parts": [[2017, 6, 12]]},
                        "DOI": "10.5555/3295222.3295349",
                        "is-referenced-by-count": 90000
                    }
                ]
            }
        }"#;
        let results = parse_search(body).unwrap();
        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert_eq!(r.title, "Attention Is All You Need");
        assert_eq!(r.year, "2017");
        assert_eq!(r.authors, vec!["Ashish Vaswani", "Noam Shazeer"]);
        assert_eq!(r.doi.as_deref(), Some("10.5555/3295222.3295349"));
        assert_eq!(r.citations, Some(90000));
    }

    #[test]
    fn test_parse_search_empty_items() {
        let body = r#"{"message": {"items": []}}"#;
        let results = parse_search(body).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_parse_search_missing_optional_fields() {
        let body = r#"{
            "message": {
                "items": [
                    {
                        "title": ["Some Paper"]
                    }
                ]
            }
        }"#;
        let results = parse_search(body).unwrap();
        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert_eq!(r.title, "Some Paper");
        assert_eq!(r.year, "?");
        assert!(r.authors.is_empty());
        assert!(r.doi.is_none());
        assert!(r.citations.is_none());
    }

    #[test]
    fn test_parse_search_author_family_only() {
        let body = r#"{
            "message": {
                "items": [
                    {
                        "title": ["Test"],
                        "author": [{"family": "Pearl"}],
                        "published": {"date-parts": [[2000]]}
                    }
                ]
            }
        }"#;
        let results = parse_search(body).unwrap();
        assert_eq!(results[0].authors, vec!["Pearl"]);
    }

    #[test]
    fn test_parse_search_author_empty_family_skipped() {
        let body = r#"{
            "message": {
                "items": [
                    {
                        "title": ["Test"],
                        "author": [{"given": "Org", "family": ""}]
                    }
                ]
            }
        }"#;
        let results = parse_search(body).unwrap();
        assert!(results[0].authors.is_empty());
    }

    #[test]
    fn test_parse_doi_basic() {
        let body = r#"{
            "message": {
                "title": ["Causality"],
                "author": [
                    {"given": "Judea", "family": "Pearl"}
                ],
                "published": {"date-parts": [[2009]]},
                "container-title": ["Cambridge University Press"],
                "DOI": "10.1017/CBO9780511803161"
            }
        }"#;
        let result = parse_doi(body).unwrap();
        assert_eq!(result.title, "Causality");
        assert_eq!(result.year, "2009");
        assert_eq!(result.authors, vec!["Judea Pearl"]);
        assert_eq!(result.venue.as_deref(), Some("Cambridge University Press"));
        assert_eq!(result.doi.as_deref(), Some("10.1017/CBO9780511803161"));
    }

    #[test]
    fn test_parse_doi_missing_optional_fields() {
        let body = r#"{
            "message": {
                "title": ["Minimal"]
            }
        }"#;
        let result = parse_doi(body).unwrap();
        assert_eq!(result.title, "Minimal");
        assert_eq!(result.year, "?");
        assert!(result.authors.is_empty());
        assert!(result.venue.is_none());
        assert!(result.doi.is_none());
    }

    #[test]
    fn test_extract_year_valid() {
        let v: Value = serde_json::from_str(r#"{"date-parts": [[2021, 3, 15]]}"#).unwrap();
        assert_eq!(extract_year(&v), "2021");
    }

    #[test]
    fn test_extract_year_missing() {
        let v: Value = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(extract_year(&v), "?");
    }
}
