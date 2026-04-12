use super::PaperResult;
use quick_xml::events::Event;
use quick_xml::Reader;

const ATOM_NS: &[u8] = b"http://www.w3.org/2005/Atom";

/// Build URL for querying a single arXiv paper by ID.
pub fn query_url(arxiv_id: &str) -> String {
    format!("https://export.arxiv.org/api/query?id_list={}", arxiv_id)
}

/// Parse an arXiv Atom XML response into a `PaperResult`.
///
/// Extracts title, summary (truncated to 200 chars), published date, authors,
/// and the PDF link from the first `<entry>` element.
pub fn parse_entry(xml: &str) -> Result<PaperResult, Box<dyn std::error::Error>> {
    let mut reader = Reader::from_str(xml);

    // State machine: track where we are in the document
    let mut in_entry = false;
    let mut in_author = false;
    let mut current_tag: Option<EntryTag> = None;

    let mut title = String::new();
    let mut summary = String::new();
    let mut published = String::new();
    let mut entry_id = String::new();
    let mut authors: Vec<String> = Vec::new();
    let mut current_author_name = String::new();
    let mut pdf_url: Option<String> = None;
    let mut categories: Vec<String> = Vec::new();

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
                            pdf_url = href;
                        }
                    }
                    b"category" if in_entry => {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"term" {
                                let term = String::from_utf8_lossy(attr.value.as_ref()).to_string();
                                if !categories.contains(&term) {
                                    categories.push(term);
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
                        EntryTag::Title => title.push_str(&text),
                        EntryTag::Summary => summary.push_str(&text),
                        EntryTag::Published => published.push_str(&text),
                        EntryTag::Id => entry_id.push_str(&text),
                        EntryTag::AuthorName => current_author_name.push_str(&text),
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                match local {
                    b"entry" => break, // only parse first entry
                    b"author" => {
                        if !current_author_name.is_empty() {
                            authors.push(current_author_name.trim().to_string());
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

    if title.is_empty() {
        return Err("no entry found in arXiv response".into());
    }

    // Clean up title: collapse whitespace
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");

    // Truncate abstract to 200 chars
    let abstract_text = if summary.len() > 200 {
        let truncated = &summary[..summary.floor_char_boundary(200)];
        Some(format!("{}...", truncated.trim()))
    } else {
        Some(summary.trim().to_string())
    };

    // Extract year from published date (format: "2020-06-19T...")
    let year = published.get(..4).unwrap_or("?").to_string();

    // Extract published date (first 10 chars: "2020-06-19")
    let published_date = if published.len() >= 10 {
        Some(published[..10].to_string())
    } else if !published.is_empty() {
        Some(published.clone())
    } else {
        None
    };

    // Extract clean arxiv_id from the entry id URL, stripping version
    // e.g. "http://arxiv.org/abs/2006.11239v2" -> "2006.11239"
    let arxiv_id = extract_arxiv_id(&entry_id);

    Ok(PaperResult {
        title,
        authors,
        year,
        arxiv_id: Some(arxiv_id),
        pdf_url,
        abstract_text,
        published_date,
        categories,
        ..Default::default()
    })
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
    if full.contains(&b':') {
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
