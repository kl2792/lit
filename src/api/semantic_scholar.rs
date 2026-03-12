use super::{extract_last_name, urlencode, PaperResult};
use serde_json::Value;

/// Build URL for a single paper lookup by identifier.
///
/// `id` can be an arXiv ID (use `arXiv:{id}`), DOI, or S2 paper ID.
pub fn paper_url(id: &str) -> String {
    format!(
        "https://api.semanticscholar.org/graph/v1/paper/{}?fields=paperId,externalIds,venue,citationCount,openAccessPdf",
        id
    )
}

/// Parse response from the single-paper endpoint.
///
/// Extracts s2_id, DOI, venue, citation count, and open-access PDF URL.
pub fn parse_paper(body: &str) -> Result<PaperResult, Box<dyn std::error::Error>> {
    let data: Value = serde_json::from_str(body)?;

    let s2_id = data["paperId"].as_str().map(|s| s.to_string());
    let ext = &data["externalIds"];
    let doi = ext["DOI"].as_str().map(|s| s.to_string());
    let arxiv_id = ext["ArXiv"].as_str().map(|s| s.to_string());
    let venue = data["venue"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let citations = data["citationCount"].as_u64();
    let pdf_url = data["openAccessPdf"]["url"]
        .as_str()
        .map(|s| s.to_string());

    Ok(PaperResult {
        s2_id,
        doi,
        arxiv_id,
        venue,
        citations,
        pdf_url,
        ..Default::default()
    })
}

/// Build URL for paper search.
pub fn search_url(query: &str, limit: usize) -> String {
    format!(
        "https://api.semanticscholar.org/graph/v1/paper/search?query={}&limit={}&fields=title,authors,year,citationCount,externalIds",
        urlencode(query),
        limit
    )
}

/// Build URL for a paper's references.
///
/// `paper_id` can be an arXiv ID, Semantic Scholar ID, or DOI.
/// Bare DOIs (matching `10.\d{4,}/`) must be prefixed with `DOI:`.
pub fn refs_url(paper_id: &str) -> String {
    let id = normalize_paper_id(paper_id);
    format!(
        "https://api.semanticscholar.org/graph/v1/paper/{}/references?fields=title,authors,year,externalIds&limit=50",
        id
    )
}

/// Build URL for papers citing a given paper.
///
/// Same DOI-prefix convention as `refs_url`.
pub fn cites_url(paper_id: &str) -> String {
    let id = normalize_paper_id(paper_id);
    format!(
        "https://api.semanticscholar.org/graph/v1/paper/{}/citations?fields=title,authors,year,externalIds&limit=50",
        id
    )
}

/// Parse response from the search endpoint.
pub fn parse_search(body: &str) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>> {
    let data: Value = serde_json::from_str(body)?;
    let papers = data
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or("missing data array")?;

    let mut results = Vec::with_capacity(papers.len());

    for p in papers {
        let title = p["title"].as_str().unwrap_or("N/A").to_string();

        let year = match p["year"].as_u64() {
            Some(y) => y.to_string(),
            None => "?".to_string(),
        };

        let authors: Vec<String> = p["authors"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|a| {
                        a["name"]
                            .as_str()
                            .map(|name| extract_last_name(name).to_string())
                    })
                    .collect()
            })
            .unwrap_or_default();

        let citations = p["citationCount"].as_u64();

        let ext = &p["externalIds"];
        let arxiv_id = ext["ArXiv"].as_str().map(|s| s.to_string());
        let doi = ext["DOI"].as_str().map(|s| s.to_string());

        results.push(PaperResult {
            title,
            authors,
            year,
            doi,
            arxiv_id,
            citations,
            ..Default::default()
        });
    }

    Ok(results)
}

/// Parse response from the references endpoint.
///
/// Each item in `data[]` has a `citedPaper` object.
pub fn parse_refs(body: &str) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>> {
    parse_related(body, "citedPaper")
}

/// Parse response from the citations endpoint.
///
/// Each item in `data[]` has a `citingPaper` object.
pub fn parse_cites(body: &str) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>> {
    parse_related(body, "citingPaper")
}

/// Shared parser for refs/cites responses which nest the paper under a key.
fn parse_related(
    body: &str,
    paper_key: &str,
) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>> {
    let data: Value = serde_json::from_str(body)?;
    let items = data
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or("missing data array")?;

    let mut results = Vec::with_capacity(items.len());

    for item in items {
        let paper = &item[paper_key];
        let title = paper["title"].as_str().unwrap_or("N/A").to_string();

        let year = match paper["year"].as_u64() {
            Some(y) => y.to_string(),
            None => "?".to_string(),
        };

        let authors: Vec<String> = paper["authors"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|a| a["name"].as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let s2_id = paper["paperId"].as_str().map(|s| s.to_string());
        let ext = &paper["externalIds"];
        let arxiv_id = ext["ArXiv"].as_str().map(|s| s.to_string());
        let doi = ext["DOI"].as_str().map(|s| s.to_string());

        results.push(PaperResult {
            title,
            authors,
            year,
            s2_id,
            arxiv_id,
            doi,
            ..Default::default()
        });
    }

    Ok(results)
}

/// Prepend `ARXIV:` or `DOI:` for bare arXiv IDs / DOIs so S2 can resolve them.
fn normalize_paper_id(paper_id: &str) -> String {
    // Already prefixed (ARXIV:, arXiv:, DOI:, CorpusId:, etc.)
    if paper_id.contains(':') {
        return paper_id.to_string();
    }

    // Bare arXiv ID: YYMM.NNNNN (4 digits, dot, 4-5 digits, optional version)
    let is_bare_arxiv = {
        let base = paper_id.split('v').next().unwrap_or(paper_id);
        if let Some(dot_pos) = base.find('.') {
            let (yymm, nnnnn) = base.split_at(dot_pos);
            let nnnnn = &nnnnn[1..]; // skip the dot
            yymm.len() == 4
                && rest_is_digits(yymm)
                && (nnnnn.len() == 4 || nnnnn.len() == 5)
                && rest_is_digits(nnnnn)
        } else {
            false
        }
    };

    if is_bare_arxiv {
        return format!("ARXIV:{}", paper_id);
    }

    // Bare DOI: 10.NNNN/...
    let is_bare_doi = paper_id.starts_with("10.")
        && paper_id
            .get(3..)
            .and_then(|rest| rest.find('/'))
            .map(|slash_pos| {
                slash_pos >= 4 && rest_is_digits(&paper_id[3..3 + slash_pos])
            })
            .unwrap_or(false);

    if is_bare_doi {
        format!("DOI:{}", paper_id)
    } else {
        paper_id.to_string()
    }
}

/// Check if all characters in `s` are ASCII digits.
fn rest_is_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_paper_id_bare_doi() {
        assert_eq!(
            normalize_paper_id("10.1145/3442188.3445899"),
            "DOI:10.1145/3442188.3445899"
        );
    }

    #[test]
    fn test_normalize_paper_id_already_prefixed() {
        assert_eq!(
            normalize_paper_id("DOI:10.1145/3442188.3445899"),
            "DOI:10.1145/3442188.3445899"
        );
    }

    #[test]
    fn test_normalize_paper_id_bare_arxiv() {
        assert_eq!(normalize_paper_id("2006.11239"), "ARXIV:2006.11239");
    }

    #[test]
    fn test_normalize_paper_id_bare_arxiv_5digit() {
        assert_eq!(normalize_paper_id("2004.12265"), "ARXIV:2004.12265");
    }

    #[test]
    fn test_normalize_paper_id_arxiv_already_prefixed() {
        assert_eq!(
            normalize_paper_id("ARXIV:2006.11239"),
            "ARXIV:2006.11239"
        );
    }

    #[test]
    fn test_normalize_paper_id_arxiv_lowercase_prefix() {
        assert_eq!(
            normalize_paper_id("arXiv:2006.11239"),
            "arXiv:2006.11239"
        );
    }

    #[test]
    fn test_normalize_paper_id_short_prefix() {
        // "10.12/foo" has only 2 digits before slash -- not a valid DOI prefix
        assert_eq!(normalize_paper_id("10.12/foo"), "10.12/foo");
    }

    #[test]
    fn test_parse_search_basic() {
        let body = r#"{
            "data": [
                {
                    "title": "Attention Is All You Need",
                    "year": 2017,
                    "authors": [
                        {"name": "Ashish Vaswani"},
                        {"name": "Noam Shazeer"}
                    ],
                    "citationCount": 90000,
                    "externalIds": {
                        "ArXiv": "1706.03762",
                        "DOI": "10.5555/3295222.3295349"
                    }
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
        assert_eq!(r.arxiv_id.as_deref(), Some("1706.03762"));
        assert_eq!(r.doi.as_deref(), Some("10.5555/3295222.3295349"));
    }

    #[test]
    fn test_parse_search_empty() {
        let body = r#"{"data": []}"#;
        let results = parse_search(body).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_parse_search_missing_optional_fields() {
        let body = r#"{
            "data": [
                {
                    "title": "Minimal Paper",
                    "authors": []
                }
            ]
        }"#;
        let results = parse_search(body).unwrap();
        let r = &results[0];
        assert_eq!(r.title, "Minimal Paper");
        assert_eq!(r.year, "?");
        assert!(r.authors.is_empty());
        assert!(r.citations.is_none());
        assert!(r.arxiv_id.is_none());
        assert!(r.doi.is_none());
    }

    #[test]
    fn test_parse_refs_basic() {
        let body = r#"{
            "data": [
                {
                    "citedPaper": {
                        "title": "BERT: Pre-training",
                        "year": 2019,
                        "authors": [
                            {"name": "Jacob Devlin"},
                            {"name": "Ming-Wei Chang"}
                        ]
                    }
                },
                {
                    "citedPaper": {
                        "title": "GPT-2",
                        "year": 2019,
                        "authors": [
                            {"name": "Alec Radford"}
                        ]
                    }
                }
            ]
        }"#;
        let results = parse_refs(body).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "BERT: Pre-training");
        assert_eq!(results[0].authors, vec!["Jacob Devlin", "Ming-Wei Chang"]);
        assert_eq!(results[1].title, "GPT-2");
        assert_eq!(results[1].year, "2019");
    }

    #[test]
    fn test_parse_cites_basic() {
        let body = r#"{
            "data": [
                {
                    "citingPaper": {
                        "title": "A Follow-Up Paper",
                        "year": 2022,
                        "authors": [{"name": "Eve Jones"}]
                    }
                }
            ]
        }"#;
        let results = parse_cites(body).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "A Follow-Up Paper");
        assert_eq!(results[0].year, "2022");
        assert_eq!(results[0].authors, vec!["Eve Jones"]);
    }

    #[test]
    fn test_parse_refs_empty() {
        let body = r#"{"data": []}"#;
        let results = parse_refs(body).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_paper_url() {
        let url = paper_url("arXiv:2006.11239");
        assert!(url.contains("api.semanticscholar.org/graph/v1/paper/arXiv:2006.11239"));
        assert!(url.contains("paperId"));
        assert!(url.contains("citationCount"));
        assert!(url.contains("openAccessPdf"));
    }

    #[test]
    fn test_parse_paper_full() {
        let body = r#"{
            "paperId": "abc123",
            "externalIds": {
                "DOI": "10.1234/test",
                "ArXiv": "2006.11239"
            },
            "venue": "NeurIPS",
            "citationCount": 500,
            "openAccessPdf": {
                "url": "https://arxiv.org/pdf/2006.11239"
            }
        }"#;
        let r = parse_paper(body).unwrap();
        assert_eq!(r.s2_id.as_deref(), Some("abc123"));
        assert_eq!(r.doi.as_deref(), Some("10.1234/test"));
        assert_eq!(r.arxiv_id.as_deref(), Some("2006.11239"));
        assert_eq!(r.venue.as_deref(), Some("NeurIPS"));
        assert_eq!(r.citations, Some(500));
        assert_eq!(r.pdf_url.as_deref(), Some("https://arxiv.org/pdf/2006.11239"));
    }

    #[test]
    fn test_parse_paper_minimal() {
        let body = r#"{
            "paperId": "def456",
            "externalIds": {}
        }"#;
        let r = parse_paper(body).unwrap();
        assert_eq!(r.s2_id.as_deref(), Some("def456"));
        assert!(r.doi.is_none());
        assert!(r.venue.is_none());
        assert!(r.citations.is_none());
        assert!(r.pdf_url.is_none());
    }

    #[test]
    fn test_parse_paper_empty_venue() {
        let body = r#"{
            "paperId": "x",
            "externalIds": {},
            "venue": ""
        }"#;
        let r = parse_paper(body).unwrap();
        assert!(r.venue.is_none());
    }

    #[test]
    fn test_parse_cites_missing_year() {
        let body = r#"{
            "data": [
                {
                    "citingPaper": {
                        "title": "No Year"
                    }
                }
            ]
        }"#;
        let results = parse_cites(body).unwrap();
        assert_eq!(results[0].year, "?");
        assert!(results[0].authors.is_empty());
    }
}
