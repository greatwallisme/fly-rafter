//! Cross-process write-ownership lock for one session's artifact directory,
//! held for the whole life of a write handle (dsh lease.ts, POSIX peer).
//!
//! The arbiter is the kernel: a non-blocking `flock(2)` on `session.lock`
//! beside the log. Contention maps to
//! [`PersistError::AlreadyOwned`]; the kernel releases the lock when the
//! holder's descriptor closes or the process dies, so a crashed holder
//! never blocks a successor. A live but wedged holder keeps the lock until
//! its process exits: there is deliberately no expiry that could
//! expropriate a stalled writer whose resumed appends would tear the log.
//!
//! A POSIX lock names an inode, not a path, so after locking, the holder
//! verifies the locked inode is still the file at the lock path and retries
//! otherwise: an unlinked-and-recreated lock file carries a fresh inode,
//! and a lock on the orphaned one proves nothing. Removing a live session's
//! lock file would forfeit exclusion on POSIX, so the release never removes
//! the lock file: every acquired lock belongs to a materialized or
//! materializing session, and the surviving file keeps the stable inode
//! later lockers verify against. Readers never touch the lock.

use std::fs::File;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use super::PersistError;

/// Base name of the kernel lock file inside a session's directory.
pub const LEASE_FILENAME: &str = "session.lock";

/// Whether an `flock` failure means another descriptor holds the lock.
fn is_lock_contention(error: &std::io::Error) -> bool {
    // flock(2) reports EAGAIN; some libcs spell it EWOULDBLOCK (the same
    // value on every libc shipped today, so the or-pattern is total).
    matches!(error.raw_os_error(), Some(libc::EAGAIN))
}

/// One held write lock. Constructed only by [`SessionWriteLease::acquire`];
/// [`SessionWriteLease::release`] closes the descriptor, which is what
/// releases the lock.
#[derive(Debug)]
pub struct SessionWriteLease {
    handle: Option<File>,
    released: bool,
}

impl SessionWriteLease {
    /// Acquires the session directory's kernel write lock; the directory is
    /// created (owner-only) when absent, agreeing with the materialize
    /// creator on the mode.
    ///
    /// # Errors
    ///
    /// Returns [`PersistError::AlreadyOwned`] while another holder keeps
    /// the lock, and `Io` for every other filesystem failure.
    pub fn acquire(dir: &Path, id: &fr_wire::ids::SessionId) -> Result<Self, PersistError> {
        let path = dir.join(LEASE_FILENAME);
        // Owner-only like materialize_posix's directories: the lock may
        // create the session directory first, and both creators must agree
        // on the mode.
        super::io::create_dir_all_mode(dir, 0o700)?;
        // Bounded retry: locking an inode a releasing creator just unlinked
        // (or a recreated path) re-opens the fresh file; steady state needs
        // one pass.
        for _attempt in 0..3 {
            let handle = open_lock_file(&path)?;
            match flock_exclusive(&handle) {
                Ok(()) => {}
                Err(error) => {
                    if is_lock_contention(&error) {
                        return Err(PersistError::AlreadyOwned(format!(
                            "session \"{}\" is already owned by another writer",
                            id.as_str()
                        )));
                    }
                    return Err(PersistError::Io(error));
                }
            }
            let held = handle.metadata().map_err(PersistError::Io)?;
            let current = std::fs::metadata(&path).ok();
            if let Some(current) = current
                && current.ino() == held.ino()
                && current.dev() == held.dev()
            {
                return Ok(Self {
                    handle: Some(handle),
                    released: false,
                });
            }
            // The locked inode is no longer the file at the lock path:
            // start over against whatever now stands there.
            drop(handle);
        }
        Err(PersistError::AlreadyOwned(format!(
            "session \"{}\" write lock could not be verified against its lock file",
            id.as_str()
        )))
    }

    /// Releases the kernel lock by closing its descriptor. The lock file is
    /// never removed: keeping it preserves the stable inode later lockers
    /// verify against. Idempotent.
    ///
    /// # Errors
    ///
    /// Returns `Io` when closing the descriptor fails.
    pub fn release(&mut self) -> Result<(), PersistError> {
        if self.released {
            return Ok(());
        }
        self.released = true;
        // Closing the descriptor releases the kernel lock; the lock file
        // stays on disk (see the module notes).
        drop(self.handle.take());
        Ok(())
    }
}

impl Drop for SessionWriteLease {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

/// Opens the lock file with the dsh `open(path, 'w')` semantics: create or
/// truncate; the file's bytes are never interpreted.
fn open_lock_file(path: &Path) -> Result<File, std::io::Error> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o666)
        .open(path)
}

fn flock_exclusive(handle: &File) -> Result<(), std::io::Error> {
    // SAFETY: `flock` operates on the descriptor `File` owns; the call has
    // no memory-safety preconditions beyond a valid descriptor.
    let code = unsafe {
        libc::flock(
            std::os::unix::io::AsRawFd::as_raw_fd(handle),
            libc::LOCK_EX | libc::LOCK_NB,
        )
    };
    if code == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{LEASE_FILENAME, SessionWriteLease};
    use crate::persist::PersistError;
    use fr_wire::ids::SessionId;

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("fr-session-lease-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("temp dir");
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn contention_maps_to_already_owned() {
        let dir = TempDir::new("contention");
        let id = SessionId::new("s-1");
        let mut holder = SessionWriteLease::acquire(&dir.0, &id).expect("first lock");
        let error = SessionWriteLease::acquire(&dir.0, &id).expect_err("second lock");
        assert!(matches!(error, PersistError::AlreadyOwned(_)));
        holder.release().expect("release");
        let second = SessionWriteLease::acquire(&dir.0, &id);
        assert!(second.is_ok(), "the lock frees after release");
    }

    #[test]
    fn release_never_removes_the_lock_file() {
        let dir = TempDir::new("keep-file");
        let id = SessionId::new("s-2");
        let lock_path = dir.0.join(LEASE_FILENAME);
        let mut lease = SessionWriteLease::acquire(&dir.0, &id).expect("lock");
        lease.release().expect("release");
        assert!(lock_path.is_file(), "the lock file survives release");
    }
}
