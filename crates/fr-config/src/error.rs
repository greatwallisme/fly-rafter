//! Errors produced by configuration parsing and compilation.

/// Configuration domain errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A `fr.patch.yml` document failed to parse as a patch line sequence;
    /// the message is the full user-facing diagnostic (dsh throws carry the
    /// whole `${bin}: ...` line).
    #[error("{0}")]
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

    /// A file-system operation failed during layered loading; the message
    /// is the full user-facing diagnostic (dsh throws carry the whole
    /// `${bin}: ...` line).
    #[error("{0}")]
    Io(String),

    /// The profile layer is invalid: illegal name, unknown profile, or a
    /// malformed `profile.yml` manifest; the message is the full
    /// user-facing diagnostic.
    #[error("{0}")]
    Profile(String),

    /// A `.env` layer set a bootstrap-only name that only the launching
    /// environment may set; startup is refused (fail-closed, dsh
    /// `readEnvLayer` throw path). The message is the full user-facing
    /// diagnostic.
    #[error("{0}")]
    EnvBootstrap(String),
}

/// Convenience alias for configuration results.
pub type Result<T> = std::result::Result<T, Error>;
