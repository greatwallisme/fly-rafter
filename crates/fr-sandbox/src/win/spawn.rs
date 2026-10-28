// Rust guideline compliant 2026-10-29
//! Restricted-token child spawn adapters (dsh `spawn.ts` over the shared
//! `dsh-win32-process` process helpers): piped stdio and inherited
//! kill-on-close Job lifecycles over CreateProcessAsUserW.

use std::ffi::c_void;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::ReadFile;
use windows_sys::Win32::System::Console::GetStdHandle;
use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
use windows_sys::Win32::System::JobObjects::CreateJobObjectW;
use windows_sys::Win32::System::JobObjects::SetInformationJobObject;
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Pipes::PeekNamedPipe;
use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;
use windows_sys::Win32::System::Threading::CreateProcessAsUserW;
use windows_sys::Win32::System::Threading::GetExitCodeProcess;
use windows_sys::Win32::System::Threading::PROCESS_INFORMATION;
use windows_sys::Win32::System::Threading::ResumeThread;
use windows_sys::Win32::System::Threading::STARTUPINFOW;
use windows_sys::Win32::System::Threading::TerminateProcess;
use windows_sys::Win32::System::Threading::WaitForSingleObject;

use super::ffi;
use super::ffi::Win32Error;
use super::win32_abi as abi;

/// Quotes one argument according to CommandLineToArgvW parsing (dsh
/// `quoteArg`).
#[must_use]
pub fn quote_arg(argument: &str) -> String {
    if argument.is_empty() {
        return "\"\"".to_owned();
    }
    if !argument.chars().any(|c| c.is_whitespace() || c == '"') {
        return argument.to_owned();
    }
    let mut quoted = String::from("\"");
    let characters: Vec<char> = argument.chars().collect();
    let mut index = 0;
    while index < characters.len() {
        let mut backslashes = 0;
        while index < characters.len() && characters[index] == '\\' {
            backslashes += 1;
            index += 1;
        }
        if index == characters.len() {
            quoted.push_str(&"\\".repeat(backslashes * 2));
        } else if characters[index] == '"' {
            quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
            quoted.push('"');
            index += 1;
        } else {
            quoted.push_str(&"\\".repeat(backslashes));
            quoted.push(characters[index]);
            index += 1;
        }
    }
    quoted.push('"');
    quoted
}

/// Builds the mutable command line accepted by CreateProcessAsUserW (dsh
/// `buildCommandLine`).
#[must_use]
pub fn build_command_line(program: &str, args: &[String]) -> String {
    let mut line = quote_arg(program);
    for argument in args {
        line.push(' ');
        line.push_str(&quote_arg(argument));
    }
    line
}

/// A zeroed STARTUPINFOW with the stdio-bearing fields set (dsh
/// `encodeStartupInfo` shape); the control block stays absent.
fn startup_info(stdin: HANDLE, stdout: HANDLE, stderr: HANDLE) -> STARTUPINFOW {
    STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        lpReserved: std::ptr::null_mut(),
        lpDesktop: std::ptr::null_mut(),
        lpTitle: std::ptr::null_mut(),
        dwX: 0,
        dwY: 0,
        dwXSize: 0,
        dwYSize: 0,
        dwXCountChars: 0,
        dwYCountChars: 0,
        dwFillAttribute: 0,
        dwFlags: abi::STARTF_USESTDHANDLES | abi::STARTF_USESHOWWINDOW,
        wShowWindow: abi::SW_HIDE,
        cbReserved2: 0,
        lpReserved2: std::ptr::null_mut(),
        hStdInput: stdin,
        hStdOutput: stdout,
        hStdError: stderr,
    }
}

/// CRT descriptor-table flags (dsh `control-stdio.ts` constants).
const FOPEN: u8 = 0x01;
/// CRT flag marking a pipe descriptor.
const FPIPE: u8 = 0x08;
/// CRT flag marking a character device.
const FDEV: u8 = 0x40;
/// GetFileType code for a character device.
const FILE_TYPE_CHAR: u32 = 2;
/// GetFileType code for a pipe.
const FILE_TYPE_PIPE: u32 = 3;

/// Encodes the CRT's descriptor table before the child runtime allocates
/// descriptors (dsh `inheritedControlStdio`): standard handles plus the
/// single additional descriptor the launcher selected, as
/// STARTUPINFO's reserved CRT fields. Supported Windows targets use
/// 64-bit handles; empty slots remain closed.
fn inherited_control_stdio(
    stdin: HANDLE,
    stdout: HANDLE,
    stderr: HANDLE,
    control: HANDLE,
) -> Result<Vec<u8>, Win32Error> {
    let count = (SUBPROCESS_CONTROL_FD + 1) as usize;
    let handle_offset = 4 + count;
    let mut bytes = vec![0u8; handle_offset + count * 8];
    bytes[0..4].copy_from_slice(&(count as u32).to_le_bytes());
    for index in 0..count {
        bytes[handle_offset + index * 8..handle_offset + index * 8 + 8]
            .copy_from_slice(&usize::MAX.to_le_bytes()); // INVALID_HANDLE
    }
    let entries = [
        (0usize, stdin),
        (1, stdout),
        (2, stderr),
        (SUBPROCESS_CONTROL_FD as usize, control),
    ];
    for (fd, handle) in entries {
        // SAFETY: GetFileType only inspects the handle's device kind.
        let kind = unsafe { windows_sys::Win32::Storage::FileSystem::GetFileType(handle) };
        if fd == SUBPROCESS_CONTROL_FD as usize && kind != FILE_TYPE_PIPE {
            return Err(Win32Error::new(
                "GetFileType",
                0,
                "subprocess control descriptor is not a Windows pipe",
            ));
        }
        bytes[4 + fd] = FOPEN
            | u8::from(kind == FILE_TYPE_PIPE) * FPIPE
            | u8::from(kind == FILE_TYPE_CHAR) * FDEV;
        bytes[handle_offset + fd * 8..handle_offset + fd * 8 + 8]
            .copy_from_slice(&(handle as usize).to_le_bytes());
    }
    Ok(bytes)
}

/// Resolves the OS handle of this process's control descriptor (the
/// `_get_osfhandle` CRT call standing in for dsh's
/// `uv_get_osfhandle`).
///
/// # Errors
///
/// Returns the refusal for an invalid descriptor handle (dsh
/// `descriptorHandle` wording adapted to the CRT call).
pub fn control_handle_from_fd() -> Result<HANDLE, Win32Error> {
    // SAFETY: _get_osfhandle only translates the descriptor; fd 7 is
    // either open in this process or the call reports -1.
    let raw = unsafe { msvcrt_get_osfhandle(SUBPROCESS_CONTROL_FD) };
    let handle = raw as HANDLE;
    if ffi::is_invalid_handle(handle) {
        return Err(Win32Error::new(
            "_get_osfhandle",
            0,
            format!("returned an invalid handle for target control fd {SUBPROCESS_CONTROL_FD}"),
        ));
    }
    Ok(handle)
}

// The CRT descriptor-to-handle translation (ucrtbase), declared
// locally because the Rust std surface does not expose it.
// SAFETY: the symbol exists in every supported MSVC CRT.
unsafe extern "C" {
    fn _get_osfhandle(fd: i32) -> isize;
}

/// Forwards to the CRT declaration (a named wrapper for the import).
unsafe fn msvcrt_get_osfhandle(fd: i32) -> isize {
    // SAFETY: see the declaration above.
    unsafe { _get_osfhandle(fd) }
}

/// Closes this process's copy of the control descriptor after the
/// spawn (the child owns the pipe from here; dsh `closeSync(fd)`).
pub fn close_control_fd() {
    // SAFETY: the descriptor is either open in this process or the
    // call reports -1; both outcomes close nothing further.
    unsafe { _close(SUBPROCESS_CONTROL_FD) };
}

// The CRT descriptor close (ucrtbase), declared locally for the same
// reason as `_get_osfhandle`.
// SAFETY: the symbol exists in every supported MSVC CRT.
unsafe extern "C" {
    fn _close(fd: i32) -> i32;
}

/// Creates one kill-on-close Job (dsh `createKillOnCloseJob`).
fn create_kill_on_close_job() -> Result<HANDLE, Win32Error> {
    // SAFETY: an unnamed job object returns an owned handle.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if ffi::is_invalid_handle(job) {
        return Err(ffi::win32_error("CreateJobObjectW", "unnamed job"));
    }
    let mut information = vec![0u8; abi::JOBOBJECT_EXTENDED_LIMIT_SIZE];
    let flags_offset = abi::JOBOBJECT_EXTENDED_LIMIT_FLAGS_OFFSET;
    information[flags_offset..flags_offset + 4]
        .copy_from_slice(&abi::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE.to_le_bytes());
    // SAFETY: the information buffer owns the extended-limit record.
    if unsafe {
        SetInformationJobObject(
            job,
            abi::JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
            information.as_ptr().cast(),
            information.len() as u32,
        )
    } == 0
    {
        let error = ffi::win32_error("SetInformationJobObject", "kill-on-close limit");
        let _ignored = ffi::close_handle_checked(job, "job after limit failure");
        return Err(error);
    }
    Ok(job)
}

/// One restricted-token spawn request.
pub struct RestrictedSpawn<'a> {
    /// The restricted primary token the child runs under.
    pub token: HANDLE,
    /// Executable argv entry passed through CreateProcess.
    pub command: &'a str,
    /// Arguments excluding the executable.
    pub args: &'a [String],
    /// Existing child working directory.
    pub cwd: &'a str,
    /// The inherited control-pipe handle riding the CRT descriptor
    /// table at fd 7, when the caller's control plane is active.
    pub control: Option<HANDLE>,
}

impl std::fmt::Debug for RestrictedSpawn<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestrictedSpawn")
            .field("command", &self.command)
            .field("cwd", &self.cwd)
            .field("control", &self.control.is_some())
            .finish_non_exhaustive()
    }
}

/// Spawns a restricted-token child with anonymous-pipe stdout/stderr
/// and immediate stdin EOF (dsh `spawnPipedProcess`); the caller owns
/// the process and pipe-read handles.
///
/// # Errors
///
/// Returns the Win32 failure of the first refusing call; every owned
/// resource is closed on failure.
pub fn spawn_piped_process(options: &RestrictedSpawn<'_>) -> Result<PipedChild, Win32Error> {
    let mut owned: Vec<HANDLE> = Vec::new();
    let pipe = || -> Result<(HANDLE, HANDLE), Win32Error> {
        let (mut read, mut write) = (std::ptr::null_mut(), std::ptr::null_mut());
        // SAFETY: both out-slots are local; the handles join the owned
        // set on success.
        if unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 0) } == 0 {
            return Err(ffi::win32_error("CreatePipe", "piped stdio"));
        }
        Ok((read, write))
    };
    let result = (|| -> Result<PipedChild, Win32Error> {
        let (stdin_read, stdin_write) = pipe()?;
        let (stdout_read, stdout_write) = pipe()?;
        let (stderr_read, stderr_write) = pipe()?;
        owned.extend([
            stdin_read,
            stdin_write,
            stdout_read,
            stdout_write,
            stderr_read,
            stderr_write,
        ]);
        for (handle, label) in [
            (stdin_read, "stdin read end"),
            (stdout_write, "stdout write end"),
            (stderr_write, "stderr write end"),
        ] {
            ffi::set_handle_inherit(handle, label)?;
        }
        let command_line = build_command_line(options.command, options.args);
        let mut wide_line = ffi::wide(&command_line);
        let wide_cwd = ffi::wide(options.cwd);
        let startup = startup_info(stdin_read, stdout_write, stderr_write);
        let mut info = PROCESS_INFORMATION {
            hProcess: std::ptr::null_mut(),
            hThread: std::ptr::null_mut(),
            dwProcessId: 0,
            dwThreadId: 0,
        };
        // The sandbox mutates its process environment before this call;
        // lpEnvironment stays null (the dsh ERROR_INVALID_PARAMETER
        // finding for explicit blocks through raw imports).
        // SAFETY: the STARTUPINFO/PROCESS_INFORMATION records are local
        // and outlive the call; the wide strings outlive it too.
        let created = unsafe {
            CreateProcessAsUserW(
                options.token,
                std::ptr::null(),
                wide_line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                0,
                std::ptr::null(),
                wide_cwd.as_ptr(),
                &startup,
                &mut info,
            )
        };
        if created == 0 {
            return Err(ffi::win32_error(
                "CreateProcessAsUserW",
                format!("command: {}, cwd: {}", options.command, options.cwd),
            ));
        }
        if ffi::is_invalid_handle(info.hProcess) || ffi::is_invalid_handle(info.hThread) {
            // SAFETY: the created child must not outlive a failed
            // handshake; terminate then close both handles.
            unsafe {
                TerminateProcess(info.hProcess, 1);
            }
            let _ignored = ffi::close_handle_checked(info.hThread, "thread after null check");
            let _ignored = ffi::close_handle_checked(info.hProcess, "process after null check");
            return Err(ffi::win32_error(
                "CreateProcessAsUserW",
                format!(
                    "succeeded but returned null process/thread handles (pid {})",
                    info.dwProcessId
                ),
            ));
        }
        // The thread handle and the child's pipe write ends die here.
        let _ignored = ffi::close_handle_checked(info.hThread, "primary thread");
        for handle in [stdin_read, stdin_write, stdout_write, stderr_write] {
            let _ignored = ffi::close_handle_checked(handle, "pipe write end");
            owned.retain(|owned| *owned != handle);
        }
        owned.retain(|owned| *owned != stdout_read && *owned != stderr_read);
        Ok(PipedChild {
            pid: info.dwProcessId,
            process: info.hProcess,
            stdout_read,
            stderr_read,
        })
    })();
    if result.is_err() {
        for handle in owned {
            // Best-effort cleanup: each successfully created pipe end is
            // uniquely owned.
            // SAFETY: the handle came from CreatePipe and is closed
            // exactly once.
            unsafe { CloseHandle(handle) };
        }
    }
    result
}

/// A piped restricted child whose process and read handles remain
/// caller-owned (dsh `SpawnedPipedProcess`).
pub struct PipedChild {
    /// Direct child process id.
    pub pid: u32,
    /// Process handle closed by [`wait_for_process_exit`].
    pub process: HANDLE,
    /// Stdout pipe read end closed by [`drain_pipe`].
    pub stdout_read: HANDLE,
    /// Stderr pipe read end closed by [`drain_pipe`].
    pub stderr_read: HANDLE,
}

impl std::fmt::Debug for PipedChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipedChild")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

/// Drains one anonymous pipe until the writer closes it (dsh
/// `drainPipe`, the synchronous poll shape).
///
/// # Errors
///
/// Returns the Win32 failure of the first refusing call; the handle is
/// always closed.
pub fn drain_pipe(handle: HANDLE) -> Result<Vec<u8>, Win32Error> {
    let mut chunks: Vec<u8> = Vec::new();
    let result = (|| -> Result<(), Win32Error> {
        let mut available: u32 = 0;
        let mut read: u32 = 0;
        loop {
            // SAFETY: the peek writes only into the local slot.
            let peeked = unsafe {
                PeekNamedPipe(
                    handle,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut available,
                    std::ptr::null_mut(),
                )
            };
            if peeked == 0 {
                let code = ffi::last_error();
                if code == abi::ERROR_BROKEN_PIPE || code == abi::ERROR_NO_DATA {
                    break;
                }
                return Err(ffi::win32_error(
                    "PeekNamedPipe",
                    format!("drain failure after {} chunk(s)", chunks.len()),
                ));
            }
            if available > 0 {
                let mut chunk = vec![0u8; available as usize];
                // SAFETY: the read writes at most the chunk length.
                if unsafe {
                    ReadFile(
                        handle,
                        chunk.as_mut_ptr().cast(),
                        chunk.len() as u32,
                        &raw mut read,
                        std::ptr::null_mut(),
                    )
                } == 0
                {
                    return Err(ffi::win32_error(
                        "ReadFile",
                        format!("drain failure after {} chunk(s)", chunks.len()),
                    ));
                }
                chunk.truncate(read as usize);
                chunks.extend(chunk);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        Ok(())
    })();
    let closed = ffi::close_handle_checked(handle, "drained pipe read end");
    result?;
    closed?;
    Ok(chunks)
}

/// Waits for a process and always closes its handle (dsh
/// `waitForProcessExit`).
///
/// # Errors
///
/// Returns the Win32 failure of the first refusing call; the handle is
/// always closed.
pub fn wait_for_process_exit(process: HANDLE) -> Result<u32, Win32Error> {
    let result = (|| -> Result<u32, Win32Error> {
        // SAFETY: an INFINITE wait on a caller-owned handle.
        if unsafe { WaitForSingleObject(process, u32::MAX) } == u32::MAX {
            return Err(ffi::win32_error("WaitForSingleObject", "child exit"));
        }
        let mut exit_code: u32 = 0;
        // SAFETY: the slot is local.
        if unsafe { GetExitCodeProcess(process, &mut exit_code) } == 0 {
            return Err(ffi::win32_error("GetExitCodeProcess", "child exit"));
        }
        Ok(exit_code)
    })();
    let closed = ffi::close_handle_checked(process, "child process");
    let code = result?;
    closed?;
    Ok(code)
}

/// A suspended child assigned to one kill-on-close Job before resume (dsh
/// `SpawnedJobProcess`).
pub struct JobChild {
    /// Direct child process id.
    pub pid: u32,
    /// Process handle closed by [`wait_for_process_exit`].
    pub process: HANDLE,
    /// Job handle closed by the lifecycle owner.
    pub job: HANDLE,
}

impl std::fmt::Debug for JobChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobChild")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

/// Spawns a restricted-token process suspended, assigns its
/// kill-on-close Job, then resumes it with the CALLER's stdio inherited
/// (dsh `spawnInheritedJobProcess`); bytes flow straight through and
/// the child dies with the caller.
///
/// # Errors
///
/// Returns the Win32 failure of the first refusing call; every owned
/// resource is closed on failure.
pub fn spawn_inherited_job_process(options: &RestrictedSpawn<'_>) -> Result<JobChild, Win32Error> {
    let job = create_kill_on_close_job()?;
    let mut enabled: Vec<HANDLE> = Vec::new();
    let result = (|| -> Result<JobChild, Win32Error> {
        let standard = |selector: u32, label: &str| -> Result<HANDLE, Win32Error> {
            // SAFETY: GetStdHandle reads the caller's slot.
            let handle = unsafe { GetStdHandle(selector) };
            if ffi::is_invalid_handle(handle) {
                return Err(ffi::win32_error(
                    "GetStdHandle",
                    format!("null {label} handle"),
                ));
            }
            Ok(handle)
        };
        let stdin = standard(abi::STD_INPUT_HANDLE, "stdin")?;
        let stdout = standard(abi::STD_OUTPUT_HANDLE, "stdout")?;
        let stderr = standard(abi::STD_ERROR_HANDLE, "stderr")?;
        let inherited = [(stdin, "stdin"), (stdout, "stdout"), (stderr, "stderr")];
        if let Some(control) = options.control {
            ffi::set_handle_inherit(control, "control (enable inherit)")?;
            enabled.push(control);
        }
        for (handle, label) in &inherited {
            ffi::set_handle_inherit(*handle, &format!("{label} (enable inherit)"))?;
            enabled.push(*handle);
        }
        let mut reserved2: Option<Vec<u8>> = None;
        if let Some(control) = options.control {
            reserved2 = Some(inherited_control_stdio(stdin, stdout, stderr, control)?);
        }
        let command_line = build_command_line(options.command, options.args);
        let mut wide_line = ffi::wide(&command_line);
        let wide_cwd = ffi::wide(options.cwd);
        let mut startup = startup_info(stdin, stdout, stderr);
        if let Some(bytes) = &reserved2 {
            startup.cbReserved2 = bytes.len() as u16;
            startup.lpReserved2 = bytes.as_ptr() as *mut u8;
        }
        let mut info = PROCESS_INFORMATION {
            hProcess: std::ptr::null_mut(),
            hThread: std::ptr::null_mut(),
            dwProcessId: 0,
            dwThreadId: 0,
        };
        // Preserve console inheritance: CREATE_NO_WINDOW can fail
        // restricted-token DLL initialization (the dsh note).
        // SAFETY: the records are local and outlive the call.
        let created = unsafe {
            CreateProcessAsUserW(
                options.token,
                std::ptr::null(),
                wide_line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                CREATE_SUSPENDED,
                std::ptr::null(),
                wide_cwd.as_ptr(),
                &startup,
                &mut info,
            )
        };
        if created == 0 {
            return Err(ffi::win32_error(
                "CreateProcessAsUserW",
                format!("command: {}, cwd: {}", options.command, options.cwd),
            ));
        }
        if ffi::is_invalid_handle(info.hProcess) || ffi::is_invalid_handle(info.hThread) {
            // SAFETY: never leave a half-created child running.
            unsafe {
                TerminateProcess(info.hProcess, 1);
            }
            let _ignored = ffi::close_handle_checked(info.hThread, "thread after null check");
            let _ignored = ffi::close_handle_checked(info.hProcess, "process after null check");
            ffi::close_handle_checked(job, "job after null handles")?;
            return Err(ffi::win32_error(
                "CreateProcessAsUserW",
                format!(
                    "succeeded but returned null process/thread handles (pid {})",
                    info.dwProcessId
                ),
            ));
        }
        // SAFETY: the child process handle is owned for the assignment.
        if unsafe { AssignProcessToJobObject(job, info.hProcess) } == 0 {
            let error = ffi::win32_error(
                "AssignProcessToJobObject",
                format!("pid {}", info.dwProcessId),
            );
            // SAFETY: the unassigned child must not outlive the failure.
            unsafe {
                TerminateProcess(info.hProcess, 1);
            }
            let _ignored =
                ffi::close_handle_checked(info.hThread, "thread after assignment failure");
            let _ignored =
                ffi::close_handle_checked(info.hProcess, "process after assignment failure");
            ffi::close_handle_checked(job, "job after assignment failure")?;
            return Err(error);
        }
        // SAFETY: the suspended thread starts here.
        if unsafe { ResumeThread(info.hThread) } == u32::MAX {
            let error = ffi::win32_error("ResumeThread", format!("pid {}", info.dwProcessId));
            let _ignored = ffi::close_handle_checked(info.hThread, "thread after resume failure");
            let _ignored = ffi::close_handle_checked(info.hProcess, "process after resume failure");
            ffi::close_handle_checked(job, "job after resume failure")?;
            return Err(error);
        }
        let _ignored = ffi::close_handle_checked(info.hThread, "primary thread");
        Ok(JobChild {
            pid: info.dwProcessId,
            process: info.hProcess,
            job,
        })
    })();
    // The runner spawns nothing else; cleanup failure must not mask the
    // child's outcome — restore the inherit bits best-effort.
    for handle in enabled {
        ffi::clear_handle_inherit(handle);
    }
    match result {
        Ok(child) => Ok(child),
        Err(error) => {
            let _ignored = ffi::close_handle_checked(job, "job after spawn failure");
            Err(error)
        }
    }
}

/// The control-descriptor payload reserved for the Phase 16 subprocess
/// control plane (dsh `inheritedControlStdio`): the STARTUPINFOW
/// reserved2 block shape is pinned here so the runner's fd-7 arm has a
/// single owner.
pub const SUBPROCESS_CONTROL_FD: i32 = 7;

/// The environment variable gating the control-pipe arm (dsh
/// `SUBPROCESS_CONTROL_ENV = 'DSH_SUBPROCESS_CONTROL'`, brand-mapped).
pub const SUBPROCESS_CONTROL_ENV: &str = "FR_SUBPROCESS_CONTROL";

/// Reads the caller's stdio control state for the spawn decision.
#[must_use]
pub fn control_pipe_requested() -> bool {
    std::env::var(SUBPROCESS_CONTROL_ENV).as_deref() == Ok("pipe")
}

/// Opens the inherited control pipe at the reserved descriptor (the
/// carrier side; Phase 16 owns the protocol). Present for argv-contract
/// parity with the dsh runner.
pub struct ControlPipe {
    /// The read end moved into the child at descriptor 7.
    pub read: HANDLE,
    /// The write end the caller keeps.
    pub write: HANDLE,
}

impl std::fmt::Debug for ControlPipe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ControlPipe")
    }
}

impl ControlPipe {
    /// Creates the pipe pair and dup2-style binds the read end to the
    /// reserved descriptor in THIS process (the child inherits it).
    ///
    /// # Errors
    ///
    /// Returns the Win32 failure of the first refusing call.
    pub fn open() -> Result<Self, Win32Error> {
        let (mut read, mut write) = (std::ptr::null_mut(), std::ptr::null_mut());
        // SAFETY: both out-slots are local; both handles are owned.
        if unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 0) } == 0 {
            return Err(ffi::win32_error("CreatePipe", "control pipe"));
        }
        ffi::set_handle_inherit(read, "control (enable inherit)")?;
        Ok(Self { read, write })
    }

    /// Closes this process's copy of the child-side read end after the
    /// spawn (the child owns it from here).
    pub fn close_read(&self) {
        // SAFETY: the handle is owned exactly once.
        unsafe { CloseHandle(self.read as *mut c_void) };
    }
}
