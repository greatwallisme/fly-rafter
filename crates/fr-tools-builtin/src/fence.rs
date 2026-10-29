// Rust guideline compliant 2026-10-29
//! The per-call policy fence on the two fs mutations (dsh
//! `fs-sandbox` port, module-for-module: the sandboxing backend's
//! `checkedTarget` plus its containment mechanics).
//!
//! The fence is a policy check in trusted code over a model-controlled
//! path, NOT a kernel boundary — the operations are the seam's own
//! (open, rename), and only the target path is untrusted, so
//! canonicalize-then-contain is the complete answer to this surface.
//! `read-only` denies every mutation; `workspace-write` re-canonicalizes
//! NOW and requires containment under a writable root; and
//! `danger-full-access` passes the caller's target unfenced. A denial
//! carries the structured `FS_SANDBOX_DENIED` code and the verbatim
//! per-mode refusal text.

use serde_json::Value;

use crate::fs_types::FsError;
use crate::fs_types::FsErrorCode;
use crate::fs_types::FsTarget;
use crate::fsio;

/// Whether a canonical target is a writable root or lies beneath it
/// (the dsh `isPathUnder`).
///
/// The lexical fast path handles normal canonical spellings; when
/// spellings differ, the canonical-form comparison recognizes
/// alias-equivalent roots without weakening containment to a textual
/// approximation (the fr canonical derivation is the shared fr-sandbox
/// roots home, the dsh `writableRoots` co-derivation judgment).
#[must_use]
pub fn is_path_under(path: &str, root: &str) -> bool {
    if path == root {
        return true;
    }
    let root_prefix = format!("{root}/");
    path.starts_with(&root_prefix)
}

/// Enforce the per-call policy against `target` and return the exact
/// target the mutation must use, so the checked identity is the
/// mutated one (no check-here-write-there TOCTOU; the dsh
/// `checkedTarget`).
///
/// # Errors
///
/// The two per-mode refusals (`read-only` / `workspace-write`) and a
/// missing standing policy.
pub fn checked_target(target: &FsTarget, policy: Option<&Value>) -> Result<FsTarget, FsError> {
    let policy = policy.ok_or_else(|| {
        FsError::new(
            FsErrorCode::SandboxDenied,
            "the mounted filesystem confines but no per-call policy resolved",
        )
    })?;
    let mode = policy
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if mode == "danger-full-access" {
        return Ok(target.clone());
    }
    if mode == "read-only" {
        return Err(FsError::new(
            FsErrorCode::SandboxDenied,
            format!(
                "cannot write \"{}\": file access denied under read-only mode",
                target.display_path
            ),
        ));
    }
    // workspace-write: containment on the FRESH canonical path (catches
    // a symlink ancestor swapped since the tool resolved this target),
    // and the mutation delegates with THIS fresh target — never the
    // stale one.
    let policy_root = policy
        .get("workspace_root")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let fresh = fsio::resolve_local_target(policy_root, &target.display_path)?;
    let parsed_mode = fr_sandbox::seam::SandboxMode::parse(mode).ok_or_else(|| {
        FsError::new(
            FsErrorCode::IoError,
            "the standing policy mode is not a sandbox mode",
        )
    })?;
    for root in fr_sandbox::roots::writable_roots(parsed_mode, policy_root) {
        if is_path_under(&fresh.target_key, &root) {
            return Ok(FsTarget {
                target_key: fresh.target_key,
                display_path: target.display_path.clone(),
            });
        }
    }
    Err(FsError::new(
        FsErrorCode::SandboxDenied,
        format!(
            "cannot write \"{}\": file access denied under workspace-write mode",
            target.display_path
        ),
    ))
}
