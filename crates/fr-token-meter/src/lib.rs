//! The replay token-meter plugin (dsh `packages/llm/token-meter` 1:1):
//! single replay-aware metering service for request and surface
//! pressure.
//!
//! Two faces over the dougong dynamic channel:
//!
//! - The `token-meter` service (PRD 4.5 row; PRD 19.2 entry 37
//!   erratum): `measure` replays a session's durable tail through the
//!   per-session fold and answers the [`types::TokenMeasurement`] the
//!   compaction trigger, selection, and shrink gates consume;
//!   `estimate-message` is the instance face of the pure estimator.
//! - Three projection units (`tokenUsage` stateVersion 2,
//!   `contextPressure` stateVersion 5, `contextBreakdown` stateVersion
//!   6) registered with the `session-projections` relay at activation;
//!   the fold endpoints run under the runtime grant (Q110′A′, the
//!   permission-presets precedent).
//!
//! Documented structurals (behavior-identical, not protocol or output
//! deltas):
//!
//! - dsh syncs measured sessions eagerly through a `session/event`
//!   listener; fr M1 has no such kernel event, so the fold catches up
//!   at `measure` time only. Every measurement replays through the
//!   current durable tail first, so the answers are byte-identical;
//!   only the (invisible) read latency of an already-measured session
//!   differs.
//! - dsh keys replay state by live `Session` object in a `WeakMap`
//!   (bounded by GC); fr keys by session id in an internally bounded
//!   LRU. Eviction only forces a from-scratch re-replay on the next
//!   measure of that session — answers stay identical.
//! - dsh takes `llm` as a soft dependency (`ctx.get`, absent → fixed
//!   heuristic); fr's declared-dependency model has no soft get, so the
//!   descriptor injects `llm` as a hard dependency. The base assembly
//!   always mounts it, so the dsh degraded arm is unreachable.

pub mod breakdown_projection;
pub mod estimate;
pub mod meter;
pub mod plugin_shell;
pub mod projection;
pub mod route_pricing;
pub mod surface_fold;
pub mod surface_projection;
pub mod turn_usage;
pub mod types;
pub mod usage_projection;
