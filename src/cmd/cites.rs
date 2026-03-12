/// `lit cites <paper_id>` -- Get papers citing a given paper via Semantic Scholar.
///
/// Same as refs but uses the citations endpoint and `citingPaper` key.
/// Prints first 20 citing papers in `{rank}. {title} ({year}) - {author}` format.
/// With `--hops N`, performs BFS traversal up to N hops deep.

use super::Context;
use crate::api::{semantic_scholar, PaperResult};

/// Get citations and return as structured data.
pub async fn run_data(
    ctx: &Context,
    paper_id: &str,
    hops: usize,
    max_papers: usize,
) -> Result<Vec<PaperResult>, Box<dyn std::error::Error>> {
    super::fetch_related_data(
        ctx,
        paper_id,
        "citations",
        "cites",
        semantic_scholar::cites_url,
        semantic_scholar::parse_cites,
        hops,
        max_papers,
    )
    .await
}

pub async fn run(
    ctx: &Context,
    paper_id: &str,
    hops: usize,
    max_papers: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    super::fetch_related(
        ctx,
        paper_id,
        "citations",
        "cites",
        semantic_scholar::cites_url,
        semantic_scholar::parse_cites,
        hops,
        max_papers,
    )
    .await
}
