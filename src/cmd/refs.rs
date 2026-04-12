//! `lit refs <paper_id>` -- Get references of a paper via Semantic Scholar.
//!
//! If the paper_id matches a bare DOI pattern, the API module handles
//! prepending `DOI:`. Prints first 20 references in `{rank}. {title} ({year}) - {author}` format.
//! With `--hops N`, performs BFS traversal up to N hops deep.

use super::Context;
use crate::api::{semantic_scholar, PaperResult};

/// Get references and return as structured data.
pub async fn run_data(
    ctx: &Context,
    paper_id: &str,
    hops: usize,
    max_papers: usize,
) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>> {
    super::fetch_related_data(
        ctx,
        paper_id,
        "references",
        "refs",
        semantic_scholar::refs_url,
        semantic_scholar::parse_refs,
        hops,
        max_papers,
    )
    .await
}

/// Fetch and print references for a paper via Semantic Scholar.
pub async fn run(
    ctx: &Context,
    paper_id: &str,
    hops: usize,
    max_papers: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    super::fetch_related(
        ctx,
        paper_id,
        "references",
        "refs",
        semantic_scholar::refs_url,
        semantic_scholar::parse_refs,
        hops,
        max_papers,
    )
    .await
}
