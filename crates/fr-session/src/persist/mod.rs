//! The JSONL+zstd persistence physical layer (PRD 7.2; dsh
//! session-persistence-jsonl + session-format, v4 only — the v0-v3
//! migration chain is excluded by the PRD scope red line).
//!
//! Slices: [`filename`] and [`paths`] assemble artifact locations;
//! [`header_line`] and [`framing`] guard the physical records; [`v4_checks`]
//! admits raw rows and validates the released artifact; [`zstd`] owns the
//! concatenated-frame container; [`lease`] arbitrates single-writer access;
//! [`io`] materializes and appends; [`read`] decodes stored logs.

pub mod error;
pub mod filename;
pub mod framing;
pub mod header_line;
pub mod io;
pub mod lease;
pub mod paths;
pub mod read;
pub mod v4_checks;
pub mod zstd;

pub use error::{PersistError, session_format_version_refusal};

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
