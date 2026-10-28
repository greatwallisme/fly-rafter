// Rust guideline compliant 2026-10-29
//! Checked Win32 call layer (dsh `ffi.ts` + the shared `dsh-win32-process`
//! ffi): the ACL/token binding surface as thin `windows-sys` wrappers,
//! every call checked, every failure reported with the API name, the
//! exact Win32 code, and the formatted system text.

use std::ffi::OsStr;
use std::ffi::OsString;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::ffi::OsStringExt;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Foundation::HANDLE_FLAG_INHERIT;
use windows_sys::Win32::Foundation::HLOCAL;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
use windows_sys::Win32::Security::Authorization::GetNamedSecurityInfoW;
use windows_sys::Win32::Security::Authorization::SetEntriesInAclW;
use windows_sys::Win32::Security::Authorization::SetNamedSecurityInfoW;
use windows_sys::Win32::Security::CopySid;
use windows_sys::Win32::Security::CreateRestrictedToken;
use windows_sys::Win32::Security::CreateWellKnownSid;
use windows_sys::Win32::Security::GetLengthSid;
use windows_sys::Win32::Security::GetTokenInformation;
use windows_sys::Win32::Security::IsValidSid;
use windows_sys::Win32::Security::SetTokenInformation;
use windows_sys::Win32::Storage::FileSystem::CreateFileW;
use windows_sys::Win32::Storage::FileSystem::LockFileEx;
use windows_sys::Win32::Storage::FileSystem::UnlockFileEx;
use windows_sys::Win32::System::Memory::LocalAlloc;
use windows_sys::Win32::System::Threading::OpenProcess;
use windows_sys::Win32::System::Threading::OpenProcessToken;

use super::win32_abi as abi;

/// A native pointer (the dsh `NativePtr`): owned Win32 allocations and
/// handles move through the checked helpers below.
pub type NativePtr = *mut c_void;

/// A Win32 call failure with the exact API name and code (dsh
/// `Win32Error`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Win32Error {
    /// Win32 function whose checked result failed.
    pub api: &'static str,
    /// Exact GetLastError value or direct Win32 API error code.
    pub win32_code: u32,
    /// Operation context and/or the formatted system text.
    pub detail: String,
}

impl Win32Error {
    /// Builds the failure for one API call.
    #[must_use]
    pub fn new(api: &'static str, win32_code: u32, detail: impl Into<String>) -> Self {
        Self {
            api,
            win32_code,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for Win32Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} failed (Win32 {}): {}",
            self.api, self.win32_code, self.detail
        )
    }
}

impl std::error::Error for Win32Error {}

/// Whether a pointer is null.
#[must_use]
pub fn is_null_ptr(value: NativePtr) -> bool {
    value.is_null()
}

/// Whether CreateFileW produced INVALID_HANDLE_VALUE.
#[must_use]
pub fn is_invalid_handle(handle: HANDLE) -> bool {
    // SAFETY: HANDLE is a raw pointer value; comparing bits cannot
    // dereference it.
    let bits = handle as usize;
    bits == 0 || bits == usize::MAX
}

/// Encodes one string as UTF-16 with the NUL terminator.
pub fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Decodes one NUL-terminated UTF-16 buffer into a string.
#[must_use]
pub fn from_wide(buffer: &[u16]) -> String {
    let end = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    OsString::from_wide(&buffer[..end])
        .to_string_lossy()
        .into_owned()
}

/// Formats a Win32 error code through FormatMessageW (dsh `errorText`).
#[must_use]
pub fn error_text(win32_code: u32) -> String {
    // SAFETY: FormatMessageW into a fixed 1024-WCHAR buffer with no
    // arguments; the call writes at most the buffer capacity.
    let mut buffer = [0u16; 1024];
    let length = unsafe {
        windows_sys::Win32::System::Diagnostics::Debug::FormatMessageW(
            0x0000_1000 | 0x0000_0200, // FORMAT_MESSAGE_FROM_SYSTEM | IGNORE_INSERTS
            std::ptr::null(),
            win32_code,
            0,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            std::ptr::null(),
        )
    };
    if length == 0 {
        return String::new();
    }
    from_wide(&buffer[..length as usize]).trim().to_owned()
}

/// The captured last error of the calling thread.
#[must_use]
pub fn last_error() -> u32 {
    // SAFETY: GetLastError reads the calling thread's slot.
    unsafe { GetLastError() }
}

/// Builds the [`Win32Error`] for the calling thread's last error.
#[must_use]
pub fn win32_error(api: &'static str, detail: impl Into<String>) -> Win32Error {
    let code = last_error();
    let text = error_text(code);
    let detail = detail.into();
    let joined = if text.is_empty() {
        detail
    } else if detail.is_empty() {
        text
    } else {
        format!("{detail}: {text}")
    };
    Win32Error::new(api, code, joined)
}

/// Closes one handle, reporting a labelled failure (dsh
/// `closeHandleChecked`).
///
/// # Errors
///
/// Returns the Win32 failure when CloseHandle refuses the handle.
pub fn close_handle_checked(handle: HANDLE, detail: &str) -> Result<(), Win32Error> {
    // SAFETY: the caller owns the handle and passes it exactly once.
    if unsafe { CloseHandle(handle) } == 0 {
        return Err(win32_error("CloseHandle", detail));
    }
    Ok(())
}

/// Allocates zero-initialized fixed memory (LocalAlloc LPTR).
///
/// # Errors
///
/// Returns the Win32 failure when the allocation refuses.
pub fn local_alloc(bytes: usize) -> Result<NativePtr, Win32Error> {
    // SAFETY: LocalAlloc with LPTR returns a fresh zeroed allocation of
    // the requested size or null.
    let pointer = unsafe { LocalAlloc(abi::LPTR, bytes) } as NativePtr;
    if is_null_ptr(pointer) {
        return Err(win32_error("LocalAlloc", format!("{bytes} bytes")));
    }
    Ok(pointer)
}

/// Frees one local allocation; a non-null return is the failure (dsh
/// LocalFree contract).
///
/// # Errors
///
/// Returns the Win32 failure when LocalFree refuses the block.
pub fn local_free(block: NativePtr, detail: &str) -> Result<(), Win32Error> {
    // SAFETY: the caller owns the block and passes it exactly once.
    let freed = unsafe { LocalFree(block as HLOCAL) };
    if !freed.is_null() {
        return Err(win32_error("LocalFree", detail));
    }
    Ok(())
}

/// Opens the current process with the query right (dsh `OpenProcess`
/// call inside `openCurrentProcessToken`).
///
/// # Errors
///
/// Returns the Win32 failure when the process refuses the open.
pub fn open_process_query(pid: u32, detail: &str) -> Result<HANDLE, Win32Error> {
    // SAFETY: OpenProcess with PROCESS_QUERY_INFORMATION returns an
    // owned handle or null.
    let handle = unsafe { OpenProcess(abi::PROCESS_QUERY_INFORMATION, 0, pid) };
    if is_invalid_handle(handle) {
        return Err(win32_error("OpenProcess", detail));
    }
    Ok(handle)
}

/// Opens one process's access token (dsh `OpenProcessToken`).
///
/// # Errors
///
/// Returns the Win32 failure when the token refuses the open.
pub fn open_process_token(
    process: HANDLE,
    desired_access: u32,
    detail: &str,
) -> Result<HANDLE, Win32Error> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the caller owns the process handle; the out-slot is a
    // local HANDLE.
    if unsafe { OpenProcessToken(process, desired_access, &mut token) } == 0 {
        return Err(win32_error("OpenProcessToken", detail));
    }
    Ok(token)
}

/// Parses one SDDL SID string into a fresh allocation (dsh
/// `ConvertStringSidToSidW` arm).
///
/// # Errors
///
/// Returns the Win32 failure for an unparsable SID.
pub fn convert_string_sid_to_sid(string_sid: &str) -> Result<NativePtr, Win32Error> {
    let wide_sid = wide(string_sid);
    let mut sid: NativePtr = std::ptr::null_mut();
    // SAFETY: the wide string outlives the call; the out-slot is local.
    if unsafe { ConvertStringSidToSidW(wide_sid.as_ptr(), (&mut sid as *mut NativePtr).cast()) }
        == 0
    {
        return Err(win32_error("ConvertStringSidToSidW", string_sid));
    }
    if is_null_ptr(sid) {
        return Err(win32_error(
            "ConvertStringSidToSidW",
            format!("null SID for {string_sid}"),
        ));
    }
    Ok(sid)
}

/// Creates one well-known SID (68-byte buffer) and asserts its validity
/// (dsh `makeWellKnownSid`).
///
/// # Errors
///
/// Returns the Win32 failure for either refusing call.
pub fn create_well_known_sid(kind: i32) -> Result<NativePtr, Win32Error> {
    let sid = local_alloc(abi::SECURITY_MAX_SID_SIZE)?;
    let mut size: u32 = abi::SECURITY_MAX_SID_SIZE as u32;
    // SAFETY: the SID buffer owns `size` bytes; the size slot is local.
    if unsafe { CreateWellKnownSid(kind, std::ptr::null_mut(), sid, &raw mut size) } == 0 {
        let error = win32_error("CreateWellKnownSid", format!("type {kind}"));
        let _ignored = local_free(sid, "CreateWellKnownSid buffer");
        return Err(error);
    }
    // SAFETY: IsValidSid reads the header the prior call produced.
    if unsafe { IsValidSid(sid) } == 0 {
        let error = win32_error("IsValidSid", format!("CreateWellKnownSid type {kind}"));
        let _ignored = local_free(sid, "CreateWellKnownSid buffer");
        return Err(error);
    }
    Ok(sid)
}

/// The byte length of one SID.
///
/// # Errors
///
/// Returns the Win32 failure when GetLengthSid refuses.
pub fn get_length_sid(sid: NativePtr, detail: &str) -> Result<usize, Win32Error> {
    // SAFETY: the SID allocation is valid for the call.
    let length = unsafe { GetLengthSid(sid) };
    if length == 0 {
        return Err(win32_error("GetLengthSid", detail));
    }
    Ok(length as usize)
}

/// Copies one SID into a fresh allocation (dsh `CopySid` arm).
///
/// # Errors
///
/// Returns the Win32 failure when CopySid refuses.
pub fn copy_sid(source: NativePtr, detail: &str) -> Result<NativePtr, Win32Error> {
    let length = get_length_sid(source, detail)?;
    let destination = local_alloc(length)?;
    // SAFETY: both allocations are valid for the call and sized by
    // GetLengthSid.
    if unsafe { CopySid(length as u32, destination, source) } == 0 {
        let error = win32_error("CopySid", detail);
        let _ignored = local_free(destination, "CopySid buffer");
        return Err(error);
    }
    Ok(destination)
}

/// Reads one token information class into a fresh buffer (dsh
/// `GetTokenInformation` two-call shape).
///
/// # Errors
///
/// Returns the Win32 failure when either call refuses.
pub fn get_token_information(
    token: HANDLE,
    class: i32,
    detail: &str,
) -> Result<Vec<u8>, Win32Error> {
    let mut needed: u32 = 0;
    // SAFETY: the size probe passes a null buffer; the slot is local.
    unsafe {
        GetTokenInformation(token, class, std::ptr::null_mut(), 0, &raw mut needed);
    }
    if needed == 0 {
        return Err(win32_error(
            "GetTokenInformation",
            format!("{detail} size query"),
        ));
    }
    let mut buffer = vec![0u8; needed as usize];
    // SAFETY: the buffer owns `needed` bytes; the slot is local.
    if unsafe {
        GetTokenInformation(
            token,
            class,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            &mut needed,
        )
    } == 0
    {
        return Err(win32_error("GetTokenInformation", detail));
    }
    Ok(buffer)
}

/// Writes one token information class (dsh `SetTokenInformation` arm).
///
/// # Errors
///
/// Returns the Win32 failure when the call refuses.
pub fn set_token_information(
    token: HANDLE,
    class: i32,
    information: &[u8],
    detail: &str,
) -> Result<(), Win32Error> {
    // SAFETY: the information buffer owns the stated length.
    if unsafe {
        SetTokenInformation(
            token,
            class,
            information.as_ptr().cast(),
            information.len() as u32,
        )
    } == 0
    {
        return Err(win32_error("SetTokenInformation", detail));
    }
    Ok(())
}

/// Creates the write-restricted token (dsh `CreateRestrictedToken` arm).
///
/// # Errors
///
/// Returns the Win32 failure when the call refuses.
pub fn create_restricted_token(
    current_token: HANDLE,
    restricting_sids: &[u8],
    detail: &str,
) -> Result<HANDLE, Win32Error> {
    let mut new_token: HANDLE = std::ptr::null_mut();
    let count = restricting_sids.len() / abi::SID_AND_ATTRIBUTES_SIZE;
    // SAFETY: the packed SID_AND_ATTRIBUTES array owns the stated
    // count; every disable/delete count is zero with null pointers.
    if unsafe {
        CreateRestrictedToken(
            current_token,
            abi::DISABLE_MAX_PRIVILEGE | abi::LUA_TOKEN | abi::WRITE_RESTRICTED,
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
            count as u32,
            restricting_sids.as_ptr().cast(),
            &mut new_token,
        )
    } == 0
    {
        return Err(win32_error(
            "CreateRestrictedToken",
            format!("{detail}: restricting SIDs: {count}"),
        ));
    }
    Ok(new_token)
}

/// Merges packed EXPLICIT_ACCESS_W entries into an ACL (dsh
/// `SetEntriesInAclW` arm).
///
/// # Errors
///
/// Returns the Win32 failure with the explicit code when the merge
/// refuses.
pub fn set_entries_in_acl(
    entries: &[u8],
    old_acl: NativePtr,
    detail: &str,
) -> Result<NativePtr, Win32Error> {
    let count = entries.len() / abi::EXPLICIT_ACCESS_W_SIZE;
    let mut new_acl: NativePtr = std::ptr::null_mut();
    // SAFETY: the entries buffer owns `count` packed records; the old
    // ACL (or null) is owned by the caller for the duration.
    let result = unsafe {
        SetEntriesInAclW(
            count as u32,
            entries.as_ptr().cast(),
            old_acl.cast(),
            (&mut new_acl as *mut NativePtr).cast(),
        )
    };
    if result != abi::ERROR_SUCCESS {
        return Err(Win32Error::new("SetEntriesInAclW", result, detail));
    }
    if is_null_ptr(new_acl) {
        return Err(Win32Error::new(
            "SetEntriesInAclW",
            result,
            format!("{detail}: null new ACL"),
        ));
    }
    Ok(new_acl)
}

/// Reads a directory's current DACL and label ACL (dsh
/// `GetNamedSecurityInfoW` arm). The returned pointers sit INSIDE the
/// descriptor allocation — only the descriptor may be LocalFree'd, and
/// not before SetEntriesInAclW has consumed the ACL.
///
/// # Errors
///
/// Returns the Win32 failure with the explicit code when the read
/// refuses.
pub fn get_named_security_info(
    path: &str,
) -> Result<(NativePtr, NativePtr, NativePtr), Win32Error> {
    let wide_path = wide(path);
    let mut owner: NativePtr = std::ptr::null_mut();
    let mut group: NativePtr = std::ptr::null_mut();
    let mut dacl: NativePtr = std::ptr::null_mut();
    let mut sacl: NativePtr = std::ptr::null_mut();
    let mut descriptor: NativePtr = std::ptr::null_mut();
    // SAFETY: the wide path outlives the call; every out-slot is local.
    let result = unsafe {
        GetNamedSecurityInfoW(
            wide_path.as_ptr(),
            abi::SE_FILE_OBJECT,
            abi::DACL_SECURITY_INFORMATION | abi::LABEL_SECURITY_INFORMATION,
            (&mut owner as *mut NativePtr).cast(),
            (&mut group as *mut NativePtr).cast(),
            (&mut dacl as *mut NativePtr).cast(),
            (&mut sacl as *mut NativePtr).cast(),
            (&mut descriptor as *mut NativePtr).cast(),
        )
    };
    if result != abi::ERROR_SUCCESS {
        return Err(Win32Error::new("GetNamedSecurityInfoW", result, path));
    }
    Ok((dacl, sacl, descriptor))
}

/// Applies a merged DACL and optional label ACL in one call (dsh
/// `SetNamedSecurityInfoW` arm).
///
/// # Errors
///
/// Returns the Win32 failure with the explicit code when the apply
/// refuses.
pub fn set_named_security_info(
    path: &str,
    dacl: NativePtr,
    label_acl: Option<NativePtr>,
) -> Result<(), Win32Error> {
    let wide_path = wide(path);
    let information = match label_acl {
        Some(_) => abi::DACL_SECURITY_INFORMATION | abi::LABEL_SECURITY_INFORMATION,
        None => abi::DACL_SECURITY_INFORMATION,
    };
    // SAFETY: the ACL pointers are owned by the caller for the
    // duration; the wide path outlives the call.
    let label: *const windows_sys::Win32::Security::ACL = match label_acl {
        Some(acl) => acl.cast(),
        None => std::ptr::null(),
    };
    let result = unsafe {
        SetNamedSecurityInfoW(
            wide_path.as_ptr(),
            abi::SE_FILE_OBJECT,
            information,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl.cast(),
            label,
        )
    };
    if result != abi::ERROR_SUCCESS {
        return Err(Win32Error::new("SetNamedSecurityInfoW", result, path));
    }
    Ok(())
}

/// Creates or opens the per-path lock file (dsh `CreateFileW` arm of
/// `withPathLock`).
///
/// # Errors
///
/// Returns the Win32 failure when the open refuses.
pub fn open_lock_file(path: &str) -> Result<HANDLE, Win32Error> {
    let wide_path = wide(path);
    // SAFETY: the wide path outlives the call; the handle is owned by
    // the caller.
    let handle = unsafe {
        CreateFileW(
            wide_path.as_ptr(),
            abi::GENERIC_READ | abi::GENERIC_WRITE,
            abi::FILE_SHARE_READ | abi::FILE_SHARE_WRITE,
            std::ptr::null(),
            abi::OPEN_ALWAYS,
            0,
            std::ptr::null_mut(),
        )
    };
    if is_invalid_handle(handle) {
        return Err(win32_error("CreateFileW", path));
    }
    Ok(handle)
}

/// Takes the one-byte exclusive range lock (dsh `LockFileEx` arm).
///
/// # Errors
///
/// Returns the Win32 failure when the lock refuses.
pub fn lock_file_exclusive(handle: HANDLE, detail: &str) -> Result<(), Win32Error> {
    let mut overlapped = Overlapped::zeroed();
    // SAFETY: the overlapped record stays zeroed (offset 0, hEvent
    // null) and outlives the synchronous call.
    if unsafe {
        LockFileEx(
            handle,
            abi::LOCKFILE_EXCLUSIVE_LOCK,
            0,
            1,
            0,
            (&mut overlapped.0) as *mut _,
        )
    } == 0
    {
        return Err(win32_error("LockFileEx", detail));
    }
    Ok(())
}

/// Releases the one-byte range lock (dsh `UnlockFileEx` arm).
///
/// # Errors
///
/// Returns the Win32 failure when the unlock refuses.
pub fn unlock_file_exclusive(handle: HANDLE, detail: &str) -> Result<(), Win32Error> {
    let mut overlapped = Overlapped::zeroed();
    // SAFETY: as above; the zeroed record governs the same range.
    if unsafe { UnlockFileEx(handle, 0, 1, 0, (&mut overlapped.0) as *mut _) } == 0 {
        return Err(win32_error("UnlockFileEx", detail));
    }
    Ok(())
}

/// A zeroed OVERLAPPED record (LockFileEx on a synchronous handle reads
/// offset 0 and a null event from it).
pub struct Overlapped(
    /// The raw record; constructed zeroed and never mutated.
    pub windows_sys::Win32::System::IO::OVERLAPPED,
);

impl std::fmt::Debug for Overlapped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The windows-sys record is not Debug; the wrapper always holds
        // the zeroed construction, so the constant rendering is exact.
        f.write_str("Overlapped(zeroed)")
    }
}

impl Overlapped {
    /// Builds the zeroed record.
    #[must_use]
    pub fn zeroed() -> Self {
        Self(windows_sys::Win32::System::IO::OVERLAPPED {
            Internal: 0,
            InternalHigh: 0,
            Anonymous: windows_sys::Win32::System::IO::OVERLAPPED_0 {
                Pointer: std::ptr::null_mut(),
            },
            hEvent: std::ptr::null_mut(),
        })
    }
}

/// Marks one handle inheritable by children (dsh
/// `SetHandleInformation` arm).
///
/// # Errors
///
/// Returns the Win32 failure when the call refuses.
pub fn set_handle_inherit(handle: HANDLE, detail: &str) -> Result<(), Win32Error> {
    // SAFETY: SetHandleInformation toggles the inherit bit on a
    // caller-owned handle.
    if unsafe {
        windows_sys::Win32::Foundation::SetHandleInformation(
            handle,
            HANDLE_FLAG_INHERIT,
            HANDLE_FLAG_INHERIT,
        )
    } == 0
    {
        return Err(win32_error("SetHandleInformation", detail));
    }
    Ok(())
}

/// Clears one handle's inheritable bit (best-effort restore, dsh
/// cleanup arm).
pub fn clear_handle_inherit(handle: HANDLE) {
    // SAFETY: the caller owns the handle; a cleanup failure must not
    // mask the child's outcome.
    unsafe {
        windows_sys::Win32::Foundation::SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
    };
}
