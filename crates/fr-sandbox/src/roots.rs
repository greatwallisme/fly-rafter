// Rust guideline compliant 2026-10-29
//! The writable-root derivation shared by every enforcement dialect that
//! expresses a mode as a canonical allow-list (dsh `sandbox/src/roots.ts`).
//!
//! `workspace-write` means "the workspace root plus the platform temp
//! areas", and this module is that meaning's one home. The Seatbelt
//! profile and the in-process filesystem fence both derive their
//! allow-list here (the Phase 16 fs fence consumes this function), so
//! "the write tool cannot write /tmp but bash can" asymmetries cannot
//! arise between them. The bwrap and Landlock dialects keep their own
//! grant spellings (an ephemeral `/tmp` mount, launcher-owned flags) —
//! the honest per-runner differences recorded in the sandbox RFC — with
//! parity pinned by test.

use crate::seam::SandboxMode;

/// Resolves a granted root to the path the enforcement layer actually
/// compares (dsh `canonicalPath`): canonical (symlinks resolved).
///
/// Both Seatbelt filters and the fs fence's containment check match
/// resolved paths — `/tmp` IS `/private/tmp` on darwin, and an
/// as-spelled grant would match nothing.
///
/// Falls back to the spelling as-is when resolution fails (a missing
/// root matches nothing until it exists — the conservative outcome;
/// inventing a fallback would grant a path the caller never named).
#[must_use]
pub fn canonical_path(path: &str) -> String {
    std::fs::canonicalize(path).map_or_else(
        |_| path.to_owned(),
        |resolved| {
            resolved
                .into_os_string()
                .into_string()
                .unwrap_or_else(|_| path.to_owned())
        },
    )
}

/// The roots one confined execution may WRITE under (dsh `writableRoots`): the mode's meaning as a canonical, deduplicated allow-list.
///
/// `read-only` allows nothing; `workspace-write` allows the
/// policy's workspace root, the host `/tmp`, and the per-user platform
/// temp dir (the real temp area for mkstemp-family tools; omitting it
/// would deny what the mode promises).
#[must_use]
pub fn writable_roots(mode: SandboxMode, workspace_root: &str) -> Vec<String> {
    if mode != SandboxMode::WorkspaceWrite {
        return Vec::new();
    }
    let tempdir = std::env::temp_dir().to_string_lossy().into_owned();
    let mut roots: Vec<String> = Vec::with_capacity(3);
    for raw in [workspace_root, "/tmp", tempdir.as_str()] {
        let canonical = canonical_path(raw);
        if !roots.contains(&canonical) {
            roots.push(canonical);
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_grants_nothing() {
        assert_eq!(
            writable_roots(SandboxMode::ReadOnly, "/ws"),
            [] as [std::string::String; 0]
        );
        assert_eq!(
            writable_roots(SandboxMode::DangerFullAccess, "/ws"),
            [] as [std::string::String; 0]
        );
    }

    #[test]
    fn missing_roots_keep_their_spelling() {
        assert_eq!(canonical_path("/no/such/root/here"), "/no/such/root/here");
    }

    #[test]
    fn workspace_write_dedupes_the_canonical_roots() {
        // A temp dir that is itself /tmp collapses the three grants into
        // a two-entry list, canonically spelled.
        let roots = writable_roots(SandboxMode::WorkspaceWrite, "/");
        assert_eq!(roots.first().map(String::as_str), Some("/"));
        assert_eq!(
            roots.len(),
            2,
            "the /tmp alias dedupes against the host temp dir: {roots:?}"
        );
        let tmp = std::env::temp_dir().to_string_lossy().into_owned();
        assert_eq!(
            roots.get(1).map(String::as_str),
            Some(canonical_path(&tmp).as_str())
        );
    }
}
