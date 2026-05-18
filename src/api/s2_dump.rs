//! Local Semantic Scholar bulk-dump backend (DuckDB).
//!
//! Stage A: connection wrapper + query functions backed by a local DuckDB file
//! (or in-memory database for tests). The download + ingest pipeline is
//! stubbed pending Stage B.
//!
//! Schema (loaded by Stage B from S2 `papers` / `citations` datasets):
//!
//! ```sql
//! CREATE TABLE papers (
//!     id              VARCHAR PRIMARY KEY,       -- S2 paperId
//!     title           VARCHAR,
//!     abstract        VARCHAR,
//!     year            INTEGER,
//!     venue           VARCHAR,
//!     citation_count  INTEGER,
//!     external_ids    VARCHAR                    -- JSON blob; {"ArXiv": "...", "DOI": "..."}
//! );
//! CREATE TABLE citations (
//!     citing_paper_id VARCHAR,
//!     cited_paper_id  VARCHAR
//! );
//! ```
//!
//! See `etc/projects/meta/lit-s2-dump-spec.md` for the full spec.

use std::path::{Path, PathBuf};

use duckdb::{params, params_from_iter, Connection};

/// Direction of closure traversal over the citation graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Papers that cite a seed (incoming edges).
    Cites,
    /// Papers that a seed cites (outgoing edges).
    Refs,
    /// Union of both directions.
    Both,
}

/// A paper row materialised from the local dump.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Paper {
    /// Semantic Scholar paper ID (the `id` column).
    pub id: String,
    pub title: String,
    pub abstract_text: Option<String>,
    pub year: Option<u32>,
    pub venue: Option<String>,
    pub citation_count: Option<u64>,
    /// Raw JSON blob from the `external_ids` column (arbitrary keys: `ArXiv`, `DOI`, ...).
    pub external_ids: Option<String>,
}

/// A paper produced by a multi-seed closure query, annotated with which seeds
/// it connects to and the seed count (used as the primary ranking key).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClosurePaper {
    pub paper: Paper,
    /// Seeds linked to this paper (in no particular order).
    pub linked_seeds: Vec<String>,
    /// `linked_seeds.len()` as `u32` (matches the SQL `seed_count` column).
    pub seed_count: u32,
}

/// Thin wrapper around a DuckDB connection with the lit-specific schema.
///
/// Owns the connection for the caller's lifetime; queries borrow immutably.
pub struct DuckDbConnection {
    conn: Connection,
}

impl DuckDbConnection {
    /// Open (or create) a DuckDB database at `path`.
    ///
    /// Does not ingest any data; use [`ensure_schema`](Self::ensure_schema) to
    /// create empty tables on a fresh database.
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let conn = Connection::open(path.as_ref())?;
        Ok(Self { conn })
    }

    /// Open an in-memory DuckDB database (for tests and ephemeral use).
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        Ok(Self { conn })
    }

    /// Borrow the underlying connection (for advanced callers and test fixtures).
    pub fn raw(&self) -> &Connection {
        &self.conn
    }

    /// Create the `papers` and `citations` tables if they don't exist.
    ///
    /// Safe to call on an existing database; uses `CREATE TABLE IF NOT EXISTS`.
    /// Stage B will populate these via `read_json_auto` from the downloaded
    /// S2 release.
    pub fn ensure_schema(&self) -> anyhow::Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS papers (
                id              VARCHAR PRIMARY KEY,
                title           VARCHAR,
                abstract        VARCHAR,
                year            INTEGER,
                venue           VARCHAR,
                citation_count  INTEGER,
                external_ids    VARCHAR
            );
            CREATE TABLE IF NOT EXISTS citations (
                citing_paper_id VARCHAR,
                cited_paper_id  VARCHAR
            );
            CREATE INDEX IF NOT EXISTS idx_citations_src ON citations(citing_paper_id);
            CREATE INDEX IF NOT EXISTS idx_citations_dst ON citations(cited_paper_id);
            "#,
        )?;
        Ok(())
    }

    /// Look up a single paper by its S2 paper ID.
    ///
    /// Returns `Ok(None)` if the paper is not in the dump (expected for recent
    /// papers that post-date the release), in which case callers should fall
    /// back to the S2 API.
    pub fn find_by_id(&self, paper_id: &str) -> anyhow::Result<Option<Paper>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, abstract, year, venue, citation_count, external_ids \
             FROM papers WHERE id = ?",
        )?;
        let mut rows = stmt.query(params![paper_id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row_to_paper(row)?))
        } else {
            Ok(None)
        }
    }

    /// Papers that cite `paper_id` (incoming edges).
    pub fn cites(&self, paper_id: &str, limit: usize) -> anyhow::Result<Vec<Paper>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.title, p.abstract, p.year, p.venue, p.citation_count, p.external_ids \
             FROM citations c JOIN papers p ON p.id = c.citing_paper_id \
             WHERE c.cited_paper_id = ? \
             ORDER BY p.citation_count DESC NULLS LAST LIMIT ?",
        )?;
        let mut rows = stmt.query(params![paper_id, limit as i64])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(row_to_paper(row)?);
        }
        Ok(out)
    }

    /// Papers that `paper_id` references (outgoing edges).
    pub fn refs(&self, paper_id: &str, limit: usize) -> anyhow::Result<Vec<Paper>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.title, p.abstract, p.year, p.venue, p.citation_count, p.external_ids \
             FROM citations c JOIN papers p ON p.id = c.cited_paper_id \
             WHERE c.citing_paper_id = ? \
             ORDER BY p.citation_count DESC NULLS LAST LIMIT ?",
        )?;
        let mut rows = stmt.query(params![paper_id, limit as i64])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(row_to_paper(row)?);
        }
        Ok(out)
    }

    /// Free-text search across titles and abstracts.
    ///
    /// Uses `ILIKE '%query%'` matching (DuckDB's case-insensitive `LIKE`).
    /// Results are ordered by `citation_count` descending.
    pub fn search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Paper>> {
        let needle = format!("%{}%", query);
        let mut stmt = self.conn.prepare(
            "SELECT id, title, abstract, year, venue, citation_count, external_ids \
             FROM papers \
             WHERE title ILIKE ? OR abstract ILIKE ? \
             ORDER BY citation_count DESC NULLS LAST LIMIT ?",
        )?;
        let mut rows = stmt.query(params![needle, needle, limit as i64])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(row_to_paper(row)?);
        }
        Ok(out)
    }

    /// Multi-seed closure query over the citation graph, with co-citation
    /// ranking.
    ///
    /// Returns papers connected to one or more seeds (in `direction`), filtered
    /// by `min_year`, ordered by `seed_count` DESC then `citation_count` DESC.
    ///
    /// The seed papers themselves are excluded from the result set.
    pub fn closure(
        &self,
        seeds: &[&str],
        direction: Direction,
        min_year: u32,
        limit: usize,
    ) -> anyhow::Result<Vec<ClosurePaper>> {
        if seeds.is_empty() {
            return Ok(Vec::new());
        }

        // DuckDB's rust binding supports VARCHAR arrays via `Value::List`, but
        // the simpler and schema-agnostic approach is to build an IN-list with
        // positional placeholders. Seeds are short S2 IDs; size is bounded by
        // the caller.
        let placeholders = (0..seeds.len()).map(|_| "?").collect::<Vec<_>>().join(",");

        // Build the closure CTE according to `direction`.
        let closure_cte = match direction {
            Direction::Cites => format!(
                "SELECT citing_paper_id AS id, cited_paper_id AS seed FROM citations \
                 WHERE cited_paper_id IN ({ph})",
                ph = placeholders,
            ),
            Direction::Refs => format!(
                "SELECT cited_paper_id AS id, citing_paper_id AS seed FROM citations \
                 WHERE citing_paper_id IN ({ph})",
                ph = placeholders,
            ),
            Direction::Both => format!(
                "SELECT citing_paper_id AS id, cited_paper_id AS seed FROM citations \
                 WHERE cited_paper_id IN ({ph}) \
                 UNION ALL \
                 SELECT cited_paper_id AS id, citing_paper_id AS seed FROM citations \
                 WHERE citing_paper_id IN ({ph})",
                ph = placeholders,
            ),
        };

        // Pre-dedup `(id, seed)` pairs in a subquery so we can use the plain
        // (non-DISTINCT) `string_agg` aggregate, which is the most portable
        // across DuckDB versions. The unit separator (`chr(31)`) is split on
        // the Rust side.
        let sql = format!(
            "WITH closure AS (SELECT DISTINCT id, seed FROM ({cte}) _c) \
             SELECT p.id, p.title, p.abstract, p.year, p.venue, p.citation_count, p.external_ids, \
                    string_agg(closure.seed, chr(31)) AS linked_seeds, \
                    count(closure.seed) AS seed_count \
             FROM closure JOIN papers p ON p.id = closure.id \
             WHERE (p.year IS NULL OR p.year >= ?) \
               AND p.id NOT IN ({seed_ph}) \
             GROUP BY p.id, p.title, p.abstract, p.year, p.venue, p.citation_count, p.external_ids \
             ORDER BY seed_count DESC, p.citation_count DESC NULLS LAST \
             LIMIT ?",
            cte = closure_cte,
            seed_ph = placeholders,
        );

        let mut stmt = self.conn.prepare(&sql)?;
        // Parameter order: CTE seeds (1 or 2 copies) then min_year then seed
        // exclusion list then limit.
        let mut params_vec: Vec<duckdb::types::Value> = Vec::new();
        let cte_copies = if direction == Direction::Both { 2 } else { 1 };
        for _ in 0..cte_copies {
            for s in seeds {
                params_vec.push(duckdb::types::Value::Text((*s).to_string()));
            }
        }
        params_vec.push(duckdb::types::Value::Int(min_year as i32));
        for s in seeds {
            params_vec.push(duckdb::types::Value::Text((*s).to_string()));
        }
        params_vec.push(duckdb::types::Value::BigInt(limit as i64));

        let mut rows = stmt.query(params_from_iter(params_vec.iter()))?;

        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let paper = row_to_paper(row)?;
            let linked_raw: Option<String> = row.get(7)?;
            let seed_count: i64 = row.get(8)?;
            let linked_seeds = match linked_raw {
                Some(s) => s
                    .split('\u{1f}')
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect(),
                None => Vec::new(),
            };
            out.push(ClosurePaper {
                paper,
                linked_seeds,
                seed_count: seed_count as u32,
            });
        }
        Ok(out)
    }
}

/// Materialise a query row into a [`Paper`].
///
/// Expects columns in the order:
/// `id, title, abstract, year, venue, citation_count, external_ids`.
fn row_to_paper(row: &duckdb::Row<'_>) -> anyhow::Result<Paper> {
    let year: Option<i32> = row.get(3)?;
    let citation_count: Option<i64> = row.get(5)?;
    Ok(Paper {
        id: row.get(0)?,
        title: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
        abstract_text: row.get(2)?,
        year: year.map(|y| y as u32),
        venue: row.get(4)?,
        citation_count: citation_count.map(|c| c as u64),
        external_ids: row.get(6)?,
    })
}

// ---------------------------------------------------------------------------
// Stage B stubs: download + ingest pipeline.
//
// These signatures are load-bearing for Stage B. Keep them stable; Stage B
// fills in bodies. Any signature change here requires a Stage B follow-up.
// ---------------------------------------------------------------------------

/// Where a release is installed on disk (`~/.lit/s2/<release-id>/`).
#[derive(Debug, Clone)]
pub struct ReleaseLayout {
    pub root: PathBuf,
    pub release_id: String,
}

/// Manifest written next to the DuckDB file after a successful load.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Manifest {
    pub release_id: String,
    /// RFC3339 timestamp of when the download/ingest finished.
    pub download_date: String,
    /// Record counts for sanity checks; see `lit dump status` in Stage C.
    pub papers: u64,
    pub citations: u64,
}

/// Discover the latest S2 release ID via the datasets API.
///
/// Stage B: implement via `GET https://api.semanticscholar.org/datasets/v1/release/latest`.
/// Requires `SEMANTIC_SCHOLAR_API_KEY` / `s2_api_key` config.
pub async fn latest_release_id(_api_key: &str) -> anyhow::Result<String> {
    // TODO(stage-b): implement.
    unimplemented!("Stage B: fetch latest release from S2 datasets API");
}

/// Download all required datasets (`papers`, `citations`, optional `tldrs`)
/// for `release_id` into `layout.root`.
///
/// Stage B: parallel (≤8 concurrent) signed-URL downloads with progress bar;
/// resumable per-file. See spec §"Download".
pub async fn download_release(
    _api_key: &str,
    _release_id: &str,
    _layout: &ReleaseLayout,
) -> anyhow::Result<()> {
    // TODO(stage-b): implement downloader.
    unimplemented!("Stage B: download S2 release datasets");
}

/// Ingest downloaded jsonl.gz files into a DuckDB database.
///
/// Stage B: use `read_json_auto('papers/*.jsonl.gz')` into the `papers` table,
/// and likewise for `citations`; create indices; write the manifest.
pub fn ingest_release(_layout: &ReleaseLayout, _db_path: &Path) -> anyhow::Result<Manifest> {
    // TODO(stage-b): implement ingest via DuckDB `read_json_auto`.
    unimplemented!("Stage B: ingest downloaded datasets into DuckDB");
}

/// Atomically swap the `current` symlink to `release_id`, moving the prior
/// `current` to `previous`.
///
/// Stage B: `std::os::unix::fs::symlink` + rename dance; spec §"Background
/// refresh mechanics".
pub fn promote_release(_root: &Path, _release_id: &str) -> anyhow::Result<()> {
    // TODO(stage-b): implement atomic symlink swap.
    unimplemented!("Stage B: promote release via atomic symlink swap");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a small in-memory dump with ~10 papers and ~30 citation edges.
    ///
    /// Graph shape (abbreviated):
    /// - `p1` (2020, Transformers) is a hub cited by p3..p10
    /// - `p2` (2019, BERT) cited by p4..p8
    /// - `p3` (2021) cites p1, p2; cited by p5, p9
    /// - miscellaneous edges to exercise closure ranking
    pub(super) fn fixture() -> DuckDbConnection {
        let conn = DuckDbConnection::open_in_memory().unwrap();
        conn.ensure_schema().unwrap();

        let papers: &[(&str, &str, Option<&str>, Option<u32>, Option<&str>, Option<u64>, Option<&str>)] = &[
            ("p1", "Attention Is All You Need", Some("Transformers propose self-attention for sequence modeling."), Some(2017), Some("NeurIPS"), Some(90000), Some(r#"{"ArXiv":"1706.03762"}"#)),
            ("p2", "BERT Pre-training", Some("Masked language modeling for bidirectional representations."), Some(2019), Some("NAACL"), Some(80000), Some(r#"{"ArXiv":"1810.04805"}"#)),
            ("p3", "Activation Patching", Some("Method for causal mediation analysis in transformers."), Some(2021), Some("NeurIPS"), Some(500), Some(r#"{"ArXiv":"2111.11111"}"#)),
            ("p4", "Locating Knowledge in GPT", Some("Rank-one model editing."), Some(2022), Some("NeurIPS"), Some(1200), None),
            ("p5", "Circuit Analysis", Some("Mechanistic interpretability study of induction heads."), Some(2022), Some("Anthropic"), Some(900), None),
            ("p6", "Probing Classifiers", Some("Linear probes for representation analysis."), Some(2020), Some("EMNLP"), Some(400), None),
            ("p7", "Causal Tracing", Some("Causal tracing in language models."), Some(2022), Some("ICLR"), Some(700), None),
            ("p8", "Scaling Laws", None, Some(2020), Some("arXiv"), Some(3500), None),
            ("p9", "Recent Follow-up", Some("Work building on activation patching."), Some(2023), Some("ICML"), Some(50), None),
            ("p10", "Older Work", Some("Pre-transformer era attention."), Some(2014), Some("ICLR"), Some(12000), None),
        ];

        for (id, title, abs_, year, venue, cc, ext) in papers {
            conn.raw().execute(
                "INSERT INTO papers (id, title, abstract, year, venue, citation_count, external_ids) VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![id, title, abs_, year.map(|y| y as i32), venue, cc.map(|c| c as i64), ext],
            ).unwrap();
        }

        // Citation edges: (citing, cited). ~30 edges.
        let edges: &[(&str, &str)] = &[
            // p1 is cited by almost everyone
            ("p3", "p1"), ("p4", "p1"), ("p5", "p1"), ("p6", "p1"),
            ("p7", "p1"), ("p8", "p1"), ("p9", "p1"), ("p10", "p1"),
            // p2 cited by several
            ("p3", "p2"), ("p4", "p2"), ("p5", "p2"), ("p6", "p2"),
            ("p7", "p2"), ("p8", "p2"),
            // p3 cites and is cited
            ("p5", "p3"), ("p9", "p3"), ("p7", "p3"),
            ("p3", "p10"),
            // p4 relationships
            ("p9", "p4"), ("p5", "p4"), ("p7", "p4"),
            // misc
            ("p6", "p10"), ("p8", "p10"), ("p9", "p5"),
            ("p9", "p7"), ("p9", "p2"),
            // duplicates (should not crash; dedup is caller's job)
            ("p3", "p1"),
            // p5 cites p2 already counted; add p8->p5
            ("p8", "p5"),
            ("p7", "p6"),
        ];
        for (citing, cited) in edges {
            conn.raw().execute(
                "INSERT INTO citations (citing_paper_id, cited_paper_id) VALUES (?, ?)",
                params![citing, cited],
            ).unwrap();
        }
        conn
    }

    #[test]
    fn find_by_id_hit_and_miss() {
        let db = fixture();
        let p = db.find_by_id("p1").unwrap().unwrap();
        assert_eq!(p.id, "p1");
        assert_eq!(p.title, "Attention Is All You Need");
        assert_eq!(p.year, Some(2017));
        assert_eq!(p.citation_count, Some(90000));
        assert!(p.external_ids.as_deref().unwrap().contains("1706.03762"));

        assert!(db.find_by_id("nonexistent").unwrap().is_none());
    }

    #[test]
    fn cites_returns_citers_ordered_by_citation_count() {
        let db = fixture();
        let citers = db.cites("p1", 100).unwrap();
        // Everyone from p3..p10 cites p1 (duplicate edge p3->p1 produces two rows;
        // dedup is not the query's job).
        let ids: Vec<&str> = citers.iter().map(|p| p.id.as_str()).collect();
        for expected in &["p3", "p4", "p5", "p6", "p7", "p8", "p9", "p10"] {
            assert!(ids.contains(expected), "missing {} in {:?}", expected, ids);
        }
        // Ordering: p10 (12000 cc) should appear before p3 (500 cc).
        let pos_p10 = ids.iter().position(|i| *i == "p10").unwrap();
        let pos_p3 = ids.iter().position(|i| *i == "p3").unwrap();
        assert!(pos_p10 < pos_p3, "p10 should rank above p3 by citation_count");
    }

    #[test]
    fn cites_honours_limit() {
        let db = fixture();
        let citers = db.cites("p1", 3).unwrap();
        assert_eq!(citers.len(), 3);
    }

    #[test]
    fn refs_returns_references() {
        let db = fixture();
        // p3 references p1, p2, p10 (and a duplicate p1 edge).
        let refs = db.refs("p3", 100).unwrap();
        let ids: Vec<&str> = refs.iter().map(|p| p.id.as_str()).collect();
        for expected in &["p1", "p2", "p10"] {
            assert!(ids.contains(expected), "missing {}", expected);
        }
    }

    #[test]
    fn search_matches_title_and_abstract_case_insensitive() {
        let db = fixture();
        // Title match.
        let r = db.search("BERT", 10).unwrap();
        assert!(r.iter().any(|p| p.id == "p2"));

        // Abstract match (lowercase, uppercase query).
        let r = db.search("INDUCTION", 10).unwrap();
        assert!(r.iter().any(|p| p.id == "p5"));

        // No match.
        let r = db.search("quantumchromodynamics", 10).unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn search_ordered_by_citation_count() {
        let db = fixture();
        // Every abstract contains at least some common words; filter to "attention".
        // p1.abstract has "attention" (via "self-attention"), p10.abstract has "attention".
        let r = db.search("attention", 10).unwrap();
        // p1 (90000) should come before p10 (12000).
        let pos_p1 = r.iter().position(|p| p.id == "p1").unwrap();
        let pos_p10 = r.iter().position(|p| p.id == "p10").unwrap();
        assert!(pos_p1 < pos_p10);
    }

    #[test]
    fn closure_cites_direction() {
        let db = fixture();
        // Closure over {p1, p2}, Cites direction: papers that cite either seed.
        let rows = db.closure(&["p1", "p2"], Direction::Cites, 0, 100).unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.paper.id.as_str()).collect();
        // p3..p8 cite both; should rank above single-seed citers.
        assert!(ids.contains(&"p3"));
        let p3 = rows.iter().find(|r| r.paper.id == "p3").unwrap();
        assert_eq!(p3.seed_count, 2);
        assert!(p3.linked_seeds.iter().any(|s| s == "p1"));
        assert!(p3.linked_seeds.iter().any(|s| s == "p2"));

        // Seeds themselves must not appear.
        assert!(!ids.contains(&"p1"));
        assert!(!ids.contains(&"p2"));
    }

    #[test]
    fn closure_ranks_by_seed_count_then_citation_count() {
        let db = fixture();
        let rows = db.closure(&["p1", "p2"], Direction::Cites, 0, 100).unwrap();
        // First rows should have seed_count=2.
        assert_eq!(rows.first().unwrap().seed_count, 2);
        // Sequence of seed_counts must be non-increasing.
        let seeds: Vec<u32> = rows.iter().map(|r| r.seed_count).collect();
        for w in seeds.windows(2) {
            assert!(w[0] >= w[1], "seed_count not monotonically non-increasing: {:?}", seeds);
        }
    }

    #[test]
    fn closure_respects_min_year() {
        let db = fixture();
        let rows = db.closure(&["p1"], Direction::Cites, 2022, 100).unwrap();
        for r in &rows {
            // Papers with a year must satisfy the filter.
            if let Some(y) = r.paper.year {
                assert!(y >= 2022, "paper {} has year {} < 2022", r.paper.id, y);
            }
        }
        // p3 (2021) must be excluded; p4 (2022) included.
        let ids: Vec<&str> = rows.iter().map(|r| r.paper.id.as_str()).collect();
        assert!(!ids.contains(&"p3"));
        assert!(ids.contains(&"p4"));
    }

    #[test]
    fn closure_both_direction_unions_cites_and_refs() {
        let db = fixture();
        // From seed p3: Cites gives papers that cite p3 (p5, p7, p9);
        // Refs gives papers p3 references (p1, p2, p10).
        let rows = db.closure(&["p3"], Direction::Both, 0, 100).unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.paper.id.as_str()).collect();
        for expected in &["p5", "p7", "p9", "p1", "p2", "p10"] {
            assert!(ids.contains(expected), "missing {} in both-direction closure", expected);
        }
    }

    #[test]
    fn closure_empty_seeds_returns_empty() {
        let db = fixture();
        let rows = db.closure(&[], Direction::Cites, 0, 100).unwrap();
        assert!(rows.is_empty());
    }
}
