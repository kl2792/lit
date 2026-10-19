//! ACL Anthology: open-access PDFs for ACL DOIs.

use std::sync::LazyLock;

use regex::Regex;

/// An ACL DOI: `10.18653/v1/<id>`, or `10.3115/v1/<id>` for 2014-2015 papers,
/// where `<id>` is an old-style (`P19-1452`) or new-style
/// (`2020.emnlp-main.263`) Anthology id.
/// Capture 1 is an old-style id, capture 2 a new-style one.
static ACL_DOI_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^10\.(?:18653|3115)/v1/(?:([a-z]\d{2}-\d{4})|(\d{4}\.[a-z0-9-]+\.\d+))$").unwrap()
});

/// The Anthology PDF URL for an ACL DOI, or `None` for any other DOI.
///
/// DOIs are case-insensitive but Anthology URLs are not, so the id is
/// respelled as the Anthology writes it: old-style uppercase, new-style lowercase.
pub fn pdf_url(doi: &str) -> Option<String> {
    let caps = ACL_DOI_RE.captures(doi.trim())?;
    let id = match (caps.get(1), caps.get(2)) {
        (Some(old), _) => old.as_str().to_ascii_uppercase(),
        (_, Some(new)) => new.as_str().to_ascii_lowercase(),
        _ => return None,
    };
    Some(format!("https://aclanthology.org/{}.pdf", id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_acl_doi_maps_to_its_anthology_pdf() {
        let cases = [
            ("10.18653/v1/2020.emnlp-main.263", "https://aclanthology.org/2020.emnlp-main.263.pdf"),
            ("10.18653/v1/P19-1452", "https://aclanthology.org/P19-1452.pdf"),
            // 2014-2015 ACL papers carry the 10.3115/v1/ form.
            ("10.3115/v1/P15-1115", "https://aclanthology.org/P15-1115.pdf"),
        ];
        for (doi, url) in cases {
            assert_eq!(pdf_url(doi).as_deref(), Some(url), "doi {doi}");
        }
    }

    #[test]
    fn an_acl_doi_in_the_wrong_case_maps_to_the_anthology_spelling() {
        // DOIs are case-insensitive, but Anthology URLs are not: an old-style
        // id has an uppercase letter and a new-style id is all lowercase.
        let cases = [
            ("10.18653/V1/2020.ACL-MAIN.148", "https://aclanthology.org/2020.acl-main.148.pdf"),
            ("10.18653/v1/p19-1452", "https://aclanthology.org/P19-1452.pdf"),
            ("10.3115/V1/w14-0301", "https://aclanthology.org/W14-0301.pdf"),
        ];
        for (doi, url) in cases {
            assert_eq!(pdf_url(doi).as_deref(), Some(url), "doi {doi}");
        }
    }

    #[test]
    fn a_doi_outside_the_anthology_maps_to_nothing() {
        let cases = [
            "10.1145/3442188.3445899",
            // Pre-2014 10.3115 DOIs are ACM-style numbers, not Anthology ids.
            "10.3115/1073083.1073135",
            "10.18653/v1/",
            "10.18653/",
            "10.18653/v1/2020.emnlp-main.263/extra",
            "",
        ];
        for doi in cases {
            assert_eq!(pdf_url(doi), None, "doi {doi}");
        }
    }
}
