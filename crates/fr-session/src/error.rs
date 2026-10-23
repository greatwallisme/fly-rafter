//! Session admission and surface errors (dsh message-verbatim where the
//! source pins exact text).

/// One admission failure with its dsh-equivalent message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct AdmissionError {
    /// The failure text, matching the dsh check that owns the invariant.
    pub message: String,
}

impl AdmissionError {
    /// Builds an admission error from the dsh message text.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
