/// `lit add <input> <bib_file>` -- Fetch BibTeX and append to a .bib file.
///
/// Detects the input type (arXiv, DOI, ISBN), fetches the corresponding
/// BibTeX entry, validates it starts with `@`, appends to the bib file,
/// and prints confirmation with the entry.
///
/// When the input is a free-text search query (not a recognized identifier),
/// searches for the paper, takes the top result, extracts the best available
/// identifier (DOI > arXiv > ISBN), and uses that to fetch BibTeX.
///
/// A URL on an ADR-005 host (Distill, Transformer Circuits, Alignment Forum,
/// LessWrong) stores the page text as an artifact, then writes the entry
/// for the page's DOI when it has one, else a `@misc` entry.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use super::metadata::{arxiv_metadata, Metadata, MetadataSource};
use super::Context;
use crate::api::crossref;
use crate::api::openlibrary;
use crate::api::semantic_scholar as s2_api;
use crate::bibtex;
use crate::db;
use crate::detect::{detect_type, normalize_arxiv, normalize_doi, normalize_isbn, InputType};

/// Result of a successful add operation.
#[derive(Debug)]
pub struct AddResult {
    /// The BibTeX citation key (e.g. "schulman2017ppo").
    pub entry_key: String,
    /// The full BibTeX entry text.
    pub bib_text: String,
    /// True when the citekey was new to the bib file; false when an existing
    /// entry was replaced.
    pub added: bool,
}

impl AddResult {
    /// The `--json` document for a write to `bib_file`.
    pub fn to_json(&self, bib_file: &Path) -> serde_json::Value {
        serde_json::json!({
            "entry_key": self.entry_key,
            "bib_file": bib_file.display().to_string(),
            "added": self.added,
        })
    }
}

/// Fetch BibTeX for a paper, append to a .bib file, and return structured result.
pub async fn run_data(ctx: &Context, input: &str, bib_file: &Path, key: Option<&str>, force: bool) -> Result<AddResult, Box<dyn std::error::Error>> {
    let input_type = detect_type(input);

    // CausalAI tech reports have no DOI/arXiv id and the site's own .bib files
    // are unreliable, so we download the PDF, parse its title page, and ingest
    // it through the shared misc artifact pipeline (PDF + source.yaml + entry).
    if input_type == InputType::Causalai {
        return add_causalai(input, bib_file, key, force);
    }
    if input_type == InputType::Url && crate::api::web::is_supported(input) {
        return add_web(ctx, input, bib_file, key, force).await;
    }

    let client = ctx.client();
    // The arXiv record behind an arXiv entry, indexed after the write.
    let mut arxiv_record: Option<Metadata> = None;

    let bib_text = match input_type {
        InputType::Arxiv => {
            let id = normalize_arxiv(input);
            let meta = arxiv_metadata_with_venue(ctx, &id).await?;
            // CrossRef has inconsistent author ordering vs arXiv, so we prefer our
            // generated entry (correct author order + S2 venue) over CrossRef BibTeX.
            let bib = generate_arxiv_bibtex(&meta.paper, &id);
            arxiv_record = Some(meta);
            bib
        }
        InputType::Doi => {
            let doi = normalize_doi(input);
            let url = crossref::bibtex_url(&doi);
            let bib = client.get(&url).await?;
            // Normalize the CrossRef-generated citekey to our scheme.
            normalize_bibtex_key_from_content(&bib)
        }
        InputType::Isbn => {
            let stripped = normalize_isbn(input);
            let key = db::Db::cache_key("isbn", &stripped);
            let url = crate::api::openlibrary::isbn_url(&stripped);
            let body = client.get_cached(&key, &url, db::TTL_DOI).await?;
            let result = crate::api::openlibrary::parse_isbn(&body)?;
            generate_book_bibtex(&result)
        }
        InputType::Search => {
            let top = super::search::resolve_top(ctx, input).await?;
            resolve_bibtex_from_result(ctx, &top).await?
        }
        InputType::PhilPapersUrl => {
            let id = crate::api::philpapers::extract_id(input)
                .ok_or_else(|| format!("Could not extract PhilPapers ID from: {}", input))?;
            let url = crate::api::philpapers::bib_url(&id);
            let bib = client.get(&url).await?;
            normalize_bibtex_key_from_content(&bib)
        }
        InputType::OpenLibraryUrl => {
            fetch_ol_bibtex(ctx, input).await?
        }
        InputType::Url => {
            let title = fetch_title_from_url(input).await?;
            eprintln!("Extracted title: {}", &title[..title.len().min(80)]);
            let top = super::search::resolve_top(ctx, &title).await?;
            resolve_bibtex_from_result(ctx, &top).await?
        }
        _ => {
            return Err(format!(
                "Cannot fetch BibTeX for: {}\nProvide an arXiv ID, DOI, ISBN, or a search query",
                input
            )
            .into());
        }
    };

    let bib_text = bib_text.trim().to_string();
    if !bib_text.starts_with('@') {
        return Err("Failed to fetch BibTeX".into());
    }

    let bib_text = if let Some(k) = key {
        replace_bib_key(&bib_text, k)
    } else {
        bib_text
    };

    // upsert_to_file returns the sanitized text as written, so the printed
    // and JSON-emitted entry always matches the file.
    let written = bibtex::upsert_to_file(bib_file, &bib_text, force)?;

    // Opportunistic index
    match input_type {
        InputType::Arxiv => {
            if let Some(meta) = &arxiv_record {
                super::try_upsert(ctx, &meta.paper, meta.source.id());
            }
        }
        InputType::Doi => {
            let doi = normalize_doi(input);
            let key = db::Db::cache_key("doi", &doi);
            let url = crate::api::crossref::doi_url(&doi);
            if let Ok(body) = client.get_cached(&key, &url, db::TTL_DOI).await {
                if let Ok(result) = crate::api::crossref::parse_doi(&body) {
                    super::try_upsert(ctx, &result, "crossref");
                }
            }
        }
        _ => {}
    }

    let entry_key = bibtex::extract_entry_key(&written.text).unwrap_or_else(|| "unknown".to_string());

    Ok(AddResult { entry_key, bib_text: written.text, added: written.added })
}

/// Run `lit add`: the human report, or with `--json` one document from
/// `AddResult::to_json`.
pub async fn run(ctx: &Context, input: &str, bib_file: &Path, key: Option<&str>, force: bool) -> Result<(), Box<dyn std::error::Error>> {
    let result = run_data(ctx, input, bib_file, key, force).await?;
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&result.to_json(bib_file))?);
    } else {
        println!("Added {} to {}", result.entry_key, bib_file.display());
        println!("{}", result.bib_text);
    }
    Ok(())
}

/// Ingest a CausalAI Lab technical report: download the PDF, parse its title
/// page for metadata, and write a `@misc` entry plus the `etc/pdf/` artifact
/// (PDF + source.yaml + extracted text) through the shared misc pipeline.
fn add_causalai(input: &str, bib_file: &Path, key: Option<&str>, force: bool) -> Result<AddResult, Box<dyn std::error::Error>> {
    use crate::api::causalai;

    let id = crate::detect::normalize_causalai(input)
        .ok_or_else(|| format!("could not parse a report number from: {}", input))?;
    let (meta, bytes) = causalai::fetch(&id)?;

    let citekey = match key {
        Some(k) => k.to_string(),
        None => crate::citekey::generate(&meta.authors, &meta.year, &meta.title),
    };
    let params = super::misc::MiscParams {
        citekey,
        title: meta.title.clone(),
        authors: meta.authors.clone(),
        year: meta.year.clone(),
        howpublished: Some(format!("\\url{{{}}}", causalai::pdf_url(&id))),
        note: Some(format!(
            "Technical Report {}, Causal Artificial Intelligence Lab, Columbia University",
            meta.number
        )),
        url: None,
    };

    // Hand the already-downloaded bytes to the shared misc pipeline via a temp
    // file, so the artifact is written without a second download.
    let tmp = std::env::temp_dir().join(format!("lit_causalai_add_{}.pdf", std::process::id()));
    std::fs::write(&tmp, &bytes)?;
    let tmp_str = tmp.to_str().ok_or("temp path is not valid UTF-8")?.to_string();
    let pdf_root = super::read::find_pdf_base()?;
    let result = super::misc::run_pdf_data(&params, bib_file, &tmp_str, force, &pdf_root);
    let _ = std::fs::remove_file(&tmp);
    result
}

/// Add a page from an ADR-005 host: store or reuse its `etc/pdf/` artifact,
/// then take the entry from the page's DOI when it has one (under the
/// artifact's citekey unless `key` is given), else write `@misc`.
async fn add_web(ctx: &Context, input: &str, bib_file: &Path, key: Option<&str>, force: bool) -> Result<AddResult, Box<dyn std::error::Error>> {
    use super::web::BibSource;

    let artifact = super::web::ensure(&ctx.client(), &super::read::find_pdf_base()?, input, key, ctx.no_cache).await?;
    match artifact.bib_source(key) {
        BibSource::Doi(doi) => Box::pin(run_data(ctx, &doi, bib_file, Some(key.unwrap_or(&artifact.citekey)), force)).await,
        BibSource::Misc(params) => super::misc::run_data(&params, bib_file, force),
    }
}

/// Normalize an OL author name to "First Last" display order for citekey generation.
///
/// OL sometimes returns `name` in "Last, First" format (e.g. "Ezekiel, Mordecai").
/// Converting to "First Last" ensures `extract_last_name` picks the surname correctly.
fn normalize_ol_author(name: &str) -> String {
    if let Some(comma_pos) = name.find(',') {
        let last = name[..comma_pos].trim();
        let rest = name[comma_pos + 1..].trim();
        if rest.is_empty() {
            last.to_string()
        } else {
            format!("{} {}", rest, last)
        }
    } else {
        name.to_string()
    }
}

/// Fetch an Open Library works or books URL and return BibTeX for the book.
async fn fetch_ol_bibtex(ctx: &Context, url: &str) -> Result<String, Box<dyn std::error::Error>> {
    let parts = openlibrary::parse_ol_url(url)
        .ok_or_else(|| format!("Could not parse Open Library URL: {}", url))?;
    let client = ctx.client();

    let (title, publisher, year, author_keys) = match parts.kind {
        openlibrary::OlKind::Books => {
            let edition_url = openlibrary::edition_url(&parts.id);
            let body = client.get(&edition_url).await?;
            let ed = openlibrary::parse_edition(&body)?;
            if !ed.author_keys.is_empty() {
                (ed.title, ed.publisher, ed.year, ed.author_keys)
            } else {
                let edition: serde_json::Value = serde_json::from_str(&body)?;
                let work_keys = edition["works"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|w| w["key"].as_str().map(str::to_string))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let mut fallback_authors = Vec::new();
                for work_key in work_keys {
                    let work_id = work_key.trim_start_matches("/works/");
                    let work_body = client.get(&openlibrary::work_url(work_id)).await?;
                    fallback_authors.extend(openlibrary::parse_work(&work_body)?.author_keys);
                }
                (ed.title, ed.publisher, ed.year, fallback_authors)
            }
        }
        openlibrary::OlKind::Works => {
            let work_url = openlibrary::work_url(&parts.id);
            let editions_url = openlibrary::work_editions_url(&parts.id);
            let (work_body, editions_body) = tokio::join!(
                client.get(&work_url),
                client.get(&editions_url),
            );
            let work = openlibrary::parse_work(&work_body?)?;
            let editions = openlibrary::parse_editions_list(&editions_body?)?;
            let earliest = editions
                .into_iter()
                .find(|e| e.year != "?")
                .unwrap_or(openlibrary::EditionResult {
                    title: work.title.clone(),
                    publisher: None,
                    year: "?".to_string(),
                    author_keys: work.author_keys.clone(),
                    isbn: None,
                });
            let author_keys = if !earliest.author_keys.is_empty() {
                earliest.author_keys
            } else {
                work.author_keys
            };
            (work.title, earliest.publisher, earliest.year, author_keys)
        }
    };

    // Resolve every author, and fail rather than emitting a misleading
    // `Unknown` author when the metadata endpoint is unavailable.
    let mut authors = Vec::with_capacity(author_keys.len());
    for key in &author_keys {
        let body = client.get(&openlibrary::author_url(key)).await?;
        let raw = openlibrary::parse_author(&body)?;
        authors.push(normalize_ol_author(&raw));
    }
    if authors.is_empty() {
        return Err("Open Library edition has no resolvable author names".into());
    }

    let result = crate::api::PaperResult {
        title,
        authors,
        year,
        venue: publisher,
        ..Default::default()
    };
    Ok(generate_book_bibtex(&result))
}

/// Download a PDF from a URL and extract its title using `pdftotext`.
///
/// Downloads the bytes to a temp file, runs `pdftotext <file> -`, and returns
/// the first line of extracted text with more than 15 characters.
pub async fn fetch_title_from_url(url: &str) -> Result<String, Box<dyn std::error::Error>> {
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .timeout(Duration::from_secs(60))
        .build()?;

    let bytes = client.get(url).send().await?.bytes().await?;

    let tmp_path = std::env::temp_dir().join("lit_url_download.pdf");
    std::fs::write(&tmp_path, &bytes)?;

    let output = Command::new("pdftotext")
        .arg(&tmp_path)
        .arg("-")
        .output();

    // Clean up temp file regardless of pdftotext result
    let _ = std::fs::remove_file(&tmp_path);

    let output = output?;
    let text = String::from_utf8_lossy(&output.stdout);

    let title = text
        .lines()
        .map(str::trim)
        .find(|line| line.len() > 15)
        .ok_or("Could not extract title from PDF: no line with >15 chars found")?
        .to_string();

    Ok(title)
}

/// Metadata for an arXiv paper (arXiv API or its fallbacks) with a publication
/// venue: the fallback record's own, or Semantic Scholar's when the arXiv API
/// answered. Preprint servers are dropped as venues.
async fn arxiv_metadata_with_venue(ctx: &Context, id: &str) -> Result<Metadata, Box<dyn std::error::Error>> {
    let client = ctx.client();
    let mut meta = arxiv_metadata(&client, id).await?;
    // Only the arXiv API lacks venues; after a fallback, S2 was already asked.
    if meta.source == MetadataSource::Arxiv {
        let s2_key = db::Db::cache_key("s2_paper", id);
        let s2_url = s2_api::paper_url(&format!("arXiv:{}", id));
        if let Ok(body) = client.get_cached(&s2_key, &s2_url, db::TTL_DOI).await {
            if let Ok(s2) = s2_api::parse_paper(&body) {
                meta.paper.venue = s2.venue;
            }
        }
    }
    meta.paper.venue = meta.paper.venue.filter(|v| !is_junk_venue(v));
    Ok(meta)
}

/// Generate BibTeX for an arXiv paper from a PaperResult.
///
/// If `result.venue` is set (from S2), generates `@inproceedings` with `booktitle`.
/// Otherwise generates a minimal `@article` with only arXiv eprint fields.
fn generate_arxiv_bibtex(result: &crate::api::PaperResult, arxiv_id: &str) -> String {
    let key = crate::citekey::generate(&result.authors, &result.year, &result.title);
    let author_str = result.authors.join(" and ");

    if let Some(ref venue) = result.venue {
        format!(
            "@inproceedings{{{key},\n  title = {{{title}}},\n  author = {{{authors}}},\n  booktitle = {{{venue}}},\n  year = {{{year}}},\n  eprint = {{{eprint}}},\n  archivePrefix = {{arXiv}},\n}}",
            key = key,
            title = result.title,
            authors = author_str,
            venue = venue,
            year = result.year,
            eprint = arxiv_id,
        )
    } else {
        format!(
            "@article{{{key},\n  title = {{{title}}},\n  author = {{{authors}}},\n  year = {{{year}}},\n  eprint = {{{eprint}}},\n  archivePrefix = {{arXiv}},\n}}",
            key = key,
            title = result.title,
            authors = author_str,
            year = result.year,
            eprint = arxiv_id,
        )
    }
}

/// Resolve BibTeX from a search result by extracting the best identifier.
///
/// Priority: DOI > arXiv > ISBN. Falls back to generating BibTeX directly
/// from the search result metadata if no identifier is available.
async fn resolve_bibtex_from_result(
    ctx: &Context,
    result: &crate::api::PaperResult,
) -> Result<String, Box<dyn std::error::Error>> {
    let client = ctx.client();

    if let Some(ref doi) = result.doi {
        let truncated = crate::format::truncate(&result.title, 60);
        eprintln!("Resolved: {} (DOI:{})", truncated, doi);
        let url = crossref::bibtex_url(doi);
        return client.get(&url).await;
    }

    if let Some(ref arxiv_id) = result.arxiv_id {
        let truncated = crate::format::truncate(&result.title, 60);
        eprintln!("Resolved: {} (arXiv:{})", truncated, arxiv_id);
        let meta = arxiv_metadata(&client, arxiv_id).await?;
        return Ok(generate_arxiv_bibtex(&meta.paper, arxiv_id));
    }

    if let Some(ref isbn) = result.isbn {
        let truncated = crate::format::truncate(&result.title, 60);
        eprintln!("Resolved: {} (ISBN:{})", truncated, isbn);
        let key = db::Db::cache_key("isbn", isbn);
        let url = crate::api::openlibrary::isbn_url(isbn);
        let body = client.get_cached(&key, &url, db::TTL_DOI).await?;
        let parsed = crate::api::openlibrary::parse_isbn(&body)?;
        return Ok(generate_book_bibtex(&parsed));
    }

    // No recognized identifier: generate BibTeX directly from search metadata
    let truncated = crate::format::truncate(&result.title, 60);
    eprintln!("Resolved: {} (no DOI/arXiv/ISBN, using search metadata)", truncated);
    let key = crate::citekey::generate(&result.authors, &result.year, &result.title);
    let author_str = result.authors.join(" and ");
    let mut fields = vec![
        format!("  title = {{{}}}", result.title),
        format!("  author = {{{}}}", author_str),
        format!("  year = {{{}}}", result.year),
    ];
    if let Some(ref venue) = result.venue {
        fields.push(format!("  booktitle = {{{}}}", venue));
    }
    Ok(format!("@inproceedings{{{},\n{}\n}}", key, fields.join(",\n")))
}

/// Generate BibTeX for a book from a PaperResult.
fn generate_book_bibtex(result: &crate::api::PaperResult) -> String {
    let key = crate::citekey::generate(&result.authors, &result.year, &result.title);
    let author_str = result.authors.join(" and ");

    let mut fields = vec![
        format!("  title = {{{}}}", result.title),
        format!("  author = {{{}}}", author_str),
        format!("  year = {{{}}}", result.year),
    ];

    if let Some(ref venue) = result.venue {
        fields.push(format!("  publisher = {{{}}}", venue));
    }

    if let Some(ref isbn) = result.isbn {
        fields.push(format!("  isbn = {{{}}}", isbn));
    }

    format!("@book{{{},\n{}\n}}", key, fields.join(",\n"))
}

/// Replace the citekey in a CrossRef BibTeX string, inferring metadata from the entry itself.
///
/// CrossRef generates keys like `Andonian_2022` (first author by their ordering, which
/// may differ from arXiv). Used for pure DOI input where arXiv metadata isn't available.
fn normalize_bibtex_key_from_content(bib: &str) -> String {
    let entries = crate::bibtex::parse_bib_file(bib);
    if let Some(entry) = entries.first() {
        let title = entry.get_field("title").unwrap_or("").to_string();
        let year = entry.get_field("year").unwrap_or("?").to_string();
        // CrossRef author field is "Last, First and Last2, First2" — extract first last name.
        let author_raw = entry.get_field("author").unwrap_or("");
        let first_last = author_raw
            .split(" and ")
            .next()
            .and_then(|a| a.split(',').next())
            .map(|s| crate::api::extract_last_name(s.trim()).to_string())
            .unwrap_or_default();
        let authors = if first_last.is_empty() { vec![] } else { vec![first_last] };
        let new_key = crate::citekey::generate(&authors, &year, &title);
        return replace_bib_key(bib, &new_key);
    }
    bib.to_string()
}

/// Returns true if the venue string is a known junk/non-venue from Semantic Scholar.
///
/// S2 returns preprint servers as venue names (e.g. "arXiv.org"), which would
/// cause `generate_arxiv_bibtex` to emit `@inproceedings` with a bogus booktitle.
fn is_junk_venue(v: &str) -> bool {
    let v = v.to_lowercase();
    v.starts_with("arxiv") || v.starts_with("biorxiv")
}

/// Replace the first citekey in a BibTeX string with `new_key`.
fn replace_bib_key(bib: &str, new_key: &str) -> String {
    if let Some(open) = bib.find('{') {
        // Find the end of the old key: first ',' or newline after the '{'
        let after_brace = &bib[open + 1..];
        if let Some(end_offset) = after_brace.find(|c| c == ',' || c == '\n') {
            let rest = &bib[open + 1 + end_offset..]; // from ',' onward
            return format!("{}{{{}{}", &bib[..open], new_key, rest);
        }
    }
    bib.to_string()
}

#[cfg(test)]
mod tests {
    use super::is_junk_venue;

    #[test]
    fn preprint_servers_are_not_venues_under_any_provider_name() {
        // S2 says "arXiv.org"; OpenAlex says "arXiv (Cornell University)".
        for v in ["arXiv.org", "arxiv", "arXiv (Cornell University)", "bioRxiv"] {
            assert!(is_junk_venue(v), "{}", v);
        }
        assert!(!is_junk_venue("NeurIPS"));
    }
}
