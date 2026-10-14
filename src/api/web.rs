//! Web-published papers on an allowlist of hosts (ADR-005): AI Alignment
//! Forum and LessWrong posts (with their GreaterWrong mirror), Distill
//! articles, and Transformer Circuits Thread articles.
//!
//! Everything here is pure: URL classification, the URL to fetch, and the
//! parsers that turn a fetched body into metadata plus plain text.

use crate::html::{self, Element, Node};

/// A supported publishing site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site {
    AlignmentForum,
    LessWrong,
    Distill,
    TransformerCircuits,
}

/// Hosts `lit read` and `lit add` accept, for the unsupported-host error.
pub const SUPPORTED_HOSTS: &[&str] =
    &["alignmentforum.org", "lesswrong.com", "greaterwrong.com", "distill.pub", "transformer-circuits.pub"];

impl Site {
    /// The site's name, used as the BibTeX `howpublished` value.
    pub fn name(self) -> &'static str {
        match self {
            Site::AlignmentForum => "AI Alignment Forum",
            Site::LessWrong => "LessWrong",
            Site::Distill => "Distill",
            Site::TransformerCircuits => "Transformer Circuits Thread",
        }
    }

    /// Stable identifier recorded as `host` in `source.yaml`.
    pub fn id(self) -> &'static str {
        match self {
            Site::AlignmentForum => "alignmentforum",
            Site::LessWrong => "lesswrong",
            Site::Distill => "distill",
            Site::TransformerCircuits => "transformer-circuits",
        }
    }

    fn from_host(host: &str) -> Option<Site> {
        match host.strip_prefix("www.").unwrap_or(host) {
            "alignmentforum.org" => Some(Site::AlignmentForum),
            "lesswrong.com" | "greaterwrong.com" => Some(Site::LessWrong),
            "distill.pub" => Some(Site::Distill),
            "transformer-circuits.pub" => Some(Site::TransformerCircuits),
            _ => None,
        }
    }
}

/// A supported page: its site, canonical URL and, for forum posts, post id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebUrl {
    pub site: Site,
    /// The page URL with scheme `https`, no query or fragment, and no
    /// `index.html`; forum posts reduce to `https://<site host>/posts/<id>`.
    pub url: String,
    pub post_id: Option<String>,
}

impl WebUrl {
    /// What identifies the work across URL spellings: a forum post's id
    /// (one post is served by LessWrong, the Alignment Forum and
    /// GreaterWrong alike), otherwise the canonical URL.
    pub fn identity(&self) -> String {
        match &self.post_id {
            Some(id) => format!("lesswrong-post:{}", id),
            None => self.url.clone(),
        }
    }

    /// The URL whose body `parse_page` reads: the LessWrong GraphQL query for
    /// a forum post, otherwise the page itself.
    pub fn fetch_url(&self) -> String {
        match &self.post_id {
            Some(id) => graphql_url(id),
            None => self.url.clone(),
        }
    }
}

/// True for an `http://` or `https://` URL.
pub fn is_http_url(input: &str) -> bool {
    let lower = input.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Classify and canonicalize a URL on a supported host.
///
/// The error for any other host names the supported ones; lit does not
/// scrape arbitrary pages (ADR-005).
pub fn parse_url(input: &str) -> Result<WebUrl, String> {
    let (host, path) = host_and_path(input).ok_or_else(|| format!("not an http(s) URL: {}", input))?;
    let site = Site::from_host(&host).ok_or_else(|| {
        format!("unsupported host '{}' in {}; lit reads web pages only from: {}", host, input, SUPPORTED_HOSTS.join(", "))
    })?;
    match site {
        Site::AlignmentForum | Site::LessWrong => {
            let id = post_id(path)
                .ok_or_else(|| format!("expected a post URL of the form https://{}/posts/<id>/<slug>: {}", host, input))?;
            let canonical_host = if site == Site::AlignmentForum { "www.alignmentforum.org" } else { "www.lesswrong.com" };
            Ok(WebUrl { site, url: format!("https://{}/posts/{}", canonical_host, id), post_id: Some(id) })
        }
        Site::Distill | Site::TransformerCircuits => {
            let mut path = path.strip_suffix("index.html").unwrap_or(path).to_string();
            let last = path.rsplit('/').next().unwrap_or("");
            if !path.ends_with('/') && !last.contains('.') {
                path.push('/');
            }
            if path.len() <= 1 {
                return Err(format!("expected an article URL, got the site root: {}", input));
            }
            Ok(WebUrl { site, url: format!("https://{}{}", host.trim_start_matches("www."), path), post_id: None })
        }
    }
}

/// True for an http(s) URL on a supported host, whatever its path; such a
/// URL goes to `parse_url`, which reports a malformed path.
pub fn is_supported(input: &str) -> bool {
    host_and_path(input).is_some_and(|(host, _)| Site::from_host(&host).is_some())
}

/// The lowercased host and the path (query and fragment removed) of an
/// http(s) URL.
fn host_and_path(input: &str) -> Option<(String, &str)> {
    let trimmed = input.trim();
    let lower = trimmed.to_ascii_lowercase();
    let after_scheme = ["https://", "http://"].iter().find(|s| lower.starts_with(*s)).map(|s| &trimmed[s.len()..])?;
    let end = after_scheme.find(['?', '#']).unwrap_or(after_scheme.len());
    let without_query = &after_scheme[..end];
    let (host, path) = without_query.split_at(without_query.find('/').unwrap_or(without_query.len()));
    Some((host.to_ascii_lowercase(), path))
}

/// The post id in a `/posts/<id>[/<slug>]` path.
fn post_id(path: &str) -> Option<String> {
    let mut parts = path.split('/').filter(|p| !p.is_empty());
    (parts.next()? == "posts").then_some(())?;
    let id = parts.next()?;
    id.chars().all(|c| c.is_ascii_alphanumeric()).then(|| id.to_string())
}

/// GraphQL GET for one post's title, body, date and authors. Alignment
/// Forum posts are also LessWrong posts, so one endpoint serves both.
pub fn graphql_url(post_id: &str) -> String {
    let query = format!(
        "{{ post(input: {{selector: {{_id: \"{}\"}}}}) {{ result {{ title postedAt htmlBody user {{ displayName }} coauthors {{ displayName }} }} }} }}",
        post_id
    );
    format!("https://www.lesswrong.com/graphql?query={}", super::urlencode(&query))
}

/// A page's bibliographic metadata and plain-text body.
#[derive(Debug, Clone, PartialEq)]
pub struct WebPage {
    pub title: String,
    /// Display names in "First Last" order where the source allows.
    pub authors: Vec<String>,
    /// ISO date (`YYYY-MM-DD`) when the source gives a parseable one.
    pub date: Option<String>,
    /// Four-digit year, or empty when the page states none.
    pub year: String,
    pub doi: Option<String>,
    pub text: String,
    /// Where the body came from, recorded as `extraction` in `source.yaml`.
    pub extraction: &'static str,
}

/// Parse a fetched body for `site`.
pub fn parse_page(site: Site, body: &str) -> Result<WebPage, String> {
    match site {
        Site::AlignmentForum | Site::LessWrong => parse_lesswrong(body),
        Site::Distill | Site::TransformerCircuits => parse_distill_template(body),
    }
}

/// Parse a LessWrong GraphQL `post` response.
pub fn parse_lesswrong(body: &str) -> Result<WebPage, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| format!("LessWrong GraphQL: invalid JSON: {}", e))?;
    if let Some(msg) = v["errors"][0]["message"].as_str() {
        return Err(format!("LessWrong GraphQL error: {}", msg));
    }
    let post = &v["data"]["post"]["result"];
    if post.is_null() {
        return Err("LessWrong GraphQL: no such post".to_string());
    }
    let title = html::collapse_whitespace(post["title"].as_str().unwrap_or(""));
    let mut authors: Vec<String> = Vec::new();
    let names = std::iter::once(&post["user"]).chain(post["coauthors"].as_array().into_iter().flatten());
    for name in names.filter_map(|u| u["displayName"].as_str()).map(html::collapse_whitespace) {
        if !name.is_empty() && !authors.contains(&name) {
            authors.push(name);
        }
    }
    let date = post["postedAt"].as_str().and_then(iso_date);
    let text = html::render_text(&html::parse(post["htmlBody"].as_str().unwrap_or("")));
    finish(WebPage { title, authors, year: year_of(&date), date, doi: None, text, extraction: "lesswrong-graphql-htmlBody" })
}

/// Parse a page built on the Distill template, which Distill and the
/// Transformer Circuits Thread both use.
///
/// Title: `citation_title`, else the `<d-front-matter>` JSON, else `<h1>`,
/// else `<title>`. Authors: `citation_author` (stored "Last, First"), else
/// the front matter, else the byline (`span.author` or `p.author a.name`).
/// Date: `citation_publication_date`, else the byline's Published block.
/// Body: `<d-article>`, else `<article>`, else `<main>`.
pub fn parse_distill_template(body: &str) -> Result<WebPage, String> {
    let doc = html::parse(body);
    let front: serde_json::Value = html::find(&doc, &|e| e.name == "d-front-matter")
        .and_then(|fm| html::find(&fm.children, &|e| e.name == "script" || e.name == "code"))
        .and_then(|e| serde_json::from_str(&e.text()).ok())
        .unwrap_or_default();

    let title = html::meta_all(&doc, "citation_title")
        .into_iter()
        .next()
        .or_else(|| front["title"].as_str().map(html::collapse_whitespace))
        .or_else(|| html::find(&doc, &|e| e.name == "h1").map(Element::text))
        .or_else(|| html::find(&doc, &|e| e.name == "title").map(Element::text))
        .unwrap_or_default();

    let mut authors: Vec<String> = html::meta_all(&doc, "citation_author").iter().map(|a| first_last(a)).collect();
    if authors.is_empty() {
        authors = front["authors"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a["author"].as_str().map(html::collapse_whitespace))
            .collect();
    }
    if authors.is_empty() {
        authors = byline_authors(&doc);
    }

    let date = html::meta_all(&doc, "citation_publication_date")
        .first()
        .and_then(|d| iso_date(d))
        .or_else(|| published_date(&doc));
    let doi = html::meta_all(&doc, "citation_doi").into_iter().next();

    let (extraction, article) = ["d-article", "article", "main"]
        .iter()
        .find_map(|name| html::find(&doc, &|e| e.name == *name).map(|e| (*name, e)))
        .ok_or("no <d-article>, <article> or <main> element in the page")?;
    let text = html::render_text(&article.children);
    finish(WebPage { title, authors, year: year_of(&date), date, doi, text, extraction })
}

/// Reject a page with no title or no body text: either means the page is not
/// the article the URL named (an error page, a login wall, a layout change).
fn finish(page: WebPage) -> Result<WebPage, String> {
    if page.title.is_empty() {
        return Err("page has no title".to_string());
    }
    if page.text.is_empty() {
        return Err("page has no body text".to_string());
    }
    Ok(page)
}

/// Byline author names: `span.author` (Transformer Circuits) or
/// `p.author a.name` (Distill), with footnote marks and separators removed.
fn byline_authors(doc: &[Node]) -> Vec<String> {
    let spans = html::find_all(doc, &|e| e.name == "span" && e.has_class("author"));
    let names: Vec<String> = if spans.is_empty() {
        html::find_all(doc, &|e| e.name == "a" && e.has_class("name")).into_iter().map(Element::text).collect()
    } else {
        spans.into_iter().map(|span| text_without(span, "sup")).collect()
    };
    names
        .into_iter()
        .map(|n| n.trim_end_matches([',', ';', ' ']).to_string())
        .filter(|n| !n.is_empty())
        .collect()
}

/// An element's text with every `<skip>` subtree removed.
fn text_without(e: &Element, skip: &str) -> String {
    fn walk(nodes: &[Node], skip: &str, out: &mut String) {
        for node in nodes {
            match node {
                Node::Text(t) => out.push_str(t),
                Node::Element(c) if c.name != skip => walk(&c.children, skip, out),
                Node::Element(_) => {}
            }
        }
    }
    let mut out = String::new();
    walk(&e.children, skip, &mut out);
    html::collapse_whitespace(&out)
}

/// The date under the byline's "Published" heading (`div.published`).
fn published_date(doc: &[Node]) -> Option<String> {
    let block = html::find(doc, &|e| e.has_class("published"))?;
    let value = html::find(&block.children, &|e| e.name == "div").map(Element::text)?;
    iso_date(&value)
}

/// "Last, First" to "First Last"; other forms unchanged.
fn first_last(name: &str) -> String {
    match name.split_once(',') {
        Some((last, first)) if !first.trim().is_empty() => format!("{} {}", first.trim(), last.trim()),
        _ => name.trim().to_string(),
    }
}

/// Normalize "2020/03/10", "2022-12-03T00:58:36Z" or "Dec 22, 2021" to
/// `YYYY-MM-DD`; `None` for anything else.
fn iso_date(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let numeric: Vec<&str> = raw.get(..10).unwrap_or(raw).split(['/', '-']).collect();
    if let [y, m, d] = numeric[..]
        && y.len() == 4
        && [y, m, d].iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    {
        return Some(format!("{}-{:0>2}-{:0>2}", y, m, d));
    }
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let words: Vec<String> = raw.split([' ', ',']).filter(|w| !w.is_empty()).map(str::to_ascii_lowercase).collect();
    if let [month, day, year] = &words[..] {
        let m = MONTHS.iter().position(|p| month.starts_with(p))? + 1;
        let d: u32 = day.parse().ok()?;
        let y: u32 = year.parse().ok().filter(|y| (1000..10000).contains(y))?;
        return Some(format!("{}-{:02}-{:02}", y, m, d));
    }
    None
}

fn year_of(date: &Option<String>) -> String {
    date.as_deref().map(|d| d[..4].to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISTILL: &str = include_str!("../../test/web/distill-zoom-in.html");
    const TC: &str = include_str!("../../test/web/transformer-circuits-framework.html");
    const LW: &str = include_str!("../../test/web/lesswrong-causal-scrubbing.json");

    #[test]
    fn supported_urls_canonicalize() {
        let cases = [
            ("https://distill.pub/2020/circuits/zoom-in/", Site::Distill, "https://distill.pub/2020/circuits/zoom-in/"),
            ("http://distill.pub/2020/circuits/zoom-in#claim-1", Site::Distill, "https://distill.pub/2020/circuits/zoom-in/"),
            (
                "https://transformer-circuits.pub/2021/framework/index.html",
                Site::TransformerCircuits,
                "https://transformer-circuits.pub/2021/framework/",
            ),
            (
                "https://www.alignmentforum.org/posts/JvZhhzycHu2Yd57RN/causal-scrubbing-a-method?commentId=x",
                Site::AlignmentForum,
                "https://www.alignmentforum.org/posts/JvZhhzycHu2Yd57RN",
            ),
            ("https://lesswrong.com/posts/JvZhhzycHu2Yd57RN", Site::LessWrong, "https://www.lesswrong.com/posts/JvZhhzycHu2Yd57RN"),
            (
                "https://www.greaterwrong.com/posts/JvZhhzycHu2Yd57RN/slug",
                Site::LessWrong,
                "https://www.lesswrong.com/posts/JvZhhzycHu2Yd57RN",
            ),
        ];
        for (input, site, url) in cases {
            let u = parse_url(input).unwrap();
            assert_eq!((u.site, u.url.as_str()), (site, url), "input: {}", input);
        }
    }

    #[test]
    fn one_post_has_one_identity_on_every_forum_host() {
        let af = parse_url("https://www.alignmentforum.org/posts/JvZhhzycHu2Yd57RN/a").unwrap();
        let lw = parse_url("https://www.lesswrong.com/posts/JvZhhzycHu2Yd57RN/b").unwrap();
        assert_eq!(af.identity(), lw.identity());
        assert_ne!(af.site, lw.site);
    }

    #[test]
    fn unsupported_host_error_names_every_supported_host() {
        let err = parse_url("https://example.com/post/1").unwrap_err();
        assert!(err.contains("unsupported host 'example.com'"), "err: {}", err);
        for host in SUPPORTED_HOSTS {
            assert!(err.contains(host), "err: {}", err);
        }
    }

    #[test]
    fn is_supported_checks_the_host_only() {
        assert!(is_supported("https://distill.pub/"));
        assert!(is_supported("HTTP://www.LessWrong.com/tag/x?y=1"));
        assert!(!is_supported("https://example.com/distill.pub/x"));
        assert!(!is_supported("distill.pub/2020/x"));
    }

    #[test]
    fn is_http_url_needs_an_http_scheme() {
        assert!(is_http_url(" HTTPS://example.com/x"));
        assert!(is_http_url("http://example.com"));
        assert!(!is_http_url("ftp://example.com"));
        assert!(!is_http_url("10.23915/distill.00024.001"));
    }

    #[test]
    fn forum_url_without_a_post_id_is_rejected() {
        assert!(parse_url("https://www.lesswrong.com/tag/interpretability").unwrap_err().contains("/posts/<id>"));
        assert!(parse_url("https://distill.pub/").unwrap_err().contains("site root"));
    }

    #[test]
    fn graphql_url_queries_the_named_post_and_fields() {
        let url = parse_url("https://www.alignmentforum.org/posts/JvZhhzycHu2Yd57RN/x").unwrap().fetch_url();
        assert!(url.starts_with("https://www.lesswrong.com/graphql?query="), "url: {}", url);
        for part in ["JvZhhzycHu2Yd57RN", "htmlBody", "postedAt", "coauthors", "displayName"] {
            assert!(url.contains(part), "missing {}: {}", part, url);
        }
    }

    #[test]
    fn distill_metadata_comes_from_citation_meta_tags() {
        let page = parse_page(Site::Distill, DISTILL).unwrap();
        assert_eq!(page.title, "Zoom In: An Introduction to Circuits");
        assert_eq!(page.authors, vec!["Chris Olah", "Nick Cammarata", "Ludwig Schubert"]);
        assert_eq!(page.date.as_deref(), Some("2020-03-10"));
        assert_eq!(page.year, "2020");
        assert_eq!(page.doi.as_deref(), Some("10.23915/distill.00024.001"));
        assert_eq!(page.extraction, "d-article");
    }

    #[test]
    fn distill_body_is_the_article_without_chrome() {
        let text = parse_page(Site::Distill, DISTILL).unwrap().text;
        assert!(text.starts_with("Many important transition points"), "text: {}", text);
        assert!(text.contains("science \u{201c}zoomed in.\u{201d}"), "text: {}", text);
        assert!(text.contains("## Three Speculative Claims"), "text: {}", text);
        assert!(text.contains("Micrographia[hooke1666micrographia] revealed"), "text: {}", text);
        assert!(text.contains("[footnote: By \u{201c}direction\u{201d} we mean"), "text: {}", text);
        assert!(text.contains("- Claim 1: Features\n- Claim 2: Circuits"), "text: {}", text);
        for absent in ["Contents", "Introduction", "svg label", "console.log", "Glossary", "Distill footer", "About"] {
            assert!(!text.contains(absent), "unexpected {:?} in: {}", absent, text);
        }
    }

    #[test]
    fn transformer_circuits_metadata_comes_from_the_byline() {
        let page = parse_page(Site::TransformerCircuits, TC).unwrap();
        assert_eq!(page.title, "A Mathematical Framework for Transformer Circuits");
        assert_eq!(page.authors, vec!["Nelson Elhage", "Neel Nanda", "Catherine Olsson", "Amanda Askell", "Chris Olah"]);
        assert_eq!(page.date.as_deref(), Some("2021-12-22"));
        assert_eq!(page.year, "2021");
        assert_eq!(page.doi, None);
    }

    #[test]
    fn transformer_circuits_body_keeps_math_citations_and_code() {
        let text = parse_page(Site::TransformerCircuits, TC).unwrap().text;
        assert!(text.starts_with("## Summary of Results"), "text: {}", text);
        assert!(text.contains("Transformer[vaswani2017attention] language models"), "text: {}", text);
        assert!(text.contains("matrices $W_Q^TW_K$ and $W_OW_V$."), "text: {}", text);
        assert!(text.contains("$$h(x) = \\sum_i W_O^i W_V^i x$$"), "text: {}", text);
        assert!(text.contains("def attn(x):\n    return softmax(x)"), "text: {}", text);
        assert_eq!(text.matches("Summary of Results").count(), 1, "nav repeated the heading: {}", text);
        for absent in ["text-decoration", "Author Contributions", "Everyone helped"] {
            assert!(!text.contains(absent), "unexpected {:?} in: {}", absent, text);
        }
    }

    #[test]
    fn lesswrong_metadata_and_body_come_from_graphql() {
        let page = parse_page(Site::AlignmentForum, LW).unwrap();
        assert_eq!(page.title, "Causal Scrubbing: a method for rigorously testing interpretability hypotheses [Redwood Research]");
        assert_eq!(page.authors, vec!["LawrenceC", "Adri\u{e0} Garriga-alonso", "ryan_greenblatt", "Buck"]);
        assert_eq!(page.date.as_deref(), Some("2022-12-03"));
        assert_eq!(page.year, "2022");
        assert_eq!(page.extraction, "lesswrong-graphql-htmlBody");
        let text = page.text;
        assert!(text.starts_with("* Authors sorted alphabetically."), "text: {}", text);
        assert!(text.contains("# 1 Introduction"), "text: {}", text);
        assert!(text.contains("We assume a dataset $D$ over a domain."), "text: {}", text);
        assert!(text.contains("def scrub(model):\n    return model"), "text: {}", text);
        assert!(text.contains("- Ad hoc methods include ablations."), "text: {}", text);
        assert!(!text.contains("mjx-chtml"), "text: {}", text);
    }

    #[test]
    fn lesswrong_missing_post_and_graphql_errors_are_errors() {
        assert!(parse_lesswrong(r#"{"data":{"post":{"result":null}}}"#).unwrap_err().contains("no such post"));
        let err = parse_lesswrong(r#"{"errors":[{"message":"bad id"}],"data":null}"#).unwrap_err();
        assert!(err.contains("bad id"), "err: {}", err);
    }

    #[test]
    fn a_page_without_an_article_or_title_is_rejected() {
        assert!(parse_distill_template("<html><title>Not found</title><p>x</p></html>").unwrap_err().contains("<d-article>"));
        assert!(parse_distill_template("<d-article><p>body</p></d-article>").unwrap_err().contains("no title"));
    }

    #[test]
    fn dates_normalize_to_iso() {
        assert_eq!(iso_date("2020/03/10").as_deref(), Some("2020-03-10"));
        assert_eq!(iso_date("2022-12-03T00:58:36.973Z").as_deref(), Some("2022-12-03"));
        assert_eq!(iso_date("Dec 22, 2021").as_deref(), Some("2021-12-22"));
        assert_eq!(iso_date("March 4, 2024").as_deref(), Some("2024-03-04"));
        assert_eq!(iso_date("someday"), None);
    }

    #[test]
    fn citekeys_follow_the_author_year_word_scheme() {
        let key = |site, body| {
            let p = parse_page(site, body).unwrap();
            crate::citekey::generate(&p.authors, &p.year, &p.title)
        };
        assert_eq!(key(Site::Distill, DISTILL), "olah2020zoom");
        assert_eq!(key(Site::TransformerCircuits, TC), "elhage2021mathematical");
        assert_eq!(key(Site::LessWrong, LW), "lawrencec2022causal");
    }
}
