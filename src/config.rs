//! Git-style hierarchical configuration.
//!
//! Reads `.litconfig` files from three locations (lowest to highest priority):
//! 1. `/etc/litconfig` — system-wide
//! 2. `~/.litconfig` — user global
//! 3. `<cwd>/.litconfig` — project-specific
//! 4. Environment variables — override everything
//!
//! Format is TOML. Missing or invalid files are silently skipped.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Global singleton.
static CONFIG: OnceLock<Config> = OnceLock::new();

/// Resolved configuration from all sources.
#[derive(Debug, Default, Clone)]
pub struct Config {
    pub db_path: Option<String>,
    pub pdf_dir: Option<String>,
    pub pdf_extractor: Option<String>,
    pub email: Option<String>,
    pub s2_api_key: Option<String>,
    pub timeout: Option<u64>,
    pub ttl_search: Option<u64>,
    pub ttl_lookup: Option<u64>,
    pub no_color: Option<bool>,
}

/// Intermediate TOML representation.
#[derive(serde::Deserialize, Default)]
struct ConfigFile {
    core: Option<CoreSection>,
    cache: Option<CacheSection>,
    api: Option<ApiSection>,
    extract: Option<ExtractSection>,
}

#[derive(serde::Deserialize, Default)]
struct CoreSection {
    db_path: Option<String>,
    pdf_dir: Option<String>,
    email: Option<String>,
    timeout: Option<u64>,
}

#[derive(serde::Deserialize, Default)]
struct CacheSection {
    ttl_search: Option<u64>,
    ttl_lookup: Option<u64>,
}

#[derive(serde::Deserialize, Default)]
struct ApiSection {
    s2_key: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct ExtractSection {
    pdf_extractor: Option<String>,
}

impl Config {
    /// Load configuration from all sources.
    ///
    /// Reads `/etc/litconfig`, `~/.litconfig`, and `<cwd>/.litconfig` in order,
    /// then applies environment variable overrides.
    pub fn load() -> Config {
        let mut cfg = Config::default();

        // 1. System-wide
        cfg.merge_file(Path::new("/etc/litconfig"));

        // 2. User global
        if let Some(home) = home_dir() {
            cfg.merge_file(&home.join(".litconfig"));
        }

        // 3. Project-specific
        if let Ok(cwd) = std::env::current_dir() {
            cfg.merge_file(&cwd.join(".litconfig"));
        }

        // 4. Environment variable overrides
        cfg.apply_env();

        // Expand tildes in path values
        cfg.expand_paths();

        cfg
    }

    /// Return the global singleton, initializing on first call.
    pub fn get() -> &'static Config {
        CONFIG.get_or_init(Config::load)
    }

    /// Database path with platform default.
    pub fn db_path(&self) -> PathBuf {
        if let Some(ref p) = self.db_path {
            return PathBuf::from(p);
        }
        let home = home_dir().unwrap_or_else(|| PathBuf::from("."));
        if cfg!(target_os = "macos") {
            home.join("Library/Application Support/lit/lit.db")
        } else {
            home.join(".local/share/lit/lit.db")
        }
    }

    /// PDF storage directory with platform default.
    pub fn pdf_dir(&self) -> PathBuf {
        if let Some(ref p) = self.pdf_dir {
            return PathBuf::from(p);
        }
        let home = home_dir().unwrap_or_else(|| PathBuf::from("."));
        if cfg!(target_os = "macos") {
            home.join("Library/Application Support/lit/pdf")
        } else {
            home.join(".local/share/lit/pdf")
        }
    }

    /// PDF extractor command (None means use built-in pdftotext).
    pub fn pdf_extractor(&self) -> Option<&str> {
        self.pdf_extractor.as_deref()
    }

    /// Email for Unpaywall API.
    pub fn email(&self) -> &str {
        self.email.as_deref().unwrap_or("lit-user@example.com")
    }

    /// Semantic Scholar API key.
    pub fn s2_api_key(&self) -> Option<&str> {
        self.s2_api_key.as_deref().filter(|k| !k.is_empty())
    }

    /// HTTP timeout in seconds.
    pub fn timeout(&self) -> u64 {
        self.timeout.unwrap_or(15)
    }

    /// Cache TTL for search results (seconds).
    pub fn ttl_search(&self) -> u64 {
        self.ttl_search.unwrap_or(86400)
    }

    /// Cache TTL for DOI/arXiv/ISBN lookups (seconds).
    pub fn ttl_lookup(&self) -> u64 {
        self.ttl_lookup.unwrap_or(604800)
    }

    /// Whether color output is disabled by config.
    pub fn no_color(&self) -> bool {
        self.no_color.unwrap_or(false)
    }

    /// Merge values from a TOML file. Missing or invalid files are skipped.
    fn merge_file(&mut self, path: &Path) {
        let content = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => return,
        };
        let file: ConfigFile = match toml::from_str(&content) {
            Ok(f) => f,
            Err(e) => {
                eprintln!(
                    "warning: ignoring invalid config {}: {}",
                    path.display(),
                    e
                );
                return;
            }
        };
        self.merge_parsed(&file);
    }

    /// Merge a parsed TOML config into self (later values override).
    fn merge_parsed(&mut self, file: &ConfigFile) {
        if let Some(ref core) = file.core {
            if core.db_path.is_some() {
                self.db_path.clone_from(&core.db_path);
            }
            if core.pdf_dir.is_some() {
                self.pdf_dir.clone_from(&core.pdf_dir);
            }
            if core.email.is_some() {
                self.email.clone_from(&core.email);
            }
            if core.timeout.is_some() {
                self.timeout = core.timeout;
            }
        }
        if let Some(ref cache) = file.cache {
            if cache.ttl_search.is_some() {
                self.ttl_search = cache.ttl_search;
            }
            if cache.ttl_lookup.is_some() {
                self.ttl_lookup = cache.ttl_lookup;
            }
        }
        if let Some(ref api) = file.api
            && api.s2_key.is_some()
        {
            self.s2_api_key.clone_from(&api.s2_key);
        }
        if let Some(ref extract) = file.extract
            && extract.pdf_extractor.is_some()
        {
            self.pdf_extractor.clone_from(&extract.pdf_extractor);
        }
    }

    /// Apply environment variable overrides.
    fn apply_env(&mut self) {
        if let Ok(v) = std::env::var("LIT_DB_PATH") {
            self.db_path = Some(v);
        }
        if let Ok(v) = std::env::var("LIT_PDF_DIR") {
            self.pdf_dir = Some(v);
        }
        if let Ok(v) = std::env::var("LIT_PDF_EXTRACTOR") {
            self.pdf_extractor = Some(v);
        }
        if let Ok(v) = std::env::var("LIT_EMAIL") {
            self.email = Some(v);
        }
        if let Ok(v) = std::env::var("S2_API_KEY") {
            self.s2_api_key = Some(v);
        }
        if let Ok(v) = std::env::var("CURL_TIMEOUT")
            && let Ok(n) = v.parse()
        {
            self.timeout = Some(n);
        }
        if let Ok(v) = std::env::var("LIT_TTL_SEARCH")
            && let Ok(n) = v.parse()
        {
            self.ttl_search = Some(n);
        }
        if let Ok(v) = std::env::var("LIT_TTL_LOOKUP")
            && let Ok(n) = v.parse()
        {
            self.ttl_lookup = Some(n);
        }
        if let Ok(v) = std::env::var("NO_COLOR")
            && !v.is_empty()
        {
            self.no_color = Some(true);
        }
    }

    /// Expand `~` prefix in path-valued fields.
    fn expand_paths(&mut self) {
        if let Some(ref mut p) = self.db_path {
            *p = expand_tilde(p);
        }
        if let Some(ref mut p) = self.pdf_dir {
            *p = expand_tilde(p);
        }
        if let Some(ref mut p) = self.pdf_extractor {
            *p = expand_tilde(p);
        }
    }

    /// Parse a TOML string into a Config (for testing).
    #[cfg(test)]
    fn from_toml(s: &str) -> Config {
        let file: ConfigFile = toml::from_str(s).unwrap();
        let mut cfg = Config::default();
        cfg.merge_parsed(&file);
        cfg
    }

    /// Merge another config into self (later overrides earlier).
    #[cfg(test)]
    fn merge(&mut self, other: &Config) {
        if other.db_path.is_some() {
            self.db_path.clone_from(&other.db_path);
        }
        if other.pdf_dir.is_some() {
            self.pdf_dir.clone_from(&other.pdf_dir);
        }
        if other.pdf_extractor.is_some() {
            self.pdf_extractor.clone_from(&other.pdf_extractor);
        }
        if other.email.is_some() {
            self.email.clone_from(&other.email);
        }
        if other.s2_api_key.is_some() {
            self.s2_api_key.clone_from(&other.s2_api_key);
        }
        if other.timeout.is_some() {
            self.timeout = other.timeout;
        }
        if other.ttl_search.is_some() {
            self.ttl_search = other.ttl_search;
        }
        if other.ttl_lookup.is_some() {
            self.ttl_lookup = other.ttl_lookup;
        }
        if other.no_color.is_some() {
            self.no_color = other.no_color;
        }
    }
}

/// Expand leading `~` to the user's home directory.
fn expand_tilde(path: &str) -> String {
    if !path.starts_with('~') {
        return path.to_string();
    }
    let home = home_dir()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|| "~".to_string());
    if path == "~" {
        home
    } else if path.starts_with("~/") || path.starts_with("~\\") {
        format!("{}{}", home, &path[1..])
    } else {
        // ~otheruser — don't expand
        path.to_string()
    }
}

/// Get the user's home directory.
fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_toml() {
        let toml = r#"
[core]
db_path = "/tmp/test.db"
pdf_dir = "/tmp/papers"
email = "test@example.com"
timeout = 30

[cache]
ttl_search = 3600
ttl_lookup = 7200

[api]
s2_key = "my-key"

[extract]
pdf_extractor = "/usr/bin/extract"
"#;
        let cfg = Config::from_toml(toml);
        assert_eq!(cfg.db_path.as_deref(), Some("/tmp/test.db"));
        assert_eq!(cfg.pdf_dir.as_deref(), Some("/tmp/papers"));
        assert_eq!(cfg.email.as_deref(), Some("test@example.com"));
        assert_eq!(cfg.timeout, Some(30));
        assert_eq!(cfg.ttl_search, Some(3600));
        assert_eq!(cfg.ttl_lookup, Some(7200));
        assert_eq!(cfg.s2_api_key.as_deref(), Some("my-key"));
        assert_eq!(cfg.pdf_extractor.as_deref(), Some("/usr/bin/extract"));
    }

    #[test]
    fn parse_partial_toml() {
        let toml = r#"
[core]
email = "partial@example.com"
"#;
        let cfg = Config::from_toml(toml);
        assert_eq!(cfg.email.as_deref(), Some("partial@example.com"));
        assert!(cfg.db_path.is_none());
        assert!(cfg.pdf_dir.is_none());
        assert!(cfg.timeout.is_none());
        assert!(cfg.ttl_search.is_none());
        assert!(cfg.s2_api_key.is_none());
    }

    #[test]
    fn merge_configs() {
        let base_toml = r#"
[core]
db_path = "/base/lit.db"
email = "base@example.com"
timeout = 10

[cache]
ttl_search = 1000
"#;
        let override_toml = r#"
[core]
email = "override@example.com"

[cache]
ttl_search = 2000
ttl_lookup = 3000
"#;
        let base = Config::from_toml(base_toml);
        let over = Config::from_toml(override_toml);
        let mut merged = base.clone();
        merged.merge(&over);

        // Overridden
        assert_eq!(merged.email.as_deref(), Some("override@example.com"));
        assert_eq!(merged.ttl_search, Some(2000));
        assert_eq!(merged.ttl_lookup, Some(3000));
        // Preserved from base
        assert_eq!(merged.db_path.as_deref(), Some("/base/lit.db"));
        assert_eq!(merged.timeout, Some(10));
    }

    #[test]
    fn env_overrides_config() {
        let toml = r#"
[core]
timeout = 10
"#;
        let mut cfg = Config::from_toml(toml);
        assert_eq!(cfg.timeout, Some(10));

        // Simulate env override
        // SAFETY: test-only, single-threaded test
        unsafe { std::env::set_var("CURL_TIMEOUT", "99"); }
        cfg.apply_env();
        assert_eq!(cfg.timeout, Some(99));
        unsafe { std::env::remove_var("CURL_TIMEOUT"); }
    }

    #[test]
    fn tilde_expansion() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/test".to_string());
        assert_eq!(expand_tilde("~/papers"), format!("{}/papers", home));
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("/absolute/path"), "/absolute/path");
        assert_eq!(expand_tilde("relative/path"), "relative/path");
        assert_eq!(expand_tilde("~otheruser/foo"), "~otheruser/foo");
    }

    #[test]
    fn missing_file_returns_empty() {
        let mut cfg = Config::default();
        cfg.merge_file(Path::new("/nonexistent/.litconfig"));
        assert!(cfg.db_path.is_none());
        assert!(cfg.email.is_none());
    }

    #[test]
    fn invalid_toml_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".litconfig");
        std::fs::write(&path, "this is not [valid toml @@@@").unwrap();
        let mut cfg = Config::default();
        cfg.merge_file(&path);
        assert!(cfg.db_path.is_none());
    }

    #[test]
    fn defaults_when_empty() {
        let cfg = Config::default();
        assert_eq!(cfg.timeout(), 15);
        assert_eq!(cfg.ttl_search(), 86400);
        assert_eq!(cfg.ttl_lookup(), 604800);
        assert_eq!(cfg.email(), "lit-user@example.com");
        assert_eq!(cfg.no_color(), false);
        assert!(cfg.s2_api_key().is_none());
        assert!(cfg.pdf_extractor().is_none());
    }
}
