// Rust guideline compliant 2026-10-29
//! ACL/token-specific Win32 constants (dsh `win32-abi.ts` + the shared
//! `dsh-win32-process` abi table), verified on x64.

// ---------------------------------------------------------------------------
// Process and token rights
// ---------------------------------------------------------------------------

/// OpenProcess access required to query the current process token.
pub const PROCESS_QUERY_INFORMATION: u32 = 0x0400;
/// Token right required by CreateProcessAsUserW.
pub const TOKEN_ASSIGN_PRIMARY: u32 = 0x0001;
/// Token right required by DuplicateTokenEx.
pub const TOKEN_DUPLICATE: u32 = 0x0002;
/// Token right required to read token information.
pub const TOKEN_QUERY: u32 = 0x0008;
/// Token right required to replace the token default DACL or integrity.
pub const TOKEN_ADJUST_DEFAULT: u32 = 0x0080;
/// Group attribute identifying the token logon SID.
pub const SE_GROUP_LOGON_ID: u32 = 0xC000_0000;
/// Group attribute marking the integrity SID of a TOKEN_MANDATORY_LABEL.
pub const SE_GROUP_INTEGRITY: u32 = 0x0000_0020;

// ---------------------------------------------------------------------------
// Access masks
// ---------------------------------------------------------------------------

/// Standard-rights portion excluded from the write capability grant.
pub const STANDARD_RIGHTS_WRITE: u32 = 0x0002_0000;
/// Generic file write access bits.
pub const FILE_GENERIC_WRITE: u32 = 0x0012_0116;
/// Delete or rename an object.
pub const DELETE: u32 = 0x0001_0000;
/// Delete or rename a directory child.
pub const FILE_DELETE_CHILD: u32 = 0x0040;
/// Capability-SID access mask granting write, delete, and child
/// deletion. WRITE_DAC and WRITE_OWNER stay excluded so a confined child
/// cannot rewrite DACLs or take ownership to escape the allowlist.
pub const GRANT_MASK: u32 =
    (FILE_GENERIC_WRITE | DELETE | FILE_DELETE_CHILD) & !STANDARD_RIGHTS_WRITE;
/// Full access used in the restricted token default DACL.
pub const FILE_ALL_ACCESS: u32 = 0x001F_01FF;
/// Generic read access bit.
pub const GENERIC_READ: u32 = 0x8000_0000;
/// Generic write access bit.
pub const GENERIC_WRITE: u32 = 0x4000_0000;

// ---------------------------------------------------------------------------
// CreateRestrictedToken
// ---------------------------------------------------------------------------

/// CreateRestrictedToken flag that disables maximum privileges.
pub const DISABLE_MAX_PRIVILEGE: u32 = 0x1;
/// CreateRestrictedToken limited-user flag.
pub const LUA_TOKEN: u32 = 0x4;
/// Restrict write access to the listed restricting SIDs.
pub const WRITE_RESTRICTED: u32 = 0x8;

// ---------------------------------------------------------------------------
/// WELL_KNOWN_SID_TYPE value for Everyone.
pub const WIN_WORLD_SID: i32 = 1;
/// WELL_KNOWN_SID_TYPE value for the Low mandatory level (S-1-16-4096).
pub const WIN_LOW_LABEL_SID: i32 = 66;
/// TOKEN_INFORMATION_CLASS value for token groups.
pub const TOKEN_GROUPS: i32 = 2;
/// TOKEN_INFORMATION_CLASS value for the token default DACL.
pub const TOKEN_DEFAULT_DACL: i32 = 6;
/// TOKEN_INFORMATION_CLASS value for the token's integrity level.
pub const TOKEN_INTEGRITY_LEVEL: i32 = 25;

// ---------------------------------------------------------------------------
// Security descriptors and ACLs
// ---------------------------------------------------------------------------

/// SECURITY_INFORMATION flag selecting the DACL.
pub const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
/// SECURITY_INFORMATION flag selecting the mandatory integrity label.
pub const LABEL_SECURITY_INFORMATION: u32 = 0x0000_0010;
/// ACE type carrying a mandatory integrity label.
pub const SYSTEM_MANDATORY_LABEL_ACE_TYPE: u8 = 0x11;
/// Mandatory policy denying write-class access to higher-integrity
/// objects. The kernel applies it inside the access check, so it also
/// covers writes and deletes granted through a parent directory's
/// FILE_DELETE_CHILD right — the path the write-restricted pass-2
/// intersection does not reach.
pub const SYSTEM_MANDATORY_LABEL_NO_WRITE_UP: u32 = 0x0000_0001;
/// ACE type for an allowed-access entry.
pub const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
/// ACE type for a denied-access entry (shares the allowed ACE's
/// Mask/SID layout).
pub const ACCESS_DENIED_ACE_TYPE: u8 = 1;
/// ACE inheritance flags for child containers and objects.
pub const SUB_CONTAINERS_AND_OBJECTS_INHERIT: u32 = 0x3;
/// ACE inheritance flag for child containers only (directories; files
/// do not inherit).
pub const CONTAINER_INHERIT_ACE: u32 = 0x2;
/// ACE flag marking inherited entries.
pub const INHERITED_ACE: u16 = 0x10;
/// ACL revision accepted by InitializeAcl and AddMandatoryAce.
pub const ACL_REVISION: u32 = 2;

// ---------------------------------------------------------------------------
// EXPLICIT_ACCESS_W packing (layout verified by the dsh abi-probe)
// ---------------------------------------------------------------------------

/// TRUSTEE_TYPE value used when trustee classification is unknown.
pub const TRUSTEE_IS_UNKNOWN: u32 = 0;
/// TRUSTEE_FORM value indicating a SID pointer.
pub const TRUSTEE_IS_SID: u32 = 0;
/// Trustee record has no chained trustee.
pub const NO_MULTIPLE_TRUSTEE: u32 = 0;
/// EXPLICIT_ACCESS mode that grants access.
pub const GRANT_ACCESS: u32 = 1;
/// EXPLICIT_ACCESS mode that denies access.
pub const DENY_ACCESS: u32 = 3;
/// EXPLICIT_ACCESS mode that revokes access.
pub const REVOKE_ACCESS: u32 = 4;
/// x64 EXPLICIT_ACCESS_W byte size.
pub const EXPLICIT_ACCESS_W_SIZE: usize = 48;

// ---------------------------------------------------------------------------
// SID layout
// ---------------------------------------------------------------------------

/// Maximum SID sub-authority count.
pub const SID_MAX_SUB_AUTHORITIES: u8 = 15;
/// Maximum SID allocation size in bytes.
pub const SECURITY_MAX_SID_SIZE: usize = 68;
/// x64 SID_AND_ATTRIBUTES byte size.
pub const SID_AND_ATTRIBUTES_SIZE: usize = 16;
/// x64 TOKEN_GROUPS offset of the first group entry.
pub const TOKEN_GROUPS_OFFSET: usize = 8;
/// x64 TOKEN_MANDATORY_LABEL byte size (the SID is referenced, not
/// embedded).
pub const TOKEN_MANDATORY_LABEL_SIZE: usize = 16;
/// x64 ACL header byte size (AclRevision, Sbz1, AclSize, AceCount, Sbz2).
pub const ACL_HEADER_SIZE: usize = 8;
/// Bytes a SYSTEM_MANDATORY_LABEL_ACE occupies beyond the ACL header and
/// its SID.
pub const MANDATORY_ACE_OVERHEAD: usize = 8;

// ---------------------------------------------------------------------------
// Memory, files, and wait
// ---------------------------------------------------------------------------

/// LocalAlloc flag selecting zero-initialized fixed memory.
pub const LPTR: u32 = 0x0040;
/// SE_OBJECT_TYPE value for filesystem objects.
pub const SE_FILE_OBJECT: i32 = 1;
/// CreateFile share-read flag.
pub const FILE_SHARE_READ: u32 = 0x0000_0001;
/// CreateFile share-write flag.
pub const FILE_SHARE_WRITE: u32 = 0x0000_0002;
/// CreateFile disposition that opens or creates the file.
pub const OPEN_ALWAYS: u32 = 4;
/// LockFileEx exclusive-lock flag.
pub const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x2;
/// LockFileEx immediate-failure flag.
pub const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x1;
/// STARTUPINFOW uses the standard input, output, and error handles.
pub const STARTF_USESTDHANDLES: u32 = 0x0000_0100;
/// STARTUPINFOW applies wShowWindow when creating a console window.
pub const STARTF_USESHOWWINDOW: u32 = 0x0000_0001;
/// Initial window visibility that preserves the child's console
/// attachment.
pub const SW_HIDE: u16 = 0;
/// HandleInformation flag that permits child inheritance.
pub const HANDLE_FLAG_INHERIT: u32 = 0x1;
/// CreateProcess flag that prevents user code from running before
/// resume.
pub const CREATE_SUSPENDED: u32 = 0x4;
/// WaitForSingleObject returned because a zero-time probe is not
/// signalled.
pub const WAIT_TIMEOUT: u32 = 258;
/// Job limit that terminates every member when the final Job handle
/// closes.
pub const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
/// SetInformationJobObject class for JOBOBJECT_EXTENDED_LIMIT_INFORMATION.
pub const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;
/// x64 JOBOBJECT_EXTENDED_LIMIT_INFORMATION byte size.
pub const JOBOBJECT_EXTENDED_LIMIT_SIZE: usize = 144;
/// Byte offset of BasicLimitInformation.LimitFlags in the extended Job
/// record.
pub const JOBOBJECT_EXTENDED_LIMIT_FLAGS_OFFSET: usize = 16;
/// GetStdHandle selector for standard input.
pub const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6; // -10
/// GetStdHandle selector for standard output.
pub const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5; // -11
/// GetStdHandle selector for standard error.
pub const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4; // -12

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Successful Win32 status code.
pub const ERROR_SUCCESS: u32 = 0;
/// Win32 error reported when a caller-provided buffer is too small.
pub const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
/// Win32 error reporting that the other pipe end closed.
pub const ERROR_BROKEN_PIPE: u32 = 109;
/// Win32 error reporting that a pipe has no remaining data.
pub const ERROR_NO_DATA: u32 = 232;
/// Legacy Win32 maximum path character count used by GetTempPathW.
pub const MAX_PATH: u32 = 260;
