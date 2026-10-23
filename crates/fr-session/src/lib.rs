//! The session log subsystem: append-side admission, the ordered surface
//! fold with its dual generation counters, derived LLM history, tool
//! history projection, and the event vocabulary aggregation (PRD 7).
//!
//! Wire shapes live in `fr-wire`; this crate owns every behavioral
//! invariant the dsh session package enforces on typed events.

pub mod admission;
pub mod closer_text;
pub mod error;
pub mod fork;
pub mod persist;
pub mod plugin_shell;
pub mod repair;
pub mod session;
pub mod surface;
pub mod tool_history;
pub mod vocabulary;

#[cfg(test)]
pub(crate) mod tests_support;

pub use admission::{is_plugin_record_type, validate_event_data};
pub use error::{AdmissionError, SessionError};
pub use session::{Session, SurfaceIntent, canonical_header};
pub use surface::{SurfaceFoldResult, SurfaceReplacement, fold_surface};
pub use tool_history::ToolHistoryProjection;
pub use vocabulary::{EventVocabulary, refuse_unknown_required};

// Rust guideline compliant 2026-10-23
