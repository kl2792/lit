pub mod arxiv;
pub mod crossref;
pub mod dblp;
pub mod openalex;
pub mod openlibrary;
pub mod semantic_scholar;
pub mod unpaywall;

/// Common paper result used across all APIs.
#[derive(Debug, Clone, Default)]
pub struct PaperResult {
    pub title: String,
    pub authors: Vec<String>,
    pub year: String,
    pub doi: Option<String>,
    pub arxiv_id: Option<String>,
    pub citations: Option<u64>,
    pub venue: Option<String>,
    pub pdf_url: Option<String>,
    pub abstract_text: Option<String>,
    pub isbn: Option<String>,
    /// Semantic Scholar paper ID, used for multi-hop BFS.
    pub s2_id: Option<String>,
    /// Full published date (e.g. "2020-06-19"), used by arXiv.
    pub published_date: Option<String>,
    /// Category tags (e.g. ["cs.LG", "stat.ML"]), used by arXiv.
    pub categories: Vec<String>,
}

/// Extract the last name (last whitespace-delimited token) from a display name.
pub fn extract_last_name(name: &str) -> &str {
    name.split_whitespace().last().unwrap_or(name)
}

/// Extract the last name of the first author from a list of author display names.
///
/// Returns the last whitespace-delimited token of the first author.
/// Returns an empty string if the author list is empty.
pub fn first_author_lastname(authors: &[String]) -> String {
    authors
        .first()
        .map(|a| extract_last_name(a).to_string())
        .unwrap_or_default()
}

/// Extract a 4-digit year from a date string like "2019", "March 2019", "2019-03-15".
///
/// Finds the first 4-digit sequence that looks like a year. Falls back to the
/// last 4 chars if they are all digits.
pub(crate) fn extract_year_from_date(date: &str) -> String {
    for word in date.split(|c: char| !c.is_ascii_digit()) {
        if word.len() == 4 {
            return word.to_string();
        }
    }
    if date.len() >= 4 {
        let tail = &date[date.len() - 4..];
        if tail.chars().all(|c| c.is_ascii_digit()) {
            return tail.to_string();
        }
    }
    "?".to_string()
}

/// Percent-encode a string for use in URL query parameters.
///
/// Encodes all characters except unreserved ones (A-Z, a-z, 0-9, `-`, `_`, `.`, `~`).
pub(crate) fn urlencode(s: &str) -> String {
    let mut encoded = String::with_capacity(s.len() * 2);
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => {
                encoded.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_year_from_date() {
        assert_eq!(extract_year_from_date("2019"), "2019");
        assert_eq!(extract_year_from_date("March 2019"), "2019");
        assert_eq!(extract_year_from_date("2019-03-15"), "2019");
        assert_eq!(extract_year_from_date("January 1, 2020"), "2020");
    }

    #[test]
    fn test_extract_last_name() {
        assert_eq!(extract_last_name("Jonathan Ho"), "Ho");
        assert_eq!(extract_last_name("Pieter"), "Pieter");
        assert_eq!(extract_last_name(""), "");
    }

    #[test]
    fn test_urlencode_ascii_passthrough() {
        assert_eq!(urlencode("hello"), "hello");
        assert_eq!(urlencode("foo-bar_baz.qux~123"), "foo-bar_baz.qux~123");
    }

    #[test]
    fn test_urlencode_spaces_and_special() {
        assert_eq!(urlencode("hello world"), "hello%20world");
        assert_eq!(urlencode("a&b=c"), "a%26b%3Dc");
    }

    #[test]
    fn test_urlencode_empty() {
        assert_eq!(urlencode(""), "");
    }

    #[test]
    fn test_urlencode_unicode() {
        // UTF-8 bytes of 'e' with accent: 0xC3 0xA9
        let encoded = urlencode("caf\u{00e9}");
        assert_eq!(encoded, "caf%C3%A9");
    }

    #[test]
    fn test_first_author_lastname() {
        assert_eq!(first_author_lastname(&["Judea Pearl".to_string()]), "Pearl");
        assert_eq!(first_author_lastname(&["Judea Pearl".to_string(), "Dana Scott".to_string()]), "Pearl");
        assert_eq!(first_author_lastname(&[]), "");
    }
}
