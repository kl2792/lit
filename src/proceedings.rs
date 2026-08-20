/// Proceedings landing pages -> PDF URLs.
///
/// Conference papers are often reachable only through a publisher's landing
/// page: many have no arXiv mirror, and their DOI (when one exists) resolves to
/// that same page rather than to a file. Unpaywall and the DOI cascade in
/// `cmd::download` therefore come up empty for them. Each host below states its
/// landing page and its PDF in the same URL, so the file is one rewrite away.

use std::sync::LazyLock;

use regex::Regex;

/// NeurIPS: `.../paper/2021/hash/<h>-Abstract.html` and the post-2022
/// `.../paper_files/paper/2023/hash/<h>-Abstract-Conference.html`.
static NEURIPS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^https?://(?:proceedings\.neurips\.cc|papers\.nips\.cc)/.*/hash/[^/]+-Abstract[^/]*\.html$")
        .unwrap()
});

/// PMLR: `.../v139/chen21a.html`, whose PDF nests under a directory of the
/// same name.
static PMLR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^https?://proceedings\.mlr\.press/(v\d+)/([^/]+)\.html$").unwrap()
});

/// OpenReview: `forum?id=X`, plus the `attachment` and `pdf` forms already
/// pointing at a file.
static OPENREVIEW_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^https?://openreview\.net/forum\?id=([^&]+)").unwrap());

/// ACL Anthology: `aclanthology.org/2020.acl-main.1/`.
static ACL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^https?://aclanthology\.org/([^/]+)/?$").unwrap());

/// Any http(s) URL whose path ends in `.pdf`, ignoring query and fragment.
static DIRECT_PDF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^https?://[^?#]+\.pdf(?:[?#]|$)").unwrap());

/// Rewrite a proceedings landing-page URL into its PDF URL.
///
/// A URL that already points at a PDF denotes itself, so a direct PDF link and
/// a landing page reach the fetch path the same way. Returns `None` for every
/// other input, so callers can treat `Some` as "there is a PDF at this URL".
pub fn pdf_url(url: &str) -> Option<String> {
    let url = url.trim();

    if NEURIPS_RE.is_match(url) {
        return Some(url.replace("/hash/", "/file/").replace("-Abstract", "-Paper").replace(".html", ".pdf"));
    }

    if let Some(c) = PMLR_RE.captures(url) {
        return Some(format!("https://proceedings.mlr.press/{}/{}/{}.pdf", &c[1], &c[2], &c[2]));
    }

    if let Some(c) = OPENREVIEW_RE.captures(url) {
        return Some(format!("https://openreview.net/pdf?id={}", &c[1]));
    }

    if let Some(c) = ACL_RE.captures(url) {
        let id = &c[1];
        if !id.ends_with(".pdf") {
            return Some(format!("https://aclanthology.org/{}.pdf", id));
        }
    }

    if DIRECT_PDF_RE.is_match(url) {
        return Some(url.to_string());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neurips_pre_2022_layout() {
        assert_eq!(
            pdf_url("https://proceedings.neurips.cc/paper/2021/hash/fe1f9c70bdf347497e1a01b6c486bdb9-Abstract.html"),
            Some("https://proceedings.neurips.cc/paper/2021/file/fe1f9c70bdf347497e1a01b6c486bdb9-Paper.pdf".into())
        );
    }

    #[test]
    fn neurips_post_2022_layout_keeps_track_suffix() {
        assert_eq!(
            pdf_url("https://proceedings.neurips.cc/paper_files/paper/2023/hash/abc123-Abstract-Conference.html"),
            Some("https://proceedings.neurips.cc/paper_files/paper/2023/file/abc123-Paper-Conference.pdf".into())
        );
    }

    #[test]
    fn neurips_datasets_and_benchmarks_track() {
        assert_eq!(
            pdf_url("https://proceedings.neurips.cc/paper_files/paper/2022/hash/abc-Abstract-Datasets_and_Benchmarks.html"),
            Some("https://proceedings.neurips.cc/paper_files/paper/2022/file/abc-Paper-Datasets_and_Benchmarks.pdf".into())
        );
    }

    #[test]
    fn nips_legacy_host() {
        assert_eq!(
            pdf_url("https://papers.nips.cc/paper/2017/hash/abc-Abstract.html"),
            Some("https://papers.nips.cc/paper/2017/file/abc-Paper.pdf".into())
        );
    }

    #[test]
    fn pmlr_nests_pdf_under_paper_directory() {
        assert_eq!(
            pdf_url("https://proceedings.mlr.press/v139/chen21a.html"),
            Some("https://proceedings.mlr.press/v139/chen21a/chen21a.pdf".into())
        );
    }

    #[test]
    fn openreview_forum_to_pdf() {
        assert_eq!(
            pdf_url("https://openreview.net/forum?id=rJl-b3RcF7"),
            Some("https://openreview.net/pdf?id=rJl-b3RcF7".into())
        );
    }

    #[test]
    fn openreview_drops_trailing_query_params() {
        assert_eq!(
            pdf_url("https://openreview.net/forum?id=rJl-b3RcF7&noteId=xyz"),
            Some("https://openreview.net/pdf?id=rJl-b3RcF7".into())
        );
    }

    #[test]
    fn acl_anthology_with_and_without_slash() {
        let expected = Some("https://aclanthology.org/2020.acl-main.1.pdf".to_string());
        assert_eq!(pdf_url("https://aclanthology.org/2020.acl-main.1/"), expected);
        assert_eq!(pdf_url("https://aclanthology.org/2020.acl-main.1"), expected);
    }

    #[test]
    fn http_scheme_accepted() {
        assert!(pdf_url("http://proceedings.mlr.press/v139/chen21a.html").is_some());
    }

    #[test]
    fn surrounding_whitespace_ignored() {
        assert!(pdf_url("  https://openreview.net/forum?id=abc  ").is_some());
    }

    #[test]
    fn unrecognized_inputs_return_none() {
        for input in [
            "https://arxiv.org/abs/2106.15195",
            "https://doi.org/10.1234/foo",
            "10.1234/foo",
            "structural credit assignment",
            "https://proceedings.mlr.press/v139/",
            "https://example.com/paper.pdfx",
            "ftp://example.com/paper.pdf",
        ] {
            assert_eq!(pdf_url(input), None, "expected None for {}", input);
        }
    }

    #[test]
    fn direct_pdf_urls_denote_themselves() {
        for input in [
            "https://example.com/paper.pdf",
            "http://incompleteideas.net/papers/Sutton-PhD-thesis.pdf",
            "https://aclanthology.org/2020.acl-main.1.pdf",
            "https://proceedings.neurips.cc/paper/2021/file/abc-Paper.pdf",
            "https://example.com/a.pdf?token=1",
        ] {
            assert_eq!(pdf_url(input).as_deref(), Some(input), "expected identity for {}", input);
        }
    }
}
