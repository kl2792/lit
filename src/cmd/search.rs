use super::Context;
use crate::api::PaperResult;
use crate::db;
use crate::format;
use crate::http;

/// Search source selection.
#[derive(Clone, Copy, PartialEq)]
pub enum Source {
    Oa,
    Ss,
    Cr,
    Dblp,
    Book,
    PhilPapers,
    /// Columbia Clio catalog (live JSON API — no sync required)
    Clio,
    All,
}

/// A searchable API backend. Each variant knows its cache prefix, URL builder, and parser.
struct Backend {
    name: &'static str,
    prefix: &'static str,
    url: fn(&str, usize) -> String,
    parse: fn(&str) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>>,
}

const BACKENDS: &[Backend] = &[
    Backend {
        name: "Semantic Scholar",
        prefix: "ss",
        url: crate::api::semantic_scholar::search_url,
        parse: crate::api::semantic_scholar::parse_search,
    },
    Backend {
        name: "OpenAlex",
        prefix: "oa",
        url: crate::api::openalex::search_url,
        parse: crate::api::openalex::parse_search,
    },
    Backend {
        name: "CrossRef",
        prefix: "cr",
        url: crate::api::crossref::search_url,
        parse: crate::api::crossref::parse_search,
    },
    Backend {
        name: "DBLP",
        prefix: "dblp",
        url: crate::api::dblp::search_url,
        parse: crate::api::dblp::parse_search,
    },
    Backend {
        name: "OpenLibrary",
        prefix: "book",
        url: crate::api::openlibrary::search_url,
        parse: crate::api::openlibrary::parse_search,
    },
    Backend {
        name: "PhilPapers",
        prefix: "philpapers",
        url: crate::api::philpapers::search_url,
        parse: crate::api::philpapers::parse_search,
    },
    Backend {
        name: "Clio",
        prefix: "clio",
        url: crate::api::clio::search_url,
        parse: crate::api::clio::parse_search,
    },
];

fn backend_for(source: Source) -> &'static Backend {
    match source {
        Source::Ss => &BACKENDS[0],
        Source::Oa => &BACKENDS[1],
        Source::Cr => &BACKENDS[2],
        Source::Dblp => &BACKENDS[3],
        Source::Book => &BACKENDS[4],
        Source::PhilPapers => &BACKENDS[5],
        Source::Clio => &BACKENDS[6],
        Source::All => unreachable!(),
    }
}

/// Default cascade: SS -> OA -> CR -> books -> Clio.
/// Clio is last — it covers books/physical items the others miss, but has lower relevance ranking.
const CASCADE: &[Source] = &[Source::Ss, Source::Oa, Source::Cr, Source::Book, Source::Clio];

/// Minimum relevance score (0.0-1.0) for the top result to be considered "good enough"
/// to stop the cascade. Below this threshold, we try the next source.
const RELEVANCE_THRESHOLD: f64 = 0.3;

/// Run a search and return structured results.
pub async fn run_data(
    ctx: &Context,
    query: &str,
    limit: usize,
    source: Option<Source>,
) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>> {
    if query.is_empty() {
        return Err("Usage: lit search <query> [-l N] [-s source]".into());
    }

    let client = ctx.client();
    let results = match source {
        Some(Source::All) => fetch_all(&client, ctx, query, limit).await,
        Some(src) => fetch_single(&client, ctx, query, limit, src).await,
        None => fetch_cascade(&client, ctx, query, limit).await,
    };

    let mut results = rerank(results, query);
    results.truncate(limit);
    Ok(results)
}

/// Run a search with optional source selection.
pub async fn run(
    ctx: &Context,
    query: &str,
    limit: usize,
    source: Option<Source>,
) -> Result<(), Box<dyn std::error::Error>> {
    let results = run_data(ctx, query, limit, source).await?;
    print_results(ctx, &results);
    Ok(())
}

/// Search and return the top result. Used by `lit add` for query resolution.
pub async fn resolve_top(
    ctx: &Context,
    query: &str,
) -> Result<PaperResult, Box<dyn std::error::Error>> {
    let client = ctx.client();
    let results = fetch_cascade(&client, ctx, query, 5).await;
    let results = rerank(results, query);
    results
        .into_iter()
        .next()
        .ok_or_else(|| format!("No results found for: {}", query).into())
}

/// Fetch from a single backend.
async fn fetch_single(
    client: &http::Client,
    ctx: &Context,
    query: &str,
    limit: usize,
    source: Source,
) -> Vec<PaperResult> {
    let b = backend_for(source);
    fetch_backend(client, ctx, b, query, limit).await
}

/// Cascade through backends, stopping when we get a good-enough result.
async fn fetch_cascade(
    client: &http::Client,
    ctx: &Context,
    query: &str,
    limit: usize,
) -> Vec<PaperResult> {
    let query_tokens = tokenize(query);
    let mut all_results: Vec<PaperResult> = Vec::new();

    for &src in CASCADE {
        let b = backend_for(src);
        let results = fetch_backend(client, ctx, b, query, limit).await;

        if ctx.verbose {
            format::info(&format!("Trying {}...", b.name));
        }

        if results.is_empty() {
            continue;
        }

        // Check if the top result is relevant enough to stop
        let top_score = if is_book_review(&results[0].title) {
            0.0 // Book review false-positive: keep cascading
        } else {
            relevance_score(&results[0], &query_tokens)
        };
        if top_score >= RELEVANCE_THRESHOLD {
            if ctx.verbose {
                format::info(&format!(
                    "{}: top result relevance {:.0}%",
                    b.name,
                    top_score * 100.0
                ));
            }
            return results;
        }

        // Low relevance -- collect and try next source
        if ctx.verbose {
            format::warn(&format!(
                "{}: top result relevance {:.0}% (below threshold), trying next source...",
                b.name,
                top_score * 100.0
            ));
        }
        eprintln!(
            "note: {} results may be imprecise, trying other sources",
            b.name
        );
        all_results.extend(results);
    }

    // All sources had low relevance -- return everything we collected
    all_results
}

/// Fetch from all backends concurrently and merge (includes Clio).
async fn fetch_all(
    client: &http::Client,
    ctx: &Context,
    query: &str,
    limit: usize,
) -> Vec<PaperResult> {
    let (r0, r1, r2, r3, r4, r5, r6) = tokio::join!(
        fetch_backend(client, ctx, &BACKENDS[0], query, limit),
        fetch_backend(client, ctx, &BACKENDS[1], query, limit),
        fetch_backend(client, ctx, &BACKENDS[2], query, limit),
        fetch_backend(client, ctx, &BACKENDS[3], query, limit),
        fetch_backend(client, ctx, &BACKENDS[4], query, limit),
        fetch_backend(client, ctx, &BACKENDS[5], query, limit),
        fetch_backend(client, ctx, &BACKENDS[6], query, limit),
    );
    let mut all = Vec::new();
    all.extend(r0);
    all.extend(r1);
    all.extend(r2);
    all.extend(r3);
    all.extend(r4);
    all.extend(r5);
    all.extend(r6);
    all
}

/// Fetch and parse results from a single backend.
async fn fetch_backend(
    client: &http::Client,
    ctx: &Context,
    backend: &Backend,
    query: &str,
    limit: usize,
) -> Vec<PaperResult> {
    let key = db::Db::cache_key(backend.prefix, query);
    let url = (backend.url)(query, limit);
    match client.get_cached(&key, &url, db::TTL_SEARCH).await {
        Ok(body) => match (backend.parse)(&body) {
            Ok(results) => results,
            Err(e) => {
                print_cascade_error(ctx, backend.name, &e);
                Vec::new()
            }
        },
        Err(e) => {
            print_cascade_error(ctx, backend.name, &e);
            Vec::new()
        }
    }
}

// --- Relevance scoring ---

/// Tokenize a string into lowercase alphanumeric words.
fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// Score how relevant a paper result is to the query (0.0 - 1.0).
/// Uses Jaccard-like overlap of query tokens with title + author tokens.
fn relevance_score(paper: &PaperResult, query_tokens: &[String]) -> f64 {
    if query_tokens.is_empty() {
        return 0.0;
    }

    let mut paper_tokens: Vec<String> = tokenize(&paper.title);
    for author in &paper.authors {
        paper_tokens.extend(tokenize(author));
    }

    let matches = query_tokens
        .iter()
        .filter(|qt| paper_tokens.iter().any(|pt| pt == *qt))
        .count();

    matches as f64 / query_tokens.len() as f64
}

/// Rerank results by relevance to the query, then by citation count.
fn rerank(mut results: Vec<PaperResult>, query: &str) -> Vec<PaperResult> {
    if results.is_empty() {
        return results;
    }

    let query_tokens = tokenize(query);

    // Deduplicate by DOI (keep first occurrence, which may have more metadata)
    let mut seen_dois = std::collections::HashSet::new();
    results.retain(|p| {
        if let Some(ref doi) = p.doi {
            let key = doi.to_lowercase();
            seen_dois.insert(key)
        } else {
            true
        }
    });

    // Sort by: relevance score (desc), then citation count (desc)
    results.sort_by(|a, b| {
        let sa = relevance_score(a, &query_tokens);
        let sb = relevance_score(b, &query_tokens);
        sb.partial_cmp(&sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let ca = a.citations.unwrap_or(0);
                let cb = b.citations.unwrap_or(0);
                cb.cmp(&ca)
            })
    });

    results
}

// --- Output ---

/// Print search results in JSON, concise, or verbose format.
fn print_results(ctx: &Context, results: &[PaperResult]) {
    if results.is_empty() {
        println!("No results found");
        return;
    }

    if ctx.json {
        let arr: Vec<serde_json::Value> = results
            .iter()
            .map(|p| super::paper_to_json(p))
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr).unwrap());
        return;
    }

    for (i, p) in results.iter().enumerate() {
        let rank = i + 1;
        let author = p.authors.first().map(|s| s.as_str()).unwrap_or("?");

        if ctx.verbose {
            let cites = p.citations.map(|c| c.to_string()).unwrap_or_default();
            println!("{}. {} ({})", rank, p.title, p.year);
            println!("   Author: {} | Citations: {}", author, cites);
            if let Some(ref arxiv) = p.arxiv_id {
                println!("   arXiv: {}", arxiv);
            }
            if let Some(ref doi) = p.doi {
                println!("   DOI: {}", doi);
            }
            if let Some(ref venue) = p.venue {
                println!("   Venue: {}", venue);
            }
            if let Some(ref isbn) = p.isbn {
                println!("   ISBN: {}", isbn);
            }
            println!();
        } else {
            let id_str = if let Some(ref arxiv) = p.arxiv_id {
                format!("arXiv:{}", arxiv)
            } else if let Some(ref doi) = p.doi {
                format!("DOI:{}", doi)
            } else if let Some(ref isbn) = p.isbn {
                format!("ISBN:{}", isbn)
            } else {
                String::new()
            };
            let title = format::truncate(&p.title, 70);
            println!("{}. {} {} | {} | {}", rank, author, p.year, title, id_str);
        }
    }
}

/// Print a diagnostic message to stderr when an API source fails in the cascade.
/// Returns true if the title looks like a book review that should not stop the cascade.
fn is_book_review(title: &str) -> bool {
    let lower = title.to_lowercase();
    lower.contains("book review") || lower.starts_with("review of ") || lower.contains("reviews:")
}

fn print_cascade_error(ctx: &Context, source: &str, error: &dyn std::fmt::Display) {
    if ctx.verbose {
        eprintln!("note: {} unavailable: {}", source, error);
    } else {
        eprintln!("note: {} unavailable", source);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_basic() {
        assert_eq!(tokenize("Attention Is All You Need"), vec!["attention", "is", "all", "you", "need"]);
    }

    #[test]
    fn tokenize_with_punctuation() {
        assert_eq!(tokenize("SHAP: A Unified Approach"), vec!["shap", "a", "unified", "approach"]);
    }

    #[test]
    fn relevance_exact_title_match() {
        let paper = PaperResult {
            title: "Attention Is All You Need".to_string(),
            authors: vec!["Ashish Vaswani".to_string()],
            ..Default::default()
        };
        let query_tokens = tokenize("attention is all you need");
        let score = relevance_score(&paper, &query_tokens);
        assert!(score >= 0.9, "exact title match should score high: {}", score);
    }

    #[test]
    fn relevance_author_match_boosts() {
        let paper = PaperResult {
            title: "A Unified Approach to Interpreting Model Predictions".to_string(),
            authors: vec!["Scott M. Lundberg".to_string()],
            ..Default::default()
        };
        let query_tokens = tokenize("SHAP lundberg unified approach");
        let score = relevance_score(&paper, &query_tokens);
        // "lundberg", "unified", "approach" match (3/4 = 0.75). "shap" doesn't match title/author.
        assert!(score >= 0.5, "author+title partial match: {}", score);
    }

    #[test]
    fn relevance_no_match() {
        let paper = PaperResult {
            title: "Schooling incentives and child labor".to_string(),
            authors: vec!["Eric Edmonds".to_string()],
            ..Default::default()
        };
        let query_tokens = tokenize("mesnard counterfactual credit assignment");
        let score = relevance_score(&paper, &query_tokens);
        assert!(score < 0.2, "unrelated paper should score low: {}", score);
    }

    #[test]
    fn rerank_puts_relevant_first() {
        let good = PaperResult {
            title: "Proximal Policy Optimization Algorithms".to_string(),
            authors: vec!["John Schulman".to_string()],
            citations: Some(5000),
            ..Default::default()
        };
        let bad = PaperResult {
            title: "Crystallization Process Design".to_string(),
            authors: vec!["Someone Else".to_string()],
            citations: Some(10),
            ..Default::default()
        };
        let results = rerank(vec![bad, good], "PPO proximal policy optimization schulman");
        assert_eq!(results[0].title, "Proximal Policy Optimization Algorithms");
    }

    #[test]
    fn is_book_review_matches_patterns() {
        assert!(is_book_review("Book Reviews: Statistics for Psychologists by William L. Hays"));
        assert!(is_book_review("book review of something"));
        assert!(is_book_review("Reviews: Analysis of Variance"));
        assert!(is_book_review("review of statistical methods"));
        assert!(!is_book_review("Statistical methods for research workers"));
        assert!(!is_book_review("Reviewing the evidence for X"));
    }

    #[test]
    fn rerank_deduplicates_by_doi() {
        let a = PaperResult {
            title: "Paper A".to_string(),
            doi: Some("10.1234/test".to_string()),
            ..Default::default()
        };
        let b = PaperResult {
            title: "Paper A".to_string(),
            doi: Some("10.1234/test".to_string()),
            ..Default::default()
        };
        let results = rerank(vec![a, b], "paper");
        assert_eq!(results.len(), 1);
    }
}
