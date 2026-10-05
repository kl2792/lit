/// `lit read <id>` -- Get path to readable text of a downloaded paper.
///
/// Searches `etc/pdf/` for a matching directory, ensures text is available
/// (runs pdftotext if needed), and returns the file path.
/// The caller (Claude) can then use Read tool with offset/limit.

use std::path::{Path, PathBuf};

use super::Context;

/// Why `run_data` failed, so callers can distinguish "no local source at all"
/// (auto-download may help) from "source directory exists but is unreadable"
/// (auto-download will not help; surface the real cause).
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
                "no paper directory found matching '{}'. Use `download` first.",
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
/// Tries exact match first, then substring match on directory names,
/// then checks source.yaml for arxiv ID matches.
fn find_paper_dir_in(base: &Path, query: &str) -> Result<PathBuf, ReadError> {
    let normalized = query.trim().to_lowercase().replace('/', "_");

    // Exact match
    let exact = base.join(&normalized);
    if exact.is_dir() {
        return Ok(exact);
    }

    // Without dots (arxiv IDs)
    let dotted = normalized.replace('.', "");
    let exact2 = base.join(&dotted);
    if exact2.is_dir() {
        return Ok(exact2);
    }

    // Substring match
    let io_err = |e: std::io::Error| ReadError::Other(e.to_string());
    let mut matches = Vec::new();
    for entry in std::fs::read_dir(base).map_err(io_err)? {
        let entry = entry.map_err(io_err)?;
        if !entry.file_type().map_err(io_err)?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if name.contains(&normalized) || normalized.contains(&name) {
            matches.push(entry.path());
        }
    }

    // Check source.yaml for arxiv ID
    if matches.is_empty() {
        for entry in std::fs::read_dir(base).map_err(io_err)? {
            let entry = entry.map_err(io_err)?;
            if !entry.file_type().map_err(io_err)?.is_dir() {
                continue;
            }
            let yaml_path = entry.path().join("source.yaml");
            if yaml_path.exists() {
                if let Ok(content) = std::fs::read_to_string(&yaml_path) {
                    if content.to_lowercase().contains(&normalized) {
                        matches.push(entry.path());
                    }
                }
            }
        }
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
        let tex_files = list_tex_files(dir);
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

/// Find the main .tex file in a directory.
fn find_main_tex(dir: &Path) -> Option<PathBuf> {
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

/// Find the paper under `base` and ensure readable text, with typed errors.
fn read_in(base: &Path, query: &str) -> Result<ReadResult, ReadError> {
    let dir = find_paper_dir_in(base, query)?;
    ensure_text(&dir).map_err(|e| ReadError::Unreadable {
        dir,
        message: e.to_string(),
    })
}

/// Run the read command: find paper, ensure text, return path.
pub fn run_data(_ctx: &Context, query: &str) -> Result<ReadResult, ReadError> {
    let base = find_pdf_base().map_err(|e| ReadError::Other(e.to_string()))?;
    read_in(&base, query)
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
}
