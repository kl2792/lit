use super::{extract_last_name, urlencode, PaperResult};
use regex::Regex;
use serde_json::Value;

/// Build URL for looking up a single work by DOI.
pub fn work_by_doi_url(doi: &str) -> String {
    format!("https://api.openalex.org/works/doi:{}", doi)
}

/// Parse response from the single-work endpoint.
///
/// Extracts openalex_id, citation count, and open-access URL.
pub fn parse_work(body: &str) -> Result<WorkResult, Box<dyn std::error::Error>> {
    let data: Value = serde_json::from_str(body)?;

    let openalex_id = data["id"]
        .as_str()
        .map(|s| s.to_string());
    let citations = data["cited_by_count"].as_u64();
    let oa_url = data["open_access"]["oa_url"]
        .as_str()
        .map(|s| s.to_string());

    Ok(WorkResult {
        openalex_id,
        citations,
        oa_url,
    })
}

/// Minimal result from a single OpenAlex work lookup (for enrichment).
#[derive(Debug, Clone, Default)]
pub struct WorkResult {
    pub openalex_id: Option<String>,
    pub citations: Option<u64>,
    pub oa_url: Option<String>,
}

/// Build URL for a general search query.
pub fn search_url(query: &str, limit: usize) -> String {
    format!(
        "https://api.openalex.org/works?search={}&per-page={}",
        urlencode(query),
        limit
    )
}

/// Build URL for a title-specific search (used by verify).
pub fn title_search_url(title: &str, limit: usize) -> String {
    format!(
        "https://api.openalex.org/works?filter=title.search:{}&per-page={}",
        urlencode(title),
        limit
    )
}

/// Parse response from the general search endpoint.
pub fn parse_search(body: &str) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>> {
    parse_works_response(body, "results")
}

/// Shared parser for OpenAlex works responses.
fn parse_works_response(
    body: &str,
    array_key: &str,
) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>> {
    let data: Value = serde_json::from_str(body)?;
    let works = data
        .get(array_key)
        .and_then(|v| v.as_array())
        .ok_or("missing results array")?;

    let html_re = Regex::new(r"<[^>]+>")?;
    let mut results = Vec::with_capacity(works.len());

    for w in works {
        let raw_title = w["title"].as_str().unwrap_or("N/A");
        let title = decode_html_entities(&html_re.replace_all(raw_title, ""));

        let year = match w["publication_year"].as_u64() {
            Some(y) => y.to_string(),
            None => "?".to_string(),
        };

        let authors: Vec<String> = w["authorships"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|a| {
                        a["author"]["display_name"]
                            .as_str()
                            .map(|name| extract_last_name(name).to_string())
                    })
                    .collect()
            })
            .unwrap_or_default();

        let citations = w["cited_by_count"].as_u64();

        let doi = w["doi"]
            .as_str()
            .map(|d| d.trim_start_matches("https://doi.org/").to_string())
            .filter(|d| !d.is_empty());

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

/// Decode common HTML entities to their plain-text equivalents.
fn decode_html_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_search_basic() {
        let body = r#"{
            "results": [
                {
                    "title": "Attention Is All You Need",
                    "publication_year": 2017,
                    "authorships": [
                        {"author": {"display_name": "Ashish Vaswani"}},
                        {"author": {"display_name": "Noam Shazeer"}}
                    ],
                    "cited_by_count": 90000,
                    "doi": "https://doi.org/10.5555/3295222.3295349"
                }
            ]
        }"#;
        let results = parse_search(body).unwrap();
        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert_eq!(r.title, "Attention Is All You Need");
        assert_eq!(r.year, "2017");
        assert_eq!(r.authors, vec!["Vaswani", "Shazeer"]);
        assert_eq!(r.citations, Some(90000));
        assert_eq!(r.doi.as_deref(), Some("10.5555/3295222.3295349"));
    }

    #[test]
    fn test_parse_search_strips_html_tags() {
        let body = r#"{
            "results": [
                {
                    "title": "A <i>Bold</i> Claim &amp; More",
                    "publication_year": 2020,
                    "authorships": [],
                    "doi": null
                }
            ]
        }"#;
        let results = parse_search(body).unwrap();
        assert_eq!(results[0].title, "A Bold Claim & More");
    }

    #[test]
    fn test_parse_search_empty_results() {
        let body = r#"{"results": []}"#;
        let results = parse_search(body).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_parse_search_missing_optional_fields() {
        let body = r#"{
            "results": [
                {
                    "title": "Minimal"
                }
            ]
        }"#;
        let results = parse_search(body).unwrap();
        let r = &results[0];
        assert_eq!(r.title, "Minimal");
        assert_eq!(r.year, "?");
        assert!(r.authors.is_empty());
        assert!(r.doi.is_none());
        assert!(r.citations.is_none());
    }

    #[test]
    fn test_parse_search_doi_stripping() {
        let body = r#"{
            "results": [
                {
                    "title": "Test",
                    "doi": "https://doi.org/10.1234/test"
                }
            ]
        }"#;
        let results = parse_search(body).unwrap();
        assert_eq!(results[0].doi.as_deref(), Some("10.1234/test"));
    }

    #[test]
    fn test_work_by_doi_url() {
        let url = work_by_doi_url("10.1234/test");
        assert_eq!(url, "https://api.openalex.org/works/doi:10.1234/test");
    }

    #[test]
    fn test_parse_work_full() {
        let body = r#"{
            "id": "https://openalex.org/W123",
            "cited_by_count": 42,
            "open_access": {
                "oa_url": "https://example.com/paper.pdf"
            }
        }"#;
        let r = parse_work(body).unwrap();
        assert_eq!(r.openalex_id.as_deref(), Some("https://openalex.org/W123"));
        assert_eq!(r.citations, Some(42));
        assert_eq!(r.oa_url.as_deref(), Some("https://example.com/paper.pdf"));
    }

    #[test]
    fn test_parse_work_minimal() {
        let body = r#"{}"#;
        let r = parse_work(body).unwrap();
        assert!(r.openalex_id.is_none());
        assert!(r.citations.is_none());
        assert!(r.oa_url.is_none());
    }

    #[test]
    fn test_decode_html_entities() {
        assert_eq!(decode_html_entities("A &amp; B"), "A & B");
        assert_eq!(decode_html_entities("&lt;tag&gt;"), "<tag>");
        assert_eq!(decode_html_entities("it&#39;s"), "it's");
        assert_eq!(decode_html_entities("it&#x27;s"), "it's");
        assert_eq!(decode_html_entities("it&apos;s"), "it's");
        assert_eq!(decode_html_entities("&quot;hi&quot;"), "\"hi\"");
    }
}

