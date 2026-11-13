use super::Context;
use crate::api::PaperResult;
use crate::db;
use crate::format;
use crate::http;
use futures_util::future::join_all;
use std::future::Future;
use std::time::Duration;

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
    Arxiv,
    All,
}

/// A searchable API backend.
struct Backend {
    /// Name in notes and errors.
    name: &'static str,
    /// Cache key prefix.
    prefix: &'static str,
    /// Value of `source` in `--json` output; `s2` and `openalex` match `lit refs` and `lit cites`.
    label: &'static str,
    url: fn(&str, usize) -> String,
    parse: fn(&str) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>>,
}

const SEMANTIC_SCHOLAR: Backend = Backend {
    name: "Semantic Scholar",
    prefix: "ss",
    label: "s2",
    url: crate::api::semantic_scholar::search_url,
    parse: crate::api::semantic_scholar::parse_search,
};
const OPENALEX: Backend = Backend {
    name: "OpenAlex",
    prefix: "oa",
    label: "openalex",
    url: crate::api::openalex::search_url,
    parse: crate::api::openalex::parse_search,
};
const ARXIV: Backend = Backend {
    name: "arXiv",
    prefix: "arxiv",
    label: "arxiv",
    url: crate::api::arxiv::search_url,
    parse: crate::api::arxiv::parse_search,
};
const CROSSREF: Backend = Backend {
    name: "CrossRef",
    prefix: "cr",
    label: "crossref",
    url: crate::api::crossref::search_url,
    parse: crate::api::crossref::parse_search,
};
const DBLP: Backend = Backend {
    name: "DBLP",
    prefix: "dblp",
    label: "dblp",
    url: crate::api::dblp::search_url,
    parse: crate::api::dblp::parse_search,
};
const OPENLIBRARY: Backend = Backend {
    name: "OpenLibrary",
    prefix: "book",
    label: "openlibrary",
    url: crate::api::openlibrary::search_url,
    parse: crate::api::openlibrary::parse_search,
};
const PHILPAPERS: Backend = Backend {
    name: "PhilPapers",
    prefix: "philpapers",
    label: "philpapers",
    url: crate::api::philpapers::search_url,
    parse: crate::api::philpapers::parse_search,
};
const CLIO: Backend = Backend {
    name: "Clio",
    prefix: "clio",
    label: "clio",
    url: crate::api::clio::search_url,
    parse: crate::api::clio::parse_search,
};

/// Every backend, queried together by `--source all`.
const BACKENDS: &[&Backend] = &[&SEMANTIC_SCHOLAR, &OPENALEX, &ARXIV, &CROSSREF, &DBLP, &OPENLIBRARY, &PHILPAPERS, &CLIO];

fn backend_for(source: Source) -> &'static Backend {
    match source {
        Source::Ss => &SEMANTIC_SCHOLAR,
        Source::Oa => &OPENALEX,
        Source::Arxiv => &ARXIV,
        Source::Cr => &CROSSREF,
        Source::Dblp => &DBLP,
        Source::Book => &OPENLIBRARY,
        Source::PhilPapers => &PHILPAPERS,
        Source::Clio => &CLIO,
        Source::All => unreachable!(),
    }
}

/// The default cascade (ADR-011), one tier at a time; a tier's backends are
/// queried together and their results merged. Semantic Scholar, OpenAlex and
/// arXiv rank title and abstract matches, so they come first; the catalogs
/// match titles only and are reached when no earlier tier has a good match.
/// Clio is last: it covers books the others miss but ranks loosely.
const CASCADE: &[&[&Backend]] = &[
    &[&SEMANTIC_SCHOLAR],
    &[&OPENALEX, &ARXIV],
    &[&CROSSREF],
    &[&OPENLIBRARY],
    &[&CLIO],
];

/// Share of the query's topic words a result must contain in its title,
/// abstract or authors to be on topic. On-topic results rank first, and a
/// tier with one ends the cascade.
const ON_TOPIC_SHARE: f64 = 0.5;

/// Fewest results asked of each backend, so local ranking chooses the
/// top `--limit` from a pool larger than a short limit.
const MIN_POOL: usize = 25;

/// One search result and the label of the backend that returned it.
#[derive(Debug, Clone)]
pub struct Hit {
    pub paper: PaperResult,
    pub source: &'static str,
}

/// What querying some backends produced.
#[derive(Debug, Default)]
struct Outcome {
    hits: Vec<Hit>,
    /// Backends that failed, by name, with the error each reported.
    failures: Vec<(&'static str, String)>,
    /// Backends that answered with no results, by name.
    empty: Vec<&'static str>,
}

/// Run a search and return ranked results.
///
/// Fails when no result was found and some source failed, naming every
/// source consulted, so an outage never reads as zero hits.
pub async fn run_data(
    ctx: &Context,
    query: &str,
    limit: usize,
    source: Option<Source>,
) -> Result<Vec<Hit>, Box<dyn std::error::Error>> {
    if query.is_empty() {
        return Err("Usage: lit search <query> [-l N] [-s source]".into());
    }

    let client = ctx.client();
    let pool = limit.max(MIN_POOL);
    let outcome = match source {
        None => {
            let fetch = |b| fetch_backend(&client, b, query, pool, http::FALLBACK_RETRY_BUDGET);
            cascade(CASCADE, query, fetch).await
        }
        Some(Source::All) => {
            fetch_tier(BACKENDS, |b| fetch_backend(&client, b, query, pool, http::RETRY_BUDGET)).await
        }
        Some(src) => {
            fetch_tier(&[backend_for(src)], |b| fetch_backend(&client, b, query, pool, http::RETRY_BUDGET)).await
        }
    };

    let mut hits = finish(outcome, query)?;
    hits.truncate(limit);
    Ok(hits)
}

/// Run a search with optional source selection.
pub async fn run(
    ctx: &Context,
    query: &str,
    limit: usize,
    source: Option<Source>,
) -> Result<(), Box<dyn std::error::Error>> {
    let hits = run_data(ctx, query, limit, source).await?;
    print_results(ctx, &hits);
    Ok(())
}

/// Search and return the best title match (`best_title_match`). Used by
/// `lit add` for query resolution.
pub async fn resolve_top(
    ctx: &Context,
    query: &str,
) -> Result<PaperResult, Box<dyn std::error::Error>> {
    let hits = run_data(ctx, query, MIN_POOL, None).await?;
    best_title_match(hits, &query_terms(query)).ok_or_else(|| format!("No results found for: {}", query).into())
}

/// Fetch and parse one backend's results for `query`, retrying for at most `budget`.
async fn fetch_backend(
    client: &http::Client,
    backend: &'static Backend,
    query: &str,
    pool: usize,
    budget: Duration,
) -> Result<Vec<PaperResult>, String> {
    let key = db::Db::cache_key(backend.prefix, query);
    let url = (backend.url)(query, pool);
    let body = client
        .get_cached_within(&key, &url, db::TTL_SEARCH, budget)
        .await
        .map_err(|e| e.to_string())?;
    (backend.parse)(&body).map_err(|e| e.to_string())
}

/// Query every backend of `tier` concurrently through `fetch`.
async fn fetch_tier<F, Fut>(tier: &[&'static Backend], mut fetch: F) -> Outcome
where
    F: FnMut(&'static Backend) -> Fut,
    Fut: Future<Output = Result<Vec<PaperResult>, String>>,
{
    let results = join_all(tier.iter().map(|&b| fetch(b))).await;
    tier_outcome(tier, results)
}

/// Sort each backend's reply into hits, failures and empty answers; hits are
/// interleaved by rank, so equally scored results keep their backends' order.
fn tier_outcome(tier: &[&'static Backend], results: Vec<Result<Vec<PaperResult>, String>>) -> Outcome {
    let mut outcome = Outcome::default();
    let mut lists: Vec<std::vec::IntoIter<Hit>> = Vec::new();
    for (b, result) in tier.iter().zip(results) {
        match result {
            Ok(papers) if papers.is_empty() => outcome.empty.push(b.name),
            Ok(papers) => lists.push(
                papers
                    .into_iter()
                    .map(|paper| Hit { paper, source: b.label })
                    .collect::<Vec<_>>()
                    .into_iter(),
            ),
            Err(e) => {
                eprintln!("{}", cascade_note(b.name, &e));
                outcome.failures.push((b.name, e));
            }
        }
    }
    while !lists.is_empty() {
        lists.retain_mut(|list| match list.next() {
            Some(hit) => {
                outcome.hits.push(hit);
                true
            }
            None => false,
        });
    }
    outcome
}

/// Query `tiers` in order through `fetch` until a tier returns an on-topic
/// result (`on_topic`); that tier's ranked results are the answer. When no
/// tier does, the answer is every tier's results, in tier order.
/// Failures and empty answers of every tier queried are kept for reporting.
async fn cascade<F, Fut>(tiers: &[&[&'static Backend]], query: &str, mut fetch: F) -> Outcome
where
    F: FnMut(&'static Backend) -> Fut,
    Fut: Future<Output = Result<Vec<PaperResult>, String>>,
{
    let terms = query_terms(query);
    let mut seen = Outcome::default();
    for tier in tiers {
        let mut outcome = fetch_tier(tier, &mut fetch).await;
        seen.failures.append(&mut outcome.failures);
        seen.empty.append(&mut outcome.empty);
        if outcome.hits.is_empty() {
            continue;
        }
        let ranked = rank(outcome.hits, query);
        let top = &ranked[0].paper;
        if !is_book_review(&top.title) && on_topic(top, &terms) {
            seen.hits = ranked;
            return seen;
        }
        let names: Vec<&str> = tier.iter().map(|b| b.name).collect();
        eprintln!("note: {} results may be imprecise, trying other sources", names.join(" + "));
        seen.hits.extend(ranked);
    }
    seen
}

/// The ranked hits of `outcome`, or an error when there are none and some
/// source failed: the error names each source consulted and what it returned.
fn finish(outcome: Outcome, query: &str) -> Result<Vec<Hit>, Box<dyn std::error::Error>> {
    if outcome.hits.is_empty() && !outcome.failures.is_empty() {
        let mut msg = format!(
            "search for \"{}\" found nothing: {} of {} sources failed",
            query,
            outcome.failures.len(),
            outcome.failures.len() + outcome.empty.len()
        );
        for (name, error) in &outcome.failures {
            msg.push_str(&format!("\n  {}: {}", name, error));
        }
        for name in &outcome.empty {
            msg.push_str(&format!("\n  {}: no results", name));
        }
        return Err(msg.into());
    }
    Ok(rank(outcome.hits, query))
}

// --- Relevance ranking ---

/// Tokenize a string into lowercase alphanumeric words.
fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// Matching form of a lowercase word: a trailing plural `s` dropped
/// (`units` matches `unit`), except after `s` (`loss`) and in short words.
fn fold(word: &str) -> String {
    if word.len() > 3 && word.ends_with('s') && !word.ends_with("ss") {
        word[..word.len() - 1].to_string()
    } else {
        word.to_string()
    }
}

/// The query's topic words (`query_words`) in matching form, without repeats.
fn query_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for t in crate::api::query_words(query).iter().map(|w| fold(w)) {
        if !terms.contains(&t) {
            terms.push(t);
        }
    }
    terms
}

/// Share (0.0 to 1.0) of `terms` found among the words of the paper's title and
/// authors, and also of its abstract when `with_abstract`.
fn coverage(paper: &PaperResult, terms: &[String], with_abstract: bool) -> f64 {
    if terms.is_empty() {
        return 0.0;
    }
    let mut words: Vec<String> = tokenize(&paper.title);
    for author in &paper.authors {
        words.extend(tokenize(author));
    }
    if with_abstract {
        words.extend(tokenize(paper.abstract_text.as_deref().unwrap_or_default()));
    }
    let words: std::collections::HashSet<String> = words.iter().map(|w| fold(w)).collect();
    terms.iter().filter(|t| words.contains(*t)).count() as f64 / terms.len() as f64
}

/// Lowercase alphanumeric words of a title joined by single spaces.
fn normalized_title(title: &str) -> String {
    tokenize(title).join(" ")
}

/// Whether two records are the same work, judged by the strongest identifier
/// both carry: DOI, then arXiv id, then normalized title.
fn same_work(a: &PaperResult, b: &PaperResult) -> bool {
    if let (Some(x), Some(y)) = (&a.doi, &b.doi) {
        return x.eq_ignore_ascii_case(y);
    }
    if let (Some(x), Some(y)) = (&a.arxiv_id, &b.arxiv_id) {
        return crate::detect::normalize_arxiv(x) == crate::detect::normalize_arxiv(y);
    }
    let title = normalized_title(&a.title);
    !title.is_empty() && title == normalized_title(&b.title)
}

/// Fill the fields `kept` lacks from `other`, a record of the same work.
fn absorb(kept: &mut PaperResult, other: PaperResult) {
    kept.doi = kept.doi.take().or(other.doi);
    kept.arxiv_id = kept.arxiv_id.take().or(other.arxiv_id);
    kept.abstract_text = kept.abstract_text.take().or(other.abstract_text);
    kept.venue = kept.venue.take().or(other.venue);
    kept.citations = kept.citations.or(other.citations);
    kept.pdf_url = kept.pdf_url.take().or(other.pdf_url);
    kept.s2_id = kept.s2_id.take().or(other.s2_id);
}

/// Whether `paper` holds `ON_TOPIC_SHARE` of `terms` in its title, abstract or authors.
fn on_topic(paper: &PaperResult, terms: &[String]) -> bool {
    coverage(paper, terms, true) >= ON_TOPIC_SHARE
}

/// Merge records of the same work (`same_work`; the earlier record and its
/// source are kept), then put on-topic results (`on_topic`) before the rest.
/// Each group keeps the order the backends ranked it: their relevance scores
/// weight rare words over common ones, which a count of matched words cannot.
fn rank(hits: Vec<Hit>, query: &str) -> Vec<Hit> {
    let terms = query_terms(query);
    let mut kept: Vec<Hit> = Vec::with_capacity(hits.len());
    for hit in hits {
        match kept.iter_mut().find(|k| same_work(&k.paper, &hit.paper)) {
            Some(k) => absorb(&mut k.paper, hit.paper),
            None => kept.push(hit),
        }
    }
    let (mut ranked, rest): (Vec<Hit>, Vec<Hit>) = kept.into_iter().partition(|h| on_topic(&h.paper, &terms));
    ranked.extend(rest);
    ranked
}

/// The first hit with the largest share of `terms` in its title and authors:
/// a query naming a title resolves to that title, not to the top-ranked
/// paper that discusses it.
fn best_title_match(hits: Vec<Hit>, terms: &[String]) -> Option<PaperResult> {
    let mut best: Option<(f64, PaperResult)> = None;
    for hit in hits {
        let score = coverage(&hit.paper, terms, false);
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, hit.paper));
        }
    }
    best.map(|(_, paper)| paper)
}

// --- Output ---

/// `paper_to_json` plus the `source` label of the backend that returned the hit.
fn hit_to_json(hit: &Hit) -> serde_json::Value {
    let mut json = super::paper_to_json(&hit.paper);
    json["source"] = serde_json::Value::String(hit.source.into());
    json
}

/// Print search results in JSON, concise, or verbose format.
fn print_results(ctx: &Context, hits: &[Hit]) {
    if hits.is_empty() {
        println!("No results found");
        return;
    }

    if ctx.json {
        let arr: Vec<serde_json::Value> = hits.iter().map(hit_to_json).collect();
        println!("{}", serde_json::to_string_pretty(&arr).unwrap());
        return;
    }

    for (i, hit) in hits.iter().enumerate() {
        let p = &hit.paper;
        let rank = i + 1;
        let author = p.authors.first().map(|s| s.as_str()).unwrap_or("?");

        if ctx.verbose {
            let cites = p.citations.map(|c| c.to_string()).unwrap_or_default();
            println!("{}. {} ({})", rank, p.title, p.year);
            println!("   Author: {} | Citations: {} | Source: {}", author, cites, hit.source);
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

/// Returns true if the title looks like a book review that should not stop the cascade.
fn is_book_review(title: &str) -> bool {
    let lower = title.to_lowercase();
    lower.contains("book review") || lower.starts_with("review of ") || lower.contains("reviews:")
}

/// Diagnostic for a failed API source; always names the error so callers can tell a 429 from a parse failure.
fn cascade_note(source: &str, error: &dyn std::fmt::Display) -> String {
    format!("note: {} unavailable: {}", source, error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{arxiv, crossref, openalex};
    use std::cell::RefCell;

    const QUERY: &str = "dropout Shapley value hidden units";
    /// Recorded 2026-11-13; see the parser tests in `api::openalex`, `api::arxiv`.
    const OA_REPLY: &str = include_str!("../../test/search/openalex-dropout-shapley.json");
    const ARXIV_REPLY: &str = include_str!("../../test/search/arxiv-dropout-shapley.xml");
    /// Recorded 2026-11-13 from `crossref::search_url(QUERY, 10)`.
    const CROSSREF_REPLY: &str = include_str!("../../test/search/crossref-dropout-shapley.json");
    /// On topic for `QUERY`: Shapley values of hidden units, used to prune them.
    const ON_TOPIC: &str = "Shapley Value as Principled Metric for Structured Network Pruning";
    const S2_429: &str = "HTTP 429 Too Many Requests for https://api.semanticscholar.org/graph/v1/paper/search after 3 attempts";

    /// A backend fetch serving the recorded replies, with Semantic Scholar
    /// rate limited; records the label of every backend asked.
    fn recorded<'a>(
        asked: &'a RefCell<Vec<&'static str>>,
    ) -> impl FnMut(&'static Backend) -> std::future::Ready<Result<Vec<PaperResult>, String>> + 'a {
        move |b| {
            asked.borrow_mut().push(b.label);
            let reply = match b.label {
                "s2" => Err(S2_429.to_string()),
                "openalex" => openalex::parse_search(OA_REPLY).map_err(|e| e.to_string()),
                "arxiv" => arxiv::parse_search(ARXIV_REPLY).map_err(|e| e.to_string()),
                "crossref" => crossref::parse_search(CROSSREF_REPLY).map_err(|e| e.to_string()),
                _ => Ok(Vec::new()),
            };
            std::future::ready(reply)
        }
    }

    fn paper(title: &str) -> PaperResult {
        PaperResult { title: title.to_string(), ..Default::default() }
    }

    fn hit(paper: PaperResult, source: &'static str) -> Hit {
        Hit { paper, source }
    }

    fn titles(hits: &[Hit]) -> Vec<&str> {
        hits.iter().map(|h| h.paper.title.as_str()).collect()
    }

    #[tokio::test]
    async fn s2_rate_limit_falls_back_to_openalex_and_arxiv_before_any_catalog() {
        let asked = RefCell::new(Vec::new());
        let outcome = cascade(CASCADE, QUERY, recorded(&asked)).await;
        assert_eq!(asked.into_inner(), vec!["s2", "openalex", "arxiv"]);
        assert_eq!(outcome.failures, vec![("Semantic Scholar", S2_429.to_string())]);
        assert!(outcome.hits.iter().any(|h| h.source == "openalex"));
        assert!(outcome.hits.iter().any(|h| h.source == "arxiv"));
        assert!(outcome.hits.iter().all(|h| h.source == "openalex" || h.source == "arxiv"));
    }

    /// Regression: on 2026-11-13 this fallback answered with CrossRef's
    /// game-theory chapters ("Power and the Shapley Value", ...), whose titles
    /// share two of the five query words, and dropped every other source.
    #[tokio::test]
    async fn free_text_fallback_puts_the_on_topic_paper_in_the_top_five() {
        let asked = RefCell::new(Vec::new());
        let hits = finish(cascade(CASCADE, QUERY, recorded(&asked)).await, QUERY).unwrap();
        let top: Vec<&str> = titles(&hits).into_iter().take(5).collect();
        assert!(top.contains(&ON_TOPIC), "top five: {:#?}", top);
        assert!(hits.iter().take(5).all(|h| h.source != "crossref"), "top five: {:#?}", top);
    }

    #[tokio::test]
    async fn an_s2_answer_covering_the_query_ends_the_cascade() {
        let asked = RefCell::new(Vec::new());
        let on_topic = PaperResult {
            abstract_text: Some("Shapley values of hidden units under dropout.".to_string()),
            ..paper(ON_TOPIC)
        };
        let outcome = cascade(CASCADE, QUERY, |b: &'static Backend| {
            asked.borrow_mut().push(b.label);
            std::future::ready(Ok(vec![on_topic.clone()]))
        })
        .await;
        assert_eq!(asked.into_inner(), vec!["s2"]);
        assert_eq!(outcome.hits[0].source, "s2");
    }

    #[tokio::test]
    async fn every_source_failing_is_an_error_naming_each_source_and_its_error() {
        let outcome = cascade(CASCADE, QUERY, |b: &'static Backend| {
            std::future::ready(Err(format!("HTTP 503 from {}", b.label)))
        })
        .await;
        let err = finish(outcome, QUERY).unwrap_err().to_string();
        assert!(err.contains("6 of 6 sources failed"), "{}", err);
        for (name, label) in [
            ("Semantic Scholar", "s2"),
            ("OpenAlex", "openalex"),
            ("arXiv", "arxiv"),
            ("CrossRef", "crossref"),
            ("OpenLibrary", "openlibrary"),
            ("Clio", "clio"),
        ] {
            assert!(err.contains(&format!("{}: HTTP 503 from {}", name, label)), "{}", err);
        }
    }

    #[tokio::test]
    async fn a_failure_among_empty_answers_is_an_error_not_zero_hits() {
        let outcome = cascade(CASCADE, QUERY, |b: &'static Backend| {
            std::future::ready(if b.label == "s2" { Err(S2_429.to_string()) } else { Ok(Vec::new()) })
        })
        .await;
        let err = finish(outcome, QUERY).unwrap_err().to_string();
        assert!(err.contains("1 of 6 sources failed"), "{}", err);
        assert!(err.contains("Semantic Scholar: HTTP 429"), "{}", err);
        assert!(err.contains("arXiv: no results"), "{}", err);
    }

    #[test]
    fn every_source_answering_empty_is_zero_hits() {
        let outcome = Outcome { empty: vec!["Semantic Scholar", "OpenAlex"], ..Default::default() };
        assert!(finish(outcome, QUERY).unwrap().is_empty());
    }

    #[test]
    fn tier_outcome_interleaves_sources_by_rank_and_sorts_out_failures() {
        let results = vec![
            Ok(vec![paper("O1"), paper("O2"), paper("O3")]),
            Ok(vec![paper("A1")]),
        ];
        let outcome = tier_outcome(&[&OPENALEX, &ARXIV], results);
        assert_eq!(titles(&outcome.hits), vec!["O1", "A1", "O2", "O3"]);
        assert_eq!(outcome.hits[1].source, "arxiv");
        let outcome = tier_outcome(&[&OPENALEX, &ARXIV], vec![Err("HTTP 429".into()), Ok(vec![])]);
        assert_eq!(outcome.failures, vec![("OpenAlex", "HTTP 429".to_string())]);
        assert_eq!(outcome.empty, vec!["arXiv"]);
    }

    #[test]
    fn same_work_compares_the_strongest_identifier_both_records_carry() {
        let with = |doi: Option<&str>, arxiv: Option<&str>, title: &str| PaperResult {
            doi: doi.map(String::from),
            arxiv_id: arxiv.map(String::from),
            ..paper(title)
        };
        assert!(same_work(&with(Some("10.1/ABC"), None, "X"), &with(Some("10.1/abc"), None, "Y")));
        assert!(!same_work(&with(Some("10.1/a"), None, "Same"), &with(Some("10.1/b"), None, "Same")));
        assert!(same_work(&with(Some("10.1/a"), Some("2001.00001"), "X"), &with(None, Some("2001.00001v2"), "Y")));
        assert!(!same_work(&with(None, Some("2001.00001"), "Same"), &with(None, Some("2001.00002"), "Same")));
        assert!(same_work(&with(Some("10.1/a"), None, "Attention Is All You Need."), &with(None, Some("1706.03762"), "attention is all you need")));
        assert!(!same_work(&paper(""), &paper("")));
    }

    #[test]
    fn rank_merges_duplicates_keeping_the_first_source_and_filling_its_gaps() {
        let oa = PaperResult { doi: Some("10.1/x".into()), ..paper("Concrete Dropout") };
        let ax = PaperResult {
            arxiv_id: Some("1705.07832".into()),
            abstract_text: Some("Dropout probabilities tuned by gradient.".into()),
            ..paper("Concrete dropout")
        };
        let ranked = rank(vec![hit(oa, "openalex"), hit(ax, "arxiv")], "concrete dropout");
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].source, "openalex");
        assert_eq!(ranked[0].paper.doi.as_deref(), Some("10.1/x"));
        assert_eq!(ranked[0].paper.arxiv_id.as_deref(), Some("1705.07832"));
        assert!(ranked[0].paper.abstract_text.is_some());
    }

    /// Regression: sorting by the share of topic words put papers matching
    /// "stochastic rate uniform distribution" above arXiv's dropout papers for
    /// "stochastic dropout rate uniform distribution" (live check, 2026-11-13).
    #[test]
    fn rank_puts_on_topic_results_first_and_keeps_backend_order_within_each_group() {
        let off_first = paper("Dropout A"); // 1 of 5 topic words
        let on_three = paper("Dropout and the Shapley value"); // 3 of 5
        let off_two = paper("The Shapley value"); // 2 of 5
        let on_five = PaperResult {
            abstract_text: Some("We relate dropout to the Shapley value of hidden units.".into()),
            ..paper("Ensembles of subnetworks")
        };
        let ranked = rank(
            vec![hit(off_first, "s2"), hit(on_three, "s2"), hit(off_two, "s2"), hit(on_five, "s2")],
            QUERY,
        );
        assert_eq!(
            titles(&ranked),
            vec!["Dropout and the Shapley value", "Ensembles of subnetworks", "Dropout A", "The Shapley value"]
        );
    }

    #[test]
    fn best_title_match_takes_the_first_hit_with_the_most_query_words_in_its_title() {
        let hits = vec![
            hit(PaperResult { abstract_text: Some("attention need".into()), ..paper("A survey") }, "s2"),
            hit(paper("Attention Is All You Need"), "s2"),
            hit(paper("Attention is all you need (reprint)"), "openalex"),
        ];
        let best = best_title_match(hits, &query_terms("attention is all you need")).unwrap();
        assert_eq!(best.title, "Attention Is All You Need");
        assert!(best_title_match(Vec::new(), &query_terms("x")).is_none());
    }

    #[test]
    fn hit_json_carries_its_source() {
        let json = hit_to_json(&hit(paper("T"), "arxiv"));
        assert_eq!(json["source"], "arxiv");
        assert_eq!(json["title"], "T");
    }

    #[test]
    fn cascade_note_includes_error() {
        assert_eq!(
            cascade_note("OpenAlex", &"HTTP 429 Too Many Requests"),
            "note: OpenAlex unavailable: HTTP 429 Too Many Requests"
        );
    }

    #[test]
    fn tokenize_basic() {
        assert_eq!(tokenize("Attention Is All You Need"), vec!["attention", "is", "all", "you", "need"]);
    }

    #[test]
    fn tokenize_with_punctuation() {
        assert_eq!(tokenize("SHAP: A Unified Approach"), vec!["shap", "a", "unified", "approach"]);
    }

    #[test]
    fn fold_drops_plural_s_only() {
        assert_eq!(fold("units"), "unit");
        assert_eq!(fold("masks"), "mask");
        assert_eq!(fold("loss"), "loss");
        assert_eq!(fold("gas"), "gas");
    }

    #[test]
    fn relevance_exact_title_match() {
        let paper = PaperResult {
            title: "Attention Is All You Need".to_string(),
            authors: vec!["Ashish Vaswani".to_string()],
            ..Default::default()
        };
        let score = coverage(&paper, &query_terms("attention is all you need"), true);
        assert!(score >= 0.9, "exact title match should score high: {}", score);
    }

    #[test]
    fn relevance_author_match_boosts() {
        let paper = PaperResult {
            title: "A Unified Approach to Interpreting Model Predictions".to_string(),
            authors: vec!["Scott M. Lundberg".to_string()],
            ..Default::default()
        };
        // "lundberg", "unified", "approach" match (3/4 = 0.75). "shap" doesn't match title/author.
        let score = coverage(&paper, &query_terms("SHAP lundberg unified approach"), true);
        assert!(score >= 0.5, "author+title partial match: {}", score);
    }

    #[test]
    fn relevance_no_match() {
        let paper = PaperResult {
            title: "Schooling incentives and child labor".to_string(),
            authors: vec!["Eric Edmonds".to_string()],
            ..Default::default()
        };
        let score = coverage(&paper, &query_terms("mesnard counterfactual credit assignment"), true);
        assert!(score < 0.2, "unrelated paper should score low: {}", score);
    }

    #[test]
    fn rank_puts_relevant_first() {
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
        let ranked = rank(vec![hit(bad, "s2"), hit(good, "s2")], "PPO proximal policy optimization schulman");
        assert_eq!(ranked[0].paper.title, "Proximal Policy Optimization Algorithms");
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
    fn rank_deduplicates_by_doi() {
        let a = PaperResult { doi: Some("10.1234/test".to_string()), ..paper("Paper A") };
        let b = PaperResult { doi: Some("10.1234/test".to_string()), ..paper("Paper A") };
        assert_eq!(rank(vec![hit(a, "s2"), hit(b, "openalex")], "paper").len(), 1);
    }
}
