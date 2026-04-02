/// `lit read <id>` -- Get path to readable text of a downloaded paper.
///
/// Searches `etc/pdf/` for a matching directory, ensures text is available
/// (runs pdftotext if needed), and returns the file path.
/// The caller (Claude) can then use Read tool with offset/limit.

use std::path::{Path, PathBuf};

use super::Context;

/// Find the paper directory in etc/pdf/ matching the given query.
///
/// Tries exact match first, then substring match on directory names,
/// then checks source.yaml for arxiv ID matches.
fn find_paper_dir(query: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let base = find_pdf_base()?;

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
    let mut matches = Vec::new();
    for entry in std::fs::read_dir(&base)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if name.contains(&normalized) || normalized.contains(&name) {
            matches.push(entry.path());
        }
    }

    // Check source.yaml for arxiv ID
    if matches.is_empty() {
        for entry in std::fs::read_dir(&base)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
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
        0 => Err(format!("no paper directory found matching '{}'. Use `download` first.", query).into()),
        1 => Ok(matches.into_iter().next().unwrap()),
        _ => {
            let names: Vec<String> = matches
                .iter()
                .map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned())
                .collect();
            Err(format!("ambiguous: {}", names.join(", ")).into())
        }
    }
}

fn find_pdf_base() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(cwd) = std::env::current_dir() {
        let mut dir = cwd.as_path();
        loop {
            let candidate = dir.join("etc/pdf");
            if candidate.is_dir() {
                return Ok(candidate);
            }
            match dir.parent() {
                Some(p) => dir = p,
                None => break,
            }
        }
    }
    Err("etc/pdf/ directory not found".into())
}

/// Ensure readable text exists and return the path.
///
/// Priority: .tex main file > paper.txt > pdftotext paper.pdf (cached as paper.txt)
fn ensure_text(dir: &Path) -> Result<ReadResult, Box<dyn std::error::Error>> {
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

    // Find file with \documentclass
    tex_files
        .iter()
        .find(|p| {
            std::fs::read_to_string(p)
                .map(|s| s.contains("\\documentclass"))
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

pub struct ReadResult {
    pub path: PathBuf,
    pub format: String,
    pub extra_files: Vec<String>,
}

/// Run the read command: find paper, ensure text, return path.
pub fn run_data(
    _ctx: &Context,
    query: &str,
) -> Result<ReadResult, Box<dyn std::error::Error>> {
    let dir = find_paper_dir(query)?;
    ensure_text(&dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_pdf_base_exists() {
        // This test only works when run from within the ice repo
        if let Ok(base) = find_pdf_base() {
            assert!(base.ends_with("etc/pdf"));
        }
    }
}
