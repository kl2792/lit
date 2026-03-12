/// MCP (Model Context Protocol) server for the lit tool.
///
/// Reads JSON-RPC 2.0 messages from stdin (newline-delimited), dispatches
/// tool calls to the lit library, and writes JSON-RPC responses to stdout.
/// Stderr is free for logging.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lit::{cmd, db};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};

const SERVER_NAME: &str = "lit-mcp";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

fn tool_definitions() -> Value {
    json!([
        {
            "name": "search",
            "description": "Search for academic papers. Searches local database by default. Use remote=true to search APIs.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search query"},
                    "limit": {"type": "integer", "description": "Max results (default 10)"},
                    "remote": {"type": "boolean", "description": "Search remote APIs instead of local DB"}
                },
                "required": ["query"]
            }
        },
        {
            "name": "lookup",
            "description": "Look up a paper by arXiv ID, DOI, or ISBN. Returns metadata, abstract, and BibTeX.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "arXiv ID, DOI, ISBN, or DBLP URL"}
                },
                "required": ["id"]
            }
        },
        {
            "name": "refs",
            "description": "Get references of a paper (papers it cites).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "paper_id": {"type": "string", "description": "Paper ID (arXiv, DOI, or S2 ID)"},
                    "hops": {"type": "integer", "description": "BFS depth (default 1)"},
                    "max_papers": {"type": "integer", "description": "Max total papers (default 50)"}
                },
                "required": ["paper_id"]
            }
        },
        {
            "name": "cites",
            "description": "Get papers that cite a given paper.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "paper_id": {"type": "string", "description": "Paper ID (arXiv, DOI, or S2 ID)"},
                    "hops": {"type": "integer", "description": "BFS depth (default 1)"},
                    "max_papers": {"type": "integer", "description": "Max total papers (default 50)"}
                },
                "required": ["paper_id"]
            }
        },
        {
            "name": "path",
            "description": "Find shortest citation path between two papers.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "paper_a": {"type": "string", "description": "First paper ID"},
                    "paper_b": {"type": "string", "description": "Second paper ID"},
                    "max_hops": {"type": "integer", "description": "Max hops per direction (default 5)"}
                },
                "required": ["paper_a", "paper_b"]
            }
        },
        {
            "name": "add",
            "description": "Fetch BibTeX for a paper and append to a .bib file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "input": {"type": "string", "description": "arXiv ID, DOI, ISBN, or search query"},
                    "bib_file": {"type": "string", "description": "Path to .bib file"}
                },
                "required": ["input", "bib_file"]
            }
        },
        {
            "name": "db_stats",
            "description": "Get database statistics: paper count, citation count, cache entries, DB size.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }
    ])
}

/// Build the Context used for all tool calls.
///
/// JSON mode is always on (structured output for MCP), bib options disabled.
fn make_context(database: Arc<db::Db>) -> cmd::Context {
    cmd::Context {
        verbose: false,
        bib_file: None,
        bib_stdout: false,
        json: true,
        no_cache: false,
        db: database,
    }
}

// -- Tool handlers ----------------------------------------------------------

/// Serialize a list of PaperResults to pretty-printed JSON.
fn papers_to_json_string(papers: &[lit::PaperResult]) -> Result<String, String> {
    if papers.is_empty() {
        return Ok("No results found".to_string());
    }
    let arr: Vec<Value> = papers.iter().map(|p| cmd::paper_to_json(p)).collect();
    serde_json::to_string_pretty(&arr).map_err(|e| e.to_string())
}

async fn handle_search(ctx: &cmd::Context, args: &Value) -> Result<String, String> {
    let query = args["query"].as_str().ok_or("missing 'query'")?;
    let limit = args["limit"].as_u64().unwrap_or(10) as usize;
    let remote = args["remote"].as_bool().unwrap_or(false);

    if remote {
        let results = cmd::search::run_data(ctx, query, limit, None)
            .await
            .map_err(|e| e.to_string())?;
        papers_to_json_string(&results)
    } else {
        let rows = ctx.db.search_local(query, limit).map_err(|e| e.to_string())?;
        if rows.is_empty() {
            return Ok("No results found".to_string());
        }
        let results: Vec<Value> = rows
            .iter()
            .map(|r| cmd::paper_to_json(&r.to_paper_result()))
            .collect();
        serde_json::to_string_pretty(&results).map_err(|e| e.to_string())
    }
}

async fn handle_lookup(ctx: &cmd::Context, args: &Value) -> Result<String, String> {
    let id = args["id"].as_str().ok_or("missing 'id'")?;
    let lr = cmd::lookup_data(ctx, id).await.map_err(|e| e.to_string())?;
    let mut json = cmd::paper_to_json(&lr.paper);
    if let Some(ref bib) = lr.bibtex {
        json.as_object_mut().unwrap().insert(
            "bibtex".into(),
            Value::String(bib.clone()),
        );
    }
    serde_json::to_string_pretty(&json).map_err(|e| e.to_string())
}

async fn handle_refs(ctx: &cmd::Context, args: &Value) -> Result<String, String> {
    let paper_id = args["paper_id"].as_str().ok_or("missing 'paper_id'")?;
    let hops = args["hops"].as_u64().unwrap_or(1) as usize;
    let max_papers = args["max_papers"].as_u64().unwrap_or(50) as usize;
    let results = cmd::refs::run_data(ctx, paper_id, hops, max_papers)
        .await
        .map_err(|e| e.to_string())?;
    papers_to_json_string(&results)
}

async fn handle_cites(ctx: &cmd::Context, args: &Value) -> Result<String, String> {
    let paper_id = args["paper_id"].as_str().ok_or("missing 'paper_id'")?;
    let hops = args["hops"].as_u64().unwrap_or(1) as usize;
    let max_papers = args["max_papers"].as_u64().unwrap_or(50) as usize;
    let results = cmd::cites::run_data(ctx, paper_id, hops, max_papers)
        .await
        .map_err(|e| e.to_string())?;
    papers_to_json_string(&results)
}

async fn handle_path(ctx: &cmd::Context, args: &Value) -> Result<String, String> {
    let paper_a = args["paper_a"].as_str().ok_or("missing 'paper_a'")?;
    let paper_b = args["paper_b"].as_str().ok_or("missing 'paper_b'")?;
    let max_hops = args["max_hops"].as_u64().unwrap_or(5) as usize;
    let path = cmd::path::run_data(ctx, paper_a, paper_b, max_hops)
        .await
        .map_err(|e| e.to_string())?;
    match path {
        Some(steps) => {
            let arr: Vec<Value> = steps
                .iter()
                .map(|(id, title)| json!({"id": id, "title": title}))
                .collect();
            let result = json!({
                "hops": steps.len().saturating_sub(1),
                "path": arr,
            });
            serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
        }
        None => Ok("No path found".to_string()),
    }
}

async fn handle_add(ctx: &cmd::Context, args: &Value) -> Result<String, String> {
    let input = args["input"].as_str().ok_or("missing 'input'")?;
    let bib_file = args["bib_file"].as_str().ok_or("missing 'bib_file'")?;
    let result = cmd::add::run_data(ctx, input, Path::new(bib_file))
        .await
        .map_err(|e| e.to_string())?;
    let json = json!({
        "entry_key": result.entry_key,
        "bib_file": bib_file,
        "bibtex": result.bib_text,
    });
    serde_json::to_string_pretty(&json).map_err(|e| e.to_string())
}

fn handle_db_stats(ctx: &cmd::Context) -> Result<String, String> {
    let stats = ctx.db.db_stats().map_err(|e| e.to_string())?;
    let result = json!({
        "paper_count": stats.paper_count,
        "citation_count": stats.citation_count,
        "cache_entries": stats.cache_entries,
        "db_size_bytes": stats.db_size_bytes,
    });
    serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
}

// -- JSON-RPC dispatch ------------------------------------------------------

fn make_response(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

fn make_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        },
    })
}

fn make_tool_result(text: &str) -> Value {
    json!({
        "content": [
            {
                "type": "text",
                "text": text,
            }
        ]
    })
}

fn make_tool_error(text: &str) -> Value {
    json!({
        "content": [
            {
                "type": "text",
                "text": text,
            }
        ],
        "isError": true,
    })
}

async fn handle_message(ctx: &cmd::Context, msg: &Value) -> Option<Value> {
    let method = msg["method"].as_str().unwrap_or("");
    let id = msg.get("id").cloned();

    match method {
        "initialize" => {
            let result = json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": SERVER_NAME,
                    "version": SERVER_VERSION,
                }
            });
            Some(make_response(id.unwrap_or(Value::Null), result))
        }
        "notifications/initialized" => {
            eprintln!("[lit-mcp] initialized");
            None // notifications have no response
        }
        "tools/list" => {
            let result = json!({
                "tools": tool_definitions(),
            });
            Some(make_response(id.unwrap_or(Value::Null), result))
        }
        "tools/call" => {
            let id = id.unwrap_or(Value::Null);
            let name = msg["params"]["name"].as_str().unwrap_or("");
            let args = msg["params"]["arguments"].clone();
            let args = if args.is_null() {
                json!({})
            } else {
                args
            };

            let result = match name {
                "search" => handle_search(ctx, &args).await,
                "lookup" => handle_lookup(ctx, &args).await,
                "refs" => handle_refs(ctx, &args).await,
                "cites" => handle_cites(ctx, &args).await,
                "path" => handle_path(ctx, &args).await,
                "add" => handle_add(ctx, &args).await,
                "db_stats" => handle_db_stats(ctx),
                _ => Err(format!("unknown tool: {}", name)),
            };

            match result {
                Ok(text) => Some(make_response(id, make_tool_result(&text))),
                Err(e) => Some(make_response(id, make_tool_error(&e))),
            }
        }
        "notifications/cancelled" => None,
        "ping" => {
            Some(make_response(id.unwrap_or(Value::Null), json!({})))
        }
        _ => {
            // Unknown method
            if let Some(id) = id {
                Some(make_error(id, -32601, &format!("method not found: {}", method)))
            } else {
                None // unknown notification, ignore
            }
        }
    }
}

#[tokio::main]
async fn main() {
    // Resolve DB path (same logic as main.rs)
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

    let database = match db::Db::open(&db_path) {
        Ok(db) => Arc::new(db),
        Err(e) => {
            eprintln!("[lit-mcp] failed to open database: {}", e);
            std::process::exit(1);
        }
    };

    let ctx = make_context(database);

    eprintln!("[lit-mcp] server started, reading from stdin");

    let stdin = tokio::io::stdin();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();
    let mut stdout = tokio::io::stdout();

    loop {
        use tokio::io::AsyncWriteExt;

        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => {
                eprintln!("[lit-mcp] stdin closed, shutting down");
                break;
            }
            Err(e) => {
                eprintln!("[lit-mcp] read error: {}", e);
                break;
            }
        };

        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[lit-mcp] parse error: {}", e);
                let err = make_error(Value::Null, -32700, &format!("parse error: {}", e));
                let out = serde_json::to_string(&err).unwrap();
                let _ = stdout.write_all(out.as_bytes()).await;
                let _ = stdout.write_all(b"\n").await;
                let _ = stdout.flush().await;
                continue;
            }
        };

        if let Some(response) = handle_message(&ctx, &msg).await {
            let out = serde_json::to_string(&response).unwrap();
            let _ = stdout.write_all(out.as_bytes()).await;
            let _ = stdout.write_all(b"\n").await;
            let _ = stdout.flush().await;
        }
    }
}
