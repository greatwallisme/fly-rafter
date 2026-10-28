// Rust guideline compliant 2026-10-29
//! One write-restricted sandbox instance: token + write-SID grants +
//! spawn (dsh `sandbox-windows-acl/src/index.ts` `AclSandbox`).
//!
//! `init()` is fail-closed — any Win32 failure revokes the revocable
//! (temp) grants and throws; `dispose()` revokes the temp grants,
//! leaves the standing workspace ACEs in place (the cross-instance
//! reuse cache), frees every allocation, and reports every cleanup
//! failure. With `manage_dacls = false` the caller owns the grants (the
//! sandbox seam's grant reuse): init applies none and dispose revokes
//! none.

use windows_sys::Win32::Foundation::HANDLE;

use super::acl;
use super::ffi;
use super::ffi::NativePtr;
use super::ffi::Win32Error;
use super::spawn;
use super::token;
use super::win32_abi as abi;

/// Construction options (dsh `AclSandboxOptions`): the writable
/// directories, the private temp grant, and their distinct SID
/// identities.
#[derive(Debug, Clone)]
pub struct AclSandboxOptions {
    /// Directories the confined child may write into (must exist and be
    /// caller-owned).
    pub writable_dirs: Vec<String>,
    /// Existing private temp directory to grant: `Some` grants it,
    /// `None` disables temp writes. Read-only accepts only `None`.
    pub temp_dir: Option<String>,
    /// The write SID forming the workspace-write allowlist: required
    /// under workspace-write, ignored (and must be absent) under
    /// read-only.
    pub write_sid: Option<String>,
    /// The private temp directory's write SID, required whenever
    /// workspace-write grants a temp directory, absent otherwise; it
    /// must be distinct from the workspace SID.
    pub temp_write_sid: Option<String>,
    /// The file-effect mode this instance confines under — selects the
    /// restricted token's restricting-SID list and MUST match the grant
    /// shape: read-only pairs with zero grants.
    pub mode: ConfinedWord,
    /// Whether this instance owns its DACL grants (default true). False
    /// means the CALLER has already materialized the ACEs (the sandbox
    /// seam's grant lifecycle): init/dispose skip grant/revoke entirely.
    pub manage_dacls: bool,
}

/// The confining mode word the typed seam trusts (the runner validates
/// the argv-borne string at its boundary).
pub type ConfinedWord = crate::seam::ConfinedMode;

/// A settled confined child: captured stdio and the exit code (dsh
/// `AclSandboxChildResult`).
#[derive(Debug)]
pub struct ChildResult {
    /// The child's stdout bytes.
    pub stdout: Vec<u8>,
    /// The child's stderr bytes.
    pub stderr: Vec<u8>,
    /// The child's direct exit code.
    pub exit_code: u32,
}

/// The spawn stdio shape (dsh `stdio` option).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stdio {
    /// Capture stdout/stderr via anonymous pipes.
    Piped,
    /// The child inherits the caller's stdio directly (runner usage —
    /// bytes flow straight through), always wrapped in a kill-on-close
    /// job so the child dies with the caller.
    Inherit,
}

/// One spawn request (dsh `AclSandboxSpawnOptions`).
#[derive(Debug)]
pub struct SpawnOptions<'a> {
    /// Program to run (resolved via PATH search when unqualified).
    pub command: &'a str,
    /// Arguments, quoted per CommandLineToArgvW rules.
    pub args: &'a [String],
    /// Working directory; defaults to the caller's cwd.
    pub cwd: Option<&'a str>,
    /// The stdio shape.
    pub stdio: Stdio,
    /// The control-pipe handle forwarded through the CRT descriptor
    /// table at fd 7 in inherited-stdio mode.
    pub control: Option<HANDLE>,
}

impl Default for AclSandboxOptions {
    fn default() -> Self {
        Self {
            writable_dirs: Vec::new(),
            temp_dir: None,
            write_sid: None,
            temp_write_sid: None,
            mode: ConfinedWord::ReadOnly,
            manage_dacls: true,
        }
    }
}

/// One write-restricted sandbox instance (dsh `AclSandbox`).
#[derive(Debug)]
pub struct AclSandbox {
    /// Absolute writable directories (constructor-validated).
    pub writable_dirs: Vec<String>,
    /// The workspace SID string whose ACEs form the workspace allowlist.
    pub write_sid: Option<String>,
    /// The private temp directory's write SID (workspace-write with
    /// temp only).
    pub temp_write_sid: Option<String>,
    /// The file-effect mode — the restricting-SID list selection.
    pub mode: ConfinedWord,
    temp_dir_option: Option<String>,
    manage_dacls: bool,
    temp_dir_resolved: Option<String>,
    token: Option<HANDLE>,
    write_sid_ptr: Option<NativePtr>,
    temp_write_sid_ptr: Option<NativePtr>,
    /// The well-known/logon SID allocations init makes; freed by dispose
    /// alongside the write SIDs.
    sid_allocations: Vec<NativePtr>,
    granted_paths: Vec<(String, NativePtr)>,
}

impl AclSandbox {
    /// Validates the option shape (dsh constructor, all eight refusals
    /// verbatim): writable directories must exist, workspace-write
    /// requires a write SID and an explicit temp option (a directory or
    /// `None`), read-only accepts neither, a granted temp requires its
    /// distinct SID, and the two write SIDs must differ.
    ///
    /// # Errors
    ///
    /// Returns the verbatim dsh refusal for the first violated rule.
    pub fn new(options: &AclSandboxOptions) -> Result<Self, String> {
        let mut writable_dirs = Vec::with_capacity(options.writable_dirs.len());
        for directory in &options.writable_dirs {
            let absolute = std::path::Path::new(directory)
                .canonicalize()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|_| directory.clone());
            let metadata = std::fs::metadata(&absolute).map_err(|_| {
                format!("AclSandbox writable dir does not exist or is not a directory: {absolute}")
            })?;
            if !metadata.is_dir() {
                return Err(format!(
                    "AclSandbox writable dir does not exist or is not a directory: {absolute}"
                ));
            }
            writable_dirs.push(absolute);
        }
        if options.mode == ConfinedWord::WorkspaceWrite && options.write_sid.is_none() {
            return Err(
                "AclSandbox workspace-write requires a write SID — derive it from the workspace \
via workspaceWriteSid()"
                    .to_owned(),
            );
        }
        if options.mode == ConfinedWord::WorkspaceWrite && options.temp_dir.is_none() {
            return Err(
                "AclSandbox workspace-write requires an explicit private temp directory or null"
                    .to_owned(),
            );
        }
        if options.mode == ConfinedWord::ReadOnly && options.temp_dir.is_some() {
            return Err("AclSandbox read-only does not accept a temp directory".to_owned());
        }
        if options.mode == ConfinedWord::ReadOnly
            && (options.write_sid.is_some() || options.temp_write_sid.is_some())
        {
            return Err("AclSandbox read-only does not accept write SIDs".to_owned());
        }
        if options.mode == ConfinedWord::WorkspaceWrite
            && options.temp_dir.is_some()
            && options.temp_write_sid.is_none()
        {
            return Err(
                "AclSandbox workspace-write with temp requires a temp write SID — derive it via \
tempWriteSid()"
                    .to_owned(),
            );
        }
        if options.temp_dir.is_none() && options.temp_write_sid.is_some() {
            return Err("AclSandbox temp write SID requires a temp directory".to_owned());
        }
        if options.write_sid.is_some()
            && options.temp_write_sid.is_some()
            && options.write_sid == options.temp_write_sid
        {
            return Err("AclSandbox workspace and temp write SIDs must be distinct".to_owned());
        }
        Ok(Self {
            writable_dirs,
            write_sid: options.write_sid.clone(),
            temp_write_sid: options.temp_write_sid.clone(),
            mode: options.mode,
            temp_dir_option: options.temp_dir.clone(),
            manage_dacls: options.manage_dacls,
            temp_dir_resolved: None,
            token: None,
            write_sid_ptr: None,
            temp_write_sid_ptr: None,
            sid_allocations: Vec::new(),
            granted_paths: Vec::new(),
        })
    }

    /// The resolved temp directory (available after init; `None` when
    /// temp grants are disabled).
    #[must_use]
    pub fn temp_dir(&self) -> Option<&str> {
        self.temp_dir_resolved.as_deref()
    }

    /// Creates the restricted token and applies the capability-SID
    /// grants (dsh `init`). Idempotent-unsafe: once per instance.
    ///
    /// # Errors
    ///
    /// Fail-closed: returns the first failure and cleans every
    /// revocable grant and SID allocation before it propagates.
    pub fn init(&mut self) -> Result<(), String> {
        if self.token.is_some() {
            return Err("AclSandbox is already initialized".to_owned());
        }
        match self.init_inner() {
            Ok(()) => Ok(()),
            Err((error, cleanup)) => {
                let mut failures: Vec<String> = vec![error];
                failures.extend(cleanup);
                if failures.len() > 1 {
                    Err(format!(
                        "AclSandbox init failed and {} cleanup operation(s) also failed: {}",
                        failures.len() - 1,
                        failures.join("; ")
                    ))
                } else {
                    Err(failures.join("; "))
                }
            }
        }
    }

    /// The init body; the error carries the cleanup failures collected
    /// on the way out.
    fn init_inner(&mut self) -> Result<(), (String, Vec<String>)> {
        let to_string = |error: &Win32Error| error.to_string();
        let parsed_write_sid = match &self.write_sid {
            Some(sid) => match ffi::convert_string_sid_to_sid(sid)
                .map_err(|error| (to_string(&error), Vec::new()))
            {
                Ok(ptr) => Some(ptr),
                Err(failure) => return Err(failure),
            },
            None => None,
        };
        let parsed_temp_sid = match &self.temp_write_sid {
            Some(sid) => match ffi::convert_string_sid_to_sid(sid)
                .map_err(|error| (to_string(&error), Vec::new()))
            {
                Ok(ptr) => Some(ptr),
                Err(failure) => return Err(failure),
            },
            None => None,
        };
        self.write_sid_ptr = parsed_write_sid;
        self.temp_write_sid_ptr = parsed_temp_sid;
        let temp_dir: Option<String> = if self.mode == ConfinedWord::ReadOnly {
            None
        } else {
            self.temp_dir_option.clone()
        };
        if let Some(temp_dir) = &temp_dir {
            let metadata = std::fs::metadata(temp_dir).map_err(|_| {
                (
                    format!("AclSandbox temp dir does not exist or is not a directory: {temp_dir}"),
                    Vec::new(),
                )
            })?;
            if !metadata.is_dir() {
                return Err((
                    format!("AclSandbox temp dir does not exist or is not a directory: {temp_dir}"),
                    Vec::new(),
                ));
            }
            if let Err(error) =
                crate::path_boundary::assert_private_temp_disjoint(&self.writable_dirs, temp_dir)
            {
                return Err((error, Vec::new()));
            }
        }
        self.temp_dir_resolved = temp_dir.clone();
        let low_label_sid = match ffi::create_well_known_sid(abi::WIN_LOW_LABEL_SID) {
            Ok(sid) => sid,
            Err(error) => return Err((to_string(&error), Vec::new())),
        };
        self.sid_allocations.push(low_label_sid);
        let world_sid = match ffi::create_well_known_sid(abi::WIN_WORLD_SID) {
            Ok(sid) => sid,
            Err(error) => return Err((to_string(&error), Vec::new())),
        };
        self.sid_allocations.push(world_sid);

        let mut granted: Vec<(String, NativePtr)> = Vec::new();
        if self.manage_dacls {
            if let Some(write_sid_ptr) = self.write_sid_ptr {
                for path in &self.writable_dirs {
                    if let Err(error) =
                        acl::grant_write(path, write_sid_ptr, low_label_sid, world_sid)
                    {
                        return Err((to_string(&error), Vec::new()));
                    }
                }
                if let (Some(temp_dir), Some(temp_write_sid_ptr)) =
                    (&temp_dir, self.temp_write_sid_ptr)
                {
                    // Record BEFORE granting: grantWrite can throw after
                    // a successful apply, and the fail-closed catch must
                    // still revoke that path (revoking an ungranted path
                    // is a no-op merge).
                    granted.push((temp_dir.clone(), temp_write_sid_ptr));
                    if let Err(error) =
                        acl::grant_write(temp_dir, temp_write_sid_ptr, low_label_sid, world_sid)
                    {
                        self.granted_paths = granted;
                        return Err((to_string(&error), Vec::new()));
                    }
                }
            }
        }
        self.granted_paths = granted;

        let current_token = match token::open_current_process_token() {
            Ok(token) => token,
            Err(error) => return Err((to_string(&error), Vec::new())),
        };
        let logon_sid = match token::find_logon_sid(current_token) {
            Ok(sid) => sid,
            Err(error) => {
                let _ignored = ffi::close_handle_checked(current_token, "current process token");
                return Err((to_string(&error), Vec::new()));
            }
        };
        self.sid_allocations.push(logon_sid);
        let write_sids: Vec<NativePtr> = [self.write_sid_ptr, self.temp_write_sid_ptr]
            .into_iter()
            .flatten()
            .collect();
        let restricted = token::create_restricted_token(
            current_token,
            logon_sid,
            self.mode == ConfinedWord::WorkspaceWrite,
            &write_sids,
            &token::RestrictingSidSet { world: world_sid },
        );
        let restricted = match restricted {
            Ok(token) => token,
            Err(error) => {
                let _ignored = ffi::close_handle_checked(current_token, "current process token");
                return Err((to_string(&error), Vec::new()));
            }
        };
        if let Err(error) = ffi::close_handle_checked(current_token, "current process token") {
            let _ignored =
                ffi::close_handle_checked(restricted, "restricted token after close failure");
            return Err((to_string(&error), Vec::new()));
        }
        if let Err(error) = token::restrict_token_integrity(restricted, low_label_sid) {
            let _ignored =
                ffi::close_handle_checked(restricted, "restricted token after integrity failure");
            return Err((to_string(&error), Vec::new()));
        }
        // The restricted token's default DACL names only the user's
        // ambient SIDs; merge a full-access ACE for a restricting SID so
        // new-object creation stays gated by the parent's DACL while
        // the new object's own DACL passes pass-2. Choosing the temp
        // SID prevents default-DACL objects in one session's temp tree
        // from acquiring the shared workspace capability.
        let default_dacl_sid = self
            .temp_write_sid_ptr
            .or(self.write_sid_ptr)
            .unwrap_or(world_sid);
        if let Err(error) = token::set_token_default_dacl_grant(restricted, default_dacl_sid) {
            let _ignored =
                ffi::close_handle_checked(restricted, "restricted token after DACL failure");
            return Err((to_string(&error), Vec::new()));
        }
        self.token = Some(restricted);
        Ok(())
    }

    /// Spawns a process under the restricted token (dsh `spawn`). Fails
    /// closed: throws on every Win32 failure; the child is never created
    /// unrestricted. Call dispose only after all children have exited —
    /// revoking grants under a live child removes its remaining write
    /// allowance.
    ///
    /// # Errors
    ///
    /// Returns the refusal for an uninitialized sandbox or a refusing
    /// Win32 call.
    pub fn spawn(&self, options: &SpawnOptions<'_>) -> Result<Child, String> {
        let Some(token) = self.token else {
            return Err("AclSandbox is not initialized: call init() first".to_owned());
        };
        if options.control.is_some() && options.stdio != Stdio::Inherit {
            return Err("control pipe requires inherited stdio".to_owned());
        }
        let cwd = options.cwd.unwrap_or_else(|| ".");
        let restricted = spawn::RestrictedSpawn {
            token,
            command: options.command,
            args: options.args,
            cwd,
            control: options.control,
        };
        match options.stdio {
            Stdio::Piped => {
                let child =
                    spawn::spawn_piped_process(&restricted).map_err(|error| error.to_string())?;
                Ok(Child::Piped(child))
            }
            Stdio::Inherit => {
                let child = spawn::spawn_inherited_job_process(&restricted)
                    .map_err(|error| error.to_string())?;
                Ok(Child::Inherited(child))
            }
        }
    }

    /// Revokes the revocable (temp) grants, frees the SIDs, closes the
    /// token (dsh `dispose`); the standing workspace ACEs stay. Reports
    /// every cleanup failure.
    ///
    /// # Errors
    ///
    /// Returns every cleanup failure joined, if any occurred.
    pub fn dispose(&mut self) -> Result<(), String> {
        let mut failures: Vec<String> = Vec::new();
        if self.manage_dacls {
            for (path, sid_ptr) in &self.granted_paths {
                if let Err(error) = acl::revoke_write(path, *sid_ptr) {
                    failures.push(error.to_string());
                }
            }
        }
        self.granted_paths.clear();
        for (label, sid) in [
            ("workspace write SID", self.write_sid_ptr),
            ("temp write SID", self.temp_write_sid_ptr),
        ] {
            if let Some(sid) = sid {
                if let Err(error) = ffi::local_free(sid, label) {
                    failures.push(error.to_string());
                }
            }
        }
        self.write_sid_ptr = None;
        self.temp_write_sid_ptr = None;
        if let Some(token) = self.token.take() {
            if let Err(error) = ffi::close_handle_checked(token, "restricted token") {
                failures.push(error.to_string());
            }
        }
        for sid in std::mem::take(&mut self.sid_allocations) {
            if let Err(error) = ffi::local_free(sid, "init SID allocation") {
                failures.push(error.to_string());
            }
        }
        self.temp_dir_resolved = None;
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "AclSandbox dispose completed with {} cleanup failure(s): {}",
                failures.len(),
                failures.join("; ")
            ))
        }
    }
}

impl Drop for AclSandbox {
    fn drop(&mut self) {
        // The documented dispose path is the explicit call; drop only
        // guards against leaked allocations and handles.
        let _ignored = self.dispose();
    }
}

/// A running confined child (dsh `AclSandboxChild`): its pid and a
/// settlement call.
pub enum Child {
    /// The piped child: drain both pipes and the exit code together.
    Piped(spawn::PipedChild),
    /// The inherited child: settle the exit code and close the Job.
    Inherited(spawn::JobChild),
}

impl std::fmt::Debug for Child {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Piped(child) => f.debug_tuple("Piped").field(&child.pid).finish(),
            Self::Inherited(child) => f.debug_tuple("Inherited").field(&child.pid).finish(),
        }
    }
}

impl Child {
    /// The child's process id.
    #[must_use]
    pub fn pid(&self) -> u32 {
        match self {
            Self::Piped(child) => child.pid,
            Self::Inherited(child) => child.pid,
        }
    }

    /// Resolves stdout/stderr and the exit code once the child exits.
    ///
    /// # Errors
    ///
    /// Returns the Win32 failure of the first refusing call.
    pub fn wait(self) -> Result<ChildResult, String> {
        match self {
            Self::Piped(child) => {
                let stdout =
                    spawn::drain_pipe(child.stdout_read).map_err(|error| error.to_string())?;
                let stderr =
                    spawn::drain_pipe(child.stderr_read).map_err(|error| error.to_string())?;
                // waitForExit is deliberately NOT started before the
                // drains return: the pipes resolve only after the child
                // closed its ends — by then the wait returns immediately
                // (the pipe-buffer deadlock judgment).
                let exit_code = spawn::wait_for_process_exit(child.process)
                    .map_err(|error| error.to_string())?;
                Ok(ChildResult {
                    stdout,
                    stderr,
                    exit_code,
                })
            }
            Self::Inherited(child) => {
                let exit_code = spawn::wait_for_process_exit(child.process)
                    .map_err(|error| error.to_string())?;
                ffi::close_handle_checked(child.job, "kill-on-close job")
                    .map_err(|error| error.to_string())?;
                Ok(ChildResult {
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    exit_code,
                })
            }
        }
    }
}
