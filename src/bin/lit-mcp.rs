/// MCP (Model Context Protocol) server for the lit tool.
///
/// Reads JSON-RPC 2.0 messages from stdin (newline-delimited), dispatches
/// tool calls to the lit library, and writes JSON-RPC responses to stdout.
/// Notifications are pushed to stdout when async tasks complete.
/// Stderr is free for logging.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use lit::mcp::{
    dispatch_tool, handle_clean, handle_misc, handle_read, handle_search, is_slow_tool,
    make_context, make_error, make_response, make_tool_error, make_tool_result, tool_definitions,
    SERVER_NAME, SERVER_VERSION,
};
use lit::{cmd, db};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

// -- Notification helpers ----------------------------------------------------

fn make_notification(task_id: &str, outcome: Result<String, String>) -> Value {
    match outcome {
        Ok(result) => json!({
            "jsonrpc": "2.0",
            "method": "notifications/message",
            "params": {
                "level": "info",
                "data": {
                    "task_id": task_id,
                    "status": "complete",
                    "result": result,
                }
            }
        }),
        Err(error) => json!({
            "jsonrpc": "2.0",
            "method": "notifications/message",
            "params": {
                "level": "warning",
                "data": {
                    "task_id": task_id,
                    "status": "error",
                    "error": error,
                }
            }
        }),
    }
}

// -- JSON-RPC dispatch -------------------------------------------------------

async fn handle_message(
    ctx: &cmd::Context,
    counter: &Arc<AtomicU64>,
    notif_tx: &mpsc::UnboundedSender<Value>,
    msg: &Value,
) -> Option<Value> {
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
            let result = json!({ "tools": tool_definitions() });
            Some(make_response(id.unwrap_or(Value::Null), result))
        }
        "tools/call" => {
            let id = id.unwrap_or(Value::Null);
            let name = msg["params"]["name"].as_str().unwrap_or("");
            let args = {
                let a = msg["params"]["arguments"].clone();
                if a.is_null() { json!({}) } else { a }
            };

            if is_slow_tool(name, &args) {
                // Assign task ID, spawn, return immediately.
                let seq = counter.fetch_add(1, Ordering::Relaxed) + 1;
                let task_id = format!("task_{}", seq);

                let ctx_clone = ctx.clone();
                let name_owned = name.to_string();
                let args_clone = args.clone();
                let tx = notif_tx.clone();
                let tid = task_id.clone();

                tokio::task::spawn_local(async move {
                    let outcome = dispatch_tool(&ctx_clone, &name_owned, &args_clone).await;
                    let notif = make_notification(&tid, outcome);
                    let _ = tx.send(notif);
                });

                Some(make_response(id, make_tool_result(&json!({
                    "status": "started",
                    "task_id": task_id,
                }).to_string())))
            } else {
                // Fast/sync path: handle inline.
                let result = match name {
                    "search" => handle_search(ctx, &args).await,
                    "read" => handle_read(ctx, &args).await,
                    "misc" => handle_misc(&args),
                    "clean" => handle_clean(&args),
                    _ => Err(format!("unknown tool: {}", name)),
                };
                match result {
                    Ok(text) => Some(make_response(id, make_tool_result(&text))),
                    Err(e) => Some(make_response(id, make_tool_error(&e))),
                }
            }
        }
        "notifications/cancelled" => None,
        "ping" => Some(make_response(id.unwrap_or(Value::Null), json!({}))),
        _ => {
            if let Some(id) = id {
                Some(make_error(id, -32601, &format!("method not found: {}", method)))
            } else {
                None
            }
        }
    }
}

fn main() {
    // Resolve DB path (same logic as main.rs).
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

    // Use a single-threaded runtime so that spawn_local works for non-Send futures
    // produced by cmd handlers that use Box<dyn Error> (non-Send) internally.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let local = tokio::task::LocalSet::new();

    local.block_on(&rt, async move {
        let ctx = make_context(database);
        let counter = Arc::new(AtomicU64::new(0));
        let (notif_tx, mut notif_rx) = mpsc::unbounded_channel::<Value>();

        eprintln!("[lit-mcp] server started, reading from stdin");

        let stdin = tokio::io::stdin();
        let reader = BufReader::new(stdin);
        let mut lines = reader.lines();
        let mut stdout = tokio::io::stdout();

        loop {
            tokio::select! {
                line_result = lines.next_line() => {
                    let line = match line_result {
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

                    if let Some(response) = handle_message(&ctx, &counter, &notif_tx, &msg).await {
                        let out = serde_json::to_string(&response).unwrap();
                        let _ = stdout.write_all(out.as_bytes()).await;
                        let _ = stdout.write_all(b"\n").await;
                        let _ = stdout.flush().await;
                    }
                }

                Some(notif) = notif_rx.recv() => {
                    let out = serde_json::to_string(&notif).unwrap();
                    let _ = stdout.write_all(out.as_bytes()).await;
                    let _ = stdout.write_all(b"\n").await;
                    let _ = stdout.flush().await;
                }
            }
        }
    });
}
