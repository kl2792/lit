pub mod add;
pub mod check;
pub mod clean;
pub mod clio;
pub mod misc;
pub mod cites;
pub mod download;
pub mod open;
pub mod path;
pub mod read;
pub mod refs;
pub mod search;
pub mod verify;

use std::path::PathBuf;
use std::sync::Arc;

use crate::api::PaperResult;
use crate::db::{Db, PaperRow};

#[derive(Clone)]
pub struct Context {
    pub verbose: bool,
    pub bib_file: Option<PathBuf>,
    pub bib_stdout: bool,
    pub json: bool,
    pub no_cache: bool,
    pub db: Arc<Db>,
}

impl Context {
    pub fn client(&self) -> crate::http::Client {
        crate::http::Client::new(Arc::clone(&self.db), self.no_cache)
    }

    /// Handle -b/--bib: append to file, or print to stdout.
    pub fn handle_bib(&self, bibtex: &str) {
        if !bibtex.contains('@') {
            return;
        }
        if let Some(ref path) = self.bib_file {
            if let Err(e) = crate::bibtex::append_to_file(path, bibtex) {
                crate::format::warn(&format!("Failed to append to bib file: {}", e));
            }
        }
        if self.bib_stdout {
            println!("{}", bibtex);
        }
    }
}

/// Opportunistic upsert: index a paper into the local DB, warn on failure.
fn try_upsert(ctx: &Context, paper: &PaperResult, source: &str) {
    let row = PaperRow::from(paper);
    if let Err(e) = ctx.db.upsert_paper(&row, Some(source)) {
        eprintln!("warning: failed to index paper: {}", e);
    }
}

/// Auto-dispatch based on input type detection.
pub async fn auto_dispatch(ctx: &Context, input: &str, open: bool) -> Result<(), Box<dyn std::error::Error>> {
    use crate::detect::{detect_type, InputType};
    if open {
        return open::run(ctx, input);
    }
    match detect_type(input) {
        InputType::Arxiv => lookup_arxiv(ctx, input).await,
        InputType::Doi => lookup_doi(ctx, input).await,
        InputType::Isbn => lookup_isbn(ctx, input).await,
        InputType::DblpUrl => lookup_dblp_url(ctx, input).await,
        InputType::SemanticScholarUrl => open::run(ctx, input),
        InputType::PhilPapersUrl => lookup_philpapers_url(ctx, input).await,
        InputType::OpenLibraryUrl => {
            // OL URL: open it in the browser (lookup not supported)
            open::run(ctx, input)
        }
        InputType::Url => {
            let lr = lookup_url_data(ctx, input).await?;
            display_paper(ctx, &lr.paper, lr.bibtex.as_deref());
            Ok(())
        }
        InputType::Search => search::run(ctx, input, 10, None).await,
    }
}

/// Display a PaperResult according to context flags (--json, -b).
///
/// This is the single display function for all lookup commands.
/// When --json is set, output JSON. When -b is set without a file, print
/// BibTeX to stdout. Otherwise, print formatted metadata.
fn display_paper(ctx: &Context, paper: &PaperResult, bibtex: Option<&str>) {
    if ctx.json {
        let json = paper_to_json(paper);
        println!("{}", serde_json::to_string_pretty(&json).unwrap());
        return;
    }

    // If -b with no file path (bib_stdout), print only BibTeX.
    if ctx.bib_stdout {
        if let Some(bib) = bibtex {
            println!("{}", bib);
        }
        // Also append to bib_file if given (both can be true).
        if let Some(ref path) = ctx.bib_file {
            if let Some(bib) = bibtex {
                if bib.contains('@') {
                    if let Err(e) = crate::bibtex::append_to_file(path, bib) {
                        crate::format::warn(&format!("Failed to append to bib file: {}", e));
                    }
                }
            }
        }
        return;
    }

    // Print metadata
    println!("Title: {}", paper.title);
    let authors_display = if paper.authors.len() > 3 {
        format!("{} et al.", paper.authors[..3].join(", "))
    } else {
        paper.authors.join(", ")
    };
    if !authors_display.is_empty() {
        println!("Authors: {}", authors_display);
    }
    if let Some(ref date) = paper.published_date {
        println!("Published: {}", date);
    }
    if paper.year != "?" && paper.published_date.is_none() {
        println!("Year: {}", paper.year);
    }
    if let Some(ref arxiv) = paper.arxiv_id {
        println!("arXiv: {}", arxiv);
    }
    if let Some(ref doi) = paper.doi {
        println!("DOI: {}", doi);
    }
    if let Some(ref venue) = paper.venue {
        println!("Venue: {}", venue);
    }
    if let Some(ref isbn) = paper.isbn {
        println!("ISBN: {}", isbn);
    }
    if !paper.categories.is_empty() {
        println!("Categories: {}", paper.categories.join(", "));
    }
    if let Some(ref url) = paper.pdf_url {
        println!("PDF: {}", url);
    }
    if let Some(ref abs) = paper.abstract_text {
        println!();
        println!("Abstract: {}", abs);
    }

    if let Some(bib) = bibtex {
        println!();
        println!("{}", bib);
    }

    // Append to bib file if --bib <file> was given
    if let Some(ref path) = ctx.bib_file {
        if let Some(bib) = bibtex {
            if bib.contains('@') {
                if let Err(e) = crate::bibtex::append_to_file(path, bib) {
                    crate::format::warn(&format!("Failed to append to bib file: {}", e));
                }
            }
        }
    }
}

/// Convert a PaperResult to a JSON value.
pub fn paper_to_json(paper: &PaperResult) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("title".into(), serde_json::Value::String(paper.title.clone()));
    map.insert(
        "authors".into(),
        serde_json::Value::Array(
            paper
                .authors
                .iter()
                .map(|a| serde_json::Value::String(a.clone()))
                .collect(),
        ),
    );
    map.insert("year".into(), serde_json::Value::String(paper.year.clone()));
    if let Some(ref doi) = paper.doi {
        map.insert("doi".into(), serde_json::Value::String(doi.clone()));
    }
    if let Some(ref arxiv) = paper.arxiv_id {
        map.insert("arxiv_id".into(), serde_json::Value::String(arxiv.clone()));
    }
    if let Some(ref venue) = paper.venue {
        map.insert("venue".into(), serde_json::Value::String(venue.clone()));
    }
    if let Some(ref isbn) = paper.isbn {
        map.insert("isbn".into(), serde_json::Value::String(isbn.clone()));
    }
    if let Some(ref url) = paper.pdf_url {
        map.insert("pdf_url".into(), serde_json::Value::String(url.clone()));
    }
    if let Some(ref abs) = paper.abstract_text {
        map.insert("abstract".into(), serde_json::Value::String(abs.clone()));
    }
    if let Some(cites) = paper.citations {
        map.insert(
            "citations".into(),
            serde_json::Value::Number(serde_json::Number::from(cites)),
        );
    }
    if !paper.categories.is_empty() {
        map.insert(
            "categories".into(),
            serde_json::Value::Array(
                paper
                    .categories
                    .iter()
                    .map(|c| serde_json::Value::String(c.clone()))
                    .collect(),
            ),
        );
    }
    if let Some(ref date) = paper.published_date {
        map.insert(
            "published_date".into(),
            serde_json::Value::String(date.clone()),
        );
    }
    serde_json::Value::Object(map)
}

/// Convert a PaperResult to a compact JSON value for space-constrained contexts.
///
/// Omits categories, published_date, and pdf_url. Truncates abstracts to 150 chars.
pub fn paper_to_json_brief(paper: &PaperResult) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("title".into(), serde_json::Value::String(paper.title.clone()));
    map.insert(
        "authors".into(),
        serde_json::Value::Array(
            paper
                .authors
                .iter()
                .map(|a| serde_json::Value::String(a.clone()))
                .collect(),
        ),
    );
    map.insert("year".into(), serde_json::Value::String(paper.year.clone()));
    if let Some(ref doi) = paper.doi {
        map.insert("doi".into(), serde_json::Value::String(doi.clone()));
    }
    if let Some(ref arxiv) = paper.arxiv_id {
        map.insert("arxiv_id".into(), serde_json::Value::String(arxiv.clone()));
    }
    if let Some(ref venue) = paper.venue {
        map.insert("venue".into(), serde_json::Value::String(venue.clone()));
    }
    if let Some(ref isbn) = paper.isbn {
        map.insert("isbn".into(), serde_json::Value::String(isbn.clone()));
    }
    if let Some(ref abs) = paper.abstract_text {
        let truncated = if abs.len() > 150 {
            format!("{}...", &abs[..150])
        } else {
            abs.clone()
        };
        map.insert("abstract".into(), serde_json::Value::String(truncated));
    }
    if let Some(cites) = paper.citations {
        map.insert(
            "citations".into(),
            serde_json::Value::Number(serde_json::Number::from(cites)),
        );
    }
    serde_json::Value::Object(map)
}

// -- Shared refs/cites helper -------------------------------------------------

/// Get a Semantic Scholar API identifier from a PaperResult.
///
/// Prefers s2_id, then DOI (prefixed), then arXiv ID (prefixed).
fn s2_api_id(p: &PaperResult) -> Option<String> {
    if let Some(ref id) = p.s2_id {
        return Some(id.clone());
    }
    if let Some(ref doi) = p.doi {
        return Some(format!("DOI:{}", doi));
    }
    if let Some(ref arxiv) = p.arxiv_id {
        return Some(format!("ARXIV:{}", arxiv));
    }
    None
}

/// Upsert a PaperResult and store a citation edge from/to source_id.
///
/// For refs (is_refs=true): source_id cites the new paper.
/// For cites (is_refs=false): the new paper cites source_id.
fn upsert_and_link(ctx: &Context, paper: &PaperResult, source_id: i64, is_refs: bool) -> Option<i64> {
    let row = PaperRow::from(paper);
    match ctx.db.upsert_paper(&row, Some("s2")) {
        Ok(target_id) => {
            let (s, t) = if is_refs {
                (source_id, target_id)
            } else {
                (target_id, source_id)
            };
            if let Err(e) = ctx.db.insert_citation(s, t) {
                eprintln!("warning: failed to store citation edge: {}", e);
            }
            Some(target_id)
        }
        Err(e) => {
            eprintln!("warning: failed to index paper: {}", e);
            None
        }
    }
}

/// Fetch related papers (references or citations) and return them as structured data.
///
/// Same BFS logic as `fetch_related` but returns `Vec<PaperResult>` instead of printing.
pub async fn fetch_related_data(
    ctx: &Context,
    paper_id: &str,
    direction: &str,
    cache_prefix: &str,
    url_fn: fn(&str) -> String,
    parse_fn: fn(&str) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>>,
    hops: usize,
    max_papers: usize,
) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>> {
    use std::collections::HashSet;

    let client = ctx.client();
    let is_refs = direction == "references";

    if ctx.verbose {
        crate::format::info(&format!(
            "Getting {} for: {} (hops={}, max={})",
            direction, paper_id, hops, max_papers
        ));
    }

    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(paper_id.to_string());

    let mut frontier: Vec<(String, Option<i64>)> = vec![(paper_id.to_string(), None)];
    let mut collected: Vec<PaperResult> = Vec::new();

    for _hop in 0..hops {
        if frontier.is_empty() || collected.len() >= max_papers {
            break;
        }

        let mut next_frontier: Vec<(String, Option<i64>)> = Vec::new();

        for (api_id, source_db_id) in &frontier {
            if collected.len() >= max_papers {
                break;
            }

            let key = crate::db::Db::cache_key(cache_prefix, api_id);
            let url = url_fn(api_id);

            let body = match client.get_cached(&key, &url, crate::db::TTL_SEARCH).await {
                Ok(b) => b,
                Err(e) => {
                    if ctx.verbose {
                        crate::format::warn(&format!(
                            "Failed to fetch {} for {}: {}",
                            direction, api_id, e
                        ));
                    }
                    continue;
                }
            };
            let results = match parse_fn(&body) {
                Ok(r) => r,
                Err(e) => {
                    if ctx.verbose {
                        crate::format::warn(&format!(
                            "Failed to parse {} for {}: {}",
                            direction, api_id, e
                        ));
                    }
                    continue;
                }
            };

            for p in &results {
                if collected.len() >= max_papers {
                    break;
                }

                let child_api_id = s2_api_id(p);

                if let Some(ref cid) = child_api_id {
                    if visited.contains(cid) {
                        continue;
                    }
                    visited.insert(cid.clone());
                }

                // Upsert and store citation edge
                let child_db_id = if let Some(src_id) = source_db_id {
                    upsert_and_link(ctx, p, *src_id, is_refs)
                } else {
                    let row = PaperRow::from(p);
                    ctx.db.upsert_paper(&row, Some("s2")).ok()
                };

                collected.push(p.clone());

                if let Some(cid) = child_api_id {
                    next_frontier.push((cid, child_db_id));
                }
            }
        }

        frontier = next_frontier;
    }

    Ok(collected)
}

/// Fetch and display related papers (references or citations) from Semantic Scholar.
///
/// `direction` is "references" or "citations" (for display messages).
/// `cache_prefix` is the cache key prefix (e.g. "refs" or "cites").
/// `url_fn` builds the API URL from a paper ID.
/// `parse_fn` parses the API response body.
/// `hops` controls BFS depth (1 = direct only).
/// `max_papers` caps total papers fetched across all hops.
pub(crate) async fn fetch_related(
    ctx: &Context,
    paper_id: &str,
    direction: &str,
    cache_prefix: &str,
    url_fn: fn(&str) -> String,
    parse_fn: fn(&str) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>>,
    hops: usize,
    max_papers: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let results = fetch_related_data(ctx, paper_id, direction, cache_prefix, url_fn, parse_fn, hops, max_papers).await?;

    if results.is_empty() {
        println!("No {} found", direction);
    } else {
        for (i, p) in results.iter().enumerate() {
            let rank = i + 1;
            let author = p.authors.first().map(|s| s.as_str()).unwrap_or("Unknown");
            println!("{rank}. {} ({}) - {}", p.title, p.year, author);
        }
    }

    Ok(())
}

// -- Lookup functions ---------------------------------------------------------

/// Lookup result containing a paper and optional BibTeX.
pub struct LookupResult {
    pub paper: PaperResult,
    pub bibtex: Option<String>,
}

/// Look up a paper by identifier and return structured data.
///
/// Dispatches based on input type (arXiv, DOI, ISBN, DBLP URL).
/// Returns `LookupResult` with the paper metadata and optional BibTeX.
pub async fn lookup_data(ctx: &Context, input: &str) -> Result<LookupResult, Box<dyn std::error::Error>> {
    use crate::detect::{detect_type, InputType};
    match detect_type(input) {
        InputType::Arxiv => lookup_arxiv_data(ctx, input).await,
        InputType::Doi => lookup_doi_data(ctx, input).await,
        InputType::Isbn => lookup_isbn_data(ctx, input).await,
        InputType::DblpUrl => lookup_dblp_url_data(ctx, input).await,
        InputType::SemanticScholarUrl => {
            Err("Semantic Scholar URLs are not supported for lookup_data; use open instead".into())
        }
        InputType::PhilPapersUrl => lookup_philpapers_url_data(ctx, input).await,
        InputType::OpenLibraryUrl => {
            Err("Open Library URLs are not supported for lookup_data; use lit add instead".into())
        }
        InputType::Url => lookup_url_data(ctx, input).await,
        InputType::Search => {
            Err("Search queries are not supported for lookup_data; use search instead".into())
        }
    }
}

/// Look up a paper from an arbitrary HTTPS URL by extracting the title and searching.
async fn lookup_url_data(ctx: &Context, url: &str) -> Result<LookupResult, Box<dyn std::error::Error>> {
    let title = add::fetch_title_from_url(url).await?;
    eprintln!("Extracted title: {}", &title[..title.len().min(80)]);
    let top = search::resolve_top(ctx, &title).await?;
    if let Some(ref doi) = top.doi {
        return lookup_doi_data(ctx, doi).await;
    }
    if let Some(ref arxiv_id) = top.arxiv_id {
        return lookup_arxiv_data(ctx, arxiv_id).await;
    }
    // No canonical identifier: return the search result as-is with no BibTeX
    Ok(LookupResult { paper: top, bibtex: None })
}

/// Look up an arXiv paper and return structured data.
async fn lookup_arxiv_data(ctx: &Context, input: &str) -> Result<LookupResult, Box<dyn std::error::Error>> {
    use crate::api::arxiv as arxiv_api;
    use crate::api::semantic_scholar as s2_api;
    use crate::{db, detect, format};

    let arxiv_id = detect::normalize_arxiv(input);
    let arxiv_url = arxiv_api::query_url(&arxiv_id);

    if ctx.verbose {
        format::info(&format!("Looking up arXiv: {}", arxiv_id));
    }

    let client = ctx.client();
    let arxiv_cache_key = db::Db::cache_key("arxiv", &arxiv_id);
    let s2_url = s2_api::paper_url(&format!("arXiv:{}", arxiv_id));
    let s2_cache_key = db::Db::cache_key("s2_paper", &arxiv_id);

    let (arxiv_body, s2_body) = tokio::join!(
        client.get_cached_deferred(&arxiv_cache_key, &arxiv_url, db::TTL_DOI),
        client.get_cached(&s2_cache_key, &s2_url, db::TTL_DOI),
    );

    let arxiv_body = arxiv_body?;
    let mut result = arxiv_api::parse_entry(&arxiv_body)?;
    client.cache_set(&arxiv_cache_key, &arxiv_url, &arxiv_body);

    if let Ok(body) = s2_body {
        if let Ok(s2) = s2_api::parse_paper(&body) {
            if result.s2_id.is_none() { result.s2_id = s2.s2_id; }
            if result.doi.is_none() { result.doi = s2.doi; }
            if result.citations.is_none() { result.citations = s2.citations; }
            if result.venue.is_none() { result.venue = s2.venue; }
            if result.pdf_url.is_none() { result.pdf_url = s2.pdf_url; }
        }
    }

    let resolved_id = result.arxiv_id.as_deref().unwrap_or(&arxiv_id);
    let citekey = crate::citekey::generate(&result.authors, &result.year, &result.title);
    let author_bib = result.authors.join(" and ");
    let bibtex = format!(
        "@article{{{},\n  title = {{{}}},\n  author = {{{}}},\n  year = {{{}}},\n  eprint = {{{}}},\n  archivePrefix = {{arXiv}},\n}}",
        citekey, result.title, author_bib, result.year, resolved_id
    );

    try_upsert(ctx, &result, "arxiv");
    Ok(LookupResult { paper: result, bibtex: Some(bibtex) })
}

/// Look up a DOI and return structured data.
async fn lookup_doi_data(ctx: &Context, input: &str) -> Result<LookupResult, Box<dyn std::error::Error>> {
    use crate::api::openalex as oa_api;
    use crate::{db, detect, format};

    let doi = detect::normalize_doi(input);
    let cr_url = crate::api::crossref::doi_url(&doi);

    if ctx.verbose {
        format::info(&format!("Looking up DOI: {}", doi));
    }

    let client = ctx.client();
    let cr_cache_key = db::Db::cache_key("doi", &doi);
    let oa_url = oa_api::work_by_doi_url(&doi);
    let oa_cache_key = db::Db::cache_key("oa_work", &doi);
    let bib_url = crate::api::crossref::bibtex_url(&doi);

    let (cr_body, oa_body, bibtex) = tokio::join!(
        client.get_cached(&cr_cache_key, &cr_url, db::TTL_DOI),
        client.get_cached(&oa_cache_key, &oa_url, db::TTL_DOI),
        client.get(&bib_url),
    );

    let cr_body = cr_body?;
    let mut paper = crate::api::crossref::parse_doi(&cr_body)?;

    if let Ok(body) = oa_body {
        if let Ok(oa) = oa_api::parse_work(&body) {
            if paper.citations.is_none() {
                paper.citations = oa.citations;
            }
        }
    }

    try_upsert(ctx, &paper, "crossref");
    Ok(LookupResult { paper, bibtex: bibtex.ok() })
}

/// Look up a book by ISBN and return structured data.
async fn lookup_isbn_data(ctx: &Context, input: &str) -> Result<LookupResult, Box<dyn std::error::Error>> {
    use crate::{db, detect, format};

    let isbn = detect::normalize_isbn(input);

    if ctx.verbose {
        format::info(&format!("Looking up ISBN: {}", isbn));
    }

    let client = ctx.client();
    let cache_key = format!("isbn_{}", isbn);
    let url = crate::api::openlibrary::isbn_url(&isbn);
    let body = client.get_cached(&cache_key, &url, db::TTL_DOI).await?;

    let book = crate::api::openlibrary::parse_isbn_detail(&body)?;

    let full_title = match book.subtitle {
        Some(ref sub) => format!("{}: {}", book.title, sub),
        None => book.title.clone(),
    };

    let paper = PaperResult {
        title: full_title.clone(),
        authors: book.authors.clone(),
        year: book.year.clone(),
        venue: book.publisher.clone(),
        isbn: book.isbn_13.clone().or(book.isbn_10.clone()),
        published_date: book.publish_date.clone(),
        ..Default::default()
    };

    let author = if book.authors.is_empty() {
        "Unknown".to_string()
    } else {
        book.authors[0].clone()
    };
    let ckey = crate::citekey::generate(&book.authors, &book.year, &full_title);
    let isbn_val = book.isbn_13.as_deref().or(book.isbn_10.as_deref()).unwrap_or_default();
    let publisher_line = book.publisher.as_ref()
        .map(|p| format!("  publisher = {{{}}},\n", p))
        .unwrap_or_default();
    let isbn_line = if isbn_val.is_empty() {
        String::new()
    } else {
        format!("  isbn = {{{}}}\n", isbn_val)
    };
    let bibtex = format!(
        "@book{{{},\n  title = {{{}}},\n  author = {{{}}},\n  year = {{{}}},\n{}{}}}",
        ckey, full_title, author, book.year, publisher_line, isbn_line
    );

    try_upsert(ctx, &paper, "openlibrary");
    Ok(LookupResult { paper, bibtex: Some(bibtex) })
}

/// Fetch BibTeX from a DBLP URL and return structured data.
async fn lookup_dblp_url_data(ctx: &Context, url: &str) -> Result<LookupResult, Box<dyn std::error::Error>> {
    use crate::format;

    let bib_url = format!("{}.bib", url.trim_end_matches(".bib"));

    if ctx.verbose {
        format::info("Fetching BibTeX from DBLP...");
    }

    let client = ctx.client();
    let result = client.get(&bib_url).await?;

    // DBLP only returns BibTeX, no structured metadata
    let paper = PaperResult {
        title: url.to_string(),
        ..Default::default()
    };
    Ok(LookupResult { paper, bibtex: Some(result) })
}

/// Look up an arXiv paper and display results.
async fn lookup_arxiv(ctx: &Context, input: &str) -> Result<(), Box<dyn std::error::Error>> {
    let lr = lookup_arxiv_data(ctx, input).await?;
    display_paper(ctx, &lr.paper, lr.bibtex.as_deref());
    Ok(())
}

/// Look up a DOI via CrossRef and display results.
async fn lookup_doi(ctx: &Context, input: &str) -> Result<(), Box<dyn std::error::Error>> {
    let lr = lookup_doi_data(ctx, input).await?;
    display_paper(ctx, &lr.paper, lr.bibtex.as_deref());
    Ok(())
}

/// Look up a book by ISBN via OpenLibrary and display results.
async fn lookup_isbn(ctx: &Context, input: &str) -> Result<(), Box<dyn std::error::Error>> {
    let lr = lookup_isbn_data(ctx, input).await?;
    display_paper(ctx, &lr.paper, lr.bibtex.as_deref());
    Ok(())
}

/// Fetch BibTeX from a PhilPapers rec URL and return structured data.
async fn lookup_philpapers_url_data(ctx: &Context, url: &str) -> Result<LookupResult, Box<dyn std::error::Error>> {
    use crate::api::philpapers;

    let id = philpapers::extract_id(url)
        .ok_or_else(|| format!("Could not extract PhilPapers ID from: {}", url))?;

    let bib_url = philpapers::bib_url(&id);
    let client = ctx.client();
    let bib = client.get(&bib_url).await?;

    let paper = philpapers::parse_bib_entry(&bib);
    Ok(LookupResult { paper, bibtex: Some(bib) })
}

/// Fetch BibTeX from a PhilPapers rec URL and display it.
async fn lookup_philpapers_url(ctx: &Context, url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let lr = lookup_philpapers_url_data(ctx, url).await?;
    display_paper(ctx, &lr.paper, lr.bibtex.as_deref());
    Ok(())
}

/// Fetch BibTeX from a DBLP URL and display it.
async fn lookup_dblp_url(ctx: &Context, url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let lr = lookup_dblp_url_data(ctx, url).await?;
    if let Some(ref bib) = lr.bibtex {
        println!("{}", bib);
        ctx.handle_bib(bib);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_s2_api_id_prefers_s2_id() {
        let p = PaperResult {
            s2_id: Some("abc123".into()),
            doi: Some("10.1234/test".into()),
            arxiv_id: Some("2006.11239".into()),
            ..Default::default()
        };
        assert_eq!(s2_api_id(&p), Some("abc123".into()));
    }

    #[test]
    fn test_s2_api_id_falls_back_to_doi() {
        let p = PaperResult {
            doi: Some("10.1234/test".into()),
            arxiv_id: Some("2006.11239".into()),
            ..Default::default()
        };
        assert_eq!(s2_api_id(&p), Some("DOI:10.1234/test".into()));
    }

    #[test]
    fn test_s2_api_id_falls_back_to_arxiv() {
        let p = PaperResult {
            arxiv_id: Some("2006.11239".into()),
            ..Default::default()
        };
        assert_eq!(s2_api_id(&p), Some("ARXIV:2006.11239".into()));
    }

    #[test]
    fn test_s2_api_id_none_when_no_ids() {
        let p = PaperResult {
            title: "No IDs".into(),
            ..Default::default()
        };
        assert_eq!(s2_api_id(&p), None);
    }

    #[test]
    fn test_bfs_visited_set_prevents_cycles() {
        use std::collections::HashSet;

        // Simulate BFS with papers that form a cycle: A -> B -> C -> A
        let papers_hop1 = vec![PaperResult {
            title: "B".into(),
            s2_id: Some("id_b".into()),
            ..Default::default()
        }];
        let papers_hop2 = vec![PaperResult {
            title: "C".into(),
            s2_id: Some("id_c".into()),
            ..Default::default()
        }];
        let papers_hop3 = vec![
            PaperResult {
                title: "A again".into(),
                s2_id: Some("id_a".into()), // cycle back to root
                ..Default::default()
            },
            PaperResult {
                title: "B again".into(),
                s2_id: Some("id_b".into()), // cycle back to hop1
                ..Default::default()
            },
        ];

        let mut visited: HashSet<String> = HashSet::new();
        visited.insert("id_a".into()); // root

        // Hop 1
        let mut frontier = Vec::new();
        for p in &papers_hop1 {
            if let Some(cid) = s2_api_id(p) {
                if visited.insert(cid.clone()) {
                    frontier.push(cid);
                }
            }
        }
        assert_eq!(frontier, vec!["id_b"]);

        // Hop 2
        let mut frontier2 = Vec::new();
        for p in &papers_hop2 {
            if let Some(cid) = s2_api_id(p) {
                if visited.insert(cid.clone()) {
                    frontier2.push(cid);
                }
            }
        }
        assert_eq!(frontier2, vec!["id_c"]);

        // Hop 3: both papers are already visited
        let mut frontier3 = Vec::new();
        for p in &papers_hop3 {
            if let Some(cid) = s2_api_id(p) {
                if visited.insert(cid.clone()) {
                    frontier3.push(cid);
                }
            }
        }
        assert!(frontier3.is_empty(), "cycles should be prevented by visited set");
    }
}
