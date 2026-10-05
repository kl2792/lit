//! `lit read <URL>` and `lit add <URL> <bib>` for web-published papers on the
//! ADR-005 host allowlist.
//!
//! The artifact `etc/pdf/<citekey>/` (`paper.txt` plus `source.yaml`) is the
//! cache: a URL whose work already has an artifact is answered from disk with
//! no request. Page bodies never enter the HTTP cache; a Distill page can be
//! 15 MB, and the extracted text is what later calls need.

use std::path::{Path, PathBuf};

use super::metadata::MetadataFetch;
use super::misc::MiscParams;
use super::read::ReadResult;
use crate::api::web::{self, WebPage, WebUrl};

/// A stored web artifact and the metadata its `source.yaml` records.
#[derive(Debug, Clone, PartialEq)]
pub struct WebArtifact {
    pub dir: PathBuf,
    pub citekey: String,
    pub title: String,
    pub authors: Vec<String>,
    pub year: String,
    pub doi: Option<String>,
    pub url: WebUrl,
}

/// Where `lit add` gets the BibTeX entry for a web artifact.
#[derive(Debug)]
pub(crate) enum BibSource {
    /// The page has a DOI; the DOI path supplies the entry.
    Doi(String),
    /// A `@misc` entry from the page's own metadata.
    Misc(MiscParams),
}

impl WebArtifact {
    /// The extracted body text.
    pub fn text_path(&self) -> PathBuf {
        self.dir.join("paper.txt")
    }

    /// The `@misc` fields for this work under `citekey`: howpublished is the
    /// site's name and url the canonical page URL.
    fn misc_params(&self, citekey: &str) -> MiscParams {
        MiscParams {
            citekey: citekey.to_string(),
            title: self.title.clone(),
            authors: self.authors.clone(),
            year: self.year.clone(),
            howpublished: Some(self.url.site.name().to_string()),
            note: None,
            url: Some(self.url.url.clone()),
        }
    }

    /// The entry source for `lit add`: the DOI when the page has one,
    /// otherwise `@misc` under `key` or the artifact's citekey.
    pub(crate) fn bib_source(&self, key: Option<&str>) -> BibSource {
        match &self.doi {
            Some(doi) => BibSource::Doi(doi.clone()),
            None => BibSource::Misc(self.misc_params(key.unwrap_or(&self.citekey))),
        }
    }
}

/// True when `lit read` should treat `id` as a web page: an http(s) URL
/// that is not an identifier URL (arXiv, DOI, PhilPapers, Open Library).
/// Unsupported hosts are included so `read` reports the host allowlist.
pub fn is_page_url(id: &str) -> bool {
    web::is_http_url(id) && crate::detect::detect_type(id) == crate::detect::InputType::Url
}

/// `lit read <URL>`: the text path of the URL's artifact, fetching it first
/// when none exists or `ctx.no_cache` is set.
pub async fn read(ctx: &super::Context, input: &str) -> Result<ReadResult, Box<dyn std::error::Error>> {
    let root = super::read::find_pdf_base()?;
    let artifact = ensure(&ctx.client(), &root, input, None, ctx.no_cache).await?;
    Ok(ReadResult { path: artifact.text_path(), format: "txt".to_string(), extra_files: Vec::new() })
}

/// The artifact under `root` for the work at `input`, fetched and stored
/// when none exists or `refresh` is set.
///
/// A new artifact's directory is `key`, else the generated
/// author-year-word citekey; an existing one keeps its directory. A
/// directory of that name holding a different work is an error, never
/// overwritten. On any failure nothing is written.
/// Cost: one `source.yaml` read per artifact under `root`, plus one request
/// when the work has no artifact.
pub(crate) async fn ensure<F: MetadataFetch>(
    f: &F,
    root: &Path,
    input: &str,
    key: Option<&str>,
    refresh: bool,
) -> Result<WebArtifact, String> {
    let url = web::parse_url(input)?;
    let existing = find_artifact(root, &url.identity())?;
    if let Some(dir) = &existing
        && !refresh
    {
        return load_artifact(dir, url);
    }

    let fetch_url = url.fetch_url();
    let body = f
        .load(&crate::db::Db::cache_key("web", &fetch_url), &fetch_url)
        .await
        .map_err(|e| format!("fetching {}: {}", url.url, e))?;
    let page = web::parse_page(url.site, &body).map_err(|e| format!("{}: {}", url.url, e))?;

    let dir = match existing {
        Some(dir) => dir,
        None => {
            let citekey = key
                .map(str::to_string)
                .unwrap_or_else(|| crate::citekey::generate(&page.authors, &page.year, &page.title));
            let dir = root.join(&citekey);
            if dir.exists() {
                return Err(format!(
                    "{} already holds a different work; rerun `lit add {} <bib> --key <citekey>` to store this page under another citekey",
                    dir.display(),
                    url.url
                ));
            }
            dir
        }
    };
    let artifact = artifact_for(dir, url, &page);
    write_artifact(&artifact, &page).map_err(|e| format!("writing {}: {}", artifact.dir.display(), e))?;
    crate::format::info(&format!("stored {} in {}", artifact.url.url, artifact.dir.display()));
    Ok(artifact)
}

/// An artifact's citekey is its directory name.
fn dir_citekey(dir: &Path) -> String {
    dir.file_name().unwrap_or_default().to_string_lossy().into_owned()
}

fn artifact_for(dir: PathBuf, url: WebUrl, page: &WebPage) -> WebArtifact {
    WebArtifact {
        citekey: dir_citekey(&dir),
        dir,
        title: page.title.clone(),
        authors: page.authors.clone(),
        year: page.year.clone(),
        doi: page.doi.clone(),
        url,
    }
}

/// The artifact directory whose recorded `url` names the same work as
/// `identity`, if any. Staging directories (dot-prefixed) are skipped.
fn find_artifact(root: &Path, identity: &str) -> Result<Option<PathBuf>, String> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("reading {}: {}", root.display(), e)),
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') || !dir.is_dir() {
            continue;
        }
        let Ok(yaml) = std::fs::read_to_string(dir.join("source.yaml")) else {
            continue;
        };
        let same = yaml_value(&yaml, "url")
            .and_then(|u| web::parse_url(&u).ok())
            .is_some_and(|u| u.identity() == identity);
        if same {
            return Ok(Some(dir));
        }
    }
    Ok(None)
}

/// Read an artifact's metadata back from its `source.yaml`.
fn load_artifact(dir: &Path, url: WebUrl) -> Result<WebArtifact, String> {
    let yaml = std::fs::read_to_string(dir.join("source.yaml")).map_err(|e| format!("{}: {}", dir.display(), e))?;
    if !dir.join("paper.txt").is_file() {
        return Err(format!("{} has no paper.txt; rerun with --no-cache to fetch the page again", dir.display()));
    }
    let field = |k: &str| yaml_value(&yaml, k).unwrap_or_default();
    Ok(WebArtifact {
        dir: dir.to_path_buf(),
        citekey: dir_citekey(dir),
        title: field("title"),
        authors: field("authors").split(" and ").filter(|a| !a.is_empty()).map(str::to_string).collect(),
        year: field("year"),
        doi: yaml_value(&yaml, "doi"),
        url,
    })
}

/// A `source.yaml` value with its quotes and `\"` escapes removed.
fn yaml_value(yaml: &str, key: &str) -> Option<String> {
    super::check::yaml_field(yaml, key).map(|v| v.replace("\\\"", "\"")).filter(|v| !v.is_empty())
}

/// Write `paper.txt` and `source.yaml` into a staging directory, then move
/// it onto `artifact.dir`, so a failure leaves the previous artifact or none.
fn write_artifact(artifact: &WebArtifact, page: &WebPage) -> std::io::Result<()> {
    let staging = super::misc::staging_path(&artifact.dir);
    let _ = std::fs::remove_dir_all(&staging);
    let result = fill_staging(artifact, page, &staging).and_then(|()| super::misc::publish(&staging, &artifact.dir));
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

fn fill_staging(artifact: &WebArtifact, page: &WebPage, staging: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(staging)?;
    std::fs::write(staging.join("paper.txt"), format!("{}\n", page.text))?;
    std::fs::write(staging.join("source.yaml"), source_yaml(artifact, page, &super::utc_timestamp()))
}

/// `source.yaml` for a web artifact: the misc fields (title, authors, year,
/// howpublished, url, retrieved = fetch time) plus the site, the extraction
/// method, the page's date and DOI when present, and the confirmation that
/// the publishing site supplied the metadata.
fn source_yaml(artifact: &WebArtifact, page: &WebPage, fetched: &str) -> String {
    let mut yaml = super::misc::build_misc_source_yaml(&artifact.misc_params(&artifact.citekey), fetched);
    yaml.push_str(&format!("host: \"{}\"\n", artifact.url.site.id()));
    yaml.push_str(&format!("extraction: \"{}\"\n", page.extraction));
    if let Some(date) = &page.date {
        yaml.push_str(&format!("date: \"{}\"\n", date));
    }
    if let Some(doi) = &page.doi {
        yaml.push_str(&format!("doi: \"{}\"\n", doi.replace('"', "\\\"")));
    }
    yaml.push_str(&format!("metadata_source: \"{}\"\n", artifact.url.site.id()));
    yaml.push_str("metadata_confirmed: true\n");
    yaml
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::metadata::tests::MockMetadataFetch;

    const DISTILL: &str = include_str!("../../test/web/distill-zoom-in.html");
    const TC: &str = include_str!("../../test/web/transformer-circuits-framework.html");
    const LW: &str = include_str!("../../test/web/lesswrong-causal-scrubbing.json");

    const DISTILL_URL: &str = "https://distill.pub/2020/circuits/zoom-in/";
    const TC_URL: &str = "https://transformer-circuits.pub/2021/framework/index.html";
    const AF_URL: &str = "https://www.alignmentforum.org/posts/JvZhhzycHu2Yd57RN/causal-scrubbing-a-method-for-rigorously-testing";

    fn mock() -> MockMetadataFetch {
        MockMetadataFetch::new(vec![
            ("distill.pub/2020/circuits/zoom-in/", Ok(DISTILL.to_string())),
            ("transformer-circuits.pub/2021/framework/", Ok(TC.to_string())),
            ("www.lesswrong.com/graphql", Ok(LW.to_string())),
        ])
    }

    fn calls(f: &MockMetadataFetch) -> usize {
        f.fetch.calls.borrow().len()
    }

    #[tokio::test]
    async fn each_host_stores_text_and_provenance_under_the_generated_citekey() {
        let root = tempfile::tempdir().unwrap();
        let f = mock();
        for (url, key, host, extraction) in [
            (DISTILL_URL, "olah2020zoom", "distill", "d-article"),
            (TC_URL, "elhage2021mathematical", "transformer-circuits", "d-article"),
            (AF_URL, "lawrencec2022causal", "alignmentforum", "lesswrong-graphql-htmlBody"),
        ] {
            let a = ensure(&f, root.path(), url, None, false).await.unwrap();
            assert_eq!(a.dir, root.path().join(key));
            assert_eq!(a.citekey, key);
            let text = std::fs::read_to_string(a.text_path()).unwrap();
            assert!(text.len() > 100 && text.ends_with('\n'), "{}: {}", key, text);
            let yaml = std::fs::read_to_string(a.dir.join("source.yaml")).unwrap();
            for line in [
                format!("bibtex_key: \"{}\"", key),
                format!("url: \"{}\"", a.url.url),
                format!("host: \"{}\"", host),
                format!("extraction: \"{}\"", extraction),
                "metadata_confirmed: true".to_string(),
            ] {
                assert!(yaml.contains(&line), "{}: missing {:?} in:\n{}", key, line, yaml);
            }
            let retrieved = yaml_value(&yaml, "retrieved").unwrap();
            assert!(retrieved.len() == 20 && retrieved.ends_with('Z'), "fetch time: {}", retrieved);
        }
        assert_eq!(calls(&f), 3);
        let leftovers: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "staging left behind: {:?}", leftovers);
    }

    #[tokio::test]
    async fn second_read_is_served_from_the_artifact_with_no_request() {
        let root = tempfile::tempdir().unwrap();
        let f = mock();
        let first = ensure(&f, root.path(), DISTILL_URL, None, false).await.unwrap();
        // A different spelling of the same page still hits.
        let again = ensure(&f, root.path(), "http://distill.pub/2020/circuits/zoom-in#claim-1", None, false).await.unwrap();
        assert_eq!(calls(&f), 1);
        assert_eq!(again, first);
        assert_eq!(again.doi.as_deref(), Some("10.23915/distill.00024.001"));
        assert_eq!(again.authors, vec!["Chris Olah", "Nick Cammarata", "Ludwig Schubert"]);
    }

    #[tokio::test]
    async fn one_forum_post_has_one_artifact_across_hosts() {
        let root = tempfile::tempdir().unwrap();
        let f = mock();
        let af = ensure(&f, root.path(), AF_URL, None, false).await.unwrap();
        let lw = ensure(&f, root.path(), "https://www.lesswrong.com/posts/JvZhhzycHu2Yd57RN/x", None, false).await.unwrap();
        assert_eq!(calls(&f), 1);
        assert_eq!(lw.dir, af.dir);
    }

    #[tokio::test]
    async fn refresh_refetches_into_the_same_directory() {
        let root = tempfile::tempdir().unwrap();
        let f = mock();
        let first = ensure(&f, root.path(), TC_URL, None, false).await.unwrap();
        let again = ensure(&f, root.path(), TC_URL, Some("ignored2021key"), true).await.unwrap();
        assert_eq!(calls(&f), 2);
        assert_eq!(again.dir, first.dir);
        assert!(!root.path().join("ignored2021key").exists());
    }

    #[tokio::test]
    async fn key_names_a_new_artifact() {
        let root = tempfile::tempdir().unwrap();
        let a = ensure(&mock(), root.path(), DISTILL_URL, Some("olah2020circuits"), false).await.unwrap();
        assert_eq!(a.dir, root.path().join("olah2020circuits"));
        assert!(std::fs::read_to_string(a.dir.join("source.yaml")).unwrap().contains("bibtex_key: \"olah2020circuits\""));
    }

    #[tokio::test]
    async fn unsupported_host_fails_before_any_request() {
        let root = tempfile::tempdir().unwrap();
        let f = mock();
        let err = ensure(&f, root.path(), "https://example.com/blog/post", None, false).await.unwrap_err();
        assert!(err.contains("distill.pub") && err.contains("alignmentforum.org"), "err: {}", err);
        assert_eq!(calls(&f), 0);
    }

    #[tokio::test]
    async fn a_citekey_held_by_another_work_is_an_error_not_an_overwrite() {
        let root = tempfile::tempdir().unwrap();
        let other = root.path().join("olah2020zoom");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("source.yaml"), "bibtex_key: \"olah2020zoom\"\ntitle: \"Other\"\n").unwrap();
        let err = ensure(&mock(), root.path(), DISTILL_URL, None, false).await.unwrap_err();
        assert!(err.contains("already holds a different work") && err.contains("--key"), "err: {}", err);
        assert_eq!(std::fs::read_to_string(other.join("source.yaml")).unwrap(), "bibtex_key: \"olah2020zoom\"\ntitle: \"Other\"\n");
    }

    #[tokio::test]
    async fn fetch_or_parse_failure_writes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let throttled = MockMetadataFetch::new(vec![(
            "distill.pub",
            Err("HTTP 429 Too Many Requests for https://distill.pub/x after 5 attempts".to_string()),
        )]);
        let err = ensure(&throttled, root.path(), DISTILL_URL, None, false).await.unwrap_err();
        assert!(err.contains("HTTP 429"), "err: {}", err);
        let not_article = MockMetadataFetch::new(vec![("distill.pub", Ok("<html><title>404</title></html>".to_string()))]);
        assert!(ensure(&not_article, root.path(), DISTILL_URL, None, false).await.is_err());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn bib_source_is_the_doi_when_present_else_misc_with_site_and_url() {
        let root = tempfile::tempdir().unwrap();
        let f = mock();
        let distill = ensure(&f, root.path(), DISTILL_URL, None, false).await.unwrap();
        assert!(matches!(distill.bib_source(None), BibSource::Doi(d) if d == "10.23915/distill.00024.001"));

        let af = ensure(&f, root.path(), AF_URL, None, false).await.unwrap();
        let BibSource::Misc(p) = af.bib_source(None) else { panic!("expected @misc") };
        assert_eq!(p.citekey, "lawrencec2022causal");
        assert_eq!(p.howpublished.as_deref(), Some("AI Alignment Forum"));
        assert_eq!(p.url.as_deref(), Some("https://www.alignmentforum.org/posts/JvZhhzycHu2Yd57RN"));
        assert_eq!(p.year, "2022");
        assert_eq!(p.authors[0], "LawrenceC");

        let tc = ensure(&f, root.path(), TC_URL, None, false).await.unwrap();
        let BibSource::Misc(p) = tc.bib_source(Some("elhage2021framework")) else { panic!("expected @misc") };
        assert_eq!(p.citekey, "elhage2021framework");
        assert_eq!(p.howpublished.as_deref(), Some("Transformer Circuits Thread"));
    }

    #[test]
    fn read_routes_page_urls_and_leaves_identifier_urls_alone() {
        for url in [DISTILL_URL, TC_URL, AF_URL, "https://example.com/post"] {
            assert!(is_page_url(url), "{}", url);
        }
        for id in ["https://arxiv.org/abs/2106.09685", "https://doi.org/10.23915/distill.00024.001", "olah2020zoom", "2106.09685"] {
            assert!(!is_page_url(id), "{}", id);
        }
    }
}
