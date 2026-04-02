/// `lit add <input> <bib_file>` -- Fetch BibTeX and append to a .bib file.
///
/// Detects the input type (arXiv, DOI, ISBN), fetches the corresponding
/// BibTeX entry, validates it starts with `@`, appends to the bib file,
/// and prints confirmation with the entry.
///
/// When the input is a free-text search query (not a recognized identifier),
/// searches for the paper, takes the top result, extracts the best available
/// identifier (DOI > arXiv > ISBN), and uses that to fetch BibTeX.

use std::path::Path;

use super::Context;
use crate::api::crossref;
use crate::api::semantic_scholar as s2_api;
use crate::bibtex;
use crate::db;
use crate::detect::{detect_type, normalize_arxiv, normalize_doi, normalize_isbn, InputType};

/// Result of a successful add operation.
pub struct AddResult {
    /// The BibTeX citation key (e.g. "schulman2017ppo").
    pub entry_key: String,
    /// The full BibTeX entry text.
    pub bib_text: String,
}

/// Fetch BibTeX for a paper, append to a .bib file, and return structured result.
pub async fn run_data(ctx: &Context, input: &str, bib_file: &Path) -> Result<AddResult, Box<dyn std::error::Error>> {
    let input_type = detect_type(input);
    let client = ctx.client();

    let bib_text = match input_type {
        InputType::Arxiv => {
            let id = normalize_arxiv(input);
            let arxiv_key = db::Db::cache_key("arxiv", &id);
            let arxiv_url = crate::api::arxiv::query_url(&id);
            let s2_key = db::Db::cache_key("s2_paper", &id);
            let s2_url = s2_api::paper_url(&format!("arXiv:{}", id));

            let (arxiv_body, s2_body) = tokio::join!(
                client.get_cached(&arxiv_key, &arxiv_url, db::TTL_DOI),
                client.get_cached(&s2_key, &s2_url, db::TTL_DOI),
            );

            let arxiv_body = arxiv_body?;
            let mut result = crate::api::arxiv::parse_entry(&arxiv_body)?;

            if let Ok(body) = s2_body {
                if let Ok(s2) = s2_api::parse_paper(&body) {
                    if result.venue.is_none() { result.venue = s2.venue; }
                }
            }

            // S2 venue is now used by generate_arxiv_bibtex; no need to hit CrossRef.
            // CrossRef has inconsistent author ordering vs arXiv, so we prefer our
            // generated entry (correct author order + S2 venue) over CrossRef BibTeX.
            generate_arxiv_bibtex(&result, &id)
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

    bibtex::upsert_to_file(bib_file, &bib_text)?;

    // Opportunistic index
    match input_type {
        InputType::Arxiv => {
            let id = normalize_arxiv(input);
            let key = db::Db::cache_key("arxiv", &id);
            let url = crate::api::arxiv::query_url(&id);
            if let Ok(body) = client.get_cached(&key, &url, db::TTL_DOI).await {
                if let Ok(result) = crate::api::arxiv::parse_entry(&body) {
                    super::try_upsert(ctx, &result, "arxiv");
                }
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

    let entry_key = bibtex::extract_entry_key(&bib_text).unwrap_or_else(|| "unknown".to_string());

    Ok(AddResult { entry_key, bib_text })
}

pub async fn run(ctx: &Context, input: &str, bib_file: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let result = run_data(ctx, input, bib_file).await?;
    println!("Added {} to {}", result.entry_key, bib_file.display());
    println!("{}", result.bib_text);
    Ok(())
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
        let key = db::Db::cache_key("arxiv", arxiv_id);
        let url = crate::api::arxiv::query_url(arxiv_id);
        let body = client.get_cached(&key, &url, db::TTL_DOI).await?;
        let parsed = crate::api::arxiv::parse_entry(&body)?;
        return Ok(generate_arxiv_bibtex(&parsed, arxiv_id));
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
    let author_str = result
        .authors
        .first()
        .cloned()
        .unwrap_or_else(|| "Unknown".to_string());

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
