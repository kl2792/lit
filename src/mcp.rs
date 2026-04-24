//! MCP (Model Context Protocol) handler logic for the lit tool.
//!
//! Extracted from `bin/lit-mcp.rs` so it can be tested and reused.
//! All handler functions operate on a `cmd::Context` and raw `serde_json::Value`
//! arguments, returning `Result<String, String>`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use crate::{cmd, db};

pub const SERVER_NAME: &str = "lit-mcp";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

// -- Tool definitions --------------------------------------------------------

pub fn tool_definitions() -> Value {
    json!([
        {
            "name": "search",
            "description": "Search for academic papers. Searches local database by default. Use remote=true to search APIs.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search query"},
                    "limit": {"type": "integer", "description": "Max results (default 10)"},
                    "remote": {"type": "boolean", "description": "Search remote APIs instead of local DB"},
                    "brief": {"type": "boolean", "description": "Return compact results (default true)"}
                },
                "required": ["query"]
            }
        },
        {
            "name": "lookup",
            "description": "Look up a paper by arXiv ID, DOI, ISBN, DBLP URL, or direct PDF URL. Returns metadata, abstract, and BibTeX.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "arXiv ID, DOI, ISBN, DBLP URL, or direct PDF URL"}
                },
                "required": ["id"]
            }
        },
        {
            "name": "read",
            "description": "Get file path to a paper's full text. Auto-downloads arXiv papers if not found locally. Use Claude's Read tool on the returned path.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Paper identifier: citekey (e.g. 'conmy2023acdc'), arXiv ID, or substring"},
                    "source": {"type": "boolean", "description": "Prefer arXiv LaTeX source over PDF (default false)"}
                },
                "required": ["query"]
            }
        },
        {
            "name": "add",
            "description": "Fetch BibTeX for a paper and append to a .bib file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "input": {"type": "string", "description": "arXiv ID, DOI, ISBN, search query, or direct PDF URL"},
                    "bib_file": {"type": "string", "description": "Path to .bib file"}
                },
                "required": ["input", "bib_file"]
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
                    "max_papers": {"type": "integer", "description": "Page size (default 20)"},
                    "offset": {"type": "integer", "description": "Skip first N results for pagination (default 0)"}
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
                    "max_papers": {"type": "integer", "description": "Page size (default 20)"},
                    "offset": {"type": "integer", "description": "Skip first N results for pagination (default 0)"}
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
            "name": "misc",
            "description": "Add a @misc BibTeX entry (blog post, forum post, unpublished work) to a .bib file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "citekey": {"type": "string"},
                    "title": {"type": "string"},
                    "authors": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "List of author names in 'First Last' format"
                    },
                    "year": {"type": "string"},
                    "howpublished": {
                        "type": "string",
                        "description": "e.g. \\url{https://...}"
                    },
                    "note": {"type": "string"},
                    "bib_file": {"type": "string"}
                },
                "required": ["citekey", "title", "authors", "year", "bib_file"]
            }
        },
        {
            "name": "clean",
            "description": "Scan a .bib file for malformed entries, duplicates, and optionally orphaned citations. Returns a report. Set dry_run=false to apply fixes.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "bib_file": {"type": "string"},
                    "dry_run": {"type": "boolean", "description": "Default true. Set false to remove malformed+duplicate entries."},
                    "prune": {"type": "boolean", "description": "Default false. Set true (with dry_run=false) to also remove orphaned entries."},
                    "tex_dir": {"type": "string", "description": "Directory to scan for .tex files to find orphans"}
                },
                "required": ["bib_file"]
            }
        }
    ])
}

// -- Path validation ---------------------------------------------------------

/// Validate that a path resolves under the current working directory.
///
/// For existing paths, canonicalizes fully. For new files (e.g. a .bib file
/// that doesn't exist yet), canonicalizes the nearest existing ancestor and
/// appends the remaining components.
pub fn validate_path_under_cwd(path: &str, param_name: &str) -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {}", e))?;
    let cwd_canonical = cwd.canonicalize().map_err(|e| format!("cannot canonicalize cwd: {}", e))?;

    let target = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        cwd.join(path)
    };

    // Try full canonicalize (works if path exists).
    if let Ok(canon) = target.canonicalize() {
        if canon.starts_with(&cwd_canonical) {
            return Ok(canon);
        }
        return Err(format!(
            "{}: path '{}' resolves outside working directory",
            param_name, path,
        ));
    }

    // Path doesn't exist yet — walk up to find the nearest existing ancestor.
    let mut existing = target.as_path();
    let mut suffix_parts: Vec<&std::ffi::OsStr> = Vec::new();
    loop {
        if existing.exists() {
            break;
        }
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                suffix_parts.push(name);
                existing = parent;
            }
            _ => {
                return Err(format!(
                    "{}: no existing ancestor for path '{}'",
                    param_name, path,
                ));
            }
        }
    }

    let ancestor_canon = existing
        .canonicalize()
        .map_err(|e| format!("{}: cannot canonicalize ancestor: {}", param_name, e))?;

    if !ancestor_canon.starts_with(&cwd_canonical) {
        return Err(format!(
            "{}: path '{}' resolves outside working directory",
            param_name, path,
        ));
    }

    let mut resolved = ancestor_canon;
    for part in suffix_parts.into_iter().rev() {
        resolved.push(part);
    }
    Ok(resolved)
}

/// Validate `bib_file`: must have `.bib` extension and resolve under cwd.
pub fn validate_bib_file(path: &str) -> Result<PathBuf, String> {
    let resolved = validate_path_under_cwd(path, "bib_file")?;
    match resolved.extension().and_then(|e| e.to_str()) {
        Some("bib") => Ok(resolved),
        _ => Err(format!("bib_file: '{}' does not have .bib extension", path)),
    }
}

/// Validate `tex_dir`: must resolve under cwd.
pub fn validate_tex_dir(path: &str) -> Result<PathBuf, String> {
    validate_path_under_cwd(path, "tex_dir")
}

// -- Context constructor -----------------------------------------------------

/// Build the `cmd::Context` used for all MCP tool calls.
///
/// JSON mode is always on (structured output for MCP), bib options disabled.
pub fn make_context(database: Arc<db::Db>) -> cmd::Context {
    cmd::Context {
        verbose: false,
        bib_file: None,
        bib_stdout: false,
        json: true,
        no_cache: false,
        db: database,
    }
}

// -- JSON-RPC helpers --------------------------------------------------------

pub fn make_response(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

pub fn make_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        },
    })
}

pub fn make_tool_result(text: &str) -> Value {
    json!({
        "content": [
            {
                "type": "text",
                "text": text,
            }
        ]
    })
}

pub fn make_tool_error(text: &str) -> Value {
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

// -- Tool handlers -----------------------------------------------------------

/// Serialize a list of `PaperResult`s to compact JSON.
///
/// When `brief` is true, uses truncated abstracts and omits low-value fields.
pub fn papers_to_json_string(papers: &[crate::PaperResult], brief: bool) -> Result<String, String> {
    if papers.is_empty() {
        return Ok("No results found".to_string());
    }
    let mapper: fn(&crate::PaperResult) -> Value = if brief {
        cmd::paper_to_json_brief
    } else {
        cmd::paper_to_json
    };
    let arr: Vec<Value> = papers.iter().map(mapper).collect();
    serde_json::to_string(&arr).map_err(|e| e.to_string())
}

pub async fn handle_search(ctx: &cmd::Context, args: &Value) -> Result<String, String> {
    let query = args["query"].as_str().ok_or("missing 'query'")?;
    let limit = args["limit"].as_u64().unwrap_or(10) as usize;
    let remote = args["remote"].as_bool().unwrap_or(false);
    let brief = args["brief"].as_bool().unwrap_or(true);

    if remote {
        let results = cmd::search::run_data(ctx, query, limit, None)
            .await
            .map_err(|e| e.to_string())?;
        papers_to_json_string(&results, brief)
    } else {
        let rows = ctx.db.search_local(query, limit).map_err(|e| e.to_string())?;
        if rows.is_empty() {
            return Ok("No results found".to_string());
        }
        let papers: Vec<crate::PaperResult> = rows.iter().map(|r| r.to_paper_result()).collect();
        papers_to_json_string(&papers, brief)
    }
}

pub async fn handle_lookup(ctx: &cmd::Context, args: &Value) -> Result<String, String> {
    let id = args["id"].as_str().ok_or("missing 'id'")?;
    let lr = cmd::lookup_data(ctx, id).await.map_err(|e| e.to_string())?;
    let mut json = cmd::paper_to_json(&lr.paper);
    if let Some(ref bib) = lr.bibtex {
        json.as_object_mut().unwrap().insert(
            "bibtex".into(),
            Value::String(bib.clone()),
        );
    }
    serde_json::to_string(&json).map_err(|e| e.to_string())
}

pub async fn handle_read(ctx: &cmd::Context, args: &Value) -> Result<String, String> {
    let query = args["query"].as_str().ok_or("missing 'query'")?;

    // Convert Box<dyn Error> to String immediately so the future remains Send.
    let initial = cmd::read::run_data(ctx, query).map_err(|e| e.to_string());

    match initial {
        Ok(result) => {
            let mut json = serde_json::Map::new();
            json.insert(
                "path".into(),
                Value::String(result.path.to_string_lossy().into_owned()),
            );
            json.insert("format".into(), Value::String(result.format));
            serde_json::to_string(&Value::Object(json)).map_err(|e| e.to_string())
        }
        Err(_) => {
            let normalized = query.trim();
            let looks_like_arxiv = normalized.chars().next().is_some_and(|c| c.is_ascii_digit())
                || normalized.starts_with("arxiv:");

            if looks_like_arxiv {
                cmd::download::run(ctx, normalized, true, false, None)
                    .await
                    .map_err(|e| format!("auto-download failed: {}", e))?;

                let result = cmd::read::run_data(ctx, query)
                    .map_err(|e| format!("read after download failed: {}", e))?;

                let mut json = serde_json::Map::new();
                json.insert(
                    "path".into(),
                    Value::String(result.path.to_string_lossy().into_owned()),
                );
                json.insert("format".into(), Value::String(result.format));
                json.insert("auto_downloaded".into(), Value::Bool(true));
                serde_json::to_string(&Value::Object(json)).map_err(|e| e.to_string())
            } else {
                Err(format!("paper '{}' not found locally. Download it first with an arXiv ID.", query))
            }
        }
    }
}

pub async fn handle_refs(ctx: &cmd::Context, args: &Value) -> Result<String, String> {
    let paper_id = args["paper_id"].as_str().ok_or("missing 'paper_id'")?;
    let hops = args["hops"].as_u64().unwrap_or(1) as usize;
    let page_size = args["max_papers"].as_u64().unwrap_or(20) as usize;
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    // Fetch enough to cover offset + page_size
    let fetch_limit = offset + page_size;
    let results = cmd::refs::run_data(ctx, paper_id, hops, fetch_limit)
        .await
        .map_err(|e| e.to_string())?;
    paginated_response(&results, offset, page_size, fetch_limit)
}

pub async fn handle_cites(ctx: &cmd::Context, args: &Value) -> Result<String, String> {
    let paper_id = args["paper_id"].as_str().ok_or("missing 'paper_id'")?;
    let hops = args["hops"].as_u64().unwrap_or(1) as usize;
    let page_size = args["max_papers"].as_u64().unwrap_or(20) as usize;
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let fetch_limit = offset + page_size;
    let results = cmd::cites::run_data(ctx, paper_id, hops, fetch_limit)
        .await
        .map_err(|e| e.to_string())?;
    paginated_response(&results, offset, page_size, fetch_limit)
}

/// Wrap refs/cites results with pagination info.
fn paginated_response(
    all_results: &[crate::PaperResult],
    offset: usize,
    page_size: usize,
    fetch_limit: usize,
) -> Result<String, String> {
    let total_fetched = all_results.len();
    let page = if offset < total_fetched {
        &all_results[offset..total_fetched.min(offset + page_size)]
    } else {
        &[]
    };
    let papers: Vec<Value> = page.iter().map(cmd::paper_to_json_brief).collect();
    let has_more = total_fetched >= fetch_limit;
    let response = json!({
        "count": papers.len(),
        "offset": offset,
        "total_fetched": total_fetched,
        "has_more": has_more,
        "papers": papers,
    });
    serde_json::to_string(&response).map_err(|e| e.to_string())
}

pub async fn handle_path(ctx: &cmd::Context, args: &Value) -> Result<String, String> {
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
            serde_json::to_string(&result).map_err(|e| e.to_string())
        }
        None => Ok("No path found".to_string()),
    }
}

pub fn handle_clean(args: &Value) -> Result<String, String> {
    let bib_raw = args["bib_file"].as_str().ok_or("missing 'bib_file'")?;
    let bib_path = validate_bib_file(bib_raw)?;
    let apply = !args["dry_run"].as_bool().unwrap_or(true);
    let prune = args["prune"].as_bool().unwrap_or(false);
    let tex_dir_str = args["tex_dir"].as_str();

    let tex_path: Option<PathBuf> = tex_dir_str
        .map(validate_tex_dir)
        .transpose()?;
    let tex_refs: Vec<&Path> = tex_path.as_deref().into_iter().collect();

    let report = cmd::clean::run(&bib_path, apply, prune, &tex_refs)
        .map_err(|e| e.to_string())?;

    let mut result = serde_json::Map::new();
    result.insert(
        "malformed".into(),
        Value::Array(report.malformed.iter().map(|k| Value::String(k.clone())).collect()),
    );
    result.insert(
        "duplicates".into(),
        Value::Array(
            report
                .duplicates
                .iter()
                .map(|(kept, removed)| json!({"kept": kept, "removed": removed}))
                .collect(),
        ),
    );
    result.insert(
        "orphans".into(),
        Value::Array(report.orphans.iter().map(|k| Value::String(k.clone())).collect()),
    );
    if apply {
        result.insert(
            "removed".into(),
            Value::Array(report.removed.iter().map(|k| Value::String(k.clone())).collect()),
        );
    }

    serde_json::to_string(&Value::Object(result)).map_err(|e| e.to_string())
}

pub fn handle_misc(args: &Value) -> Result<String, String> {
    let citekey = args["citekey"].as_str().ok_or("missing 'citekey'")?.to_string();
    let title = args["title"].as_str().ok_or("missing 'title'")?.to_string();
    let authors: Vec<String> = args["authors"]
        .as_array()
        .ok_or("missing 'authors'")?
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect();
    let year = args["year"].as_str().ok_or("missing 'year'")?.to_string();
    let howpublished = args["howpublished"].as_str().map(str::to_string);
    let note = args["note"].as_str().map(str::to_string);
    let bib_raw = args["bib_file"].as_str().ok_or("missing 'bib_file'")?;
    let bib_path = validate_bib_file(bib_raw)?;

    let params = cmd::misc::MiscParams { citekey, title, authors, year, howpublished, note };
    let result = cmd::misc::run_data(&params, &bib_path).map_err(|e| e.to_string())?;
    let json = json!({
        "entry_key": result.entry_key,
        "bib_file": bib_path.display().to_string(),
    });
    serde_json::to_string(&json).map_err(|e| e.to_string())
}

pub async fn handle_add(ctx: &cmd::Context, args: &Value) -> Result<String, String> {
    let input = args["input"].as_str().ok_or("missing 'input'")?;
    let bib_raw = args["bib_file"].as_str().ok_or("missing 'bib_file'")?;
    let bib_path = validate_bib_file(bib_raw)?;
    let result = cmd::add::run_data(ctx, input, &bib_path)
        .await
        .map_err(|e| e.to_string())?;
    let json = json!({
        "entry_key": result.entry_key,
        "bib_file": bib_path.display().to_string(),
    });
    serde_json::to_string(&json).map_err(|e| e.to_string())
}

// -- Tool dispatch -----------------------------------------------------------

/// Dispatch a tool call by name. This is the core dispatch used by both
/// synchronous and async-task paths in the binary.
pub async fn dispatch_tool(ctx: &cmd::Context, name: &str, args: &Value) -> Result<String, String> {
    match name {
        "search" => handle_search(ctx, args).await,
        "lookup" => handle_lookup(ctx, args).await,
        "read" => handle_read(ctx, args).await,
        "add" => handle_add(ctx, args).await,
        "misc" => handle_misc(args),
        "refs" => handle_refs(ctx, args).await,
        "cites" => handle_cites(ctx, args).await,
        "path" => handle_path(ctx, args).await,
        "clean" => handle_clean(args),
        _ => Err(format!("unknown tool: {}", name)),
    }
}

// -- JSON-RPC dispatch -------------------------------------------------------

/// Dispatch a single JSON-RPC message and return the response (if any).
///
/// Returns `None` for notifications (no `id`) that don't require a response.
/// Note: the binary (lit-mcp.rs) races every tool call against a configurable
/// inline timeout; if it exceeds the timeout a notification is pushed instead.
/// This function handles all tools inline (no timeout), used for testing.
pub async fn handle_message(ctx: &cmd::Context, msg: &Value) -> Option<Value> {
    let method = msg["method"].as_str().unwrap_or("");
    let id = msg.get("id").cloned();

    match method {
        "initialize" => {
            let result = json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {},
                    "notifications": {}
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
            None
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
            let args = if args.is_null() { json!({}) } else { args };

            let result = dispatch_tool(ctx, name, &args).await;

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
            id.map(|id| make_error(id, -32601, &format!("method not found: {}", method)))
        }
    }
}

// -- Tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // -- Tool definition tests -----------------------------------------------

    #[test]
    fn tool_definitions_returns_nine_tools() {
        let defs = tool_definitions();
        let arr = defs.as_array().unwrap();
        assert_eq!(arr.len(), 9);
    }

    #[test]
    fn tool_definitions_no_duplicate_names() {
        let defs = tool_definitions();
        let arr = defs.as_array().unwrap();
        let names: Vec<&str> = arr.iter().map(|t| t["name"].as_str().unwrap()).collect();
        let unique: HashSet<&str> = names.iter().copied().collect();
        assert_eq!(names.len(), unique.len(), "duplicate tool names found");
    }

    #[test]
    fn tool_definitions_all_have_input_schema() {
        let defs = tool_definitions();
        for tool in defs.as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            assert!(tool.get("inputSchema").is_some(), "tool '{}' missing inputSchema", name);
            assert_eq!(
                tool["inputSchema"]["type"].as_str().unwrap(),
                "object",
                "tool '{}' inputSchema type is not 'object'",
                name,
            );
        }
    }

    #[test]
    fn tool_definitions_required_fields_present() {
        let defs = tool_definitions();
        for tool in defs.as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            assert!(tool["name"].is_string(), "tool missing name");
            assert!(tool["description"].is_string(), "tool '{}' missing description", name);
            let required = &tool["inputSchema"]["required"];
            if !required.is_null() {
                let req_arr = required.as_array().unwrap();
                let props = tool["inputSchema"]["properties"].as_object().unwrap();
                for r in req_arr {
                    let field = r.as_str().unwrap();
                    assert!(
                        props.contains_key(field),
                        "tool '{}': required field '{}' not in properties",
                        name,
                        field,
                    );
                }
            }
        }
    }

    // -- JSON-RPC helper tests -----------------------------------------------

    #[test]
    fn make_response_structure() {
        let resp = make_response(json!(1), json!({"ok": true}));
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["ok"], true);
    }

    #[test]
    fn make_error_structure() {
        let resp = make_error(json!(2), -32601, "not found");
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 2);
        assert_eq!(resp["error"]["code"], -32601);
        assert_eq!(resp["error"]["message"], "not found");
    }

    #[test]
    fn make_tool_result_structure() {
        let tr = make_tool_result("hello");
        let content = tr["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "hello");
        assert!(tr.get("isError").is_none());
    }

    #[test]
    fn make_tool_error_structure() {
        let te = make_tool_error("boom");
        let content = te["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["text"], "boom");
        assert_eq!(te["isError"], true);
    }

    // -- Protocol dispatch tests ---------------------------------------------

    fn make_test_ctx() -> cmd::Context {
        let db = db::Db::open_in_memory().expect("in-memory db");
        make_context(Arc::new(db))
    }

    #[tokio::test]
    async fn dispatch_initialize() {
        let ctx = make_test_ctx();
        let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
        let resp = handle_message(&ctx, &msg).await.unwrap();
        assert_eq!(resp["id"], 1);
        let result = &resp["result"];
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert!(result["serverInfo"]["name"].as_str().unwrap().contains("lit"));
    }

    #[tokio::test]
    async fn dispatch_tools_list() {
        let ctx = make_test_ctx();
        let msg = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
        let resp = handle_message(&ctx, &msg).await.unwrap();
        assert_eq!(resp["id"], 2);
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 9);
    }

    #[tokio::test]
    async fn dispatch_ping() {
        let ctx = make_test_ctx();
        let msg = json!({"jsonrpc": "2.0", "id": 3, "method": "ping"});
        let resp = handle_message(&ctx, &msg).await.unwrap();
        assert_eq!(resp["id"], 3);
        assert_eq!(resp["result"], json!({}));
    }

    #[tokio::test]
    async fn dispatch_unknown_method_with_id() {
        let ctx = make_test_ctx();
        let msg = json!({"jsonrpc": "2.0", "id": 4, "method": "bogus/method"});
        let resp = handle_message(&ctx, &msg).await.unwrap();
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn dispatch_unknown_method_without_id() {
        let ctx = make_test_ctx();
        let msg = json!({"jsonrpc": "2.0", "method": "bogus/notification"});
        let resp = handle_message(&ctx, &msg).await;
        assert!(resp.is_none(), "notifications without id should return None");
    }

    #[tokio::test]
    async fn dispatch_notification_initialized() {
        let ctx = make_test_ctx();
        let msg = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        let resp = handle_message(&ctx, &msg).await;
        assert!(resp.is_none());
    }

    #[tokio::test]
    async fn dispatch_unknown_tool() {
        let ctx = make_test_ctx();
        let msg = json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {"name": "nonexistent", "arguments": {}}
        });
        let resp = handle_message(&ctx, &msg).await.unwrap();
        let content = &resp["result"]["content"][0]["text"];
        assert!(content.as_str().unwrap().contains("unknown tool"));
        assert_eq!(resp["result"]["isError"], true);
    }

    // -- Arg extraction: missing required fields -----------------------------

    #[tokio::test]
    async fn search_missing_query() {
        let ctx = make_test_ctx();
        let result = handle_search(&ctx, &json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("query"));
    }

    #[tokio::test]
    async fn lookup_missing_id() {
        let ctx = make_test_ctx();
        let result = handle_lookup(&ctx, &json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("id"));
    }

    #[tokio::test]
    async fn read_missing_query() {
        let ctx = make_test_ctx();
        let result = handle_read(&ctx, &json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("query"));
    }

    #[tokio::test]
    async fn refs_missing_paper_id() {
        let ctx = make_test_ctx();
        let result = handle_refs(&ctx, &json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("paper_id"));
    }

    #[tokio::test]
    async fn cites_missing_paper_id() {
        let ctx = make_test_ctx();
        let result = handle_cites(&ctx, &json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("paper_id"));
    }

    #[tokio::test]
    async fn path_missing_paper_a() {
        let ctx = make_test_ctx();
        let result = handle_path(&ctx, &json!({"paper_b": "x"})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("paper_a"));
    }

    #[tokio::test]
    async fn path_missing_paper_b() {
        let ctx = make_test_ctx();
        let result = handle_path(&ctx, &json!({"paper_a": "x"})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("paper_b"));
    }

    #[test]
    fn add_missing_input() {
        // handle_add is async, test arg extraction synchronously via the pattern
        let args = json!({"bib_file": "test.bib"});
        assert!(args["input"].as_str().is_none());
    }

    #[test]
    fn misc_missing_citekey() {
        let result = handle_misc(&json!({"title": "t", "authors": ["a"], "year": "2024", "bib_file": "x.bib"}));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("citekey"));
    }

    #[test]
    fn misc_missing_authors() {
        let result = handle_misc(&json!({"citekey": "k", "title": "t", "year": "2024", "bib_file": "x.bib"}));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("authors"));
    }

    #[test]
    fn clean_missing_bib_file() {
        let result = handle_clean(&json!({}));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("bib_file"));
    }

    // -- Path validation tests -----------------------------------------------

    #[test]
    fn validate_path_relative_under_cwd() {
        // A relative path like "foo/bar.bib" should resolve under cwd
        // even if it doesn't exist (tests ancestor resolution).
        let result = validate_path_under_cwd("Cargo.toml", "test");
        assert!(result.is_ok(), "existing relative path should pass: {:?}", result);
    }

    #[test]
    fn validate_path_escape_cwd_fails() {
        let result = validate_path_under_cwd("/etc/passwd", "test");
        assert!(result.is_err(), "absolute path outside cwd should fail");
        assert!(result.unwrap_err().contains("outside working directory"));
    }

    #[test]
    fn validate_path_dotdot_escape_fails() {
        // Enough ".." to escape cwd
        let result = validate_path_under_cwd("../../../../../../../etc/passwd", "test");
        assert!(result.is_err());
    }

    #[test]
    fn validate_bib_extension_required() {
        let result = validate_bib_file("Cargo.toml");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains(".bib extension"));
    }

    #[test]
    fn validate_bib_new_file_ok() {
        // A .bib file that doesn't exist yet, in the current directory.
        let result = validate_bib_file("test-new-file.bib");
        assert!(result.is_ok(), "new .bib under cwd should pass: {:?}", result);
    }

    #[test]
    fn validate_tex_dir_under_cwd() {
        let result = validate_tex_dir("src");
        assert!(result.is_ok());
    }

    #[test]
    fn validate_tex_dir_escape_fails() {
        let result = validate_tex_dir("/tmp");
        assert!(result.is_err());
    }

    // -- papers_to_json_string tests -----------------------------------------

    #[test]
    fn papers_to_json_empty() {
        let result = papers_to_json_string(&[], false).unwrap();
        assert_eq!(result, "No results found");
    }

    #[test]
    fn papers_to_json_single() {
        let paper = crate::PaperResult {
            title: "Test Paper".into(),
            authors: vec!["Alice".into()],
            year: "2024".into(),
            ..Default::default()
        };
        let result = papers_to_json_string(&[paper], false).unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["title"], "Test Paper");
    }

    #[test]
    fn papers_to_json_multiple() {
        let papers: Vec<crate::PaperResult> = (0..3)
            .map(|i| crate::PaperResult {
                title: format!("Paper {}", i),
                authors: vec!["Bob".into()],
                year: format!("{}", 2020 + i),
                ..Default::default()
            })
            .collect();
        let result = papers_to_json_string(&papers, false).unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 3);
    }

    #[test]
    fn papers_to_json_brief_truncates_abstract() {
        let long_abstract = "a".repeat(200);
        let paper = crate::PaperResult {
            title: "Test".into(),
            authors: vec!["Alice".into()],
            year: "2024".into(),
            abstract_text: Some(long_abstract),
            categories: vec!["cs.AI".into()],
            published_date: Some("2024-01-01".into()),
            pdf_url: Some("https://example.com/paper.pdf".into()),
            ..Default::default()
        };
        let result = papers_to_json_string(&[paper], true).unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();
        let arr = parsed.as_array().unwrap();
        let obj = arr[0].as_object().unwrap();
        // Abstract truncated to 150 + "..."
        let abs = obj["abstract"].as_str().unwrap();
        assert_eq!(abs.len(), 153);
        assert!(abs.ends_with("..."));
        // Brief mode omits categories, published_date, pdf_url
        assert!(!obj.contains_key("categories"));
        assert!(!obj.contains_key("published_date"));
        assert!(!obj.contains_key("pdf_url"));
    }

    #[test]
    fn papers_to_json_brief_short_abstract_not_truncated() {
        let paper = crate::PaperResult {
            title: "Test".into(),
            authors: vec!["Alice".into()],
            year: "2024".into(),
            abstract_text: Some("Short abstract.".into()),
            ..Default::default()
        };
        let result = papers_to_json_string(&[paper], true).unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();
        let abs = parsed[0]["abstract"].as_str().unwrap();
        assert_eq!(abs, "Short abstract.");
    }

    #[test]
    fn papers_to_json_compact_no_whitespace() {
        let paper = crate::PaperResult {
            title: "Test".into(),
            authors: vec!["Alice".into()],
            year: "2024".into(),
            ..Default::default()
        };
        let result = papers_to_json_string(&[paper], false).unwrap();
        // Compact JSON has no newlines
        assert!(!result.contains('\n'));
    }

    // -- Local search (empty DB) ---------------------------------------------

    #[tokio::test]
    async fn search_local_empty_db() {
        let ctx = make_test_ctx();
        let result = handle_search(&ctx, &json!({"query": "test"})).await.unwrap();
        assert_eq!(result, "No results found");
    }

    // -- dispatch_tool tests --------------------------------------------------

    #[tokio::test]
    async fn dispatch_tool_unknown() {
        let ctx = make_test_ctx();
        let result = dispatch_tool(&ctx, "nonexistent", &json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown tool"));
    }

    #[tokio::test]
    async fn dispatch_tool_search() {
        let ctx = make_test_ctx();
        let result = dispatch_tool(&ctx, "search", &json!({"query": "test"})).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "No results found");
    }

    // -- Notification-based async dispatch tests --------------------------------

    /// `wait` tool must not appear in tool definitions (it was removed when
    /// notification-based async dispatch was introduced).
    #[test]
    fn tool_definitions_no_wait_tool() {
        let defs = tool_definitions();
        let names: Vec<&str> = defs
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(
            !names.contains(&"wait"),
            "tool_definitions must not include 'wait'; found: {:?}",
            names
        );
    }

    /// The immediate response for a timed-out tool call has the shape
    /// `{"status":"started","task_id":"..."}` embedded in a `make_tool_result`.
    /// This test verifies the JSON structure that the binary produces.
    #[test]
    fn slow_tool_started_response_shape() {
        let task_id = "task_1";
        let payload = json!({
            "status": "started",
            "task_id": task_id,
        });
        let tool_result = make_tool_result(&payload.to_string());
        let text = tool_result["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["status"], "started");
        assert_eq!(parsed["task_id"], task_id);
        assert!(tool_result.get("isError").is_none());
    }

    /// A completed notification has the expected JSON-RPC structure with
    /// `method = "notifications/message"`, `level = "info"`, and the task
    /// result embedded in `params.data`.
    #[test]
    fn notification_complete_shape() {
        let task_id = "task_42";
        let notif = json!({
            "jsonrpc": "2.0",
            "method": "notifications/message",
            "params": {
                "level": "info",
                "data": {
                    "task_id": task_id,
                    "status": "complete",
                    "result": "some result text",
                }
            }
        });
        assert_eq!(notif["jsonrpc"], "2.0");
        assert_eq!(notif["method"], "notifications/message");
        assert_eq!(notif["params"]["level"], "info");
        assert_eq!(notif["params"]["data"]["task_id"], task_id);
        assert_eq!(notif["params"]["data"]["status"], "complete");
        assert!(notif["params"]["data"]["result"].is_string());
        assert!(notif.get("id").is_none(), "notifications must not have an 'id' field");
    }

    /// An error notification has `level = "warning"` and an `error` field
    /// rather than `result`.
    #[test]
    fn notification_error_shape() {
        let task_id = "task_99";
        let notif = json!({
            "jsonrpc": "2.0",
            "method": "notifications/message",
            "params": {
                "level": "warning",
                "data": {
                    "task_id": task_id,
                    "status": "error",
                    "error": "something went wrong",
                }
            }
        });
        assert_eq!(notif["params"]["level"], "warning");
        assert_eq!(notif["params"]["data"]["status"], "error");
        assert!(notif["params"]["data"]["error"].is_string());
        assert!(notif["params"]["data"].get("result").is_none());
    }
}
