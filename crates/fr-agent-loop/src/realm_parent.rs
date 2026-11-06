// Rust guideline compliant 2026-11-06
//! The parent-activation machinery for the 18e `fiber_realm` ruling
//! (decision 47-1A): role adoption and the self-artifact path discovery.
//!
//! One dylib artifact serves both activation roles (the parent activated
//! by the loader, realm-spawned children activated per session), so the
//! role cannot come from the descriptor — it is *adopted* by the first
//! role-defining call (`CREATE`/`RESUME` adopt `Parent`, the realm-spawn
//! handshake adopts `Child`) and refuses conflicting transitions
//! fail-closed.

use std::path::PathBuf;

/// The activation role (the 18e `fiber_realm` ruling).
///
/// Every activation starts [`Role::Undefined`]; the first role-defining
/// call adopts one terminal role. Session-scoped methods route by role:
/// a child serves them on its local session map, any other role looks the
/// session up in the realm registry and `realm_require`s the child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// No role-defining call yet: session methods refuse (fail-closed,
    /// no mailbox is minted), the first `CREATE`/`RESUME` adopts
    /// [`Role::Parent`], the first `METHOD_REALM_ADOPT` adopts
    /// [`Role::Child`].
    Undefined,
    /// The loader-activated instance: owns the global `agent` face,
    /// realm-spawns one child per session, routes session methods by
    /// session id.
    Parent,
    /// A realm-spawned session carrier: serves the session lifecycle and
    /// session methods on its local session map inside its session realm.
    Child,
}

impl Role {
    /// The role adopted by the first `CREATE`/`RESUME` (`Parent`), or the
    /// refusal for a `Child` receiving a lifecycle arm.
    ///
    /// # Errors
    ///
    /// Refuses the parent-only lifecycle on a session-realm child.
    pub fn adopt_for_lifecycle(current: Self) -> Result<Self, String> {
        match current {
            Self::Undefined | Self::Parent => Ok(Self::Parent),
            Self::Child => Err("create/resume is not served on a session-realm child".to_owned()),
        }
    }

    /// The role adopted by the first realm-spawn handshake (`Child`), or
    /// the refusal for an activation that already has a role.
    ///
    /// # Errors
    ///
    /// Refuses a second handshake or a handshake on the parent.
    pub fn adopt_for_realm_handshake(current: Self) -> Result<Self, String> {
        match current {
            Self::Undefined => Ok(Self::Child),
            Self::Parent => Err("realm adopt is not served on the parent activation".to_owned()),
            Self::Child => Err("realm adopt already completed".to_owned()),
        }
    }
}

/// The dylib artifact file name this crate compiles to (the cdylib
/// output name; the host resolver searches the same file).
pub const ARTIFACT_FILE_NAME: &str = "libfr_agent_loop.so";

/// Resolves the own-artifact path for `realm_spawn`.
///
/// Mirrors the host resolver (`crates/fr/src/assembly.rs`):
/// `FR_PLUGIN_PATH` entries first, then the executable's directory and
/// its parent (the dev layout `target/debug` and the installed layout
/// both resolve).
///
/// # Errors
///
/// Returns `None` when no candidate directory holds the artifact — the
/// caller refuses the lifecycle arm fail-closed.
#[must_use]
pub fn own_artifact_path() -> Option<String> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(paths) = std::env::var_os("FR_PLUGIN_PATH") {
        dirs.extend(std::env::split_paths(&paths));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        dirs.push(dir.to_path_buf());
        if let Some(parent) = dir.parent() {
            dirs.push(parent.to_path_buf());
        }
    }
    resolve_artifact(&dirs, ARTIFACT_FILE_NAME).map(|path| path.to_string_lossy().into_owned())
}

/// The pure candidate search: the first directory holding `file_name`
/// wins, in the given order.
#[must_use]
pub fn resolve_artifact(dirs: &[PathBuf], file_name: &str) -> Option<PathBuf> {
    dirs.iter()
        .map(|dir| dir.join(file_name))
        .find(|candidate| candidate.is_file())
}

/// One decoded `realm_spawn` success document (the host's
/// `{"realm":L,"child":C,"state":"active"|"pending"}` reply).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnedRealm {
    /// The realm label the child activated in.
    pub realm: u64,
    /// The child handle for `realm_retire`.
    pub child: u64,
}

impl SpawnedRealm {
    /// Decodes the host's spawn document, refusing any missing or
    /// mistyped field.
    ///
    /// # Errors
    ///
    /// Returns the decode refusal text (the reply never leaves the
    /// parent arm; the error surfaces as the lifecycle failure).
    pub fn decode(reply: &[u8]) -> Result<Self, String> {
        let document: serde_json::Value = serde_json::from_slice(reply)
            .map_err(|error| format!("spawn reply decode failed: {error}"))?;
        let realm = document
            .get("realm")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "spawn reply carries no realm".to_owned())?;
        let child = document
            .get("child")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "spawn reply carries no child".to_owned())?;
        Ok(Self { realm, child })
    }
}

/// The handshake payload the parent sends its fresh child (the role word
/// the [`crate::agent_service::METHOD_REALM_ADOPT`] face validates).
pub const REALM_ADOPT_PAYLOAD: &[u8] = br#"{"role":"child"}"#;

/// Validates the realm-adopt handshake payload: exactly the role word
/// `child` is accepted (fail-closed on anything else).
///
/// # Errors
///
/// Returns the refusal text for a malformed or mismatched payload.
pub fn validate_realm_adopt_payload(payload: &[u8]) -> Result<(), String> {
    #[derive(serde::Deserialize)]
    struct Handshake {
        role: String,
    }
    let handshake: Handshake = serde_json::from_slice(payload)
        .map_err(|error| format!("realm-adopt payload is invalid: {error}"))?;
    if handshake.role == "child" {
        Ok(())
    } else {
        Err(format!(
            "realm-adopt role \"{}\" is not supported",
            handshake.role
        ))
    }
}

/// Keep the `Path` import used by the pure resolver's callers in tests
/// honest about the public surface (the type appears in no signature
/// otherwise).
#[cfg(test)]
mod tests {
    use super::{
        REALM_ADOPT_PAYLOAD, Role, SpawnedRealm, resolve_artifact, validate_realm_adopt_payload,
    };
    use std::path::PathBuf;

    /// A scratch directory holding one artifact file; removed on drop.
    struct ArtifactDir(std::path::PathBuf);

    impl ArtifactDir {
        fn with_artifact(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "fr-agent-loop-realm-{}-{}-{}",
                name,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock")
                    .as_nanos()
            ));
            std::fs::create_dir_all(&root).expect("scratch dir");
            std::fs::write(root.join(name), b"artifact").expect("artifact file");
            Self(root)
        }
    }

    impl Drop for ArtifactDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn lifecycle_adopts_parent_once_and_refuses_on_child() {
        assert_eq!(Role::adopt_for_lifecycle(Role::Undefined), Ok(Role::Parent));
        assert_eq!(Role::adopt_for_lifecycle(Role::Parent), Ok(Role::Parent));
        let refusal = Role::adopt_for_lifecycle(Role::Child).expect_err("child refuses");
        assert!(refusal.contains("not served on a session-realm child"));
    }

    #[test]
    fn handshake_adopts_child_once_and_refuses_elsewhere() {
        assert_eq!(
            Role::adopt_for_realm_handshake(Role::Undefined),
            Ok(Role::Child)
        );
        let again =
            Role::adopt_for_realm_handshake(Role::Child).expect_err("second handshake refuses");
        assert!(again.contains("already completed"));
        let parent = Role::adopt_for_realm_handshake(Role::Parent).expect_err("parent refuses");
        assert!(parent.contains("not served on the parent"));
    }

    #[test]
    fn resolver_takes_the_first_directory_holding_the_artifact() {
        let empty = ArtifactDir::with_artifact("other-file.so");
        let hit = ArtifactDir::with_artifact("libfr_agent_loop.so");
        let late = ArtifactDir::with_artifact("libfr_agent_loop.so");
        let dirs: Vec<PathBuf> = vec![empty.0.clone(), hit.0.clone(), late.0.clone()];
        let resolved = resolve_artifact(&dirs, "libfr_agent_loop.so").expect("a hit");
        assert_eq!(resolved, hit.0.join("libfr_agent_loop.so"));
    }

    #[test]
    fn resolver_reports_a_miss_when_no_directory_holds_it() {
        let empty = ArtifactDir::with_artifact("other-file.so");
        assert!(resolve_artifact(std::slice::from_ref(&empty.0), "libfr_agent_loop.so").is_none());
    }

    #[test]
    fn spawned_realm_decodes_the_host_document() {
        let decoded = SpawnedRealm::decode(br#"{"realm":7,"child":3,"state":"active"}"#)
            .expect("the document decodes");
        assert_eq!(decoded, SpawnedRealm { realm: 7, child: 3 });
        assert!(
            SpawnedRealm::decode(br#"{"child":3,"state":"active"}"#)
                .expect_err("missing realm refuses")
                .contains("no realm")
        );
        assert!(
            SpawnedRealm::decode(b"not json")
                .expect_err("garbage refuses")
                .contains("decode failed")
        );
    }

    #[test]
    fn handshake_payload_accepts_only_the_child_word() {
        validate_realm_adopt_payload(REALM_ADOPT_PAYLOAD).expect("the child word validates");
        let wrong = validate_realm_adopt_payload(br#"{"role":"parent"}"#)
            .expect_err("the parent word refuses");
        assert!(wrong.contains("\"parent\" is not supported"));
        assert!(
            validate_realm_adopt_payload(b"not json")
                .expect_err("garbage refuses")
                .contains("invalid")
        );
    }

    #[test]
    fn artifact_file_name_matches_the_cdylib_output() {
        // The cdylib output name derives from the crate name; this pin
        // catches a rename that would silently break self-spawning.
        assert_eq!(super::ARTIFACT_FILE_NAME, "libfr_agent_loop.so");
    }
}
