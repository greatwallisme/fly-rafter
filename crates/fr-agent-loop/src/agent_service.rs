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
/// (`{sessionId}` → `{status, provider?, model?}` — the additive route
/// fields are the dsh summarizer's `agent.options` fallback).
pub const METHOD_STATUS: u32 = 6;

/// Method selector: open the remote maintenance window
/// (`{sessionId}` → `{token}`; the dsh `runMaintenance` busy check and
/// phase admission).
///
/// The window splits across three faces because the dsh job closure
/// cannot cross the wire and a callback endpoint would re-enter the
/// caller's instance.
pub const METHOD_MAINTENANCE_BEGIN: u32 = 7;

/// Method selector: read the window's abort flag
/// (`{sessionId, token}` → `{aborted}`; the dsh job's `AbortSignal`).
pub const METHOD_MAINTENANCE_POLL: u32 = 8;

/// Method selector: close the window and converge
/// (`{sessionId, token}` → `{}`; the dsh `finally` idle commit plus the
/// wake replay).
pub const METHOD_MAINTENANCE_END: u32 = 9;

/// Method selector: create the session and spawn its driver thread
/// (`{sessionId, cwd, provider?, model?, reasoningEffort?, maxTokens?}`
/// → `{sessionId}`; the dsh `agents.create`; Q9A route projection).
pub const METHOD_CREATE: u32 = 10;

/// Method selector: adopt one persisted session and spawn its driver
/// over the restored log (the dsh `agents.resume`).
///
/// `{sessionId, provider?, model?, reasoningEffort?, maxTokens?}` →
/// `{sessionId}`; Q9A route projection.
pub const METHOD_RESUME: u32 = 11;

/// Method selector: liveness probe (`{sessionId}` → `{live}`); never
/// creates the session mailbox (the dsh `agents.get` liveness read).
pub const METHOD_LIVE: u32 = 12;

/// Method selector: realm-spawned child handshake (`{"role":"child"}` →
/// `{adopted: true}`).
///
/// The parent calls it on its realm-spawned child before routing the
/// session lifecycle into the realm; the first ADOPT adopts the `Child`
/// role on a role-less activation (the 18e `fiber_realm` ruling).
pub const METHOD_REALM_ADOPT: u32 = 13;

#[cfg(test)]
mod tests {
    /// The local face pins the shared protocol table selector-for-selector
    /// (the batch-2a placement judgment: dependent artifacts share the
    /// constants through fr-wire; the owner pins its own spellings).
    #[test]
    fn face_matches_the_shared_protocol_table() {
        use super::{
            METHOD_CANCEL, METHOD_CREATE, METHOD_DESCRIBE, METHOD_FOLLOWUP, METHOD_INJECT,
            METHOD_LIVE, METHOD_MAINTENANCE_BEGIN, METHOD_MAINTENANCE_END, METHOD_MAINTENANCE_POLL,
            METHOD_REALM_ADOPT, METHOD_RESUME, METHOD_SEND, METHOD_STATUS, METHOD_STEER,
            SERVICE_NAME,
        };
        use fr_wire::services::agent::{
            METHOD_CANCEL as W_CANCEL, METHOD_CREATE as W_CREATE, METHOD_DESCRIBE as W_DESCRIBE,
            METHOD_FOLLOWUP as W_FOLLOWUP, METHOD_INJECT as W_INJECT, METHOD_LIVE as W_LIVE,
            METHOD_MAINTENANCE_BEGIN as W_BEGIN, METHOD_MAINTENANCE_END as W_END,
            METHOD_MAINTENANCE_POLL as W_POLL, METHOD_REALM_ADOPT as W_REALM_ADOPT,
            METHOD_RESUME as W_RESUME, METHOD_SEND as W_SEND, METHOD_STATUS as W_STATUS,
            METHOD_STEER as W_STEER, SERVICE_NAME as W_SERVICE,
        };
        assert_eq!(SERVICE_NAME, W_SERVICE);
        assert_eq!(METHOD_SEND, W_SEND);
        assert_eq!(METHOD_STEER, W_STEER);
        assert_eq!(METHOD_INJECT, W_INJECT);
        assert_eq!(METHOD_FOLLOWUP, W_FOLLOWUP);
        assert_eq!(METHOD_CANCEL, W_CANCEL);
        assert_eq!(METHOD_DESCRIBE, W_DESCRIBE);
        assert_eq!(METHOD_STATUS, W_STATUS);
        assert_eq!(METHOD_MAINTENANCE_BEGIN, W_BEGIN);
        assert_eq!(METHOD_MAINTENANCE_POLL, W_POLL);
        assert_eq!(METHOD_MAINTENANCE_END, W_END);
        assert_eq!(METHOD_CREATE, W_CREATE);
        assert_eq!(METHOD_RESUME, W_RESUME);
        assert_eq!(METHOD_LIVE, W_LIVE);
        assert_eq!(METHOD_REALM_ADOPT, W_REALM_ADOPT);
    }
}

// Rust guideline compliant 2026-10-29
