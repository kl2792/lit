pub mod api;
pub mod bibtex;
pub mod citekey;
pub mod cmd;
pub mod db;
pub mod detect;
pub mod format;
pub mod html;
pub mod http;
pub mod mcp;
pub mod paths;
pub mod sanitize;

/// `lit --version` and the `lit_version` that `lit closure` records: crate
/// version and git short hash of the build.
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("LIT_GIT_HASH"), ")");

// Re-export key types for MCP and other library consumers.
pub use api::PaperResult;
pub use cmd::add::AddResult;
pub use cmd::LookupResult;
