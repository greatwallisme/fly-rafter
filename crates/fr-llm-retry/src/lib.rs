// Rust guideline compliant 2026-11-05
//! The provider-routed retry executor plugin (dsh `llm-retry` 1:1; PRD
//! decision 47-2A: an independent plugin on the loop's request recovery
//! extension point).
//!
//! Behavior split (the 18c Q1A two-stage carrier): this plugin *rules*
//! and makes the retry durable (`llm/retry` appended before any wait,
//! the dsh backoff's append-first discipline), replying immediately with
//! the fr-fabric decision `{"kind":"retry","delayMs":N,"retryId":..,
//! "retry":N}`; the loop thread runs the cancellable wait and appends
//! `llm/retry-started` after it. The durable surface is byte-identical to
//! dsh (`llm/retry` persists even when the wait is cancelled; a cancelled
//! wait never produces `llm/retry-started`), while cancellation stays
//! immediate because no kernel-bus listener ever sleeps inside its
//! foreign entry.
//!
//! Retry state rides an in-crate table keyed by
//! `(session, turn, step, provider, policyKey)` rebuilt by a cold read of
//! the session log on first sight of a session (the fr-compaction
//! in-crate session-table judgment; the dsh sessionProjections service is
//! not built until a second projection consumer arrives). The event data
//! carries every fold input, so the rebuild is the exact dsh fold over
//! restarts.
//!
//! Kernel-family edge: `plugin_shell` is the only `dougong-abi` consumer;
//! the rest of the crate stays a plain rlib.

pub mod core;
pub mod persist;
pub mod plugin_shell;
pub mod wire;

// The dylib export table lives in its own module (the 18b CGU
// co-location finding): the version script promotes the export symbol to
// a GC root in every cdylib link, so its codegen unit must never share
// an object with generic monomorphizations a dependent crate's rlib link
// could pull (fr-agent-loop consumes this crate's rlib for the wire
// vocabulary).
mod export;
