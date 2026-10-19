//! Every source file must be reachable from the crate root.
//!
//! `cargo` cannot warn about a file it never reads. An undeclared module is
//! invisible to the compiler, so it is never type-checked, its tests never run,
//! and it can drift arbitrarily far from the code around it while still reading
//! as authoritative to anyone who opens it. Three such files existed at once in
//! this crate: `src/config.rs`, which could not have compiled if declared
//! because it used a dependency the crate does not have, while `docs/DESIGN.md`
//! asserted MUST-level invariants about it; `src/api/s2_dump.rs`, which had the
//! same problem with a different dependency; and the declared-but-uncalled
//! `clear_clio_db`, whose inline duplicate at the call site is the one that
//! actually ran. Both `config.rs` and `s2_dump.rs` have since been deleted.
//!
//! This test makes that condition visible. A file that is deliberately not yet
//! wired in goes in `INTENTIONALLY_UNREACHABLE` with a reason, which turns an
//! invisible accident into a recorded decision.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Files that exist on purpose without being declared, each with the reason.
/// Adding a name here is a decision; leaving a file out of the crate silently
/// is not.
const INTENTIONALLY_UNREACHABLE: &[(&str, &str)] = &[];

fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).expect("src/ is readable");
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            source_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn every_source_file_is_declared_in_the_module_tree() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    source_files(&src, &mut files);
    assert!(!files.is_empty(), "found no sources under {}", src.display());

    // Collect every module name the crate declares, from every file at once.
    // Which file declares which does not matter here; a stem that appears in no
    // declaration anywhere is unreachable no matter how the tree is arranged.
    let mut declared: HashSet<String> = HashSet::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("source file is readable");
        for line in text.lines() {
            let line = line.trim();
            let rest = line
                .strip_prefix("pub mod ")
                .or_else(|| line.strip_prefix("mod "))
                .or_else(|| line.strip_prefix("pub(crate) mod "));
            if let Some(rest) = rest
                && let Some(name) = rest.split(&[';', ' ', '{'][..]).next()
                && !name.is_empty()
            {
                declared.insert(name.to_string());
            }
        }
    }

    let allowed: HashSet<&str> = INTENTIONALLY_UNREACHABLE.iter().map(|(n, _)| *n).collect();
    let mut unreachable = Vec::new();

    for file in &files {
        let stem = file.file_stem().unwrap().to_string_lossy().to_string();
        // Crate roots and `mod.rs` are reachable by construction, and anything
        // under `src/bin/` is its own binary target rather than a module.
        if stem == "lib" || stem == "main" || stem == "mod" {
            continue;
        }
        if file.parent().is_some_and(|p| p.ends_with("bin")) {
            continue;
        }
        if declared.contains(&stem) || allowed.contains(stem.as_str()) {
            continue;
        }
        unreachable.push(
            file.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                .unwrap_or(file)
                .display()
                .to_string(),
        );
    }

    unreachable.sort();
    assert!(
        unreachable.is_empty(),
        "these files are in src/ but declared by no `mod` statement, so the compiler \
         never reads them:\n  {}\nDeclare them, delete them, or record the reason in \
         INTENTIONALLY_UNREACHABLE in this file.",
        unreachable.join("\n  ")
    );
}

#[test]
fn intentionally_unreachable_entries_still_exist() {
    // An allowlist that outlives its files starts granting exemptions to
    // nothing, and the next file to take one of these names inherits it.
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    source_files(&src, &mut files);
    let stems: HashSet<String> = files
        .iter()
        .map(|f| f.file_stem().unwrap().to_string_lossy().to_string())
        .collect();

    for (name, reason) in INTENTIONALLY_UNREACHABLE {
        assert!(
            stems.contains(*name),
            "INTENTIONALLY_UNREACHABLE lists '{}' ({}) but no such file exists under src/; \
             remove the entry",
            name,
            reason
        );
    }
}
