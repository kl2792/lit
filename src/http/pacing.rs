//! Request pacing shared across processes through one file (ADR-012).
//!
//! The file holds the next free request slot, in milliseconds since the Unix
//! epoch. A process takes the exclusive `flock`, claims the later of that slot
//! and the current time, writes back the claimed slot plus the interval, and
//! releases the lock before waiting, so no process waits while holding it.

use std::io::{self, Read, Seek, Write};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A stored slot further ahead than this is stale (a clock jump or a damaged
/// file) and is replaced by the current time.
const MAX_AHEAD_MS: u64 = 10 * 60 * 1000;

/// Claim a slot given the stored next free slot `stored` and the time `now`:
/// returns the wait until the claimed slot and the next free slot to store.
pub(crate) fn claim(stored: Option<u64>, now: u64, interval_ms: u64) -> (u64, u64) {
    let slot = match stored {
        Some(s) if s > now && s - now <= MAX_AHEAD_MS => s,
        _ => now,
    };
    (slot - now, slot + interval_ms)
}

/// Reserve the next request slot in `path`, at least `interval` after the
/// previous reservation by any process, and return the wait until it.
/// Creates the file when missing; unparsable contents count as no slot.
/// The lock is released when the file closes, before the caller waits.
pub(crate) fn reserve(path: &Path, interval: Duration) -> io::Result<Duration> {
    let mut file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
    // SAFETY: `flock` on a descriptor this function owns; no memory is shared.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    let (wait, next) = claim(text.trim().parse().ok(), now_ms(), interval.as_millis() as u64);
    file.set_len(0)?;
    file.seek(io::SeekFrom::Start(0))?;
    file.write_all(next.to_string().as_bytes())?;
    Ok(Duration::from_millis(wait))
}

/// The current time in milliseconds since the Unix epoch.
fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    const CHILD_FILE_VAR: &str = "LIT_PACING_TEST_FILE";
    const CHILD_INTERVAL: Duration = Duration::from_millis(200);
    const CHILD_REQUESTS: usize = 3;

    #[test]
    fn first_claim_waits_nothing_and_reserves_one_interval_ahead() {
        assert_eq!(claim(None, 10_000, 1100), (0, 11_100));
    }

    #[test]
    fn claim_after_the_stored_slot_passed_waits_nothing() {
        assert_eq!(claim(Some(9_000), 10_000, 1100), (0, 11_100));
    }

    #[test]
    fn claim_before_the_stored_slot_waits_for_it() {
        assert_eq!(claim(Some(10_800), 10_000, 1100), (800, 11_900));
    }

    #[test]
    fn a_slot_more_than_ten_minutes_ahead_is_stale() {
        assert_eq!(claim(Some(10_000 + MAX_AHEAD_MS + 1), 10_000, 1100), (0, 11_100));
        assert_eq!(claim(Some(10_000 + MAX_AHEAD_MS), 10_000, 1100), (MAX_AHEAD_MS, 10_000 + MAX_AHEAD_MS + 1100));
    }

    #[test]
    fn reserve_creates_the_file_and_spaces_successive_reservations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pacing");
        let interval = Duration::from_millis(500);
        assert_eq!(reserve(&path, interval).unwrap(), Duration::ZERO);
        let second = reserve(&path, interval).unwrap();
        assert!(second > Duration::from_millis(400) && second <= interval, "{:?}", second);
    }

    #[test]
    fn reserve_replaces_a_damaged_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pacing");
        std::fs::write(&path, "not a number").unwrap();
        assert_eq!(reserve(&path, Duration::from_millis(500)).unwrap(), Duration::ZERO);
    }

    /// Two processes reserving through one file get slots at least one
    /// interval apart, interleaved, rather than each pacing only itself.
    #[test]
    fn two_processes_share_one_pace_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pacing");
        let exe = std::env::current_exe().unwrap();
        let children: Vec<_> = (0..2)
            .map(|_| {
                Command::new(&exe)
                    .args(["--exact", "http::pacing::tests::pacing_child", "--ignored", "--nocapture", "--test-threads=1"])
                    .env(CHILD_FILE_VAR, &path)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap()
            })
            .collect();
        let mut times: Vec<(u64, usize)> = Vec::new();
        for (i, child) in children.into_iter().enumerate() {
            let out = child.wait_with_output().unwrap();
            assert!(out.status.success());
            let text = String::from_utf8(out.stdout).unwrap();
            // libtest prints "test ... " before the child's first line, on the same line.
            let got: Vec<u64> = text
                .lines()
                .filter_map(|l| l.split_once("granted ").map(|(_, t)| t.trim().parse().unwrap()))
                .collect();
            assert_eq!(got.len(), CHILD_REQUESTS, "child {} printed: {}", i, text);
            times.extend(got.into_iter().map(|t| (t, i)));
        }
        times.sort();
        // A request runs at or after its slot, and slots are one interval apart,
        // so a gap shorter than the interval is only scheduling delay on the
        // earlier request; 50ms bounds it.
        let floor = CHILD_INTERVAL.as_millis() as u64 - 50;
        for pair in times.windows(2) {
            assert!(pair[1].0 - pair[0].0 >= floor, "requests too close: {:?}", times);
        }
        let switches = times.windows(2).filter(|p| p[0].1 != p[1].1).count();
        assert!(switches >= 1, "both processes must contribute slots: {:?}", times);
    }

    /// Child half of `two_processes_share_one_pace_through_the_file`; does
    /// nothing unless the parent names the pacing file.
    #[test]
    #[ignore]
    fn pacing_child() {
        let Some(path) = std::env::var_os(CHILD_FILE_VAR) else { return };
        for _ in 0..CHILD_REQUESTS {
            let wait = reserve(Path::new(&path), CHILD_INTERVAL).unwrap();
            std::thread::sleep(wait);
            println!("granted {}", now_ms());
        }
    }
}
