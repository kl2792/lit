use super::{extract_last_name, urlencode, PaperResult};
use serde_json::Value;

/// Build URL for a DBLP publication search.
pub fn search_url(query: &str, limit: usize) -> String {
    format!(
        "https://dblp.org/search/publ/api?q={}&format=json&h={}",
        urlencode(query),
        limit
    )
}

/// Parse response from the DBLP search endpoint.
///
/// The JSON structure is `result.hits.hit[]`, where each hit has an `info` object
/// containing `title`, `year`, `authors.author` (may be a single dict or a list),
/// `venue`, and `url`.
pub fn parse_search(body: &str) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>> {
    let data: Value = serde_json::from_str(body)?;
    let hits = data["result"]["hits"]["hit"]
        .as_array()
        .ok_or("missing result.hits.hit array")?;

    let mut results = Vec::with_capacity(hits.len());

    for h in hits {
        let info = &h["info"];

        let title = info["title"].as_str().unwrap_or("N/A").to_string();
        let year = info["year"].as_str().unwrap_or("?").to_string();

        let authors = parse_dblp_authors(&info["authors"]["author"]);

        let venue = info["venue"].as_str().map(|s| s.to_string());

        // DBLP url field (e.g. "https://dblp.org/rec/conf/nips/VaswaniSPUJGKP17")
        let pdf_url = info["url"].as_str().map(|s| s.to_string());

        results.push(PaperResult {
            title,
            authors,
            year,
            venue,
            pdf_url,
            ..Default::default()
        });
    }

    Ok(results)
}

/// Parse DBLP author field, which can be either a single object or an array.
///
/// Each author object has a `"text"` field with the display name.
fn parse_dblp_authors(author_val: &Value) -> Vec<String> {
    match author_val {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|a| {
                a.get("text")
                    .or_else(|| a.as_str().map(|_| a))
                    .and_then(|v| v.as_str())
                    .map(|name| extract_last_name(name).to_string())
            })
            .collect(),
        Value::Object(_) => {
            // Single author as a dict
            author_val["text"]
                .as_str()
                .map(|name| vec![extract_last_name(name).to_string()])
                .unwrap_or_default()
        }
        Value::String(s) => {
            // Bare string (unlikely but defensive)
            vec![extract_last_name(s).to_string()]
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_search_basic() {
        let body = r#"{
            "result": {
                "hits": {
                    "hit": [
                        {
                            "info": {
                                "title": "Attention Is All You Need",
                                "year": "2017",
                                "authors": {
                                    "author": [
                                        {"text": "Ashish Vaswani"},
                                        {"text": "Noam Shazeer"}
                                    ]
                                },
                                "venue": "NeurIPS",
                                "url": "https://dblp.org/rec/conf/nips/VaswaniSPUJGKP17"
                            }
                        }
                    ]
                }
            }
        }"#;
        let results = parse_search(body).unwrap();
        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert_eq!(r.title, "Attention Is All You Need");
        assert_eq!(r.year, "2017");
        assert_eq!(r.authors, vec!["Vaswani", "Shazeer"]);
        assert_eq!(r.venue.as_deref(), Some("NeurIPS"));
        assert_eq!(r.pdf_url.as_deref(), Some("https://dblp.org/rec/conf/nips/VaswaniSPUJGKP17"));
    }

    #[test]
    fn test_parse_search_single_author_object() {
        let body = r#"{
            "result": {
                "hits": {
                    "hit": [
                        {
                            "info": {
                                "title": "Solo Paper",
                                "year": "2020",
                                "authors": {
                                    "author": {"text": "Judea Pearl"}
                                }
                            }
                        }
                    ]
                }
            }
        }"#;
        let results = parse_search(body).unwrap();
        assert_eq!(results[0].authors, vec!["Pearl"]);
    }

    #[test]
    fn test_parse_search_empty_hits() {
        let body = r#"{"result": {"hits": {"hit": []}}}"#;
        let results = parse_search(body).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_parse_search_missing_optional_fields() {
        let body = r#"{
            "result": {
                "hits": {
                    "hit": [
                        {
                            "info": {
                                "title": "Minimal Paper",
                                "year": "2021"
                            }
                        }
                    ]
                }
            }
        }"#;
        let results = parse_search(body).unwrap();
        let r = &results[0];
        assert_eq!(r.title, "Minimal Paper");
        assert!(r.authors.is_empty());
        assert!(r.venue.is_none());
        assert!(r.pdf_url.is_none());
    }

    #[test]
    fn test_parse_dblp_authors_bare_string() {
        let val = serde_json::json!("Albert Einstein");
        let authors = parse_dblp_authors(&val);
        assert_eq!(authors, vec!["Einstein"]);
    }

    #[test]
    fn test_parse_dblp_authors_null() {
        let val = serde_json::Value::Null;
        let authors = parse_dblp_authors(&val);
        assert!(authors.is_empty());
    }
}

