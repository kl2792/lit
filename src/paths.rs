//! Where lit's state lives: the database file and the project root.
//!
//! `lit` and `lit-mcp` are separate binaries, so neither can call the other's
//! resolver. A copy in either one drifts silently and leaves the two opening
//! different databases, which this project has already hit once. Every resolver
//! lives here and both binaries call it, so the literals below appear once.

use std::path::{Path, PathBuf};

const DB_ENV: &str = "LIT_DB_PATH";

/// Database location relative to the executable's grandparent directory.
const DB_DEFAULT_RELATIVE: &str = "etc/lit/lit.db";

const ROOT_ENV: &str = "LIT_PROJECT_ROOT";

/// Directory whose presence, non-empty, marks a directory as the project root.
const ARTIFACT_DIR: &str = "etc/pdf";

/// Resolve the main database path and name the source that won.
///
/// `LIT_DB_PATH` wins; otherwise the path is taken relative to the executable,
/// which `docs/DESIGN.md` records as a deviation from the intended resolution.
fn resolve_db_path(env_value: Option<String>, exe: &Path) -> (PathBuf, &'static str) {
    match env_value {
        Some(p) => (PathBuf::from(p), DB_ENV),
        None => {
            let here = Path::new(".");
            let base = exe.parent().unwrap_or(here).parent().unwrap_or(here);
            (base.join(DB_DEFAULT_RELATIVE), "default, relative to the executable")
        }
    }
}

/// The database path for this process, with the source that set it.
///
/// The single entry point for every binary: `lit` and `lit-mcp` must agree.
pub fn db_path() -> (PathBuf, &'static str) {
    resolve_db_path(
        std::env::var(DB_ENV).ok(),
        &std::env::current_exe().unwrap_or_default(),
    )
}

/// Resolve the project root from an override and a working directory.
///
/// `LIT_PROJECT_ROOT` wins; otherwise the search walks up from `cwd` for a
/// non-empty `etc/pdf/`. Failure is an error rather than the working directory,
/// because a scan rooted at the wrong place finds no artifacts and reports the
/// same success as a scan that found everything intact.
fn resolve_project_root(env_value: Option<PathBuf>, cwd: &Path) -> Result<PathBuf, String> {
    if let Some(root) = env_value {
        return Ok(root);
    }
    let mut dir = cwd;
    loop {
        let artifacts = dir.join(ARTIFACT_DIR);
        if artifacts.is_dir()
            && std::fs::read_dir(&artifacts).map(|mut d| d.next().is_some()).unwrap_or(false)
        {
            return Ok(dir.to_path_buf());
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => break,
        }
    }
    Err(format!(
        "no project root found above {}: no non-empty {}/ directory in any parent. Run from inside the project or set {}",
        cwd.display(),
        ARTIFACT_DIR,
        ROOT_ENV
    ))
}

/// The project root for this process.
pub fn project_root() -> Result<PathBuf, String> {
    let cwd = std::env::current_dir()
        .map_err(|e| format!("cannot read the working directory: {}", e))?;
    resolve_project_root(std::env::var_os(ROOT_ENV).map(PathBuf::from), &cwd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_db_path_wins_over_the_executable_relative_default() {
        let (path, source) = resolve_db_path(
            Some("/var/lib/lit/custom.db".to_string()),
            Path::new("/usr/local/bin/lit"),
        );
        assert_eq!(path, PathBuf::from("/var/lib/lit/custom.db"));
        assert_eq!(source, "LIT_DB_PATH");
    }

    #[test]
    fn default_db_path_is_the_executable_grandparent() {
        let (path, source) = resolve_db_path(None, Path::new("/opt/lit/bin/lit"));
        assert_eq!(path, PathBuf::from("/opt/lit/etc/lit/lit.db"));
        assert_eq!(source, "default, relative to the executable");
    }

    /// Both binaries call `db_path`, so pinning it to the resolver pins them to
    /// each other: any divergence would have to be a second resolver, which the
    /// private `resolve_db_path` and the single copy of the literals prevent.
    #[test]
    fn db_path_is_the_resolver_applied_to_this_process() {
        let expected = resolve_db_path(
            std::env::var(DB_ENV).ok(),
            &std::env::current_exe().unwrap_or_default(),
        );
        assert_eq!(db_path(), expected);
    }

    #[test]
    fn project_root_env_override_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let root = resolve_project_root(Some(PathBuf::from("/srv/corpus")), tmp.path()).unwrap();
        assert_eq!(root, PathBuf::from("/srv/corpus"));
    }

    #[test]
    fn project_root_is_the_ancestor_holding_a_non_empty_artifact_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let artifacts = tmp.path().join(ARTIFACT_DIR).join("smith2020paper");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::write(artifacts.join("source.yaml"), "title: T\n").unwrap();
        let start = tmp.path().join("a/b/c");
        std::fs::create_dir_all(&start).unwrap();

        let root = resolve_project_root(None, &start).unwrap();
        assert_eq!(
            std::fs::canonicalize(root).unwrap(),
            std::fs::canonicalize(tmp.path()).unwrap()
        );
    }

    /// A missing root must stop the run, not scan an empty directory and pass.
    #[test]
    fn project_root_without_artifacts_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = resolve_project_root(None, tmp.path()).unwrap_err();
        assert!(err.contains("no project root found"), "unexpected message: {}", err);
    }

    /// An empty `etc/pdf/` is a stale checkout, not a root worth scanning.
    #[test]
    fn empty_artifact_dir_does_not_count_as_a_root() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(ARTIFACT_DIR)).unwrap();
        assert!(resolve_project_root(None, tmp.path()).is_err());
    }
}
