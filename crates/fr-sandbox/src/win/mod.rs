// Rust guideline compliant 2026-10-29
//! The Windows ACL backend (dsh `sandbox-windows-acl`, Q99-B full port).
//!
//! Mirrors the mechanism of github.com/huoyaoyuan/windows-acl-restrict-poc
//! @ 10e4dfb (the fixed revision): a WRITE_RESTRICTED token whose
//! restricting SIDs include distinct workspace and temp write SIDs that
//! this sandbox adds to their owning directories' DACLs — the
//! intersection check then allows writes exactly where either
//! capability has a Write ACE. The intersection covers only the object's
//! own access check, so the token is also lowered to Low integrity and
//! every granted directory carries a Low no-write-up label and the
//! ambient-delete deny the [`acl`] module documents. Unlike the POC,
//! every API failure throws with the API name and exact Win32 code; a
//! child is NEVER spawned unrestricted.
//!
//! Compiled only into Windows builds (`#[cfg(windows)]`); the
//! platform-independent halves — [`crate::workspace_sid`] and
//! [`crate::path_boundary`] — live outside this module and stay testable
//! on every host.

pub mod acl;
pub mod ffi;
pub mod grant;
pub mod sandbox;
pub mod spawn;
pub mod token;
pub mod win32_abi;

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::provider::SandboxInternals;

pub use crate::workspace_sid::temp_write_sid;
pub use crate::workspace_sid::workspace_write_sid;

pub use grant::AclWriteGrant;
pub use sandbox::AclSandbox;
pub use sandbox::AclSandboxOptions;
pub use sandbox::ChildResult;
pub use sandbox::SpawnOptions;
pub use sandbox::Stdio;

/// One live session/workspace pair's private temp directory and
/// capability (dsh `AclTempCapability`).
#[derive(Debug)]
pub struct TempCapability {
    /// The random private temp directory.
    pub dir: String,
    /// Its distinct write SID.
    pub write_sid: String,
    /// The revocable grant owning its ACEs.
    pub grant: AclWriteGrant,
}

/// Materializes one workspace-write policy's ACEs once per provider
/// lifetime (dsh `materializeAclGrant`): the workspace SID and standing
/// root grant are shared by the workspace; the temp directory is random
/// and carries a distinct SID, so another session on the same workspace
/// cannot use the shared workspace SID to enter it. A fresh provider
/// always chooses a new path; crash residue therefore cannot collide
/// with or authorize a resumed session. Fail-closed: a half-materialized
/// temp grant is revoked and its directory removed before the error
/// propagates.
pub fn materialize_grant(
    session_id: &str,
    workspace_root: &str,
    workspace_grants: &Mutex<HashMap<String, AclWriteGrant>>,
    temp_capabilities: &Mutex<HashMap<String, TempCapability>>,
    _internals: &SandboxInternals,
) -> Result<(String, String), String> {
    crate::path_boundary::assert_temp_root_outside_workspace(
        workspace_root,
        &std::env::temp_dir().to_string_lossy(),
    )?;
    let write_sid = workspace_write_sid(workspace_root);
    let mut grants = workspace_grants.lock().expect("workspace grants lock");
    if !grants.contains_key(workspace_root) {
        let mut grant = AclWriteGrant::create(&write_sid).map_err(|error| error.to_string())?;
        // A failed standing grant carries no revocable edits; a standing
        // ACE applied before a post-apply throw is the intended end
        // state, not an error artifact — nothing to revoke.
        if let Err(error) = grant.add_standing(workspace_root) {
            return Err(error.to_string());
        }
        grants.insert(workspace_root.to_owned(), grant);
    }
    drop(grants);
    let key =
        serde_json::to_string(&(session_id, workspace_root)).map_err(|error| error.to_string())?;
    let mut capabilities = temp_capabilities.lock().expect("temp capabilities lock");
    if let Some(existing) = capabilities.get(&key) {
        return Ok((existing.dir.clone(), existing.write_sid.clone()));
    }
    let temp_dir = std::env::temp_dir().join(format!("fr-{}", unique_suffix()));
    std::fs::create_dir(&temp_dir).map_err(|error| {
        format!("sandbox-local windows-acl temp grant materialization failed: {error}")
    })?;
    let temp_dir = temp_dir.to_string_lossy().into_owned();
    let temp_sid = temp_write_sid(&temp_dir);
    let mut grant = match AclWriteGrant::create(&temp_sid) {
        Ok(grant) => grant,
        Err(error) => {
            let _ignored = std::fs::remove_dir_all(&temp_dir);
            return Err(error.to_string());
        }
    };
    if let Err(error) = grant.add(&temp_dir) {
        // Fail-closed cleanup: never leave a revocable grant or its
        // directory behind a failed materialization.
        let mut failures: Vec<String> = vec![error.to_string()];
        if let Err(cleanup) = grant.dispose() {
            failures.push(cleanup);
        }
        let _ignored = std::fs::remove_dir_all(&temp_dir);
        return Err(failures.join("; "));
    }
    let view = (temp_dir.clone(), temp_sid);
    capabilities.insert(
        key,
        TempCapability {
            dir: temp_dir,
            write_sid: view.1.clone(),
            grant,
        },
    );
    Ok(view)
}

/// Derives one random private-suffix for a provider-owned temp
/// directory (the mkdtemp stand-in with the fr prefix).
fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_nanos());
    let pid = std::process::id();
    let mut seed = nanos.unwrap_or_else(|_| u128::from(pid)) ^ (u128::from(pid) << 64);
    // xorshift over the mixed clock/pid state; uniqueness, not
    // unpredictability, is the property this suffix needs (the SID's
    // power is its ACEs, dsh workspace-sid doc).
    seed ^= seed << 13;
    seed ^= seed >> 7;
    seed ^= seed << 17;
    format!("{seed:032x}")
}

/// Disposes every write grant (provider teardown; dsh `revokeAclGrants`):
/// the revocable temp ACEs are revoked, the private temp directories
/// this provider created are removed, and every SID allocation is freed;
/// the standing workspace ACEs stay (the reuse cache). Cleanup failures
/// are reported, not thrown.
pub fn revoke_grants(
    workspace_grants: &Mutex<HashMap<String, AclWriteGrant>>,
    temp_capabilities: &Mutex<HashMap<String, TempCapability>>,
    internals: &SandboxInternals,
) -> Result<(), String> {
    let remove_temp_dir = |dir: &str| -> Result<(), String> {
        match &internals.rm_temp_dir {
            Some(remove) => {
                remove(dir);
                Ok(())
            }
            None => std::fs::remove_dir_all(dir).map_err(|error| error.to_string()),
        }
    };
    let mut failures: Vec<String> = Vec::new();
    let mut grants: Vec<AclWriteGrant> = {
        let mut guards = workspace_grants.lock().expect("workspace grants lock");
        guards.drain().map(|(_, grant)| grant).collect()
    };
    let mut capabilities: Vec<TempCapability> = {
        let mut guards = temp_capabilities.lock().expect("temp capabilities lock");
        guards.drain().map(|(_, capability)| capability).collect()
    };
    for grant in grants.iter_mut().chain(
        capabilities
            .iter_mut()
            .map(|capability| &mut capability.grant),
    ) {
        if let Err(error) = grant.dispose() {
            failures.push(error);
        }
    }
    for capability in &capabilities {
        if let Err(error) = remove_temp_dir(&capability.dir) {
            failures.push(error);
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}
