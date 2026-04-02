use clap::{Parser, Subcommand, ValueEnum};
use lit::{api, cmd, db, format};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "lit", about = "Literature search tool for academic papers")]
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
    /// All sources, merge results
    All,
}

#[derive(Subcommand)]
enum Commands {
    /// Search papers (local DB by default, --remote for API search)
    Search {
        query: Vec<String>,
        /// Maximum number of results
        #[arg(short, long, default_value = "10")]
        limit: usize,
        /// Search source (implies --remote)
        #[arg(short, long)]
        source: Option<SearchSource>,
        /// Search remote APIs instead of local database
        #[arg(long)]
        remote: bool,
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
    },
    /// Fetch BibTeX and append to .bib file
    Add { input: String, bib_file: PathBuf },
    /// Verify all entries in a .bib file
    Verify {
        bib_file: PathBuf,
        #[arg(short = 'j', long, default_value = "4")]
        jobs: usize,
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
    },
    /// Database operations
    Db {
        #[command(subcommand)]
        action: DbAction,
    },
}

#[derive(Subcommand)]
enum DbAction {
    /// Show database statistics
    Stats,
    /// Rebuild database from etc/pdf/**/source.yaml files
    Rebuild,
    /// Rollback database to a previous state (not yet implemented)
    Rollback {
        /// Timestamp to roll back to (ISO 8601)
        timestamp: String,
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
    let db_path = std::env::var("LIT_DB_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let exe = std::env::current_exe().unwrap_or_default();
            exe.parent()
                .unwrap_or(std::path::Path::new("."))
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .join("etc/lit/lit.db")
        });

    // Handle `lit db rebuild` before opening the DB — rebuild creates a fresh DB
    // and doesn't need the old one (which may have a stale schema version).
    if let Some(Commands::Db { action: DbAction::Rebuild }) = &cli.command {
        if let Err(e) = cmd::check::rebuild(&db_path) {
            format::error(&e.to_string());
            std::process::exit(1);
        }
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
            remote,
        }) => {
            let q = query.join(" ");
            let use_remote = remote || source.is_some();
            if use_remote {
                let src = source.map(|s| match s {
                    SearchSource::Oa => cmd::search::Source::Oa,
                    SearchSource::Ss => cmd::search::Source::Ss,
                    SearchSource::Cr => cmd::search::Source::Cr,
                    SearchSource::Dblp => cmd::search::Source::Dblp,
                    SearchSource::Book => cmd::search::Source::Book,
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
        }) => cmd::download::run(&ctx, &id, source, url_only, dir.as_deref()).await,
        Some(Commands::Add { input, bib_file }) => cmd::add::run(&ctx, &input, &bib_file).await,
        Some(Commands::Verify { bib_file, jobs }) => cmd::verify::run(&ctx, &bib_file, jobs).await,
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
        Some(Commands::Check { fix, conflicts }) => {
            if conflicts {
                cmd::check::run_conflicts(&ctx)
            } else {
                cmd::check::run(&ctx, fix).await
            }
        }
        Some(Commands::Db { action }) => match action {
            DbAction::Stats => run_db_stats(&ctx),
            DbAction::Rebuild => {
                cmd::check::rebuild(&db_path).map_err(|e| e.into())
            }
            DbAction::Rollback { timestamp } => {
                eprintln!("rollback to {}: not yet implemented", timestamp);
                Ok(())
            }
        },
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
