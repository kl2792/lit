/// Columbia Clio catalog integration.
///
/// Provides live search against the Columbia University Library catalog via
/// the Clio JSON API (catalog.json). No local index or sync required.
/// Also provides optional local FTS5 index for offline use via `lit clio sync`.

use std::io::Read;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use quick_xml::events::Event;
use quick_xml::Reader;
use rusqlite::{params, Connection};

use super::PaperResult;

/// Given the main lit.db path, return the sibling clio.db path.
pub fn clio_db_path(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("clio.db")
}

/// Derive clio.db path, walking up from cwd to find the project root.
///
/// Priority:
/// 1. `LIT_CLIO_DB_PATH` env var
/// 2. Walk up from `cwd` looking for an `etc/lit/` directory
/// 3. Fallback: `etc/lit/clio.db` relative to cwd
pub fn default_clio_db_path() -> PathBuf {
    if let Ok(p) = std::env::var("LIT_CLIO_DB_PATH") {
        return PathBuf::from(p);
    }
    if let Ok(cwd) = std::env::current_dir() {
        let mut dir = cwd.as_path();
        loop {
            let candidate = dir.join("etc/lit/clio.db");
            if dir.join("etc/lit").is_dir() {
                return candidate;
            }
            match dir.parent() {
                Some(p) => dir = p,
                None => break,
            }
        }
    }
    PathBuf::from("etc/lit/clio.db")
}

// --- Live Clio API search ---

/// Build the Clio catalog.json search URL.
pub fn search_url(query: &str, limit: usize) -> String {
    format!(
        "https://clio.columbia.edu/catalog.json?q={}&per_page={}",
        super::urlencode(query),
        limit
    )
}

/// Parse the Clio catalog.json response into PaperResults.
pub fn parse_search(body: &str) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>> {
    let data: serde_json::Value = serde_json::from_str(body)?;
    let docs = data["response"]["docs"]
        .as_array()
        .ok_or("missing response.docs")?;

    let mut results = Vec::new();
    for doc in docs {
        let title = doc["title_display"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if title.is_empty() {
            continue;
        }

        let authors: Vec<String> = doc["author_facet"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();

        let year = doc["pub_year_display"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // url_munged_display entries are pipe-delimited; the URL is the first
        // pipe-separated field that starts with "http".
        let mut doi: Option<String> = None;
        let mut pdf_url: Option<String> = None;
        if let Some(entries) = doc["url_munged_display"].as_array() {
            for entry in entries {
                if let Some(s) = entry.as_str() {
                    if let Some(url) = s.split('|').find(|p| p.starts_with("http")) {
                        if url.contains("doi.org/") {
                            if doi.is_none() {
                                doi = extract_doi_from_url(url);
                            }
                        } else if pdf_url.is_none() {
                            pdf_url = Some(url.to_string());
                        }
                    }
                }
            }
        }

        let venue = doc["pub_name_display"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let isbn = doc["isbn_display"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .map(|s| s.split_whitespace().next().unwrap_or(s).to_string());

        results.push(PaperResult {
            title,
            authors,
            year,
            doi,
            pdf_url,
            venue,
            isbn,
            ..Default::default()
        });
    }

    Ok(results)
}

/// Initialize the clio.db schema (idempotent — safe to call on an existing DB).
pub fn init_clio_db(conn: &Connection) -> Result<(), Box<dyn std::error::Error>> {
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS clio_fts USING fts5(
            title, authors, year, isbn, issn, doi, url, online, publisher
        );
        CREATE TABLE IF NOT EXISTS clio_meta (key TEXT PRIMARY KEY, value TEXT);",
    )?;
    Ok(())
}

/// Clear the FTS index before a full re-sync (makes sync idempotent).
pub fn clear_clio_db(conn: &Connection) -> Result<(), Box<dyn std::error::Error>> {
    conn.execute_batch("DELETE FROM clio_fts;")?;
    Ok(())
}

/// A single Clio catalog record (pre-parsed from MARCXML).
#[derive(Debug, Default, Clone)]
pub struct ClioRecord {
    pub title: String,
    pub authors: String,
    pub year: String,
    pub isbn: String,
    pub issn: String,
    pub doi: String,
    pub url: String,
    pub online: bool,
    pub publisher: String,
}

/// Insert a batch of records into the FTS5 table in a single transaction.
pub fn insert_batch(
    conn: &Connection,
    records: &[ClioRecord],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stmt = conn.prepare(
        "INSERT INTO clio_fts (title, authors, year, isbn, issn, doi, url, online, publisher)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for r in records {
        stmt.execute(params![
            r.title,
            r.authors,
            r.year,
            r.isbn,
            r.issn,
            r.doi,
            r.url,
            if r.online { "1" } else { "0" },
            r.publisher,
        ])?;
    }
    Ok(())
}

/// Search the Clio FTS5 table and return matching papers.
///
/// Falls back to a LIKE query if the query string is not valid FTS5 syntax.
pub fn search_clio(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>> {
    let rows = match try_fts_search(conn, query, limit) {
        Ok(rows) => rows,
        Err(_) => try_like_search(conn, query, limit)?,
    };
    Ok(rows)
}

fn try_fts_search(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> rusqlite::Result<Vec<PaperResult>> {
    let mut stmt = conn.prepare(
        "SELECT title, authors, year, isbn, issn, doi, url, online, publisher
         FROM clio_fts WHERE clio_fts MATCH ? LIMIT ?",
    )?;
    let rows = stmt.query_map(params![query, limit as i64], map_row)?;
    rows.collect()
}

fn try_like_search(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> rusqlite::Result<Vec<PaperResult>> {
    let pattern = format!("%{}%", query);
    let mut stmt = conn.prepare(
        "SELECT title, authors, year, isbn, issn, doi, url, online, publisher
         FROM clio_fts WHERE title LIKE ?1 OR authors LIKE ?1 LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![pattern, limit as i64], map_row)?;
    rows.collect()
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PaperResult> {
    let title: String = row.get(0)?;
    let authors_str: String = row.get(1)?;
    let year: String = row.get(2)?;
    let isbn: String = row.get(3)?;
    let doi: String = row.get(5)?;
    let url: String = row.get(6)?;
    let online: String = row.get(7)?;
    let publisher: String = row.get(8)?;

    let authors: Vec<String> = authors_str
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let pdf_url = if online == "1" && !url.is_empty() {
        Some(url)
    } else {
        None
    };

    Ok(PaperResult {
        title,
        authors,
        year,
        doi: if doi.is_empty() { None } else { Some(doi) },
        isbn: if isbn.is_empty() { None } else { Some(isbn) },
        venue: if publisher.is_empty() { None } else { Some(publisher) },
        pdf_url,
        ..Default::default()
    })
}

// --- MARCXML parser ---

/// State machine context while parsing a single MARC record.
#[derive(Default)]
struct RecordState {
    /// Current tag being processed (e.g. "245").
    tag: String,
    /// ind2 value for 856 fields.
    ind2: String,
    /// Current subfield code.
    subfield_code: char,
    /// Are we inside a <subfield> element?
    in_subfield: bool,
    /// Are we inside a <controlfield> element (e.g. tag 008)?
    in_controlfield: bool,

    // Accumulated field data
    title_a: String,
    title_b: String,
    authors: Vec<String>,
    current_author: String,
    year: String,
    isbn: String,
    issn: String,
    doi: String,
    url: String,
    online: bool,
    publisher: String,
}

impl RecordState {
    /// Flush a completed <datafield> into the record state.
    ///
    /// Called when we leave a datafield or encounter a new one.
    fn flush_datafield(&mut self) {
        if !self.current_author.is_empty() {
            self.authors.push(std::mem::take(&mut self.current_author));
        }
    }

    /// Build a `ClioRecord` from accumulated state and reset.
    fn into_record(mut self) -> ClioRecord {
        // Flush any pending author
        if !self.current_author.is_empty() {
            self.authors.push(self.current_author);
        }

        let title = match (self.title_a.as_str(), self.title_b.as_str()) {
            ("", "") => String::new(),
            (a, "") => a.trim_end_matches(|c| matches!(c, '/' | ' ')).to_string(),
            (a, b) => {
                let a = a.trim_end_matches(|c| matches!(c, '/' | ' ' | ':'));
                format!("{} {}", a, b.trim_end_matches(|c| matches!(c, '/' | ' ')))
            }
        };

        ClioRecord {
            title,
            authors: self.authors.join(", "),
            year: self.year,
            isbn: self.isbn,
            issn: self.issn,
            doi: self.doi,
            url: self.url,
            online: self.online,
            publisher: self.publisher,
        }
    }
}

/// Parse a gzipped MARCXML buffer and return a list of `ClioRecord`s.
pub fn parse_marcxml_gz(gz_bytes: &[u8]) -> Result<Vec<ClioRecord>, Box<dyn std::error::Error>> {
    let mut decompressed = Vec::new();
    GzDecoder::new(gz_bytes).read_to_end(&mut decompressed)?;
    parse_marcxml(&decompressed)
}

/// Parse a raw (uncompressed) MARCXML buffer.
pub fn parse_marcxml(xml: &[u8]) -> Result<Vec<ClioRecord>, Box<dyn std::error::Error>> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut records: Vec<ClioRecord> = Vec::new();

    // Current record being built; None when outside a <record>.
    let mut state: Option<RecordState> = None;

    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let name = std::str::from_utf8(e.name().as_ref()).unwrap_or("").to_lowercase();
                match name.as_str() {
                    "record" => {
                        state = Some(RecordState::default());
                    }
                    "controlfield" if state.is_some() => {
                        if let Some(ref mut s) = state {
                            s.tag = attr_str(e, b"tag");
                            s.in_controlfield = true;
                        }
                    }
                    "datafield" if state.is_some() => {
                        if let Some(ref mut s) = state {
                            // Flush previous author if switching datafields
                            s.flush_datafield();
                            s.tag = attr_str(e, b"tag");
                            s.ind2 = attr_str(e, b"ind2");
                        }
                    }
                    "subfield" if state.is_some() => {
                        if let Some(ref mut s) = state {
                            let code = attr_str(e, b"code");
                            s.subfield_code = code.chars().next().unwrap_or('\0');
                            s.in_subfield = true;
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                let name = std::str::from_utf8(e.name().as_ref()).unwrap_or("").to_lowercase();
                match name.as_str() {
                    "record" => {
                        if let Some(s) = state.take() {
                            let rec = s.into_record();
                            if !rec.title.is_empty() {
                                records.push(rec);
                            }
                        }
                    }
                    "controlfield" => {
                        if let Some(ref mut s) = state {
                            s.in_controlfield = false;
                        }
                    }
                    "subfield" => {
                        if let Some(ref mut s) = state {
                            s.in_subfield = false;
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(ref e)) => {
                let text = e.unescape().unwrap_or_default();
                let text = text.as_ref();

                if let Some(ref mut s) = state {
                    if s.in_controlfield && s.tag == "008" {
                        // Bytes 7-10 (0-indexed) are the year
                        if text.len() >= 11 && s.year.is_empty() {
                            let year_part = &text[7..11];
                            if year_part.chars().all(|c| c.is_ascii_digit()) {
                                s.year = year_part.to_string();
                            }
                        }
                    } else if s.in_subfield {
                        match s.tag.as_str() {
                            "245" => match s.subfield_code {
                                'a' => s.title_a = text.to_string(),
                                'b' => s.title_b = text.to_string(),
                                _ => {}
                            },
                            "100" | "700" => {
                                if s.subfield_code == 'a' {
                                    // Flush previous 700 author accumulation
                                    if !s.current_author.is_empty() && s.tag == "700" {
                                        s.authors.push(std::mem::take(&mut s.current_author));
                                    }
                                    s.current_author = text
                                        .trim_end_matches(|c| matches!(c, ',' | '.' | ' '))
                                        .to_string();
                                }
                            }
                            "020" => {
                                if s.subfield_code == 'a' && s.isbn.is_empty() {
                                    // Strip qualifiers like " (pbk.)"
                                    s.isbn = text
                                        .split_whitespace()
                                        .next()
                                        .unwrap_or("")
                                        .trim_end_matches(|c: char| !c.is_ascii_alphanumeric())
                                        .to_string();
                                }
                            }
                            "022" => {
                                if s.subfield_code == 'a' && s.issn.is_empty() {
                                    s.issn = text.trim().to_string();
                                }
                            }
                            "856" => {
                                if s.subfield_code == 'u' {
                                    if text.contains("doi.org") {
                                        // Extract the DOI path from a doi.org URL
                                        if s.doi.is_empty() {
                                            if let Some(path) = extract_doi_from_url(text) {
                                                s.doi = path;
                                            }
                                        }
                                    } else if s.ind2 == "0" && s.url.is_empty() {
                                        // Full-text link
                                        s.url = text.to_string();
                                        s.online = true;
                                    }
                                }
                            }
                            "260" | "264" => match s.subfield_code {
                                'b' => {
                                    if s.publisher.is_empty() {
                                        s.publisher = text
                                            .trim_end_matches(|c| matches!(c, ',' | '.' | ' '))
                                            .to_string();
                                    }
                                }
                                'c' => {
                                    // Year fallback: extract first 4-digit run
                                    if s.year.is_empty() {
                                        if let Some(y) = extract_4digit_year(text) {
                                            s.year = y;
                                        }
                                    }
                                }
                                _ => {}
                            },
                            _ => {}
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("XML parse error: {}", e).into()),
            _ => {}
        }
    }

    Ok(records)
}

// --- XML attribute helpers ---

fn attr_str(e: &quick_xml::events::BytesStart<'_>, name: &[u8]) -> String {
    e.attributes()
        .filter_map(|a| a.ok())
        .find(|a| a.key.as_ref() == name)
        .and_then(|a| String::from_utf8(a.value.to_vec()).ok())
        .unwrap_or_default()
}

fn extract_doi_from_url(url: &str) -> Option<String> {
    // Find "doi.org/" and take everything after it
    url.find("doi.org/")
        .map(|pos| url[pos + "doi.org/".len()..].to_string())
        .filter(|s| !s.is_empty())
}

fn extract_4digit_year(s: &str) -> Option<String> {
    let mut run = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            run.push(c);
            if run.len() == 4 {
                return Some(run);
            }
        } else {
            run.clear();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // --- Unit tests for helpers ---

    #[test]
    fn test_extract_doi_from_url_standard() {
        assert_eq!(
            extract_doi_from_url("https://doi.org/10.1234/foo"),
            Some("10.1234/foo".into())
        );
    }

    #[test]
    fn test_extract_doi_from_url_http() {
        assert_eq!(
            extract_doi_from_url("http://dx.doi.org/10.5678/bar"),
            Some("10.5678/bar".into())
        );
    }

    #[test]
    fn test_extract_doi_from_url_no_doi() {
        assert_eq!(extract_doi_from_url("https://example.com/paper"), None);
    }

    #[test]
    fn test_extract_4digit_year_plain() {
        assert_eq!(extract_4digit_year("1990"), Some("1990".into()));
    }

    #[test]
    fn test_extract_4digit_year_embedded() {
        assert_eq!(extract_4digit_year("c2001."), Some("2001".into()));
    }

    #[test]
    fn test_extract_4digit_year_none() {
        assert_eq!(extract_4digit_year("no year here"), None);
    }

    #[test]
    fn test_clio_db_path() {
        let db = PathBuf::from("/home/user/etc/lit/lit.db");
        assert_eq!(clio_db_path(&db), PathBuf::from("/home/user/etc/lit/clio.db"));
    }

    // --- MARCXML parsing tests ---

    fn minimal_marc(body: &str) -> Vec<u8> {
        format!(
            r#"<?xml version="1.0"?><collection><record>{}</record></collection>"#,
            body
        )
        .into_bytes()
    }

    #[test]
    fn test_parse_marcxml_title_only() {
        let xml = minimal_marc(
            r#"<datafield tag="245" ind1=" " ind2=" ">
                 <subfield code="a">The Art of War /</subfield>
               </datafield>"#,
        );
        let records = parse_marcxml(&xml).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].title, "The Art of War");
    }

    #[test]
    fn test_parse_marcxml_title_with_subtitle() {
        let xml = minimal_marc(
            r#"<datafield tag="245" ind1=" " ind2=" ">
                 <subfield code="a">Deep Learning :</subfield>
                 <subfield code="b">a practitioner's guide</subfield>
               </datafield>"#,
        );
        let records = parse_marcxml(&xml).unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].title.contains("Deep Learning"));
        assert!(records[0].title.contains("practitioner"));
    }

    #[test]
    fn test_parse_marcxml_author_100() {
        let xml = minimal_marc(
            r#"<datafield tag="245" ind1=" " ind2=" ">
                 <subfield code="a">Some Book</subfield>
               </datafield>
               <datafield tag="100" ind1="1" ind2=" ">
                 <subfield code="a">Smith, John,</subfield>
               </datafield>"#,
        );
        let records = parse_marcxml(&xml).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].authors, "Smith, John");
    }

    #[test]
    fn test_parse_marcxml_year_from_008() {
        let xml = minimal_marc(
            r#"<controlfield tag="008">910528s1990    xxxxxxxxxx            </controlfield>
               <datafield tag="245" ind1=" " ind2=" ">
                 <subfield code="a">Old Book</subfield>
               </datafield>"#,
        );
        let records = parse_marcxml(&xml).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].year, "1990");
    }

    #[test]
    fn test_parse_marcxml_isbn() {
        let xml = minimal_marc(
            r#"<datafield tag="245" ind1=" " ind2=" ">
                 <subfield code="a">Book Title</subfield>
               </datafield>
               <datafield tag="020" ind1=" " ind2=" ">
                 <subfield code="a">9780262033848 (hardcover)</subfield>
               </datafield>"#,
        );
        let records = parse_marcxml(&xml).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].isbn, "9780262033848");
    }

    #[test]
    fn test_parse_marcxml_doi_from_856() {
        let xml = minimal_marc(
            r#"<datafield tag="245" ind1=" " ind2=" ">
                 <subfield code="a">Some Paper</subfield>
               </datafield>
               <datafield tag="856" ind1="4" ind2="0">
                 <subfield code="u">https://doi.org/10.1234/test.567</subfield>
               </datafield>"#,
        );
        let records = parse_marcxml(&xml).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].doi, "10.1234/test.567");
    }

    #[test]
    fn test_parse_marcxml_online_url() {
        let xml = minimal_marc(
            r#"<datafield tag="245" ind1=" " ind2=" ">
                 <subfield code="a">Online Resource</subfield>
               </datafield>
               <datafield tag="856" ind1="4" ind2="0">
                 <subfield code="u">https://example.com/fulltext.pdf</subfield>
               </datafield>"#,
        );
        let records = parse_marcxml(&xml).unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].online);
        assert_eq!(records[0].url, "https://example.com/fulltext.pdf");
    }

    #[test]
    fn test_parse_marcxml_publisher() {
        let xml = minimal_marc(
            r#"<datafield tag="245" ind1=" " ind2=" ">
                 <subfield code="a">A Book</subfield>
               </datafield>
               <datafield tag="260" ind1=" " ind2=" ">
                 <subfield code="b">MIT Press,</subfield>
                 <subfield code="c">c2020.</subfield>
               </datafield>"#,
        );
        let records = parse_marcxml(&xml).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].publisher, "MIT Press");
        // Year should be populated from 260$c since no 008
        assert_eq!(records[0].year, "2020");
    }

    #[test]
    fn test_parse_marcxml_skips_titleless_records() {
        let xml = minimal_marc(
            r#"<datafield tag="100" ind1="1" ind2=" ">
                 <subfield code="a">Nobody Important</subfield>
               </datafield>"#,
        );
        let records = parse_marcxml(&xml).unwrap();
        assert!(records.is_empty(), "titleless records should be skipped");
    }

    #[test]
    fn test_parse_marcxml_gz_roundtrip() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let xml = minimal_marc(
            r#"<datafield tag="245" ind1=" " ind2=" ">
                 <subfield code="a">Compressed Title</subfield>
               </datafield>"#,
        );
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(&xml).unwrap();
        let compressed = gz.finish().unwrap();

        let records = parse_marcxml_gz(&compressed).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].title, "Compressed Title");
    }

    // --- DB round-trip test ---

    #[test]
    fn test_insert_and_search_clio() {
        let conn = Connection::open_in_memory().unwrap();
        init_clio_db(&conn).unwrap();

        let records = vec![
            ClioRecord {
                title: "Reinforcement Learning: An Introduction".into(),
                authors: "Sutton, Richard, Barto, Andrew".into(),
                year: "2018".into(),
                isbn: "9780262039246".into(),
                publisher: "MIT Press".into(),
                ..Default::default()
            },
            ClioRecord {
                title: "Deep Learning".into(),
                authors: "Goodfellow, Ian".into(),
                year: "2016".into(),
                ..Default::default()
            },
        ];

        // Use a transaction as the real code does
        conn.execute("BEGIN", []).unwrap();
        insert_batch(&conn, &records).unwrap();
        conn.execute("COMMIT", []).unwrap();

        let results = search_clio(&conn, "reinforcement", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].title.contains("Reinforcement"));

        let results2 = search_clio(&conn, "deep", 10).unwrap();
        assert_eq!(results2.len(), 1);
        assert_eq!(results2[0].title, "Deep Learning");
    }

    #[test]
    fn test_search_clio_like_fallback() {
        let conn = Connection::open_in_memory().unwrap();
        init_clio_db(&conn).unwrap();

        let records = vec![ClioRecord {
            title: "Graph Theory".into(),
            authors: "Bondy, J.A.".into(),
            year: "1976".into(),
            ..Default::default()
        }];
        insert_batch(&conn, &records).unwrap();

        // FTS5 MATCH with a bare "*" suffix is invalid syntax — should fall back to LIKE
        let results = search_clio(&conn, "Graph Theory", 10).unwrap();
        assert!(!results.is_empty(), "should find via FTS or LIKE fallback");
    }
}
