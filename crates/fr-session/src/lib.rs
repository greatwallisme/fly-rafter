//! The session log subsystem: append-side admission, the ordered surface
//! fold with its dual generation counters, derived LLM history, tool
//! history projection, and the event vocabulary aggregation (PRD 7).
//!
//! Wire shapes live in `fr-wire`; this crate owns every behavioral
//! invariant the dsh session package enforces on typed events.

#![forbid(unsafe_code)]

pub mod admission;
pub mod error;
#[cfg(test)]
pub(crate) mod tests_support;

pub mod surface;
pub mod tool_history;

pub use admission::validate_event_data;
pub use error::AdmissionError;
pub use surface::{SurfaceFoldResult, SurfaceReplacement, fold_surface};
pub use tool_history::ToolHistoryProjection;

// Rust guideline compliant 2026-10-23
