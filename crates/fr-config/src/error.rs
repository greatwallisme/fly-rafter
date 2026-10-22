//! Errors produced by configuration parsing and compilation.

/// Configuration domain errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A `fr.patch.yml` document failed to parse as a patch line sequence.
    #[error("patch parse error: {0}")]
    PatchParse(String),

    /// A patch row violates the row schema (unknown key, missing id on an
    /// inserted row, insert group carrying entry keys, config that is neither
    /// object nor null, expression outside the restricted subset).
    #[error("invalid patch row {row_no}: {message}")]
    PatchLine {
        /// 1-based patch row index within the document.
        row_no: usize,
        /// Human-readable rule violation.
        message: String,
    },
}

/// Convenience alias for configuration results.
pub type Result<T> = std::result::Result<T, Error>;
