// Rust guideline compliant 2026-10-29
//! ACL editing (dsh `acl.ts`): grant/revoke a capability SID on a
//! directory via SetEntriesInAclW + SetNamedSecurityInfoW, with the
//! failure handling the POC lacks.
//!
//! Each grant applies three edits in ONE SetNamedSecurityInfoW call:
//! the capability-SID allow ACE, a Deny ACE that removes the ambient
//! `FILE_DELETE_CHILD` right from the world SID, and a Low no-write-up
//! mandatory label. The deny is what keeps one granted root out of
//! another's reach: Windows also authorizes a delete from the parent
//! directory's `FILE_DELETE_CHILD` right, which the token's
//! write-restricted intersection does not reach.
//!
//! Concurrency: grants are read-merge-write against the directory's
//! CURRENT DACL, and the whole get-merge-set sequence runs under a
//! per-path exclusive LockFileEx lock so concurrent sandbox instances
//! cannot clobber each other's ACEs.

use sha2::{Digest, Sha256};

use super::ffi;
use super::ffi::NativePtr;
use super::ffi::Win32Error;
use super::win32_abi as abi;

/// Packs one EXPLICIT_ACCESS_W (48 bytes; dsh `buildExplicitAccess`):
/// perms@0, mode@4, inheritance@8, Trustee@16 with ptstrName@40.
/// `permissions` is the access mask; a revoke passes 0, which removes
/// every ACE for the trustee. `inheritance` defaults to children of
/// both kinds; the ambient-delete deny narrows it to containers because
/// FILE_DELETE_CHILD is meaningless on a file and its bit would
/// otherwise spread through the file's inherited mask.
#[must_use]
pub fn build_explicit_access(
    sid: NativePtr,
    mode: u32,
    permissions: u32,
    inheritance: u32,
) -> Vec<u8> {
    let mut entry = vec![0u8; abi::EXPLICIT_ACCESS_W_SIZE];
    entry[0..4].copy_from_slice(&permissions.to_le_bytes()); // grfAccessPermissions
    entry[4..8].copy_from_slice(&mode.to_le_bytes()); // grfAccessMode
    entry[8..12].copy_from_slice(&inheritance.to_le_bytes()); // grfInheritance
    entry[24..28].copy_from_slice(&abi::NO_MULTIPLE_TRUSTEE.to_le_bytes()); // MultipleTrusteeOperation
    entry[28..32].copy_from_slice(&abi::TRUSTEE_IS_SID.to_le_bytes()); // TrusteeForm
    entry[32..36].copy_from_slice(&abi::TRUSTEE_IS_UNKNOWN.to_le_bytes()); // TrusteeType
    entry[40..48].copy_from_slice(&(sid as usize).to_le_bytes()); // ptstrName = the capability SID
    entry
}

/// One lock file per protected path (dsh `lockFilePath`):
/// `<GetTempPathW()>\fr-acl-locks\<first 16 hex of
/// sha256(lowercased path)>.lock`. The lock root derives from the
/// system temp path (never from runner argv or `FR_HOME`), and the
/// lowercasing maps Windows's case-insensitive path spellings onto one
/// lock.
#[must_use]
pub fn lock_file_path(path: &str) -> String {
    let digest = Sha256::digest(path.to_lowercase().as_bytes());
    let mut hex = String::with_capacity(16);
    for byte in &digest[..8] {
        hex.push_str(&format!("{byte:02x}"));
    }
    std::env::temp_dir()
        .join("fr-acl-locks")
        .join(format!("{hex}.lock"))
        .to_string_lossy()
        .into_owned()
}

/// Runs `action` holding the per-path exclusive lock (dsh
/// `withPathLock`): CreateFileW (OPEN_ALWAYS, shared read/write but NOT
/// delete — a deletable lock file could be removed and recreated under
/// the holder, letting two processes hold "the same" lock), then a
/// one-byte LockFileEx exclusive range, then UnlockFileEx + CloseHandle.
/// An `action` failure still unlocks (best-effort) and rethrows.
///
/// # Errors
///
/// Returns the Win32 failure of the first refusing call, or the action's
/// own failure.
pub fn with_path_lock<T>(
    path: &str,
    action: impl FnOnce() -> Result<T, Win32Error>,
) -> Result<T, Win32Error> {
    let lock_path = lock_file_path(path);
    if let Some(parent) = std::path::Path::new(&lock_path).parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            Win32Error::new(
                "CreateDirectoryW",
                0,
                format!("lock root for {path}: {error}"),
            )
        })?;
    }
    let handle = ffi::open_lock_file(&lock_path)?;
    if let Err(error) = ffi::lock_file_exclusive(handle, &lock_path) {
        let _ignored = ffi::close_handle_checked(handle, &lock_path);
        return Err(error);
    }
    let result = action();
    // Best-effort release on the action-failure path: cleanup failures
    // must not mask the action's error.
    if result.is_err() {
        let _ignored = ffi::unlock_file_exclusive(handle, &lock_path);
        let _ignored = ffi::close_handle_checked(handle, &lock_path);
        return result;
    }
    ffi::unlock_file_exclusive(handle, &lock_path)?;
    ffi::close_handle_checked(handle, &format!("lock file {lock_path}"))?;
    result
}

/// Reads a u8 at one native offset (the koffi.decode replacement).
///
/// # Safety
///
/// The pointer must address at least `offset + 1` readable bytes.
unsafe fn read_u8_at(base: NativePtr, offset: usize) -> u8 {
    // SAFETY: caller guarantees the readable window.
    unsafe { *(base.byte_add(offset) as *const u8) }
}

/// Reads a u16 at one native offset.
///
/// # Safety
///
/// The pointer must address at least `offset + 2` readable bytes.
unsafe fn read_u16_at(base: NativePtr, offset: usize) -> u16 {
    // SAFETY: caller guarantees the readable window.
    unsafe { *(base.byte_add(offset) as *const u16) }
}

/// Reads a u32 at one native offset.
///
/// # Safety
///
/// The pointer must address at least `offset + 4` readable bytes.
unsafe fn read_u32_at(base: NativePtr, offset: usize) -> u32 {
    // SAFETY: caller guarantees the readable window.
    unsafe { *(base.byte_add(offset) as *const u32) }
}

/// Compares two in-memory SID records without allocating strings (dsh
/// `sameSidAt`): revision, authority, and every sub-authority match.
///
/// # Safety
///
/// Both windows must address complete SID records at their offsets.
unsafe fn same_sid_at(
    left: NativePtr,
    left_offset: usize,
    right: NativePtr,
    right_offset: usize,
) -> bool {
    // SAFETY: the callers pass ACL-internal SIDs validated by the ACL
    // size walk and freshly allocated SIDs.
    unsafe {
        if read_u8_at(left, left_offset) != read_u8_at(right, right_offset) {
            return false;
        }
        let left_count = read_u8_at(left, left_offset + 1);
        let right_count = read_u8_at(right, right_offset + 1);
        if left_count != right_count || left_count > abi::SID_MAX_SUB_AUTHORITIES {
            return false;
        }
        for index in 0..6 {
            if read_u8_at(left, left_offset + 2 + index)
                != read_u8_at(right, right_offset + 2 + index)
            {
                return false;
            }
        }
        for index in 0..left_count {
            if read_u32_at(left, left_offset + 8 + usize::from(index) * 4)
                != read_u32_at(right, right_offset + 8 + usize::from(index) * 4)
            {
                return false;
            }
        }
        true
    }
}

/// True when the ACL already carries the EXACT entry
/// `(ace_type, inheritance, mask, trustee SID)` (dsh `hasExactEntry`).
/// The ACE's SID is INLINE (embedded after the 4-byte mask — there is no
/// pointer to read), compared field-by-field through bounded offset
/// reads. A malformed header reads as "no exact entry" so the caller
/// falls back to the merge-apply path, which owns the robust failure
/// handling.
fn has_exact_entry(
    acl: NativePtr,
    ace_type: u8,
    inheritance: u32,
    mask: u32,
    sid: NativePtr,
) -> bool {
    // SAFETY: the ACL header and each ACE are validated by the size
    // walk below before any read; a malformed walk returns false
    // without reading past the declared bounds.
    unsafe {
        let acl_size = usize::from(read_u16_at(acl, 2));
        let ace_count = usize::from(read_u16_at(acl, 4));
        if !(8..=1_048_576).contains(&acl_size) {
            return false; // implausible: fall back to the merge path
        }
        let mut offset = 8usize; // the first ACE follows the 8-byte ACL header
        for _ in 0..ace_count {
            // ACE_HEADER: AceType@0, AceFlags@1, AceSize@2 (WORD);
            // Mask@4, inline SID@8.
            let ace_size = usize::from(read_u16_at(acl, offset + 2));
            if ace_size < 8 || offset + ace_size > acl_size {
                return false; // implausible: fall back to the merge path
            }
            let exact = read_u8_at(acl, offset) == ace_type
                && u32::from(read_u8_at(acl, offset + 1)) == inheritance
                && read_u32_at(acl, offset + 4) == mask;
            if exact && same_sid_at(acl, offset + 8, sid, 0) {
                return true;
            }
            offset += ace_size;
        }
        false
    }
}

/// True when the explicit DACL already carries the EXACT write grant
/// (the Allow ACE for [`abi::GRANT_MASK`] naming the capability SID).
fn has_exact_grant(old_acl: NativePtr, sid: NativePtr) -> bool {
    has_exact_entry(
        old_acl,
        abi::ACCESS_ALLOWED_ACE_TYPE,
        abi::SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        abi::GRANT_MASK,
        sid,
    )
}

/// True when the explicit DACL already carries the EXACT ambient-delete
/// deny: the container-inherited Deny ACE for
/// [`abi::FILE_DELETE_CHILD`] naming the world SID.
fn has_exact_deny(old_acl: NativePtr, world_sid: NativePtr) -> bool {
    has_exact_entry(
        old_acl,
        abi::ACCESS_DENIED_ACE_TYPE,
        abi::CONTAINER_INHERIT_ACE,
        abi::FILE_DELETE_CHILD,
        world_sid,
    )
}

/// True when the label ACL already carries the EXACT Low no-write-up
/// label this module would add.
fn has_exact_label(label_acl: NativePtr, low_label_sid: NativePtr) -> bool {
    has_exact_entry(
        label_acl,
        abi::SYSTEM_MANDATORY_LABEL_ACE_TYPE,
        abi::SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        abi::SYSTEM_MANDATORY_LABEL_NO_WRITE_UP,
        low_label_sid,
    )
}

/// True when a capability grant for a SID OTHER than `sid` stands on
/// this DACL — the condition under which a revoke must leave the shared
/// Low label in place, or the remaining grant's child would lose its
/// write authority.
fn has_foreign_grant(old_acl: NativePtr, sid: NativePtr) -> bool {
    // SAFETY: the same bounded walk as has_exact_entry.
    unsafe {
        let acl_size = usize::from(read_u16_at(old_acl, 2));
        let ace_count = usize::from(read_u16_at(old_acl, 4));
        if !(8..=1_048_576).contains(&acl_size) {
            return false; // implausible: leave the label alone
        }
        let mut offset = 8usize;
        for _ in 0..ace_count {
            let ace_size = usize::from(read_u16_at(old_acl, offset + 2));
            if ace_size < 8 || offset + ace_size > acl_size {
                return false;
            }
            let is_grant = read_u8_at(old_acl, offset) == abi::ACCESS_ALLOWED_ACE_TYPE
                && read_u32_at(old_acl, offset + 4) == abi::GRANT_MASK;
            if is_grant && !same_sid_at(old_acl, offset + 8, sid, 0) {
                return true;
            }
            offset += ace_size;
        }
        false
    }
}

/// The mandatory-label half of an apply: set the given label ACL,
/// clear the label, or leave it untouched (dsh `LabelEdit`).
enum LabelEdit {
    /// Apply this label ACL alongside the DACL.
    Apply(NativePtr),
    /// Clear the label (no other grant relies on the shared level).
    Clear,
    /// Leave the label (a foreign grant still relies on it).
    Keep,
}

/// Builds the Low mandatory label applied with every write grant (dsh
/// `buildLowLabelAcl`): one SYSTEM_MANDATORY_LABEL_ACE naming the Low
/// SID with the no-write-up policy, inheriting to subcontainers and
/// objects. The caller frees the returned ACL.
///
/// # Errors
///
/// Returns the Win32 failure of the first refusing call; a half-built
/// ACL is released before the error propagates.
fn build_low_label_acl(low_label_sid: NativePtr) -> Result<NativePtr, Win32Error> {
    let sid_length = ffi::get_length_sid(low_label_sid, "Low mandatory label SID")?;
    let acl_length = abi::ACL_HEADER_SIZE + abi::MANDATORY_ACE_OVERHEAD + sid_length;
    let acl = ffi::local_alloc(acl_length)?;
    // SAFETY: InitializeAcl writes the header into the owned buffer.
    if unsafe {
        windows_sys::Win32::Security::InitializeAcl(
            acl.cast(),
            acl_length as u32,
            abi::ACL_REVISION,
        )
    } == 0
    {
        let error = ffi::win32_error("InitializeAcl", "Low mandatory label ACL");
        let _ignored = ffi::local_free(acl, "Low mandatory label ACL");
        return Err(error);
    }
    // SAFETY: AddMandatoryAce appends one ACE inside the allocated
    // length.
    if unsafe {
        windows_sys::Win32::Security::AddMandatoryAce(
            acl.cast(),
            abi::ACL_REVISION,
            abi::SUB_CONTAINERS_AND_OBJECTS_INHERIT,
            abi::SYSTEM_MANDATORY_LABEL_NO_WRITE_UP,
            low_label_sid,
        )
    } == 0
    {
        let error = ffi::win32_error("AddMandatoryAce", "Low mandatory label ACL");
        let _ignored = ffi::local_free(acl, "Low mandatory label ACL");
        return Err(error);
    }
    Ok(acl)
}

/// Shared tail of grant and revoke (dsh `mergeAndApply`): merge
/// `entries` into `old_acl` (null = no explicit DACL yet;
/// SetEntriesInAclW builds one from scratch), free the descriptor
/// before applying the merged ACL, apply the merged DACL together with
/// the label edit in one SetNamedSecurityInfoW call, then free every
/// ACL this call owns.
fn merge_and_apply(
    path: &str,
    entries: &[u8],
    old_acl: NativePtr,
    label_edit: LabelEdit,
    descriptor: NativePtr,
    label: &str,
) -> Result<(), Win32Error> {
    let new_acl = match ffi::set_entries_in_acl(entries, old_acl, &format!("{label}({path})")) {
        Ok(acl) => acl,
        Err(error) => {
            if !ffi::is_null_ptr(descriptor) {
                // Frees the ACL block too.
                let _ignored = ffi::local_free(descriptor, "descriptor after merge failure");
            }
            if let LabelEdit::Apply(acl) = label_edit {
                let _ignored = ffi::local_free(acl, "label ACL after merge failure");
            }
            return Err(error);
        }
    };
    // The descriptor block (old_acl included) is dead after the merge —
    // free it before applying, exactly like the POC.
    if !ffi::is_null_ptr(descriptor) {
        let _ignored = ffi::local_free(descriptor, "descriptor after merge");
    }
    let label_acl = match label_edit {
        LabelEdit::Apply(acl) => Some(acl),
        LabelEdit::Clear | LabelEdit::Keep => None,
    };
    let applied = ffi::set_named_security_info(path, new_acl, label_acl);
    let freed_new = ffi::local_free(new_acl, "new ACL");
    if let Some(acl) = label_acl {
        let _ignored = ffi::local_free(acl, "label ACL");
    }
    applied?;
    freed_new?;
    Ok(())
}

/// Grants `GRANT_MASK` (Write+Delete, displays as "Modify") to the
/// capability SID on `path`, denies the world SID the ambient
/// `FILE_DELETE_CHILD` right, and applies the Low mandatory label — one
/// merge (dsh `grantWrite`). Idempotent: the exact ACE, deny, and label
/// together SKIP the SetNamedSecurityInfoW apply, which would otherwise
/// re-propagate the identical descriptor across the whole tree (eager
/// inheritance; minutes on large workspaces). Runs under the per-path
/// lock. The directory must be owned by the caller AND grant
/// WRITE_OWNER (the label lives in the SACL).
///
/// # Errors
///
/// Returns the Win32 failure of the first refusing call.
pub fn grant_write(
    path: &str,
    sid: NativePtr,
    low_label_sid: NativePtr,
    world_sid: NativePtr,
) -> Result<(), Win32Error> {
    with_path_lock(path, || {
        let (old_acl, label_acl, descriptor) = ffi::get_named_security_info(path)?;
        if !ffi::is_null_ptr(old_acl)
            && !ffi::is_null_ptr(label_acl)
            && has_exact_grant(old_acl, sid)
            && has_exact_deny(old_acl, world_sid)
            && has_exact_label(label_acl, low_label_sid)
        {
            // The exact ACE, deny, and label stand: releasing the
            // descriptor is the whole operation.
            if !ffi::is_null_ptr(descriptor) {
                ffi::local_free(descriptor, &format!("grantWrite({path}) descriptor"))?;
            }
            return Ok(());
        }
        let label = match build_low_label_acl(low_label_sid) {
            Ok(label) => label,
            Err(error) => {
                if !ffi::is_null_ptr(descriptor) {
                    let _ignored = ffi::local_free(descriptor, "descriptor before label failure");
                }
                return Err(error);
            }
        };
        let mut entries = build_explicit_access(
            world_sid,
            abi::DENY_ACCESS,
            abi::FILE_DELETE_CHILD,
            abi::CONTAINER_INHERIT_ACE,
        );
        entries.extend(build_explicit_access(
            sid,
            abi::GRANT_ACCESS,
            abi::GRANT_MASK,
            abi::SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        ));
        merge_and_apply(
            path,
            &entries,
            old_acl,
            LabelEdit::Apply(label),
            descriptor,
            "grantWrite",
        )
    })
}

/// Removes every ACE for the capability SID from the directory DACL
/// (REVOKE_ACCESS merge — other entries are preserved; dsh
/// `revokeWrite`). The shared Low label is cleared only when no other
/// capability grant remains. Returns whether an ACE removal was
/// attempted (false when the directory carries no DACL at all).
///
/// # Errors
///
/// Returns the Win32 failure of the first refusing call.
pub fn revoke_write(path: &str, sid: NativePtr) -> Result<bool, Win32Error> {
    with_path_lock(path, || {
        let (old_acl, _label_acl, descriptor) = ffi::get_named_security_info(path)?;
        if ffi::is_null_ptr(old_acl) {
            if !ffi::is_null_ptr(descriptor) {
                ffi::local_free(descriptor, &format!("revokeWrite({path}) descriptor"))?;
            }
            return Ok(false);
        }
        let label_edit = if has_foreign_grant(old_acl, sid) {
            LabelEdit::Keep
        } else {
            LabelEdit::Clear
        };
        let entries = build_explicit_access(
            sid,
            abi::REVOKE_ACCESS,
            0,
            abi::SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        );
        merge_and_apply(
            path,
            &entries,
            old_acl,
            label_edit,
            descriptor,
            "revokeWrite",
        )?;
        Ok(true)
    })
}
