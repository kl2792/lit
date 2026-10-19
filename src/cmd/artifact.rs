//! Which paper an artifact directory under `etc/pdf` holds, and which
//! directory a download of a given paper may write.
//!
//! A directory belongs to the paper its `source.yaml` names by `arxiv` or
//! `doi`. Downloads and `lit read` both decide by that identity, so a second
//! paper whose generated slug collides with a first one gets its own
//! directory, and a read for one id never returns another paper's directory.

use std::path::{Path, PathBuf};

use super::check::yaml_field;
use super::lock::{self, Acquired, Lock};
use crate::detect::{arxiv_id_from_doi, detect_type, normalize_arxiv, normalize_doi, InputType};

/// A paper named by an arXiv id (normalized) or a DOI. DOIs compare
/// case-insensitively, as the DOI system resolves them.
#[derive(Debug, Clone, Eq)]
pub(crate) enum Target {
    Arxiv(String),
    Doi(String),
}

impl Target {
    /// The id as written: the arXiv id or the DOI.
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Target::Arxiv(id) | Target::Doi(id) => id,
        }
    }
}

impl PartialEq for Target {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Target::Arxiv(a), Target::Arxiv(b)) => a == b,
            (Target::Doi(a), Target::Doi(b)) => a.eq_ignore_ascii_case(b),
            _ => false,
        }
    }
}

/// Classify a download input as an arXiv preprint (fetched from arXiv itself,
/// never through a DOI resolver or EZProxy) or a DOI.
///
/// arXiv-registered DOIs (`10.48550/arXiv.<id>`) are arXiv preprints too.
pub(crate) fn classify(input: &str) -> Target {
    if detect_type(input) == InputType::Arxiv {
        return Target::Arxiv(normalize_arxiv(input));
    }
    let doi = normalize_doi(input);
    match arxiv_id_from_doi(&doi) {
        Some(id) => Target::Arxiv(id),
        None => Target::Doi(doi),
    }
}

/// The paper `input` names when it is an arXiv id or a DOI (bare or as a
/// URL), or `None` for a directory name, citekey or free text.
pub(crate) fn paper_id(input: &str) -> Option<Target> {
    let input = input.trim();
    matches!(detect_type(input), InputType::Arxiv | InputType::Doi).then(|| classify(input))
}

/// The papers `dir/source.yaml` names by its `arxiv` and `doi` fields; empty
/// when the file is missing or names neither.
pub(crate) fn dir_paper_ids(dir: &Path) -> Vec<Target> {
    std::fs::read_to_string(dir.join("source.yaml"))
        .map(|yaml| yaml_paper_ids(&yaml))
        .unwrap_or_default()
}

/// The papers a `source.yaml` record names by its `arxiv` and `doi` fields.
pub(crate) fn yaml_paper_ids(yaml: &str) -> Vec<Target> {
    let arxiv = yaml_field(yaml, "arxiv").map(|id| Target::Arxiv(normalize_arxiv(&id)));
    let doi = yaml_field(yaml, "doi").and_then(|doi| paper_id(&doi));
    arxiv.into_iter().chain(doi).collect()
}

/// Whether `dir` holds anything besides `source.yaml` and dotfiles (such as
/// the `.DS_Store` Finder leaves). A directory that holds only metadata is a
/// stale copy: there is nothing in it to read.
pub(crate) fn holds_content(dir: &Path) -> bool {
    let is_content = |name: &std::ffi::OsStr| name != "source.yaml" && !name.to_string_lossy().starts_with('.');
    std::fs::read_dir(dir)
        .map(|mut entries| entries.any(|e| e.is_ok_and(|e| is_content(&e.file_name()))))
        .unwrap_or(false)
}

/// The directories under `base` whose `source.yaml` names `id`, sorted by
/// name. One pass over `base`, reading each `source.yaml` once.
pub(crate) fn dirs_naming(base: &Path, id: &Target) -> std::io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(base)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && dir_paper_ids(&entry.path()).contains(id) {
            found.push(entry.path());
        }
    }
    found.sort();
    Ok(found)
}

/// The directory under `base` that holds `id`: the first, by name, of those
/// whose `source.yaml` names it. Two such directories are a defect an earlier
/// lit left behind; both `lit read` and downloads pick the same one, and a
/// warning names them all so the duplicate can be merged by hand.
pub(crate) fn owner_dir(base: &Path, id: &Target) -> std::io::Result<Option<PathBuf>> {
    let dirs = dirs_naming(base, id)?;
    if let Some(warning) = duplicate_owner_warning(id, &dirs) {
        crate::format::warn(&warning);
    }
    Ok(dirs.into_iter().next())
}

/// The warning `owner_dir` prints when more than one of `dirs` names `id`.
fn duplicate_owner_warning(id: &Target, dirs: &[PathBuf]) -> Option<String> {
    let (first, rest) = dirs.split_first()?;
    (!rest.is_empty()).then(|| {
        let others: Vec<String> = rest.iter().map(|d| d.display().to_string()).collect();
        format!("warning: {} is named by {} and {}; using the first", id.as_str(), first.display(), others.join(", "))
    })
}

/// Whether a download may write into `dir`: it is absent, or it holds no
/// content and names no paper (`.DS_Store` and other dotfiles do not count).
fn is_free(dir: &Path) -> bool {
    !dir.exists() || (dir.is_dir() && !holds_content(dir) && dir_paper_ids(dir).is_empty())
}

/// A directory a download of one paper writes into, held until dropped.
///
/// Dropping releases the lock, if this claim took it, and removes the
/// directory if nothing was written to it, so a failed first fetch leaves no
/// directory behind. A directory holding anything, such as a stale copy being
/// refetched, is never removed.
#[derive(Debug)]
pub(crate) struct Claim {
    dir: PathBuf,
    lock: Option<Lock>,
}

impl Claim {
    /// Claim `dir` as named by the caller (`--dir`, `--citekey`), creating it.
    pub(crate) fn at(dir: &Path) -> std::io::Result<Claim> {
        std::fs::create_dir_all(dir)?;
        Ok(Claim { dir: dir.to_path_buf(), lock: None })
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        // The lock goes first: the directory is removed only once it is empty.
        drop(self.lock.take());
        let _ = std::fs::remove_dir(&self.dir);
    }
}

/// The names a new artifact for `slug` may take, in order: `slug`, `slug`b,
/// ..., `slug`z, `slug`27, ... (ADR-007 decision 3). Unbounded, so a free one
/// exists.
pub(crate) fn candidate_names(slug: &str) -> impl Iterator<Item = String> + '_ {
    std::iter::once(slug.to_string())
        .chain(('b'..='z').map(move |c| format!("{}{}", slug, c)))
        .chain((27u32..).map(move |n| format!("{}{}", slug, n)))
}

/// Claim the directory under `base` that a download of `id` writes.
///
/// The directory already naming `id` is reused, so a refetch lands on the
/// existing copy. Otherwise the first of `candidate_names(slug)` that
/// `is_free` is claimed by taking its lock (`lock::acquire`), which is
/// atomic: of two downloads racing for one name, only one takes it, and the
/// other moves on unless it fetches the same paper. A lock whose
/// download crashed is stale and is broken rather than skipped. A paper
/// therefore never writes into a directory holding or being written with
/// another paper, or holding content of unknown provenance.
pub(crate) fn claim_dir(base: &Path, slug: &str, id: &Target) -> std::io::Result<Claim> {
    if let Some(own) = owner_dir(base, id)? {
        return Ok(Claim { dir: own, lock: None });
    }
    for dir in candidate_names(slug).map(|name| base.join(name)) {
        match std::fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        if !is_free(&dir) {
            continue;
        }
        lock::sweep(&dir, lock::LOCK_SCRATCH_PREFIX);
        match lock::acquire(&dir, id.as_str())? {
            Acquired::Locked(held) => return Ok(Claim { dir, lock: Some(held) }),
            Acquired::Held(holder) if paper_id(&holder.id).as_ref() == Some(id) => {
                return Ok(Claim { dir, lock: None });
            }
            Acquired::Held(_) => {}
        }
    }
    unreachable!("an unbounded sequence of names has a free one")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Make `base/name` holding `source.yaml` with `yaml` and, if given, one
    /// content file.
    fn artifact(base: &Path, name: &str, yaml: &str, content: Option<&str>) -> PathBuf {
        let dir = base.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("source.yaml"), yaml).unwrap();
        if let Some(file) = content {
            std::fs::write(dir.join(file), "x").unwrap();
        }
        dir
    }

    #[test]
    fn test_classify_routes_every_arxiv_form_to_arxiv() {
        let cases = [
            ("2510.24941", "2510.24941"),
            ("2510.24941v4", "2510.24941"),
            ("arXiv:2510.24941", "2510.24941"),
            ("arxiv:2510.24941v4", "2510.24941"),
            ("hep-th/9901001", "hep-th/9901001"),
            ("https://arxiv.org/abs/2510.24941", "2510.24941"),
            ("10.48550/arXiv.2510.24941", "2510.24941"),
            ("https://doi.org/10.48550/arXiv.2510.24941", "2510.24941"),
            ("10.48550/arXiv.hep-th/9901001", "hep-th/9901001"),
            ("arxiv:hep-th/9901001", "hep-th/9901001"),
            ("https://www.arxiv.org/abs/2510.24941v2", "2510.24941"),
            ("http://export.arxiv.org/abs/2510.24941", "2510.24941"),
            ("doi:10.48550/arXiv.2510.24941", "2510.24941"),
        ];
        for (input, id) in cases {
            assert_eq!(classify(input), Target::Arxiv(id.to_string()), "input {:?}", input);
        }
    }

    #[test]
    fn test_classify_keeps_a_publisher_doi_a_doi() {
        assert_eq!(classify("10.1145/3442188.3445899"), Target::Doi("10.1145/3442188.3445899".to_string()));
        assert_eq!(
            classify("https://doi.org/10.1145/3442188.3445899"),
            Target::Doi("10.1145/3442188.3445899".to_string())
        );
        assert_eq!(classify("DOI:10.1145/3442188.3445899"), Target::Doi("10.1145/3442188.3445899".to_string()));
    }

    #[test]
    fn paper_id_recognizes_arxiv_ids_and_dois_only() {
        assert_eq!(paper_id("2508.02360"), Some(Target::Arxiv("2508.02360".into())));
        assert_eq!(paper_id("arXiv:2508.02360v2"), Some(Target::Arxiv("2508.02360".into())));
        assert_eq!(
            paper_id("10.18653/v1/2025.acl-long.228"),
            Some(Target::Doi("10.18653/v1/2025.acl-long.228".into()))
        );
        assert_eq!(paper_id("10.48550/arXiv.2508.02360"), Some(Target::Arxiv("2508.02360".into())));
        assert_eq!(paper_id("zhang2025understanding"), None);
        assert_eq!(paper_id("nosuchpaper"), None);
    }

    #[test]
    fn dois_compare_case_insensitively() {
        assert_eq!(Target::Doi("10.1/ABC".into()), Target::Doi("10.1/abc".into()));
        assert_ne!(Target::Doi("10.1/abc".into()), Target::Arxiv("10.1/abc".into()));
    }

    #[test]
    fn dir_paper_ids_reads_arxiv_and_doi_fields() {
        let base = tempfile::tempdir().unwrap();
        let both = artifact(base.path(), "a", "title: \"T\"\narxiv: \"2508.02360\"\ndoi: \"10.1234/X\"\n", None);
        assert_eq!(
            dir_paper_ids(&both),
            vec![Target::Arxiv("2508.02360".into()), Target::Doi("10.1234/x".into())]
        );
        let bare = base.path().join("bare");
        std::fs::create_dir(&bare).unwrap();
        assert!(dir_paper_ids(&bare).is_empty());
    }

    #[test]
    fn yaml_paper_ids_maps_an_arxiv_doi_to_its_arxiv_id() {
        assert_eq!(yaml_paper_ids("doi: \"10.48550/arXiv.2502.11447\"\n"), vec![Target::Arxiv("2502.11447".into())]);
        assert!(yaml_paper_ids("title: \"T\"\ndoi: \"not a doi\"\n").is_empty());
    }

    #[test]
    fn holds_content_is_false_for_a_metadata_only_or_empty_directory() {
        let base = tempfile::tempdir().unwrap();
        assert!(!holds_content(&artifact(base.path(), "stale", "arxiv: \"2606.21078\"\n", None)));
        let empty = base.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        assert!(!holds_content(&empty));
        assert!(!holds_content(&artifact(base.path(), "browsed", "arxiv: \"2606.21078\"\n", Some(".DS_Store"))));
        assert!(holds_content(&artifact(base.path(), "full", "arxiv: \"2606.21078\"\n", Some("main.tex"))));
    }

    /// Entry names in `dir`, sorted.
    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> =
            std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn candidate_names_are_the_slug_then_letter_then_number_suffixes() {
        let nth = |n: usize| candidate_names("s").nth(n).unwrap();
        assert_eq!([nth(0), nth(1), nth(25), nth(26)], ["s", "sb", "sz", "s27"]);
    }

    #[test]
    fn claim_dir_uses_the_slug_when_it_is_free() {
        let base = tempfile::tempdir().unwrap();
        let id = Target::Arxiv("2502.11447".into());
        assert_eq!(claim_dir(base.path(), "wang2025does", &id).unwrap().dir(), base.path().join("wang2025does"));
    }

    #[test]
    fn claim_dir_skips_a_slug_owned_by_another_paper() {
        // wang2025does held 2502.11447; 2510.03659 has the same generated slug
        // and was extracted into it. It must get its own directory instead.
        let base = tempfile::tempdir().unwrap();
        artifact(base.path(), "wang2025does", "arxiv: \"2502.11447\"\n", Some("arxiv.tex"));
        let id = Target::Arxiv("2510.03659".into());
        assert_eq!(claim_dir(base.path(), "wang2025does", &id).unwrap().dir(), base.path().join("wang2025doesb"));
        artifact(base.path(), "wang2025doesb", "arxiv: \"2601.00001\"\n", None);
        assert_eq!(claim_dir(base.path(), "wang2025does", &id).unwrap().dir(), base.path().join("wang2025doesc"));
    }

    #[test]
    fn claim_dir_skips_a_slug_holding_content_of_unknown_provenance() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("doe2020paper");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("notes.tex"), "x").unwrap();
        let id = Target::Arxiv("2001.00001".into());
        assert_eq!(claim_dir(base.path(), "doe2020paper", &id).unwrap().dir(), base.path().join("doe2020paperb"));
    }

    #[test]
    fn claim_dir_reuses_the_directory_already_naming_the_paper() {
        // A stale copy under a name other than today's slug is refetched in
        // place, so the id never ends up named by two directories.
        let base = tempfile::tempdir().unwrap();
        let stale = artifact(base.path(), "ahmed2026validation", "arxiv: \"2606.21078\"\n", None);
        let id = Target::Arxiv("2606.21078".into());
        assert_eq!(claim_dir(base.path(), "ahmed2026validationgated", &id).unwrap().dir(), stale);
    }

    #[test]
    fn claim_dir_reuses_an_empty_slug_directory() {
        let base = tempfile::tempdir().unwrap();
        std::fs::create_dir(base.path().join("doe2020paper")).unwrap();
        let id = Target::Doi("10.1/x".into());
        assert_eq!(claim_dir(base.path(), "doe2020paper", &id).unwrap().dir(), base.path().join("doe2020paper"));
    }

    #[test]
    fn claim_dir_treats_a_dotfile_only_directory_as_free() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("doe2020paper");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join(".DS_Store"), "x").unwrap();
        let id = Target::Arxiv("2001.00001".into());
        assert_eq!(claim_dir(base.path(), "doe2020paper", &id).unwrap().dir(), dir);
    }

    #[test]
    fn a_live_claim_turns_away_another_paper_and_admits_the_same_one() {
        // Two fetches of different papers with one slug used to pick the same
        // free directory, since neither had written anything yet.
        let base = tempfile::tempdir().unwrap();
        let first = Target::Arxiv("2502.11447".into());
        let other = Target::Arxiv("2510.03659".into());
        let held = claim_dir(base.path(), "wang2025does", &first).unwrap();
        assert_eq!(held.dir(), base.path().join("wang2025does"));
        assert_eq!(claim_dir(base.path(), "wang2025does", &other).unwrap().dir(), base.path().join("wang2025doesb"));
        assert_eq!(claim_dir(base.path(), "wang2025does", &first).unwrap().dir(), base.path().join("wang2025does"));
    }

    #[test]
    fn dropping_a_claim_removes_a_directory_left_empty_and_keeps_one_with_content() {
        let base = tempfile::tempdir().unwrap();
        let id = Target::Arxiv("2001.00001".into());
        let claim = claim_dir(base.path(), "doe2020paper", &id).unwrap();
        let dir = claim.dir().to_path_buf();
        drop(claim);
        assert!(!dir.exists());

        let claim = claim_dir(base.path(), "doe2020paper", &id).unwrap();
        std::fs::write(dir.join("main.tex"), "x").unwrap();
        drop(claim);
        assert_eq!(entries(&dir), vec!["main.tex"]);
    }

    #[test]
    fn dropping_a_claim_on_a_dotfile_only_directory_restores_it() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("doe2020paper");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join(".DS_Store"), "x").unwrap();
        drop(claim_dir(base.path(), "doe2020paper", &Target::Arxiv("2001.00001".into())).unwrap());
        assert_eq!(entries(&dir), vec![".DS_Store"]);
    }

    #[test]
    fn a_crashed_claim_is_reclaimed_by_the_next_download() {
        // A download that died holding its claim used to block the slug for
        // every other paper forever.
        use super::super::lock::testing::{dead_pid, lock_owner, now, plant_lock};
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("doe2020paper");
        std::mem::forget(claim_dir(base.path(), "doe2020paper", &Target::Arxiv("2001.00001".into())).unwrap());
        // The process that leaked the guard then exits.
        plant_lock(&dir, "2001.00001", dead_pid(), now());
        let next = claim_dir(base.path(), "doe2020paper", &Target::Arxiv("2002.00002".into())).unwrap();
        assert_eq!(next.dir(), dir);
        assert_eq!(lock_owner(&dir).unwrap().id, "2002.00002");
        drop(next);
        assert!(!dir.exists(), "releasing the reclaimed lock must leave nothing behind");
    }

    #[test]
    fn claim_dir_sweeps_stale_lock_scratch_from_the_directory_it_claims() {
        use super::super::lock::{testing::dead_pid, LOCK_DIR};
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("doe2020paper");
        let dead = dead_pid();
        std::fs::create_dir_all(dir.join(format!(".lit-lock.tmp-{}-1-0", dead))).unwrap();
        std::fs::create_dir_all(dir.join(format!(".lit-lock.stale-{}-1-0", dead))).unwrap();
        let claim = claim_dir(base.path(), "doe2020paper", &Target::Arxiv("2001.00001".into())).unwrap();
        assert_eq!(claim.dir(), dir);
        assert_eq!(entries(&dir), vec![LOCK_DIR]);
    }

    #[test]
    fn owner_dir_picks_the_first_of_two_directories_naming_the_id() {
        let base = tempfile::tempdir().unwrap();
        let id = Target::Arxiv("2510.03659".into());
        artifact(base.path(), "wang2025doesb", "arxiv: \"2510.03659\"\n", Some("main.tex"));
        let first = artifact(base.path(), "wang2025does", "arxiv: \"2510.03659\"\n", None);
        assert_eq!(owner_dir(base.path(), &id).unwrap(), Some(first.clone()));
        assert_eq!(claim_dir(base.path(), "unrelated", &id).unwrap().dir(), first);
        assert_eq!(owner_dir(base.path(), &Target::Arxiv("2001.00001".into())).unwrap(), None);
    }

    #[test]
    fn duplicate_owner_warning_names_the_id_and_every_directory() {
        let id = Target::Arxiv("2510.03659".into());
        let dirs = [PathBuf::from("/pdf/wang2025does"), PathBuf::from("/pdf/wang2025doesb")];
        let warning = duplicate_owner_warning(&id, &dirs).unwrap();
        for part in ["2510.03659", "/pdf/wang2025does", "/pdf/wang2025doesb"] {
            assert!(warning.contains(part), "{:?} lacks {:?}", warning, part);
        }
        assert_eq!(duplicate_owner_warning(&id, &dirs[..1]), None);
    }

    #[test]
    fn dirs_naming_matches_identity_rather_than_substrings() {
        let base = tempfile::tempdir().unwrap();
        artifact(base.path(), "a", "arxiv: \"2508.02360\"\n", None);
        artifact(base.path(), "b", "arxiv: \"2508.0236\"\nsource_url: \"https://arxiv.org/e-print/2508.0236\"\n", None);
        let found = dirs_naming(base.path(), &Target::Arxiv("2508.0236".into())).unwrap();
        assert_eq!(found, vec![base.path().join("b")]);
    }
}
