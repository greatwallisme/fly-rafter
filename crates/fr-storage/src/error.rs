//! The error vocabulary for the storage unit backend and the domain layer
//! (the dsh `StorageError` plus `DomainError` codes, one fr error type).

#![forbid(unsafe_code)]

use std::fmt;

/// Discriminant codes carried by every [`FrStorageError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrStorageErrorCode {
    /// The stored unit version differs from the descriptor's.
    VersionMismatch,
    /// The medium cannot be parsed as this unit.
    MalformedMedium,
    /// The unit or domain was closed.
    Closed,
    /// The domain name is already open on this facility.
    AlreadyOpen,
    /// An `update` named a record the table does not hold.
    MissingKey,
    /// A stored record failed its owner's schema at the durable boundary.
    InvalidRecord,
}

impl FrStorageErrorCode {
    /// The stable wire-facing discriminant (the dsh code strings verbatim).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::VersionMismatch => "version-mismatch",
            Self::MalformedMedium => "malformed-medium",
            Self::Closed => "closed",
            Self::AlreadyOpen => "already-open",
            Self::MissingKey => "missing-key",
            Self::InvalidRecord => "invalid-record",
        }
    }
}

impl fmt::Display for FrStorageErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error from the storage unit backend or the domain layer. The `code` is the
/// stable contract consumers may switch on; `message` is diagnostic prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrStorageError {
    /// Stable discriminant for the failure class.
    pub code: FrStorageErrorCode,
    /// Human-readable diagnostic detail.
    pub message: String,
}

impl FrStorageError {
    /// Builds one error from a code and diagnostic prose.
    #[must_use]
    pub fn new(code: FrStorageErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Convenience constructor for [`FrStorageErrorCode::MalformedMedium`].
    #[must_use]
    pub fn malformed_medium(message: impl Into<String>) -> Self {
        Self::new(FrStorageErrorCode::MalformedMedium, message)
    }

    /// Convenience constructor for [`FrStorageErrorCode::Closed`].
    #[must_use]
    pub fn closed(message: impl Into<String>) -> Self {
        Self::new(FrStorageErrorCode::Closed, message)
    }
}

impl fmt::Display for FrStorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for FrStorageError {}
