//! Cross-process file lock for serializing integration-test runs.
//!
//! Wraps `File::try_lock` (stable since Rust 1.89). The lock is held
//! while the `ItestLock` guard is alive and auto-releases on Drop —
//! including the implicit Drop that runs on Ctrl-C, panic, or OOM,
//! because the kernel closes the file handle when the process exits.
//!
//! On acquire, the holder writes its PID into the file so a contending
//! second invocation can surface a clear error: "another process
//! (pid X) is holding the lock."
//!
//! The lock file is never deleted on Drop — future invocations need it
//! to exist. It lives at `target/.itest.lock` by convention, which is
//! gitignored.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Live lock guard. Dropping it releases the lock.
#[derive(Debug)]
pub struct ItestLock {
    file: File,
    _path: PathBuf,
}

impl ItestLock {
    /// Try to acquire an exclusive lock at `path`. Non-blocking.
    /// Returns `Err(LockError::AlreadyHeld { pid })` if another process
    /// is already holding it (with the holder's PID if readable from
    /// the file, otherwise `None`).
    pub fn acquire(path: &Path) -> Result<Self, LockError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(LockError::Io)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(LockError::Io)?;

        match file.try_lock() {
            Ok(()) => {
                // We hold the lock. Stamp our PID into the file so
                // a contender can see who holds it.
                file.set_len(0).map_err(LockError::Io)?;
                file.seek(SeekFrom::Start(0)).map_err(LockError::Io)?;
                writeln!(&mut file, "{}", std::process::id()).map_err(LockError::Io)?;
                Ok(ItestLock {
                    file,
                    _path: path.to_path_buf(),
                })
            }
            Err(_would_block) => {
                let pid = read_pid_from_file(path).ok().flatten();
                Err(LockError::AlreadyHeld { pid })
            }
        }
    }
}

impl Drop for ItestLock {
    fn drop(&mut self) {
        // Explicit unlock so future acquires within the same process
        // see the release immediately. The kernel would also release
        // when the file handle closes, but being explicit avoids any
        // ambiguity around Drop ordering.
        let _ = self.file.unlock();
    }
}

#[derive(Debug)]
pub enum LockError {
    /// Another process holds the lock. `pid` is the PID parsed from
    /// the lock file's contents, or `None` if the file was unreadable
    /// or its contents weren't a valid PID.
    AlreadyHeld { pid: Option<u32> },
    Io(io::Error),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::AlreadyHeld { pid: Some(p) } => {
                write!(f, "another integration test run is in progress (pid {p})")
            }
            LockError::AlreadyHeld { pid: None } => {
                write!(f, "another integration test run is in progress (pid unknown)")
            }
            LockError::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for LockError {}

fn read_pid_from_file(path: &Path) -> io::Result<Option<u32>> {
    let mut buf = String::new();
    File::open(path)?.read_to_string(&mut buf)?;
    Ok(buf.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Per-test unique path. Avoids interference when tests run in
    /// parallel; doesn't need cleanup between tests.
    fn fresh_path() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let i = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "itest-harness-test-{}-{i}.lock",
            std::process::id()
        ))
    }

    #[test]
    fn acquire_succeeds_on_fresh_path() {
        let path = fresh_path();
        let _g = ItestLock::acquire(&path).expect("first acquire should succeed");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn second_acquire_fails_while_first_is_held() {
        let path = fresh_path();
        let _first = ItestLock::acquire(&path).expect("first acquire");
        match ItestLock::acquire(&path) {
            Err(LockError::AlreadyHeld { pid }) => {
                // The lock file should record our own pid.
                assert_eq!(pid, Some(std::process::id()));
            }
            Ok(_) => panic!("expected AlreadyHeld, got a successful second acquire"),
            Err(other) => panic!("expected AlreadyHeld, got {other:?}"),
        }
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn reacquire_succeeds_after_first_dropped() {
        let path = fresh_path();
        drop(ItestLock::acquire(&path).expect("first acquire"));
        let _second = ItestLock::acquire(&path).expect("re-acquire after drop");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn error_display_includes_pid_when_available() {
        let e = LockError::AlreadyHeld { pid: Some(12345) };
        let msg = format!("{e}");
        assert!(msg.contains("12345"));
        assert!(msg.contains("integration test"));
    }

    #[test]
    fn error_display_handles_missing_pid() {
        let e = LockError::AlreadyHeld { pid: None };
        let msg = format!("{e}");
        assert!(msg.contains("pid unknown"));
    }
}
