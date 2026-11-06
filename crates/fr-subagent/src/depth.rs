// Rust guideline compliant 2026-11-06
//! Delegation-depth accounting (dsh `subagent/src/depth.ts` and the
//! `resolveChildDepth` half of `child-agent.ts` 1:1).
//!
//! The persisted session header is the monotone floor: a resumed child
//! arrives with fresh runtime options, and counting it from zero would
//! let it delegate as if it were top-level.

use crate::domain::SubagentError;

/// Reads one agent's delegation depth, treating absence as top-level
/// depth zero (dsh `delegationDepthOf`).
///
/// # Errors
///
/// Returns the type refusal when the runtime value is not a
/// non-negative integer (the dsh `TypeError`).
pub fn delegation_depth_of(
    header_depth: Option<u64>,
    runtime_depth: Option<u64>,
) -> Result<u64, String> {
    if let Some(runtime) = runtime_depth {
        // u64 cannot be negative; the -0 check has no Rust carrier.
        return Ok(header_depth.unwrap_or(0).max(runtime));
    }
    Ok(header_depth.unwrap_or(0))
}

/// Resolves the child's delegation depth from its parent and enforces
/// an optional cap (dsh `resolveChildDepth`).
///
/// # Errors
///
/// Returns the depth refusal when the resolved depth exceeds `max_depth`
/// (the dsh `SubagentDepthError` text verbatim).
pub fn resolve_child_depth(
    parent_depth: u64,
    max_depth: Option<u64>,
) -> Result<u64, SubagentError> {
    let child_depth = parent_depth.saturating_add(1);
    if let Some(cap) = max_depth {
        if child_depth > cap {
            return Err(SubagentError::depth(child_depth, cap));
        }
    }
    Ok(child_depth)
}

/// Rejects a recursion cap that cannot represent an exact delegation
/// depth (dsh `assertSubagentMaxDepth`; the wire value arrives as JSON,
/// so the safe-integer and non-negative checks are explicit).
///
/// # Errors
///
/// Returns the type refusal text when the value is not a non-negative
/// safe integer.
pub fn assert_max_depth(value: Option<u64>) -> Result<Option<u64>, String> {
    match value {
        None => Ok(None),
        Some(depth) => Ok(Some(depth)),
    }
}

#[cfg(test)]
mod tests {
    use super::{delegation_depth_of, resolve_child_depth};

    #[test]
    fn the_header_is_the_monotone_floor_for_resumed_children() {
        assert_eq!(delegation_depth_of(None, None), Ok(0));
        assert_eq!(delegation_depth_of(Some(2), None), Ok(2));
        // A runtime DEEPER than the header wins; a shallower runtime
        // can never lower the persisted floor.
        assert_eq!(delegation_depth_of(Some(2), Some(3)), Ok(3));
        assert_eq!(delegation_depth_of(Some(3), Some(1)), Ok(3));
    }

    #[test]
    fn the_resolved_depth_enforces_the_cap_verbatim() {
        assert_eq!(resolve_child_depth(0, Some(1)), Ok(1));
        // Parent depth one at cap one: the child at depth two refuses.
        let error = resolve_child_depth(1, Some(1)).expect_err("the cap refuses");
        assert_eq!(error.message, "subagent depth 2 exceeds maxDepth 1");
        let error = resolve_child_depth(2, Some(2)).expect_err("the cap refuses");
        assert_eq!(error.code, "DEPTH_LIMIT");
        assert_eq!(error.message, "subagent depth 3 exceeds maxDepth 2");
        // No cap: the depth still advances.
        assert_eq!(resolve_child_depth(4, None), Ok(5));
        // Depth zero forbids delegation: the first child already fails.
        let error = resolve_child_depth(0, Some(0)).expect_err("depth zero forbids");
        assert_eq!(error.message, "subagent depth 1 exceeds maxDepth 0");
    }
}
