// Rust guideline compliant 2026-11-05
//! The working-directory context plugin (dsh `session/working-directory`
//! 1:1; PRD decision 47-3A: an independent plugin owning each session's
//! committed directory and its model-visible changes).
//!
//! Behavior split (the 16f carrier, rehomed by 18d): the domain
//! (projection fold, per-session serialized `ensure`/`set`, the
//! recovered/changed notice pair, and the runtime-context text) lives
//! in [`core`]; the wire vocabulary (service name, method selectors,
//! the change event type) is single-sourced in [`wire`] —
//! fr-tools-builtin consumes this crate's rlib for the service face
//! constants its ensure/set seams require (the `tools_client`
//! judgment); the copied-vtable require seam and the three domain
//! seams' wire implementations live in [`outbound`]; [`plugin_shell`]
//! is the only `dougong-abi` consumer.
//!
//! Kernel-family edge: the dylib export table lives in its own module
//! (the 18b CGU co-location finding); `plugin_shell` is the only
//! `dougong-abi` consumer; the rest of the crate stays a plain rlib.

pub mod core;
pub mod outbound;
pub mod plugin_shell;
pub mod wire;

// The dylib export table lives in its own module (the 18b CGU
// co-location finding): the version script promotes the export symbol to
// a GC root in every cdylib link, so its codegen unit must never share
// an object with generic monomorphizations a dependent crate's rlib link
// could pull (fr-tools-builtin consumes this crate's rlib for the wire
// vocabulary).
mod export;
