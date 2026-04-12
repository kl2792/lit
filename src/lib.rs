#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]

pub mod api;
pub mod bibtex;
pub mod citekey;
pub mod cmd;
pub mod config;
pub mod db;
pub mod detect;
pub mod format;
pub mod http;
pub mod mcp;


use std::path::PathBuf;

// Re-export key types for MCP and other library consumers.
pub use api::PaperResult;
pub use cmd::add::AddResult;
pub use cmd::LookupResult;

/// Resolve the database path.
///
/// Uses `Config::get().db_path()` (which respects `.litconfig` files and
/// `LIT_DB_PATH` env var). Creates the parent directory if it doesn't exist.
pub fn resolve_db_path() -> PathBuf {
    let path = config::Config::get().db_path();
    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        let _ = std::fs::create_dir_all(parent);
    }
    path
}

/// Resolve the paper storage directory.
///
/// Uses `Config::get().pdf_dir()` (which respects `.litconfig` files and
/// `LIT_PDF_DIR` env var). Creates the directory if it doesn't exist.
pub fn find_pdf_base() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let base = config::Config::get().pdf_dir();
    std::fs::create_dir_all(&base)?;
    Ok(base)
}
