//! Model-visible session text constants (GENERATED FILE — do not edit by
//! hand). Regenerate with `python3 scripts/extract-session-texts.py`; the
//! source of truth is the dsh `repair.ts` `CLOSER_TEXT` block.
//!
//! These texts reach the model as synthetic tool-result content, so they are
//! extracted verbatim from the dsh source per the PRD 1:1 wording rule.

#![forbid(unsafe_code)]
// Rust guideline compliant 2026-10-23

/// `CLOSER_TEXT.interrupted.started`, verbatim from dsh repair.ts.
pub const INTERRUPTED_STARTED: &str = "The tool call was interrupted after it was recorded, but no result was durably recorded. Its outcome is unknown. Decide whether to retry from the tool semantics: retry only if the operation is read-only or idempotent; if it may have side effects, first verify external state or ask the user. Do not retry blindly.";

/// `CLOSER_TEXT.interrupted.notStarted`, verbatim from dsh repair.ts.
pub const INTERRUPTED_NOT_STARTED: &str = "The tool call was interrupted before the Harness recorded it as started. Retry it if it is still needed.";

/// `CLOSER_TEXT.forked.started`, verbatim from dsh repair.ts.
pub const FORKED_STARTED: &str = "The history inherited by this branch records this tool call starting but does not include its result. The parent session may have completed it after the fork point. Decide whether to retry from the tool semantics: retry only if the operation is read-only or idempotent; if it may have side effects, first verify external state or ask the user. Do not retry blindly.";

/// `CLOSER_TEXT.forked.notStarted`, verbatim from dsh repair.ts.
pub const FORKED_NOT_STARTED: &str = "The history inherited by this branch has no record of this tool call starting. The parent session may have executed it after the fork point. Decide whether to retry from the tool semantics: retry only if the operation is read-only or idempotent; if it may have side effects, first verify external state or ask the user. Do not retry blindly.";

/// The dsh `TOOL_NOT_STARTED` recovery code, verbatim.
pub const TOOL_NOT_STARTED: &str = "TOOL_NOT_STARTED";

/// The dsh `TOOL_OUTCOME_UNKNOWN` recovery code, verbatim.
pub const TOOL_OUTCOME_UNKNOWN: &str = "TOOL_OUTCOME_UNKNOWN";
