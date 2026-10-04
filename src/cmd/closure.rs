//! `lit closure`: breadth-first search over the citation graph from seed papers,
//! one record per paper (deduplicated by `id_key`), emitted as JSONL.
//!
//! Graph traversal and dedup are generic and live here; screening policy lives
//! in the calling repository (ADR-001).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::neighbors::{self, Direction, Fetch, Source};
use crate::api::PaperResult;

/// Neighbor calls in flight at once. S2 rate-limits hard, so this stays small;
/// a 429 is retried by the HTTP client and then falls back to OpenAlex.
const CONCURRENCY: usize = 4;

/// Which neighbor calls each expanded node gets.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Directions {
    Both,
    Refs,
    Cites,
}

impl Directions {
    fn calls(self) -> Vec<Direction> {
        match self {
            Directions::Both => vec![Direction::Refs, Direction::Cites],
            Directions::Refs => vec![Direction::Refs],
            Directions::Cites => vec![Direction::Cites],
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Directions::Both => "both",
            Directions::Refs => "refs",
            Directions::Cites => "cites",
        }
    }
}

/// `lit closure` arguments. Output is always JSONL; `--json` is accepted.
#[derive(clap::Args, Debug)]
pub struct Args {
    /// Seed papers (DOI or arXiv id)
    pub seeds: Vec<String>,
    /// File of seeds, one per line; `#` starts a comment
    #[arg(long)]
    pub seeds_file: Option<PathBuf>,
    /// Levels to expand (1 = the seeds' neighbors only)
    #[arg(long, default_value = "1", value_parser = clap::value_parser!(u64).range(1..))]
    pub hops: u64,
    /// Neighbor calls per expanded node
    #[arg(long, value_enum, default_value = "both")]
    pub direction: Directions,
    /// Mark (never drop) papers found in this .bib; repeatable
    #[arg(long)]
    pub exclude_bib: Vec<PathBuf>,
    /// Expand a node at hop >= 1 only if its id_key is listed in this file
    #[arg(long)]
    pub expand_only: Option<PathBuf>,
    /// Stop adding papers at this many
    #[arg(long)]
    pub max_papers: Option<usize>,
}

/// Traversal settings, parsed from the command line.
pub struct Options {
    /// Seed ids as given (DOI or arXiv id, any accepted form).
    pub seeds: Vec<String>,
    /// Levels to expand: 1 expands only the seeds.
    pub hops: usize,
    /// Calls made per expanded node.
    pub directions: Vec<Direction>,
    /// When set, a node at hop >= 1 is expanded only if one of its keys is listed.
    pub expand_only: Option<HashSet<String>>,
    /// Stop adding new records at this many.
    pub max_papers: Option<usize>,
}

/// A failed neighbor call: the call kind, the node's `id_key`, and why.
#[derive(Debug)]
pub struct CallError {
    pub call: Direction,
    pub id: String,
    pub message: String,
}

/// One paper; `alias` points at the record it was merged into.
struct Record {
    paper: PaperResult,
    hop: usize,
    /// (call kind, record index of the expanded node).
    edges: Vec<(Direction, usize)>,
    source: Option<Source>,
    queued: bool,
    alias: Option<usize>,
}

/// Records with a key index; merged records forward to their survivor.
#[derive(Default)]
pub struct Graph {
    records: Vec<Record>,
    index: HashMap<String, usize>,
    live: usize,
    max_papers: Option<usize>,
    /// Papers dropped because they carried no DOI, arXiv id, or title.
    pub skipped: usize,
    /// Whether `max_papers` stopped a new record.
    pub capped: bool,
}

/// Everything a closure run found.
pub struct Closure {
    pub graph: Graph,
    pub errors: Vec<CallError>,
    pub calls_ok: usize,
}

/// Bib entries indexed by DOI, arXiv id, and normalized title.
#[derive(Default)]
pub struct KnownIndex {
    by_key: HashMap<String, (String, String)>,
}

/// Lowercase, alphanumerics separated by single spaces.
pub fn normalize_title(title: &str) -> String {
    let spaced: String = title
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// arXiv id without version, from the field or from an arXiv DOI.
fn arxiv_of(p: &PaperResult) -> Option<String> {
    use crate::detect::{arxiv_id_from_doi, normalize_arxiv};
    p.arxiv_id
        .as_deref()
        .map(normalize_arxiv)
        .or_else(|| p.doi.as_deref().and_then(arxiv_id_from_doi))
}

/// Every key `p` can be matched by, in `id_key` precedence order.
fn keys(p: &PaperResult) -> Vec<String> {
    let title = normalize_title(&p.title);
    [
        p.doi.as_ref().map(|d| format!("doi:{}", d.to_lowercase())),
        arxiv_of(p).map(|a| format!("arxiv:{}", a)),
        (!title.is_empty()).then(|| format!("title:{}", title)),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The dedup key: `doi:` > `arxiv:` > `title:`; `None` without any of them.
pub fn id_key(p: &PaperResult) -> Option<String> {
    keys(p).into_iter().next()
}

/// Ids from a seeds or expand-only file: one per line, `#` starts a comment.
pub fn read_id_list(content: &str) -> Vec<String> {
    content
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// Whether two papers carry no contradicting DOI or arXiv id.
fn compatible(a: &PaperResult, b: &PaperResult) -> bool {
    let clash = |x: Option<String>, y: Option<String>| matches!((x, y), (Some(x), Some(y)) if !x.eq_ignore_ascii_case(&y));
    !clash(a.doi.clone(), b.doi.clone()) && !clash(arxiv_of(a), arxiv_of(b))
}

/// Fill the fields `into` lacks from `from`.
fn fill_missing(into: &mut PaperResult, from: PaperResult) {
    if into.title.is_empty() {
        into.title = from.title;
    }
    if into.authors.is_empty() {
        into.authors = from.authors;
    }
    if into.year.is_empty() {
        into.year = from.year;
    }
    into.doi = into.doi.take().or(from.doi);
    into.arxiv_id = into.arxiv_id.take().or(from.arxiv_id);
    into.s2_id = into.s2_id.take().or(from.s2_id);
    into.venue = into.venue.take().or(from.venue);
}

impl Graph {
    pub fn new(max_papers: Option<usize>) -> Self {
        Graph { max_papers, ..Default::default() }
    }

    /// Add or merge `p`; returns the surviving record's index, or `None` when
    /// it was skipped (no key) or refused (cap reached).
    ///
    /// A shared DOI or arXiv id merges; a shared title merges only when no DOI
    /// or arXiv id contradicts it. Cost: O(keys) map lookups.
    pub fn add(&mut self, mut p: PaperResult, hop: usize, edge: Option<(Direction, usize)>, source: Option<Source>) -> Option<usize> {
        p.arxiv_id = arxiv_of(&p);
        let ks = keys(&p);
        if ks.is_empty() {
            self.skipped += 1;
            return None;
        }
        let mut roots: Vec<usize> = Vec::new();
        for k in &ks {
            let Some(&i) = self.index.get(k) else { continue };
            let r = self.find(i);
            if !roots.contains(&r) && (!k.starts_with("title:") || compatible(&self.records[r].paper, &p)) {
                roots.push(r);
            }
        }
        let target = match roots.first() {
            Some(&r) => {
                fill_missing(&mut self.records[r].paper, p);
                r
            }
            None => {
                if self.max_papers.is_some_and(|m| self.live >= m) {
                    self.capped = true;
                    return None;
                }
                self.records.push(Record { paper: p, hop, edges: Vec::new(), source, queued: false, alias: None });
                self.live += 1;
                self.records.len() - 1
            }
        };
        for &r in &roots[roots.len().min(1)..] {
            self.absorb(target, r);
        }
        let rec = &mut self.records[target];
        rec.hop = rec.hop.min(hop);
        rec.source = rec.source.or(source);
        if let Some(e) = edge
            && !rec.edges.contains(&e)
        {
            rec.edges.push(e);
        }
        for k in keys(&self.records[target].paper) {
            self.index.entry(k).or_insert(target);
        }
        Some(target)
    }

    /// Merge record `r` into `target`; `r` forwards to `target` from now on.
    fn absorb(&mut self, target: usize, r: usize) {
        let gone = &mut self.records[r];
        gone.alias = Some(target);
        let (paper, edges) = (std::mem::take(&mut gone.paper), std::mem::take(&mut gone.edges));
        let (hop, source, queued) = (gone.hop, gone.source, gone.queued);
        self.live -= 1;
        let rec = &mut self.records[target];
        fill_missing(&mut rec.paper, paper);
        rec.hop = rec.hop.min(hop);
        rec.source = rec.source.or(source);
        rec.queued |= queued;
        for e in edges {
            if !rec.edges.contains(&e) {
                rec.edges.push(e);
            }
        }
    }

    /// The surviving record that `i` was merged into (itself if never merged).
    fn find(&self, mut i: usize) -> usize {
        while let Some(next) = self.records[i].alias {
            i = next;
        }
        i
    }

    /// Number of distinct papers.
    pub fn papers(&self) -> usize {
        self.live
    }
}

impl KnownIndex {
    /// Index the entries of one .bib file, citing `file` as given.
    pub fn add_bib(&mut self, content: &str, file: &str) {
        for e in crate::bibtex::parse_bib_file(content) {
            let arxiv_eprint = e
                .get_field("archiveprefix")
                .is_none_or(|a| a.trim().eq_ignore_ascii_case("arxiv"));
            let p = PaperResult {
                title: e.get_field("title").unwrap_or("").to_string(),
                doi: e.get_field("doi").map(|d| crate::detect::normalize_doi(d.trim())),
                arxiv_id: e.get_field("eprint").filter(|_| arxiv_eprint).map(|a| a.trim().to_string()),
                ..Default::default()
            };
            for k in keys(&p) {
                self.by_key.entry(k).or_insert_with(|| (e.key.clone(), file.to_string()));
            }
        }
    }

    /// The (citekey, bib file) of the first entry matching `p` by DOI, arXiv id, or title.
    pub fn lookup(&self, p: &PaperResult) -> Option<(String, String)> {
        keys(p).iter().find_map(|k| self.by_key.get(k).cloned())
    }
}

/// Run the BFS. Cost: one neighbor call (one or more pages) per expanded node
/// and direction, at most `CONCURRENCY` in flight.
///
/// Seeds must carry a DOI or arXiv id so that they have an `id_key`.
pub(crate) async fn closure<F: Fetch>(f: &F, opts: &Options) -> Result<Closure, String> {
    use futures_util::stream::{self, StreamExt};

    // Seeds go in before the cap applies: they are always kept and expanded.
    let mut graph = Graph::new(None);
    let mut frontier = Vec::new();
    for s in &opts.seeds {
        let p = neighbors::seed_paper(s);
        if p.doi.is_none() && p.arxiv_id.is_none() {
            return Err(format!("seed {}: lit closure needs a DOI or arXiv id", s));
        }
        let i = graph.add(p, 0, None, None).expect("a seed has a key and no cap applies");
        if !std::mem::replace(&mut graph.records[i].queued, true) {
            frontier.push(i);
        }
    }
    graph.max_papers = opts.max_papers;

    let mut errors = Vec::new();
    let mut calls_ok = 0;
    for hop in 0..opts.hops {
        let mut nodes: Vec<usize> = frontier.iter().map(|&i| graph.find(i)).collect();
        nodes.sort_unstable();
        nodes.dedup();
        let calls: Vec<(usize, Direction, PaperResult)> = nodes
            .into_iter()
            .filter(|&i| hop == 0 || opts.expand_only.as_ref().is_none_or(|ok| keys(&graph.records[i].paper).iter().any(|k| ok.contains(k))))
            .flat_map(|i| opts.directions.iter().map(move |&d| (i, d)))
            .map(|(i, d)| (i, d, graph.records[i].paper.clone()))
            .collect();
        let results: Vec<_> = stream::iter(calls)
            .map(|(i, d, p)| async move {
                let r = neighbors::fetch(f, &p, d).await;
                (i, d, p, r)
            })
            .buffered(CONCURRENCY)
            .collect()
            .await;

        let mut next = Vec::new();
        for (i, d, p, result) in results {
            let found = match result {
                Ok(n) => n,
                Err(message) => {
                    errors.push(CallError { call: d, id: id_key(&p).unwrap_or_default(), message });
                    continue;
                }
            };
            calls_ok += 1;
            let node = graph.find(i);
            graph.records[node].source = graph.records[node].source.or(Some(found.source));
            for q in found.papers {
                let Some(j) = graph.add(q, hop + 1, Some((d, i)), Some(found.source)) else { continue };
                if !std::mem::replace(&mut graph.records[j].queued, true) {
                    next.push(j);
                }
            }
        }
        frontier = next;
    }
    Ok(Closure { graph, errors, calls_ok })
}

fn read_file(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {}", path.display(), e))
}

/// Print the closure as JSONL: run header, papers, errors, summary.
///
/// Fails (after printing) only if every neighbor call failed.
pub async fn run(ctx: &super::Context, args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let mut seeds = args.seeds;
    if let Some(ref f) = args.seeds_file {
        seeds.extend(read_id_list(&read_file(f)?));
    }
    if seeds.is_empty() {
        return Err("no seeds: give SEED arguments or --seeds-file".into());
    }
    let expand_only = match args.expand_only {
        Some(ref f) => Some(read_id_list(&read_file(f)?).into_iter().collect()),
        None => None,
    };
    let mut known = KnownIndex::default();
    for b in &args.exclude_bib {
        known.add_bib(&read_file(b)?, &b.display().to_string());
    }
    let header = json!({
        "type": "run",
        "lit_version": crate::VERSION,
        "timestamp": super::utc_timestamp(),
        "argv": std::env::args().collect::<Vec<_>>(),
        "seeds": seeds,
        "hops": args.hops,
        "direction": args.direction.as_str(),
    });
    let opts = Options {
        seeds,
        hops: args.hops as usize,
        directions: args.direction.calls(),
        expand_only,
        max_papers: args.max_papers,
    };

    let c = closure(&ctx.client(), &opts).await?;
    println!("{}", header);
    for line in render(&c, &known) {
        println!("{}", line);
    }
    if c.graph.skipped > 0 {
        crate::format::warn(&format!("skipped {} paper(s) with no DOI, arXiv id, or title", c.graph.skipped));
    }
    if c.graph.capped {
        crate::format::warn(&format!("stopped adding papers at --max-papers={}", opts.max_papers.unwrap_or(0)));
    }
    if c.calls_ok == 0 {
        return Err(format!("all {} neighbor call(s) failed", c.errors.len()).into());
    }
    Ok(())
}

/// JSONL objects in output order: papers by hop, then errors, then the summary.
/// The run header is the caller's.
pub fn render(c: &Closure, known: &KnownIndex) -> Vec<Value> {
    let g = &c.graph;
    let mut live: Vec<usize> = (0..g.records.len()).filter(|&i| g.records[i].alias.is_none()).collect();
    live.sort_by_key(|&i| g.records[i].hop);
    let key = |i: usize| id_key(&g.records[i].paper).unwrap_or_default();

    let mut out = Vec::with_capacity(live.len() + c.errors.len() + 1);
    let mut n_known = 0;
    for i in live {
        let r = &g.records[i];
        let mut edges: Vec<(&str, String)> = Vec::new();
        for &(d, from) in &r.edges {
            let from = g.find(from);
            let e = (d.as_str(), key(from));
            if from != i && !edges.contains(&e) {
                edges.push(e);
            }
        }
        let k = known.lookup(&r.paper);
        n_known += k.is_some() as usize;
        let p = &r.paper;
        out.push(json!({
            "type": "paper",
            "id_key": key(i),
            "doi": p.doi,
            "arxiv_id": p.arxiv_id,
            "s2_id": p.s2_id,
            "title": (!p.title.is_empty()).then_some(&p.title),
            "authors": p.authors,
            "year": p.year.trim().parse::<i64>().ok(),
            "venue": p.venue,
            "hop": r.hop,
            "edges": edges.iter().map(|(kind, from)| json!({"kind": kind, "from": from})).collect::<Vec<_>>(),
            "known": k.map(|(citekey, bib)| json!({"citekey": citekey, "bib": bib})),
            "source": r.source.map(Source::as_str),
        }));
    }
    for e in &c.errors {
        out.push(json!({"type": "error", "call": e.call.as_str(), "id": e.id, "message": e.message}));
    }
    out.push(json!({"type": "summary", "papers": g.papers(), "known": n_known, "errors": c.errors.len()}));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::neighbors::tests::{rate_limited, s2_page, MockFetch};

    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(fut)
    }

    fn paper(title: &str, doi: Option<&str>, arxiv: Option<&str>) -> PaperResult {
        PaperResult {
            title: title.into(),
            doi: doi.map(String::from),
            arxiv_id: arxiv.map(String::from),
            ..Default::default()
        }
    }

    fn opts(seeds: &[&str], hops: usize) -> Options {
        Options {
            seeds: seeds.iter().map(|s| s.to_string()).collect(),
            hops,
            directions: vec![Direction::Refs],
            expand_only: None,
            max_papers: None,
        }
    }

    fn of_type<'a>(lines: &'a [Value], t: &str) -> Vec<&'a Value> {
        lines.iter().filter(|l| l["type"] == t).collect()
    }

    #[test]
    fn normalize_title_keeps_alphanumerics_and_single_spaces() {
        assert_eq!(normalize_title("  {Attention} Is All-You  Need!  "), "attention is all you need");
    }

    #[test]
    fn id_key_prefers_doi_then_arxiv_then_title() {
        assert_eq!(id_key(&paper("T", Some("10.1/ABC"), Some("2408.01416"))).as_deref(), Some("doi:10.1/abc"));
        assert_eq!(id_key(&paper("T", None, Some("2408.01416v3"))).as_deref(), Some("arxiv:2408.01416"));
        assert_eq!(id_key(&paper("A Title.", None, None)).as_deref(), Some("title:a title"));
        assert_eq!(id_key(&paper("", None, None)), None);
    }

    #[test]
    fn read_id_list_skips_blanks_and_comments() {
        let ids = read_id_list("# header\n2408.01416  # the survey\n\n  10.1/x\n");
        assert_eq!(ids, vec!["2408.01416", "10.1/x"]);
    }

    #[test]
    fn records_with_doi_and_arxiv_merge_with_records_keyed_by_either() {
        let mut g = Graph::new(None);
        let a = g.add(paper("Paper", None, Some("2408.01416")), 1, Some((Direction::Refs, 0)), Some(Source::S2)).unwrap();
        let b = g.add(paper("Paper", Some("10.1/x"), None), 2, Some((Direction::Cites, 0)), Some(Source::S2)).unwrap();
        assert_eq!(g.papers(), 1, "same title, no conflicting id: one paper");
        let c = g.add(paper("Paper", Some("10.1/X"), Some("2408.01416v2")), 1, None, Some(Source::S2)).unwrap();
        assert_eq!(g.papers(), 1);
        assert_eq!(g.find(a), g.find(c));
        assert_eq!(g.find(b), g.find(c));
    }

    #[test]
    fn arxiv_and_doi_records_merge_through_a_record_carrying_both() {
        let mut g = Graph::new(None);
        g.add(paper("Arxiv title", None, Some("2408.01416")), 1, None, Some(Source::S2));
        g.add(paper("Published title", Some("10.1/x"), None), 1, None, Some(Source::S2));
        assert_eq!(g.papers(), 2);
        g.add(paper("Either", Some("10.1/x"), Some("2408.01416")), 1, None, Some(Source::S2));
        assert_eq!(g.papers(), 1);
    }

    #[test]
    fn same_title_with_conflicting_dois_stays_two_papers() {
        let mut g = Graph::new(None);
        g.add(paper("Introduction", Some("10.1/a"), None), 1, None, Some(Source::S2));
        g.add(paper("Introduction", Some("10.1/b"), None), 1, None, Some(Source::S2));
        assert_eq!(g.papers(), 2);
    }

    #[test]
    fn paper_without_any_key_is_skipped_and_counted() {
        let mut g = Graph::new(None);
        assert_eq!(g.add(paper("", None, None), 1, None, Some(Source::S2)), None);
        assert_eq!((g.papers(), g.skipped), (0, 1));
    }

    #[test]
    fn max_papers_refuses_new_records_but_still_merges() {
        let mut g = Graph::new(Some(1));
        g.add(paper("A", Some("10.1/a"), None), 1, None, Some(Source::S2)).unwrap();
        assert!(g.add(paper("B", Some("10.1/b"), None), 1, None, Some(Source::S2)).is_none());
        assert!(g.capped);
        assert!(g.add(paper("A", Some("10.1/a"), None), 1, None, Some(Source::S2)).is_some());
    }

    /// The old client stopped at 50 neighbors.
    #[test]
    fn closure_keeps_more_than_fifty_neighbors() {
        let f = MockFetch::new(vec![("ARXIV:2408.01416/references", Ok(s2_page("citedPaper", "r", 120, None)))]);
        let c = block_on(closure(&f, &opts(&["2408.01416"], 1))).unwrap();
        assert_eq!(c.graph.papers(), 121, "seed plus 120 references");
        assert_eq!(c.calls_ok, 1);
    }

    #[test]
    fn paper_reached_twice_keeps_one_record_with_both_edges() {
        let f = MockFetch::new(vec![
            ("ARXIV:2408.01416/references", Ok(s2_page("citedPaper", "r", 2, None))),
            ("DOI:10.1234/b/references", Ok(s2_page("citedPaper", "r", 1, None))),
        ]);
        let c = block_on(closure(&f, &opts(&["2408.01416", "10.1234/b"], 1))).unwrap();
        let lines = render(&c, &KnownIndex::default());
        let r0 = of_type(&lines, "paper").into_iter().find(|p| p["id_key"] == "doi:10.1/r0").unwrap().clone();
        let mut froms: Vec<&str> = r0["edges"].as_array().unwrap().iter().map(|e| e["from"].as_str().unwrap()).collect();
        froms.sort();
        assert_eq!(froms, vec!["arxiv:2408.01416", "doi:10.1234/b"]);
        assert_eq!(r0["edges"][0]["kind"], "refs");
    }

    #[test]
    fn expand_only_gates_expansion_at_hop_two() {
        let f = MockFetch::new(vec![
            ("ARXIV:2408.01416/references", Ok(s2_page("citedPaper", "a", 2, None))),
            ("paper/a0/references", Ok(s2_page("citedPaper", "b", 3, None))),
            ("paper/a1/references", Ok(s2_page("citedPaper", "c", 3, None))),
        ]);
        let mut o = opts(&["2408.01416"], 2);
        o.expand_only = Some(["doi:10.1/a0".to_string()].into_iter().collect());
        let c = block_on(closure(&f, &o)).unwrap();
        assert_eq!(c.graph.papers(), 1 + 2 + 3, "seed, a0, a1, and a0's three references");
        assert!(!f.calls.borrow().iter().any(|u| u.contains("paper/a1/")), "a1 is not listed");
        let lines = render(&c, &KnownIndex::default());
        let b0 = of_type(&lines, "paper").into_iter().find(|p| p["id_key"] == "doi:10.1/b0").unwrap().clone();
        assert_eq!(b0["hop"], 2);
    }

    #[test]
    fn seeds_are_expanded_even_when_not_listed() {
        let f = MockFetch::new(vec![("ARXIV:2408.01416/references", Ok(s2_page("citedPaper", "a", 2, None)))]);
        let mut o = opts(&["2408.01416"], 1);
        o.expand_only = Some(HashSet::new());
        let c = block_on(closure(&f, &o)).unwrap();
        assert_eq!(c.graph.papers(), 3);
    }

    #[test]
    fn s2_rate_limit_falls_back_to_openalex_without_an_error_record() {
        let f = MockFetch::new(vec![
            ("semanticscholar", rate_limited()),
            ("works/doi:10.48550/arXiv.2408.01416", Ok(r#"{"id": "https://openalex.org/W9",
                "referenced_works": ["https://openalex.org/W1"]}"#.into())),
            ("filter=openalex:W1", Ok(r#"{"results": [{"title": "One", "doi": "https://doi.org/10.1/one"}]}"#.into())),
        ]);
        let c = block_on(closure(&f, &opts(&["2408.01416"], 1))).unwrap();
        let lines = render(&c, &KnownIndex::default());
        let papers = of_type(&lines, "paper");
        assert_eq!(papers.len(), 2);
        assert!(papers.iter().all(|p| p["source"] == "openalex"));
        assert!(of_type(&lines, "error").is_empty());
    }

    #[test]
    fn call_failing_on_both_sources_is_an_error_record() {
        let f = MockFetch::new(vec![
            ("DOI:10.1234/ok/references", Ok(s2_page("citedPaper", "r", 1, None))),
            ("semanticscholar", rate_limited()),
        ]);
        let c = block_on(closure(&f, &opts(&["2408.01416", "10.1234/ok"], 1))).unwrap();
        let lines = render(&c, &KnownIndex::default());
        let errors = of_type(&lines, "error");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["call"], "refs");
        assert_eq!(errors[0]["id"], "arxiv:2408.01416");
        assert!(errors[0]["message"].as_str().unwrap().contains("429"));
        assert_eq!(c.calls_ok, 1);
        let summary = lines.last().unwrap();
        assert_eq!(summary["type"], "summary");
        assert_eq!(summary["errors"], 1);
    }

    #[test]
    fn s2_only_seed_is_rejected() {
        let f = MockFetch::new(vec![]);
        assert!(block_on(closure(&f, &opts(&["CorpusId:1"], 1))).is_err());
    }

    const BIB: &str = r#"
@article{bydoi2020,
  title = {Something Else},
  doi = {10.1/ABC},
}
@misc{byeprint2024,
  title = {Another},
  eprint = {2408.01416v2},
  archivePrefix = {arXiv},
}
@inproceedings{bytitle2019,
  title = {{Causal} Abstraction: A Theory},
}
"#;

    #[test]
    fn exclude_bib_matches_by_doi_eprint_and_title() {
        let mut k = KnownIndex::default();
        k.add_bib(BIB, "refs.bib");
        let hit = |p: PaperResult| k.lookup(&p).map(|(key, _)| key);
        assert_eq!(hit(paper("X", Some("10.1/abc"), None)).as_deref(), Some("bydoi2020"));
        assert_eq!(hit(paper("X", None, Some("2408.01416"))).as_deref(), Some("byeprint2024"));
        assert_eq!(hit(paper("Causal abstraction: a theory", None, None)).as_deref(), Some("bytitle2019"));
        assert_eq!(hit(paper("Unrelated", Some("10.1/zzz"), None)), None);
        assert_eq!(k.lookup(&paper("X", Some("10.1/abc"), None)).unwrap().1, "refs.bib");
    }

    #[test]
    fn render_emits_the_spec_schema_in_order() {
        let f = MockFetch::new(vec![("ARXIV:2408.01416/references", Ok(s2_page("citedPaper", "r", 2, None)))]);
        let c = block_on(closure(&f, &opts(&["2408.01416"], 1))).unwrap();
        let mut k = KnownIndex::default();
        k.add_bib("@article{known1, doi = {10.1/r1}}", "a.bib");
        let lines = render(&c, &k);
        let types: Vec<&str> = lines.iter().map(|l| l["type"].as_str().unwrap()).collect();
        assert_eq!(types, vec!["paper", "paper", "paper", "summary"]);
        let seed = &lines[0];
        assert_eq!((seed["id_key"].as_str(), seed["hop"].as_u64()), (Some("arxiv:2408.01416"), Some(0)));
        let mut keys: Vec<&str> = lines[1].as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort();
        assert_eq!(keys, vec!["arxiv_id", "authors", "doi", "edges", "hop", "id_key", "known", "s2_id", "source", "title", "type", "venue", "year"]);
        assert_eq!(lines[1]["known"], Value::Null);
        assert_eq!(lines[2]["known"], json!({"citekey": "known1", "bib": "a.bib"}));
        assert_eq!(lines[1]["edges"], json!([{"kind": "refs", "from": "arxiv:2408.01416"}]));
        assert_eq!(lines[3], json!({"type": "summary", "papers": 3, "known": 1, "errors": 0}));
    }

    #[test]
    fn year_is_an_integer_or_null() {
        let mut g = Graph::new(None);
        let mut p = paper("Y", Some("10.1/y"), None);
        p.year = "2021".into();
        g.add(p, 0, None, Some(Source::S2));
        g.add(paper("Z", Some("10.1/z"), None), 0, None, Some(Source::S2));
        let c = Closure { graph: g, errors: vec![], calls_ok: 1 };
        let lines = render(&c, &KnownIndex::default());
        assert_eq!(lines[0]["year"], 2021);
        assert_eq!(lines[1]["year"], Value::Null);
    }
}
