//! The JSONL persistence error vocabulary (dsh message-verbatim; the stable
//! seam errors the jsonl backend reports).

/// One persistence failure carrying its dsh-equivalent message text.
#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    /// The artifact uses a format generation this build refuses
    /// (`SessionFormatUnsupportedError` equivalence).
    #[error("{0}")]
    Unsupported(String),
    /// The artifact is damaged or fails structural validation
    /// (`SessionPersistenceCorruptionError` equivalence).
    #[error("{0}")]
    Corruption(String),
    /// The session id already exists (`SessionAlreadyExistsError`
    /// equivalence).
    #[error("{0}")]
    AlreadyExists(String),
    /// Another writer holds the session's write lease
    /// (`SessionAlreadyOwnedError` equivalence).
    #[error("{0}")]
    AlreadyOwned(String),
    /// The requested session has no stored log
    /// (`SessionPersistenceNotFoundError` equivalence).
    #[error("{0}")]
    NotFound(String),
    /// A filesystem error propagated unchanged (Node `ErrnoException`
    /// equivalence); callers classify `ENOENT` with [`Self::is_enoent`].
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl PersistError {
    /// Whether the payload message matches the dsh refusal text; used by
    /// tests and callers that assert exact wording.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Unsupported(message)
            | Self::Corruption(message)
            | Self::AlreadyExists(message)
            | Self::AlreadyOwned(message)
            | Self::NotFound(message) => message,
            Self::Io(_) => "",
        }
    }

    /// Whether the carried I/O failure is `ENOENT` (dsh `isENOENT`).
    #[must_use]
    pub fn is_enoent(&self) -> bool {
        matches!(self, Self::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
    }
}

/// Builds the dsh `sessionFormatVersionRefusal` text for a foreign or older
/// log format (dsh session-persistence errors.ts:133-137).
#[must_use]
pub fn session_format_version_refusal(id: &str, version: u64) -> String {
    if version > crate::persist::SESSION_FORMAT_VERSION {
        format!(
            "session \"{id}\" uses log format v{version}, but this harness reads only v{}: \
the log was written by a newer harness — upgrade the harness to open it",
            crate::persist::SESSION_FORMAT_VERSION
        )
    } else {
        format!(
            "session \"{id}\" uses log format v{version}, older than the supported v{}, \
and this build ships no upgrade path for it",
            crate::persist::SESSION_FORMAT_VERSION
        )
    }
}

// Rust guideline compliant 2026-10-23
