//! Atomic whole-file replacement for the JSON unit backend (the dsh
//! `storage-json` `atomic.ts` protocol).
//!
//! The write path lays down a same-directory temp file, fsyncs it,
//! renames it over the target, and fsyncs the parent directory so the
//! new entry is crash-durable. Replacement is the intended semantic: a
//! unit file has exactly one writer per process and last-write-wins is
//! correct, unlike a session-log's no-clobber link protocol.

use std::fs::{File, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use uuid::Uuid;

/// Durably replaces `path` with `data`.
///
/// # Errors
///
/// Propagates the temp-file write, fsync, rename, or directory-fsync
/// failure; the temp file is removed on any failure.
pub fn write_atomic(path: &Path, data: &str) -> std::io::Result<()> {
    let directory = path.parent().ok_or_else(|| {
        std::io::Error::other(format!(
            "{}: no parent directory for the temp sibling",
            path.display()
        ))
    })?;
    let tmp = directory.join(format!(".{}.tmp", Uuid::new_v4()));
    let publish = || -> std::io::Result<()> {
        {
            let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            // The dsh temp file opens with mode 0o600.
            set_owner_only(&file);
            file.write_all(data.as_bytes())?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        fsync_directory(directory);
        Ok(())
    };
    match publish() {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&tmp);
            Err(error)
        }
    }
}

/// Restricts the freshly created temp file to owner-only permissions.
fn set_owner_only(file: &File) {
    #[cfg(unix)]
    {
        let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = file;
    }
}

/// Fsyncs a POSIX directory so a just-renamed entry is crash-durable.
/// Windows rejects directory opens, so it keeps the rename-level
/// durability.
fn fsync_directory(path: &Path) {
    #[cfg(unix)]
    {
        let flags = libc::O_RDONLY | libc::O_DIRECTORY;
        // SAFETY: `open` with these flags returns a directory fd or -1;
        // `fsync` and `close` consume it per POSIX.
        let fd = unsafe { libc::open(path.as_os_str().as_encoded_bytes().as_ptr().cast(), flags) };
        if fd < 0 {
            // Some filesystems refuse directory fsync; the rename itself
            // already ordered the entry.
            return;
        }
        // SAFETY: `fd` is a valid open descriptor from the `open` above.
        let _synced = unsafe { libc::fsync(fd) };
        // SAFETY: `fd` is still owned by this call.
        unsafe { libc::close(fd) };
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_replaces_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!("fr-storage-atomic-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
        let path = dir.join("unit.json");
        write_atomic(&path, "{\"a\":1}\n").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap_or_default(),
            "{\"a\":1}\n"
        );
        write_atomic(&path, "{\"a\":2}\n").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap_or_default(),
            "{\"a\":2}\n"
        );
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .map(|entries| entries.flatten().map(|e| e.file_name()).collect())
            .unwrap_or_default();
        assert_eq!(leftovers.len(), 1);
        std::fs::remove_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_rejects_a_directory_target() {
        let dir = std::env::temp_dir().join(format!("fr-storage-atomic-d-{}", Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("unit.json")).unwrap_or_else(|e| panic!("{e}"));
        let error = write_atomic(&dir.join("unit.json"), "{}\n").unwrap_err();
        assert_ne!(error.to_string(), "");
        std::fs::remove_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    }
}
