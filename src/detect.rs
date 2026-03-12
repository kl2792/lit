/// Input type detection and normalization.
///
/// Detects whether user input is an arXiv ID/URL, DOI, ISBN, DBLP URL,
/// Semantic Scholar URL, or a free-text search query. Matches the behavior
/// of the bash `detect_type()` function exactly.

use std::sync::LazyLock;

use regex::Regex;

static ARXIV_URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^https?://([a-z]+\.)?arxiv\.org/").unwrap());
static DOI_URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^https?://(dx\.)?doi\.org/").unwrap());
static DBLP_URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^https?://dblp\.org/").unwrap());
static SS_URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^https?://www\.semanticscholar\.org/").unwrap());
static ARXIV_NEW_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(arXiv:|arxiv:)?[0-9]{4}\.[0-9]{4,5}(v[0-9]+)?$").unwrap());
static ARXIV_OLD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-z-]+(\.[A-Z][A-Za-z-]*)?/[0-9]{7}(v[0-9]+)?$").unwrap()
});
static DOI_BARE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^10\.[0-9]{4,}/").unwrap());
static ISBN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9]{9}[0-9Xx]$|^[0-9]{13}$").unwrap());
static VERSION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"v[0-9]+$").unwrap());

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputType {
    Arxiv,
    Doi,
    Isbn,
    DblpUrl,
    SemanticScholarUrl,
    Search,
}

/// Detect the input type from a string.
///
/// Priority order (matches bash):
/// 1. arXiv URL (https://arxiv.org/... or http://...)
/// 2. DOI URL (https://doi.org/... or dx.doi.org)
/// 3. DBLP URL
/// 4. Semantic Scholar URL
/// 5. arXiv ID (YYMM.NNNNN or category/NNNNNNN, optional arXiv: prefix)
/// 6. DOI (10.NNNN+/...)
/// 7. ISBN (10 or 13 digits after stripping hyphens/spaces)
/// 8. Fallback: Search
pub fn detect_type(input: &str) -> InputType {
    if ARXIV_URL_RE.is_match(input) {
        return InputType::Arxiv;
    }

    if DOI_URL_RE.is_match(input) {
        return InputType::Doi;
    }

    if DBLP_URL_RE.is_match(input) {
        return InputType::DblpUrl;
    }

    if SS_URL_RE.is_match(input) {
        return InputType::SemanticScholarUrl;
    }

    if ARXIV_NEW_RE.is_match(input) {
        return InputType::Arxiv;
    }

    if ARXIV_OLD_RE.is_match(input) {
        return InputType::Arxiv;
    }

    if DOI_BARE_RE.is_match(input) {
        return InputType::Doi;
    }

    // ISBN: 10 or 13 digits after stripping hyphens and spaces
    let stripped: String = input.chars().filter(|c| *c != '-' && *c != ' ').collect();
    if ISBN_RE.is_match(&stripped) {
        return InputType::Isbn;
    }

    InputType::Search
}

/// Strip URL prefixes, `arXiv:` prefix, version suffix, and `.pdf` suffix
/// from an arXiv identifier.
pub fn normalize_arxiv(input: &str) -> String {
    let mut s = input.to_string();

    // Strip URL prefixes (order matters: longer prefixes first)
    let prefixes = [
        "https://export.arxiv.org/abs/",
        "https://arxiv.org/abs/",
        "http://arxiv.org/abs/",
        "https://arxiv.org/pdf/",
        "http://arxiv.org/pdf/",
    ];
    for prefix in &prefixes {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.to_string();
            break;
        }
    }

    // Strip arXiv: prefix (case-insensitive for the two known variants)
    if let Some(rest) = s.strip_prefix("arXiv:") {
        s = rest.to_string();
    } else if let Some(rest) = s.strip_prefix("arxiv:") {
        s = rest.to_string();
    }

    // Strip version suffix vN (e.g., v1, v12)
    s = VERSION_RE.replace(&s, "").to_string();

    // Strip .pdf suffix
    if let Some(rest) = s.strip_suffix(".pdf") {
        s = rest.to_string();
    }

    s
}

/// Strip URL prefixes from a DOI string.
pub fn normalize_doi(input: &str) -> String {
    let prefixes = [
        "https://doi.org/",
        "http://doi.org/",
        "https://dx.doi.org/",
        "http://dx.doi.org/",
    ];
    for prefix in &prefixes {
        if let Some(rest) = input.strip_prefix(prefix) {
            return rest.to_string();
        }
    }
    input.to_string()
}

/// Strip hyphens and spaces from an ISBN string.
pub fn normalize_isbn(input: &str) -> String {
    input.chars().filter(|c| *c != '-' && *c != ' ').collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── detect_type ──

    #[test]
    fn detect_arxiv_url() {
        assert_eq!(
            detect_type("https://arxiv.org/abs/2006.11239"),
            InputType::Arxiv
        );
    }

    #[test]
    fn detect_arxiv_pdf_url() {
        assert_eq!(
            detect_type("https://arxiv.org/pdf/2006.11239v2"),
            InputType::Arxiv
        );
    }

    #[test]
    fn detect_arxiv_export_url() {
        assert_eq!(
            detect_type("https://export.arxiv.org/abs/2006.11239"),
            InputType::Arxiv
        );
    }

    #[test]
    fn detect_arxiv_http_url() {
        assert_eq!(
            detect_type("http://arxiv.org/abs/2006.11239"),
            InputType::Arxiv
        );
    }

    #[test]
    fn detect_arxiv_id_bare() {
        assert_eq!(detect_type("2006.11239"), InputType::Arxiv);
    }

    #[test]
    fn detect_arxiv_id_with_version() {
        assert_eq!(detect_type("2006.11239v3"), InputType::Arxiv);
    }

    #[test]
    fn detect_arxiv_id_with_prefix() {
        assert_eq!(detect_type("arXiv:2006.11239"), InputType::Arxiv);
    }

    #[test]
    fn detect_arxiv_id_lowercase_prefix() {
        assert_eq!(detect_type("arxiv:2006.11239"), InputType::Arxiv);
    }

    #[test]
    fn detect_arxiv_id_five_digit() {
        assert_eq!(detect_type("1511.05952"), InputType::Arxiv);
    }

    #[test]
    fn detect_arxiv_old_style() {
        assert_eq!(detect_type("hep-ph/9905221"), InputType::Arxiv);
    }

    #[test]
    fn detect_arxiv_old_style_versioned() {
        assert_eq!(detect_type("hep-ph/9905221v2"), InputType::Arxiv);
    }

    #[test]
    fn detect_arxiv_old_style_dotted_category() {
        assert_eq!(detect_type("math.AG/0601001"), InputType::Arxiv);
    }

    #[test]
    fn detect_arxiv_old_style_dotted_versioned() {
        assert_eq!(detect_type("math.AG/0601001v1"), InputType::Arxiv);
    }

    #[test]
    fn detect_doi_url() {
        assert_eq!(
            detect_type("https://doi.org/10.1145/3442188.3445899"),
            InputType::Doi
        );
    }

    #[test]
    fn detect_doi_dx_url() {
        assert_eq!(
            detect_type("https://dx.doi.org/10.1145/3442188.3445899"),
            InputType::Doi
        );
    }

    #[test]
    fn detect_doi_bare() {
        assert_eq!(
            detect_type("10.1145/3442188.3445899"),
            InputType::Doi
        );
    }

    #[test]
    fn detect_doi_five_digit_registrant() {
        assert_eq!(detect_type("10.48550/arXiv.2006.11239"), InputType::Doi);
    }

    #[test]
    fn detect_isbn_13_with_hyphens() {
        assert_eq!(detect_type("978-0262039246"), InputType::Isbn);
    }

    #[test]
    fn detect_isbn_13_bare() {
        assert_eq!(detect_type("9780262039246"), InputType::Isbn);
    }

    #[test]
    fn detect_isbn_10() {
        assert_eq!(detect_type("0262039249"), InputType::Isbn);
    }

    #[test]
    fn detect_isbn_with_spaces() {
        assert_eq!(detect_type("978 0 262 039246"), InputType::Isbn);
    }

    #[test]
    fn detect_isbn_10_with_check_digit_x() {
        assert_eq!(detect_type("080442957X"), InputType::Isbn);
    }

    #[test]
    fn detect_isbn_10_with_check_digit_x_hyphenated() {
        assert_eq!(detect_type("0-8044-2957-X"), InputType::Isbn);
    }

    #[test]
    fn detect_dblp_url() {
        assert_eq!(
            detect_type("https://dblp.org/rec/journals/corr/abs-2006-11239"),
            InputType::DblpUrl
        );
    }

    #[test]
    fn detect_semantic_scholar_url() {
        assert_eq!(
            detect_type("https://www.semanticscholar.org/paper/abc123"),
            InputType::SemanticScholarUrl
        );
    }

    #[test]
    fn detect_search_free_text() {
        assert_eq!(
            detect_type("attention is all you need"),
            InputType::Search
        );
    }

    #[test]
    fn detect_search_partial_numbers() {
        // Not a valid arXiv ID (too few digits after dot)
        assert_eq!(detect_type("2006.1"), InputType::Search);
    }

    // ── normalize_arxiv ──

    #[test]
    fn normalize_arxiv_bare_id() {
        assert_eq!(normalize_arxiv("2006.11239"), "2006.11239");
    }

    #[test]
    fn normalize_arxiv_with_version() {
        assert_eq!(normalize_arxiv("2006.11239v2"), "2006.11239");
    }

    #[test]
    fn normalize_arxiv_with_prefix() {
        assert_eq!(normalize_arxiv("arXiv:2006.11239"), "2006.11239");
    }

    #[test]
    fn normalize_arxiv_abs_url() {
        assert_eq!(
            normalize_arxiv("https://arxiv.org/abs/2006.11239"),
            "2006.11239"
        );
    }

    #[test]
    fn normalize_arxiv_pdf_url_with_version() {
        assert_eq!(
            normalize_arxiv("https://arxiv.org/pdf/2006.11239v2"),
            "2006.11239"
        );
    }

    #[test]
    fn normalize_arxiv_export_url() {
        assert_eq!(
            normalize_arxiv("https://export.arxiv.org/abs/2006.11239"),
            "2006.11239"
        );
    }

    #[test]
    fn normalize_arxiv_pdf_suffix() {
        assert_eq!(
            normalize_arxiv("https://arxiv.org/pdf/2006.11239.pdf"),
            "2006.11239"
        );
    }

    // ── normalize_doi ──

    #[test]
    fn normalize_doi_bare() {
        assert_eq!(
            normalize_doi("10.1145/3442188.3445899"),
            "10.1145/3442188.3445899"
        );
    }

    #[test]
    fn normalize_doi_https_url() {
        assert_eq!(
            normalize_doi("https://doi.org/10.1145/3442188.3445899"),
            "10.1145/3442188.3445899"
        );
    }

    #[test]
    fn normalize_doi_dx_url() {
        assert_eq!(
            normalize_doi("https://dx.doi.org/10.1145/3442188.3445899"),
            "10.1145/3442188.3445899"
        );
    }

    #[test]
    fn normalize_doi_http_url() {
        assert_eq!(
            normalize_doi("http://doi.org/10.1145/3442188.3445899"),
            "10.1145/3442188.3445899"
        );
    }

    // ── normalize_isbn ──

    #[test]
    fn normalize_isbn_with_hyphens() {
        assert_eq!(normalize_isbn("978-0262039246"), "9780262039246");
    }

    #[test]
    fn normalize_isbn_with_spaces() {
        assert_eq!(normalize_isbn("978 0 262 039246"), "9780262039246");
    }

    #[test]
    fn normalize_isbn_already_clean() {
        assert_eq!(normalize_isbn("9780262039246"), "9780262039246");
    }
}
