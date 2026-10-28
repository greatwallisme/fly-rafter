//! The `'agent'` service face constants.
//!
//! Shell-free so dependent plugin artifacts can share the selectors
//! without pulling this crate's FFI export object into their link (the
//! rlib-pulls-by-object judgment, batch 2a).

/// The provided service name consumers call.
pub const SERVICE_NAME: &str = "agent";

/// Method selector: queue one input with full target and wake control
/// (`{sessionId, message, target, wakeup}`; dsh `send`).
pub const METHOD_SEND: u32 = 0;

/// Method selector: queue one steering input (`{sessionId, message}`;
/// target next-step, wakes the driver; dsh `steer`).
pub const METHOD_STEER: u32 = 1;

/// Method selector: queue one non-waking injection (`{sessionId,
/// message}`; target next-step, no wake; dsh `inject`).
pub const METHOD_INJECT: u32 = 2;

/// Method selector: queue one follow-up input (`{sessionId, message}`;
/// target next-turn, wakes the driver; dsh `followup`).
pub const METHOD_FOLLOWUP: u32 = 3;

/// Method selector: request cancellation (`{sessionId, cause,
/// keepInbox?}`; dsh `cancel`).
pub const METHOD_CANCEL: u32 = 4;

/// Method selector: shell metadata echo (`{}`).
pub const METHOD_DESCRIBE: u32 = 5;

/// Method selector: query the session's loop status
/// (`{sessionId}` → `{status}`).
pub const METHOD_STATUS: u32 = 6;

// Rust guideline compliant 2026-10-29
