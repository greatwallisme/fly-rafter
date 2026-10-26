//! Provider-specific stable error codes (dsh `llm-deepseek`).
//!
//! Shared codes (`AUTH`/`QUOTA`/`RATE_LIMIT`/`SERVER`/
//! `CONTEXT_WINDOW_EXCEEDED`, …) come from `fr_llm::error`; these stay
//! crate-local because the retry plugin's default retryable set
//! deliberately excludes provider-specific codes.

/// `INVALID_REQUEST`: provider rejected the request shape (400/413 family).
pub const INVALID_REQUEST_CODE: &str = "INVALID_REQUEST";
/// `MALFORMED_RESPONSE`: stream framing or JSON shape is unusable.
pub const MALFORMED_RESPONSE_CODE: &str = "MALFORMED_RESPONSE";
/// `INVALID_RESPONSE`: 2xx payload failed post-hoc validation.
pub const INVALID_RESPONSE_CODE: &str = "INVALID_RESPONSE";
/// `STREAM_CLOSED`: stream ended before `message_stop`.
pub const STREAM_CLOSED_CODE: &str = "STREAM_CLOSED";
/// `UNSUPPORTED_CONTENT`: history carries blocks this protocol cannot send.
pub const UNSUPPORTED_CONTENT_CODE: &str = "UNSUPPORTED_CONTENT";
/// `INVALID_REPLAY_STATE`: durable replay metadata failed validation.
pub const INVALID_REPLAY_STATE_CODE: &str = "INVALID_REPLAY_STATE";
/// `UNSUPPORTED_REASONING_EFFORT`: the resolved effort is outside the
/// protocol's set, or reasoning is disabled while an effort is requested.
pub const UNSUPPORTED_REASONING_EFFORT_CODE: &str = "UNSUPPORTED_REASONING_EFFORT";

/// `MISSING_CREDENTIAL`: the route's credential reference resolves to
/// nothing (dsh `MISSING_CREDENTIAL` in the shared taxonomy).
pub const MISSING_CREDENTIAL_CODE: &str = "MISSING_CREDENTIAL";
