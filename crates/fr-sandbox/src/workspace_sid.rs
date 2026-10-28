// Rust guideline compliant 2026-10-29
//! The per-workspace write identity (dsh
//! `sandbox-windows-acl/src/workspace-sid.ts`).
//!
//! A deterministic `S-1-4-x-y` SID derived from the canonical workspace
//! path, whose ACEs form that workspace's write allowlist. Every
//! confined execution of the same workspace — across sessions, server
//! restarts, and calls — carries the SAME write SID, so the
//! workspace-root ACE materializes once per workspace per machine (the
//! grant's exact-ACE skip then makes every later provision O(1))
//! instead of once per session. The SID's power is defined solely by
//! the ACEs that name it (which exist only on the workspace tree and
//! the session's private temp directory), and only tokens minted for
//! that workspace carry it — the SID string itself is not a secret.
//! Temp directories use a separate, per-directory identity; sharing
//! the workspace identity with temp would let sibling sessions write
//! one another's temp trees.
//!
//! The input MUST be the canonical workspace path (the policy home's
//! resolution already applies it): canonicalization converges
//! case/alias spellings, so two spellings of one workspace derive one
//! SID. Renaming the workspace directory derives a new SID — the old
//! standing ACEs are inert residue, and the next session re-propagates
//! once.

use sha2::{Digest, Sha256};

/// Derives the workspace's write SID (`S-1-4-x-y`; subauthorities
/// 30-bit, matching the workspace-capability shape the token and ACE
/// layers carry).
#[must_use]
pub fn workspace_write_sid(workspace_root: &str) -> String {
    let digest = Sha256::digest(workspace_root.as_bytes());
    let first =
        (u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]) % (2u32.pow(30) - 1)) + 1;
    let second =
        (u32::from_le_bytes([digest[4], digest[5], digest[6], digest[7]]) % (2u32.pow(30) - 1)) + 1;
    format!("S-1-4-{first}-{second}")
}

/// Derives one private temp directory's write SID. The random directory
/// path is the capability identity; a fixed third subauthority
/// domain-separates the result from every two-subauthority workspace
/// SID.
#[must_use]
pub fn temp_write_sid(temp_dir: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"temp\0");
    hasher.update(temp_dir.as_bytes());
    let digest = hasher.finalize();
    let first =
        (u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]) % (2u32.pow(30) - 1)) + 1;
    let second =
        (u32::from_le_bytes([digest[4], digest[5], digest[6], digest[7]]) % (2u32.pow(30) - 1)) + 1;
    format!("S-1-4-{first}-{second}-1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sids_pin_the_dsh_derivation_shape() {
        // dsh: sha256("C:/ws") first two LE u32 % (2^30-1) + 1.
        let digest = Sha256::digest(b"C:/ws");
        let first = (u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]])
            % (2u32.pow(30) - 1))
            + 1;
        let second = (u32::from_le_bytes([digest[4], digest[5], digest[6], digest[7]])
            % (2u32.pow(30) - 1))
            + 1;
        assert_eq!(
            workspace_write_sid("C:/ws"),
            format!("S-1-4-{first}-{second}")
        );

        // The temp identity hashes `temp\0` + the directory, so its two
        // subauthorities differ from the workspace derivation of the
        // same path, and the fixed third subauthority domain-separates
        // the result from every workspace SID.
        let mut hasher = Sha256::new();
        hasher.update(b"temp\0");
        hasher.update(b"C:/ws");
        let temp_digest = hasher.finalize();
        let temp_first = (u32::from_le_bytes([
            temp_digest[0],
            temp_digest[1],
            temp_digest[2],
            temp_digest[3],
        ]) % (2u32.pow(30) - 1))
            + 1;
        let temp_second = (u32::from_le_bytes([
            temp_digest[4],
            temp_digest[5],
            temp_digest[6],
            temp_digest[7],
        ]) % (2u32.pow(30) - 1))
            + 1;
        assert_eq!(
            temp_write_sid("C:/ws"),
            format!("S-1-4-{temp_first}-{temp_second}-1")
        );
    }

    #[test]
    fn distinct_paths_derive_distinct_identities() {
        let left = workspace_write_sid("/a");
        let right = workspace_write_sid("/b");
        assert_ne!(left, right);
        assert_ne!(workspace_write_sid("/t"), temp_write_sid("/t"));
        assert!(left.starts_with("S-1-4-"));
        assert!(temp_write_sid("/t").ends_with("-1"));
    }

    #[test]
    fn subauthorities_stay_in_the_30_bit_band() {
        for path in ["/", "/ws", "C:\\deep\\workspace\\name", "/tmp/zone"] {
            for sid in [workspace_write_sid(path), temp_write_sid(path)] {
                let parts: Vec<&str> = sid.trim_start_matches("S-1-4-").split('-').collect();
                assert!(!parts.is_empty() && parts.len() <= 3, "shape: {sid}");
                for part in parts {
                    let value: u64 = part.parse().expect("numeric subauthority");
                    assert!(
                        value >= 1 && value <= 2u64.pow(30),
                        "subauthority {value} stays in (0, 2^30]: {sid}"
                    );
                }
            }
        }
    }
}
