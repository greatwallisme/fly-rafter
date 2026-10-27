// Rust guideline compliant 2026-10-28
//! fr-agent-loop: the dougong plugin that drives agent sessions.
//!
//! One plugin instance maintains one loop driver per session — a dedicated
//! OS thread running the natural blocking control flow (PRD 8.1 as amended
//! by 19.2 item 24, decision 46). Inbound agent-service entries (`send`,
//! `steer`, `inject`, `followup`, `cancel`) enqueue and wake the driver;
//! outbound request/response traffic crosses dougong ABI v3 `require`; LLM
//! streams arrive as `llm/chunk-ready` notifications drained in batches
//! (Q80-B / Q84-B2); agent events publish through `emit`.
//!
//! M1 slice 7 scope: the durable inbox fold and planner, the turn-boundary
//! and runtime-context projections, the assistant-stream attempt frame, and
//! the driver body running against test doubles for the tools and
//! system-prompt seams (Q81-A). Real services attach in Phases 14/15.

pub mod agent_events;
pub mod assistant_stream;
pub mod driver;
pub mod inbox;
pub mod llm_client;
pub mod loop_inbox;
pub mod outbound;
pub mod phase;
pub mod plugin_shell;
pub mod runtime_context;
pub mod session_client;
pub mod turn_boundary;
