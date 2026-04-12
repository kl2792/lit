//! MCP (Model Context Protocol) server for the lit tool.
//!
//! Reads JSON-RPC 2.0 messages from stdin (newline-delimited), dispatches
//! tool calls to the lit library, and writes JSON-RPC responses to stdout.
//! Stderr is free for logging.

use std::sync::Arc;

use lit::{db, mcp};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::main]
async fn main() {
    let db_path = lit::resolve_db_path();

    let database = match db::Db::open(&db_path) {
        Ok(db) => Arc::new(db),
        Err(e) => {
            eprintln!("[lit-mcp] failed to open database: {}", e);
            std::process::exit(1);
        }
    };

    let ctx = mcp::make_context(database);

    eprintln!("[lit-mcp] server started, reading from stdin");

    let stdin = tokio::io::stdin();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();
    let mut stdout = tokio::io::stdout();

    loop {
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
                let err = mcp::make_error(Value::Null, -32700, &format!("parse error: {}", e));
                let out = serde_json::to_string(&err).unwrap();
                let _ = stdout.write_all(out.as_bytes()).await;
                let _ = stdout.write_all(b"\n").await;
                let _ = stdout.flush().await;
                continue;
            }
        };

        if let Some(response) = mcp::handle_message(&ctx, &msg).await {
            let out = serde_json::to_string(&response).unwrap();
            let _ = stdout.write_all(out.as_bytes()).await;
            let _ = stdout.write_all(b"\n").await;
            let _ = stdout.flush().await;
        }
    }
}
