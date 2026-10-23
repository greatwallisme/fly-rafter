//! The JSONL+zstd persistence physical layer (PRD 7.2; dsh
//! session-persistence-jsonl + session-format, v4 only — the v0-v3
//! migration chain is excluded by the PRD scope red line).
//!
//! Slices: [`filename`] and [`paths`] assemble artifact locations;
//! [`header_line`] and [`framing`] guard the physical records; [`v4_checks`]
//! admits raw rows and validates the released artifact; [`zstd`] owns the
//! concatenated-frame container; [`lease`] arbitrates single-writer access;
//! [`io`] materializes and appends; [`read`] decodes stored logs.

// Q39A (PRD 19.2 item 15 / decision 29): the persistence physical layer is
// POSIX-only in M1 — the dsh win32 lease semaphore and durable-publish path
// port verbatim with the M1 platform gate, never silently absent.
#[cfg(windows)]
compile_error!(
    "fr-session persistence is POSIX-only in M1; the win32 lease and publish paths land with the M1 platform gate (PRD 19.2 item 15)"
);

pub mod backend;
pub mod error;
pub mod filename;
pub mod framing;
pub mod handle;
pub mod header_line;
pub mod io;
pub mod lease;
pub mod paths;
pub mod read;
pub mod scheduler;
pub(crate) mod tracker;
pub mod v4_checks;
pub mod zstd;

pub use backend::{JsonlBackend, JsonlBackendBuilder, SessionSnapshot};
pub use error::{PersistError, session_format_version_refusal};
pub use handle::{HandleRead, JsonlSessionHandle, SessionAccess};

/// The current session format generation this build writes and reads.
pub const SESSION_FORMAT_VERSION: u64 = 4;

/// The physical encoding selected for JSONL session artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JsonlCompression {
    /// Zstandard frames with checksums (the dsh default).
    #[default]
    Zstd,
    /// Plaintext JSONL.
    None,
}

/// The dsh `encodingMismatch` refusal: a session directory holds the other
/// physical encoding's log.
#[must_use]
pub fn encoding_mismatch(path: &std::path::Path, compression: JsonlCompression) -> PersistError {
    let opposite_suffix = filename::log_suffix(match compression {
        JsonlCompression::Zstd => JsonlCompression::None,
        JsonlCompression::None => JsonlCompression::Zstd,
    });
    PersistError::Corruption(format!(
        "session artifact {} uses {opposite_suffix}, but this backend is configured for \
compression {}; use a separate root or select the matching compression mode",
        serde_json::to_string(&path.display().to_string()).unwrap_or_default(),
        serde_json::to_string(match compression {
            JsonlCompression::Zstd => "zstd",
            JsonlCompression::None => "none",
        })
        .unwrap_or_default()
    ))
}
