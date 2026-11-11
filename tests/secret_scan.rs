//! No git-tracked file may contain a credential-shaped string.
//!
//! Patterns come from `lit::secrets::PATTERNS`, the list that also redacts
//! publisher captures. A known false positive goes in
//! `test/secret-scan-allowlist.txt` as one `path<TAB>PATTERN_NAME` line with a
//! `#` comment giving the reason; an entry that no longer matches fails the test,
//! so the list cannot outlive its reason.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

const ALLOWLIST: &str = "test/secret-scan-allowlist.txt";

fn tracked_files(root: &Path) -> Vec<String> {
    let output = Command::new("git").arg("-C").arg(root).args(["ls-files", "-z"]).output()
        .expect("git is installed");
    assert!(output.status.success(), "git ls-files failed in {}: {}", root.display(), String::from_utf8_lossy(&output.stderr));
    output.stdout.split(|b| *b == 0).filter(|p| !p.is_empty())
        .map(|p| String::from_utf8(p.to_vec()).expect("tracked paths are UTF-8")).collect()
}

fn allowlist(root: &Path) -> BTreeSet<(String, String)> {
    let text = std::fs::read_to_string(root.join(ALLOWLIST)).unwrap_or_else(|e| panic!("read {ALLOWLIST}: {e}"));
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(|line| {
        let (path, name) = line.split_once('\t').unwrap_or_else(|| panic!("{ALLOWLIST}: expected `path<TAB>PATTERN_NAME`, got {line:?}"));
        assert!(lit::secrets::PATTERNS.iter().any(|(n, _)| *n == name), "{ALLOWLIST}: unknown pattern name {name:?}");
        (path.to_string(), name.to_string())
    }).collect()
}

#[test]
fn tracked_files_contain_no_credential_shaped_strings() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let allowed = allowlist(root);
    let mut used = BTreeSet::new();
    let mut hits = Vec::new();
    for path in tracked_files(root) {
        let bytes = match std::fs::read(root.join(&path)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue, // deleted, not yet committed
            Err(e) => panic!("read tracked file {path}: {e}"),
        };
        for (name, range) in lit::secrets::find(&bytes) {
            let key = (path.clone(), name.to_string());
            if allowed.contains(&key) { used.insert(key); continue; }
            let line = 1 + bytes[..range.start].iter().filter(|b| **b == b'\n').count();
            let prefix = String::from_utf8_lossy(&bytes[range.start..range.start + 4]);
            hits.push(format!("{path}:{line}: {name} {prefix}<MASKED>"));
        }
    }
    let stale: Vec<_> = allowed.difference(&used).collect();
    assert!(hits.is_empty(), "credential-shaped strings in tracked files (redact them, or allowlist a known false positive in {ALLOWLIST}):\n{}", hits.join("\n"));
    assert!(stale.is_empty(), "{ALLOWLIST} entries that match nothing; remove them: {stale:?}");
}
