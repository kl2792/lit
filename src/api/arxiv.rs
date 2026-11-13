use super::PaperResult;
use quick_xml::events::Event;
use quick_xml::Reader;

const ATOM_NS: &[u8] = b"http://www.w3.org/2005/Atom";

/// Build URL for querying a single arXiv paper by ID.
pub fn query_url(arxiv_id: &str) -> String {
    format!("https://export.arxiv.org/api/query?id_list={}", arxiv_id)
}

/// Most query words `search_url` pairs; 8 words make 28 clauses.
const MAX_QUERY_WORDS: usize = 8;

/// Build URL for a relevance-sorted free-text search of up to `limit` papers.
///
/// The query ORs every pair of the query's topic words (`query_words`), each
/// pair ANDed across all fields: `(all:a AND all:b) OR (all:a AND all:c) ...`.
/// A paper must match two words to appear, and arXiv's relevance score rises
/// with the number of pairs it matches. ANDing every word returns nothing for
/// most queries of four or more words; ORing single words ranks papers that
/// repeat one rare word above papers that match several.
pub fn search_url(query: &str, limit: usize) -> String {
    let words: Vec<String> = super::query_words(query).into_iter().take(MAX_QUERY_WORDS).collect();
    let expr = if words.len() == 1 {
        format!("all:{}", words[0])
    } else {
        let mut pairs = Vec::new();
        for (i, a) in words.iter().enumerate() {
            for b in &words[i + 1..] {
                pairs.push(format!("(all:{} AND all:{})", a, b));
            }
        }
        pairs.join(" OR ")
    };
    format!(
        "https://export.arxiv.org/api/query?search_query={}&sortBy=relevance&max_results={}",
        super::urlencode(&expr),
        limit
    )
}

/// Parse a search feed into one record per entry, with full abstracts.
/// An empty feed is no results; arXiv's error entry is an error.
pub fn parse_search(xml: &str) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>> {
    parse_feed(xml)
}

/// Parse an arXiv Atom XML response into a `PaperResult`.
///
/// Extracts title, summary (truncated to 200 chars), published date, authors,
/// and the PDF link from the first `<entry>` element.
pub fn parse_entry(xml: &str) -> Result<PaperResult, Box<dyn std::error::Error>> {
    let mut paper = parse_feed(xml)?
        .into_iter()
        .next()
        .ok_or("no entry found in arXiv response")?;
    if let Some(summary) = paper.abstract_text.as_mut() {
        if summary.len() > 200 {
            *summary = format!("{}...", summary[..summary.floor_char_boundary(200)].trim());
        }
    }
    Ok(paper)
}

/// The fields of one `<entry>` as read so far.
#[derive(Default)]
struct EntryFields {
    title: String,
    summary: String,
    published: String,
    entry_id: String,
    authors: Vec<String>,
    pdf_url: Option<String>,
    categories: Vec<String>,
}

/// Every titled `<entry>` of an Atom feed, in feed order, with full summaries.
/// An entry whose id is under `arxiv.org/api/errors` makes the feed an error.
fn parse_feed(xml: &str) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>> {
    let mut reader = Reader::from_str(xml);
    let mut papers = Vec::new();

    // State machine: track where we are in the document
    let mut in_entry = false;
    let mut in_author = false;
    let mut current_tag: Option<EntryTag> = None;
    let mut entry = EntryFields::default();
    let mut current_author_name = String::new();

    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                let is_atom = is_atom_ns(e, &reader);

                match local {
                    b"entry" if is_atom => {
                        in_entry = true;
                        entry = EntryFields::default();
                    }
                    b"title" if in_entry && is_atom => {
                        current_tag = Some(EntryTag::Title);
                    }
                    b"summary" if in_entry && is_atom => {
                        current_tag = Some(EntryTag::Summary);
                    }
                    b"published" if in_entry && is_atom => {
                        current_tag = Some(EntryTag::Published);
                    }
                    b"id" if in_entry && is_atom && !in_author => {
                        current_tag = Some(EntryTag::Id);
                    }
                    b"author" if in_entry && is_atom => {
                        in_author = true;
                        current_author_name.clear();
                    }
                    b"name" if in_author && is_atom => {
                        current_tag = Some(EntryTag::AuthorName);
                    }
                    b"link" if in_entry && is_atom => {
                        let mut is_pdf = false;
                        let mut href = None;
                        for attr in e.attributes().flatten() {
                            match attr.key.as_ref() {
                                b"title" if attr.value.as_ref() == b"pdf" => {
                                    is_pdf = true;
                                }
                                b"href" => {
                                    href = Some(
                                        String::from_utf8_lossy(attr.value.as_ref()).to_string(),
                                    );
                                }
                                _ => {}
                            }
                        }
                        if is_pdf {
                            entry.pdf_url = href;
                        }
                    }
                    b"category" if in_entry => {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"term" {
                                let term = String::from_utf8_lossy(attr.value.as_ref()).to_string();
                                if !entry.categories.contains(&term) {
                                    entry.categories.push(term);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(ref e)) => {
                if let Some(ref tag) = current_tag {
                    let text = e.unescape().unwrap_or_default();
                    match tag {
                        EntryTag::Title => entry.title.push_str(&text),
                        EntryTag::Summary => entry.summary.push_str(&text),
                        EntryTag::Published => entry.published.push_str(&text),
                        EntryTag::Id => entry.entry_id.push_str(&text),
                        EntryTag::AuthorName => current_author_name.push_str(&text),
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                match local {
                    b"entry" if in_entry => {
                        in_entry = false;
                        let fields = std::mem::take(&mut entry);
                        if fields.entry_id.contains("arxiv.org/api/errors") {
                            return Err(format!("arXiv API error: {}", fields.summary.trim()).into());
                        }
                        if !fields.title.trim().is_empty() {
                            papers.push(entry_to_paper(fields));
                        }
                    }
                    b"author" => {
                        if !current_author_name.is_empty() {
                            entry.authors.push(current_author_name.trim().to_string());
                        }
                        in_author = false;
                        current_tag = None;
                    }
                    b"title" | b"summary" | b"published" | b"id" | b"name" => {
                        current_tag = None;
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(Box::new(e)),
            _ => {}
        }
        buf.clear();
    }
    Ok(papers)
}

/// One entry's fields as a record: whitespace in the title collapsed, the
/// summary trimmed, year and date cut from `published`, the id unversioned.
fn entry_to_paper(e: EntryFields) -> PaperResult {
    let title = e.title.split_whitespace().collect::<Vec<_>>().join(" ");

    // Extract year from published date (format: "2020-06-19T...")
    let year = e.published.get(..4).unwrap_or("?").to_string();

    // Extract published date (first 10 chars: "2020-06-19")
    let published_date = if e.published.len() >= 10 {
        Some(e.published[..10].to_string())
    } else if !e.published.is_empty() {
        Some(e.published.clone())
    } else {
        None
    };

    PaperResult {
        title,
        authors: e.authors,
        year,
        // e.g. "http://arxiv.org/abs/2006.11239v2" -> "2006.11239"
        arxiv_id: Some(extract_arxiv_id(&e.entry_id)),
        pdf_url: e.pdf_url,
        abstract_text: Some(e.summary.trim().to_string()),
        published_date,
        categories: e.categories,
        ..Default::default()
    }
}

/// Tags we track inside an `<entry>`.
#[derive(Debug)]
enum EntryTag {
    Title,
    Summary,
    Published,
    Id,
    AuthorName,
}

/// Strip namespace prefix from a qualified name, returning the local part.
///
/// Handles both `ns:local` and plain `local` forms.
fn local_name(full: &[u8]) -> &[u8] {
    match full.iter().rposition(|&b| b == b':') {
        Some(pos) => &full[pos + 1..],
        None => full,
    }
}

/// Check if an element belongs to the Atom namespace.
///
/// This is a heuristic: quick-xml doesn't resolve namespaces automatically,
/// so we check the raw element name. If it has no prefix or the reader
/// has the Atom namespace as default, we treat it as Atom.
fn is_atom_ns(e: &quick_xml::events::BytesStart<'_>, _reader: &Reader<&[u8]>) -> bool {
    let name = e.name();
    let full = name.as_ref();
    // If there's no colon, it's in the default namespace (Atom for arXiv responses).
    // If there is a colon, check for known Atom prefix patterns.
    if full.iter().any(|&b| b == b':') {
        // Check if namespace resolves to Atom -- we look at the prefix.
        // In practice, arXiv uses default namespace for Atom, so this is rare.
        // As a fallback, check the element's namespace declarations.
        for attr in e.attributes().flatten() {
            if attr.key.as_ref().starts_with(b"xmlns")
                && attr.value.as_ref() == ATOM_NS
            {
                return true;
            }
        }
        false
    } else {
        true
    }
}

/// Extract a clean arXiv ID from the entry's `<id>` URL.
///
/// Handles both new-style (`http://arxiv.org/abs/2006.11239v2` -> `2006.11239`)
/// and old-style (`http://arxiv.org/abs/hep-ph/0601001v1` -> `hep-ph/0601001`) IDs.
fn extract_arxiv_id(id_url: &str) -> String {
    // Strip the base URL prefix to get the raw ID (possibly with version)
    let raw = id_url
        .trim()
        .strip_prefix("http://arxiv.org/abs/")
        .or_else(|| id_url.trim().strip_prefix("https://arxiv.org/abs/"))
        .unwrap_or_else(|| {
            // Fallback: take everything after the last abs/ or just the last segment
            id_url
                .rfind("/abs/")
                .map(|pos| &id_url[pos + 5..])
                .unwrap_or(id_url.rsplit('/').next().unwrap_or(id_url))
        });

    // Strip version suffix (e.g. "v2")
    strip_version(raw)
}

/// Remove a trailing version suffix like `v2` from an arXiv ID.
fn strip_version(s: &str) -> String {
    match s.rfind('v') {
        Some(pos)
            if pos > 0
                && !s[pos + 1..].is_empty()
                && s[pos + 1..].chars().all(|c| c.is_ascii_digit()) =>
        {
            s[..pos].to_string()
        }
        _ => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_arxiv_id() {
        assert_eq!(
            extract_arxiv_id("http://arxiv.org/abs/2006.11239v2"),
            "2006.11239"
        );
        assert_eq!(
            extract_arxiv_id("http://arxiv.org/abs/2006.11239"),
            "2006.11239"
        );
        assert_eq!(
            extract_arxiv_id("http://arxiv.org/abs/hep-ph/0601001v1"),
            "hep-ph/0601001"
        );
    }

    #[test]
    fn test_parse_entry_minimal() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>http://arxiv.org/abs/2006.11239v3</id>
    <title>Denoising Diffusion Probabilistic Models</title>
    <summary>We present high quality image synthesis results.</summary>
    <published>2020-06-19T17:26:28Z</published>
    <author><name>Jonathan Ho</name></author>
    <author><name>Ajay Jain</name></author>
    <author><name>Pieter Abbeel</name></author>
    <link title="pdf" href="http://arxiv.org/pdf/2006.11239v3" rel="related" type="application/pdf"/>
    <category term="cs.LG" scheme="http://arxiv.org/schemas/atom"/>
    <category term="stat.ML" scheme="http://arxiv.org/schemas/atom"/>
  </entry>
</feed>"#;
        let result = parse_entry(xml).unwrap();
        assert_eq!(result.title, "Denoising Diffusion Probabilistic Models");
        assert_eq!(result.year, "2020");
        assert_eq!(result.arxiv_id.as_deref(), Some("2006.11239"));
        assert_eq!(result.authors.len(), 3);
        assert_eq!(result.authors[0], "Jonathan Ho");
        assert!(result.pdf_url.is_some());
        assert!(result.abstract_text.unwrap().contains("image synthesis"));
        assert_eq!(result.published_date.as_deref(), Some("2020-06-19"));
        assert_eq!(result.categories, vec!["cs.LG", "stat.ML"]);
    }

    #[test]
    fn test_parse_entry_no_pdf_link() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>http://arxiv.org/abs/2301.00001v1</id>
    <title>A Paper Without PDF Link</title>
    <summary>Short abstract.</summary>
    <published>2023-01-01T00:00:00Z</published>
    <author><name>Alice Smith</name></author>
  </entry>
</feed>"#;
        let result = parse_entry(xml).unwrap();
        assert_eq!(result.title, "A Paper Without PDF Link");
        assert!(result.pdf_url.is_none());
        assert_eq!(result.authors, vec!["Alice Smith"]);
        assert!(result.categories.is_empty());
    }

    #[test]
    fn test_parse_entry_long_abstract_truncated() {
        let long_text = "a ".repeat(200); // 400 chars
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>http://arxiv.org/abs/2301.00002v1</id>
    <title>Long Abstract</title>
    <summary>{}</summary>
    <published>2023-01-01T00:00:00Z</published>
    <author><name>Bob</name></author>
  </entry>
</feed>"#,
            long_text
        );
        let result = parse_entry(&xml).unwrap();
        let abs = result.abstract_text.unwrap();
        assert!(abs.ends_with("..."));
        // Truncated to ~200 chars + "..."
        assert!(abs.len() <= 210);
    }

    #[test]
    fn test_parse_entry_empty_feed() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
</feed>"#;
        let err = parse_entry(xml);
        assert!(err.is_err());
    }

    #[test]
    fn test_parse_entry_whitespace_in_title() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>http://arxiv.org/abs/2301.00003v1</id>
    <title>
      Multi   Line
      Title
    </title>
    <summary>Ok.</summary>
    <published>2023-06-15T00:00:00Z</published>
    <author><name>Carol</name></author>
  </entry>
</feed>"#;
        let result = parse_entry(xml).unwrap();
        assert_eq!(result.title, "Multi Line Title");
    }

    /// Recorded 2026-11-13 from `search_url("dropout Shapley value hidden units", 25)`.
    const DROPOUT_SHAPLEY: &str = include_str!("../../test/search/arxiv-dropout-shapley.xml");

    #[test]
    fn parse_search_reads_every_entry_with_full_abstracts() {
        let papers = parse_search(DROPOUT_SHAPLEY).unwrap();
        assert_eq!(papers.len(), 25);
        let first = &papers[0];
        assert_eq!(first.title, "Shapley Value as Principled Metric for Structured Network Pruning");
        assert!(first.arxiv_id.as_deref().is_some_and(|id| !id.contains('v')), "{:?}", first.arxiv_id);
        assert!(!first.authors.is_empty());
        assert!(first.year.len() == 4 && first.year.starts_with("20"), "{}", first.year);
        assert!(first.abstract_text.as_deref().is_some_and(|a| a.len() > 200 && !a.ends_with("...")));
        assert!(papers.iter().all(|p| !p.title.is_empty() && !p.title.contains("arXiv Query")));
    }

    #[test]
    fn parse_search_empty_feed_is_no_results() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom"><title>arXiv Query: x</title><id>http://arxiv.org/api/q</id></feed>"#;
        assert!(parse_search(xml).unwrap().is_empty());
    }

    #[test]
    fn parse_search_error_feed_is_an_error() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>http://arxiv.org/api/errors#incorrect_id_format_for_x</id>
    <title>Error</title>
    <summary>incorrect id format for x</summary>
  </entry>
</feed>"#;
        let err = parse_search(xml).unwrap_err().to_string();
        assert!(err.contains("incorrect id format"), "{}", err);
    }

    #[test]
    fn search_url_ors_every_pair_of_content_words() {
        let url = search_url("dropout of the Shapley value", 25);
        assert!(url.starts_with("https://export.arxiv.org/api/query?search_query="), "{}", url);
        assert!(url.ends_with("&sortBy=relevance&max_results=25"), "{}", url);
        let q = url.split("search_query=").nth(1).unwrap().split('&').next().unwrap();
        assert_eq!(
            q,
            super::super::urlencode(
                "(all:dropout AND all:shapley) OR (all:dropout AND all:value) OR (all:shapley AND all:value)"
            )
        );
    }

    #[test]
    fn search_url_single_word_and_word_cap() {
        let one = search_url("dropout", 5);
        assert!(one.contains(&format!("search_query={}&", super::super::urlencode("all:dropout"))), "{}", one);
        let many = search_url("a1 b2 c3 d4 e5 f6 g7 h8 i9 j10", 5);
        assert!(many.contains("h8") && !many.contains("i9"), "{}", many);
    }

    #[test]
    fn test_parse_entry_old_style_arxiv_id() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>http://arxiv.org/abs/hep-ph/0601001v1</id>
    <title>Old Style Paper</title>
    <summary>Old.</summary>
    <published>2006-01-01T00:00:00Z</published>
    <author><name>Dave</name></author>
  </entry>
</feed>"#;
        let result = parse_entry(xml).unwrap();
        assert_eq!(result.arxiv_id.as_deref(), Some("hep-ph/0601001"));
    }
}
