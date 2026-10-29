// Rust guideline compliant 2026-10-29
//! The filesystem-domain vocabulary (dsh `types.ts` port).
//!
//! Module-for-module port of `@deepseek-ai/dsh-fs`'s vocabulary:
//! opaque target/version identities, the typed error taxonomy, and the
//! write/edit outcome shapes shared by the local backend, the
//! observation gate, the sandbox fence, and (from batch 2) the
//! read/write/edit tools.
//!
//! One structural mapping note: dsh extends `HarnessError` (code +
//! `cause` chain); the fr body-error wire shape carries
//! `{code, message, name}` and the fr-shell/fr-tools-builtin precedent
//! keeps the wire shape authoritative while the in-memory struct stays
//! plain. The `cause` chain survives as the remediating wrapper's
//! message text (batch 2).

use serde::{Deserialize, Serialize};

/// Stable, machine-routable codes for filesystem failures (the dsh
/// `FsErrorCode` 13-code taxonomy, same members in the same order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FsErrorCode {
    /// The path (or a parent segment) does not exist.
    #[serde(rename = "FS_NOT_FOUND")]
    NotFound,
    /// The target is not a directory where one is required.
    #[serde(rename = "FS_NOT_DIRECTORY")]
    NotDirectory,
    /// The file content is binary or otherwise not text.
    #[serde(rename = "FS_NOT_TEXT")]
    NotText,
    /// The target is not a regular file where one is required.
    #[serde(rename = "FS_NOT_REGULAR_FILE")]
    NotRegularFile,
    /// The content exceeds an inclusive byte cap.
    #[serde(rename = "FS_TOO_LARGE")]
    TooLarge,
    /// A permission fault (EACCES/EPERM family).
    #[serde(rename = "FS_PERMISSION_DENIED")]
    PermissionDenied,
    /// A sandbox policy denial (the fence's structured refusal).
    #[serde(rename = "FS_SANDBOX_DENIED")]
    SandboxDenied,
    /// A generic I/O fault.
    #[serde(rename = "FS_IO_ERROR")]
    IoError,
    /// A guarded mutation's version basis went stale.
    #[serde(rename = "FS_STALE_VERSION")]
    StaleVersion,
    /// A guarded mutation's prior-observation basis is missing.
    #[serde(rename = "FS_NOT_OBSERVED")]
    NotObserved,
    /// The `old_string` matched zero occurrences.
    #[serde(rename = "FS_EDIT_NOT_FOUND")]
    EditNotFound,
    /// The `old_string` matched more than once.
    #[serde(rename = "FS_AMBIGUOUS_EDIT")]
    AmbiguousEdit,
    /// A cancellation cut the operation short.
    #[serde(rename = "FS_ABORTED")]
    Aborted,
}

impl FsErrorCode {
    /// The wire spelling (the dsh code words; the serde rename covers
    /// JSON round-trips, this covers message building).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "FS_NOT_FOUND",
            Self::NotDirectory => "FS_NOT_DIRECTORY",
            Self::NotText => "FS_NOT_TEXT",
            Self::NotRegularFile => "FS_NOT_REGULAR_FILE",
            Self::TooLarge => "FS_TOO_LARGE",
            Self::PermissionDenied => "FS_PERMISSION_DENIED",
            Self::SandboxDenied => "FS_SANDBOX_DENIED",
            Self::IoError => "FS_IO_ERROR",
            Self::StaleVersion => "FS_STALE_VERSION",
            Self::NotObserved => "FS_NOT_OBSERVED",
            Self::EditNotFound => "FS_EDIT_NOT_FOUND",
            Self::AmbiguousEdit => "FS_AMBIGUOUS_EDIT",
            Self::Aborted => "FS_ABORTED",
        }
    }
}

/// One typed filesystem failure: the stable code plus the model-facing
/// message (dsh `FsError extends HarnessError`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsError {
    /// The stable machine-routable code.
    pub code: FsErrorCode,
    /// The model-facing message.
    pub message: String,
}

impl FsError {
    /// Builds one typed failure.
    #[must_use]
    pub fn new(code: FsErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl From<std::io::Error> for FsError {
    fn from(error: std::io::Error) -> Self {
        // A raw platform fault surfaces with its message and the
        // generic I/O code (the dsh lets non-taxonomy errors through
        // without a code; the fr wire shape keeps the typed channel).
        Self::new(FsErrorCode::IoError, error.to_string())
    }
}

/// A path resolved by the backend into a stable identity
/// (dsh `FsTarget`; the local backend's key is the realpath spelling).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsTarget {
    /// Opaque key for stale guards and target lookup.
    pub target_key: String,
    /// Path for model/UI-facing output.
    pub display_path: String,
}

/// The `stat` metadata (dsh `FsInfo`; `size` rides as an option).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsInfo {
    /// Opaque freshness token of the target right now.
    pub version: String,
    /// Regular file, directory, or something else.
    pub kind: FsKind,
    /// Byte size of a regular file, when the backend can report it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// The path-level probe result without following the final symlink
/// (dsh `FsPathInfo`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsPathInfo {
    /// Opaque freshness token of the path entry right now.
    pub version: String,
    /// Regular file, directory, symlink, or other.
    pub kind: FsPathKind,
    /// Byte size of the path entry, when the backend can report it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// The target kind words (`file` / `directory` / `other`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FsKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// Anything else.
    Other,
}

/// The path-entry kind words (`file` / `directory` / `symlink` / `other`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FsPathKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// A symbolic link.
    Symlink,
    /// Anything else.
    Other,
}

/// One directory child (dsh `FsDirEntry`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsDirEntry {
    /// Basename of the child inside the listed directory.
    pub name: String,
    /// Whether the child is a regular file, a directory, or something else.
    pub kind: FsKind,
    /// Resolved child target for follow-up operations.
    pub target: FsTarget,
    /// Opaque freshness token when the backend can report metadata cheaply.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Byte size of a regular file, when the backend can report it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// One authoritative observation/intent pair (dsh `FsObservation`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum FsObservation {
    /// Present with the observed version.
    Present {
        /// The version the observation carries.
        version: String,
    },
    /// Confirmed absent.
    Absent,
}

/// The guarded write intent (dsh `FsWriteIntent`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum FsWriteIntent {
    /// Rejects an existing target with `FS_NOT_OBSERVED`.
    CreateIfAbsent,
    /// Rejects absence or mismatch with `FS_STALE_VERSION`.
    ReplaceIfVersion {
        /// The freshness basis the write guards against.
        version: String,
    },
}

/// Outcome of a full-file write (dsh `FsWriteOutcome`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsWriteOutcome {
    /// Whether the write created a new file or replaced an existing one.
    pub operation: WriteOperation,
    /// Opaque version of the file after the write.
    pub version: String,
    /// The file's LF-normalized content before the write, or `None` when
    /// the file did not exist or the backend declined a contextual basis.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    /// The file's content after the write, LF-normalized.
    pub after: String,
}

/// The write operation word (`create` / `update`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WriteOperation {
    /// The write created the file.
    Create,
    /// The write replaced an existing file.
    Update,
}

/// A literal-replacement edit request (dsh `FsEditRequest`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsEditRequest {
    /// Literal non-empty text to replace (line endings normalized before matching).
    pub old_string: String,
    /// Literal replacement text; an empty string deletes the match.
    pub new_string: String,
    /// Replace every match instead of requiring exactly one.
    pub replace_all: bool,
}

/// Outcome of a literal edit (dsh `FsEditOutcome`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsEditOutcome {
    /// Opaque version of the file after the edit.
    pub version: String,
    /// The LF-normalized content before the edit.
    pub before: String,
    /// The LF-normalized content after the edit.
    pub after: String,
}
