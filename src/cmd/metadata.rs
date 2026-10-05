/// Bibliographic metadata for an arXiv id, with fallbacks.
///
/// The arXiv API is the authoritative source, but it rate-limits for hours at a
/// time. Semantic Scholar and OpenAlex both index arXiv preprints, so when the
/// arXiv API fails they supply the same record and acquisition can continue.

use crate::api::{arxiv, openalex, semantic_scholar, PaperResult};
use crate::db::Db;

/// Cached GET for metadata lookups: `load` reads the cache under `key` or
/// fetches `url` without caching, and `store` caches a body once it has parsed
/// into a titled record, so an HTTP 200 arXiv feed with no entry is never
/// cached as the paper's record.
pub(crate) trait MetadataFetch {
    async fn load(&self, key: &str, url: &str) -> Result<String, String>;
    fn store(&self, key: &str, url: &str, body: &str);
}

impl MetadataFetch for crate::http::Client {
    async fn load(&self, key: &str, url: &str) -> Result<String, String> {
        self.get_cached_deferred(key, url, crate::db::TTL_DOI)
            .await
            .map_err(|e| e.to_string())
    }

    fn store(&self, key: &str, url: &str, body: &str) {
        self.cache_set(key, url, body);
    }
}

/// Parses one provider's response body into a record.
type Parse = fn(&str) -> Result<PaperResult, Box<dyn std::error::Error>>;

/// The service that supplied an arXiv paper's metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataSource {
    Arxiv,
    SemanticScholar,
    OpenAlex,
}

impl MetadataSource {
    /// Stable identifier recorded in `source.yaml` and the paper index.
    pub fn id(self) -> &'static str {
        match self {
            MetadataSource::Arxiv => "arxiv",
            MetadataSource::SemanticScholar => "semantic_scholar",
            MetadataSource::OpenAlex => "openalex",
        }
    }

    /// Human-readable name for notes.
    pub fn name(self) -> &'static str {
        match self {
            MetadataSource::Arxiv => "arXiv API",
            MetadataSource::SemanticScholar => "Semantic Scholar",
            MetadataSource::OpenAlex => "OpenAlex",
        }
    }
}

/// An arXiv paper's record and the service that supplied it.
#[derive(Debug, Clone)]
pub struct Metadata {
    pub paper: PaperResult,
    pub source: MetadataSource,
}

/// Resolve metadata for `arxiv_id`: the arXiv API, then Semantic Scholar's
/// `arXiv:<id>` record, then OpenAlex's record for the arXiv DOI. The first
/// record with a title wins; later sources are not queried.
///
/// Prints a one-line note when a fallback supplied the record.
/// The error names every source's failure.
/// Responses are cached under the keys `lit check` and `lookup_arxiv_data`
/// (`arxiv`) and `lit add` (`s2_paper`) already use, so the commands share them.
/// Cost: at most three lookups, each under the HTTP client's retry budget.
pub(crate) async fn arxiv_metadata<F: MetadataFetch>(f: &F, arxiv_id: &str) -> Result<Metadata, String> {
    let lookups: [(MetadataSource, &str, String, Parse); 3] = [
        (MetadataSource::Arxiv, "arxiv", arxiv::query_url(arxiv_id), arxiv::parse_entry),
        (
            MetadataSource::SemanticScholar,
            "s2_paper",
            semantic_scholar::paper_url(&format!("arXiv:{}", arxiv_id)),
            semantic_scholar::parse_paper,
        ),
        (MetadataSource::OpenAlex, "openalex_arxiv", openalex::work_by_arxiv_url(arxiv_id), openalex::parse_work_paper),
    ];
    let mut failures = Vec::new();
    for (source, prefix, url, parse) in lookups {
        let key = Db::cache_key(prefix, arxiv_id);
        let parsed = f.load(&key, &url).await.and_then(|body| {
            let paper = parse(&body).map_err(|e| e.to_string())?;
            if paper.title.trim().is_empty() {
                return Err("record has no title".to_string());
            }
            f.store(&key, &url, &body);
            Ok(paper)
        });
        match parsed {
            Ok(paper) => {
                if source != MetadataSource::Arxiv {
                    crate::format::info(&format!(
                        "note: metadata for arXiv:{} from {} (arXiv API unavailable)",
                        arxiv_id,
                        source.name()
                    ));
                }
                return Ok(Metadata { paper, source });
            }
            Err(e) => failures.push(format!("{}: {}", source.name(), e)),
        }
    }
    Err(format!("no metadata for arXiv:{} ({})", arxiv_id, failures.join("; ")))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::cmd::neighbors::tests::MockFetch;
    use crate::cmd::neighbors::Fetch;
    use std::cell::RefCell;

    /// `MockFetch` routes plus a record of every URL whose body was cached.
    pub(crate) struct MockMetadataFetch {
        pub fetch: MockFetch,
        pub stored: RefCell<Vec<String>>,
    }

    impl MockMetadataFetch {
        pub fn new(routes: Vec<(&str, Result<String, String>)>) -> Self {
            MockMetadataFetch { fetch: MockFetch::new(routes), stored: RefCell::new(Vec::new()) }
        }
    }

    impl MetadataFetch for MockMetadataFetch {
        async fn load(&self, _key: &str, url: &str) -> Result<String, String> {
            self.fetch.get(url).await
        }
        fn store(&self, _key: &str, url: &str, _body: &str) {
            self.stored.borrow_mut().push(url.to_string());
        }
    }

    const ARXIV_URL: &str = "export.arxiv.org/api/query?id_list=2609.39243";
    const S2_URL: &str = "api.semanticscholar.org/graph/v1/paper/arXiv:2609.39243";
    const OA_URL: &str = "api.openalex.org/works/doi:10.48550/arXiv.2609.39243";

    fn atom() -> String {
        r#"<feed xmlns="http://www.w3.org/2005/Atom"><entry>
            <id>http://arxiv.org/abs/2609.39243v1</id>
            <title>From arXiv</title><summary>A.</summary>
            <published>2026-09-30T00:00:00Z</published>
            <author><name>Jane Doe</name></author>
        </entry></feed>"#
            .to_string()
    }

    fn s2() -> String {
        r#"{"paperId": "p", "title": "From S2", "authors": [{"name": "Jane Doe"}],
            "year": 2026, "venue": "NeurIPS", "abstract": "Abs."}"#
            .to_string()
    }

    fn oa() -> String {
        r#"{"title": "From OpenAlex", "publication_year": 2026,
            "authorships": [{"author": {"display_name": "Jane Doe"}}]}"#
            .to_string()
    }

    fn rate_limited() -> Result<String, String> {
        Err("HTTP 429 Too Many Requests after 5 attempts".to_string())
    }

    #[tokio::test]
    async fn arxiv_success_queries_no_fallback() {
        let f = MockMetadataFetch::new(vec![(ARXIV_URL, Ok(atom())), (S2_URL, Ok(s2())), (OA_URL, Ok(oa()))]);
        let m = arxiv_metadata(&f, "2609.39243").await.unwrap();
        assert_eq!(m.source, MetadataSource::Arxiv);
        assert_eq!(m.paper.title, "From arXiv");
        assert_eq!(f.fetch.calls.borrow().len(), 1, "calls: {:?}", f.fetch.calls.borrow());
    }

    #[tokio::test]
    async fn arxiv_failure_falls_back_to_semantic_scholar() {
        let f = MockMetadataFetch::new(vec![(ARXIV_URL, rate_limited()), (S2_URL, Ok(s2())), (OA_URL, Ok(oa()))]);
        let m = arxiv_metadata(&f, "2609.39243").await.unwrap();
        assert_eq!(m.source, MetadataSource::SemanticScholar);
        assert_eq!(m.paper.title, "From S2");
        assert_eq!(m.paper.authors, vec!["Jane Doe"]);
        assert_eq!(m.paper.year, "2026");
        assert_eq!(m.paper.venue.as_deref(), Some("NeurIPS"));
        assert_eq!(m.paper.abstract_text.as_deref(), Some("Abs."));
        assert!(!f.fetch.calls.borrow().iter().any(|u| u.contains("openalex")));
    }

    #[tokio::test]
    async fn unparseable_arxiv_answer_also_falls_back() {
        // A 200 with no <entry> (arXiv's reply for an unknown or throttled id).
        let empty = r#"<feed xmlns="http://www.w3.org/2005/Atom"></feed>"#.to_string();
        let f = MockMetadataFetch::new(vec![(ARXIV_URL, Ok(empty)), (S2_URL, Ok(s2()))]);
        assert_eq!(arxiv_metadata(&f, "2609.39243").await.unwrap().source, MetadataSource::SemanticScholar);
    }

    #[tokio::test]
    async fn only_the_body_that_supplied_the_record_is_cached() {
        // A throttled arXiv reply (200, empty feed) cached as the paper's record
        // would skip arXiv for the cache lifetime; only the S2 answer may be stored.
        let empty = r#"<feed xmlns="http://www.w3.org/2005/Atom"></feed>"#.to_string();
        let f = MockMetadataFetch::new(vec![(ARXIV_URL, Ok(empty)), (S2_URL, Ok(s2()))]);
        arxiv_metadata(&f, "2609.39243").await.unwrap();
        let stored = f.stored.borrow();
        assert_eq!(stored.len(), 1, "stored: {:?}", stored);
        assert!(stored[0].contains(S2_URL), "stored: {:?}", stored);
    }

    #[tokio::test]
    async fn openalex_is_the_last_fallback() {
        let f = MockMetadataFetch::new(vec![(ARXIV_URL, rate_limited()), (S2_URL, rate_limited()), (OA_URL, Ok(oa()))]);
        let m = arxiv_metadata(&f, "2609.39243").await.unwrap();
        assert_eq!(m.source, MetadataSource::OpenAlex);
        assert_eq!(m.paper.title, "From OpenAlex");
    }

    #[tokio::test]
    async fn every_source_failing_names_each_failure() {
        let f = MockMetadataFetch::new(vec![(ARXIV_URL, rate_limited()), (S2_URL, rate_limited())]);
        let err = arxiv_metadata(&f, "2609.39243").await.unwrap_err();
        for name in ["arXiv API", "Semantic Scholar", "OpenAlex"] {
            assert!(err.contains(name), "err: {}", err);
        }
        assert!(f.stored.borrow().is_empty());
    }

    #[tokio::test]
    async fn malformed_answers_from_every_source_are_failures_not_records() {
        // Valid JSON without a title must not become a record titled "N/A" or "".
        let f = MockMetadataFetch::new(vec![
            (ARXIV_URL, Ok("not xml at all".to_string())),
            (S2_URL, Ok(r#"{"paperId": "p"}"#.to_string())),
            (OA_URL, Ok(r#"{"publication_year": 2026}"#.to_string())),
        ]);
        let err = arxiv_metadata(&f, "2609.39243").await.unwrap_err();
        assert!(err.starts_with("no metadata for arXiv:2609.39243"), "err: {}", err);
        assert!(f.stored.borrow().is_empty());
    }

    #[test]
    fn source_ids_are_the_stable_strings_recorded_in_source_yaml() {
        assert_eq!(MetadataSource::Arxiv.id(), "arxiv");
        assert_eq!(MetadataSource::SemanticScholar.id(), "semantic_scholar");
        assert_eq!(MetadataSource::OpenAlex.id(), "openalex");
        assert_eq!(MetadataSource::SemanticScholar.name(), "Semantic Scholar");
    }
}
