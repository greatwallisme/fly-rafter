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
    /// A mutation was refused on a read handle (`SessionReadOnlyError`
    /// equivalence).
    #[error("{0}")]
    ReadOnly(String),
    /// An operation hit a handle after `close` began
    /// (`SessionHandleClosedError` equivalence).
    #[error("{0}")]
    HandleClosed(String),
    /// Caller input failed a check the dsh seam reports as `TypeError`
    /// or a plain validation `Error`.
    #[error("{0}")]
    InvalidInput(String),
    /// Several failures aggregated under one dsh `AggregateError`; the
    /// display text is the aggregate message, the causes stay inspectable.
    #[error("{message}")]
    Aggregate {
        /// The dsh aggregate message.
        message: String,
        /// The aggregated failures, in collection order.
        errors: Vec<Self>,
    },
    /// A filesystem error propagated unchanged (Node `ErrnoException`
    /// equivalence); callers classify `ENOENT` with [`Self::is_enoent`].
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Clone for PersistError {
    fn clone(&self) -> Self {
        match self {
            Self::Unsupported(message) => Self::Unsupported(message.clone()),
            Self::Corruption(message) => Self::Corruption(message.clone()),
            Self::AlreadyExists(message) => Self::AlreadyExists(message.clone()),
            Self::AlreadyOwned(message) => Self::AlreadyOwned(message.clone()),
            Self::NotFound(message) => Self::NotFound(message.clone()),
            Self::ReadOnly(message) => Self::ReadOnly(message.clone()),
            Self::HandleClosed(message) => Self::HandleClosed(message.clone()),
            Self::InvalidInput(message) => Self::InvalidInput(message.clone()),
            Self::Aggregate { message, errors } => Self::Aggregate {
                message: message.clone(),
                errors: errors.clone(),
            },
            // `std::io::Error` is not `Clone`; the reconstruction keeps the
            // error kind (callers classify ENOENT via `is_enoent`) and the
            // message text.
            Self::Io(error) => Self::Io(std::io::Error::new(error.kind(), error.to_string())),
        }
    }
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
            | Self::NotFound(message)
            | Self::ReadOnly(message)
            | Self::HandleClosed(message)
            | Self::InvalidInput(message)
            | Self::Aggregate { message, .. } => message,
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
