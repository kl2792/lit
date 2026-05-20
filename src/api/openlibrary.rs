use super::{extract_last_name, extract_year_from_date, urlencode, PaperResult};
use serde_json::Value;

// --- Open Library URL support ---

/// Parsed OL URL components.
pub struct OlUrlParts {
    pub kind: OlKind,
    pub id: String,
}

pub enum OlKind {
    Works,
    Books,
}

/// Extract the entity kind and ID from an Open Library URL.
///
/// Supports:
/// - `https://openlibrary.org/works/OL{id}W`
/// - `https://openlibrary.org/books/OL{id}M`
/// Both http and https; optional `.json` suffix.
pub fn parse_ol_url(url: &str) -> Option<OlUrlParts> {
    let stripped = url
        .strip_prefix("https://openlibrary.org")
        .or_else(|| url.strip_prefix("http://openlibrary.org"))?;
    let (kind_str, rest) = if let Some(r) = stripped.strip_prefix("/works/") {
        ("works", r)
    } else if let Some(r) = stripped.strip_prefix("/books/") {
        ("books", r)
    } else {
        return None;
    };
    // Strip optional .json suffix and trailing slashes/query strings
    let id = rest
        .split('?').next().unwrap_or(rest)
        .trim_end_matches('/')
        .trim_end_matches(".json");
    if id.is_empty() {
        return None;
    }
    let kind = match kind_str {
        "works" => OlKind::Works,
        "books" => OlKind::Books,
        _ => return None,
    };
    Some(OlUrlParts { kind, id: id.to_string() })
}

/// URL for a works JSON record.
pub fn work_url(id: &str) -> String {
    format!("https://openlibrary.org/works/{}.json", id)
}

/// URL for a books (edition) JSON record.
pub fn edition_url(id: &str) -> String {
    format!("https://openlibrary.org/books/{}.json", id)
}

/// URL for the editions list of a work (sorted ascending by date, capped at 50).
pub fn work_editions_url(id: &str) -> String {
    format!("https://openlibrary.org/works/{}/editions.json?limit=50", id)
}

/// URL for an author JSON record. `key` is e.g. "/authors/OL117058A".
pub fn author_url(key: &str) -> String {
    format!("https://openlibrary.org{}.json", key)
}

/// Intermediate result from parsing a works JSON response.
pub struct WorkResult {
    pub title: String,
    pub author_keys: Vec<String>,
}

/// Intermediate result from parsing a books (edition) or editions-list entry.
pub struct EditionResult {
    pub title: String,
    pub publisher: Option<String>,
    pub year: String,
    pub author_keys: Vec<String>,
}

/// Parse a works JSON response.
pub fn parse_work(body: &str) -> Result<WorkResult, Box<dyn std::error::Error>> {
    let data: Value = serde_json::from_str(body)?;
    let title = data["title"].as_str().ok_or("missing title")?.to_string();
    let author_keys = data["authors"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a["author"]["key"].as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    Ok(WorkResult { title, author_keys })
}

/// Parse a books (edition) JSON response.
pub fn parse_edition(body: &str) -> Result<EditionResult, Box<dyn std::error::Error>> {
    let data: Value = serde_json::from_str(body)?;
    let title = data["title"].as_str().ok_or("missing title")?.to_string();
    let publisher = data["publishers"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|p| p.as_str())
        .map(|s| s.to_string());
    let year = data["publish_date"]
        .as_str()
        .map(|d| extract_year_from_date(d))
        .unwrap_or_else(|| "?".to_string());
    let author_keys = data["authors"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a["key"].as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    Ok(EditionResult { title, publisher, year, author_keys })
}

/// Parse the editions list JSON response, returning editions sorted by year ascending.
pub fn parse_editions_list(body: &str) -> Result<Vec<EditionResult>, Box<dyn std::error::Error>> {
    let data: Value = serde_json::from_str(body)?;
    let entries = data["entries"].as_array().ok_or("missing entries")?;
    let mut editions: Vec<EditionResult> = entries
        .iter()
        .filter_map(|e| {
            let title = e["title"].as_str()?.to_string();
            let publisher = e["publishers"]
                .as_array()
                .and_then(|arr| arr.first())
                .and_then(|p| p.as_str())
                .map(|s| s.to_string());
            let year = e["publish_date"]
                .as_str()
                .map(|d| extract_year_from_date(d))
                .unwrap_or_else(|| "?".to_string());
            let author_keys = e["authors"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|a| a["key"].as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            Some(EditionResult { title, publisher, year, author_keys })
        })
        .collect();
    editions.sort_by(|a, b| a.year.cmp(&b.year));
    Ok(editions)
}

/// Parse an author JSON response, returning the display name.
///
/// Uses `name` (display form, e.g. "Ronald Aylmer Fisher") so that `extract_last_name`
/// in citekey generation correctly picks the last surname token. Falls back to
/// `personal_name` ("Fisher, Ronald Aylmer") only when `name` is absent.
pub fn parse_author(body: &str) -> Result<String, Box<dyn std::error::Error>> {
    let data: Value = serde_json::from_str(body)?;
    if let Some(name) = data["name"].as_str().filter(|s| !s.is_empty()) {
        return Ok(name.to_string());
    }
    data["personal_name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| "missing author name".into())
}

// --- URL builders (existing) ---

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
        .map(|d| extract_year_from_date(d))
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
        .map(|d| extract_year_from_date(d))
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

    // ── parse_ol_url ──

    #[test]
    fn parse_ol_url_works() {
        let p = parse_ol_url("https://openlibrary.org/works/OL1153861W").unwrap();
        assert!(matches!(p.kind, OlKind::Works));
        assert_eq!(p.id, "OL1153861W");
    }

    #[test]
    fn parse_ol_url_books() {
        let p = parse_ol_url("https://openlibrary.org/books/OL13955598M").unwrap();
        assert!(matches!(p.kind, OlKind::Books));
        assert_eq!(p.id, "OL13955598M");
    }

    #[test]
    fn parse_ol_url_json_suffix() {
        let p = parse_ol_url("https://openlibrary.org/works/OL1153861W.json").unwrap();
        assert!(matches!(p.kind, OlKind::Works));
        assert_eq!(p.id, "OL1153861W");
    }

    #[test]
    fn parse_ol_url_http() {
        let p = parse_ol_url("http://openlibrary.org/books/OL13955598M").unwrap();
        assert!(matches!(p.kind, OlKind::Books));
        assert_eq!(p.id, "OL13955598M");
    }

    #[test]
    fn parse_ol_url_invalid() {
        assert!(parse_ol_url("https://openlibrary.org/authors/OL117058A").is_none());
        assert!(parse_ol_url("https://example.com/works/OL123W").is_none());
        assert!(parse_ol_url("https://openlibrary.org/works/").is_none());
    }

    // ── parse_work ──

    #[test]
    fn parse_work_full() {
        let body = r#"{
            "title": "Statistical methods for research workers",
            "authors": [
                {"type": {"key": "/type/author_role"}, "author": {"key": "/authors/OL117058A"}}
            ]
        }"#;
        let w = parse_work(body).unwrap();
        assert_eq!(w.title, "Statistical methods for research workers");
        assert_eq!(w.author_keys, vec!["/authors/OL117058A"]);
    }

    #[test]
    fn parse_work_missing_authors() {
        let body = r#"{"title": "A Book"}"#;
        let w = parse_work(body).unwrap();
        assert_eq!(w.title, "A Book");
        assert!(w.author_keys.is_empty());
    }

    #[test]
    fn parse_work_missing_title() {
        let body = r#"{"authors": []}"#;
        assert!(parse_work(body).is_err());
    }

    // ── parse_edition ──

    #[test]
    fn parse_edition_full() {
        let body = r#"{
            "title": "Statistical methods for research workers",
            "publishers": ["Oliver & Boyd"],
            "publish_date": "1925",
            "authors": [{"key": "/authors/OL117058A"}]
        }"#;
        let e = parse_edition(body).unwrap();
        assert_eq!(e.title, "Statistical methods for research workers");
        assert_eq!(e.publisher.as_deref(), Some("Oliver & Boyd"));
        assert_eq!(e.year, "1925");
        assert_eq!(e.author_keys, vec!["/authors/OL117058A"]);
    }

    #[test]
    fn parse_edition_missing_publisher() {
        let body = r#"{"title": "A Book", "publish_date": "1963"}"#;
        let e = parse_edition(body).unwrap();
        assert!(e.publisher.is_none());
        assert_eq!(e.year, "1963");
    }

    #[test]
    fn parse_edition_missing_year() {
        let body = r#"{"title": "A Book", "publishers": ["Wiley"]}"#;
        let e = parse_edition(body).unwrap();
        assert_eq!(e.year, "?");
    }

    #[test]
    fn parse_edition_missing_title() {
        let body = r#"{"publishers": ["Wiley"]}"#;
        assert!(parse_edition(body).is_err());
    }

    // ── parse_editions_list ──

    #[test]
    fn parse_editions_list_sorted() {
        let body = r#"{"entries": [
            {"title": "B", "publish_date": "1963", "publishers": ["Holt"]},
            {"title": "A", "publish_date": "1925", "publishers": ["Oliver & Boyd"]}
        ]}"#;
        let eds = parse_editions_list(body).unwrap();
        assert_eq!(eds.len(), 2);
        assert_eq!(eds[0].year, "1925");
        assert_eq!(eds[1].year, "1963");
    }

    #[test]
    fn parse_editions_list_empty() {
        let body = r#"{"entries": []}"#;
        let eds = parse_editions_list(body).unwrap();
        assert!(eds.is_empty());
    }

    #[test]
    fn parse_editions_list_missing_entries() {
        let body = r#"{}"#;
        assert!(parse_editions_list(body).is_err());
    }

    // ── parse_author ──

    #[test]
    fn parse_author_prefers_name_over_personal_name() {
        let body = r#"{"name": "Ronald Aylmer Fisher", "personal_name": "Fisher, Ronald Aylmer"}"#;
        let name = parse_author(body).unwrap();
        assert_eq!(name, "Ronald Aylmer Fisher");
    }

    #[test]
    fn parse_author_falls_back_to_personal_name() {
        let body = r#"{"personal_name": "Fisher, Ronald Aylmer"}"#;
        let name = parse_author(body).unwrap();
        assert_eq!(name, "Fisher, Ronald Aylmer");
    }

    #[test]
    fn parse_author_name_only() {
        let body = r#"{"name": "Ronald Aylmer Fisher"}"#;
        let name = parse_author(body).unwrap();
        assert_eq!(name, "Ronald Aylmer Fisher");
    }

    #[test]
    fn parse_author_missing_name() {
        let body = r#"{}"#;
        assert!(parse_author(body).is_err());
    }
}

