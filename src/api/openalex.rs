use super::{extract_last_name, urlencode, PaperResult};
use regex::Regex;
use serde_json::Value;
use crate::sanitize::decode_html_entities;

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

/// Short OpenAlex id (`W123`) from the URL form (`https://openalex.org/W123`).
fn short_id(id: &str) -> String {
    id.rsplit('/').next().unwrap_or(id).to_string()
}

/// A work's own short id and the short ids of the works it references.
pub fn parse_work_graph(body: &str) -> Result<(String, Vec<String>), Box<dyn std::error::Error>> {
    let data: Value = serde_json::from_str(body)?;
    let id = data["id"].as_str().map(short_id).ok_or("OpenAlex work has no id")?;
    let refs = data["referenced_works"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).map(short_id).collect())
        .unwrap_or_default();
    Ok((id, refs))
}

/// Maximum ids per `openalex:` OR-filter (OpenAlex caps OR filters at 100 values).
pub const IDS_PER_REQUEST: usize = 100;

/// Build URL fetching the works with the given short ids (at most `IDS_PER_REQUEST`).
pub fn works_by_ids_url(ids: &[String]) -> String {
    format!(
        "https://api.openalex.org/works?filter=openalex:{}&per-page={}",
        ids.join("|"),
        IDS_PER_REQUEST
    )
}

/// Build URL for one cursor page of the works citing `work_id`.
pub fn cited_by_url(work_id: &str, cursor: &str) -> String {
    format!(
        "https://api.openalex.org/works?filter=cites:{}&per-page=200&cursor={}",
        work_id,
        urlencode(cursor)
    )
}

/// One page of works (full author names) and the cursor of the next page, if any.
pub fn parse_works_page(body: &str) -> Result<(Vec<PaperResult>, Option<String>), Box<dyn std::error::Error>> {
    let data: Value = serde_json::from_str(body)?;
    let cursor = data["meta"]["next_cursor"].as_str().map(|s| s.to_string());
    Ok((parse_works(&data)?, cursor))
}

/// Parse response from the general search endpoint (authors as last names).
pub fn parse_search(body: &str) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>> {
    let data: Value = serde_json::from_str(body)?;
    let mut results = parse_works(&data)?;
    for p in &mut results {
        for a in &mut p.authors {
            *a = extract_last_name(a).to_string();
        }
    }
    Ok(results)
}

/// Shared parser for the `results` array of OpenAlex works responses.
fn parse_works(data: &Value) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>> {
    let works = data
        .get("results")
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
                    .filter_map(|a| a["author"]["display_name"].as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let citations = w["cited_by_count"].as_u64();

        let doi = w["doi"]
            .as_str()
            .map(|d| d.trim_start_matches("https://doi.org/").to_string())
            .filter(|d| !d.is_empty());
        let arxiv_id = doi.as_deref().and_then(crate::detect::arxiv_id_from_doi);
        let venue = w["primary_location"]["source"]["display_name"]
            .as_str()
            .map(|s| s.to_string());

        results.push(PaperResult {
            title,
            authors,
            year,
            doi,
            arxiv_id,
            venue,
            citations,
            ..Default::default()
        });
    }

    Ok(results)
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
    fn test_parse_work_graph_shortens_ids() {
        let body = r#"{
            "id": "https://openalex.org/W9",
            "referenced_works": ["https://openalex.org/W1", "https://openalex.org/W2"]
        }"#;
        let (id, refs) = parse_work_graph(body).unwrap();
        assert_eq!(id, "W9");
        assert_eq!(refs, vec!["W1", "W2"]);
    }

    #[test]
    fn test_parse_work_graph_requires_id() {
        assert!(parse_work_graph(r#"{"referenced_works": []}"#).is_err());
    }

    #[test]
    fn test_works_by_ids_url_joins_with_or() {
        let url = works_by_ids_url(&["W1".to_string(), "W2".to_string()]);
        assert!(url.contains("filter=openalex:W1|W2"), "url: {}", url);
    }

    #[test]
    fn test_cited_by_url_carries_cursor() {
        let url = cited_by_url("W9", "*");
        assert!(url.contains("filter=cites:W9"), "url: {}", url);
        assert!(url.contains("cursor=%2A") || url.contains("cursor=*"), "url: {}", url);
    }

    #[test]
    fn test_parse_works_page_keeps_full_names_venue_and_arxiv() {
        let body = r#"{
            "meta": {"next_cursor": "abc"},
            "results": [{
                "title": "Paper",
                "publication_year": 2024,
                "authorships": [{"author": {"display_name": "Ada Lovelace"}}],
                "doi": "https://doi.org/10.48550/arXiv.2408.01416",
                "primary_location": {"source": {"display_name": "arXiv"}}
            }]
        }"#;
        let (papers, cursor) = parse_works_page(body).unwrap();
        assert_eq!(cursor.as_deref(), Some("abc"));
        let p = &papers[0];
        assert_eq!(p.authors, vec!["Ada Lovelace"]);
        assert_eq!(p.venue.as_deref(), Some("arXiv"));
        assert_eq!(p.arxiv_id.as_deref(), Some("2408.01416"));
        assert_eq!(p.year, "2024");
    }

    #[test]
    fn test_parse_works_page_last_page_has_no_cursor() {
        let (papers, cursor) = parse_works_page(r#"{"meta": {"next_cursor": null}, "results": []}"#).unwrap();
        assert!(papers.is_empty());
        assert!(cursor.is_none());
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
}

