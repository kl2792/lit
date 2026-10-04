/// `lit cites <paper_id>` -- every paper citing a paper (Semantic Scholar,
/// OpenAlex fallback). With `--hops N`, BFS up to N hops deep.

use super::neighbors::Direction;
use super::{Context, Related};

/// Get citing papers and return as structured data.
pub async fn run_data(
    ctx: &Context,
    paper_id: &str,
    hops: usize,
    max_papers: usize,
) -> Result<Related, Box<dyn std::error::Error>> {
    super::fetch_related_data(ctx, paper_id, Direction::Cites, hops, max_papers).await
}

pub async fn run(
    ctx: &Context,
    paper_id: &str,
    hops: usize,
    max_papers: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    super::fetch_related(ctx, paper_id, Direction::Cites, hops, max_papers).await
}
