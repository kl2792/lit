/// SQLite database backend for the lit tool.
///
/// Replaces the filesystem-based `Cache` with a single SQLite database that
/// stores papers, citations, API cache entries, and full-text search indices.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use md5::{Digest, Md5};
use rusqlite::{params, Connection, OptionalExtension};

/// TTL for search results: 24 hours.
pub const TTL_SEARCH: u64 = 86400;

/// TTL for DOI/arXiv/ISBN lookups: 7 days.
pub const TTL_DOI: u64 = 604800;

const SCHEMA_VERSION: &str = "2";

pub struct Db {
    conn: Mutex<Connection>,
    in_bulk: AtomicBool,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db")
            .field("in_bulk", &self.in_bulk.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Db {
    /// Open (or create) a SQLite database at `path`.
    ///
    /// Sets pragmas, creates tables/indices/triggers if missing, checks schema
    /// version, and evicts stale unreferenced cache entries.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).context("failed to open database")?;

        // Pragmas
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;",
        )?;

        // Create tables
        conn.execute_batch(SCHEMA_SQL)?;

        // Create indices
        conn.execute_batch(INDICES_SQL)?;

        // Create FTS triggers
        conn.execute_batch(FTS_TRIGGERS_SQL)?;

        // Check schema version
        let version: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .ok();

        match version {
            Some(v) if v != SCHEMA_VERSION => {
                bail!(
                    "Schema version mismatch (have {v}, need {SCHEMA_VERSION}). \
                     Run `lit db rebuild`."
                );
            }
            None => {
                conn.execute(
                    "INSERT INTO meta (key, value) VALUES ('schema_version', ?)",
                    params![SCHEMA_VERSION],
                )?;
            }
            _ => {}
        }

        // Evict unreferenced cache entries older than 90 days
        conn.execute(
            "DELETE FROM api_cache
             WHERE strftime('%s','now') - strftime('%s', fetched_at) > 90 * 86400
               AND cache_key NOT IN (SELECT cache_key FROM paper_sources WHERE cache_key IS NOT NULL)
               AND cache_key NOT IN (SELECT cache_key FROM citation_sources WHERE cache_key IS NOT NULL)",
            [],
        )?;

        Ok(Db {
            conn: Mutex::new(conn),
            in_bulk: AtomicBool::new(false),
        })
    }

    /// Open an in-memory SQLite database for testing.
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("failed to open in-memory database")?;
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;",
        )?;
        conn.execute_batch(SCHEMA_SQL)?;
        conn.execute_batch(INDICES_SQL)?;
        conn.execute_batch(FTS_TRIGGERS_SQL)?;
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('schema_version', ?)",
            params![SCHEMA_VERSION],
        )?;
        Ok(Db {
            conn: Mutex::new(conn),
            in_bulk: AtomicBool::new(false),
        })
    }

    /// Return cached body if `key` exists and is younger than `ttl` seconds.
    pub fn cache_get(&self, key: &str, ttl: u64) -> Option<String> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row(
            "SELECT body FROM api_cache
             WHERE cache_key = ?
               AND strftime('%s','now') - strftime('%s', fetched_at) < ?",
            params![key, ttl],
            |row| row.get(0),
        )
        .ok()
    }

    /// Insert or replace a cache entry.
    pub fn cache_set(&self, key: &str, url: &str, body: &str) {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = conn.execute(
            "INSERT OR REPLACE INTO api_cache (cache_key, url, body, fetched_at)
             VALUES (?, ?, ?, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))",
            params![key, url, body],
        ) {
            eprintln!("warning: failed to write cache: {}", e);
        }
    }

    /// Compute a cache key: `{prefix}_{first 16 hex chars of md5(input + "\n")}`.
    ///
    /// Matches the legacy `Cache::key` algorithm for migration compatibility.
    pub fn cache_key(prefix: &str, input: &str) -> String {
        let mut hasher = Md5::new();
        hasher.update(input.as_bytes());
        hasher.update(b"\n");
        let hash = hasher.finalize();
        let hex = format!("{:x}", hash);
        format!("{}_{}", prefix, &hex[..16])
    }

    /// Migrate entries from a filesystem cache directory into the database.
    ///
    /// Reads each file in `cache_dir`, uses the filename as the cache key,
    /// and inserts into api_cache. Skips entries that already exist.
    /// Returns the number of entries migrated.
    pub fn migrate_from_cache_dir(&self, cache_dir: &Path) -> Result<usize> {
        let entries = match std::fs::read_dir(cache_dir) {
            Ok(e) => e,
            Err(_) => return Ok(0), // directory doesn't exist — nothing to migrate
        };

        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut count = 0;

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let key = match path.file_name().and_then(|n| n.to_str()) {
                Some(k) => k.to_string(),
                None => continue,
            };

            // Skip if already in DB
            let exists: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM api_cache WHERE cache_key = ?)",
                    params![key],
                    |row| row.get(0),
                )
                .unwrap_or(false);
            if exists {
                continue;
            }

            let body = match std::fs::read_to_string(&path) {
                Ok(b) => b,
                Err(_) => continue,
            };

            conn.execute(
                "INSERT OR IGNORE INTO api_cache (cache_key, url, body, fetched_at)
                 VALUES (?, '', ?, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))",
                params![key, body],
            )?;
            count += 1;
        }

        Ok(count)
    }

    /// Enter bulk mode: drop FTS triggers and suppress changelog writes.
    pub fn begin_bulk(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "DROP TRIGGER IF EXISTS papers_fts_insert;
             DROP TRIGGER IF EXISTS papers_fts_delete;
             DROP TRIGGER IF EXISTS papers_fts_update;
             DROP TRIGGER IF EXISTS papers_fts_soft_delete;",
        )?;
        self.in_bulk.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Exit bulk mode: rebuild FTS index, recreate triggers, checkpoint WAL.
    pub fn end_bulk(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch("INSERT INTO papers_fts(papers_fts) VALUES('rebuild');")?;
        conn.execute_batch(FTS_TRIGGERS_SQL)?;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        self.in_bulk.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Insert or update a paper, deduplicating on doi/arxiv_id/isbn.
    ///
    /// Identity resolution: matches on doi, arxiv_id, or isbn (UNIQUE constraints).
    /// For s2_id and openalex_id, sets if null; if non-null and different, keeps
    /// existing and logs a warning. Returns the paper's row id.
    pub fn upsert_paper(&self, paper: &PaperRow, source: Option<&str>) -> Result<i64> {
        let title = if paper.title.len() > 2000 {
            &paper.title[..2000]
        } else {
            &paper.title
        };

        let year = paper.year.as_deref().and_then(|y| {
            let trimmed = y.trim();
            if trimmed.len() == 4 && trimmed.chars().all(|c| c.is_ascii_digit()) {
                Some(trimmed.to_string())
            } else {
                None
            }
        });

        // Cap authors array at 500 entries and compute authors_text
        let authors_text = compute_authors_text(&paper.authors, 500);

        let entry_type = paper
            .entry_type
            .as_deref()
            .map(|s| s.to_lowercase());

        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());

        // Try to find existing paper by doi, arxiv_id, or isbn
        let existing_id: Option<i64> = if let Some(doi) = &paper.doi {
            conn.query_row(
                "SELECT id FROM papers WHERE doi = ?1 AND deleted_at IS NULL",
                params![doi],
                |row| row.get(0),
            )
            .optional()?
        } else {
            None
        };

        let existing_id = match existing_id {
            Some(id) => Some(id),
            None => {
                if let Some(arxiv_id) = &paper.arxiv_id {
                    conn.query_row(
                        "SELECT id FROM papers WHERE arxiv_id = ?1 AND deleted_at IS NULL",
                        params![arxiv_id],
                        |row| row.get(0),
                    )
                    .optional()?
                } else {
                    None
                }
            }
        };

        let existing_id = match existing_id {
            Some(id) => Some(id),
            None => {
                if let Some(isbn) = &paper.isbn {
                    conn.query_row(
                        "SELECT id FROM papers WHERE isbn = ?1 AND deleted_at IS NULL",
                        params![isbn],
                        |row| row.get(0),
                    )
                    .optional()?
                } else {
                    None
                }
            }
        };

        let result_id = if let Some(id) = existing_id {
            // Cross-reference ID updates: set-if-null, warn if different
            let (existing_s2, existing_oa): (Option<String>, Option<String>) = conn.query_row(
                "SELECT s2_id, openalex_id FROM papers WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;

            let s2_id = cross_ref_id("s2_id", &existing_s2, &paper.s2_id);
            let openalex_id = cross_ref_id("openalex_id", &existing_oa, &paper.openalex_id);

            conn.execute(
                "UPDATE papers SET
                    entry_type    = COALESCE(?1, entry_type),
                    title         = ?2,
                    authors       = ?3,
                    authors_text  = ?4,
                    year          = COALESCE(?5, year),
                    doi           = COALESCE(?6, doi),
                    arxiv_id      = COALESCE(?7, arxiv_id),
                    isbn          = COALESCE(?8, isbn),
                    s2_id         = ?9,
                    openalex_id   = ?10,
                    journal       = COALESCE(?11, journal),
                    booktitle     = COALESCE(?12, booktitle),
                    publisher     = COALESCE(?13, publisher),
                    volume        = COALESCE(?14, volume),
                    number        = COALESCE(?15, number),
                    pages         = COALESCE(?16, pages),
                    abstract      = COALESCE(?17, abstract),
                    url           = COALESCE(?18, url),
                    pdf_url       = COALESCE(?19, pdf_url),
                    categories    = COALESCE(?20, categories),
                    citation_count = COALESCE(?21, citation_count),
                    updated_at    = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
                WHERE id = ?22",
                params![
                    entry_type,
                    title,
                    paper.authors,
                    authors_text,
                    year,
                    paper.doi,
                    paper.arxiv_id,
                    paper.isbn,
                    s2_id,
                    openalex_id,
                    paper.journal,
                    paper.booktitle,
                    paper.publisher,
                    paper.volume,
                    paper.number,
                    paper.pages,
                    paper.r#abstract,
                    paper.url,
                    paper.pdf_url,
                    paper.categories,
                    paper.citation_count,
                    id,
                ],
            )?;
            id
        } else {
            conn.execute(
                "INSERT INTO papers (
                    entry_type, title, authors, authors_text, year,
                    doi, arxiv_id, isbn, s2_id, openalex_id,
                    journal, booktitle, publisher, volume, number, pages,
                    abstract, url, pdf_url, categories, citation_count
                ) VALUES (
                    ?1, ?2, ?3, ?4, ?5,
                    ?6, ?7, ?8, ?9, ?10,
                    ?11, ?12, ?13, ?14, ?15, ?16,
                    ?17, ?18, ?19, ?20, ?21
                )",
                params![
                    entry_type,
                    title,
                    paper.authors,
                    authors_text,
                    year,
                    paper.doi,
                    paper.arxiv_id,
                    paper.isbn,
                    paper.s2_id,
                    paper.openalex_id,
                    paper.journal,
                    paper.booktitle,
                    paper.publisher,
                    paper.volume,
                    paper.number,
                    paper.pages,
                    paper.r#abstract,
                    paper.url,
                    paper.pdf_url,
                    paper.categories,
                    paper.citation_count,
                ],
            )?;
            conn.last_insert_rowid()
        };

        // Insert claims if source is provided
        if let Some(src) = source {
            let mut claims: Vec<(&str, String)> = Vec::new();
            claims.push(("title", paper.title.clone()));
            if !paper.authors.is_empty() {
                claims.push(("authors", paper.authors.clone()));
            }
            if let Some(ref v) = paper.year {
                claims.push(("year", v.clone()));
            }
            if let Some(ref v) = paper.doi {
                claims.push(("doi", v.clone()));
            }
            if let Some(ref v) = paper.arxiv_id {
                claims.push(("arxiv_id", v.clone()));
            }
            if let Some(ref v) = paper.isbn {
                claims.push(("isbn", v.clone()));
            }
            if let Some(ref v) = paper.s2_id {
                claims.push(("s2_id", v.clone()));
            }
            if let Some(ref v) = paper.openalex_id {
                claims.push(("openalex_id", v.clone()));
            }
            if let Some(ref v) = paper.journal {
                claims.push(("journal", v.clone()));
            }
            if let Some(ref v) = paper.booktitle {
                claims.push(("booktitle", v.clone()));
            }
            if let Some(ref v) = paper.publisher {
                claims.push(("publisher", v.clone()));
            }
            if let Some(ref v) = paper.volume {
                claims.push(("volume", v.clone()));
            }
            if let Some(ref v) = paper.number {
                claims.push(("number", v.clone()));
            }
            if let Some(ref v) = paper.pages {
                claims.push(("pages", v.clone()));
            }
            if let Some(ref v) = paper.r#abstract {
                claims.push(("abstract", v.clone()));
            }
            if let Some(ref v) = paper.url {
                claims.push(("url", v.clone()));
            }
            if let Some(ref v) = paper.pdf_url {
                claims.push(("pdf_url", v.clone()));
            }
            if let Some(ref v) = paper.categories {
                claims.push(("categories", v.clone()));
            }
            if let Some(v) = paper.citation_count {
                claims.push(("citation_count", v.to_string()));
            }

            for (field, value) in &claims {
                conn.execute(
                    "INSERT OR REPLACE INTO paper_claims (paper_id, field, value, source)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![result_id, field, value, src],
                )?;
            }

            // Resolve citation_count as MAX of all claims
            if paper.citation_count.is_some() {
                conn.execute(
                    "UPDATE papers SET citation_count = (
                        SELECT MAX(CAST(value AS INTEGER)) FROM paper_claims
                        WHERE paper_id = ?1 AND field = 'citation_count'
                    ) WHERE id = ?1",
                    params![result_id],
                )?;
            }
        }

        Ok(result_id)
    }

    /// Record per-source field values for a paper. Overwrites previous claims from the same source.
    pub fn insert_claims(&self, paper_id: i64, source: &str, claims: &[(&str, &str)]) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        for &(field, value) in claims {
            conn.execute(
                "INSERT OR REPLACE INTO paper_claims (paper_id, field, value, source)
                 VALUES (?1, ?2, ?3, ?4)",
                params![paper_id, field, value, source],
            )?;
        }
        Ok(())
    }

    /// Get all claims for a paper+field, ordered by fetched_at desc.
    ///
    /// Returns Vec<(value, source, fetched_at)>.
    pub fn get_claims(&self, paper_id: i64, field: &str) -> Result<Vec<(String, String, String)>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT value, source, fetched_at FROM paper_claims
             WHERE paper_id = ?1 AND field = ?2
             ORDER BY fetched_at DESC",
        )?;
        let rows = stmt.query_map(params![paper_id, field], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Full-text search of local papers using FTS5.
    ///
    /// Returns up to `limit` non-deleted papers matching the FTS5 query,
    /// ranked by bm25 relevance.
    pub fn search_local(&self, query: &str, limit: usize) -> Result<Vec<PaperRow>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT p.id, p.entry_type, p.title, p.authors, p.year,
                    p.doi, p.arxiv_id, p.isbn, p.s2_id, p.openalex_id,
                    p.journal, p.booktitle, p.publisher, p.volume, p.number, p.pages,
                    p.abstract, p.url, p.pdf_url, p.categories, p.citation_count,
                    p.local_path
             FROM papers p
             JOIN papers_fts ON papers_fts.rowid = p.id
             WHERE papers_fts MATCH ?1
               AND p.deleted_at IS NULL
             ORDER BY bm25(papers_fts)
             LIMIT ?2",
        )?;
        Self::collect_paper_rows(&mut stmt, params![query, limit])
    }

    /// Insert a citation edge (source cites target). Idempotent.
    pub fn insert_citation(&self, source_id: i64, target_id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT OR IGNORE INTO citations (source_id, target_id) VALUES (?1, ?2)",
            params![source_id, target_id],
        )?;
        Ok(())
    }

    /// Get all references (papers cited BY this paper).
    pub fn get_refs(&self, paper_id: i64) -> Result<Vec<PaperRow>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT p.id, p.entry_type, p.title, p.authors, p.year,
                    p.doi, p.arxiv_id, p.isbn, p.s2_id, p.openalex_id,
                    p.journal, p.booktitle, p.publisher, p.volume, p.number, p.pages,
                    p.abstract, p.url, p.pdf_url, p.categories, p.citation_count,
                    p.local_path
             FROM papers p
             JOIN citations c ON c.target_id = p.id
             WHERE c.source_id = ?1
               AND p.deleted_at IS NULL",
        )?;
        Self::collect_paper_rows(&mut stmt, params![paper_id])
    }

    /// Get all citations (papers that CITE this paper).
    pub fn get_cites(&self, paper_id: i64) -> Result<Vec<PaperRow>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT p.id, p.entry_type, p.title, p.authors, p.year,
                    p.doi, p.arxiv_id, p.isbn, p.s2_id, p.openalex_id,
                    p.journal, p.booktitle, p.publisher, p.volume, p.number, p.pages,
                    p.abstract, p.url, p.pdf_url, p.categories, p.citation_count,
                    p.local_path
             FROM papers p
             JOIN citations c ON c.source_id = p.id
             WHERE c.target_id = ?1
               AND p.deleted_at IS NULL",
        )?;
        Self::collect_paper_rows(&mut stmt, params![paper_id])
    }

    /// Collect PaperRow results from a prepared statement.
    fn collect_paper_rows(
        stmt: &mut rusqlite::Statement<'_>,
        params: impl rusqlite::Params,
    ) -> Result<Vec<PaperRow>> {
        let rows = stmt.query_map(params, |row| {
            Ok(PaperRow {
                id: Some(row.get(0)?),
                entry_type: row.get(1)?,
                title: row.get::<_, String>(2).unwrap_or_default(),
                authors: row.get::<_, String>(3).unwrap_or_default(),
                year: row.get(4)?,
                doi: row.get(5)?,
                arxiv_id: row.get(6)?,
                isbn: row.get(7)?,
                s2_id: row.get(8)?,
                openalex_id: row.get(9)?,
                journal: row.get(10)?,
                booktitle: row.get(11)?,
                publisher: row.get(12)?,
                volume: row.get(13)?,
                number: row.get(14)?,
                pages: row.get(15)?,
                r#abstract: row.get(16)?,
                url: row.get(17)?,
                pdf_url: row.get(18)?,
                categories: row.get(19)?,
                citation_count: row.get(20)?,
                local_path: row.get(21)?,
            })
        })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Return all papers with a non-null local_path as (id, path) pairs.
    pub fn papers_with_local_path(&self) -> Result<Vec<(i64, String)>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, local_path FROM papers
             WHERE local_path IS NOT NULL AND deleted_at IS NULL",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Set local_path to NULL for a paper.
    pub fn clear_local_path(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE papers SET local_path = NULL WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    /// Check if any non-deleted paper has the given local_path.
    pub fn has_paper_with_local_path(&self, path: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM papers WHERE local_path = ?1 AND deleted_at IS NULL",
            params![path],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Set local_path for a paper.
    pub fn set_local_path(&self, id: i64, path: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE papers SET local_path = ?1 WHERE id = ?2",
            params![path, id],
        )?;
        Ok(())
    }

    /// Return statistics about the database.
    pub fn db_stats(&self) -> Result<DbStats> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());

        let paper_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM papers WHERE deleted_at IS NULL",
            [],
            |row| row.get(0),
        )?;

        let citation_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM citations",
            [],
            |row| row.get(0),
        )?;

        let cache_entries: i64 = conn.query_row(
            "SELECT COUNT(*) FROM api_cache",
            [],
            |row| row.get(0),
        )?;

        // page_count * page_size gives bytes
        let db_size: i64 = conn.query_row(
            "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
            [],
            |row| row.get(0),
        )?;

        Ok(DbStats {
            paper_count,
            citation_count,
            cache_entries,
            db_size_bytes: db_size,
        })
    }

    /// Find fields with conflicting claims across sources.
    ///
    /// Returns Vec<(paper_id, title, field, Vec<(value, source)>)> for fields
    /// where different sources report different values.
    pub fn find_claim_conflicts(&self) -> Result<Vec<(i64, String, String, Vec<(String, String)>)>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());

        // Find (paper_id, field) pairs with >1 distinct value
        let mut conflict_stmt = conn.prepare(
            "SELECT pc.paper_id, p.title, pc.field
             FROM paper_claims pc
             JOIN papers p ON p.id = pc.paper_id AND p.deleted_at IS NULL
             GROUP BY pc.paper_id, pc.field
             HAVING COUNT(DISTINCT value) > 1
             ORDER BY pc.paper_id, pc.field",
        )?;

        let conflict_keys: Vec<(i64, String, String)> = conflict_stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .filter_map(|r| r.ok())
            .collect();

        // For each conflict, get all (value, source) pairs
        let mut claim_stmt = conn.prepare(
            "SELECT value, source FROM paper_claims
             WHERE paper_id = ?1 AND field = ?2
             ORDER BY fetched_at DESC",
        )?;

        let mut results = Vec::new();
        for (paper_id, title, field) in conflict_keys {
            let claims: Vec<(String, String)> = claim_stmt
                .query_map(params![paper_id, &field], |row| Ok((row.get(0)?, row.get(1)?)))?
                .filter_map(|r| r.ok())
                .collect();
            results.push((paper_id, title, field, claims));
        }

        Ok(results)
    }

    /// Return papers that have multiple entries in `paper_sources`, along with
    /// the source name and cached response body for each.
    ///
    /// Returns `(paper_id, title, Vec<(source, body)>)` for papers with 2+ sources.
    pub fn papers_with_multiple_sources(&self) -> Result<Vec<(i64, String, Vec<(String, String)>)>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());

        // Find paper IDs with multiple sources.
        let mut id_stmt = conn.prepare(
            "SELECT ps.paper_id, p.title
             FROM paper_sources ps
             JOIN papers p ON p.id = ps.paper_id AND p.deleted_at IS NULL
             GROUP BY ps.paper_id
             HAVING COUNT(*) > 1"
        )?;
        let papers: Vec<(i64, String)> = id_stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();

        // For each paper, get source + cached body.
        let mut src_stmt = conn.prepare(
            "SELECT ps.source, COALESCE(ac.body, '')
             FROM paper_sources ps
             LEFT JOIN api_cache ac ON ac.cache_key = ps.cache_key
             WHERE ps.paper_id = ?1"
        )?;

        let mut result = Vec::new();
        for (id, title) in papers {
            let sources: Vec<(String, String)> = src_stmt
                .query_map(params![id], |row| Ok((row.get(0)?, row.get(1)?)))?
                .filter_map(|r| r.ok())
                .collect();
            if sources.len() >= 2 {
                result.push((id, title, sources));
            }
        }

        Ok(result)
    }
}

/// A row in the `papers` table.
#[derive(Debug, Clone, Default)]
pub struct PaperRow {
    pub id: Option<i64>,
    pub entry_type: Option<String>,
    pub title: String,
    pub authors: String,
    pub year: Option<String>,
    pub doi: Option<String>,
    pub arxiv_id: Option<String>,
    pub isbn: Option<String>,
    pub s2_id: Option<String>,
    pub openalex_id: Option<String>,
    pub journal: Option<String>,
    pub booktitle: Option<String>,
    pub publisher: Option<String>,
    pub volume: Option<String>,
    pub number: Option<String>,
    pub pages: Option<String>,
    pub r#abstract: Option<String>,
    pub url: Option<String>,
    pub pdf_url: Option<String>,
    pub categories: Option<String>,
    pub citation_count: Option<i64>,
    pub local_path: Option<String>,
}

/// Database statistics.
#[derive(Debug, Clone)]
pub struct DbStats {
    pub paper_count: i64,
    pub citation_count: i64,
    pub cache_entries: i64,
    pub db_size_bytes: i64,
}

/// Compute `authors_text` from a JSON array of author names, capping at `max`.
fn compute_authors_text(authors_json: &str, max: usize) -> String {
    if let Ok(arr) = serde_json::from_str::<Vec<String>>(authors_json) {
        let capped: Vec<&str> = arr.iter().take(max).map(|s| s.as_str()).collect();
        capped.join(", ")
    } else {
        // Not valid JSON array — use as-is (plain text author string)
        authors_json.to_string()
    }
}

/// Resolve cross-reference IDs: set-if-null, warn if different, keep existing.
fn cross_ref_id(
    field: &str,
    existing: &Option<String>,
    incoming: &Option<String>,
) -> Option<String> {
    match (existing, incoming) {
        (None, new) => new.clone(),
        (Some(old), Some(new)) if old != new => {
            eprintln!(
                "warning: {field} mismatch: existing={old}, incoming={new} — keeping existing"
            );
            Some(old.clone())
        }
        (old, _) => old.clone(),
    }
}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

impl From<&crate::api::PaperResult> for PaperRow {
    fn from(p: &crate::api::PaperResult) -> Self {
        let authors_json = serde_json::to_string(&p.authors).unwrap_or_default();
        let year = if p.year == "?" { None } else { Some(p.year.clone()) };
        PaperRow {
            title: p.title.clone(),
            authors: authors_json,
            year,
            doi: p.doi.clone(),
            arxiv_id: p.arxiv_id.clone(),
            isbn: p.isbn.clone(),
            s2_id: p.s2_id.clone(),
            r#abstract: p.abstract_text.clone(),
            pdf_url: p.pdf_url.clone(),
            citation_count: p.citations.map(|c| c as i64),
            journal: p.venue.clone(),
            categories: if p.categories.is_empty() {
                None
            } else {
                Some(p.categories.join(", "))
            },
            ..Default::default()
        }
    }
}

impl PaperRow {
    /// Convert to a PaperResult for display purposes.
    pub fn to_paper_result(&self) -> crate::api::PaperResult {
        let authors: Vec<String> = serde_json::from_str(&self.authors).unwrap_or_default();
        crate::api::PaperResult {
            title: self.title.clone(),
            authors,
            year: self.year.clone().unwrap_or_else(|| "?".to_string()),
            doi: self.doi.clone(),
            arxiv_id: self.arxiv_id.clone(),
            isbn: self.isbn.clone(),
            s2_id: self.s2_id.clone(),
            citations: self.citation_count.map(|c| c as u64),
            venue: self.journal.clone(),
            pdf_url: self.pdf_url.clone(),
            abstract_text: self.r#abstract.clone(),
            categories: self
                .categories
                .as_deref()
                .map(|s| s.split(", ").map(|c| c.to_string()).collect())
                .unwrap_or_default(),
            published_date: None,
        }
    }
}

// ---------------------------------------------------------------------------
// SQL constants
// ---------------------------------------------------------------------------

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS papers (
    id              INTEGER PRIMARY KEY,
    entry_type      TEXT,
    title           TEXT NOT NULL,
    authors         TEXT NOT NULL,
    authors_text    TEXT NOT NULL DEFAULT '',
    year            TEXT,
    doi             TEXT UNIQUE,
    arxiv_id        TEXT UNIQUE,
    isbn            TEXT UNIQUE,
    s2_id           TEXT UNIQUE,
    openalex_id     TEXT UNIQUE,
    journal         TEXT,
    booktitle       TEXT,
    publisher       TEXT,
    volume          TEXT,
    number          TEXT,
    pages           TEXT,
    editor          TEXT,
    edition         TEXT,
    series          TEXT,
    institution     TEXT,
    school          TEXT,
    month           TEXT,
    address         TEXT,
    howpublished    TEXT,
    note            TEXT,
    abstract        TEXT,
    url             TEXT,
    pdf_url         TEXT,
    categories      TEXT,
    citation_count  INTEGER,
    local_path      TEXT,
    full_text       TEXT,
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    deleted_at      TEXT
);

CREATE TABLE IF NOT EXISTS paper_sources (
    paper_id        INTEGER NOT NULL REFERENCES papers(id),
    source          TEXT NOT NULL,
    cache_key       TEXT REFERENCES api_cache(cache_key),
    fetched_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    PRIMARY KEY (paper_id, source)
);

CREATE TABLE IF NOT EXISTS citations (
    source_id       INTEGER NOT NULL REFERENCES papers(id),
    target_id       INTEGER NOT NULL REFERENCES papers(id),
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    PRIMARY KEY (source_id, target_id)
);

CREATE TABLE IF NOT EXISTS citation_sources (
    source_id       INTEGER NOT NULL,
    target_id       INTEGER NOT NULL,
    source          TEXT NOT NULL,
    cache_key       TEXT REFERENCES api_cache(cache_key),
    fetched_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    PRIMARY KEY (source_id, target_id, source),
    FOREIGN KEY (source_id, target_id) REFERENCES citations(source_id, target_id)
);

CREATE TABLE IF NOT EXISTS api_cache (
    cache_key       TEXT PRIMARY KEY,
    url             TEXT NOT NULL,
    body            TEXT NOT NULL,
    fetched_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE TABLE IF NOT EXISTS meta (
    key             TEXT PRIMARY KEY,
    value           TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS changelog (
    id              INTEGER PRIMARY KEY,
    table_name      TEXT NOT NULL,
    row_key         TEXT NOT NULL,
    action          TEXT NOT NULL,
    old_data        TEXT,
    new_data        TEXT,
    timestamp       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE TABLE IF NOT EXISTS paper_claims (
    paper_id   INTEGER NOT NULL REFERENCES papers(id),
    field      TEXT NOT NULL,
    value      TEXT NOT NULL,
    source     TEXT NOT NULL,
    fetched_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    PRIMARY KEY (paper_id, field, source)
);

CREATE VIRTUAL TABLE IF NOT EXISTS papers_fts USING fts5(
    title, authors_text, abstract, full_text,
    content='papers', content_rowid='id',
    tokenize='unicode61'
);
";

const INDICES_SQL: &str = "
CREATE INDEX IF NOT EXISTS idx_papers_year ON papers(year);
CREATE INDEX IF NOT EXISTS idx_citations_target ON citations(target_id);
CREATE INDEX IF NOT EXISTS idx_paper_sources_cache_key ON paper_sources(cache_key);
CREATE INDEX IF NOT EXISTS idx_citation_sources_cache_key ON citation_sources(cache_key);
CREATE INDEX IF NOT EXISTS idx_changelog_timestamp ON changelog(timestamp);
CREATE INDEX IF NOT EXISTS idx_paper_claims_paper ON paper_claims(paper_id);
";

const FTS_TRIGGERS_SQL: &str = "
CREATE TRIGGER IF NOT EXISTS papers_fts_insert AFTER INSERT ON papers BEGIN
    INSERT INTO papers_fts(rowid, title, authors_text, abstract, full_text)
    VALUES (new.id, new.title, new.authors_text, new.abstract, new.full_text);
END;

CREATE TRIGGER IF NOT EXISTS papers_fts_delete BEFORE DELETE ON papers BEGIN
    INSERT INTO papers_fts(papers_fts, rowid, title, authors_text, abstract, full_text)
    VALUES ('delete', old.id, old.title, old.authors_text, old.abstract, old.full_text);
END;

CREATE TRIGGER IF NOT EXISTS papers_fts_update AFTER UPDATE ON papers BEGIN
    INSERT INTO papers_fts(papers_fts, rowid, title, authors_text, abstract, full_text)
    VALUES ('delete', old.id, old.title, old.authors_text, old.abstract, old.full_text);
    INSERT INTO papers_fts(rowid, title, authors_text, abstract, full_text)
    VALUES (new.id, new.title, new.authors_text, new.abstract, new.full_text);
END;

CREATE TRIGGER IF NOT EXISTS papers_fts_soft_delete AFTER UPDATE OF deleted_at ON papers
    WHEN new.deleted_at IS NOT NULL AND old.deleted_at IS NULL BEGIN
    INSERT INTO papers_fts(papers_fts, rowid, title, authors_text, abstract, full_text)
    VALUES ('delete', old.id, old.title, old.authors_text, old.abstract, old.full_text);
END;
";

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn tmp_db() -> (NamedTempFile, Db) {
        let f = NamedTempFile::new().unwrap();
        let db = Db::open(f.path()).unwrap();
        (f, db)
    }

    #[test]
    fn test_open_creates_db() {
        let (f, db) = tmp_db();
        let conn = db.conn.lock().unwrap();
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(tables.contains(&"papers".to_string()));
        assert!(tables.contains(&"api_cache".to_string()));
        assert!(tables.contains(&"citations".to_string()));
        assert!(tables.contains(&"meta".to_string()));
        assert!(tables.contains(&"changelog".to_string()));
        drop(conn);
        drop(db);
        drop(f);
    }

    #[test]
    fn test_cache_roundtrip() {
        let (_f, db) = tmp_db();
        db.cache_set("test_key", "https://example.com", "hello world");
        let result = db.cache_get("test_key", 3600);
        assert_eq!(result, Some("hello world".to_string()));
    }

    #[test]
    fn test_cache_ttl_expiry() {
        let (_f, db) = tmp_db();
        // Insert with an old timestamp directly
        {
            let conn = db.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO api_cache (cache_key, url, body, fetched_at)
                 VALUES ('old_key', 'https://example.com', 'old data', '2020-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        }
        // TTL of 1 second — entry from 2020 should be expired
        assert_eq!(db.cache_get("old_key", 1), None);
        // TTL of 10 years in seconds — should still be valid
        assert_eq!(
            db.cache_get("old_key", 10 * 365 * 86400),
            Some("old data".to_string())
        );
    }

    #[test]
    fn test_cache_key_known_values() {
        // Verified against legacy filesystem cache filenames
        assert_eq!(Db::cache_key("arxiv", "2006.11239"), "arxiv_b2a462a972bbaa37");
        assert_eq!(Db::cache_key("arxiv", "1806.07857"), "arxiv_98f6863dcac96629");
        assert_eq!(Db::cache_key("doi", "10.1145/3442188.3445899"), "doi_36cb08a6274d1c9b");
    }

    #[test]
    fn test_migrate_from_cache_dir() {
        let (_f, db) = tmp_db();
        let dir = tempfile::tempdir().unwrap();

        // Write two fake cache files
        std::fs::write(dir.path().join("arxiv_abc123"), "body1").unwrap();
        std::fs::write(dir.path().join("doi_def456"), "body2").unwrap();

        let count = db.migrate_from_cache_dir(dir.path()).unwrap();
        assert_eq!(count, 2);

        // Verify they're in the DB
        assert_eq!(db.cache_get("arxiv_abc123", 3600), Some("body1".to_string()));
        assert_eq!(db.cache_get("doi_def456", 3600), Some("body2".to_string()));

        // Second migration should skip existing entries
        let count2 = db.migrate_from_cache_dir(dir.path()).unwrap();
        assert_eq!(count2, 0);
    }

    #[test]
    fn test_schema_version_check() {
        let f = NamedTempFile::new().unwrap();
        // First open succeeds
        {
            let _db = Db::open(f.path()).unwrap();
        }
        // Tamper with version
        {
            let conn = Connection::open(f.path()).unwrap();
            conn.execute(
                "UPDATE meta SET value = '999' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        }
        // Second open should fail
        let err = Db::open(f.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Schema version mismatch"),
            "unexpected error: {msg}"
        );
        assert!(msg.contains("have 999"));
        assert!(msg.contains(&format!("need {SCHEMA_VERSION}")));
    }

    #[test]
    fn test_bulk_mode() {
        let (_f, db) = tmp_db();

        let has_trigger = |name: &str| -> bool {
            let conn = db.conn.lock().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name=?",
                params![name],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
                > 0
        };

        // Triggers exist after open
        assert!(has_trigger("papers_fts_insert"));
        assert!(has_trigger("papers_fts_delete"));
        assert!(has_trigger("papers_fts_update"));
        assert!(has_trigger("papers_fts_soft_delete"));

        // After begin_bulk, triggers are gone
        db.begin_bulk().unwrap();
        assert!(!has_trigger("papers_fts_insert"));
        assert!(!has_trigger("papers_fts_delete"));
        assert!(!has_trigger("papers_fts_update"));
        assert!(!has_trigger("papers_fts_soft_delete"));
        assert!(db.in_bulk.load(Ordering::SeqCst));

        // After end_bulk, triggers are back
        db.end_bulk().unwrap();
        assert!(has_trigger("papers_fts_insert"));
        assert!(has_trigger("papers_fts_delete"));
        assert!(has_trigger("papers_fts_update"));
        assert!(has_trigger("papers_fts_soft_delete"));
        assert!(!db.in_bulk.load(Ordering::SeqCst));
    }

    #[test]
    fn test_upsert_paper_insert() {
        let (_f, db) = tmp_db();
        let paper = PaperRow {
            title: "Attention Is All You Need".into(),
            authors: r#"["Vaswani","Shazeer","Parmar"]"#.into(),
            doi: Some("10.5555/3295222.3295349".into()),
            year: Some("2017".into()),
            entry_type: Some("Article".into()),
            ..Default::default()
        };
        let id = db.upsert_paper(&paper, None).unwrap();
        assert!(id > 0);

        // Verify authors_text was computed
        let conn = db.conn.lock().unwrap();
        let authors_text: String = conn
            .query_row("SELECT authors_text FROM papers WHERE id = ?", params![id], |r| r.get(0))
            .unwrap();
        assert_eq!(authors_text, "Vaswani, Shazeer, Parmar");

        // Verify entry_type normalized to lowercase
        let et: String = conn
            .query_row("SELECT entry_type FROM papers WHERE id = ?", params![id], |r| r.get(0))
            .unwrap();
        assert_eq!(et, "article");
    }

    #[test]
    fn test_upsert_paper_dedup_on_doi() {
        let (_f, db) = tmp_db();
        let paper1 = PaperRow {
            title: "Original Title".into(),
            authors: r#"["Author A"]"#.into(),
            doi: Some("10.1234/test".into()),
            year: Some("2020".into()),
            ..Default::default()
        };
        let id1 = db.upsert_paper(&paper1, None).unwrap();

        // Upsert again with same DOI, different title
        let paper2 = PaperRow {
            title: "Updated Title".into(),
            authors: r#"["Author A","Author B"]"#.into(),
            doi: Some("10.1234/test".into()),
            year: Some("2021".into()),
            s2_id: Some("S2_123".into()),
            ..Default::default()
        };
        let id2 = db.upsert_paper(&paper2, None).unwrap();

        assert_eq!(id1, id2, "should return same id for same DOI");

        let conn = db.conn.lock().unwrap();
        let title: String = conn
            .query_row("SELECT title FROM papers WHERE id = ?", params![id1], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "Updated Title");

        let s2: Option<String> = conn
            .query_row("SELECT s2_id FROM papers WHERE id = ?", params![id1], |r| r.get(0))
            .unwrap();
        assert_eq!(s2, Some("S2_123".into()));
    }

    #[test]
    fn test_upsert_paper_cross_ref_keeps_existing() {
        let (_f, db) = tmp_db();
        let paper1 = PaperRow {
            title: "Test".into(),
            authors: "Auth".into(),
            doi: Some("10.1234/xref".into()),
            s2_id: Some("existing_s2".into()),
            ..Default::default()
        };
        db.upsert_paper(&paper1, None).unwrap();

        // Try to overwrite s2_id with a different value — should keep existing
        let paper2 = PaperRow {
            title: "Test".into(),
            authors: "Auth".into(),
            doi: Some("10.1234/xref".into()),
            s2_id: Some("different_s2".into()),
            ..Default::default()
        };
        let id = db.upsert_paper(&paper2, None).unwrap();

        let conn = db.conn.lock().unwrap();
        let s2: String = conn
            .query_row("SELECT s2_id FROM papers WHERE id = ?", params![id], |r| r.get(0))
            .unwrap();
        assert_eq!(s2, "existing_s2");
    }

    #[test]
    fn test_upsert_paper_validation() {
        let (_f, db) = tmp_db();
        // Title truncation: 2001-char title
        let long_title = "A".repeat(2001);
        let paper = PaperRow {
            title: long_title,
            authors: "Auth".into(),
            year: Some("not4".into()), // invalid year
            ..Default::default()
        };
        let id = db.upsert_paper(&paper, None).unwrap();

        let conn = db.conn.lock().unwrap();
        let title: String = conn
            .query_row("SELECT title FROM papers WHERE id = ?", params![id], |r| r.get(0))
            .unwrap();
        assert_eq!(title.len(), 2000);

        let year: Option<String> = conn
            .query_row("SELECT year FROM papers WHERE id = ?", params![id], |r| r.get(0))
            .unwrap();
        assert_eq!(year, None, "invalid year should be stored as NULL");
    }

    #[test]
    fn test_search_local() {
        let (_f, db) = tmp_db();
        // Insert two papers
        db.upsert_paper(&PaperRow {
            title: "Deep reinforcement learning for robotics".into(),
            authors: r#"["Smith","Jones"]"#.into(),
            r#abstract: Some("We study robot control with deep RL.".into()),
            ..Default::default()
        }, None)
        .unwrap();
        db.upsert_paper(&PaperRow {
            title: "Causal inference in statistics".into(),
            authors: r#"["Pearl"]"#.into(),
            r#abstract: Some("A primer on causal models.".into()),
            ..Default::default()
        }, None)
        .unwrap();

        let results = db.search_local("reinforcement learning", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].title.contains("reinforcement"));

        let results = db.search_local("causal", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].title.contains("Causal"));

        // No match
        let results = db.search_local("quantum computing", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_local_excludes_deleted() {
        let (_f, db) = tmp_db();
        let id = db
            .upsert_paper(&PaperRow {
                title: "Deleted paper about transformers".into(),
                authors: "Auth".into(),
                ..Default::default()
            }, None)
            .unwrap();

        // Soft-delete: drop triggers first to avoid update+soft_delete conflict,
        // then rebuild FTS.
        db.begin_bulk().unwrap();
        {
            let conn = db.conn.lock().unwrap();
            conn.execute(
                "UPDATE papers SET deleted_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?",
                params![id],
            )
            .unwrap();
        }
        db.end_bulk().unwrap();

        let results = db.search_local("transformers", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_db_stats() {
        let (_f, db) = tmp_db();

        let stats = db.db_stats().unwrap();
        assert_eq!(stats.paper_count, 0);
        assert_eq!(stats.citation_count, 0);
        assert!(stats.db_size_bytes > 0);

        // Add a paper and a cache entry
        db.upsert_paper(&PaperRow {
            title: "Test".into(),
            authors: "Auth".into(),
            ..Default::default()
        }, None)
        .unwrap();
        db.cache_set("k1", "http://example.com", "body");

        let stats = db.db_stats().unwrap();
        assert_eq!(stats.paper_count, 1);
        assert_eq!(stats.cache_entries, 1);
        assert_eq!(stats.citation_count, 0);
    }

    #[test]
    fn test_eviction() {
        let f = NamedTempFile::new().unwrap();

        // First, create DB and insert an old unreferenced cache entry
        {
            let db = Db::open(f.path()).unwrap();
            let conn = db.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO api_cache (cache_key, url, body, fetched_at)
                 VALUES ('stale_key', 'https://example.com', 'stale', '2020-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
            // Also insert a referenced entry (should survive eviction)
            conn.execute(
                "INSERT INTO api_cache (cache_key, url, body, fetched_at)
                 VALUES ('ref_key', 'https://example.com', 'referenced', '2020-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO papers (id, title, authors) VALUES (1, 'Test', 'Author')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO paper_sources (paper_id, source, cache_key)
                 VALUES (1, 'test_source', 'ref_key')",
                [],
            )
            .unwrap();
        }

        // Reopen — eviction runs during open
        {
            let db = Db::open(f.path()).unwrap();
            // Unreferenced old entry should be gone
            assert_eq!(db.cache_get("stale_key", i64::MAX as u64), None);
            // Referenced old entry should survive
            assert_eq!(
                db.cache_get("ref_key", i64::MAX as u64),
                Some("referenced".to_string())
            );
        }
    }

    #[test]
    fn test_insert_citation_and_get_refs() {
        let (_f, db) = tmp_db();
        let a = db
            .upsert_paper(&PaperRow {
                title: "Paper A".into(),
                authors: "Auth A".into(),
                ..Default::default()
            }, None)
            .unwrap();
        let b = db
            .upsert_paper(&PaperRow {
                title: "Paper B".into(),
                authors: "Auth B".into(),
                ..Default::default()
            }, None)
            .unwrap();
        let c = db
            .upsert_paper(&PaperRow {
                title: "Paper C".into(),
                authors: "Auth C".into(),
                ..Default::default()
            }, None)
            .unwrap();

        // A cites B and C
        db.insert_citation(a, b).unwrap();
        db.insert_citation(a, c).unwrap();

        let refs = db.get_refs(a).unwrap();
        assert_eq!(refs.len(), 2);
        let ref_ids: Vec<i64> = refs.iter().map(|r| r.id.unwrap()).collect();
        assert!(ref_ids.contains(&b));
        assert!(ref_ids.contains(&c));

        // B has no refs
        let refs_b = db.get_refs(b).unwrap();
        assert!(refs_b.is_empty());
    }

    #[test]
    fn test_insert_citation_and_get_cites() {
        let (_f, db) = tmp_db();
        let a = db
            .upsert_paper(&PaperRow {
                title: "Paper A".into(),
                authors: "Auth A".into(),
                ..Default::default()
            }, None)
            .unwrap();
        let b = db
            .upsert_paper(&PaperRow {
                title: "Paper B".into(),
                authors: "Auth B".into(),
                ..Default::default()
            }, None)
            .unwrap();

        // A cites B
        db.insert_citation(a, b).unwrap();

        // Papers that cite B
        let citers = db.get_cites(b).unwrap();
        assert_eq!(citers.len(), 1);
        assert_eq!(citers[0].id.unwrap(), a);

        // Nothing cites A
        let citers_a = db.get_cites(a).unwrap();
        assert!(citers_a.is_empty());
    }

    #[test]
    fn test_insert_citation_idempotent() {
        let (_f, db) = tmp_db();
        let a = db
            .upsert_paper(&PaperRow {
                title: "Paper A".into(),
                authors: "Auth".into(),
                ..Default::default()
            }, None)
            .unwrap();
        let b = db
            .upsert_paper(&PaperRow {
                title: "Paper B".into(),
                authors: "Auth".into(),
                ..Default::default()
            }, None)
            .unwrap();

        db.insert_citation(a, b).unwrap();
        db.insert_citation(a, b).unwrap(); // should not error

        let refs = db.get_refs(a).unwrap();
        assert_eq!(refs.len(), 1);
    }

    #[test]
    fn test_circular_citations_in_db() {
        let (_f, db) = tmp_db();
        let a = db
            .upsert_paper(&PaperRow {
                title: "Paper A".into(),
                authors: "Auth".into(),
                ..Default::default()
            }, None)
            .unwrap();
        let b = db
            .upsert_paper(&PaperRow {
                title: "Paper B".into(),
                authors: "Auth".into(),
                ..Default::default()
            }, None)
            .unwrap();

        // Create a cycle: A cites B, B cites A
        db.insert_citation(a, b).unwrap();
        db.insert_citation(b, a).unwrap();

        // BFS with visited set should not loop
        let mut visited = std::collections::HashSet::new();
        visited.insert(a);

        // Hop 1: refs of A
        let refs_a = db.get_refs(a).unwrap();
        let mut frontier: Vec<i64> = Vec::new();
        for r in &refs_a {
            let id = r.id.unwrap();
            if visited.insert(id) {
                frontier.push(id);
            }
        }
        assert_eq!(frontier, vec![b]);

        // Hop 2: refs of B -> A, but A is visited so frontier is empty
        let mut next_frontier: Vec<i64> = Vec::new();
        for &fid in &frontier {
            let refs = db.get_refs(fid).unwrap();
            for r in &refs {
                let id = r.id.unwrap();
                if visited.insert(id) {
                    next_frontier.push(id);
                }
            }
        }
        assert!(next_frontier.is_empty(), "visited set should prevent cycles");
    }

    #[test]
    fn test_citation_stats_count() {
        let (_f, db) = tmp_db();
        let a = db
            .upsert_paper(&PaperRow {
                title: "A".into(),
                authors: "X".into(),
                ..Default::default()
            }, None)
            .unwrap();
        let b = db
            .upsert_paper(&PaperRow {
                title: "B".into(),
                authors: "Y".into(),
                ..Default::default()
            }, None)
            .unwrap();

        db.insert_citation(a, b).unwrap();

        let stats = db.db_stats().unwrap();
        assert_eq!(stats.citation_count, 1);
    }

    #[test]
    fn test_insert_claims_and_get_claims() {
        let (_f, db) = tmp_db();
        let id = db
            .upsert_paper(&PaperRow {
                title: "Test Paper".into(),
                authors: r#"["Alice"]"#.into(),
                doi: Some("10.1234/test".into()),
                ..Default::default()
            }, None)
            .unwrap();

        db.insert_claims(id, "arxiv", &[("title", "Test Paper"), ("year", "2020")])
            .unwrap();
        db.insert_claims(id, "crossref", &[("title", "Test Paper v2"), ("year", "2020")])
            .unwrap();

        let title_claims = db.get_claims(id, "title").unwrap();
        assert_eq!(title_claims.len(), 2);
        // Both sources present
        let sources: Vec<&str> = title_claims.iter().map(|(_, s, _)| s.as_str()).collect();
        assert!(sources.contains(&"arxiv"));
        assert!(sources.contains(&"crossref"));

        let year_claims = db.get_claims(id, "year").unwrap();
        assert_eq!(year_claims.len(), 2);
        // Same value from both sources
        assert!(year_claims.iter().all(|(v, _, _)| v == "2020"));
    }

    #[test]
    fn test_insert_claims_overwrites_same_source() {
        let (_f, db) = tmp_db();
        let id = db
            .upsert_paper(&PaperRow {
                title: "Test".into(),
                authors: "Auth".into(),
                ..Default::default()
            }, None)
            .unwrap();

        db.insert_claims(id, "arxiv", &[("title", "Old Title")]).unwrap();
        db.insert_claims(id, "arxiv", &[("title", "New Title")]).unwrap();

        let claims = db.get_claims(id, "title").unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].0, "New Title");
    }

    #[test]
    fn test_upsert_paper_with_source_writes_claims() {
        let (_f, db) = tmp_db();
        let paper = PaperRow {
            title: "Claimed Paper".into(),
            authors: r#"["Bob"]"#.into(),
            year: Some("2021".into()),
            doi: Some("10.1234/claimed".into()),
            citation_count: Some(42),
            ..Default::default()
        };
        let id = db.upsert_paper(&paper, Some("crossref")).unwrap();

        let title_claims = db.get_claims(id, "title").unwrap();
        assert_eq!(title_claims.len(), 1);
        assert_eq!(title_claims[0].0, "Claimed Paper");
        assert_eq!(title_claims[0].1, "crossref");

        let year_claims = db.get_claims(id, "year").unwrap();
        assert_eq!(year_claims.len(), 1);
        assert_eq!(year_claims[0].0, "2021");

        let cc_claims = db.get_claims(id, "citation_count").unwrap();
        assert_eq!(cc_claims.len(), 1);
        assert_eq!(cc_claims[0].0, "42");
    }

    #[test]
    fn test_citation_count_resolution_max() {
        let (_f, db) = tmp_db();
        let paper1 = PaperRow {
            title: "MaxCC Paper".into(),
            authors: "Auth".into(),
            doi: Some("10.1234/maxcc".into()),
            citation_count: Some(100),
            ..Default::default()
        };
        let id = db.upsert_paper(&paper1, Some("s2")).unwrap();

        // Second source with higher citation count
        let paper2 = PaperRow {
            title: "MaxCC Paper".into(),
            authors: "Auth".into(),
            doi: Some("10.1234/maxcc".into()),
            citation_count: Some(150),
            ..Default::default()
        };
        let id2 = db.upsert_paper(&paper2, Some("openalex")).unwrap();
        assert_eq!(id, id2);

        // citation_count should be MAX(100, 150) = 150
        let conn = db.conn.lock().unwrap();
        let cc: i64 = conn
            .query_row("SELECT citation_count FROM papers WHERE id = ?", params![id], |r| r.get(0))
            .unwrap();
        assert_eq!(cc, 150);
    }

    #[test]
    fn test_citation_count_resolution_keeps_higher() {
        let (_f, db) = tmp_db();
        let paper1 = PaperRow {
            title: "KeepHigh Paper".into(),
            authors: "Auth".into(),
            doi: Some("10.1234/keephigh".into()),
            citation_count: Some(200),
            ..Default::default()
        };
        let id = db.upsert_paper(&paper1, Some("s2")).unwrap();

        // Second source with LOWER citation count
        let paper2 = PaperRow {
            title: "KeepHigh Paper".into(),
            authors: "Auth".into(),
            doi: Some("10.1234/keephigh".into()),
            citation_count: Some(50),
            ..Default::default()
        };
        db.upsert_paper(&paper2, Some("openalex")).unwrap();

        // citation_count should still be MAX(200, 50) = 200
        let conn = db.conn.lock().unwrap();
        let cc: i64 = conn
            .query_row("SELECT citation_count FROM papers WHERE id = ?", params![id], |r| r.get(0))
            .unwrap();
        assert_eq!(cc, 200);
    }

    #[test]
    fn test_find_claim_conflicts() {
        let (_f, db) = tmp_db();
        let id = db
            .upsert_paper(&PaperRow {
                title: "Conflict Paper".into(),
                authors: "Auth".into(),
                doi: Some("10.1234/conflict".into()),
                ..Default::default()
            }, None)
            .unwrap();

        // Two sources disagree on year
        db.insert_claims(id, "arxiv", &[("year", "2020")]).unwrap();
        db.insert_claims(id, "crossref", &[("year", "2021")]).unwrap();
        // Two sources agree on title
        db.insert_claims(id, "arxiv", &[("title", "Same Title")]).unwrap();
        db.insert_claims(id, "crossref", &[("title", "Same Title")]).unwrap();

        let conflicts = db.find_claim_conflicts().unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].0, id);
        assert_eq!(conflicts[0].2, "year");
        assert_eq!(conflicts[0].3.len(), 2);
    }
}
