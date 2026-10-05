use clap::{Parser, Subcommand, ValueEnum};
use lit::{api, bibtex, cmd, db, format, paths};
use std::path::PathBuf;
use lit::api::clio as clio_api;

#[derive(Parser)]
#[command(
    name = "lit",
    about = "Literature search tool for academic papers",
    version = lit::VERSION
)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Output BibTeX (optionally append to file with --bib=FILE)
    #[arg(short, long = "bib", global = true, num_args = 0..=1, default_missing_value = "", require_equals = true)]
    bib: Option<String>,

    /// Machine-readable JSON output
    #[arg(long, global = true)]
    json: bool,

    /// Bypass cache, fetch fresh
    #[arg(long, global = true)]
    no_cache: bool,

    /// Disable colored output
    #[arg(long, global = true)]
    no_color: bool,

    /// Open in browser instead of displaying metadata
    #[arg(short, long, global = true)]
    open: bool,

    /// Free-form input (arXiv ID, DOI, ISBN, URL, or search query)
    input: Vec<String>,
}

#[derive(Clone, ValueEnum)]
enum SearchSource {
    /// OpenAlex (default primary)
    Oa,
    /// Semantic Scholar
    Ss,
    /// CrossRef
    Cr,
    /// DBLP
    Dblp,
    /// OpenLibrary books
    Book,
    /// PhilPapers (philosophy)
    Philpapers,
    /// Columbia Clio catalog (local index, requires clio-sync)
    Clio,
    /// All sources, merge results
    All,
}

#[derive(Subcommand)]
enum Commands {
    /// Search papers (remote APIs by default, --local for downloaded-only search)
    Search {
        query: Vec<String>,
        /// Maximum number of results
        #[arg(short, long, default_value = "10")]
        limit: usize,
        /// Search source (remote)
        #[arg(short, long)]
        source: Option<SearchSource>,
        /// Search local DB only (only papers you have downloaded)
        #[arg(long, conflicts_with = "source")]
        local: bool,
    },
    /// Get references of a paper
    Refs {
        paper_id: String,
        /// Number of BFS hops (1 = direct only)
        #[arg(long, default_value = "1")]
        hops: usize,
        /// Maximum total papers to fetch
        #[arg(long, default_value = "1000")]
        max_papers: usize,
    },
    /// Get papers that cite this paper
    Cites {
        paper_id: String,
        /// Number of BFS hops (1 = direct only)
        #[arg(long, default_value = "1")]
        hops: usize,
        /// Maximum total papers to fetch
        #[arg(long, default_value = "1000")]
        max_papers: usize,
    },
    /// Citation-graph closure from seeds, deduplicated, as JSONL
    Closure(cmd::closure::Args),
    /// Find shortest citation path between two papers
    Path {
        /// First paper (arXiv ID, DOI, or S2 paper ID)
        paper_a: String,
        /// Second paper
        paper_b: String,
        /// Maximum hops to search in each direction
        #[arg(long, default_value = "5")]
        max_hops: usize,
    },
    /// Download PDF or arXiv LaTeX source
    Download {
        id: String,
        /// Download arXiv LaTeX source instead of PDF
        #[arg(long)]
        source: bool,
        /// Print PDF URL without downloading
        #[arg(long)]
        url_only: bool,
        /// Override output directory for --source
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Override citekey used to name the output directory (e.g. smith2021foo)
        #[arg(long)]
        citekey: Option<String>,
    },
    /// Fetch BibTeX and append to .bib file (arXiv ID, DOI, ISBN, query, or
    /// a page URL on a `lit read` web host)
    Add {
        input: String,
        bib_file: PathBuf,
        /// Override the auto-generated citekey
        #[arg(long)]
        key: Option<String>,
        /// Overwrite on citekey collision with a materially different entry
        #[arg(long)]
        force: bool,
    },
    /// Verify the entries in a .bib file (all, or those named by --key)
    Verify {
        bib_file: PathBuf,
        #[arg(short = 'j', long, default_value = "4")]
        jobs: usize,
        /// Verify only this citekey (repeatable); an unknown key is an error
        #[arg(long = "key", value_name = "KEY")]
        keys: Vec<String>,
    },
    /// Scan a .bib file for malformed entries, duplicates, and orphans
    Clean {
        bib_file: PathBuf,
        /// Apply fixes: remove malformed and duplicate entries
        #[arg(long)]
        apply: bool,
        /// Also remove orphaned entries (requires --tex)
        #[arg(long)]
        prune: bool,
        /// Directory to scan for .tex files (orphan detection; repeatable)
        #[arg(long = "tex")]
        tex_dirs: Vec<PathBuf>,
    },
    /// Check DB<->filesystem consistency
    Check {
        /// Automatically fix inconsistencies
        #[arg(long)]
        fix: bool,
        /// Report cross-source field conflicts for papers with multiple sources
        #[arg(long)]
        conflicts: bool,
        /// Bibliography used to recover metadata for unindexed artifacts
        #[arg(long = "bib-file")]
        bib_file: Option<PathBuf>,
    },
    /// Database operations
    Db {
        #[command(subcommand)]
        action: DbAction,
    },
    /// Locate (and if needed extract) the text of one or more papers.
    /// Prints one file path per id, in argument order.
    Read {
        /// Paper identifiers (arXiv ID, DOI, local cite-key, or a page URL on
        /// distill.pub, transformer-circuits.pub, alignmentforum.org,
        /// lesswrong.com or greaterwrong.com).
        /// Auto-downloads arXiv PDFs and web pages if not cached.
        #[arg(required = true, value_name = "ID")]
        ids: Vec<String>,
    },
    /// Remove an entry from a .bib file by citekey.
    Remove {
        /// Citation key to remove.
        citekey: String,
        /// Target .bib file.
        bib_file: PathBuf,
    },
    /// Columbia Clio catalog operations
    Clio {
        #[command(subcommand)]
        action: ClioAction,
    },
    /// Append a hand-rolled `@misc` BibTeX entry to a .bib file.
    /// Use this for textbooks, working papers, blog posts, and other works
    /// without a resolvable identifier.
    Misc {
        /// Citation key (e.g. "halpern2016actual").
        citekey: String,
        /// Target .bib file.
        bib_file: PathBuf,
        /// Title.
        #[arg(short, long)]
        title: String,
        /// Year (string; accepts non-numeric like "forthcoming").
        #[arg(short, long)]
        year: String,
        /// Author in "First Last" form. Repeat for each author.
        #[arg(short, long = "author", required = true)]
        authors: Vec<String>,
        /// Free-text venue (e.g. "Working paper", "Tech report TR-1").
        #[arg(long)]
        howpublished: Option<String>,
        /// Optional note field.
        #[arg(long)]
        note: Option<String>,
        /// PDF artifact (local path or URL): create etc/pdf/<citekey>/ with
        /// paper.pdf, source.yaml, and paper.txt before writing the bib entry.
        #[arg(long)]
        pdf: Option<String>,
        /// Overwrite on citekey collision with a materially different entry,
        /// and on an existing etc/pdf/<citekey>/ directory with --pdf
        #[arg(long)]
        force: bool,
    },
    /// Attach a PDF to an existing bibliography entry without changing BibTeX.
    Attach {
        citekey: String,
        bib_file: PathBuf,
        pdf: String,
        /// Overwrite an existing artifact directory.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum DbAction {
    /// Show database statistics
    Stats,
    /// Rebuild database from etc/pdf/**/source.yaml files
    Rebuild,
    /// Print every resolved state path and the source that set it
    Path,
    /// Rollback database to a previous state (not yet implemented)
    Rollback {
        /// Timestamp to roll back to (ISO 8601)
        timestamp: String,
    },
}

#[derive(Subcommand)]
enum ClioAction {
    /// Check EZProxy cookie status
    Auth,
    /// Download and index Columbia catalog (~6 GB, 93 files)
    Sync {
        /// Only report index status, don't download
        #[arg(long)]
        check: bool,
        /// Re-sync even if already synced this month (clears existing index)
        #[arg(long)]
        force: bool,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // If --no-color was passed, set NO_COLOR env var so format::use_color() picks it up.
    // SAFETY: This runs before any threads are spawned, so no data race.
    if cli.no_color {
        unsafe { std::env::set_var("NO_COLOR", "1") };
    }

    let (bib_file, bib_stdout) = match cli.bib {
        Some(ref s) if s.is_empty() => (None, true),
        Some(ref s) => (Some(PathBuf::from(s)), false),
        None => (None, false),
    };

    // Resolve DB path (used by rebuild and normal open)
    let (db_path, db_path_source) = paths::db_path();

    // Handle `lit db rebuild` before opening the DB — rebuild creates a fresh DB
    // and doesn't need the old one (which may have a stale schema version).
    if let Some(Commands::Db { action: DbAction::Rebuild }) = &cli.command {
        if let Err(e) = cmd::check::rebuild(&db_path) {
            format::error(&e.to_string());
            std::process::exit(1);
        }
        std::process::exit(0);
    }

    // `db path` diagnoses a misconfiguration, so it must run even when the
    // configured database cannot be opened.
    if let Some(Commands::Db { action: DbAction::Path }) = &cli.command {
        run_db_path(&db_path, db_path_source);
        std::process::exit(0);
    }

    // Open SQLite database
    let database = match db::Db::open(&db_path) {
        Ok(db) => std::sync::Arc::new(db),
        Err(e) => {
            format::error(&format!("Failed to open database: {}", e));
            std::process::exit(1);
        }
    };

    // One-time migration from filesystem cache
    let cache_dir = db_path
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("cache");
    if cache_dir.is_dir() {
        match database.migrate_from_cache_dir(&cache_dir) {
            Ok(0) => {}
            Ok(n) => eprintln!("Migrated {} cache entries to SQLite", n),
            Err(e) => eprintln!("warning: cache migration failed: {}", e),
        }
    }

    let ctx = cmd::Context {
        verbose: cli.verbose,
        bib_file,
        bib_stdout,
        json: cli.json,
        no_cache: cli.no_cache,
        db: database,
    };

    let result = match cli.command {
        Some(Commands::Search {
            query,
            limit,
            source,
            local,
        }) => {
            let q = query.join(" ");
            if !local {
                let src = source.map(|s| match s {
                    SearchSource::Oa => cmd::search::Source::Oa,
                    SearchSource::Ss => cmd::search::Source::Ss,
                    SearchSource::Cr => cmd::search::Source::Cr,
                    SearchSource::Dblp => cmd::search::Source::Dblp,
                    SearchSource::Book => cmd::search::Source::Book,
                    SearchSource::Philpapers => cmd::search::Source::PhilPapers,
                    SearchSource::Clio => cmd::search::Source::Clio,
                    SearchSource::All => cmd::search::Source::All,
                });
                cmd::search::run(&ctx, &q, limit, src).await
            } else {
                run_local_search(&ctx, &q, limit)
            }
        }
        Some(Commands::Refs {
            paper_id,
            hops,
            max_papers,
        }) => cmd::refs::run(&ctx, &paper_id, hops, max_papers).await,
        Some(Commands::Cites {
            paper_id,
            hops,
            max_papers,
        }) => cmd::cites::run(&ctx, &paper_id, hops, max_papers).await,
        Some(Commands::Closure(args)) => cmd::closure::run(&ctx, args).await,
        Some(Commands::Path {
            paper_a,
            paper_b,
            max_hops,
        }) => cmd::path::run(&ctx, &paper_a, &paper_b, max_hops).await,
        Some(Commands::Download {
            id,
            source,
            url_only,
            dir,
            citekey,
        }) => cmd::download::run(&ctx, &id, source, url_only, dir.as_deref(), citekey.as_deref()).await,
        Some(Commands::Add { input, bib_file, key, force }) => cmd::add::run(&ctx, &input, &bib_file, key.as_deref(), force).await,
        Some(Commands::Verify { bib_file, jobs, keys }) => cmd::verify::run(&ctx, &bib_file, jobs, &keys).await,
        Some(Commands::Clean { bib_file, apply, prune, tex_dirs }) => {
            let tex_refs: Vec<&std::path::Path> = tex_dirs.iter().map(|p| p.as_path()).collect();
            match cmd::clean::run(&bib_file, apply, prune, &tex_refs) {
                Ok(report) => {
                    cmd::clean::print_report(&report, apply);
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
        Some(Commands::Check { fix, conflicts, bib_file }) => {
            if conflicts {
                cmd::check::run_conflicts(&ctx)
            } else {
                cmd::check::run(&ctx, fix, bib_file.as_deref()).await
            }
        }
        Some(Commands::Read { ids }) => run_read(&ctx, &ids).await,
        Some(Commands::Remove { citekey, bib_file }) => run_remove(&ctx, &citekey, &bib_file),
        Some(Commands::Misc {
            citekey,
            bib_file,
            title,
            year,
            authors,
            howpublished,
            note,
            pdf,
            force,
        }) => run_misc(&ctx, citekey, &bib_file, title, year, authors, howpublished, note, pdf, force),
        Some(Commands::Attach { citekey, bib_file, pdf, force }) => {
            run_attach(&ctx, &citekey, &bib_file, &pdf, force)
        }
        Some(Commands::Db { action }) => match action {
            DbAction::Stats => run_db_stats(&ctx),
            DbAction::Rebuild => {
                cmd::check::rebuild(&db_path).map_err(|e| e.into())
            }
            // Handled before the database is opened.
            DbAction::Path => Ok(()),
            DbAction::Rollback { timestamp } => {
                eprintln!("rollback to {}: not yet implemented", timestamp);
                Ok(())
            }
        },
        Some(Commands::Clio { action }) => {
            // Use default_clio_db_path() so this agrees with fetch_clio() in search.rs
            // (both check LIT_CLIO_DB_PATH first, then fall back to exe-relative location).
            let clio_db = clio_api::default_clio_db_path();
            let cookie_path = find_clio_cookie_path();
            match action {
                ClioAction::Auth => {
                    cmd::clio::run_auth(&clio_db, &cookie_path).map_err(|e| e.into())
                }
                ClioAction::Sync { check, force } => {
                    cmd::clio::run_sync(&clio_db, check, force).await
                }
            }
        }
        None => {
            let input = cli.input.join(" ");
            if input.is_empty() {
                Cli::parse_from(["lit", "--help"]);
                Ok(())
            } else {
                cmd::auto_dispatch(&ctx, &input, cli.open).await
            }
        }
    };

    if let Err(e) = result {
        format::error(&e.to_string());
        std::process::exit(1);
    }
}

/// Run local FTS search and display results in the same format as remote search.
fn run_local_search(
    ctx: &cmd::Context,
    query: &str,
    limit: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if query.is_empty() {
        return Err("Usage: lit search --local <query>".into());
    }
    let rows = ctx.db.search_local(query, limit)?;
    if rows.is_empty() {
        println!("No results found");
        return Ok(());
    }
    let results: Vec<api::PaperResult> = rows.iter().map(|r| r.to_paper_result()).collect();
    if ctx.json {
        let arr: Vec<serde_json::Value> = results
            .iter()
            .map(|p| cmd::paper_to_json(p))
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr).unwrap());
        return Ok(());
    }
    for (i, p) in results.iter().enumerate() {
        let rank = i + 1;
        let author = p.authors.first().map(|s| s.as_str()).unwrap_or("?");
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
    Ok(())
}

/// Print every resolved state path with the source that set it.
fn run_db_path(db_path: &std::path::Path, db_path_source: &str) {
    println!("database    {}  [{}]", db_path.display(), db_path_source);

    let clio_source = match std::env::var_os("LIT_CLIO_DB_PATH") {
        Some(_) => "LIT_CLIO_DB_PATH",
        None => "default, etc/lit/ found from the working directory",
    };
    println!(
        "clio index  {}  [{}]",
        clio_api::default_clio_db_path().display(),
        clio_source
    );

    match cmd::read::find_pdf_base() {
        Ok(p) => println!(
            "pdf store   {}  [default, found from the working directory]",
            p.display()
        ),
        Err(e) => println!("pdf store   unresolved: {}", e),
    }
}

/// Print database statistics.
fn run_db_stats(ctx: &cmd::Context) -> Result<(), Box<dyn std::error::Error>> {
    let stats = ctx.db.db_stats()?;

    let size = if stats.db_size_bytes >= 1_048_576 {
        format!("{:.1} MB", stats.db_size_bytes as f64 / 1_048_576.0)
    } else if stats.db_size_bytes >= 1024 {
        format!("{:.1} KB", stats.db_size_bytes as f64 / 1024.0)
    } else {
        format!("{} B", stats.db_size_bytes)
    };

    println!("Papers:    {}", stats.paper_count);
    println!("Citations: {}", stats.citation_count);
    println!("Cache:     {} entries", stats.cache_entries);
    println!("DB size:   {}", size);
    Ok(())
}

/// Run `lit read <ID>...`: print each id's text path in argument order.
///
/// Ids are read sequentially, so each arXiv auto-download goes through the
/// shared HTTP retry budget one at a time. With one id the output and errors
/// are the single-id form. With several, a failing id is reported on stderr
/// as `<id>: <error>` and the rest still run; `--json` prints one array of the
/// successful per-id objects; the exit code is 1 if any id failed.
async fn run_read(ctx: &cmd::Context, ids: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if let [id] = ids {
        let result = read_one(ctx, id).await?;
        if ctx.json {
            println!("{}", serde_json::to_string_pretty(&read_result_json(&result))?);
        } else {
            println!("{}", result.path.display());
        }
        return Ok(());
    }

    let mut objects = Vec::new();
    let mut failed = false;
    for id in ids {
        match read_one(ctx, id).await {
            Ok(result) if ctx.json => objects.push(read_result_json(&result)),
            Ok(result) => println!("{}", result.path.display()),
            Err(e) => {
                format::error(&format!("{}: {}", id, e));
                failed = true;
            }
        }
    }
    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&objects)?);
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

/// The `--json` object for one read result.
fn read_result_json(result: &cmd::read::ReadResult) -> serde_json::Value {
    serde_json::json!({
        "path": result.path.to_string_lossy(),
        "format": result.format,
        "extra_files": result.extra_files,
    })
}

/// Locate one paper's text, auto-downloading from arXiv if needed.
async fn read_one(
    ctx: &cmd::Context,
    id: &str,
) -> Result<cmd::read::ReadResult, Box<dyn std::error::Error>> {
    if cmd::web::is_page_url(id) {
        return cmd::web::read(ctx, id).await;
    }
    let result = match cmd::read::run_data(ctx, id) {
        Ok(r) => r,
        Err(cmd::read::ReadError::NotFound(_)) => {
            let normalized = id.trim();
            let looks_like_arxiv = normalized
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
                || normalized.starts_with("arxiv:");
            if !looks_like_arxiv {
                return Err(format!(
                    "paper '{}' not found locally. Download it first with an arXiv ID.",
                    id
                )
                .into());
            }
            cmd::download::run(ctx, normalized, true, false, None, None).await?;
            cmd::read::run_data(ctx, id)?
        }
        // Local source exists but is unreadable (or other failure): surface the
        // real cause instead of the misleading "not found locally" message.
        Err(e) => return Err(e.into()),
    };
    Ok(result)
}

/// Run `lit remove`: delete an entry from a .bib file by citekey.
fn run_remove(
    ctx: &cmd::Context,
    citekey: &str,
    bib_file: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let removed = bibtex::remove_from_file(bib_file, citekey)?;
    if ctx.json {
        let json = serde_json::json!({
            "entry_key": citekey,
            "bib_file": bib_file.display().to_string(),
            "removed": removed,
        });
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else if removed {
        println!("Removed @{{{}}} from {}", citekey, bib_file.display());
    } else {
        eprintln!("No entry with key '{}' in {}", citekey, bib_file.display());
        std::process::exit(2);
    }
    Ok(())
}

/// Derive the EZProxy cookies file path from cwd (walk up looking for the file).
///
/// Returns the found path if it exists, otherwise returns the expected path
/// under cwd so `run_auth` can print a meaningful "not found" message.
fn find_clio_cookie_path() -> PathBuf {
    if let Ok(cwd) = std::env::current_dir() {
        let mut dir = cwd.as_path();
        loop {
            let candidate = dir.join(".cache/lit/clio/cookies.txt");
            if candidate.exists() {
                return candidate;
            }
            match dir.parent() {
                Some(p) => dir = p,
                None => break,
            }
        }
        cwd.join(".cache/lit/clio/cookies.txt")
    } else {
        PathBuf::from(".cache/lit/clio/cookies.txt")
    }
}

/// Run `lit misc`: append a hand-rolled `@misc` BibTeX entry.
#[allow(clippy::too_many_arguments)]
fn run_misc(
    ctx: &cmd::Context,
    citekey: String,
    bib_file: &std::path::Path,
    title: String,
    year: String,
    authors: Vec<String>,
    howpublished: Option<String>,
    note: Option<String>,
    pdf: Option<String>,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let params = cmd::misc::MiscParams {
        citekey,
        title,
        authors,
        year,
        howpublished,
        note,
        url: None,
    };
    let pdf_root = match pdf {
        Some(_) => Some(cmd::read::find_pdf_base()?),
        None => None,
    };
    let result = match (&pdf, &pdf_root) {
        (Some(p), Some(root)) => cmd::misc::run_pdf_data(&params, bib_file, p, force, root)?,
        _ => cmd::misc::run_data(&params, bib_file, force)?,
    };
    let artifact_dir = pdf_root.map(|root| root.join(&result.entry_key));
    if ctx.json {
        let mut json = result.to_json(bib_file);
        if let Some(ref dir) = artifact_dir {
            json["dir"] = serde_json::Value::String(dir.display().to_string());
        }
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else {
        if let Some(ref dir) = artifact_dir {
            println!("Saved: {}", dir.display());
        }
        println!("Added @misc{{{}}} to {}", result.entry_key, bib_file.display());
    }
    Ok(())
}

fn run_attach(
    ctx: &cmd::Context,
    citekey: &str,
    bib_file: &std::path::Path,
    pdf: &str,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let pdf_root = cmd::read::find_pdf_base()?;
    let dir = cmd::misc::attach_pdf_data(citekey, bib_file, pdf, force, &pdf_root)?;
    if ctx.json {
        println!("{}", serde_json::json!({
            "citekey": citekey,
            "bib_file": bib_file.display().to_string(),
            "dir": dir.display().to_string(),
        }));
    } else {
        println!("Attached: {}", dir.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn version_names_crate_version_and_git_hash() {
        let v = Cli::command().get_version().unwrap().to_string();
        assert!(v.starts_with(env!("CARGO_PKG_VERSION")), "version: {}", v);
        assert!(v.contains(env!("LIT_GIT_HASH")), "version: {}", v);
    }

    #[test]
    fn search_rejects_local_together_with_source() {
        let err = Cli::command()
            .try_get_matches_from(["lit", "search", "--local", "-s", "clio", "q"])
            .unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn search_accepts_local_and_source_separately() {
        let cmd = Cli::command();
        assert!(cmd.clone().try_get_matches_from(["lit", "search", "--local", "q"]).is_ok());
        assert!(cmd.try_get_matches_from(["lit", "search", "-s", "clio", "q"]).is_ok());
    }
}
