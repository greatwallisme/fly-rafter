// Rust guideline compliant 2026-10-29
//! Cordis-free local filesystem mechanics (dsh `fsio.ts` port).
//!
//! Module-for-module port: returns validated UTF-8 text, streams large
//! files, and rejects binary data; line windows belong to the read
//! tool (batch 2). Writes stage an exclusive owner-only file in a
//! private sibling directory and atomically publish it.
//!
//! The version token derives from high-resolution identity and
//! freshness fields (unix `dev:ino:size:mtime_ns:ctime_ns`; the
//! Windows arm derives from the FILETIME fields — the token is opaque
//! to every consumer, so the per-platform spelling stays internal).
//! The dsh listing sort is `localeCompare`; the fr ordering is the
//! codepoint order (identical for ASCII names; the non-ASCII
//! divergence is a documented 19.2 candidate).

use std::fs::File;
use std::io::Read;
use std::io::Seek;
use std::io::Write;
use std::path::Path;

use crate::fs_types::FsError;
use crate::fs_types::FsErrorCode;
use crate::fs_types::FsKind;
use crate::fs_types::FsPathKind;

/// The Unicode-codepoint ordering (the dsh `codepointCompare` helper,
/// reused where the source sorts by codepoint rather than locale).
#[must_use]
pub fn codepoint_compare(left: &str, right: &str) -> std::cmp::Ordering {
    left.chars().cmp(right.chars())
}

/// The NUL-byte binary sample window (the dsh `BINARY_SAMPLE_BYTES`).
pub const BINARY_SAMPLE_BYTES: usize = 8192;

/// The bounded diff-basis read chunk (the dsh
/// `DIFF_BASIS_READ_CHUNK_BYTES`).
const DIFF_BASIS_READ_CHUNK_BYTES: usize = 64 * 1024;

/// The exclusive UTF-8 byte limit on each overwrite-diff side (the dsh
/// `DEFAULT_DIFF_BASIS_MAX_BYTES`).
pub const DEFAULT_DIFF_BASIS_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// A resolved local path: the absolute path shown to callers and its
/// realpath identity (dsh `LocalTarget`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalTarget {
    /// Absolute path (symlinks not resolved) — used for display.
    pub display_path: String,
    /// Realpath identity — used as the stable target key and the I/O path.
    pub target_key: String,
}

/// Result of probing a path: `None` when it does not exist.
#[derive(Debug, Clone)]
pub struct PathInfo {
    /// Opaque version token.
    pub version: String,
    /// The POSIX mode bits (0 on platforms without them).
    pub mode: u32,
    /// The followed-entry kind.
    pub kind: FsKind,
    /// Byte size.
    pub size: u64,
}

/// Result of probing a path without following the final component.
#[derive(Debug, Clone)]
pub struct PathLinkInfo {
    /// Opaque version token.
    pub version: String,
    /// The POSIX mode bits (0 on platforms without them).
    pub mode: u32,
    /// The entry kind, including `symlink`.
    pub kind: FsPathKind,
    /// Byte size.
    pub size: u64,
}

/// One local directory child with a resolved target and cheap metadata.
#[derive(Debug, Clone)]
pub struct LocalDirEntry {
    /// Basename of the child inside the listed directory.
    pub name: String,
    /// The child's followed-entry kind.
    pub kind: FsKind,
    /// The resolved child target.
    pub target: LocalTarget,
    /// The freshness token, when still available.
    pub version: Option<String>,
    /// The byte size of a regular file, when still available.
    pub size: Option<u64>,
}

/// The opaque version token from high-resolution identity and freshness
/// metadata (the dsh `versionOf`; unix arm).
#[cfg(unix)]
#[must_use]
pub fn version_of(metadata: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    format!(
        "{}:{}:{}:{}:{}",
        metadata.dev(),
        metadata.ino(),
        metadata.size(),
        metadata.mtime_nsec(),
        metadata.ctime_nsec()
    )
}

/// The opaque version token from the FILETIME identity fields (the dsh
/// `versionOf`; Windows arm — the token stays opaque so the per-platform
/// spelling is internal).
#[cfg(windows)]
#[must_use]
pub fn version_of(metadata: &std::fs::Metadata) -> String {
    use std::os::windows::fs::MetadataExt;
    format!(
        "w:{}:{}:{}",
        metadata.creation_time(),
        metadata.last_write_time(),
        metadata.file_size()
    )
}

/// Whether the joined spelling carries a `..` segment (the dsh
/// physical-traversal regex test; POSIX-only — the Windows display arm
/// always lexically resolves).
#[cfg(unix)]
fn has_parent_segment(raw: &str) -> bool {
    raw.split(['/', '\\']).any(|segment| segment == "..")
}

/// Lexically resolve `path` against `cwd` (the Node `path.resolve`
/// spelling: collapse `.` and inner `..` without touching the
/// filesystem).
#[must_use]
pub fn lexical_resolve(cwd: &str, path: &str) -> String {
    let joined = if Path::new(path).is_absolute() {
        path.to_owned()
    } else {
        let mut base = cwd.to_owned();
        while base.ends_with('/') || base.ends_with('\\') {
            base.pop();
        }
        format!("{base}/{path}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for component in joined.split(['/', '\\']).filter(|part| !part.is_empty()) {
        match component {
            "." => {}
            ".." => {
                // A leading `..` at the root has nothing to pop; Node
                // keeps it.
                if parts.last().is_some_and(|last| *last != "..") {
                    parts.pop();
                } else {
                    parts.push(component);
                }
            }
            other => parts.push(other),
        }
    }
    let mut out = String::new();
    if joined.starts_with('/') {
        out.push('/');
    }
    out.push_str(&parts.join("/"));
    if out.is_empty() {
        out.push('.');
    }
    out
}

/// The Windows arm of the display spelling: always the lexical resolve
/// (the dsh `localDisplayPath` platform branch).
#[cfg(windows)]
#[must_use]
pub fn local_display_path(cwd: &str, path: &str) -> String {
    lexical_resolve(cwd, path)
}

/// The POSIX arm of the display spelling: a raw spelling with a `..`
/// segment keeps its physical traversal; a clean spelling lexically
/// resolves (the dsh `localDisplayPath`).
#[cfg(unix)]
#[must_use]
pub fn local_display_path(cwd: &str, path: &str) -> String {
    if Path::new(path).is_absolute() {
        if has_parent_segment(path) {
            return path.to_owned();
        }
        return lexical_resolve("/", path);
    }
    let raw = format!("{}/{}", cwd.trim_end_matches('/'), path);
    if has_parent_segment(&raw) {
        raw
    } else {
        lexical_resolve(cwd, path)
    }
}

/// The missing-file classification: ENOENT and ENOTDIR both mean the
/// target cannot exist (the dsh `isENOENT`/`isENOTDIR` pair).
fn is_missing(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

/// Whether the metadata failure is a permission fault (the dsh
/// `isPermissionError`; the std error-kind mapping covers both
/// platforms' errno/Win32-code surfaces).
fn is_permission_error(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::PermissionDenied
}

/// Resolve a path to its absolute display path and realpath identity
/// (the dsh `resolveLocalTarget`).
///
/// For a missing target, canonicalize the nearest existing ancestor and
/// append the missing suffix, preserving identity across symlinked
/// ancestors before and after creation.
///
/// # Errors
///
/// An empty/whitespace path, an ENOTDIR parent segment, or a
/// parent traversal that crosses a missing directory (all `FS_NOT_FOUND`).
pub fn resolve_local_target(cwd: &str, path: &str) -> Result<LocalTarget, FsError> {
    if path.trim().is_empty() {
        return Err(FsError::new(
            FsErrorCode::NotFound,
            "file_path must be a non-empty string",
        ));
    }
    let display_path = local_display_path(cwd, path);
    match std::fs::canonicalize(&display_path) {
        Ok(real) => {
            return Ok(LocalTarget {
                display_path,
                target_key: path_to_string(&real),
            });
        }
        Err(error) => {
            // A path component is a file, not a directory: the target
            // can neither exist nor be created.
            if error.kind() == std::io::ErrorKind::NotADirectory {
                return Err(FsError::new(
                    FsErrorCode::NotFound,
                    format!(
                        "cannot resolve \"{display_path}\": a parent path segment is not a directory"
                    ),
                ));
            }
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(FsError::new(FsErrorCode::IoError, error.to_string()));
            }
        }
    }
    // File absent: canonicalize the nearest existing ancestor and
    // re-append the missing suffix.
    let mut missing: Vec<String> = vec![basename_of(&display_path)];
    let mut ancestor = dirname_of(&display_path);
    loop {
        match std::fs::canonicalize(&ancestor) {
            Ok(real_ancestor) => {
                let real = path_to_string(&real_ancestor);
                if missing.iter().any(|part| part == "..") {
                    return Err(FsError::new(
                        FsErrorCode::NotFound,
                        format!(
                            "cannot resolve \"{display_path}\": parent traversal crosses a missing directory"
                        ),
                    ));
                }
                let mut key = real;
                for part in &missing {
                    if !key.ends_with('/') {
                        key.push('/');
                    }
                    key.push_str(part);
                }
                return Ok(LocalTarget {
                    display_path,
                    target_key: key,
                });
            }
            Err(error) => {
                if error.kind() == std::io::ErrorKind::NotADirectory {
                    return Err(FsError::new(
                        FsErrorCode::NotFound,
                        format!(
                            "cannot resolve \"{display_path}\": a parent path segment is not a directory"
                        ),
                    ));
                }
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(FsError::new(FsErrorCode::IoError, error.to_string()));
                }
                let parent = dirname_of(&ancestor);
                if parent == ancestor {
                    return Ok(LocalTarget {
                        display_path: display_path.clone(),
                        target_key: display_path,
                    });
                }
                missing.insert(0, basename_of(&ancestor));
                ancestor = parent;
            }
        }
    }
}

/// The final path segment (the `basename` helper).
#[must_use]
pub fn basename_of(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rsplit(['/', '\\']).next() {
        Some(name) if !name.is_empty() => name.to_owned(),
        _ => trimmed.to_owned(),
    }
}

/// The path minus its final segment (the `dirname` helper).
#[must_use]
pub fn dirname_of(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        Some(0) => "/".to_owned(),
        Some(index) => trimmed[..index].to_owned(),
        None => ".".to_owned(),
    }
}

/// Converts a canonicalized path back to its string spelling.
fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The followed-kind word for one metadata result.
fn path_kind(metadata: &std::fs::Metadata) -> FsKind {
    if metadata.is_file() {
        FsKind::File
    } else if metadata.is_dir() {
        FsKind::Directory
    } else {
        FsKind::Other
    }
}

/// The no-follow kind word for one symlink-metadata result.
fn path_link_kind(metadata: &std::fs::Metadata) -> FsPathKind {
    if metadata.is_symlink() {
        FsPathKind::Symlink
    } else if metadata.is_file() {
        FsPathKind::File
    } else if metadata.is_dir() {
        FsPathKind::Directory
    } else {
        FsPathKind::Other
    }
}

/// The POSIX mode bits of one metadata result (0 off unix).
#[cfg(unix)]
fn mode_bits(metadata: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o777
}

/// The POSIX mode bits of one metadata result (0 off unix).
#[cfg(not(unix))]
fn mode_bits(_metadata: &std::fs::Metadata) -> u32 {
    0
}

/// Probe a path for its version, mode, kind, and size. `None` if absent
/// (the dsh `probe`).
///
/// # Errors
///
/// A non-missing metadata fault (permission/IO) surfaces instead of an
/// absent result.
pub fn probe(absolute_path: &str) -> Result<Option<PathInfo>, std::io::Error> {
    let metadata = match std::fs::metadata(absolute_path) {
        Ok(info) => info,
        Err(error) if is_missing(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(Some(PathInfo {
        version: version_of(&metadata),
        mode: mode_bits(&metadata),
        kind: path_kind(&metadata),
        size: metadata.len(),
    }))
}

/// Probe a path without following the final symlink component (the dsh
/// `probeNoFollow`).
///
/// # Errors
///
/// A non-missing metadata fault surfaces instead of an absent result.
pub fn probe_no_follow(absolute_path: &str) -> Result<Option<PathLinkInfo>, std::io::Error> {
    let metadata = match std::fs::symlink_metadata(absolute_path) {
        Ok(info) => info,
        Err(error) if is_missing(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(Some(PathLinkInfo {
        version: version_of(&metadata),
        mode: mode_bits(&metadata),
        kind: path_link_kind(&metadata),
        size: metadata.len(),
    }))
}

/// The structured listing refusal (the dsh `listingIoError`).
fn listing_io_error(display_path: &str, error: &std::io::Error) -> FsError {
    if is_missing(error) {
        return FsError::new(
            FsErrorCode::NotFound,
            format!("cannot list \"{display_path}\": not found"),
        );
    }
    if is_permission_error(error) {
        return FsError::new(
            FsErrorCode::PermissionDenied,
            format!("cannot list \"{display_path}\": permission denied"),
        );
    }
    FsError::new(
        FsErrorCode::IoError,
        format!("cannot list \"{display_path}\": {error}"),
    )
}

/// List direct children of a directory in stable name order. Each child
/// includes a resolved target plus stat metadata when still available;
/// file contents are never read (the dsh `listDirectory`).
///
/// # Errors
///
/// An absent or non-directory target, an aborted listing (the signal
/// poll), or a listing/child-resolution I/O fault.
pub fn list_directory(
    target: &LocalTarget,
    signal: &dyn Fn() -> bool,
) -> Result<Vec<LocalDirEntry>, FsError> {
    if signal() {
        return Err(FsError::new(FsErrorCode::Aborted, "list aborted"));
    }
    let info = probe(&target.target_key)
        .map_err(|error| listing_io_error(&target.display_path, &error))?;
    let Some(info) = info else {
        return Err(FsError::new(
            FsErrorCode::NotFound,
            format!("cannot list \"{}\": not found", target.display_path),
        ));
    };
    if info.kind != FsKind::Directory {
        return Err(FsError::new(
            FsErrorCode::NotDirectory,
            format!("cannot list \"{}\": not a directory", target.display_path),
        ));
    }
    let mut names: Vec<String> = Vec::new();
    let entries = std::fs::read_dir(&target.target_key)
        .map_err(|error| listing_io_error(&target.display_path, &error))?;
    for entry in entries {
        let entry = entry.map_err(|error| listing_io_error(&target.display_path, &error))?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    if signal() {
        return Err(FsError::new(FsErrorCode::Aborted, "list aborted"));
    }
    // The stable name order (the dsh `localeCompare` sort; the fr
    // ordering is the codepoint order — ASCII names sort identically,
    // and the non-ASCII divergence is the module-docs note).
    names.sort_by(|left, right| codepoint_compare(left, right));
    let mut result: Vec<LocalDirEntry> = Vec::new();
    for name in &names {
        if signal() {
            return Err(FsError::new(FsErrorCode::Aborted, "list aborted"));
        }
        let identity = resolve_local_target(&target.target_key, name)?;
        let child = LocalTarget {
            display_path: local_display_path(&target.display_path, name),
            target_key: identity.target_key,
        };
        let child_info = probe(&child.target_key)
            .map_err(|error| listing_io_error(&child.display_path, &error))?;
        result.push(LocalDirEntry {
            name: name.clone(),
            kind: child_info.as_ref().map_or(FsKind::Other, |info| info.kind),
            target: child,
            version: child_info.as_ref().map(|info| info.version.clone()),
            size: child_info
                .as_ref()
                .filter(|info| info.kind == FsKind::File)
                .map(|info| info.size),
        });
    }
    Ok(result)
}

/// The typed not-text refusal (the dsh `notTextError`).
fn not_text_error(verb: &str, display_path: &str) -> FsError {
    FsError::new(
        FsErrorCode::NotText,
        format!("cannot {verb} \"{display_path}\": invalid UTF-8 text"),
    )
}

/// Strict UTF-8 decode of one whole buffer (the dsh `decodeUtf8`).
fn decode_utf8(buffer: &[u8], verb: &str, display_path: &str) -> Result<String, FsError> {
    String::from_utf8(buffer.to_vec()).map_err(|error| {
        // The decode detail never reaches the model; the typed refusal
        // is the whole contract (the dsh TextDecoder mapping).
        let _ = error;
        not_text_error(verb, display_path)
    })
}

/// The regular-file stat preflight shared by the read family (the dsh
/// `statRegularFile`).
fn stat_regular_file(
    target: &LocalTarget,
    verb: &str,
    signal: &dyn Fn() -> bool,
) -> Result<std::fs::Metadata, FsError> {
    if signal() {
        return Err(FsError::new(
            FsErrorCode::Aborted,
            format!("{verb} aborted"),
        ));
    }
    let info = std::fs::metadata(&target.target_key).map_err(|error| {
        if is_missing(&error) {
            FsError::new(
                FsErrorCode::NotFound,
                format!("cannot {verb} \"{}\": not found", target.display_path),
            )
        } else {
            FsError::new(FsErrorCode::IoError, error.to_string())
        }
    })?;
    if !info.is_file() {
        return Err(FsError::new(
            FsErrorCode::NotRegularFile,
            format!(
                "cannot {verb} \"{}\": not a regular file",
                target.display_path
            ),
        ));
    }
    Ok(info)
}

/// Read a whole regular UTF-8 text file into a single decoded string.
/// Rejects non-regular files, invalid UTF-8, and NUL-byte binary
/// samples (the dsh `readWholeText`).
///
/// # Errors
///
/// The stat preflight, the binary sample, or the strict decode.
pub fn read_whole_text(target: &LocalTarget, signal: &dyn Fn() -> bool) -> Result<String, FsError> {
    stat_regular_file(target, "read", signal)?;
    let raw = std::fs::read(&target.target_key).map_err(|error| {
        if is_missing(&error) {
            FsError::new(
                FsErrorCode::NotFound,
                format!("cannot read \"{}\": not found", target.display_path),
            )
        } else {
            FsError::new(FsErrorCode::IoError, error.to_string())
        }
    })?;
    if signal() {
        return Err(FsError::new(FsErrorCode::Aborted, "read aborted"));
    }
    let sample_end = raw.len().min(BINARY_SAMPLE_BYTES);
    if raw[..sample_end].contains(&0) {
        return Err(FsError::new(
            FsErrorCode::NotText,
            format!("cannot read \"{}\": binary file", target.display_path),
        ));
    }
    decode_utf8(&raw, "read", &target.display_path)
}

/// Read a whole regular file as raw bytes (the dsh `readWholeBytes`).
///
/// No decoding or binary rejection. `max_bytes` bounds the complete
/// content: the stat size short-circuits an oversized file before any
/// content I/O, and the read stops at most one byte past the cap so a
/// file growing after the stat cannot cause unbounded buffering.
///
/// # Errors
///
/// The stat preflight, the size cap, or a mid-read abort.
pub fn read_whole_bytes(
    target: &LocalTarget,
    signal: &dyn Fn() -> bool,
    max_bytes: u64,
) -> Result<Vec<u8>, FsError> {
    let info = stat_regular_file(target, "read", signal)?;
    if info.len() > max_bytes {
        return Err(FsError::new(
            FsErrorCode::TooLarge,
            format!(
                "cannot read \"{}\": {} bytes exceeds the {max_bytes}-byte limit",
                target.display_path,
                info.len()
            ),
        ));
    }
    let mut file = File::open(&target.target_key)
        .map_err(|error| FsError::new(FsErrorCode::IoError, error.to_string()))?;
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = vec![0_u8; DIFF_BASIS_READ_CHUNK_BYTES];
    loop {
        if signal() {
            return Err(FsError::new(FsErrorCode::Aborted, "read aborted"));
        }
        let read = file
            .read(&mut chunk)
            .map_err(|error| FsError::new(FsErrorCode::IoError, error.to_string()))?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() as u64 > max_bytes {
            return Err(FsError::new(
                FsErrorCode::TooLarge,
                format!(
                    "cannot read \"{}\": content exceeds the {max_bytes}-byte limit",
                    target.display_path
                ),
            ));
        }
    }
    Ok(buffer)
}

/// Read the bytes at `[offset, offset + length)` of a regular file with
/// no decoding or binary rejection; a window at or past the end is
/// empty (the dsh `readByteWindow`).
///
/// # Errors
///
/// The stat preflight or a mid-read abort.
pub fn read_byte_window(
    target: &LocalTarget,
    offset: u64,
    length: u64,
    signal: &dyn Fn() -> bool,
) -> Result<Vec<u8>, FsError> {
    stat_regular_file(target, "read", signal)?;
    if length == 0 {
        return Ok(Vec::new());
    }
    let mut file = File::open(&target.target_key)
        .map_err(|error| FsError::new(FsErrorCode::IoError, error.to_string()))?;
    file.seek(std::io::SeekFrom::Start(offset))
        .map_err(|error| FsError::new(FsErrorCode::IoError, error.to_string()))?;
    let mut remaining = length;
    let mut buffer = Vec::with_capacity(usize::try_from(length).unwrap_or(1024));
    let mut chunk = vec![0_u8; DIFF_BASIS_READ_CHUNK_BYTES];
    while remaining > 0 {
        if signal() {
            return Err(FsError::new(FsErrorCode::Aborted, "read aborted"));
        }
        let want = usize::try_from(remaining.min(chunk.len() as u64)).unwrap_or(chunk.len());
        let read = file
            .read(&mut chunk[..want])
            .map_err(|error| FsError::new(FsErrorCode::IoError, error.to_string()))?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        remaining -= read as u64;
    }
    Ok(buffer)
}

/// The streaming whole-file text reader (the dsh `streamWholeText`
/// generator, carried as a pull iterator).
///
/// Same text semantics as [`read_whole_text`] but never holds the whole
/// file in memory; each `next` polls the abort signal and yields one
/// decoded text chunk (chunk boundaries carry no meaning).
pub struct StreamWholeText {
    file: File,
    display_path: String,
    carry: Vec<u8>,
    sampled_bytes: usize,
    eof: bool,
    signal: Box<dyn Fn() -> bool>,
}

impl std::fmt::Debug for StreamWholeText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamWholeText")
            .field("display_path", &self.display_path)
            .field("sampled_bytes", &self.sampled_bytes)
            .field("eof", &self.eof)
            .finish_non_exhaustive()
    }
}

impl StreamWholeText {
    /// Opens the streaming reader behind the stat preflight.
    ///
    /// # Errors
    ///
    /// The stat preflight refusal.
    pub fn open(target: &LocalTarget, signal: Box<dyn Fn() -> bool>) -> Result<Self, FsError> {
        stat_regular_file(target, "read", signal.as_ref())?;
        let file = File::open(&target.target_key).map_err(|error| {
            if is_missing(&error) {
                FsError::new(
                    FsErrorCode::NotFound,
                    format!("cannot read \"{}\": not found", target.display_path),
                )
            } else {
                FsError::new(FsErrorCode::IoError, error.to_string())
            }
        })?;
        Ok(Self {
            file,
            display_path: target.display_path.clone(),
            carry: Vec::new(),
            sampled_bytes: 0,
            eof: false,
            signal,
        })
    }
}

impl Iterator for StreamWholeText {
    type Item = Result<String, FsError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.eof {
            return None;
        }
        let mut chunk = vec![0_u8; DIFF_BASIS_READ_CHUNK_BYTES];
        let mut got: Vec<u8> = Vec::new();
        // Fill one chunk of raw bytes (a short final read ends the
        // stream).
        while got.is_empty() {
            if (self.signal)() {
                self.eof = true;
                return Some(Err(FsError::new(FsErrorCode::Aborted, "read aborted")));
            }
            match self.file.read(&mut chunk) {
                Ok(0) => {
                    self.eof = true;
                    // Flush the carry as the final chunk (an invalid
                    // trailing sequence refuses here, like the fatal
                    // decoder flush).
                    return Some(String::from_utf8(std::mem::take(&mut self.carry)).map_err(
                        |error| {
                            // The decode detail never reaches the model
                            // (the dsh fatal-flush mapping).
                            let _ = error;
                            not_text_error("read", &self.display_path)
                        },
                    ));
                }
                Ok(read) => got.extend_from_slice(&chunk[..read]),
                Err(error) => {
                    self.eof = true;
                    return Some(Err(FsError::new(FsErrorCode::IoError, error.to_string())));
                }
            }
        }
        // The binary sample scans the first BINARY_SAMPLE_BYTES bytes.
        if self.sampled_bytes < BINARY_SAMPLE_BYTES {
            let take = got.len().min(BINARY_SAMPLE_BYTES - self.sampled_bytes);
            if got[..take].contains(&0) {
                self.eof = true;
                return Some(Err(FsError::new(
                    FsErrorCode::NotText,
                    format!("cannot read \"{}\": binary file", self.display_path),
                )));
            }
            self.sampled_bytes += take;
        }
        // Incremental decode: carry the tail bytes of a split sequence
        // to the next chunk; an invalid sequence refuses (the dsh fatal
        // decoder).
        let mut bytes = std::mem::take(&mut self.carry);
        bytes.extend_from_slice(&got);
        match std::str::from_utf8(&bytes) {
            Ok(text) => Some(Ok(text.to_owned())),
            Err(error) => {
                let valid = error.valid_up_to();
                let bad_len = bytes.len() - valid;
                // A tail shorter than the longest split sequence can
                // still decode once more bytes arrive; it rides to the
                // next chunk (the dsh streaming decoder's carry).
                if bad_len < 4 {
                    self.carry = bytes.split_off(valid);
                    if valid == 0 {
                        return self.next();
                    }
                    // The split prefix validated above, so the decode
                    // cannot fail (the expect-reason idiom).
                    let prefix = std::str::from_utf8(&bytes)
                        .expect("the split prefix validated against the same bytes")
                        .to_owned();
                    return Some(Ok(prefix));
                }
                self.eof = true;
                Some(Err(not_text_error("read", &self.display_path)))
            }
        }
    }
}

/// The POSIX mode of a file written through the staging path (an
/// owner-only 0o600 temp, the destination mode restored at staging).
pub const STAGING_FILE_MODE: u32 = 0o600;

/// The staging directory mode (private to the owner, the dsh
/// `0o700`).
#[cfg(unix)]
fn set_private_mode(path: &str, mode: u32) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// The staging-directory mode setter off unix (the ACL story rides the
/// default inheritance).
#[cfg(not(unix))]
fn set_private_mode(_path: &str, _mode: u32) -> Result<(), std::io::Error> {
    Ok(())
}

/// Removes the staging directory tree, ignoring absence (the dsh
/// `rm(recursive, force)` cleanup).
fn remove_staging_dir(staging_dir: &str) {
    let _ = std::fs::remove_dir_all(staging_dir);
}

/// Atomically replace a file through a private, synced staging file
/// in the same directory (the dsh `writeFileAtomic`).
///
/// POSIX protects the staging directory and file with `0o700` and
/// `0o600`. A guarded create publishes through a no-replace hard link
/// so a concurrent creator's file is preserved; the Windows
/// `ReplaceFileW` arm lives in the `publish_staged_windows` helper
/// (referenced by name only: the target is cfg(windows)-only).
///
/// # Errors
///
/// The staging creation, the write, or the publication refusal.
pub fn write_file_atomic(
    absolute_path: &str,
    content: &str,
    mode: Option<u32>,
    signal: &dyn Fn() -> bool,
    create_if_absent: Option<&str>,
) -> Result<(), FsError> {
    if signal() {
        return Err(FsError::new(FsErrorCode::Aborted, "write aborted"));
    }
    let directory = dirname_of(absolute_path);
    std::fs::create_dir_all(&directory)
        .map_err(|error| FsError::new(FsErrorCode::IoError, error.to_string()))?;
    if signal() {
        return Err(FsError::new(FsErrorCode::Aborted, "write aborted"));
    }
    let base = basename_of(absolute_path);
    let staging_dir = format!(
        "{directory}/.{}.{}.{}.tmpdir",
        base,
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    let temp_path = format!("{staging_dir}/{base}.tmp");
    let mut staged = false;
    let failure = (|| -> Result<(), FsError> {
        std::fs::create_dir(&staging_dir)
            .map_err(|error| FsError::new(FsErrorCode::IoError, error.to_string()))?;
        staged = true;
        set_private_mode(&staging_dir, 0o700)
            .map_err(|error| FsError::new(FsErrorCode::IoError, error.to_string()))?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .map_err(|error| FsError::new(FsErrorCode::IoError, error.to_string()))?;
        set_private_mode(&temp_path, STAGING_FILE_MODE)
            .map_err(|error| FsError::new(FsErrorCode::IoError, error.to_string()))?;
        file.write_all(content.as_bytes())
            .map_err(|error| FsError::new(FsErrorCode::IoError, error.to_string()))?;
        file.sync_all()
            .map_err(|error| FsError::new(FsErrorCode::IoError, error.to_string()))?;
        if let Some(mode) = mode {
            set_private_mode(&temp_path, mode)
                .map_err(|error| FsError::new(FsErrorCode::IoError, error.to_string()))?;
        }
        drop(file);
        if signal() {
            return Err(FsError::new(FsErrorCode::Aborted, "write aborted"));
        }
        #[cfg(windows)]
        publish_staged_windows(absolute_path, &temp_path, mode, create_if_absent)?;
        #[cfg(not(windows))]
        publish_staged(absolute_path, &temp_path, create_if_absent)?;
        // The target is committed; staging residue cannot turn the
        // write into a failure.
        remove_staging_dir(&staging_dir);
        Ok(())
    })();
    if let Err(error) = failure {
        if staged {
            remove_staging_dir(&staging_dir);
        }
        return Err(error);
    }
    Ok(())
}

/// Publishes the staged temp onto the destination: a guarded create
/// goes through the no-replace hard link (a concurrent creator's file
/// survives and this write refuses `FS_NOT_OBSERVED`), everything else
/// renames over (the dsh publication fork).
fn publish_staged(
    absolute_path: &str,
    temp_path: &str,
    create_if_absent: Option<&str>,
) -> Result<(), FsError> {
    match create_if_absent {
        Some(display_path) => {
            if let Err(error) = std::fs::hard_link(temp_path, absolute_path) {
                // Inspect the target after the failed publication so a
                // collision is not confused with missing hard-link
                // support (the dsh `throwGuardedCreateFailure`).
                let existing = std::fs::symlink_metadata(absolute_path).ok();
                if let Some(existing) = existing {
                    if !existing.is_file() {
                        return Err(FsError::new(
                            FsErrorCode::NotRegularFile,
                            format!("cannot write \"{display_path}\": not a regular file"),
                        ));
                    }
                    return Err(FsError::new(
                        FsErrorCode::NotObserved,
                        format!(
                            "cannot overwrite existing \"{display_path}\" without reading it first"
                        ),
                    ));
                }
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    return Err(FsError::new(
                        FsErrorCode::NotObserved,
                        format!(
                            "cannot overwrite existing \"{display_path}\" without reading it first"
                        ),
                    ));
                }
                return Err(FsError::new(
                    FsErrorCode::IoError,
                    format!("cannot write \"{display_path}\": {error}"),
                ));
            }
            Ok(())
        }
        None => std::fs::rename(temp_path, absolute_path)
            .map_err(|error| FsError::new(FsErrorCode::IoError, error.to_string())),
    }
}

/// The Windows publication arm: a guarded create keeps the no-replace
/// hard link; a mode-bearing replacement goes through `ReplaceFileW`
/// (the security-descriptor-preserving swap), falling back to the plain
/// rename when the observed target vanished during staging. This body
/// mirrors [`publish_staged`]'s unix fork.
#[cfg(windows)]
fn publish_staged_windows(
    absolute_path: &str,
    temp_path: &str,
    mode: Option<u32>,
    create_if_absent: Option<&str>,
) -> Result<(), FsError> {
    match create_if_absent {
        Some(_) => publish_staged(absolute_path, temp_path, create_if_absent),
        None => {
            if mode.is_some() && Path::new(absolute_path).exists() {
                if replace_file_win32(absolute_path, temp_path) {
                    return Ok(());
                }
                // The observed target vanished during staging: the
                // copied descriptor already lives on the temp, so the
                // plain rename recreates it.
            }
            std::fs::rename(temp_path, absolute_path)
                .map_err(|error| FsError::new(FsErrorCode::IoError, error.to_string()))
        }
    }
}

/// The `ReplaceFileW` swap preserving the replaced file's security
/// descriptor (the dsh `replaceFileWin32`; `false` on any failure —
/// the caller falls back to the plain rename where the source allows).
#[cfg(windows)]
fn replace_file_win32(replaced: &str, replacement: &str) -> bool {
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;
    let wide_replaced: Vec<u16> = replaced
        .replace('/', "\\")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let wide_replacement: Vec<u16> = replacement
        .replace('/', "\\")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: both paths are null-terminated UTF-16 buffers valid for
    // the call; the remaining arguments are the documented nulls/zeros.
    let ok = unsafe {
        ReplaceFileW(
            wide_replaced.as_ptr(),
            wide_replacement.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    ok != 0
}

/// The line-ending style detected before LF normalization (the dsh
/// `LineEndings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEndings {
    /// The file's newlines are plain LF.
    Lf,
    /// The file's newlines are CRLF.
    CrLf,
}

/// Collapse CRLF to LF — the canonical in-memory form every edit/diff
/// basis uses. Lone `\r` bytes stay untouched (the dsh
/// `normalizeLineEndings`).
#[must_use]
pub fn normalize_line_endings(content: &str) -> String {
    content.replace("\r\n", "\n")
}

/// Detects the line-ending style over the leading 4096-char sample
/// (the dsh `detectLineEndings`; the CRLF-majority rule).
#[must_use]
pub fn detect_line_endings(raw: &str) -> LineEndings {
    let sample: String = raw.chars().take(4096).collect();
    let crlf = sample.matches("\r\n").count();
    let lf = sample.matches('\n').count() - crlf;
    if crlf > lf {
        LineEndings::CrLf
    } else {
        LineEndings::Lf
    }
}

/// Convert LF-normalized content back to the style detected at read
/// time (the dsh `restoreLineEndings`; CRLF re-normalizes first so an
/// already-CRLF sequence never doubles).
#[must_use]
pub fn restore_line_endings(content: &str, endings: LineEndings) -> String {
    match endings {
        LineEndings::Lf => content.to_owned(),
        LineEndings::CrLf => normalize_line_endings(content).replace('\n', "\r\n"),
    }
}

/// The non-overlapping occurrence count (the dsh `countOccurrences`).
#[must_use]
pub fn count_occurrences(content: &str, needle: &str) -> usize {
    let mut count = 0;
    let mut offset = 0;
    while let Some(found) = content[offset..].find(needle) {
        count += 1;
        offset += found + needle.len();
    }
    count
}

/// Read and decode a file for editing: rejects binaries across the
/// whole buffer, returns LF-normalized content plus the original
/// line-ending style for write-back (the dsh `readForEdit`).
///
/// # Errors
///
/// The abort poll, the binary scan, or the strict decode.
pub fn read_for_edit(
    absolute_path: &str,
    display_path: &str,
    signal: &dyn Fn() -> bool,
) -> Result<(String, LineEndings), FsError> {
    if signal() {
        return Err(FsError::new(FsErrorCode::Aborted, "edit aborted"));
    }
    let buffer = std::fs::read(absolute_path).map_err(|error| {
        if is_missing(&error) {
            FsError::new(
                FsErrorCode::NotFound,
                format!("cannot edit \"{display_path}\": not found"),
            )
        } else {
            FsError::new(FsErrorCode::IoError, error.to_string())
        }
    })?;
    if signal() {
        return Err(FsError::new(FsErrorCode::Aborted, "edit aborted"));
    }
    if buffer.contains(&0) {
        return Err(FsError::new(
            FsErrorCode::NotText,
            format!("cannot edit \"{display_path}\": binary file"),
        ));
    }
    let raw = decode_utf8(&buffer, "edit", display_path)?;
    let endings = detect_line_endings(&raw);
    Ok((normalize_line_endings(&raw), endings))
}

/// Apply a literal replacement to LF-normalized content (the dsh
/// `applyLiteralEdit`).
///
/// Empty or missing search text throws `FS_EDIT_NOT_FOUND`; multiple
/// matches throw `FS_AMBIGUOUS_EDIT` unless `replace_all` is true.
///
/// # Errors
///
/// The empty-needle, not-found, and ambiguous-match refusals.
pub fn apply_literal_edit(
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
    display_path: &str,
) -> Result<(String, usize), FsError> {
    let old_norm = normalize_line_endings(old_string);
    if old_norm.is_empty() {
        return Err(FsError::new(
            FsErrorCode::EditNotFound,
            "old_string must be a non-empty string",
        ));
    }
    let new_norm = normalize_line_endings(new_string);
    let replacements = count_occurrences(content, &old_norm);
    if replacements == 0 {
        return Err(FsError::new(
            FsErrorCode::EditNotFound,
            format!("old_string was not found in \"{display_path}\""),
        ));
    }
    if !replace_all && replacements > 1 {
        return Err(FsError::new(
            FsErrorCode::AmbiguousEdit,
            format!(
                "old_string matched {replacements} times in \"{display_path}\"; \
                 provide a more specific old_string or set replace_all to true"
            ),
        ));
    }
    Ok((content.replace(&old_norm, &new_norm), replacements))
}

/// Best-effort overwrite diff basis (the dsh `readTextForDiff`).
///
/// Binary, invalid UTF-8, a file at or above the byte limit, or a file
/// deleted/made unreadable after the caller's preflight returns `None`
/// so the write still succeeds; the bound is enforced on the opened
/// file, and a size change since the descriptor stat declines the
/// basis.
///
/// # Errors
///
/// Only cancellation propagates; every I/O-shaped fault costs just the
/// optional basis.
pub fn read_text_for_diff(
    absolute_path: &str,
    max_bytes: u64,
    signal: &dyn Fn() -> bool,
) -> Result<Option<String>, FsError> {
    if signal() {
        return Err(FsError::new(FsErrorCode::Aborted, "read aborted"));
    }
    let result = (|| -> Result<Option<String>, std::io::Error> {
        let mut file = File::open(absolute_path)?;
        let info = file.metadata()?;
        if !info.is_file() {
            return Ok(None);
        }
        let opened_size = info.len();
        if opened_size >= max_bytes {
            return Ok(None);
        }
        // One extra byte detects growth after the stat without
        // retaining more than the bounded read.
        let limit = usize::try_from(opened_size).unwrap_or(0) + 1;
        let mut buffer = Vec::with_capacity(limit);
        let mut chunk = vec![0_u8; DIFF_BASIS_READ_CHUNK_BYTES];
        loop {
            if signal() {
                return Err(std::io::Error::other("read aborted"));
            }
            let want = chunk.len().min(limit.saturating_sub(buffer.len()));
            if want == 0 {
                break;
            }
            let read = file.read(&mut chunk[..want])?;
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
        if buffer.len() as u64 != opened_size {
            return Ok(None);
        }
        if buffer.contains(&0) {
            return Ok(None);
        }
        Ok(decode_utf8(&buffer, "read", absolute_path)
            .ok()
            .map(|text| normalize_line_endings(&text)))
    })();
    match result {
        Ok(basis) => Ok(basis),
        Err(error) => {
            // Cancellation is the caller's intent and still propagates;
            // every descriptor-phase fault costs only the optional
            // basis.
            if signal() {
                Err(FsError::new(FsErrorCode::Aborted, "read aborted"))
            } else {
                let _ = error;
                Ok(None)
            }
        }
    }
}
