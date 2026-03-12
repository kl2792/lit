pub mod api;
pub mod bibtex;
pub mod citekey;
pub mod cmd;
pub mod db;
pub mod detect;
pub mod format;
pub mod http;

// Re-export key types for MCP and other library consumers.
pub use api::PaperResult;
pub use cmd::add::AddResult;
pub use cmd::LookupResult;
