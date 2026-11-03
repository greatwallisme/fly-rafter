//! The spill storage capability seam (the dsh `ctx.spillStore`; PRD 4.5
//! ships the family inside fr-compaction): persist oversized text and
//! return a model-facing locator plus retrieval guidance.
//!
//! The seam owns NO retention policy (the spill policy rides the tools
//! face) and NO retrieval API. Storage is scoped by the request's owner
//! session; the returned locator is the model-facing handle — consumers
//! render it with the retrieval hint but never parse it.

/// The model-facing handle for one spilled artifact (the dsh
/// `SpillLocator` brand; opaque — a local backend uses a filesystem
/// path, a remote backend a URI, and consumers never parse it).
pub type SpillLocator = String;

/// Save-time storage namespace for a spilled artifact (the dsh
/// `SpillOwner`): forked sessions inherit locators already in the
/// seeded log without re-owning them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpillOwner {
    /// The producing session id.
    pub session_id: String,
}

/// Producer of a spilled artifact (the dsh `SpillSource`); descriptive
/// only, never access control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpillSource {
    /// A tool result, with its model-issued call id.
    Tool {
        /// The tool whose result was spilled.
        tool_name: String,
        /// The model-issued call id the result belongs to.
        call_id: String,
        /// A short human label for the artifact.
        label: String,
    },
    /// A captured source session (the session-reference consumer).
    SessionReference {
        /// Session whose projected conversation was captured.
        session_id: String,
        /// Host-provided label for the referenced session.
        label: String,
    },
}

/// One request to persist text to a spill artifact (the dsh
/// `SaveTextSpill`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveTextSpill {
    /// The save-time storage namespace.
    pub owner: SpillOwner,
    /// The producer facts.
    pub source: SpillSource,
    /// A caller-suggested base name; the backend sanitizes it to one
    /// safe path segment — a hint, never a path.
    pub suggested_name: String,
    /// The full text to persist (UTF-8).
    pub content: String,
}

/// A saved spill artifact: locator, exact byte length, and retrieval
/// guidance (the dsh `SpillRef`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpillRef {
    /// The opaque model-facing handle.
    pub locator: SpillLocator,
    /// Exact UTF-8 byte length of the persisted content.
    pub bytes: u64,
    /// Model-facing retrieval guidance for the locator.
    pub retrieval_hint: String,
}

/// The abstract spill storage seam (the dsh `SpillStore` semantics):
/// `save_text` persists the full content verbatim, rejects on a real
/// storage failure, and never truncates or renames per caller intent.
pub trait SpillStore: Send + Sync {
    /// Persists `input.content` to a session-scoped spill artifact.
    ///
    /// # Errors
    ///
    /// Returns the storage failure diagnostic (permissions, no space,
    /// backend unavailable); the caller degrades best-effort.
    fn save_text(&self, input: &SaveTextSpill) -> Result<SpillRef, String>;
}
