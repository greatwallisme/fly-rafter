// Rust guideline compliant 2026-11-06
//! The subagent plugin (dsh `subagent` package family 1:1; PRD 4.5:
//! `fr-subagent` owns the in-process spawn/fork children, the
//! continuation manager over the `subagents` service, and the six
//! model-facing tool adapters — `subagent`, `subagent_fork`,
//! `send_message`, `interrupt_agent`, `list_agents`, and
//! `list_subagent_models`).
//!
//! Behavior split (the 18d carrier): the pure domain pieces (the
//! descriptor fold, the catalog chunked list, the settlement wording,
//! the depth budget, the delegation policy capture, the result fold)
//! live in the flat modules; the [`manager`] owns activation residency,
//! input admission, cold resume, and settlement delivery; [`runtime`]
//! is the service face over the manager; [`tools`] are the six
//! model-facing adapters; [`structured`] is the scoped structured-output
//! runtime (the D1-B ruling: full 1:1 port on the minimal scoped layer).
//! The wire vocabulary (service name, method selectors, the durable
//! event types) is single-sourced in [`wire`]; the copied-vtable
//! require/emit seam and the domain seams' wire implementations live in
//! [`outbound`]; [`plugin_shell`] is the only `dougong-abi` consumer.
//!
//! Kernel-family edge: the dylib export table lives in its own module
//! (the 18b CGU co-location finding); `plugin_shell` is the only
//! `dougong-abi` consumer; the rest of the crate stays a plain rlib.

pub mod assistant_output;
pub mod catalog;
pub mod control;
pub mod depth;
pub mod descriptor;
pub mod domain;
pub mod manager;
pub mod outbound;
pub mod plugin_shell;
pub mod projection;
pub mod runtime;
pub mod schema;
pub mod structured;
pub mod tools;
pub mod wire;

// The dylib export table lives in its own module (the 18b CGU
// co-location finding): the version script promotes the export symbol to
// a GC root in every cdylib link, so its codegen unit must never share
// an object with generic monomorphizations a dependent crate's rlib link
// could pull.
mod export;
