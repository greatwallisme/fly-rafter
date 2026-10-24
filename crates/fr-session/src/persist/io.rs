//! Materialization, append, and repair file mechanics for the JSONL
//! backend (dsh index.ts "materialization / append / repair" section).
//!
//! Every published byte is crash-durable before the call returns: directory
//! entries fsync level by level, appends fsync with truncate rollback, and
//! publication goes through `link` so concurrent creators can never clobber
//! each other.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use super::{JsonlCompression, PersistError};
use fr_wire::event::{SessionEvent, SessionHeader};

/// `mkdir -p` with the dsh recursive-mode semantics: every directory the
/// call creates gets `mode`; existing entries are left alone.
pub(super) fn create_dir_all_mode(dir: &Path, mode: u32) -> Result<(), PersistError> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut current = PathBuf::from(dir);
    let mut missing: Vec<PathBuf> = Vec::new();
    while !current.exists() {
        missing.push(current.clone());
        if !current.pop() {
            break;
        }
    }
    for component in missing.into_iter().rev() {
        // Two racers can pass the `!current.exists()` probe together; the
        // mkdir loser eats `EEXIST`, which the dsh recursive-mkdir
        // semantics (`mkdir -p`) already tolerate.
        match std::fs::create_dir(&component) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(PersistError::Io(error)),
        }
        std::fs::set_permissions(&component, std::fs::Permissions::from_mode(mode))
            .map_err(PersistError::Io)?;
    }
    Ok(())
}

/// fsyncs a POSIX directory so a just-created entry is crash-durable (dsh
/// `syncDirPosix`).
pub(super) fn sync_dir(dir: &Path) -> Result<(), PersistError> {
    let handle = File::open(dir).map_err(PersistError::Io)?;
    handle.sync_all().map_err(PersistError::Io)
}

/// The 12-hex random suffix of one materialization temp file.
fn random_suffix() -> String {
    // Six random bytes from the OS entropy pool, rendered as the dsh
    // `randomBytes(6).toString("hex")`. Randomness quality only affects the
    // temp name, never correctness: the publish is exclusive either way.
    let mut bytes = [0u8; 6];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut source| std::io::Read::read_exact(&mut source, &mut bytes))
        .is_err()
    {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        bytes.copy_from_slice(&nanos.to_le_bytes()[..6]);
    }
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = std::fmt::Write::write_fmt(&mut hex, format_args!("{byte:02x}"));
    }
    hex
}

/// Writes `content` to a fresh `<finalPath>.<12hex>.tmp` beside the final
/// artifact with `wx` + 0600, fsyncs, and closes (dsh
/// `writeSyncedTempFile`).
///
/// # Errors
///
/// Returns `Io` for creation/write/fsync failures (including `EEXIST` on a
/// temp-name collision).
pub(super) fn write_synced_temp_file(
    final_path: &Path,
    content: &[u8],
) -> Result<PathBuf, PersistError> {
    let tmp = final_path.with_file_name(format!(
        "{}.{suffix}.tmp",
        final_path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or_default(),
        suffix = random_suffix()
    ));
    let handle = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(PersistError::Io)?;
    {
        let mut writer = &handle;
        writer.write_all(content).map_err(PersistError::Io)?;
    }
    handle.sync_all().map_err(PersistError::Io)?;
    Ok(tmp)
}

/// Refuses to publish over an existing committed log: materialize is the
/// first write of a session the backend believes is new (dsh
/// `rejectExistingLog`; the create path guards first, so this is a TOCTOU
/// backstop).
/// The dsh `rejectOppositeArtifact` guard: a session directory already
/// holding the other physical encoding's log refuses materialization before
/// anything is written.
fn reject_opposite_artifact(dir: &Path, compression: JsonlCompression) -> Result<(), PersistError> {
    let opposite = match compression {
        JsonlCompression::Zstd => JsonlCompression::None,
        JsonlCompression::None => JsonlCompression::Zstd,
    };
    if let Some(resolved) = super::read::resolve_generation_in_directory(dir, opposite)? {
        return Err(super::encoding_mismatch(&resolved.source_path, compression));
    }
    Ok(())
}

/// Encodes the header line + first batch without combining their frame
/// boundaries: the header line is its own zstd frame and the event batch is
/// another (dsh `encodeMaterialization`).
///
/// # Errors
///
/// Returns `Io` for compression failures.
pub fn encode_materialization(
    header_line: &[u8],
    events: &[SessionEvent],
    compression: JsonlCompression,
) -> Result<Vec<u8>, PersistError> {
    if events.is_empty() {
        return match compression {
            JsonlCompression::None => Ok(header_line.to_vec()),
            JsonlCompression::Zstd => super::zstd::compress_zstd_frame(header_line),
        };
    }
    let mut body = super::read::event_lines(events).into_bytes();
    body.push(b'\n');
    match compression {
        JsonlCompression::None => {
            let mut combined = header_line.to_vec();
            combined.extend_from_slice(&body);
            Ok(combined)
        }
        JsonlCompression::Zstd => {
            let header_frame = super::zstd::compress_zstd_frame(header_line)?;
            let event_frame = super::zstd::compress_zstd_frame(&body)?;
            let mut combined = header_frame;
            combined.extend_from_slice(&event_frame);
            Ok(combined)
        }
    }
}

/// Encodes one durable append batch in the configured physical
/// representation (dsh `encodeEventBatch`).
///
/// # Errors
///
/// Returns `Io` for compression failures.
pub fn encode_event_batch(
    events: &[SessionEvent],
    compression: JsonlCompression,
) -> Result<Vec<u8>, PersistError> {
    let mut body = super::read::event_lines(events).into_bytes();
    body.push(b'\n');
    match compression {
        JsonlCompression::None => Ok(body),
        JsonlCompression::Zstd => super::zstd::compress_zstd_frame(&body),
    }
}

/// Atomically writes the header line + first batch for one new session:
/// temp-write, fsync, link-publish (dsh `materializePosix`).
///
/// # Errors
///
/// Returns `AlreadyExists` when a log for this id already exists, and
/// `Io`/`Corruption` for the other failures the dsh path reports.
pub fn materialize(
    root: &Path,
    meta: &SessionHeader,
    inherited_event_count: u64,
    events: &[SessionEvent],
    compression: JsonlCompression,
) -> Result<(), PersistError> {
    let project = super::filename::project_dir(root, meta.cwd.as_deref())?;
    let dir = super::filename::session_dir(root, meta.cwd.as_deref(), &meta.id)?;
    let final_path = super::filename::log_path(root, meta.cwd.as_deref(), &meta.id, compression)?;
    create_dir_all_mode(root, 0o700)?;
    sync_dir(root.parent().unwrap_or_else(|| Path::new("/")))?;
    create_dir_all_mode(&project, 0o700)?;
    sync_dir(root)?;
    create_dir_all_mode(&dir, 0o700)?;
    sync_dir(&project)?;
    let header_line =
        super::header_line::to_header_line(meta, meta.is_seeded.then_some(inherited_event_count))?;
    let content =
        encode_materialization(format!("{header_line}\n").as_bytes(), events, compression)?;
    // PRD 19.2 条 16 (deliberate deviation from dsh's outright refusal): an
    // existing log is adopted only when the pending bytes are identical —
    // a republish of the same content is a no-op — and refuses otherwise.
    if let Some(existing) = super::read::resolve_generation_in_directory(&dir, compression)? {
        if existing.source_path == final_path
            && std::fs::read(&existing.source_path)?.as_slice() == content.as_slice()
        {
            return Ok(());
        }
        return Err(PersistError::Corruption(format!(
            "refusing to materialize \"{}\": a divergent log already exists on disk (open it instead)",
            meta.id.as_str()
        )));
    }
    reject_opposite_artifact(&dir, compression)?;
    let tmp = write_synced_temp_file(&final_path, &content)?;
    // Publish via link(), NOT rename(): link fails with EEXIST if the final
    // path already exists, so two processes materializing the same id
    // concurrently cannot clobber each other. rename() would silently
    // overwrite.
    let linked = std::fs::hard_link(&tmp, &final_path);
    if let Err(error) = linked {
        // Remove an unpublished temp on failure.
        let _ = std::fs::remove_file(&tmp);
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            // The race backstop applies the same 19.2 条 16 rule: a
            // concurrent publisher of byte-identical content is adopted.
            if std::fs::read(&final_path).is_ok_and(|bytes| bytes == content) {
                return Ok(());
            }
            return Err(PersistError::AlreadyExists(format!(
                "session \"{}\" already exists",
                meta.id.as_str()
            )));
        }
        return Err(PersistError::Io(error));
    }
    // link() succeeded — the log is published. fsync the directory so the
    // new entry survives a power loss.
    sync_dir(&dir)?;
    // Best-effort temp cleanup: the log is already published and durable,
    // so a failure to remove the redundant temp hard link must not reject
    // the append.
    let _ = std::fs::remove_file(&tmp);
    Ok(())
}

/// Appends and fsyncs event lines. On a partial write or sync failure,
/// restores the previous size before rethrowing because the unchanged
/// cursor will retry the batch;
///
/// leaving partial bytes would create duplicate sequence numbers (dsh
/// `appendLines` + `rollbackAppend`).
///
/// # Errors
///
/// Returns `Io` for every failure; a failed rollback reports the rollback
/// failure text alongside the original (dsh `AggregateError` equivalent).
pub fn append_lines(
    path: &Path,
    events: &[SessionEvent],
    compression: JsonlCompression,
) -> Result<(), PersistError> {
    let content = encode_event_batch(events, compression)?;
    let mut handle = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(PersistError::Io)?;
    let before = handle.metadata().map_err(PersistError::Io)?.len();
    let write_result = (|| -> Result<(), PersistError> {
        handle.write_all(&content).map_err(PersistError::Io)?;
        handle.sync_all().map_err(PersistError::Io)?;
        Ok(())
    })();
    match write_result {
        Ok(()) => Ok(()),
        Err(error) => {
            drop(handle);
            if let Err(rollback_error) = rollback_append(path, before) {
                return Err(PersistError::Corruption(format!(
                    "failed to roll back append to \"{}\": {error}; rollback failure: {rollback_error}",
                    path.display()
                )));
            }
            Err(error)
        }
    }
}

fn rollback_append(path: &Path, size: u64) -> Result<(), PersistError> {
    let handle = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(PersistError::Io)?;
    handle.set_len(size).map_err(PersistError::Io)?;
    handle.sync_all().map_err(PersistError::Io)
}

/// Truncates the log file to `offset` bytes and fsyncs, discarding a crash
/// tail (dsh `repair`).
///
/// # Errors
///
/// Returns `Io` for the truncate or fsync failure.
pub fn repair(path: &Path, offset: u64) -> Result<(), PersistError> {
    let handle = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(PersistError::Io)?;
    handle.set_len(offset).map_err(PersistError::Io)?;
    handle.sync_all().map_err(PersistError::Io)
}

/// The stored stat identity of one artifact file: the dsh
/// `JsonlPhysicalIdentity` fields (dev, ino, size, mtime/ctime in
/// nanoseconds) backing revision tokens and stable-read checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    /// The device id.
    pub dev: u64,
    /// The inode number.
    pub ino: u64,
    /// The file size in bytes.
    pub size: u64,
    /// The last modification time in nanoseconds (may be pre-epoch).
    pub mtime_ns: i128,
    /// The last status change time in nanoseconds (may be pre-epoch).
    pub ctime_ns: i128,
}

impl FileIdentity {
    /// The dsh `fileRevision` token text: the five identity fields joined
    /// with `:` (dsh session-persistence-jsonl/src/index.ts:200-208).
    #[must_use]
    pub fn revision(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            self.dev, self.ino, self.size, self.mtime_ns, self.ctime_ns
        )
    }
}

/// The current stat identity of a file, for revision tokens and
/// stable-read checks.
///
/// # Errors
///
/// Returns `Io` when the metadata read fails.
pub fn file_identity(path: &Path) -> Result<FileIdentity, PersistError> {
    let metadata = std::fs::metadata(path).map_err(PersistError::Io)?;
    Ok(FileIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
        size: metadata.size(),
        mtime_ns: i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec()),
        ctime_ns: i128::from(metadata.ctime()) * 1_000_000_000 + i128::from(metadata.ctime_nsec()),
    })
}

// Rust guideline compliant 2026-10-23
