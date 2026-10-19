//! Crash-safe locks on artifact directories, and the staleness rule for every
//! piece of scratch state a crashed lit can leave under `etc/pdf`.
//!
//! A lock is the directory `<artifact dir>/.lit-lock` holding `owner.json`.
//! It is published by renaming a fully written private directory onto that
//! name, so no process ever observes a lock without its owner. A lock, a
//! lock's scratch directory and a staging directory are stale once the process
//! that made them is gone from this host or they are older than `TTL`; whoever
//! finds a stale one removes it.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The lock directory inside an artifact directory.
pub(crate) const LOCK_DIR: &str = ".lit-lock";

/// Prefix of a lock's scratch directories: `.lit-lock.tmp-<pid>-<nanos>-<seq>`
/// while its owner is being written, `.lit-lock.stale-<pid>-<nanos>-<seq>`
/// while it is being broken.
pub(crate) const LOCK_SCRATCH_PREFIX: &str = ".lit-lock.";

/// Prefix of a staging directory, `.lit-staging-<name>-<pid>`, and of the
/// backup `misc::publish` makes from it, `.lit-staging-<name>-<pid>.old`.
pub(crate) const STAGING_PREFIX: &str = ".lit-staging-";

/// Age after which a lock or scratch directory is stale even when its process
/// still runs or ran on another host.
const TTL: Duration = Duration::from_secs(3600);

/// The retired claim marker, `.lit-claim`, which an earlier lit wrote with
/// `create_new` and could leave empty or orphaned. Any one left is debris.
const LEGACY_CLAIM: &str = ".lit-claim";

const OWNER_FILE: &str = "owner.json";

/// Rounds of break-and-retry before `acquire` gives up. Each round that does
/// not return found a stale lock, so exhausting them means locks keep dying
/// as fast as they are taken.
const MAX_ATTEMPTS: usize = 16;

/// The process holding a lock, as `owner.json` records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Owner {
    /// The paper the holder downloads, as `Target::as_str` writes it.
    pub(crate) id: String,
    pid: u32,
    host: String,
    /// Unix seconds at acquisition.
    started: u64,
}

/// The outcome of `acquire`.
#[derive(Debug)]
pub(crate) enum Acquired {
    Locked(Lock),
    /// A live lock of another download.
    Held(Owner),
}

/// A held lock, released when dropped.
#[derive(Debug)]
pub(crate) struct Lock {
    path: PathBuf,
    owner: Owner,
}

impl Owner {
    fn current(id: &str) -> Owner {
        Owner { id: id.to_string(), pid: std::process::id(), host: this_host().to_string(), started: unix_now() }
    }

    /// Whether the holder has exited, judged only on its own host since a pid
    /// means nothing elsewhere, or has held the lock longer than `TTL`.
    fn is_stale(&self) -> bool {
        (self.host == this_host() && !pid_alive(self.pid)) || unix_now().saturating_sub(self.started) > TTL.as_secs()
    }
}

impl Drop for Lock {
    /// Remove the lock if it is still this one. A lock that outlived `TTL` may
    /// have been broken and retaken, and the new holder's lock must survive.
    fn drop(&mut self) {
        if read_owner(&self.path).as_ref() == Some(&self.owner) {
            let _ = std::fs::remove_file(self.path.join(OWNER_FILE));
            let _ = std::fs::remove_dir(&self.path);
        }
    }
}

/// Lock `dir` for a download of `id`, breaking a stale lock on the way.
///
/// Returns `Held` with the owner of a live lock, whatever paper it names; the
/// caller decides whether that paper is its own.
pub(crate) fn acquire(dir: &Path, id: &str) -> io::Result<Acquired> {
    let path = dir.join(LOCK_DIR);
    let owner = Owner::current(id);
    for _ in 0..MAX_ATTEMPTS {
        if publish(dir, &path, &owner)? {
            return Ok(Acquired::Locked(Lock { path, owner }));
        }
        match read_owner(&path) {
            Some(holder) if !holder.is_stale() => return Ok(Acquired::Held(holder)),
            // Stale, unreadable, or released since `publish` saw it.
            _ => break_lock(dir)?,
        }
    }
    Err(io::Error::other(format!("{} went stale {} times in a row; giving up", path.display(), MAX_ATTEMPTS)))
}

/// Write `owner` into a private scratch directory in `dir` and rename it onto
/// `lock`. Returns `false` when `lock` is already held.
///
/// `rename` replaces an absent or empty directory and fails on a non-empty
/// one, so of several racing processes exactly one publishes, and the lock
/// is never observable without a complete `owner.json`.
fn publish(dir: &Path, lock: &Path, owner: &Owner) -> io::Result<bool> {
    let scratch = dir.join(scratch_name("tmp"));
    // `_all`: a claim that just released `dir` deletes it once empty, and the
    // name is still this caller's to take.
    std::fs::create_dir_all(&scratch)?;
    let result = write_owner(&scratch, owner).and_then(|()| std::fs::rename(&scratch, lock));
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&scratch);
    }
    match result {
        Ok(()) => Ok(true),
        Err(e) if is_held(&e) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Whether a failed `rename` onto the lock name means another lock is there:
/// a non-empty directory (`ENOTEMPTY`, or `EEXIST` on some systems) or a file.
fn is_held(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::AlreadyExists | io::ErrorKind::NotADirectory
    )
}

fn write_owner(dir: &Path, owner: &Owner) -> io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(dir.join(OWNER_FILE))?;
    file.write_all(&serde_json::to_vec(owner)?)?;
    file.sync_all()
}

/// The owner a lock directory records, or `None` when it is gone or garbage.
/// `publish` never exposes a partial `owner.json`, so garbage means damage
/// from outside lit, and callers treat it as stale.
fn read_owner(lock: &Path) -> Option<Owner> {
    serde_json::from_slice(&std::fs::read(lock.join(OWNER_FILE)).ok()?).ok()
}

fn is_live_lock(lock: &Path) -> bool {
    read_owner(lock).is_some_and(|owner| !owner.is_stale())
}

/// Move the lock in `dir` aside and delete it, unless what was moved is live.
///
/// The rename is atomic, so of several processes breaking one stale lock one
/// moves it and the others find it gone and return to retry. A breaker that
/// read the stale owner just before another broke the lock and took it anew
/// moves that live lock instead; it checks what it moved and puts a live lock
/// back.
pub(crate) fn break_lock(dir: &Path) -> io::Result<()> {
    let lock = dir.join(LOCK_DIR);
    let aside = dir.join(scratch_name("stale"));
    match std::fs::rename(&lock, &aside) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    }
    if is_live_lock(&aside) {
        return match std::fs::rename(&aside, &lock) {
            // A third process took the name meanwhile; the moved lock's holder
            // has lost it, which `Lock::drop` tolerates.
            Err(e) if !is_held(&e) => Err(e),
            _ => Ok(()),
        };
    }
    remove_path(&aside)
}

/// A unique name for a lock scratch directory of `kind` in this process.
/// The counter separates two names made within one clock tick.
fn scratch_name(kind: &str) -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}{}-{}-{}-{}", LOCK_SCRATCH_PREFIX, kind, std::process::id(), nanos, seq)
}

/// The process id a scratch directory's name carries:
/// `.lit-lock.<kind>-<pid>-...` or `.lit-staging-<name>-<pid>[.old]`.
fn debris_pid(name: &str) -> Option<u32> {
    if let Some(rest) = name.strip_prefix(LOCK_SCRATCH_PREFIX) {
        return rest.split('-').nth(1)?.parse().ok();
    }
    let rest = name.strip_prefix(STAGING_PREFIX)?;
    rest.strip_suffix(".old").unwrap_or(rest).rsplit('-').next()?.parse().ok()
}

/// Whether `path` is a lock scratch or staging directory whose process has
/// exited or which is older than `TTL`.
///
/// Names carry no host, so a pid is checked on this host: an `etc/pdf` shared
/// between hosts must not run lit on two of them at once.
fn is_stale_scratch(path: &Path) -> bool {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    if !name.starts_with(LOCK_SCRATCH_PREFIX) && !name.starts_with(STAGING_PREFIX) {
        return false;
    }
    debris_pid(&name).is_some_and(|pid| !pid_alive(pid)) || older_than_ttl(path)
}

fn older_than_ttl(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age > TTL)
}

/// Remove the stale scratch directories in `parent` whose names start with
/// `prefix`. Best effort: what a failed sweep leaves, `lit check --fix` finds.
/// One `read_dir` of `parent`, plus one `kill` and `stat` per match.
pub(crate) fn sweep(parent: &Path, prefix: &str) {
    let Ok(entries) = std::fs::read_dir(parent) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with(prefix) && is_stale_scratch(&path) {
            let _ = remove_path(&path);
        }
    }
}

/// Every stale lock, lock scratch directory and staging directory under
/// `pdf_dir`, and every retired `.lit-claim` file, sorted.
///
/// One `read_dir` of `pdf_dir` and of each artifact directory in it; artifact
/// contents below the top level are never visited.
pub(crate) fn find_debris(pdf_dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(pdf_dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') {
            if is_stale_scratch(&path) {
                found.push(path);
            }
            continue;
        }
        if !entry.file_type()?.is_dir() {
            continue;
        }
        for inner in std::fs::read_dir(&path)? {
            let inner = inner?.path();
            let stale = match inner.file_name().unwrap_or_default().to_str() {
                Some(LEGACY_CLAIM) => true,
                Some(LOCK_DIR) => !is_live_lock(&inner),
                _ => is_stale_scratch(&inner),
            };
            if stale {
                found.push(inner);
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Remove one item `find_debris` returned, then the artifact directory that
/// held it if that is left empty, as releasing a claim would.
///
/// A lock is broken through `break_lock`, so one taken anew since
/// `find_debris` ran survives.
pub(crate) fn remove_debris(path: &Path) -> io::Result<()> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let parent = path.parent().unwrap_or(Path::new("."));
    let result = if name == LOCK_DIR { break_lock(parent) } else { remove_path(path) };
    match result {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    // A staging directory's parent is `etc/pdf` itself, which stays.
    if !name.starts_with(STAGING_PREFIX) {
        let _ = std::fs::remove_dir(parent);
    }
    Ok(())
}

/// Remove a directory tree or a file.
fn remove_path(path: &Path) -> io::Result<()> {
    if std::fs::symlink_metadata(path)?.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// Whether process `pid` exists on this host. `kill` with signal 0 sends
/// nothing; it fails with `ESRCH` only when no such process exists (`EPERM`
/// means it exists under another user).
fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    if pid <= 0 {
        // 0 and negative values address process groups, not a process.
        return false;
    }
    // SAFETY: signal 0 performs only the existence and permission checks.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// This host's name, or empty if it cannot be read; read once per process.
fn this_host() -> &'static str {
    static HOST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HOST.get_or_init(|| {
        let mut buf = [0u8; 256];
        // SAFETY: `buf` is writable for `buf.len()` bytes.
        if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
            return String::new();
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..end]).into_owned()
    })
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Test fixtures shared with the modules whose tests plant crash debris.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// The id of a process that has exited.
    pub(crate) fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    /// The current Unix time in seconds.
    pub(crate) fn now() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
    }

    /// Plant a lock in `dir` as a process `pid` on this host would have left it
    /// at `started`.
    pub(crate) fn plant_lock(dir: &Path, id: &str, pid: u32, started: u64) {
        let lock = dir.join(LOCK_DIR);
        std::fs::create_dir_all(&lock).unwrap();
        let owner = Owner { id: id.to_string(), pid, host: this_host().to_string(), started };
        std::fs::write(lock.join("owner.json"), serde_json::to_vec(&owner).unwrap()).unwrap();
    }

    /// The owner recorded in `dir`'s lock.
    pub(crate) fn lock_owner(dir: &Path) -> Option<Owner> {
        read_owner(&dir.join(LOCK_DIR))
    }

    /// Backdate `path`'s modification time past `TTL`.
    pub(crate) fn expire(path: &Path) {
        let old = std::time::SystemTime::now() - TTL - Duration::from_secs(60);
        std::fs::File::open(path).unwrap().set_modified(old).unwrap();
    }

    /// Entry names in `dir`, sorted.
    pub(crate) fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> =
            std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    fn mkdir(path: &Path) -> PathBuf {
        std::fs::create_dir_all(path).unwrap();
        path.to_path_buf()
    }

    fn locked(acquired: Acquired) -> Lock {
        match acquired {
            Acquired::Locked(lock) => lock,
            Acquired::Held(owner) => panic!("held by {:?}", owner),
        }
    }

    #[test]
    fn an_acquired_lock_holds_a_complete_owner_and_no_scratch() {
        let dir = tempfile::tempdir().unwrap();
        let _lock = locked(acquire(dir.path(), "2502.11447").unwrap());
        let owner = lock_owner(dir.path()).expect("owner.json must be readable at once");
        assert_eq!(owner.id, "2502.11447");
        assert_eq!(owner.pid, std::process::id());
        assert_eq!(owner.host, this_host());
        assert!(now().abs_diff(owner.started) <= 1, "started {} vs now {}", owner.started, now());
        assert_eq!(entries(dir.path()), vec![LOCK_DIR]);
    }

    #[test]
    fn acquire_recreates_a_directory_a_releasing_claim_just_removed() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("doe2020paper");
        let _lock = locked(acquire(&dir, "2001.00001").unwrap());
        assert_eq!(entries(&dir), vec![LOCK_DIR]);
    }

    #[test]
    fn a_live_lock_of_another_download_is_held() {
        let dir = tempfile::tempdir().unwrap();
        plant_lock(dir.path(), "2502.11447", std::process::id(), now());
        match acquire(dir.path(), "2510.03659").unwrap() {
            Acquired::Held(owner) => assert_eq!(owner.id, "2502.11447"),
            Acquired::Locked(_) => panic!("a live lock must not be taken"),
        }
        assert_eq!(lock_owner(dir.path()).unwrap().id, "2502.11447");
    }

    #[test]
    fn a_lock_whose_process_died_is_broken_and_reacquired() {
        let dir = tempfile::tempdir().unwrap();
        plant_lock(dir.path(), "2502.11447", dead_pid(), now());
        let _lock = locked(acquire(dir.path(), "2510.03659").unwrap());
        let owner = lock_owner(dir.path()).unwrap();
        assert_eq!((owner.id.as_str(), owner.pid), ("2510.03659", std::process::id()));
        assert_eq!(entries(dir.path()), vec![LOCK_DIR], "the broken lock must be deleted");
    }

    #[test]
    fn a_lock_older_than_the_ttl_is_broken_even_if_its_process_runs() {
        let dir = tempfile::tempdir().unwrap();
        plant_lock(dir.path(), "2502.11447", std::process::id(), now() - TTL.as_secs() - 60);
        let _lock = locked(acquire(dir.path(), "2510.03659").unwrap());
        assert_eq!(lock_owner(dir.path()).unwrap().id, "2510.03659");
    }

    #[test]
    fn a_lock_with_an_unreadable_owner_is_broken() {
        let dir = tempfile::tempdir().unwrap();
        let lock = mkdir(&dir.path().join(LOCK_DIR));
        std::fs::write(lock.join("owner.json"), "").unwrap();
        let _lock = locked(acquire(dir.path(), "2510.03659").unwrap());
        assert_eq!(lock_owner(dir.path()).unwrap().id, "2510.03659");
    }

    #[test]
    fn a_breaker_that_finds_the_lock_already_gone_retries_without_error() {
        // The other breaker renamed the stale lock away first.
        let dir = tempfile::tempdir().unwrap();
        break_lock(dir.path()).unwrap();
        let _lock = locked(acquire(dir.path(), "2510.03659").unwrap());
    }

    #[test]
    fn a_breaker_that_moves_a_live_lock_puts_it_back() {
        // It read a stale owner, but another breaker then broke the lock and
        // reacquired it, so the lock it moves is live.
        let dir = tempfile::tempdir().unwrap();
        plant_lock(dir.path(), "2502.11447", std::process::id(), now());
        break_lock(dir.path()).unwrap();
        assert_eq!(lock_owner(dir.path()).unwrap().id, "2502.11447");
        assert_eq!(entries(dir.path()), vec![LOCK_DIR]);
    }

    #[test]
    fn dropping_a_lock_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        drop(locked(acquire(dir.path(), "2502.11447").unwrap()));
        assert!(entries(dir.path()).is_empty());
    }

    #[test]
    fn dropping_a_lock_another_process_took_over_leaves_theirs() {
        let dir = tempfile::tempdir().unwrap();
        let lock = locked(acquire(dir.path(), "2502.11447").unwrap());
        std::fs::remove_dir_all(dir.path().join(LOCK_DIR)).unwrap();
        plant_lock(dir.path(), "2510.03659", std::process::id(), now());
        drop(lock);
        assert_eq!(lock_owner(dir.path()).unwrap().id, "2510.03659");
    }

    #[test]
    fn sweep_removes_stale_lock_scratch_and_keeps_live_scratch_and_other_files() {
        let dir = tempfile::tempdir().unwrap();
        let (dead, me) = (dead_pid(), std::process::id());
        mkdir(&dir.path().join(format!(".lit-lock.tmp-{}-1-0", dead)));
        mkdir(&dir.path().join(format!(".lit-lock.stale-{}-2-0", dead)));
        mkdir(&dir.path().join(format!(".lit-lock.tmp-{}-3-0", me)));
        expire(&mkdir(&dir.path().join(format!(".lit-lock.stale-{}-4-0", me))));
        std::fs::write(dir.path().join(".DS_Store"), "x").unwrap();
        expire(&dir.path().join(".DS_Store"));
        sweep(dir.path(), LOCK_SCRATCH_PREFIX);
        assert_eq!(entries(dir.path()), vec![".DS_Store".to_string(), format!(".lit-lock.tmp-{}-3-0", me)]);
    }

    #[test]
    fn sweep_removes_stale_staging_of_the_named_artifact_only() {
        let base = tempfile::tempdir().unwrap();
        let (dead, me) = (dead_pid(), std::process::id());
        mkdir(&base.path().join(format!(".lit-staging-foo-{}", dead)));
        mkdir(&base.path().join(format!(".lit-staging-foo-{}.old", dead)));
        mkdir(&base.path().join(format!(".lit-staging-foo-{}", me)));
        mkdir(&base.path().join(format!(".lit-staging-bar-{}", dead)));
        sweep(base.path(), &format!("{}foo-", STAGING_PREFIX));
        assert_eq!(
            entries(base.path()),
            vec![format!(".lit-staging-bar-{}", dead), format!(".lit-staging-foo-{}", me)]
        );
    }

    #[test]
    fn find_debris_lists_stale_items_and_retired_claims_only() {
        let base = tempfile::tempdir().unwrap();
        let (dead, me) = (dead_pid(), std::process::id());
        let crashed = mkdir(&base.path().join("crashed"));
        plant_lock(&crashed, "2502.11447", dead, now());
        let live = mkdir(&base.path().join("live"));
        plant_lock(&live, "2510.03659", me, now());
        mkdir(&live.join(format!(".lit-lock.tmp-{}-1-0", me)));
        let old = mkdir(&base.path().join("old"));
        std::fs::write(old.join(".lit-claim"), "2001.00001").unwrap();
        mkdir(&old.join(format!(".lit-lock.stale-{}-1-0", dead)));
        mkdir(&base.path().join(format!(".lit-staging-foo-{}", dead)));
        mkdir(&base.path().join(format!(".lit-staging-foo-{}", me)));
        assert_eq!(
            find_debris(base.path()).unwrap(),
            vec![
                base.path().join(format!(".lit-staging-foo-{}", dead)),
                crashed.join(LOCK_DIR),
                old.join(".lit-claim"),
                old.join(format!(".lit-lock.stale-{}-1-0", dead)),
            ]
        );
    }

    #[test]
    fn remove_debris_removes_the_item_and_an_artifact_directory_it_leaves_empty() {
        let base = tempfile::tempdir().unwrap();
        let crashed = mkdir(&base.path().join("crashed"));
        plant_lock(&crashed, "2502.11447", dead_pid(), now());
        remove_debris(&crashed.join(LOCK_DIR)).unwrap();
        assert!(!crashed.exists());

        let kept = mkdir(&base.path().join("kept"));
        std::fs::write(kept.join(".lit-claim"), "2001.00001").unwrap();
        std::fs::write(kept.join("main.tex"), "x").unwrap();
        remove_debris(&kept.join(".lit-claim")).unwrap();
        assert_eq!(entries(&kept), vec!["main.tex"]);

        let staging = mkdir(&base.path().join(format!(".lit-staging-kept-{}", dead_pid())));
        remove_debris(&staging).unwrap();
        assert_eq!(entries(base.path()), vec!["kept"]);
    }

    #[test]
    fn remove_debris_leaves_a_lock_that_went_live_since_it_was_found() {
        let base = tempfile::tempdir().unwrap();
        let dir = mkdir(&base.path().join("doe2020paper"));
        plant_lock(&dir, "2001.00001", std::process::id(), now());
        remove_debris(&dir.join(LOCK_DIR)).unwrap();
        assert_eq!(lock_owner(&dir).unwrap().id, "2001.00001");
    }

    #[test]
    fn debris_pid_reads_the_pid_from_each_scratch_name() {
        assert_eq!(debris_pid(".lit-lock.tmp-4242-17-0"), Some(4242));
        assert_eq!(debris_pid(".lit-lock.stale-4242-17-3"), Some(4242));
        assert_eq!(debris_pid(".lit-staging-wang2025does-4242"), Some(4242));
        assert_eq!(debris_pid(".lit-staging-a-b-4242.old"), Some(4242));
        assert_eq!(debris_pid(".lit-staging-foo-x"), None);
        assert_eq!(debris_pid(".DS_Store"), None);
    }

    #[test]
    fn pid_alive_tells_a_running_process_from_an_exited_one() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(dead_pid()));
        assert!(!pid_alive(0));
    }
}
