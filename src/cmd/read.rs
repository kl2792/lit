/// `lit read <id>` -- Get path to readable text of a downloaded paper.
///
/// Searches `etc/pdf/` for a matching directory, ensures text is available
/// (runs pdftotext if needed), and returns the file path.
/// The caller (Claude) can then use Read tool with offset/limit.

use std::path::{Path, PathBuf};

use super::artifact::{dir_paper_ids, holds_content, owner_dir, paper_id, Target};
use super::Context;

/// Why locating a paper's text failed, so `plan_in` can fetch on "no local
/// source at all" and surface the real cause of "source directory exists but
/// is unreadable".
#[derive(Debug)]
pub enum ReadError {
    /// No directory under etc/pdf matches the query.
    NotFound(String),
    /// A matching directory exists but no main tex file or other readable
    /// content could be identified in it.
    Unreadable { dir: PathBuf, message: String },
    /// Any other failure (missing etc/pdf root, IO error, ambiguous match).
    Other(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::NotFound(query) => write!(
                f,
                "paper '{}' not found locally. Download it first with an arXiv ID or DOI.",
                query
            ),
            ReadError::Unreadable { dir, message } => write!(
                f,
                "source directory {} exists but no main tex file or readable text was identified: {}",
                dir.display(),
                message
            ),
            ReadError::Other(message) => write!(f, "{}", message),
        }
    }
}

impl std::error::Error for ReadError {}

/// Find the paper directory under `base` matching the given query.
///
/// An arXiv id or DOI is looked up by identity first: the directory whose
/// source.yaml names it (`owner_dir`). Failing that, or for any other query,
/// directory names are matched exactly, then without dots, then by
/// substring; a non-id query then searches source.yaml text. For an id, a
/// directory whose source.yaml names other papers is never a match, whatever
/// its name. Dot-prefixed directories (staging) never match.
fn find_paper_dir_in(base: &Path, query: &str) -> Result<PathBuf, ReadError> {
    let io_err = |e: std::io::Error| ReadError::Other(e.to_string());
    let id = paper_id(query);
    if let Some(id) = &id {
        if let Some(dir) = owner_dir(base, id).map_err(io_err)? {
            return Ok(dir);
        }
    }
    // No directory names the id, so one naming any paper names another.
    let eligible = |dir: &Path| id.is_none() || dir_paper_ids(dir).is_empty();
    let normalized = query.trim().to_lowercase().replace('/', "_");

    // Exact match
    let exact = base.join(&normalized);
    if exact.is_dir() && eligible(&exact) {
        return Ok(exact);
    }

    // Without dots (arxiv IDs)
    let dotted = normalized.replace('.', "");
    let exact2 = base.join(&dotted);
    if exact2.is_dir() && eligible(&exact2) {
        return Ok(exact2);
    }

    // Substring match
    let dirs = artifact_dirs(base).map_err(io_err)?;
    let mut matches: Vec<PathBuf> = dirs
        .iter()
        .filter(|dir| {
            let name = dir.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
            (name.contains(&normalized) || normalized.contains(&name)) && eligible(dir)
        })
        .cloned()
        .collect();

    if matches.is_empty() && id.is_none() {
        let needle = query.trim().to_lowercase();
        matches = dirs
            .into_iter()
            .filter(|dir| {
                std::fs::read_to_string(dir.join("source.yaml"))
                    .is_ok_and(|content| content.to_lowercase().contains(&needle))
            })
            .collect();
    }

    match matches.len() {
        0 => Err(ReadError::NotFound(query.to_string())),
        1 => Ok(matches.into_iter().next().unwrap()),
        _ => {
            let names: Vec<String> = matches
                .iter()
                .map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned())
                .collect();
            Err(ReadError::Other(format!("ambiguous: {}", names.join(", "))))
        }
    }
}

/// The artifact directories under `base`: subdirectories not prefixed with a
/// dot, so a download's or `lit misc`'s staging directory is never read.
fn artifact_dirs(base: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(base)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && !entry.file_name().to_string_lossy().starts_with('.') {
            dirs.push(entry.path());
        }
    }
    Ok(dirs)
}

/// The artifact root every reader and writer shares.
///
/// A fourth walk for a literal `etc/pdf` would be a fourth answer to one
/// question, so this defers to the resolver that `check` and `download` use
/// and that `LIT_PROJECT_ROOT` overrides.
pub fn find_pdf_base() -> Result<PathBuf, Box<dyn std::error::Error>> {
    crate::paths::artifact_dir().map_err(Into::into)
}

/// Ensure readable text exists and return the path.
///
/// Priority: .tex main file > paper.txt > pdftotext paper.pdf (cached as paper.txt)
pub(crate) fn ensure_text(dir: &Path) -> Result<ReadResult, Box<dyn std::error::Error>> {
    // 1. Check for .tex source
    if let Some(main_tex) = find_main_tex(dir) {
        let tex_files = list_tex_files(main_tex.parent().unwrap_or(dir));
        return Ok(ReadResult {
            path: main_tex,
            format: "tex".to_string(),
            extra_files: tex_files,
        });
    }

    // 2. Check for paper.txt
    let txt_path = dir.join("paper.txt");
    if txt_path.exists() {
        let meta = std::fs::metadata(&txt_path)?;
        if meta.len() > 0 {
            return Ok(ReadResult {
                path: txt_path,
                format: "txt".to_string(),
                extra_files: vec![],
            });
        }
    }

    // 3. Run pdftotext on paper.pdf
    let pdf_path = dir.join("paper.pdf");
    if pdf_path.exists() {
        run_pdftotext(&pdf_path, &txt_path)?;
        return Ok(ReadResult {
            path: txt_path,
            format: "txt (generated from PDF)".to_string(),
            extra_files: vec![],
        });
    }

    Err(format!("no readable content in {}", dir.display()).into())
}

/// Find the main .tex file in a directory: the toplevel source arXiv's
/// `00README.json` names, which may sit in a subdirectory, else a top-level
/// file with `\documentclass`, else `main.tex` or `paper.tex`.
fn find_main_tex(dir: &Path) -> Option<PathBuf> {
    manifest_toplevel(dir).or_else(|| scan_main_tex(dir))
}

/// The existing toplevel source file `dir/00README.json` names, if any. The
/// manifest and the tarball beside it are downloaded data, so a name counts
/// only if it resolves, through `..` and symlinks alike, to a file inside
/// `dir`; otherwise it could point `lit read` anywhere.
fn manifest_toplevel(dir: &Path) -> Option<PathBuf> {
    let manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("00README.json")).ok()?).ok()?;
    let root = dir.canonicalize().ok()?;
    manifest["sources"]
        .as_array()?
        .iter()
        .filter(|source| source["usage"] == "toplevel")
        .filter_map(|source| source["filename"].as_str())
        .map(|name| dir.join(name))
        .find(|path| path.canonicalize().is_ok_and(|real| real.starts_with(&root) && real.is_file()))
}

/// Find the main .tex file among the top-level files of `dir`.
fn scan_main_tex(dir: &Path) -> Option<PathBuf> {
    let tex_files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |e| e == "tex"))
        .collect();

    if tex_files.is_empty() {
        return None;
    }

    // Find file with \documentclass (or \documentstyle, its LaTeX 2.09 form)
    tex_files
        .iter()
        .find(|p| {
            std::fs::read_to_string(p)
                .map(|s| s.contains("\\documentclass") || s.contains("\\documentstyle"))
                .unwrap_or(false)
        })
        .or_else(|| {
            tex_files.iter().find(|p| {
                let name = p.file_stem().unwrap_or_default().to_string_lossy();
                name == "main" || name == "paper"
            })
        })
        .cloned()
}

/// List all .tex files in a directory (for the extra_files field).
fn list_tex_files(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .ok()
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .extension()
                        .map_or(false, |ext| ext == "tex")
                })
                .map(|e| e.path().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// Run pdftotext and write output to txt_path.
fn run_pdftotext(pdf: &Path, txt_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let status = std::process::Command::new("pdftotext")
        .arg("-layout")
        .arg(pdf)
        .arg(txt_path)
        .status()?;

    if !status.success() {
        return Err("pdftotext failed".into());
    }
    Ok(())
}

#[derive(Debug)]
pub struct ReadResult {
    pub path: PathBuf,
    pub format: String,
    pub extra_files: Vec<String>,
}

/// Ensure readable text in `dir`, naming the directory on failure.
fn read_dir(dir: PathBuf) -> Result<ReadResult, ReadError> {
    ensure_text(&dir).map_err(|e| ReadError::Unreadable {
        dir,
        message: e.to_string(),
    })
}

/// Find the paper under `base` and ensure readable text, with typed errors.
#[cfg(test)]
fn read_in(base: &Path, query: &str) -> Result<ReadResult, ReadError> {
    read_dir(find_paper_dir_in(base, query)?)
}

/// What `lit read` does for a query.
#[derive(Debug, PartialEq)]
pub(crate) enum Plan {
    /// Read the local copy in this directory.
    Read(PathBuf),
    /// Download this paper, then read it.
    Fetch(Target),
}

/// Decide whether `query` is read locally or fetched first.
///
/// A query naming no local directory is fetched when it is an arXiv id or
/// DOI. A local directory is fetched again when it holds only metadata, or
/// when `no_cache` is set, provided the query or its source.yaml names the
/// paper; otherwise it is read as it stands.
pub(crate) fn plan_in(base: &Path, query: &str, no_cache: bool) -> Result<Plan, ReadError> {
    match find_paper_dir_in(base, query) {
        Ok(dir) => {
            let id = paper_id(query).or_else(|| dir_paper_ids(&dir).into_iter().next());
            match id {
                Some(id) if no_cache || !holds_content(&dir) => Ok(Plan::Fetch(id)),
                _ => Ok(Plan::Read(dir)),
            }
        }
        Err(ReadError::NotFound(missing)) => paper_id(query).map(Plan::Fetch).ok_or(ReadError::NotFound(missing)),
        Err(e) => Err(e),
    }
}

/// Locate `query`'s readable text, fetching the paper first when `plan_in`
/// says so. Returns the result and whether a fetch ran.
pub async fn read_or_fetch(ctx: &Context, query: &str) -> Result<(ReadResult, bool), Box<dyn std::error::Error>> {
    let base = find_pdf_base()?;
    match plan_in(&base, query, ctx.no_cache)? {
        Plan::Read(dir) => Ok((read_dir(dir)?, false)),
        Plan::Fetch(id) => Ok((read_dir(super::download::fetch(ctx, &id).await?)?, true)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a paper directory under `base` containing a single .tex file.
    fn make_paper_dir(base: &Path, name: &str, tex_name: &str, tex_content: &str) -> PathBuf {
        let dir = base.join(name);
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join(tex_name), tex_content).unwrap();
        dir
    }

    /// `read`, `check` and `download` must name the same artifact root, which
    /// holds only while this is the shared resolver rather than a copy of it.
    #[test]
    fn find_pdf_base_is_the_shared_artifact_dir_resolver() {
        assert_eq!(find_pdf_base().ok(), crate::paths::artifact_dir().ok());
    }

    #[test]
    fn test_find_main_tex_documentclass() {
        let base = tempfile::tempdir().unwrap();
        let dir = make_paper_dir(
            base.path(),
            "modern2020paper",
            "body.tex",
            "\\documentclass{article}\n\\begin{document}\\end{document}\n",
        );
        let main = find_main_tex(&dir).expect("should identify \\documentclass file");
        assert!(main.ends_with("body.tex"));
    }

    #[test]
    fn test_find_main_tex_documentstyle() {
        // LaTeX 2.09 sources (e.g. arXiv cs/0312038 blamecorr.tex) use \documentstyle.
        let base = tempfile::tempdir().unwrap();
        let dir = make_paper_dir(
            base.path(),
            "chockler2004responsibility",
            "blamecorr.tex",
            "\\documentstyle[12pt,chicagob]{article}\n\\begin{document}\\end{document}\n",
        );
        let main = find_main_tex(&dir).expect("should identify \\documentstyle file");
        assert!(main.ends_with("blamecorr.tex"));
    }

    #[test]
    fn find_main_tex_follows_the_arxiv_manifest_into_a_subdirectory() {
        // arXiv 2505.16178 keeps its main file at latex/acl_latex.tex, named
        // only by 00README.json; a top-level scan found nothing to read.
        let base = tempfile::tempdir().unwrap();
        let dir = make_paper_dir(base.path(), "zhang2025understandingb", "acl.sty", "% style\n");
        std::fs::create_dir(dir.join("latex")).unwrap();
        std::fs::write(dir.join("latex/acl_latex.tex"), "\\documentclass{article}\n").unwrap();
        std::fs::write(dir.join("latex/appendix.tex"), "\\section{A}\n").unwrap();
        std::fs::write(
            dir.join("00README.json"),
            r#"{"sources": [{"usage": "toplevel", "filename": "latex/acl_latex.tex"}], "spec_version": 1}"#,
        )
        .unwrap();
        let result = read_in(base.path(), "zhang2025understandingb").unwrap();
        assert_eq!(result.path, dir.join("latex/acl_latex.tex"));
        assert!(result.extra_files.iter().any(|f| f.ends_with("latex/appendix.tex")), "{:?}", result.extra_files);
    }

    #[test]
    fn find_main_tex_ignores_a_manifest_naming_a_missing_file() {
        let base = tempfile::tempdir().unwrap();
        let dir = make_paper_dir(base.path(), "doe2020paper", "body.tex", "\\documentclass{article}\n");
        std::fs::write(dir.join("00README.json"), r#"{"sources": [{"usage": "toplevel", "filename": "gone.tex"}]}"#).unwrap();
        assert_eq!(find_main_tex(&dir), Some(dir.join("body.tex")));
    }

    #[test]
    fn find_main_tex_ignores_a_manifest_naming_a_file_outside_the_directory() {
        let base = tempfile::tempdir().unwrap();
        std::fs::write(base.path().join("outside.tex"), "\\documentclass{article}\n").unwrap();
        let dir = make_paper_dir(base.path(), "doe2020paper", "body.tex", "\\documentclass{article}\n");
        std::fs::write(dir.join("00README.json"), r#"{"sources": [{"usage": "toplevel", "filename": "../outside.tex"}]}"#).unwrap();
        assert_eq!(find_main_tex(&dir), Some(dir.join("body.tex")));
    }

    #[test]
    fn test_read_in_not_found() {
        // No matching directory at all -> ReadError::NotFound (download fallback applies).
        let base = tempfile::tempdir().unwrap();
        let err = read_in(base.path(), "nosuchpaper").unwrap_err();
        assert!(matches!(err, ReadError::NotFound(_)));
        assert!(err.to_string().contains("nosuchpaper"));
    }

    #[test]
    fn test_read_in_unreadable_names_directory() {
        // Directory exists but has no identifiable main tex and no txt/pdf
        // -> ReadError::Unreadable naming the directory (not "not found").
        let base = tempfile::tempdir().unwrap();
        let dir = make_paper_dir(
            base.path(),
            "stub2024fragment",
            "macros.tex",
            "\\newcommand{\\wt}{\\mbox{\\it wt}}\n",
        );
        let err = read_in(base.path(), "stub2024fragment").unwrap_err();
        assert!(matches!(err, ReadError::Unreadable { .. }));
        assert!(err.to_string().contains(&dir.display().to_string()));
        assert!(err.to_string().contains("exists"));
    }

    #[test]
    fn test_read_in_documentstyle_end_to_end() {
        let base = tempfile::tempdir().unwrap();
        make_paper_dir(
            base.path(),
            "chockler2004responsibility",
            "blamecorr.tex",
            "\\documentstyle[12pt,chicagob]{article}\n\\begin{document}\\end{document}\n",
        );
        let result = read_in(base.path(), "chockler2004responsibility").unwrap();
        assert_eq!(result.format, "tex");
        assert!(result.path.ends_with("blamecorr.tex"));
    }

    /// Make `base/name` with `source.yaml` holding `yaml` and, if given, one
    /// readable file.
    fn artifact(base: &Path, name: &str, yaml: &str, content: Option<(&str, &str)>) -> PathBuf {
        let dir = base.join(name);
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("source.yaml"), yaml).unwrap();
        if let Some((file, text)) = content {
            std::fs::write(dir.join(file), text).unwrap();
        }
        dir
    }

    const MAIN_TEX: (&str, &str) = ("main.tex", "\\documentclass{article}\n");

    #[test]
    fn a_metadata_only_directory_is_planned_as_a_refetch() {
        // etc/pdf/ahmed2026validation held only source.yaml, so `lit read`
        // failed on it forever instead of fetching the source again.
        let base = tempfile::tempdir().unwrap();
        artifact(base.path(), "ahmed2026validation", "arxiv: \"2606.21078\"\n", None);
        let id = Target::Arxiv("2606.21078".into());
        assert_eq!(plan_in(base.path(), "2606.21078", false).unwrap(), Plan::Fetch(id.clone()));
        assert_eq!(plan_in(base.path(), "ahmed2026validation", false).unwrap(), Plan::Fetch(id));
    }

    #[test]
    fn no_cache_plans_a_refetch_of_a_readable_copy() {
        let base = tempfile::tempdir().unwrap();
        let dir = artifact(base.path(), "doe2020paper", "arxiv: \"2001.00001\"\n", Some(MAIN_TEX));
        assert_eq!(plan_in(base.path(), "2001.00001", false).unwrap(), Plan::Read(dir));
        assert_eq!(
            plan_in(base.path(), "2001.00001", true).unwrap(),
            Plan::Fetch(Target::Arxiv("2001.00001".into()))
        );
    }

    #[test]
    fn no_cache_reads_a_copy_whose_paper_is_unknown() {
        // Nothing names a paper to fetch, so the local copy is all there is.
        let base = tempfile::tempdir().unwrap();
        let dir = make_paper_dir(base.path(), "notes2020", "main.tex", "\\documentclass{article}\n");
        assert_eq!(plan_in(base.path(), "notes2020", true).unwrap(), Plan::Read(dir));
    }

    #[test]
    fn a_missing_doi_is_planned_as_a_doi_fetch() {
        // A bare DOI starts with a digit and used to be fetched as an arXiv
        // id, which requested https://arxiv.org/e-print/10.18653/... (404).
        let base = tempfile::tempdir().unwrap();
        assert_eq!(
            plan_in(base.path(), "10.18653/v1/2025.acl-long.228", false).unwrap(),
            Plan::Fetch(Target::Doi("10.18653/v1/2025.acl-long.228".into()))
        );
    }

    #[test]
    fn a_missing_free_text_query_is_not_found() {
        let base = tempfile::tempdir().unwrap();
        let err = plan_in(base.path(), "nosuchpaper", false).unwrap_err();
        assert!(matches!(err, ReadError::NotFound(_)));
        assert_eq!(
            err.to_string(),
            "paper 'nosuchpaper' not found locally. Download it first with an arXiv ID or DOI."
        );
    }

    #[test]
    fn read_finds_a_doi_artifact_by_its_source_yaml() {
        let base = tempfile::tempdir().unwrap();
        artifact(
            base.path(),
            "smith2025paper",
            "doi: \"10.18653/v1/2025.acl-long.228\"\n",
            Some(("paper.txt", "text")),
        );
        let result = read_in(base.path(), "10.18653/v1/2025.acl-long.228").unwrap();
        assert!(result.path.ends_with("smith2025paper/paper.txt"));
    }

    #[test]
    fn read_never_returns_a_directory_naming_another_paper() {
        // A directory named after one id but whose source.yaml names another
        // holds the other paper; reading the first id must not return it.
        let base = tempfile::tempdir().unwrap();
        artifact(base.path(), "2509.21631", "arxiv: \"2506.21812\"\n", Some(MAIN_TEX));
        assert!(matches!(read_in(base.path(), "2509.21631"), Err(ReadError::NotFound(_))));
        assert_eq!(
            plan_in(base.path(), "2509.21631", false).unwrap(),
            Plan::Fetch(Target::Arxiv("2509.21631".into()))
        );
    }

    #[test]
    fn read_finds_the_directory_naming_the_id_among_colliding_slugs() {
        let base = tempfile::tempdir().unwrap();
        artifact(base.path(), "palikhe2025towards", "arxiv: \"2506.21812\"\n", Some(MAIN_TEX));
        let second = artifact(base.path(), "palikhe2025towardsb", "arxiv: \"2509.21631\"\n", Some(MAIN_TEX));
        assert_eq!(plan_in(base.path(), "2509.21631", false).unwrap(), Plan::Read(second));
    }

    #[test]
    fn an_id_query_prefers_the_directory_naming_it_over_a_name_match() {
        // A directory named after the id but holding no source.yaml is of
        // unknown provenance; the one whose source.yaml names the id wins.
        let base = tempfile::tempdir().unwrap();
        make_paper_dir(base.path(), "2509.21631", "main.tex", "\\documentclass{article}\n");
        let owner = artifact(base.path(), "palikhe2025towardsb", "arxiv: \"2509.21631\"\n", Some(MAIN_TEX));
        assert_eq!(plan_in(base.path(), "2509.21631", false).unwrap(), Plan::Read(owner));
    }

    #[test]
    fn an_id_query_falls_back_to_a_name_match() {
        let base = tempfile::tempdir().unwrap();
        let dir = make_paper_dir(base.path(), "2509.21631", "main.tex", "\\documentclass{article}\n");
        assert_eq!(find_paper_dir_in(base.path(), "2509.21631").unwrap(), dir);
    }

    #[test]
    fn read_picks_the_first_of_two_directories_naming_the_id() {
        let base = tempfile::tempdir().unwrap();
        artifact(base.path(), "wang2025doesb", "arxiv: \"2510.03659\"\n", Some(MAIN_TEX));
        let first = artifact(base.path(), "wang2025does", "arxiv: \"2510.03659\"\n", Some(MAIN_TEX));
        assert_eq!(find_paper_dir_in(base.path(), "2510.03659").unwrap(), first);
    }

    #[test]
    fn read_never_matches_a_staging_directory() {
        let base = tempfile::tempdir().unwrap();
        make_paper_dir(base.path(), ".lit-staging-wang2025does-4242", "main.tex", "\\documentclass{article}\n");
        let dir = make_paper_dir(base.path(), "wang2025does", "main.tex", "\\documentclass{article}\n");
        assert_eq!(find_paper_dir_in(base.path(), "wang2025").unwrap(), dir);
    }

    #[test]
    fn manifest_toplevel_rejects_a_symlink_leading_outside_the_directory() {
        let base = tempfile::tempdir().unwrap();
        std::fs::write(base.path().join("outside.tex"), "\\documentclass{article}\n").unwrap();
        let dir = make_paper_dir(base.path(), "doe2020paper", "body.tex", "\\documentclass{article}\n");
        std::os::unix::fs::symlink(base.path().join("outside.tex"), dir.join("main.tex")).unwrap();
        std::os::unix::fs::symlink(base.path(), dir.join("up")).unwrap();
        for name in ["main.tex", "up/outside.tex"] {
            let manifest = format!(r#"{{"sources": [{{"usage": "toplevel", "filename": "{}"}}]}}"#, name);
            std::fs::write(dir.join("00README.json"), manifest).unwrap();
            assert_eq!(manifest_toplevel(&dir), None, "manifest naming {}", name);
        }
    }

    #[test]
    fn manifest_toplevel_accepts_a_file_inside_the_directory() {
        let base = tempfile::tempdir().unwrap();
        let dir = make_paper_dir(base.path(), "doe2020paper", "body.tex", "\\documentclass{article}\n");
        std::fs::write(dir.join("00README.json"), r#"{"sources": [{"usage": "toplevel", "filename": "body.tex"}]}"#).unwrap();
        assert_eq!(manifest_toplevel(&dir), Some(dir.join("body.tex")));
    }
}
