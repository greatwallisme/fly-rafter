// Rust guideline compliant 2026-10-28
//! The Fly Rafter tool-system crate.
//!
//! Behavior parity target: dsh `core/tools` (schema DSL, enforced JSON
//! Schema subset, presentation vocabulary, registry, execution pipeline,
//! scheduling, and the `'tools'` service surface with the staged listener
//! registration face of decision Q101′A). Wire-facing keyword spellings
//! (`oneOf`, `additionalProperties`, ...) keep the dsh protocol shapes
//! verbatim; Rust identifiers stay `snake_case`.

pub mod definition;
pub mod json_schema;
pub mod plugin_shell;
pub mod presentation;
pub mod registry;
pub mod schema;
pub mod service;
pub mod signal;
pub mod types;

#[cfg(test)]
mod pipeline_tests;

#[cfg(test)]
mod service_tests;
