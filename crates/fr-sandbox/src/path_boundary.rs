// Rust guideline compliant 2026-10-29
//! Canonical directory-boundary checks for the Windows ACL workspace and
//! private-temp capabilities (dsh
//! `sandbox-windows-acl/src/path-boundary.ts`).
//!
//! Platform-independent on purpose (real-filesystem operations, no
//! Win32 calls), so the boundary semantics stay testable on every host
//! and the Windows build consumes the same single home.

use std::path::Path;
use std::path::PathBuf;

/// Computes `to` relative to `from` for canonical absolute inputs (the
/// Node `path.relative` shape this module needs: no `..` inside either
/// input, so a component walk reproduces it exactly).
fn relative(from: &Path, to: &Path) -> PathBuf {
    let from_parts: Vec<_> = from.components().collect();
    let to_parts: Vec<_> = to.components().collect();
    let common = from_parts
        .iter()
        .zip(to_parts.iter())
        .take_while(|(left, right)| left == right)
        .count();
    let mut result = PathBuf::new();
    for _ in common..from_parts.len() {
        result.push("..");
    }
    for part in &to_parts[common..] {
        result.push(part.as_os_str());
    }
    result
}

/// Whether `root` is the same canonical directory as `candidate` or
/// contains it (dsh `containsDirectory`).
fn contains_directory(root: &str, candidate: &str) -> bool {
    let canonical_root = crate::roots::canonical_path(root);
    let canonical_candidate = crate::roots::canonical_path(candidate);
    let relation = relative(Path::new(&canonical_root), Path::new(&canonical_candidate));
    if relation.as_os_str().is_empty() {
        return true;
    }
    let relation = relation.to_string_lossy();
    !relation.starts_with('/') && relation != ".." && !relation.starts_with("../")
}

/// Rejects a temp parent that is inside the workspace (dsh
/// `assertTempRootOutsideWorkspace`): every child created below it
/// would inherit the standing workspace capability.
///
/// # Errors
///
/// Returns the verbatim dsh refusal.
pub fn assert_temp_root_outside_workspace(
    workspace_root: &str,
    temp_root: &str,
) -> Result<(), String> {
    if contains_directory(workspace_root, temp_root) {
        return Err(format!(
            "Windows ACL temp root must be outside the workspace: workspace={workspace_root}; \
temp={temp_root}"
        ));
    }
    Ok(())
}

/// Rejects overlap between an actual private temp directory and any
/// writable directory (dsh `assertPrivateTempDisjoint`): either
/// inheritance direction would merge the two capabilities.
///
/// # Errors
///
/// Returns the verbatim dsh refusal naming the first overlapping pair.
pub fn assert_private_temp_disjoint(
    writable_dirs: &[String],
    temp_dir: &str,
) -> Result<(), String> {
    for writable_dir in writable_dirs {
        if contains_directory(writable_dir, temp_dir) || contains_directory(temp_dir, writable_dir)
        {
            return Err(format!(
                "AclSandbox private temp directory must be disjoint from writable directories: \
writable={writable_dir}; temp={temp_dir}"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_temp_root_inside_the_workspace_refuses_verbatim() {
        let workspace = std::env::temp_dir().join("fr-sandbox-boundary-ws");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        let inside = workspace.join("cache");
        std::fs::create_dir_all(&inside).expect("inside dir");
        let outside = std::env::temp_dir().join("fr-sandbox-boundary-temp");
        std::fs::create_dir_all(&outside).expect("outside dir");
        let workspace = workspace.to_string_lossy().into_owned();
        let inside = inside.to_string_lossy().into_owned();
        let outside = outside.to_string_lossy().into_owned();

        assert_eq!(
            assert_temp_root_outside_workspace(&workspace, &inside).unwrap_err(),
            format!(
                "Windows ACL temp root must be outside the workspace: workspace={workspace}; \
temp={inside}"
            )
        );
        assert_temp_root_outside_workspace(&workspace, &outside).unwrap();
        assert!(
            assert_temp_root_outside_workspace(&workspace, &workspace).is_err(),
            "the workspace itself is inside itself"
        );
        let _ws = std::fs::remove_dir_all(std::env::temp_dir().join("fr-sandbox-boundary-ws"));
        let _temp = std::fs::remove_dir_all(std::env::temp_dir().join("fr-sandbox-boundary-temp"));
    }

    #[test]
    fn either_inheritance_direction_merges_capabilities() {
        let base = std::env::temp_dir().join("fr-sandbox-boundary-pair");
        let writable = base.join("writable");
        let nested = writable.join("nested");
        let sibling = base.join("sibling");
        for dir in [&writable, &nested, &sibling] {
            std::fs::create_dir_all(dir).expect("dir");
        }
        let writable = writable.to_string_lossy().into_owned();
        let nested = nested.to_string_lossy().into_owned();
        let sibling = sibling.to_string_lossy().into_owned();

        assert_private_temp_disjoint(std::slice::from_ref(&writable), &sibling).unwrap();
        assert_eq!(
            assert_private_temp_disjoint(std::slice::from_ref(&writable), &nested).unwrap_err(),
            format!(
                "AclSandbox private temp directory must be disjoint from writable directories: \
writable={writable}; temp={nested}"
            )
        );
        assert!(
            assert_private_temp_disjoint(&[nested], &writable).is_err(),
            "a temp containing a writable dir also overlaps"
        );
        let _base = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn missing_paths_compare_by_spelling() {
        // A missing root canonicalizes to itself (the dsh roots-module
        // fallback), so a temp root spelled beneath it still refuses —
        // fail-closed either way (dsh surfaces the ENOENT instead).
        assert!(assert_temp_root_outside_workspace("/no/such/ws", "/no/such/ws/under").is_err());
        assert_temp_root_outside_workspace("/no/such/ws", "/elsewhere").unwrap();
    }
}
