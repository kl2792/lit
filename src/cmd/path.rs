/// `lit path <paper_a> <paper_b>` -- Find shortest citation path between two papers.
///
/// Uses bidirectional BFS: expands references from A and citations toward B,
/// meeting in the middle. Treats the citation graph as undirected (follows
/// both refs and cites at each hop) since two papers may be connected through
/// a shared reference even if neither cites the other.

use std::collections::{HashMap, HashSet, VecDeque};

use super::Context;
use crate::api::semantic_scholar;
use crate::db;

/// Maximum hops to search in each direction before giving up.
const MAX_HOPS: usize = 5;

/// Maximum total API calls before giving up.
const MAX_API_CALLS: usize = 200;

/// Find shortest citation path and return it as structured data.
///
/// Returns `Some(path)` where path is `Vec<(id, title)>`, or `None` if no path found.
pub async fn run_data(
    ctx: &Context,
    paper_a: &str,
    paper_b: &str,
    max_hops: usize,
) -> Result<Option<Vec<(String, String)>>, Box<dyn std::error::Error>> {
    let client = ctx.client();
    let max_hops = max_hops.min(MAX_HOPS);

    if ctx.verbose {
        crate::format::info(&format!(
            "Finding shortest path: {} → {} (max {} hops each direction)",
            paper_a, paper_b, max_hops
        ));
    }

    let mut parent_a: HashMap<String, Option<(String, String)>> = HashMap::new();
    let mut parent_b: HashMap<String, Option<(String, String)>> = HashMap::new();
    let mut titles: HashMap<String, String> = HashMap::new();

    parent_a.insert(paper_a.to_string(), None);
    parent_b.insert(paper_b.to_string(), None);

    let mut frontier_a: Vec<String> = vec![paper_a.to_string()];
    let mut frontier_b: Vec<String> = vec![paper_b.to_string()];

    let mut api_calls = 0usize;

    for hop in 0..max_hops {
        let mut next_a = Vec::new();
        for id in &frontier_a {
            if api_calls >= MAX_API_CALLS {
                break;
            }
            let neighbors = fetch_neighbors(&client, ctx, id, &mut api_calls).await;
            for (nid, ntitle) in neighbors {
                titles.insert(nid.clone(), ntitle.clone());
                if !parent_a.contains_key(&nid) {
                    parent_a.insert(nid.clone(), Some((id.clone(), ntitle)));
                    next_a.push(nid.clone());
                }
                if parent_b.contains_key(&nid) {
                    let path = reconstruct_path(&nid, &parent_a, &parent_b, &titles);
                    return Ok(Some(path));
                }
            }
        }
        frontier_a = next_a;

        let mut next_b = Vec::new();
        for id in &frontier_b {
            if api_calls >= MAX_API_CALLS {
                break;
            }
            let neighbors = fetch_neighbors(&client, ctx, id, &mut api_calls).await;
            for (nid, ntitle) in neighbors {
                titles.insert(nid.clone(), ntitle.clone());
                if !parent_b.contains_key(&nid) {
                    parent_b.insert(nid.clone(), Some((id.clone(), ntitle)));
                    next_b.push(nid.clone());
                }
                if parent_a.contains_key(&nid) {
                    let path = reconstruct_path(&nid, &parent_a, &parent_b, &titles);
                    return Ok(Some(path));
                }
            }
        }
        frontier_b = next_b;

        if frontier_a.is_empty() && frontier_b.is_empty() {
            break;
        }

        if ctx.verbose {
            eprintln!(
                "  hop {}: {} nodes from A, {} nodes from B ({} API calls)",
                hop + 1,
                parent_a.len(),
                parent_b.len(),
                api_calls
            );
        }
    }

    Ok(None)
}

pub async fn run(
    ctx: &Context,
    paper_a: &str,
    paper_b: &str,
    max_hops: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    match run_data(ctx, paper_a, paper_b, max_hops).await? {
        Some(path) => print_path(&path),
        None => println!("No path found within {} hops", max_hops.min(MAX_HOPS)),
    }
    Ok(())
}

/// Fetch both refs and cites for a paper, returning (id, title) pairs.
async fn fetch_neighbors(
    client: &crate::http::Client,
    ctx: &Context,
    paper_id: &str,
    api_calls: &mut usize,
) -> Vec<(String, String)> {
    let mut neighbors = Vec::new();

    // Fetch refs
    if *api_calls < MAX_API_CALLS {
        *api_calls += 1;
        let key = db::Db::cache_key("refs", paper_id);
        let url = semantic_scholar::refs_url(paper_id);
        if let Ok(body) = client.get_cached(&key, &url, db::TTL_SEARCH).await {
            if let Ok(results) = semantic_scholar::parse_refs(&body) {
                for p in &results {
                    if let Some(id) = super::s2_api_id(p) {
                        neighbors.push((id, p.title.clone()));
                    }
                }
            }
        }
    }

    // Fetch cites
    if *api_calls < MAX_API_CALLS {
        *api_calls += 1;
        let key = db::Db::cache_key("cites", paper_id);
        let url = semantic_scholar::cites_url(paper_id);
        if let Ok(body) = client.get_cached(&key, &url, db::TTL_SEARCH).await {
            if let Ok(results) = semantic_scholar::parse_cites(&body) {
                for p in &results {
                    if let Some(id) = super::s2_api_id(p) {
                        neighbors.push((id, p.title.clone()));
                    }
                }
            }
        }
    }

    neighbors
}

/// Reconstruct path from A → meeting_point → B using parent maps.
fn reconstruct_path(
    meeting: &str,
    parent_a: &HashMap<String, Option<(String, String)>>,
    parent_b: &HashMap<String, Option<(String, String)>>,
    titles: &HashMap<String, String>,
) -> Vec<(String, String)> {
    // Walk back from meeting point to A
    let mut path_a = Vec::new();
    let mut cur = meeting.to_string();
    loop {
        let title = titles.get(&cur).cloned().unwrap_or_else(|| cur.clone());
        path_a.push((cur.clone(), title));
        match parent_a.get(&cur) {
            Some(Some((parent, _))) => cur = parent.clone(),
            _ => break,
        }
    }
    path_a.reverse();

    // Walk back from meeting point to B (skip meeting point itself — already in path_a)
    let mut cur = meeting.to_string();
    loop {
        match parent_b.get(&cur) {
            Some(Some((parent, _))) => {
                cur = parent.clone();
                let title = titles.get(&cur).cloned().unwrap_or_else(|| cur.clone());
                path_a.push((cur.clone(), title));
            }
            _ => break,
        }
    }

    path_a
}

fn print_path(path: &[(String, String)]) {
    let hops = path.len() - 1;
    println!(
        "Shortest path: {} hop{}",
        hops,
        if hops == 1 { "" } else { "s" }
    );
    println!();
    for (i, (id, title)) in path.iter().enumerate() {
        let truncated = crate::format::truncate(title, 70);
        if i == 0 {
            println!("  {} ({})", truncated, id);
        } else {
            println!("  {} → {} ({})", "  ".repeat(i - 1), truncated, id);
        }
    }
}
