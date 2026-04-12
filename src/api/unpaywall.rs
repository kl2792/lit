use super::PaperResult;
use serde_json::Value;

/// Build URL for looking up open-access PDF by DOI.
pub fn pdf_url(doi: &str) -> String {
    let email = crate::config::Config::get().email();
    format!(
        "https://api.unpaywall.org/v2/{}?email={}",
        doi, email
    )
}

/// Parse the Unpaywall response, extracting title and best PDF URL.
///
/// Prefers `best_oa_location.url_for_pdf`; falls back to `best_oa_location.url`.
pub fn parse_response(body: &str) -> Result<PaperResult, Box<dyn std::error::Error>> {
    let data: Value = serde_json::from_str(body)?;

    let title = data["title"].as_str().unwrap_or("N/A").to_string();

    let oa = &data["best_oa_location"];
    let found_pdf_url = oa["url_for_pdf"]
        .as_str()
        .or_else(|| oa["url"].as_str())
        .map(|s| s.to_string());

    Ok(PaperResult {
        title,
        pdf_url: found_pdf_url,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_response_with_pdf_url() {
        let body = r#"{
            "title": "Attention Is All You Need",
            "best_oa_location": {
                "url_for_pdf": "https://arxiv.org/pdf/1706.03762",
                "url": "https://arxiv.org/abs/1706.03762"
            }
        }"#;
        let r = parse_response(body).unwrap();
        assert_eq!(r.title, "Attention Is All You Need");
        assert_eq!(r.pdf_url.as_deref(), Some("https://arxiv.org/pdf/1706.03762"));
    }

    #[test]
    fn test_parse_response_fallback_to_url() {
        let body = r#"{
            "title": "Some Paper",
            "best_oa_location": {
                "url": "https://example.com/paper"
            }
        }"#;
        let r = parse_response(body).unwrap();
        assert_eq!(r.pdf_url.as_deref(), Some("https://example.com/paper"));
    }

    #[test]
    fn test_parse_response_no_oa_location() {
        let body = r#"{
            "title": "Paywalled Paper",
            "best_oa_location": null
        }"#;
        let r = parse_response(body).unwrap();
        assert_eq!(r.title, "Paywalled Paper");
        assert!(r.pdf_url.is_none());
    }

    #[test]
    fn test_parse_response_missing_title() {
        let body = r#"{
            "best_oa_location": null
        }"#;
        let r = parse_response(body).unwrap();
        assert_eq!(r.title, "N/A");
    }
}
