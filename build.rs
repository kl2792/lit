//! Embed the git short hash as `LIT_GIT_HASH` for `lit --version`.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let hash = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=LIT_GIT_HASH={}", hash);

    // Rebuild when HEAD moves: HEAD itself (branch switch) and the ref it names
    // (new commit). `--git-path` resolves both inside a submodule's git dir.
    for path in [git(&["rev-parse", "--git-path", "HEAD"]), git(&["symbolic-ref", "-q", "HEAD"])
        .and_then(|r| git(&["rev-parse", "--git-path", &r]))]
    .into_iter()
    .flatten()
    {
        println!("cargo:rerun-if-changed={}", path);
    }
    println!("cargo:rerun-if-changed=build.rs");
}
