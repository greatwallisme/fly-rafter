// Rust guideline compliant 2026-10-29
//! Restricted-token construction (dsh `token.ts`): open the current
//! process token, extract its logon SID, build the well-known SIDs, and
//! call CreateRestrictedToken with the restricting-SID allowlist. Every
//! API call is checked; any failure throws with the API name and the
//! exact Win32 code.

use windows_sys::Win32::Foundation::HANDLE;

use super::ffi;
use super::ffi::NativePtr;
use super::ffi::Win32Error;
use super::win32_abi as abi;

/// The token rights CreateRestrictedToken and the later adjustments
/// require (dsh `openCurrentProcessToken` mask).
const TOKEN_RIGHTS: u32 =
    abi::TOKEN_QUERY | abi::TOKEN_DUPLICATE | abi::TOKEN_ADJUST_DEFAULT | abi::TOKEN_ASSIGN_PRIMARY;

/// Opens the current process's access token (dsh
/// `openCurrentProcessToken`): obtained through a real OpenProcess
/// handle because the GetCurrentProcess() pseudo-handle is not
/// addressable through the raw import surface.
///
/// # Errors
///
/// Returns the Win32 failure of the first refusing call; the process
/// handle is closed on every path.
pub fn open_current_process_token() -> Result<HANDLE, Win32Error> {
    let pid = std::process::id();
    let process = ffi::open_process_query(pid, &format!("pid {pid}"))?;
    let token = match ffi::open_process_token(process, TOKEN_RIGHTS, &format!("pid {pid}")) {
        Ok(token) => token,
        Err(error) => {
            // Best-effort close on the error path.
            let _ignored = ffi::close_handle_checked(process, "OpenProcess process handle");
            return Err(error);
        }
    };
    ffi::close_handle_checked(process, "OpenProcess process handle")?;
    Ok(token)
}

/// Reads one little-endian u32 out of a token buffer.
fn read_u32(buffer: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        buffer
            .get(offset..offset + 4)
            .unwrap_or(&[0, 0, 0, 0])
            .try_into()
            .unwrap_or([0, 0, 0, 0]),
    )
}

/// Reads one pointer-sized value out of a token buffer.
fn read_ptr(buffer: &[u8], offset: usize) -> NativePtr {
    usize::from_le_bytes(
        buffer
            .get(offset..offset + std::mem::size_of::<usize>())
            .unwrap_or(&[0; std::mem::size_of::<usize>()])
            .try_into()
            .unwrap_or([0; std::mem::size_of::<usize>()]),
    ) as NativePtr
}

/// Finds and copies the token's logon session SID (S-1-5-5-x-y,
/// attribute SE_GROUP_LOGON_ID; dsh `findLogonSid`). The restricted
/// token needs it for WinSta0/desktop and other per-logon objects.
///
/// # Errors
///
/// Returns the Win32 failure of the first refusing call, or the
/// prerequisite refusal when the token carries no logon SID.
pub fn find_logon_sid(token: HANDLE) -> Result<NativePtr, Win32Error> {
    let groups = ffi::get_token_information(token, abi::TOKEN_GROUPS, "TokenGroups")?;
    let group_count = read_u32(&groups, 0) as usize;
    for index in 0..group_count {
        let base = abi::TOKEN_GROUPS_OFFSET + index * abi::SID_AND_ATTRIBUTES_SIZE;
        let sid_ptr = read_ptr(&groups, base);
        let attributes = read_u32(&groups, base + 8);
        // SE_GROUP_LOGON_ID has bit 31 set; the masked comparison stays
        // in unsigned arithmetic (the dsh `>>> 0` note).
        let is_logon_id = attributes & abi::SE_GROUP_LOGON_ID == abi::SE_GROUP_LOGON_ID;
        if ffi::is_null_ptr(sid_ptr) || !is_logon_id {
            continue;
        }
        return ffi::copy_sid(sid_ptr, &format!("logon SID group {index}"));
    }
    Err(Win32Error::new(
        "CreateRestrictedToken",
        0,
        format!("prerequisite failed: no logon SID found among {group_count} token groups"),
    ))
}

/// Merges one full-access allow ACE for `sid` into the token's DEFAULT
/// DACL (dsh `setTokenDefaultDaclGrant`) — the DACL every NEW object the
/// token holder creates takes. The restricted token inherits the user's
/// default DACL verbatim, which names no restricting SID: a new
/// anonymous pipe (child stdio) would fail the write pass-2 check at
/// creation, breaking every piped-stdio grandchild spawn. The merged
/// ACE names a RESTRICTING SID, so each new object's own DACL passes
/// pass-2 while object creation stays gated by the parent container's
/// DACL.
///
/// # Errors
///
/// Returns the Win32 failure of the first refusing call.
pub fn set_token_default_dacl_grant(token: HANDLE, sid: NativePtr) -> Result<(), Win32Error> {
    let buffer = ffi::get_token_information(token, abi::TOKEN_DEFAULT_DACL, "TokenDefaultDacl")?;
    let current_dacl = read_ptr(&buffer, 0);
    if ffi::is_null_ptr(current_dacl) {
        return Err(Win32Error::new(
            "GetTokenInformation",
            0,
            "setTokenDefaultDaclGrant: the token carries no default DACL to extend",
        ));
    }
    let entries = super::acl::build_explicit_access(
        sid,
        abi::GRANT_ACCESS,
        abi::FILE_ALL_ACCESS,
        abi::SUB_CONTAINERS_AND_OBJECTS_INHERIT,
    );
    let new_dacl = ffi::set_entries_in_acl(&entries, current_dacl, "default DACL merge")?;
    // TOKEN_DEFAULT_DACL is exactly the pointer; SetTokenInformation
    // copies the ACL before returning.
    let info = (new_dacl as usize).to_le_bytes();
    let applied =
        ffi::set_token_information(token, abi::TOKEN_DEFAULT_DACL, &info, "TokenDefaultDacl");
    let freed = ffi::local_free(new_dacl, "merged default DACL");
    applied?;
    freed?;
    Ok(())
}

/// Lowers the restricted token's integrity level to Low (S-1-16-4096;
/// dsh `restrictTokenIntegrity`), the level the mandatory labels
/// `grant_write` applies are matched against; a token left at Medium
/// would ignore them.
///
/// # Errors
///
/// Returns the Win32 failure of the first refusing call.
pub fn restrict_token_integrity(token: HANDLE, low_label_sid: NativePtr) -> Result<(), Win32Error> {
    // TOKEN_MANDATORY_LABEL references the SID by pointer (16 bytes on
    // x64: pointer, Attributes, padding) — dsh allocates the SID length
    // alongside, but only the 16-byte record is the structure.
    let mut info = Vec::with_capacity(abi::TOKEN_MANDATORY_LABEL_SIZE);
    info.extend_from_slice(&(low_label_sid as usize).to_le_bytes()); // Label.Sid
    info.extend_from_slice(&abi::SE_GROUP_INTEGRITY.to_le_bytes()); // Label.Attributes
    info.extend_from_slice(&[0u8; 4]);
    ffi::set_token_information(
        token,
        abi::TOKEN_INTEGRITY_LEVEL,
        &info,
        "SetTokenInformation TokenIntegrityLevel (Low)",
    )
}

/// Packs `SID_AND_ATTRIBUTES[count]` (16-byte stride; Attributes stay 0;
/// dsh `buildRestrictingSids`).
fn build_restricting_sids(sids: &[NativePtr]) -> Vec<u8> {
    let mut buffer = vec![0u8; abi::SID_AND_ATTRIBUTES_SIZE * sids.len()];
    for (index, sid) in sids.iter().enumerate() {
        let base = abi::SID_AND_ATTRIBUTES_SIZE * index;
        buffer[base..base + std::mem::size_of::<usize>()]
            .copy_from_slice(&(*sid as usize).to_le_bytes());
    }
    buffer
}

/// The well-known SIDs packed into every restricted token's restricting
/// list (dsh `RestrictingSidSet`).
pub struct RestrictingSidSet {
    /// The Everyone SID the keep-alive group and the ambient-delete deny
    /// name.
    pub world: NativePtr,
}

impl std::fmt::Debug for RestrictingSidSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestrictingSidSet").finish_non_exhaustive()
    }
}

/// Creates the write-restricted token with the mode-selected restricting
/// list (dsh `createRestrictedToken`, verified on Win11 26200):
///  - read-only:       `[logon SID, EVERYONE]`
///  - workspace-write: `[logon SID, EVERYONE, workspace SID, temp SID]`
///
/// The logon SID + EVERYONE keep-alive group is shared by both modes:
/// early DLL init dies with 0xC0000142 and CNG fails without them. The
/// write SIDs join ONLY workspace-write — read-only carries no write
/// SID, so a standing grant ACE from an earlier workspace-write period
/// stays INERT under read-only while the unrevoked ACE keeps the
/// re-upgrade free. Authenticated Users, INTERACTIVE, and LOCAL are
/// absent from BOTH lists (the documented CIM, C:\\ tree-creation, and
/// Public-tree escapes). FAILS CLOSED: any failure throws — never spawn
/// unrestricted.
///
/// # Errors
///
/// Returns the Win32 failure when the call refuses, or the prerequisite
/// refusal when workspace-write supplies no write SID.
pub fn create_restricted_token(
    current_token: HANDLE,
    logon_sid: NativePtr,
    workspace_write: bool,
    write_sids: &[NativePtr],
    known: &RestrictingSidSet,
) -> Result<HANDLE, Win32Error> {
    let mut restricting = vec![logon_sid, known.world];
    if workspace_write {
        if write_sids.is_empty() {
            return Err(Win32Error::new(
                "CreateRestrictedToken",
                0,
                "workspace-write restricting list requires at least one write SID",
            ));
        }
        restricting.extend_from_slice(write_sids);
    }
    let packed = build_restricting_sids(&restricting);
    ffi::create_restricted_token(current_token, &packed, "CreateRestrictedToken")
}
