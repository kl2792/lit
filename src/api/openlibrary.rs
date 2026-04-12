use super::{extract_last_name, extract_year_from_date, urlencode, PaperResult};
use serde_json::Value;

/// Build URL for looking up a book by ISBN.
pub fn isbn_url(isbn: &str) -> String {
    format!(
        "https://openlibrary.org/api/books?bibkeys=ISBN:{}&format=json&jscmd=data",
        isbn
    )
}

/// Build URL for a general book search.
pub fn search_url(query: &str, limit: usize) -> String {
    format!(
        "https://openlibrary.org/search.json?q={}&limit={}",
        urlencode(query),
        limit
    )
}

/// Parse response from the ISBN lookup endpoint.
///
/// The response is keyed by `ISBN:{isbn}`, containing title, subtitle, authors,
/// publishers, publish_date, number_of_pages, and identifiers.
pub fn parse_isbn(body: &str) -> Result<PaperResult, Box<dyn std::error::Error>> {
    let data: Value = serde_json::from_str(body)?;
    let obj = data
        .as_object()
        .ok_or("expected JSON object")?;

    // The response is keyed by the ISBN bibkey (e.g. "ISBN:9780262039246")
    let (key, book) = obj.iter().next().ok_or("empty response")?;

    let title = book["title"].as_str().unwrap_or("N/A").to_string();

    // Append subtitle if present
    let full_title = match book["subtitle"].as_str() {
        Some(sub) if !sub.is_empty() => format!("{}: {}", title, sub),
        _ => title.clone(),
    };

    let authors: Vec<String> = book["authors"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a["name"].as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let venue = book["publishers"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|p| p["name"].as_str())
        .map(|s| s.to_string());

    let year = book["publish_date"]
        .as_str()
        .map(extract_year_from_date)
        .unwrap_or_else(|| "?".to_string());

    // Extract ISBN from identifiers
    let ids = &book["identifiers"];
    let isbn = ids["isbn_13"]
        .as_array()
        .and_then(|arr| arr.first())
        .or_else(|| {
            ids["isbn_10"]
                .as_array()
                .and_then(|arr| arr.first())
        })
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            // Fall back to the key itself
            key.strip_prefix("ISBN:").map(|s| s.to_string())
        });

    Ok(PaperResult {
        title: full_title,
        authors,
        year,
        venue,
        isbn,
        ..Default::default()
    })
}

/// Detailed book metadata for the ISBN command display.
pub struct BookDetail {
    pub title: String,
    pub subtitle: Option<String>,
    pub authors: Vec<String>,
    pub publisher: Option<String>,
    pub publish_date: Option<String>,
    pub year: String,
    pub isbn_13: Option<String>,
    pub isbn_10: Option<String>,
}

/// Parse response from the ISBN lookup endpoint into detailed book metadata.
///
/// Unlike `parse_isbn` (which returns a generic `PaperResult`), this preserves
/// all display fields the ISBN command needs.
pub fn parse_isbn_detail(body: &str) -> Result<BookDetail, Box<dyn std::error::Error>> {
    let data: Value = serde_json::from_str(body)?;
    let obj = data.as_object().ok_or("expected JSON object")?;
    let (_key, book) = obj.iter().next().ok_or("empty response")?;

    let title = book["title"].as_str().unwrap_or("N/A").to_string();
    let subtitle = book["subtitle"].as_str().filter(|s| !s.is_empty()).map(|s| s.to_string());

    let authors: Vec<String> = book["authors"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a["name"].as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let publisher = book["publishers"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|p| p["name"].as_str())
        .map(|s| s.to_string());

    let publish_date = book["publish_date"].as_str().map(|s| s.to_string());
    let year = publish_date
        .as_deref()
        .map(extract_year_from_date)
        .unwrap_or_else(|| "?".to_string());

    let ids = &book["identifiers"];
    let isbn_13 = ids["isbn_13"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let isbn_10 = ids["isbn_10"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(BookDetail {
        title,
        subtitle,
        authors,
        publisher,
        publish_date,
        year,
        isbn_13,
        isbn_10,
    })
}

/// Parse response from the search endpoint.
pub fn parse_search(body: &str) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>> {
    let data: Value = serde_json::from_str(body)?;
    let docs = data["docs"]
        .as_array()
        .ok_or("missing docs array")?;

    let mut results = Vec::with_capacity(docs.len());

    for doc in docs {
        let title = doc["title"].as_str().unwrap_or("N/A").to_string();

        let year = match doc["first_publish_year"].as_u64() {
            Some(y) => y.to_string(),
            None => "?".to_string(),
        };

        let authors: Vec<String> = doc["author_name"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|a| {
                        a.as_str()
                            .map(|name| extract_last_name(name).to_string())
                    })
                    .collect()
            })
            .unwrap_or_default();

        let isbn = doc["isbn"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        results.push(PaperResult {
            title,
            authors,
            year,
            isbn,
            ..Default::default()
        });
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_isbn_basic() {
        let body = r#"{
            "ISBN:9780262039246": {
                "title": "Causality",
                "subtitle": "Models, Reasoning, and Inference",
                "authors": [
                    {"name": "Judea Pearl"}
                ],
                "publishers": [
                    {"name": "Cambridge University Press"}
                ],
                "publish_date": "2009",
                "identifiers": {
                    "isbn_13": ["9780262039246"],
                    "isbn_10": ["026203924X"]
                }
            }
        }"#;
        let r = parse_isbn(body).unwrap();
        assert_eq!(r.title, "Causality: Models, Reasoning, and Inference");
        assert_eq!(r.year, "2009");
        assert_eq!(r.authors, vec!["Judea Pearl"]);
        assert_eq!(r.venue.as_deref(), Some("Cambridge University Press"));
        assert_eq!(r.isbn.as_deref(), Some("9780262039246"));
    }

    #[test]
    fn test_parse_isbn_no_subtitle() {
        let body = r#"{
            "ISBN:1234567890": {
                "title": "A Book",
                "authors": [],
                "publish_date": "January 2020"
            }
        }"#;
        let r = parse_isbn(body).unwrap();
        assert_eq!(r.title, "A Book");
        assert_eq!(r.year, "2020");
    }

    #[test]
    fn test_parse_isbn_fallback_isbn_from_key() {
        let body = r#"{
            "ISBN:9781234567890": {
                "title": "Fallback Test"
            }
        }"#;
        let r = parse_isbn(body).unwrap();
        assert_eq!(r.isbn.as_deref(), Some("9781234567890"));
    }

    #[test]
    fn test_parse_isbn_empty_response() {
        let body = r#"{}"#;
        let err = parse_isbn(body);
        assert!(err.is_err());
    }

    #[test]
    fn test_parse_isbn_detail_basic() {
        let body = r#"{
            "ISBN:9780262039246": {
                "title": "Causality",
                "subtitle": "Models, Reasoning, and Inference",
                "authors": [
                    {"name": "Judea Pearl"}
                ],
                "publishers": [
                    {"name": "Cambridge University Press"}
                ],
                "publish_date": "2009",
                "identifiers": {
                    "isbn_13": ["9780262039246"],
                    "isbn_10": ["026203924X"]
                }
            }
        }"#;
        let d = parse_isbn_detail(body).unwrap();
        assert_eq!(d.title, "Causality");
        assert_eq!(d.subtitle.as_deref(), Some("Models, Reasoning, and Inference"));
        assert_eq!(d.authors, vec!["Judea Pearl"]);
        assert_eq!(d.publisher.as_deref(), Some("Cambridge University Press"));
        assert_eq!(d.publish_date.as_deref(), Some("2009"));
        assert_eq!(d.year, "2009");
        assert_eq!(d.isbn_13.as_deref(), Some("9780262039246"));
        assert_eq!(d.isbn_10.as_deref(), Some("026203924X"));
    }

    #[test]
    fn test_parse_isbn_detail_no_subtitle() {
        let body = r#"{
            "ISBN:123": {
                "title": "Solo Title",
                "publish_date": "March 2015"
            }
        }"#;
        let d = parse_isbn_detail(body).unwrap();
        assert_eq!(d.title, "Solo Title");
        assert!(d.subtitle.is_none());
        assert_eq!(d.year, "2015");
        assert!(d.isbn_13.is_none());
        assert!(d.isbn_10.is_none());
    }

    #[test]
    fn test_parse_search_basic() {
        let body = r#"{
            "docs": [
                {
                    "title": "Causality",
                    "first_publish_year": 2000,
                    "author_name": ["Judea Pearl"],
                    "isbn": ["9780521895606", "9780262039246"]
                }
            ]
        }"#;
        let results = parse_search(body).unwrap();
        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert_eq!(r.title, "Causality");
        assert_eq!(r.year, "2000");
        assert_eq!(r.authors, vec!["Pearl"]);
        assert_eq!(r.isbn.as_deref(), Some("9780521895606"));
    }

    #[test]
    fn test_parse_search_empty() {
        let body = r#"{"docs": []}"#;
        let results = parse_search(body).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_parse_search_missing_optional_fields() {
        let body = r#"{
            "docs": [
                {
                    "title": "Unknown Book"
                }
            ]
        }"#;
        let results = parse_search(body).unwrap();
        let r = &results[0];
        assert_eq!(r.title, "Unknown Book");
        assert_eq!(r.year, "?");
        assert!(r.authors.is_empty());
        assert!(r.isbn.is_none());
    }
}

