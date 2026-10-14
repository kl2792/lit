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
static PHILPAPERS_URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^https?://philpapers\.org/rec/").unwrap());
static OL_URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^https?://openlibrary\.org/(works|books)/").unwrap());
static CAUSALAI_URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^https?://(www\.)?causalai\.net/r[0-9]+(\.pdf)?/?$").unwrap()
});
static CAUSALAI_SCHEME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^causalai:r?[0-9]+$").unwrap());
static ARXIV_NEW_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?i:arxiv:)?[0-9]{4}\.[0-9]{4,5}(v[0-9]+)?$").unwrap());
static ARXIV_OLD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?i:arxiv:)?[a-z-]+(\.[A-Z][A-Za-z-]*)?/[0-9]{7}(v[0-9]+)?$").unwrap()
});
static DOI_BARE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?i:doi:)?10\.[0-9]{4,}/").unwrap());
/// Every arXiv abstract or PDF page URL, on any host arXiv serves it from.
static ARXIV_PAGE_URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^https?://(www\.|export\.)?arxiv\.org/(abs|pdf)/").unwrap());
static ARXIV_SCHEME_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^arxiv:").unwrap());
static DOI_SCHEME_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^doi:").unwrap());
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
    PhilPapersUrl,
    OpenLibraryUrl,
    /// CausalAI Lab technical report (causalai.net/rNNN or `causalai:rNNN`).
    Causalai,
    Url,
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

    if PHILPAPERS_URL_RE.is_match(input) {
        return InputType::PhilPapersUrl;
    }

    if OL_URL_RE.is_match(input) {
        return InputType::OpenLibraryUrl;
    }

    // CausalAI tech report: causalai.net URL or `causalai:rNNN` scheme.
    // Checked before the generic URL fallback so the report PDF is ingested
    // rather than scraped as an HTML title.
    if CAUSALAI_URL_RE.is_match(input) || CAUSALAI_SCHEME_RE.is_match(input) {
        return InputType::Causalai;
    }

    // Any remaining https/http URL not matched above
    if input.starts_with("https://") || input.starts_with("http://") {
        return InputType::Url;
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

/// Strip an arXiv page URL or an `arXiv:` prefix (any case), then the `.pdf`
/// and version suffixes, from an arXiv identifier.
pub fn normalize_arxiv(input: &str) -> String {
    let s = ARXIV_PAGE_URL_RE.replace(input, "");
    let s = ARXIV_SCHEME_RE.replace(&s, "");
    let s = s.strip_suffix(".pdf").unwrap_or(&s);
    VERSION_RE.replace(s, "").into_owned()
}

/// Strip a DOI resolver URL or a `doi:` prefix (any case) from a DOI string.
pub fn normalize_doi(input: &str) -> String {
    DOI_SCHEME_RE.replace(&DOI_URL_RE.replace(input, ""), "").into_owned()
}

/// The arXiv identifier an arXiv-registered DOI (`10.48550/arXiv.<id>`) names,
/// normalized as `normalize_arxiv` would, or `None` for any other DOI.
pub fn arxiv_id_from_doi(doi: &str) -> Option<String> {
    const PREFIX: &str = "10.48550/arxiv.";
    let head = doi.get(..PREFIX.len())?;
    if !head.eq_ignore_ascii_case(PREFIX) {
        return None;
    }
    let id = &doi[PREFIX.len()..];
    (ARXIV_NEW_RE.is_match(id) || ARXIV_OLD_RE.is_match(id)).then(|| normalize_arxiv(id))
}

/// Strip hyphens and spaces from an ISBN string.
pub fn normalize_isbn(input: &str) -> String {
    input.chars().filter(|c| *c != '-' && *c != ' ').collect()
}

/// Extract the canonical report id (e.g. `r145`) from any accepted CausalAI
/// input: a `causalai.net/rNNN[.pdf]` URL or a `causalai:rNNN` / `causalai:NNN`
/// scheme. Returns `None` if no report number is present.
pub fn normalize_causalai(input: &str) -> Option<String> {
    // Take the final path/scheme segment ("r145.pdf", "r145", or "145"),
    // then keep only its digits.
    let segment = input
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or("");
    let digits: String = segment.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        Some(format!("r{}", digits))
    }
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
    fn detect_philpapers_url() {
        assert_eq!(
            detect_type("https://philpapers.org/rec/ANDCIT-6"),
            InputType::PhilPapersUrl
        );
    }

    #[test]
    fn detect_philpapers_url_http() {
        assert_eq!(
            detect_type("http://philpapers.org/rec/PEACAO"),
            InputType::PhilPapersUrl
        );
    }

    #[test]
    fn detect_openlibrary_works_url() {
        assert_eq!(
            detect_type("https://openlibrary.org/works/OL1153861W"),
            InputType::OpenLibraryUrl
        );
    }

    #[test]
    fn detect_openlibrary_books_url() {
        assert_eq!(
            detect_type("https://openlibrary.org/books/OL13955598M"),
            InputType::OpenLibraryUrl
        );
    }

    #[test]
    fn detect_openlibrary_works_json_suffix() {
        assert_eq!(
            detect_type("https://openlibrary.org/works/OL1153861W.json"),
            InputType::OpenLibraryUrl
        );
    }

    #[test]
    fn detect_openlibrary_http() {
        assert_eq!(
            detect_type("http://openlibrary.org/books/OL13955598M"),
            InputType::OpenLibraryUrl
        );
    }

    #[test]
    fn detect_causalai_pdf_url() {
        assert_eq!(detect_type("https://causalai.net/r145.pdf"), InputType::Causalai);
    }

    #[test]
    fn detect_causalai_url_no_pdf() {
        assert_eq!(detect_type("https://causalai.net/r125"), InputType::Causalai);
    }

    #[test]
    fn detect_causalai_www_http() {
        assert_eq!(detect_type("http://www.causalai.net/r152.pdf"), InputType::Causalai);
    }

    #[test]
    fn detect_causalai_scheme() {
        assert_eq!(detect_type("causalai:r145"), InputType::Causalai);
        assert_eq!(detect_type("causalai:152"), InputType::Causalai);
    }

    #[test]
    fn detect_causalai_other_path_is_plain_url() {
        // Non-report causalai.net pages fall through to the generic URL handler.
        assert_eq!(detect_type("https://causalai.net/publications"), InputType::Url);
    }

    #[test]
    fn normalize_causalai_variants() {
        assert_eq!(normalize_causalai("https://causalai.net/r145.pdf").as_deref(), Some("r145"));
        assert_eq!(normalize_causalai("https://www.causalai.net/r125/").as_deref(), Some("r125"));
        assert_eq!(normalize_causalai("causalai:r152").as_deref(), Some("r152"));
        assert_eq!(normalize_causalai("causalai:152").as_deref(), Some("r152"));
        assert_eq!(normalize_causalai("causalai:rabbit").as_deref(), None);
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

    // ── arxiv_id_from_doi ──

    #[test]
    fn arxiv_id_from_doi_new_style() {
        assert_eq!(arxiv_id_from_doi("10.48550/arXiv.2510.24941"), Some("2510.24941".to_string()));
    }

    #[test]
    fn arxiv_id_from_doi_is_case_insensitive_in_the_prefix() {
        // DOIs are case-insensitive, and DataCite lowercases them in some records.
        assert_eq!(arxiv_id_from_doi("10.48550/ARXIV.2510.24941"), Some("2510.24941".to_string()));
        assert_eq!(arxiv_id_from_doi("10.48550/arxiv.2510.24941"), Some("2510.24941".to_string()));
    }

    #[test]
    fn arxiv_id_from_doi_old_style() {
        assert_eq!(arxiv_id_from_doi("10.48550/arXiv.hep-th/9901001"), Some("hep-th/9901001".to_string()));
    }

    #[test]
    fn arxiv_id_from_doi_rejects_other_dois() {
        assert_eq!(arxiv_id_from_doi("10.1145/3442188.3445899"), None);
        // Same registrant, but the suffix is not an arXiv identifier.
        assert_eq!(arxiv_id_from_doi("10.48550/arXiv.notanid"), None);
        assert_eq!(arxiv_id_from_doi("10.48550/arXiv."), None);
        assert_eq!(arxiv_id_from_doi(""), None);
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

    // ── identifier prefixes and URL hosts ──

    #[test]
    fn detect_type_accepts_every_prefix_and_host_form() {
        let cases = [
            ("arxiv:hep-th/9901001", InputType::Arxiv),
            ("arXiv:hep-th/9901001v2", InputType::Arxiv),
            ("ARXIV:2006.11239", InputType::Arxiv),
            ("ArXiv:math.AG/0601001", InputType::Arxiv),
            ("https://www.arxiv.org/abs/2006.11239", InputType::Arxiv),
            ("http://export.arxiv.org/abs/2006.11239", InputType::Arxiv),
            ("doi:10.1145/3442188.3445899", InputType::Doi),
            ("DOI:10.1145/3442188.3445899", InputType::Doi),
            ("doi:2006.11239", InputType::Search),
        ];
        for (input, expected) in cases {
            assert_eq!(detect_type(input), expected, "input {:?}", input);
        }
    }

    #[test]
    fn normalize_arxiv_strips_every_prefix_and_host_form() {
        let cases = [
            ("arxiv:hep-th/9901001", "hep-th/9901001"),
            ("ARXIV:2006.11239v2", "2006.11239"),
            ("ArXiv:math.AG/0601001v1", "math.AG/0601001"),
            ("https://www.arxiv.org/abs/2006.11239v1", "2006.11239"),
            ("http://www.arxiv.org/pdf/2006.11239.pdf", "2006.11239"),
            ("http://export.arxiv.org/abs/2006.11239", "2006.11239"),
            ("https://export.arxiv.org/pdf/2006.11239v3", "2006.11239"),
            ("https://arxiv.org/abs/hep-th/9901001v2", "hep-th/9901001"),
        ];
        for (input, expected) in cases {
            assert_eq!(normalize_arxiv(input), expected, "input {:?}", input);
        }
    }

    #[test]
    fn normalize_doi_strips_a_doi_scheme_in_any_case() {
        let cases = [
            ("doi:10.1145/3442188.3445899", "10.1145/3442188.3445899"),
            ("DOI:10.1145/3442188.3445899", "10.1145/3442188.3445899"),
            ("Doi:10.1145/3442188.3445899", "10.1145/3442188.3445899"),
            ("https://doi.org/10.1145/3442188.3445899", "10.1145/3442188.3445899"),
        ];
        for (input, expected) in cases {
            assert_eq!(normalize_doi(input), expected, "input {:?}", input);
        }
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
