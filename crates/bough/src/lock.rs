//! Invariant (INTERIM, until the daemon): ONE interactive bough per `$BOUGH_HOME`. Two TUIs on
//! one home each open the ledger and the roster as if alone, and neither sees what the other does
//! (Andrey, 2026-08-28: "if I have the new bough open on different tabs they don't share
//! information"). Until the resident process + thin clients arrive, the second launch says so and
//! exits instead of quietly running a private copy.
//!
//! The lock is an advisory `flock` on `$BOUGH_HOME/lock`, held for the life of the process and
//! released by the kernel on exit however the process ends — a kill, a panic, SIGINT — so a stale
//! file never wedges the next launch. The file's text is the owner's pid, for the message.

use std::io::{Read, Seek, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

/// The held lock. Dropping it (or the process ending) releases the home.
pub struct HomeLock {
    _file: std::fs::File,
    /// Where the lock file is, for the teardown log.
    pub path: PathBuf,
}

#[derive(Debug)]
pub enum LockError {
    /// Another process holds the home. `pid` is what that process wrote, if it did.
    Held {
        path: PathBuf,
        pid: Option<u32>,
    },
    Io(std::io::Error),
}

/// Take the home's lock, or say who has it.
pub fn acquire(home: &Path) -> Result<HomeLock, LockError> {
    std::fs::create_dir_all(home).map_err(LockError::Io)?;
    let path = home.join("lock");
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(LockError::Io)?;
    // SAFETY: `flock` on a descriptor this process owns; it neither reads nor writes memory.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            let mut text = String::new();
            let _ = file.read_to_string(&mut text);
            return Err(LockError::Held {
                path,
                pid: text.trim().parse().ok(),
            });
        }
        return Err(LockError::Io(err));
    }
    file.set_len(0).map_err(LockError::Io)?;
    file.rewind().map_err(LockError::Io)?;
    write!(file, "{}", std::process::id()).map_err(LockError::Io)?;
    file.flush().map_err(LockError::Io)?;
    Ok(HomeLock { _file: file, path })
}

/// What the second launch prints before it exits.
pub fn held_message(path: &Path, pid: Option<u32>) -> String {
    let who = match pid {
        Some(pid) => format!("pid {pid}"),
        None => "another process".to_string(),
    };
    format!(
        "bough: this home is already open in another tab ({who}, {}).\n\
         Two tabs on one home would not see each other's work; multi-tab arrives with the daemon.\n\
         Use that tab, quit it, or set BOUGH_HOME to a different home for this one.",
        path.display()
    )
}

impl std::fmt::Debug for HomeLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HomeLock")
            .field("path", &self.path)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_acquire_is_refused_and_names_the_owner_until_the_first_is_dropped() {
        let home = std::env::temp_dir().join(format!("bough-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let first = acquire(&home).expect("first acquire");
        assert_eq!(
            std::fs::read_to_string(&first.path).unwrap(),
            std::process::id().to_string()
        );
        // `flock` locks are per open file description, so a second open in the SAME process
        // conflicts exactly as a second process would.
        match acquire(&home) {
            Err(LockError::Held { pid, .. }) => assert_eq!(pid, Some(std::process::id())),
            other => panic!("expected Held, got {other:?}"),
        }
        drop(first);
        let again = acquire(&home).expect("acquire after release");
        drop(again);
        let msg = held_message(&home.join("lock"), Some(42));
        assert!(msg.contains("pid 42") && msg.contains("multi-tab arrives with the daemon"));
        let _ = std::fs::remove_dir_all(&home);
    }
}
