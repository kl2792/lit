//! `lit read` for open-access proceedings (ADR-010): a proceedings URL is
//! fetched through the host rule table, and a title or Semantic Scholar id
//! that matches no local artifact falls back to an open-access copy.
//!
//! The artifact `etc/pdf/<citekey>/` (`paper.pdf`, `paper.txt`,
//! `source.yaml`) is the cache: a URL whose canonical PDF or landing URL is
//! recorded in an artifact is answered from disk with no request.

use std::path::{Path, PathBuf};

use super::artifact::{holds_content, owner_dir, paper_id, Target};
use super::metadata::MetadataFetch;
use super::read::ReadResult;
use super::Context;
use crate::api::proceedings::{self, Landing, Resolved};
use crate::api::{openalex, semantic_scholar as s2, PaperResult};

/// Fetches a PDF's bytes; the bytes must start with `%PDF`.
pub(crate) trait PdfFetch {
    async fn pdf(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// The curl fetch every lit PDF download uses.
pub(crate) struct Curl;

impl PdfFetch for Curl {
    async fn pdf(&self, url: &str) -> Result<Vec<u8>, String> {
        super::download::fetch_pdf_via_curl(url, None).await.map_err(|e| e.to_string())
    }
}

/// A paper's metadata and the service that supplied it, recorded as
/// `metadata_source`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Meta {
    pub title: String,
    pub authors: Vec<String>,
    pub year: String,
    pub doi: Option<String>,
    pub source: &'static str,
}

impl Meta {
    fn from_landing(landing: Landing) -> Meta {
        Meta { title: landing.title, authors: landing.authors, year: landing.year, doi: landing.doi, source: "landing-page" }
    }

    fn from_s2(paper: &PaperResult) -> Meta {
        Meta {
            title: paper.title.clone(),
            authors: paper.authors.clone(),
            year: paper.year.clone(),
            doi: paper.doi.clone(),
            source: "semantic-scholar",
        }
    }
}

/// A PDF to store: its URL, the landing page it was found through, the host
/// rule id (or `open-access` for a URL no rule accepts), and its metadata
/// when a source supplied them.
struct Work {
    pdf: String,
    landing: Option<String>,
    host: &'static str,
    meta: Option<Meta>,
}

/// True when `lit read` should hand `id` to this module: an http(s) URL that
/// a proceedings rule accepts.
pub fn handles(id: &str) -> bool {
    proceedings::resolve(id).is_some()
}

/// `lit read <URL>` for a proceedings URL: the text of its artifact, fetched
/// first when none exists or `ctx.no_cache` is set. A URL that names a DOI
/// is read as that DOI.
pub async fn read(ctx: &Context, input: &str) -> Result<ReadResult, Box<dyn std::error::Error>> {
    if let Some(Resolved::Doi(doi)) = proceedings::resolve(input) {
        return Ok(super::read::read_or_fetch(ctx, &doi).await?.0);
    }
    let root = super::read::find_pdf_base()?;
    let dir = ensure_url(&ctx.client(), &Curl, &root, input, ctx.no_cache).await?;
    super::read::ensure_text(&dir)
}

/// The artifact under `root` for the paper at proceedings URL `input`,
/// fetched and stored when none exists or `refresh` is set.
///
/// Cost: one `source.yaml` read per artifact under `root`, and a second pass
/// when the landing page names a PDF URL the input did not; with no
/// artifact, one landing-page request when the rule yields a landing URL,
/// and one PDF request. On any failure nothing is written.
pub(crate) async fn ensure_url<F: MetadataFetch, P: PdfFetch>(
    f: &F,
    p: &P,
    root: &Path,
    input: &str,
    refresh: bool,
) -> Result<PathBuf, String> {
    let Some(Resolved::Paper { rule, pdf: computed, landing }) = proceedings::resolve(input) else {
        return Err(format!("{} names no paper on a proceedings host", input));
    };
    let known: Vec<String> = computed.iter().chain(landing.iter()).cloned().collect();
    let mut existing = find_artifact(root, &known)?;
    if !refresh && let Some(dir) = existing {
        return Ok(dir);
    }

    let (meta, page_pdf, links, landing_failure) = match &landing {
        Some(page) => match fetch_landing(f, page).await {
            Ok(l) => (Some(Meta::from_landing(l.clone())), l.pdf_url.clone(), l.pdf_links, None),
            Err((why, links)) => (None, None, links, Some(why)),
        },
        None => (None, None, Vec::new(), None),
    };
    let pdf = page_pdf
        .and_then(|u| canonical_pdf(rule, &u))
        .or(computed)
        .or_else(|| links.iter().find_map(|u| canonical_pdf(rule, u)));
    let Some(pdf) = pdf else {
        let why = landing_failure.unwrap_or_else(|| "it links no PDF this host serves".to_string());
        return Err(format!("no PDF URL for {}: {}", input, why));
    };

    if existing.is_none() {
        existing = find_artifact(root, std::slice::from_ref(&pdf))?;
        if !refresh && let Some(dir) = existing {
            return Ok(dir);
        }
    }
    // A paper already downloaded through its DOI is read as it stands; a
    // refetch never replaces that artifact, which may hold more than a PDF.
    if !refresh
        && existing.is_none()
        && let Some(doi) = meta.as_ref().and_then(|m| m.doi.as_deref()).and_then(paper_id)
        && let Some(dir) = owner_dir(root, &doi).map_err(|e| e.to_string())?.filter(|d| holds_content(d))
    {
        return Ok(dir);
    }

    let bytes = p.pdf(&pdf).await.map_err(|e| match &landing_failure {
        Some(why) => format!("{}; the landing page gave no metadata: {}", e, why),
        None => e,
    })?;
    store(root, &Work { pdf, landing, host: rule, meta }, &bytes, existing)
}

/// The artifact under `root` for an open-access PDF that no rule accepts,
/// with metadata from `meta`'s source.
async fn ensure_pdf<P: PdfFetch>(p: &P, root: &Path, url: &str, meta: Meta, refresh: bool) -> Result<PathBuf, String> {
    // The URL comes from a metadata service; curl must not read a local file.
    if !crate::api::web::is_http_url(url) {
        return Err(format!("{} is not an http(s) URL", url));
    }
    let existing = find_artifact(root, &[url.to_string()])?;
    if !refresh && let Some(dir) = existing {
        return Ok(dir);
    }
    let bytes = p.pdf(url).await?;
    store(root, &Work { pdf: url.to_string(), landing: None, host: "open-access", meta: Some(meta) }, &bytes, existing)
}

/// The canonical PDF URL of `url` when the rule `rule` accepts it and
/// yields one.
fn canonical_pdf(rule: &str, url: &str) -> Option<String> {
    match proceedings::resolve(url)? {
        Resolved::Paper { rule: r, pdf, .. } if r == rule => pdf,
        _ => None,
    }
}

/// Fetch and parse a landing page, caching it only when it parses.
async fn fetch_landing<F: MetadataFetch>(f: &F, page: &str) -> Result<Landing, (String, Vec<String>)> {
    let key = crate::db::Db::cache_key("proceedings_landing", page);
    let body = f.load(&key, page).await.map_err(|e| (format!("fetching {}: {}", page, e), Vec::new()))?;
    let landing = proceedings::parse_landing(page, &body).map_err(|(why, links)| (format!("{}: {}", page, why), links))?;
    f.store(&key, page, &body);
    Ok(landing)
}

/// The artifact directory whose `source.yaml` records one of `urls` as its
/// `url` or `landing_url`. Staging directories (dot-prefixed) are skipped.
fn find_artifact(root: &Path, urls: &[String]) -> Result<Option<PathBuf>, String> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("reading {}: {}", root.display(), e)),
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .filter(|d| d.is_dir())
        .collect();
    dirs.sort();
    Ok(dirs.into_iter().find(|dir| {
        std::fs::read_to_string(dir.join("source.yaml")).is_ok_and(|yaml| {
            ["url", "landing_url"]
                .iter()
                .filter_map(|k| super::web::yaml_value(&yaml, k))
                .any(|v| urls.contains(&v))
        })
    }))
}

/// Write `bytes` as `paper.pdf` with its text and `source.yaml` into
/// `existing`, or into a newly claimed directory named by the citekey
/// (`<rule>-<file stem>` without metadata).
fn store(root: &Path, work: &Work, bytes: &[u8], existing: Option<PathBuf>) -> Result<PathBuf, String> {
    let claimed = existing.is_none();
    let dir = match existing {
        Some(dir) => dir,
        None => {
            let slug = match &work.meta {
                Some(m) => crate::citekey::generate(&m.authors, &m.year, &m.title),
                None => format!("{}-{}", work.host, file_stem(&work.pdf)),
            };
            super::web::claim_first(root, &slug).map_err(|e| format!("creating a directory for {} under {}: {}", slug, root.display(), e))?
        }
    };
    let citekey = dir.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let yaml = source_yaml(work, &citekey, &super::utc_timestamp());
    super::web::publish_claimed(&dir, claimed, |staging| {
        super::download::write_pdf(staging, bytes)?;
        std::fs::write(staging.join("source.yaml"), &yaml)
    })
    .map_err(|e| format!("writing {}: {}", dir.display(), e))?;
    crate::format::info(&format!("stored {} in {}", work.pdf, dir.display()));
    Ok(dir)
}

/// The last path segment of `url` without `.pdf`, with every character
/// other than ASCII letters, digits, `_` and `-` replaced by `-`.
fn file_stem(url: &str) -> String {
    let last = url.trim_end_matches('/').rsplit('/').next().unwrap_or(url);
    let last = last.strip_suffix(".pdf").unwrap_or(last);
    last.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' }).collect()
}

/// `source.yaml` for a proceedings or open-access PDF artifact.
fn source_yaml(work: &Work, citekey: &str, fetched: &str) -> String {
    let none = Meta { title: String::new(), authors: Vec::new(), year: String::new(), doi: None, source: "" };
    let meta = work.meta.as_ref().unwrap_or(&none);
    let params = super::misc::MiscParams {
        citekey: citekey.to_string(),
        title: meta.title.clone(),
        authors: meta.authors.clone(),
        year: meta.year.clone(),
        howpublished: None,
        note: None,
        url: Some(work.pdf.clone()),
    };
    let quoted = |v: &str| format!("\"{}\"", v.replace('"', "\\\""));
    let mut yaml = super::misc::build_misc_source_yaml(&params, fetched);
    if let Some(landing) = &work.landing {
        yaml.push_str(&format!("landing_url: {}\n", quoted(landing)));
    }
    yaml.push_str(&format!("host: {}\n", quoted(work.host)));
    yaml.push_str(&format!("source_url: {}\n", quoted(&work.pdf)));
    if let Some(doi) = &meta.doi {
        yaml.push_str(&format!("doi: {}\n", quoted(doi)));
    }
    if work.meta.is_some() {
        yaml.push_str(&format!("metadata_source: {}\n", quoted(meta.source)));
        yaml.push_str("metadata_confirmed: true\n");
    }
    yaml
}

/// The ways `lit read` can obtain a paper found by the open-access fallback.
pub(crate) trait Acquire {
    /// Fetch by arXiv id or DOI, as `lit read <id>` does.
    async fn paper(&self, id: &Target) -> Result<PathBuf, String>;
    /// Fetch a URL a proceedings rule accepts.
    async fn venue(&self, url: &str) -> Result<PathBuf, String>;
    /// Fetch an open-access PDF that no rule accepts, with `meta`.
    async fn pdf(&self, url: &str, meta: Meta) -> Result<PathBuf, String>;
}

/// The network implementation of `Acquire`, storing under `root`.
pub(crate) struct Live<'a> {
    pub ctx: &'a Context,
    pub root: PathBuf,
}

impl Acquire for Live<'_> {
    async fn paper(&self, id: &Target) -> Result<PathBuf, String> {
        super::download::fetch(self.ctx, id).await.map_err(|e| e.to_string())
    }

    async fn venue(&self, url: &str) -> Result<PathBuf, String> {
        ensure_url(&self.ctx.client(), &Curl, &self.root, url, self.ctx.no_cache).await
    }

    async fn pdf(&self, url: &str, meta: Meta) -> Result<PathBuf, String> {
        ensure_pdf(&Curl, &self.root, url, meta, self.ctx.no_cache).await
    }
}

/// What a query that matches no local artifact can be looked up as.
#[derive(Debug, PartialEq)]
pub(crate) enum Lookup {
    /// A Semantic Scholar paper id: 40 hex digits or `CorpusId:<n>`.
    S2Id(String),
    /// A title: any query containing whitespace. A one-word query is taken
    /// for a mistyped local name, so it fails with no request.
    Title(String),
}

/// How `query` is looked up remotely, or `None` when it is not.
pub(crate) fn lookup_of(query: &str) -> Option<Lookup> {
    let q = query.trim();
    let corpus = q.get(..9).is_some_and(|p| p.eq_ignore_ascii_case("corpusid:")) && q.len() > 9 && q[9..].chars().all(|c| c.is_ascii_digit());
    let sha = q.len() == 40 && q.chars().all(|c| c.is_ascii_hexdigit());
    if corpus || sha {
        Some(Lookup::S2Id(q.to_string()))
    } else if q.contains(char::is_whitespace) {
        Some(Lookup::Title(q.to_string()))
    } else {
        None
    }
}

/// Find and store an open-access copy of the paper `lookup` names, trying
/// Semantic Scholar, then OpenAlex (ADR-010 decision 6). Returns the
/// artifact directory, or one line per source tried with its outcome.
pub(crate) async fn open_access<F: MetadataFetch, A: Acquire>(f: &F, a: &A, lookup: &Lookup) -> Result<PathBuf, Vec<String>> {
    let mut tried = Vec::new();
    let mut attempted: Vec<String> = Vec::new();

    let record = match semantic_scholar(f, lookup).await {
        Ok(r) => Some(r),
        Err(e) => {
            tried.push(format!("Semantic Scholar: {}", e));
            None
        }
    };
    if let Some(r) = &record {
        let target = r.arxiv_id.as_deref().or(r.doi.as_deref()).and_then(paper_id);
        let pdf = r.pdf_url.clone().filter(|u| !u.is_empty());
        if let Some(target) = &target {
            match a.paper(target).await {
                Ok(dir) => return Ok(dir),
                Err(e) => tried.push(format!("Semantic Scholar id {}: {}", target.as_str(), e)),
            }
        }
        if let Some(url) = &pdf {
            attempted.push(url.clone());
            let result = match proceedings::resolve(url) {
                Some(_) => a.venue(url).await,
                None => a.pdf(url, Meta::from_s2(r)).await,
            };
            match result {
                Ok(dir) => return Ok(dir),
                Err(e) => tried.push(format!("Semantic Scholar open-access PDF {}: {}", url, e)),
            }
        }
        if target.is_none() && pdf.is_none() {
            tried.push(format!("Semantic Scholar: '{}' lists no arXiv id, DOI or open-access PDF", r.title));
        }
    }

    let title = match lookup {
        Lookup::Title(t) => Some(t.clone()),
        Lookup::S2Id(_) => record.map(|r| r.title),
    };
    let Some(title) = title else {
        tried.push("OpenAlex: skipped, since no title is known to search for".to_string());
        return Err(tried);
    };
    let urls = match openalex_urls(f, &title).await {
        Ok(urls) => urls,
        Err(e) => {
            tried.push(format!("OpenAlex: {}", e));
            return Err(tried);
        }
    };
    let venues: Vec<(String, Resolved)> = urls
        .into_iter()
        .filter(|u| !attempted.contains(u))
        .filter_map(|u| proceedings::resolve(&u).map(|r| (u, r)))
        .collect();
    if venues.is_empty() {
        tried.push(format!("OpenAlex: '{}' lists no URL on a proceedings host", title));
    }
    for (url, resolved) in venues {
        let result = match resolved {
            Resolved::Doi(doi) => a.paper(&paper_id(&doi).unwrap_or(Target::Doi(doi))).await,
            Resolved::Paper { .. } => a.venue(&url).await,
        };
        match result {
            Ok(dir) => return Ok(dir),
            Err(e) => tried.push(format!("OpenAlex venue URL {}: {}", url, e)),
        }
    }
    Err(tried)
}

/// The Semantic Scholar record for `lookup`; a title lookup is accepted only
/// when the matched title equals it under `title_key`.
async fn semantic_scholar<F: MetadataFetch>(f: &F, lookup: &Lookup) -> Result<PaperResult, String> {
    let (url, parse): (String, fn(&str) -> Result<PaperResult, Box<dyn std::error::Error>>) = match lookup {
        Lookup::S2Id(id) => (s2::paper_url(id), s2::parse_paper),
        Lookup::Title(t) => (s2::match_url(t), s2::parse_match),
    };
    let key = crate::db::Db::cache_key("s2_open_access", &url);
    let body = f.load(&key, &url).await?;
    let record = parse(&body).map_err(|e| e.to_string())?;
    if record.title.is_empty() {
        return Err("no paper found".to_string());
    }
    if let Lookup::Title(t) = lookup
        && title_key(t) != title_key(&record.title)
    {
        return Err(format!("the closest title match is '{}', a different title", record.title));
    }
    f.store(&key, &url, &body);
    Ok(record)
}

/// The location URLs of the OpenAlex work titled `title`.
async fn openalex_urls<F: MetadataFetch>(f: &F, title: &str) -> Result<Vec<String>, String> {
    // A comma separates alternatives in an OpenAlex filter value.
    let url = openalex::title_search_url(&title.replace(',', " "), 5);
    let key = crate::db::Db::cache_key("oa_open_access", &url);
    let body = f.load(&key, &url).await?;
    let works = openalex::parse_location_urls(&body).map_err(|e| e.to_string())?;
    let (_, urls) = works
        .into_iter()
        .find(|(t, _)| title_key(t) == title_key(title))
        .ok_or_else(|| format!("no work titled '{}'", title))?;
    f.store(&key, &url, &body);
    Ok(urls)
}

/// A title compared without case or punctuation: lowercase alphanumeric
/// words joined by single spaces.
fn title_key(title: &str) -> String {
    title
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::metadata::tests::MockMetadataFetch;
    use std::cell::RefCell;

    const NEURIPS: &str = include_str!("../../test/proceedings/neurips-2013-adaptive-dropout.html");
    const ECVA: &str = include_str!("../../test/proceedings/ecva-2020-267.html");
    const OPENALEX: &str = include_str!("../../test/proceedings/openalex-adaptive-dropout.json");

    const HASH: &str = "7b5b23f4aadf9513306bcd59afb6e4c9";
    const TITLE: &str = "Adaptive dropout for training deep neural networks";

    fn neurips_pdf() -> String {
        format!("https://proceedings.neurips.cc/paper_files/paper/2013/file/{}-Paper.pdf", HASH)
    }

    /// Serves `%PDF` bytes for URLs containing one of `ok`, and records calls.
    struct MockPdf {
        ok: Vec<String>,
        calls: RefCell<Vec<String>>,
    }

    impl MockPdf {
        fn serving(ok: &[&str]) -> MockPdf {
            MockPdf { ok: ok.iter().map(|s| s.to_string()).collect(), calls: RefCell::new(Vec::new()) }
        }
    }

    impl PdfFetch for MockPdf {
        async fn pdf(&self, url: &str) -> Result<Vec<u8>, String> {
            self.calls.borrow_mut().push(url.to_string());
            if self.ok.iter().any(|ok| url.contains(ok.as_str())) {
                Ok(b"%PDF-1.4 test".to_vec())
            } else {
                Err(format!("{} answered with 7038 bytes that are not a PDF", url))
            }
        }
    }

    fn landing_mock() -> MockMetadataFetch {
        MockMetadataFetch::new(vec![
            (&format!("/paper_files/paper/2013/hash/{}-Abstract.html", HASH), Ok(NEURIPS.to_string())),
            ("ecva.net/papers/eccv_2020/papers_ECCV/html/", Ok(ECVA.to_string())),
        ])
    }

    fn yaml(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("source.yaml")).unwrap()
    }

    #[tokio::test]
    async fn the_neurips_2013_pdf_url_is_stored_with_landing_page_metadata() {
        let root = tempfile::tempdir().unwrap();
        let (f, p) = (landing_mock(), MockPdf::serving(&["proceedings.neurips.cc"]));
        let input = format!("https://proceedings.neurips.cc/paper/2013/file/{}-Paper.pdf", HASH);
        let dir = ensure_url(&f, &p, root.path(), &input, false).await.unwrap();
        assert_eq!(dir, root.path().join("ba2013adaptive"));
        assert_eq!(std::fs::read(dir.join("paper.pdf")).unwrap(), b"%PDF-1.4 test");
        assert_eq!(*p.calls.borrow(), vec![neurips_pdf()]);
        let yaml = yaml(&dir);
        for line in [
            "bibtex_key: \"ba2013adaptive\"".to_string(),
            format!("title: \"{}\"", TITLE),
            "authors: \"Jimmy Ba and Brendan Frey\"".to_string(),
            "year: 2013".to_string(),
            format!("url: \"{}\"", neurips_pdf()),
            format!("landing_url: \"https://proceedings.neurips.cc/paper_files/paper/2013/hash/{}-Abstract.html\"", HASH),
            "host: \"neurips\"".to_string(),
            "metadata_source: \"landing-page\"".to_string(),
            "metadata_confirmed: true".to_string(),
        ] {
            assert!(yaml.contains(&line), "missing {:?} in:\n{}", line, yaml);
        }
    }

    #[tokio::test]
    async fn any_spelling_of_a_stored_paper_is_read_with_no_request() {
        let root = tempfile::tempdir().unwrap();
        let (f, p) = (landing_mock(), MockPdf::serving(&["proceedings.neurips.cc"]));
        let first = ensure_url(&f, &p, root.path(), &neurips_pdf(), false).await.unwrap();
        let abstract_url = format!("https://papers.nips.cc/paper/2013/hash/{}-Abstract.html", HASH);
        let again = ensure_url(&f, &p, root.path(), &abstract_url, false).await.unwrap();
        assert_eq!(again, first);
        assert_eq!((f.fetch.calls.borrow().len(), p.calls.borrow().len()), (1, 1));
        let refetched = ensure_url(&f, &p, root.path(), &abstract_url, true).await.unwrap();
        assert_eq!(refetched, first);
        assert_eq!(p.calls.borrow().len(), 2);
    }

    #[tokio::test]
    async fn a_pdf_without_a_landing_page_is_stored_without_metadata() {
        let root = tempfile::tempdir().unwrap();
        let (f, p) = (landing_mock(), MockPdf::serving(&["ijcai.org"]));
        let dir = ensure_url(&f, &p, root.path(), "https://www.ijcai.org/proceedings/2019/0123.pdf", false).await.unwrap();
        assert_eq!(dir, root.path().join("ijcai-0123"));
        let yaml = yaml(&dir);
        assert!(yaml.contains("title: \"\"") && !yaml.contains("metadata_confirmed"), "{}", yaml);
        assert_eq!(f.fetch.calls.borrow().len(), 0);
    }

    #[tokio::test]
    async fn a_landing_page_without_tags_still_yields_its_linked_pdf() {
        let root = tempfile::tempdir().unwrap();
        let (f, p) = (landing_mock(), MockPdf::serving(&["ecva.net"]));
        let page = "https://www.ecva.net/papers/eccv_2020/papers_ECCV/html/267_ECCV_2020_paper.php";
        let dir = ensure_url(&f, &p, root.path(), page, false).await.unwrap();
        assert_eq!(*p.calls.borrow(), vec!["https://www.ecva.net/papers/eccv_2020/papers_ECCV/papers/123460001.pdf"]);
        assert_eq!(dir, root.path().join("ecva-123460001"));
        assert!(yaml(&dir).contains(&format!("landing_url: \"{}\"", page)));
    }

    #[tokio::test]
    async fn a_bot_challenge_writes_nothing_and_names_the_cause() {
        let root = tempfile::tempdir().unwrap();
        let f = MockMetadataFetch::new(vec![("openreview.net", Ok("<html><title>Challenge</title></html>".to_string()))]);
        let p = MockPdf::serving(&[]);
        let err = ensure_url(&f, &p, root.path(), "https://openreview.net/forum?id=YicbFdNTTy", false).await.unwrap_err();
        assert!(err.contains("not a PDF") && err.contains("citation_title"), "{}", err);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn a_paper_already_downloaded_by_its_doi_is_read_without_a_pdf_request() {
        let root = tempfile::tempdir().unwrap();
        let doi_dir = root.path().join("devlin2019bert");
        std::fs::create_dir(&doi_dir).unwrap();
        std::fs::write(doi_dir.join("source.yaml"), "doi: \"10.18653/v1/N19-1423\"\n").unwrap();
        std::fs::write(doi_dir.join("paper.pdf"), "%PDF earlier").unwrap();
        let acl = include_str!("../../test/proceedings/acl-N19-1423.html");
        let f = MockMetadataFetch::new(vec![("aclanthology.org/N19-1423/", Ok(acl.to_string()))]);
        let p = MockPdf::serving(&["aclanthology.org"]);
        let dir = ensure_url(&f, &p, root.path(), "https://aclanthology.org/N19-1423.pdf", false).await.unwrap();
        assert_eq!(dir, doi_dir);
        assert!(p.calls.borrow().is_empty());
    }

    #[tokio::test]
    async fn an_open_access_pdf_url_must_be_http() {
        let root = tempfile::tempdir().unwrap();
        let p = MockPdf::serving(&["etc"]);
        let meta = Meta { title: TITLE.into(), authors: vec![], year: "2013".into(), doi: None, source: "semantic-scholar" };
        let err = ensure_pdf(&p, root.path(), "file:///etc/passwd", meta, false).await.unwrap_err();
        assert!(err.contains("not an http(s) URL"), "{}", err);
        assert!(p.calls.borrow().is_empty());
    }

    #[test]
    fn lookup_takes_s2_ids_and_titles_but_not_single_words() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(lookup_of(sha), Some(Lookup::S2Id(sha.into())));
        assert_eq!(lookup_of("CorpusId:12345"), Some(Lookup::S2Id("CorpusId:12345".into())));
        assert_eq!(lookup_of(TITLE), Some(Lookup::Title(TITLE.into())));
        for q in ["nosuchpaper", "ba2013adaptive", "CorpusId:", "CorpusId:x1"] {
            assert_eq!(lookup_of(q), None, "{}", q);
        }
    }

    #[test]
    fn title_key_ignores_case_and_punctuation() {
        assert_eq!(title_key("Adaptive dropout for training deep neural networks."), title_key("ADAPTIVE  Dropout for Training Deep-Neural Networks"));
        assert_ne!(title_key("Adaptive dropout"), title_key("Adaptive dropouts"));
    }

    /// Records each acquisition attempt and succeeds on the listed ones.
    struct MockAcquire {
        succeed: Vec<String>,
        calls: RefCell<Vec<String>>,
    }

    impl MockAcquire {
        fn succeeding(on: &[&str]) -> MockAcquire {
            MockAcquire { succeed: on.iter().map(|s| s.to_string()).collect(), calls: RefCell::new(Vec::new()) }
        }

        fn attempt(&self, call: String) -> Result<PathBuf, String> {
            self.calls.borrow_mut().push(call.clone());
            if self.succeed.contains(&call) {
                Ok(PathBuf::from("stored"))
            } else {
                Err("HTTP 403".to_string())
            }
        }
    }

    impl Acquire for MockAcquire {
        async fn paper(&self, id: &Target) -> Result<PathBuf, String> {
            self.attempt(format!("id {}", id.as_str()))
        }
        async fn venue(&self, url: &str) -> Result<PathBuf, String> {
            self.attempt(format!("venue {}", url))
        }
        async fn pdf(&self, url: &str, meta: Meta) -> Result<PathBuf, String> {
            self.attempt(format!("pdf {} by {}", url, meta.authors.join(", ")))
        }
    }

    /// A Semantic Scholar title match for Ba and Frey with the given
    /// `externalIds` and `openAccessPdf` values.
    fn s2_match(external_ids: &str, open_access_pdf: &str) -> String {
        format!(
            r#"{{"data": [{{"paperId": "abc", "title": "{}", "authors": [{{"name": "Jimmy Ba"}}, {{"name": "Brendan Frey"}}], "year": 2013, "externalIds": {}, "openAccessPdf": {}}}]}}"#,
            TITLE, external_ids, open_access_pdf
        )
    }

    const OA_VENUE: &str = "venue https://papers.nips.cc/paper/2013/file/7b5b23f4aadf9513306bcd59afb6e4c9-Paper.pdf";

    #[tokio::test]
    async fn a_title_with_no_ids_or_pdf_on_s2_falls_back_to_the_openalex_venue_url() {
        let f = MockMetadataFetch::new(vec![
            ("paper/search/match", Ok(s2_match(r#"{"DBLP": "conf/nips/BaF13"}"#, r#"{"url": "", "status": "CLOSED"}"#))),
            ("api.openalex.org", Ok(OPENALEX.to_string())),
        ]);
        let a = MockAcquire::succeeding(&[OA_VENUE]);
        let dir = open_access(&f, &a, &Lookup::Title(TITLE.into())).await.unwrap();
        assert_eq!(dir, PathBuf::from("stored"));
        assert_eq!(*a.calls.borrow(), vec![OA_VENUE.to_string()]);
        let urls = f.fetch.calls.borrow();
        assert!(urls[0].contains("semanticscholar.org") && urls[1].contains("openalex.org"), "{:?}", urls);
    }

    #[tokio::test]
    async fn s2_ids_then_its_open_access_pdf_are_tried_before_openalex() {
        let f = MockMetadataFetch::new(vec![
            ("paper/search/match", Ok(s2_match(r#"{"DOI": "10.5555/2999792"}"#, r#"{"url": "https://example.org/ba.pdf"}"#))),
            ("api.openalex.org", Ok(OPENALEX.to_string())),
        ]);
        let a = MockAcquire::succeeding(&[]);
        let tried = open_access(&f, &a, &Lookup::Title(TITLE.into())).await.unwrap_err();
        assert_eq!(
            *a.calls.borrow(),
            vec!["id 10.5555/2999792".to_string(), "pdf https://example.org/ba.pdf by Jimmy Ba, Brendan Frey".to_string(), OA_VENUE.to_string()]
        );
        assert_eq!(tried.len(), 3, "{:?}", tried);
        assert!(tried[0].starts_with("Semantic Scholar id 10.5555/2999792: HTTP 403"), "{:?}", tried);
        assert!(tried[1].starts_with("Semantic Scholar open-access PDF https://example.org/ba.pdf"), "{:?}", tried);
        assert!(tried[2].starts_with("OpenAlex venue URL https://papers.nips.cc/"), "{:?}", tried);
    }

    #[tokio::test]
    async fn a_different_title_match_is_rejected_and_every_source_is_named() {
        let f = MockMetadataFetch::new(vec![
            ("paper/search/match", Ok(r#"{"data": [{"paperId": "x", "title": "Adaptive dropout for something else"}]}"#.to_string())),
            ("api.openalex.org", Ok(r#"{"results": []}"#.to_string())),
        ]);
        let a = MockAcquire::succeeding(&[]);
        let tried = open_access(&f, &a, &Lookup::Title(TITLE.into())).await.unwrap_err();
        assert!(a.calls.borrow().is_empty());
        assert_eq!(tried.len(), 2, "{:?}", tried);
        assert!(tried[0].starts_with("Semantic Scholar: the closest title match is"), "{:?}", tried);
        assert!(tried[1].starts_with("OpenAlex: no work titled"), "{:?}", tried);
    }

    #[tokio::test]
    async fn an_s2_id_lookup_searches_openalex_by_the_s2_title() {
        let id = "0123456789abcdef0123456789abcdef01234567";
        let f = MockMetadataFetch::new(vec![
            (&format!("graph/v1/paper/{}", id), Ok(format!(r#"{{"paperId": "{}", "title": "{}"}}"#, id, TITLE))),
            ("api.openalex.org", Ok(OPENALEX.to_string())),
        ]);
        let a = MockAcquire::succeeding(&[OA_VENUE]);
        assert!(open_access(&f, &a, &Lookup::S2Id(id.into())).await.is_ok());
        assert_eq!(*a.calls.borrow(), vec![OA_VENUE.to_string()]);
    }
}
