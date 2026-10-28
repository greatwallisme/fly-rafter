// Rust guideline compliant 2026-10-29
//! Server-side write-grant materialization (dsh `grant.ts`): one write
//! SID's provider-lifetime grants. Workspace paths are added STANDING
//! (their security descriptor edits are the cross-session reuse cache
//! and outlive the grant); temp paths are revocable (dispose revokes
//! them — an inheritable ACE must not outlive its session's temp
//! directory).

use super::acl;
use super::ffi;
use super::ffi::NativePtr;
use super::ffi::Win32Error;
use super::win32_abi as abi;

/// One write SID's provider-lifetime grant materialization (dsh
/// `AclWriteGrant`): the parsed SID pointer plus every directory whose
/// DACL currently carries its ACE.
#[derive(Debug)]
pub struct AclWriteGrant {
    /// The write SID in SDDL string form.
    pub write_sid: String,
    sid_ptr: NativePtr,
    low_label_sid_ptr: NativePtr,
    world_sid_ptr: NativePtr,
    revocable_paths: Vec<String>,
    standing_paths: Vec<String>,
}

impl AclWriteGrant {
    /// Parses the SID string and creates the Low integrity and world
    /// SIDs its grants label and deny with (dsh `AclWriteGrant.create`).
    /// Fail-closed: any failure throws — nothing is granted yet.
    ///
    /// # Errors
    ///
    /// Returns the Win32 failure of the first refusing call.
    pub fn create(write_sid: &str) -> Result<Self, Win32Error> {
        let sid_ptr = ffi::convert_string_sid_to_sid(write_sid)?;
        let low_label_sid_ptr = match ffi::create_well_known_sid(abi::WIN_LOW_LABEL_SID) {
            Ok(sid) => sid,
            Err(error) => {
                let _ignored = ffi::local_free(sid_ptr, "write SID after failure");
                return Err(error);
            }
        };
        let world_sid_ptr = match ffi::create_well_known_sid(abi::WIN_WORLD_SID) {
            Ok(sid) => sid,
            Err(error) => {
                let _ignored = ffi::local_free(low_label_sid_ptr, "Low label SID after failure");
                let _ignored = ffi::local_free(sid_ptr, "write SID after failure");
                return Err(error);
            }
        };
        Ok(Self {
            write_sid: write_sid.to_owned(),
            sid_ptr,
            low_label_sid_ptr,
            world_sid_ptr,
            revocable_paths: Vec::new(),
            standing_paths: Vec::new(),
        })
    }

    /// Grants the write ACE, the ambient-delete deny, and the Low
    /// mandatory label on one directory (idempotent; dsh `add`) and
    /// records the path for [`AclWriteGrant::dispose`] unless it is
    /// standing. The path is recorded BEFORE the grant: a post-apply
    /// throw must still revoke it, and revoking an ungranted path is a
    /// no-op merge.
    ///
    /// # Errors
    ///
    /// Returns the Win32 failure of the refusing call; callers treat a
    /// throw as a failed materialization and dispose the instance.
    pub fn add(&mut self, path: &str) -> Result<(), Win32Error> {
        self.revocable_paths.push(path.to_owned());
        acl::grant_write(
            path,
            self.sid_ptr,
            self.low_label_sid_ptr,
            self.world_sid_ptr,
        )
    }

    /// The standing variant of [`AclWriteGrant::add`]: the edits outlive
    /// this grant (the workspace reuse cache; dispose skips revoking
    /// it).
    ///
    /// # Errors
    ///
    /// Returns the Win32 failure of the refusing call.
    pub fn add_standing(&mut self, path: &str) -> Result<(), Win32Error> {
        self.standing_paths.push(path.to_owned());
        acl::grant_write(
            path,
            self.sid_ptr,
            self.low_label_sid_ptr,
            self.world_sid_ptr,
        )
    }

    /// Every directory currently carrying the grant, in grant order.
    #[must_use]
    pub fn paths(&self) -> Vec<String> {
        let mut paths = self.standing_paths.clone();
        paths.extend(self.revocable_paths.iter().cloned());
        paths
    }

    /// Revokes every revocable grant (standing security descriptor edits
    /// stay) and frees the SIDs (dsh `dispose`); reports every cleanup
    /// failure.
    ///
    /// # Errors
    ///
    /// Returns every cleanup failure joined, if any occurred.
    pub fn dispose(&mut self) -> Result<(), String> {
        let mut failures: Vec<String> = Vec::new();
        for path in &self.revocable_paths {
            if let Err(error) = acl::revoke_write(path, self.sid_ptr) {
                failures.push(error.to_string());
            }
        }
        self.revocable_paths.clear();
        self.standing_paths.clear();
        for (label, sid) in [
            ("write SID", self.sid_ptr),
            ("Low label SID", self.low_label_sid_ptr),
            ("world SID", self.world_sid_ptr),
        ] {
            if let Err(error) = ffi::local_free(sid, label) {
                failures.push(error.to_string());
            }
        }
        self.sid_ptr = std::ptr::null_mut();
        self.low_label_sid_ptr = std::ptr::null_mut();
        self.world_sid_ptr = std::ptr::null_mut();
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "AclWriteGrant dispose completed with {} cleanup failure(s): {}",
                failures.len(),
                failures.join("; ")
            ))
        }
    }
}

impl Drop for AclWriteGrant {
    fn drop(&mut self) {
        // The documented dispose path is the explicit call; drop only
        // guards against a leaked allocation when an owner forgets it.
        if !ffi::is_null_ptr(self.sid_ptr) {
            let _ignored = ffi::local_free(self.sid_ptr, "write SID at drop");
        }
        if !ffi::is_null_ptr(self.low_label_sid_ptr) {
            let _ignored = ffi::local_free(self.low_label_sid_ptr, "Low label SID at drop");
        }
        if !ffi::is_null_ptr(self.world_sid_ptr) {
            let _ignored = ffi::local_free(self.world_sid_ptr, "world SID at drop");
        }
    }
}
