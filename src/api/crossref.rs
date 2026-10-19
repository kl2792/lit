use super::{urlencode, PaperResult};
use crate::html;
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
            abstract_text: jats_abstract(&w["abstract"]),
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

/// JATS elements that separate text runs; inline ones (italic, sub) do not.
const JATS_BLOCKS: &[&str] = &["jats:title", "jats:p", "jats:sec", "jats:list-item"];

/// Plain text of a Crossref `abstract` (JATS XML): tags stripped, entities
/// decoded, whitespace collapsed, and a leading "Abstract" heading dropped.
/// `None` when absent or empty.
fn jats_abstract(jats: &Value) -> Option<String> {
    let mut raw = String::new();
    jats_text(&html::parse(jats.as_str()?), &mut raw);
    let text = html::collapse_whitespace(&raw);
    let text = text
        .strip_prefix("Abstract")
        .filter(|rest| rest.is_empty() || rest.starts_with(' '))
        .map_or(text.as_str(), str::trim_start);
    (!text.is_empty()).then(|| text.to_string())
}

fn jats_text(nodes: &[html::Node], out: &mut String) {
    for node in nodes {
        match node {
            html::Node::Text(t) => out.push_str(t),
            html::Node::Element(e) => {
                let block = JATS_BLOCKS.contains(&e.name.as_str());
                if block {
                    out.push(' ');
                }
                jats_text(&e.children, out);
                if block {
                    out.push(' ');
                }
            }
        }
    }
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
    fn test_parse_search_reads_jats_abstract_as_plain_text() {
        let body = r#"{"message": {"items": [
            {"title": ["With"], "abstract": "<jats:title>Abstract</jats:title><jats:p>Causal  <jats:italic>credit</jats:italic>\n assignment &amp; Shapley values.</jats:p><jats:p>Second &lt;para&gt;.</jats:p>"},
            {"title": ["Without"]}
        ]}}"#;
        let results = parse_search(body).unwrap();
        assert_eq!(
            results[0].abstract_text.as_deref(),
            Some("Causal credit assignment & Shapley values. Second <para>.")
        );
        assert_eq!(results[1].abstract_text, None);
    }

    #[test]
    fn test_jats_abstract_keeps_section_titles_and_drops_empty() {
        let v = serde_json::json!("<jats:sec><jats:title>Background</jats:title><jats:p>Text.</jats:p></jats:sec>");
        assert_eq!(jats_abstract(&v).as_deref(), Some("Background Text."));
        assert_eq!(jats_abstract(&serde_json::json!("<jats:p> </jats:p>")), None);
        assert_eq!(jats_abstract(&serde_json::json!("<jats:title>Abstract</jats:title>")), None);
        assert_eq!(jats_abstract(&Value::Null), None);
        assert_eq!(
            jats_abstract(&serde_json::json!("<jats:p>Abstracting away details.</jats:p>")).as_deref(),
            Some("Abstracting away details.")
        );
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
