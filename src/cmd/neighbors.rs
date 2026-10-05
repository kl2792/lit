//! One paper's citation neighbors: Semantic Scholar first, OpenAlex when it fails.
//!
//! Every neighbor list is fetched in full (paginated); a call that fails on
//! both sources is an error, never an empty list.

use crate::api::{openalex, semantic_scholar, PaperResult};

/// Which side of the citation graph to fetch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Papers the node cites.
    Refs,
    /// Papers that cite the node.
    Cites,
}

impl Direction {
    /// Wire name used in JSON output (`refs` / `cites`).
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Refs => "refs",
            Direction::Cites => "cites",
        }
    }

    /// Plural noun for human-readable messages.
    pub fn noun(self) -> &'static str {
        match self {
            Direction::Refs => "references",
            Direction::Cites => "citations",
        }
    }
}

/// The service that answered a neighbor call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    S2,
    OpenAlex,
}

impl Source {
    /// Wire name used in JSON output and as the DB provenance tag.
    pub fn as_str(self) -> &'static str {
        match self {
            Source::S2 => "s2",
            Source::OpenAlex => "openalex",
        }
    }
}

/// A node's full neighbor list and the service that supplied it.
#[derive(Debug)]
pub struct Neighbors {
    pub papers: Vec<PaperResult>,
    pub source: Source,
}

/// Body fetcher; the seam that lets tests replace the network.
pub(crate) trait Fetch {
    async fn get(&self, url: &str) -> Result<String, String>;
}

impl Fetch for crate::http::Client {
    async fn get(&self, url: &str) -> Result<String, String> {
        let key = crate::db::Db::cache_key("graph", url);
        self.get_cached(&key, url, crate::db::TTL_SEARCH)
            .await
            .map_err(|e| e.to_string())
    }
}

/// Paper identifiers parsed from a user-supplied id.
///
/// Accepts `DOI:`/`ARXIV:` prefixes (any case), bare DOIs and arXiv ids, DOI
/// and arXiv URLs; anything else is taken as a Semantic Scholar id
/// (`CorpusId:...`, hex paper id).
pub fn seed_paper(input: &str) -> PaperResult {
    use crate::detect::{arxiv_id_from_doi, detect_type, normalize_arxiv, normalize_doi, InputType};
    let s = input.trim();
    let strip = |prefix: &str| {
        s.get(..prefix.len())
            .filter(|h| h.eq_ignore_ascii_case(prefix))
            .map(|_| &s[prefix.len()..])
    };
    let mut p = PaperResult::default();
    if let Some(rest) = strip("doi:") {
        p.doi = Some(normalize_doi(rest));
    } else if let Some(rest) = strip("arxiv:") {
        p.arxiv_id = Some(normalize_arxiv(rest));
    } else {
        match detect_type(s) {
            InputType::Arxiv => p.arxiv_id = Some(normalize_arxiv(s)),
            InputType::Doi => p.doi = Some(normalize_doi(s)),
            _ => p.s2_id = Some(s.to_string()),
        }
    }
    if p.arxiv_id.is_none() {
        p.arxiv_id = p.doi.as_deref().and_then(arxiv_id_from_doi);
    }
    p
}

/// Fetch every neighbor of `node` in direction `dir`.
///
/// Tries Semantic Scholar, paging until its `next` offset runs out; on any
/// failure falls back to OpenAlex (needs a DOI or arXiv id). The error names
/// both failures.
pub(crate) async fn fetch<F: Fetch>(f: &F, node: &PaperResult, dir: Direction) -> Result<Neighbors, String> {
    let s2_err = match super::s2_api_id(node) {
        Some(id) => match fetch_s2(f, &id, dir).await {
            Ok(papers) => return Ok(Neighbors { papers, source: Source::S2 }),
            Err(e) => e,
        },
        None => "no Semantic Scholar id".to_string(),
    };
    let doi = node
        .doi
        .clone()
        .or_else(|| node.arxiv_id.as_ref().map(|a| format!("10.48550/arXiv.{}", a)))
        .ok_or_else(|| format!("Semantic Scholar: {}; no DOI or arXiv id for an OpenAlex fallback", s2_err))?;
    fetch_openalex(f, &doi, dir)
        .await
        .map(|papers| Neighbors { papers, source: Source::OpenAlex })
        .map_err(|e| format!("Semantic Scholar: {}; OpenAlex: {}", s2_err, e))
}

/// All S2 pages of one neighbor list. Cost: ceil(n / S2_PAGE_MAX) calls.
async fn fetch_s2<F: Fetch>(f: &F, id: &str, dir: Direction) -> Result<Vec<PaperResult>, String> {
    let mut out = Vec::new();
    let mut offset = 0;
    loop {
        let page = match dir {
            Direction::Refs => semantic_scholar::parse_refs_page(&f.get(&semantic_scholar::refs_url(id, offset)).await?),
            Direction::Cites => semantic_scholar::parse_cites_page(&f.get(&semantic_scholar::cites_url(id, offset)).await?),
        };
        let (papers, next) = page.map_err(|e| e.to_string())?;
        out.extend(papers);
        match next {
            Some(n) if n > offset => offset = n,
            _ => return Ok(out),
        }
    }
}

/// One neighbor list from OpenAlex, resolving the work by DOI.
/// Cost: 1 + ceil(refs / IDS_PER_REQUEST) calls for refs, 1 + pages for cites.
async fn fetch_openalex<F: Fetch>(f: &F, doi: &str, dir: Direction) -> Result<Vec<PaperResult>, String> {
    let work = f.get(&openalex::work_by_doi_url(doi)).await?;
    let (work_id, referenced) = openalex::parse_work_graph(&work).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    match dir {
        Direction::Refs => {
            for chunk in referenced.chunks(openalex::IDS_PER_REQUEST) {
                let body = f.get(&openalex::works_by_ids_url(chunk)).await?;
                out.extend(openalex::parse_works_page(&body).map_err(|e| e.to_string())?.0);
            }
        }
        Direction::Cites => {
            let mut cursor = "*".to_string();
            loop {
                let body = f.get(&openalex::cited_by_url(&work_id, &cursor)).await?;
                let (papers, next) = openalex::parse_works_page(&body).map_err(|e| e.to_string())?;
                let empty = papers.is_empty();
                out.extend(papers);
                match next {
                    Some(c) if !empty => cursor = c,
                    _ => break,
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Routes URL substrings to canned bodies or errors; first match wins.
    pub(crate) struct MockFetch {
        pub routes: Vec<(String, Result<String, String>)>,
        pub calls: RefCell<Vec<String>>,
    }

    impl MockFetch {
        pub fn new(routes: Vec<(&str, Result<String, String>)>) -> Self {
            MockFetch {
                routes: routes.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl Fetch for MockFetch {
        async fn get(&self, url: &str) -> Result<String, String> {
            self.calls.borrow_mut().push(url.to_string());
            self.routes
                .iter()
                .find(|(pat, _)| url.contains(pat.as_str()))
                .map(|(_, r)| r.clone())
                .unwrap_or_else(|| Err(format!("HTTP 404 Not Found for {}", url)))
        }
    }

    /// An S2 references/citations page of `n` papers titled `{prefix}{i}`.
    pub(crate) fn s2_page(key: &str, prefix: &str, n: usize, next: Option<usize>) -> String {
        let data: Vec<serde_json::Value> = (0..n)
            .map(|i| {
                serde_json::json!({ key: {
                    "paperId": format!("{}{}", prefix, i),
                    "title": format!("{} paper {}", prefix, i),
                    "externalIds": {"DOI": format!("10.1/{}{}", prefix, i)}
                }})
            })
            .collect();
        let mut body = serde_json::json!({ "data": data });
        if let Some(n) = next {
            body["next"] = serde_json::json!(n);
        }
        body.to_string()
    }

    pub(crate) fn rate_limited() -> Result<String, String> {
        Err("HTTP 429 Too Many Requests after 4 attempts".to_string())
    }

    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(fut)
    }

    fn arxiv_node() -> PaperResult {
        seed_paper("2408.01416")
    }

    #[test]
    fn seed_paper_parses_each_id_form() {
        assert_eq!(seed_paper("2408.01416").arxiv_id.as_deref(), Some("2408.01416"));
        assert_eq!(seed_paper("arXiv:2408.01416v2").arxiv_id.as_deref(), Some("2408.01416"));
        assert_eq!(seed_paper("ARXIV:2408.01416").arxiv_id.as_deref(), Some("2408.01416"));
        assert_eq!(seed_paper("10.1093/bjps/axi147").doi.as_deref(), Some("10.1093/bjps/axi147"));
        assert_eq!(seed_paper("DOI:10.1093/bjps/axi147").doi.as_deref(), Some("10.1093/bjps/axi147"));
        assert_eq!(seed_paper("https://doi.org/10.1093/bjps/axi147").doi.as_deref(), Some("10.1093/bjps/axi147"));
        let s2 = seed_paper("CorpusId:123");
        assert_eq!(s2.s2_id.as_deref(), Some("CorpusId:123"));
        assert!(s2.doi.is_none() && s2.arxiv_id.is_none());
    }

    #[test]
    fn seed_paper_arxiv_doi_also_sets_arxiv_id() {
        let p = seed_paper("10.48550/arXiv.2408.01416");
        assert_eq!(p.arxiv_id.as_deref(), Some("2408.01416"));
    }

    /// The old client asked S2 for `limit=50` and stopped: more than 50
    /// neighbors must come back in full.
    #[test]
    fn s2_returns_more_than_fifty_neighbors() {
        let f = MockFetch::new(vec![("references", Ok(s2_page("citedPaper", "r", 120, None)))]);
        let n = block_on(fetch(&f, &arxiv_node(), Direction::Refs)).unwrap();
        assert_eq!(n.papers.len(), 120);
        assert_eq!(n.source, Source::S2);
    }

    #[test]
    fn s2_follows_next_offset_across_pages() {
        let f = MockFetch::new(vec![
            ("offset=0", Ok(s2_page("citingPaper", "a", 1000, Some(1000)))),
            ("offset=1000", Ok(s2_page("citingPaper", "b", 30, None))),
        ]);
        let n = block_on(fetch(&f, &arxiv_node(), Direction::Cites)).unwrap();
        assert_eq!(n.papers.len(), 1030);
        assert_eq!(f.calls.borrow().len(), 2);
    }

    #[test]
    fn s2_rate_limit_falls_back_to_openalex_refs() {
        let f = MockFetch::new(vec![
            ("semanticscholar", rate_limited()),
            ("works/doi:10.48550/arXiv.2408.01416", Ok(r#"{"id": "https://openalex.org/W9",
                "referenced_works": ["https://openalex.org/W1", "https://openalex.org/W2"]}"#.into())),
            ("filter=openalex:W1|W2", Ok(r#"{"meta": {}, "results": [
                {"title": "One", "doi": "https://doi.org/10.1/one"},
                {"title": "Two"}]}"#.into())),
        ]);
        let n = block_on(fetch(&f, &arxiv_node(), Direction::Refs)).unwrap();
        assert_eq!(n.source, Source::OpenAlex);
        let titles: Vec<&str> = n.papers.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, vec!["One", "Two"]);
    }

    #[test]
    fn openalex_refs_batch_ids_per_request() {
        let ids: Vec<String> = (0..150).map(|i| format!("\"https://openalex.org/W{}\"", i)).collect();
        let work = format!(r#"{{"id": "https://openalex.org/W9", "referenced_works": [{}]}}"#, ids.join(","));
        let f = MockFetch::new(vec![
            ("semanticscholar", rate_limited()),
            ("works/doi:", Ok(work)),
            ("filter=openalex:", Ok(r#"{"results": [{"title": "x"}]}"#.into())),
        ]);
        let n = block_on(fetch(&f, &arxiv_node(), Direction::Refs)).unwrap();
        // 150 ids at 100 per request: two batch calls, each answered with one work.
        assert_eq!(n.papers.len(), 2);
        let batches = f.calls.borrow().iter().filter(|u| u.contains("filter=openalex:")).count();
        assert_eq!(batches, 2);
    }

    #[test]
    fn s2_failure_falls_back_to_openalex_cites_with_cursor() {
        let f = MockFetch::new(vec![
            ("semanticscholar", Err("HTTP 503 Service Unavailable".into())),
            ("works/doi:10.1234/x", Ok(r#"{"id": "https://openalex.org/W9", "referenced_works": []}"#.into())),
            ("cursor=c2", Ok(r#"{"meta": {"next_cursor": null}, "results": [{"title": "B"}]}"#.into())),
            ("filter=cites:W9", Ok(r#"{"meta": {"next_cursor": "c2"}, "results": [{"title": "A"}]}"#.into())),
        ]);
        let node = seed_paper("10.1234/x");
        let n = block_on(fetch(&f, &node, Direction::Cites)).unwrap();
        assert_eq!(n.source, Source::OpenAlex);
        assert_eq!(n.papers.len(), 2);
    }

    #[test]
    fn failure_on_both_sources_is_an_error_naming_both() {
        let f = MockFetch::new(vec![("semanticscholar", rate_limited())]);
        let err = block_on(fetch(&f, &arxiv_node(), Direction::Refs)).unwrap_err();
        assert!(err.contains("429"), "err: {}", err);
        assert!(err.contains("OpenAlex"), "err: {}", err);
    }

    #[test]
    fn s2_failure_without_doi_or_arxiv_is_an_error() {
        let f = MockFetch::new(vec![("semanticscholar", rate_limited())]);
        let err = block_on(fetch(&f, &seed_paper("CorpusId:1"), Direction::Refs)).unwrap_err();
        assert!(err.contains("429"), "err: {}", err);
        assert_eq!(f.calls.borrow().len(), 1, "no OpenAlex call without an id");
    }
}
