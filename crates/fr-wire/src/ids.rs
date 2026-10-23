//! Cross-boundary identifier and sequence newtypes (dsh brand vocabulary).
//!
//! All ids are opaque branded strings on the dsh side; the Rust newtypes are
//! `serde(transparent)` so the wire carries the plain string. Sequence values
//! keep the dsh non-negative safe-integer cap so a value accepted here is
//! exactly a value a JS reader accepts.

use serde::{Deserialize, Serialize};

/// Upper bound shared by every dsh safe-integer (2^53 - 1).
pub const SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;

/// Macro to cut boilerplate for opaque string ids.
macro_rules! opaque_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// Wraps `raw` without further validation, mirroring the dsh
            /// brand constructors that perform no validation.
            #[must_use]
            pub fn new(raw: impl Into<String>) -> Self {
                Self(raw.into())
            }

            /// The opaque id text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

opaque_id!(
    /// Stable identity of one session in the store and its persistence artifacts.
    SessionId
);
opaque_id!(
    /// Stable identity carried by one message across inbox, log, and request boundaries.
    MessageId
);
opaque_id!(
    /// Correlates a model-issued tool call with its result; provider-issued or synthesized.
    ToolCallId
);
opaque_id!(
    /// Provider-issued request identifier retained for diagnostics.
    ProviderRequestId
);
opaque_id!(
    /// Adapter-owned identifier for one model's selectable reasoning effort.
    ReasoningEffortId
);
opaque_id!(
    /// Identity of one model streaming attempt, unique within one agent lifecycle.
    LlmAttemptId
);
opaque_id!(
    /// Opaque content-addressed identifier of one stored attachment.
    AttachmentId
);

/// A sequence number of one existing event in a session log.
///
/// Non-negative safe integer, mirroring the dsh `SessionSeq` admission
/// (non-negative, safe-integer, `-0` impossible on the wire as `0`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionSeq(u64);

impl SessionSeq {
    /// Admits `value` when it is a non-negative safe integer.
    ///
    /// # Errors
    ///
    /// Returns the value unchanged as an error payload when it exceeds
    /// [`SAFE_INTEGER_MAX`].
    pub const fn new(value: u64) -> Result<Self, SessionSeqRangeError> {
        if value > SAFE_INTEGER_MAX {
            return Err(SessionSeqRangeError(value));
        }
        Ok(Self(value))
    }

    /// The sequence value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Error payload for a sequence value beyond the safe-integer cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionSeqRangeError(pub u64);

impl std::fmt::Display for SessionSeqRangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SessionSeq must be a non-negative safe integer, got {}",
            self.0
        )
    }
}

impl std::error::Error for SessionSeqRangeError {}

/// A session log gap, prefix length, or read offset, which may equal the event count.
///
/// Same admission rules as [`SessionSeq`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionLogOffset(u64);

impl SessionLogOffset {
    /// Admits `value` when it is a non-negative safe integer.
    ///
    /// # Errors
    ///
    /// Returns the value as an error payload when it exceeds
    /// [`SAFE_INTEGER_MAX`].
    pub const fn new(value: u64) -> Result<Self, SessionSeqRangeError> {
        if value > SAFE_INTEGER_MAX {
            return Err(SessionSeqRangeError(value));
        }
        Ok(Self(value))
    }

    /// The offset value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_ids_round_trip_verbatim() {
        let id = MessageId::new("m-1");
        assert_eq!(serde_json::to_string(&id).unwrap_or_default(), "\"m-1\"");
        let back: MessageId =
            serde_json::from_str("\"m-1\"").unwrap_or_else(|_| MessageId::new(""));
        assert_eq!(back, id);
    }

    #[test]
    fn session_seq_admits_safe_integers_and_rejects_beyond() {
        assert_eq!(SessionSeq::new(0).map(SessionSeq::get), Ok(0));
        assert_eq!(
            SessionSeq::new(SAFE_INTEGER_MAX).map(SessionSeq::get),
            Ok(SAFE_INTEGER_MAX)
        );
        let SessionSeqRangeError(v) =
            SessionSeq::new(SAFE_INTEGER_MAX + 1).expect_err("beyond the safe-integer cap");
        assert_eq!(v, SAFE_INTEGER_MAX + 1);
    }

    #[test]
    fn session_seq_wire_is_plain_number() {
        let seq = SessionSeq::new(7).unwrap_or_else(|err| match err {
            SessionSeqRangeError(v) => panic!("unexpected rejection of {v}"),
        });
        let json = serde_json::to_string(&seq).unwrap_or_default();
        assert_eq!(json, "7");
    }
}
