// Rust guideline compliant 2026-10-29
//! `fr-windows-acl-run`: the windows-acl confinement runner (a full
//! port of dsh `sandbox-windows-acl/src/runner.ts`) — the argv-prefix
//! wrapper the sandbox seam spawns in place of the caller's command. It
//! creates the `WRITE_RESTRICTED` token with the workspace write-SID
//! allowlist, spawns the wrapped argv under it with the CALLER's stdio
//! inherited (bytes flow straight through), mirrors the child's exit
//! code, and revokes its temp grant on exit (workspace ACEs stay
//! standing as the reuse cache).
//!
//! Stable argv contract (the seam builds it; a native-exe replacement
//! keeps the same contract):
//! ```text
//! [fr-windows-acl-run, --workspace, <dir>, --temp, <dir>,
//!  --mode, <read-only|workspace-write>,
//!  [--write-sid, <S-1-4-…>, --temp-write-sid, <S-1-4-…>],
//!  --, <argv...>]
//! ```
//!
//! `--write-sid` + `--temp-write-sid`: the seam's grant contract — the
//! CALLER has already materialized distinct workspace and private-temp
//! ACEs and owns their revocation, so the runner neither grants nor
//! revokes. Without the pair (standalone/agentless use),
//! workspace-write treats `--temp` as a ROOT, creates a random private
//! child directory, derives its own temp SID, and removes that
//! directory after the child exits. In both flows the runner rewrites
//! TMP/TEMP in its OWN environment to the private directory before
//! spawning; the child inherits that block. Read-only leaves the
//! ambient entries untouched (writes there are denied anyway).
//!
//! Failure contract: every runner-side failure (bad args, missing
//! directories, token/grant/spawn errors) prints `windows-acl-run:
//! <detail>` to stderr and exits 127 — the seam's
//! `RUNNER_FAILURE_RULES` matches that signature. The child is NEVER
//! spawned unrestricted.
//!
//! Compiled only for Windows targets; other targets build an empty
//! shell so the workspace and cross-compile gates stay green.

#[cfg(windows)]
mod runner {
    use fr_sandbox::ConfinedMode;
    use fr_sandbox::SandboxMode;
    use fr_sandbox::win::AclSandbox;
    use fr_sandbox::win::AclSandboxOptions;
    use fr_sandbox::workspace_sid::temp_write_sid;
    use fr_sandbox::workspace_sid::workspace_write_sid;
    use windows_sys::Win32::Foundation::HANDLE;

    const RUNNER_SIGNATURE: &str = "windows-acl-run";
    const RUNNER_FAILURE_EXIT: i32 = 127;
    const CONTROL_ENV: &str = "FR_SUBPROCESS_CONTROL";

    /// Prints the runner-failure signature line and exits.
    fn fail(detail: &str) -> ! {
        eprintln!("{RUNNER_SIGNATURE}: {detail}");
        std::process::exit(RUNNER_FAILURE_EXIT);
    }

    struct ParsedArgs {
        workspace: String,
        temp: String,
        mode: ConfinedMode,
        write_sid: Option<String>,
        temp_write_sid: Option<String>,
        command: String,
        args: Vec<String>,
    }

    fn parse_args(raw: &[String]) -> ParsedArgs {
        let mut workspace: Option<String> = None;
        let mut temp: Option<String> = None;
        let mut mode: Option<String> = None;
        let mut write_sid: Option<String> = None;
        let mut parsed_temp_write_sid: Option<String> = None;
        let mut index = 0;
        let argv: &[String];
        loop {
            if index >= raw.len() {
                argv = &[];
                break;
            }
            let token = raw[index].as_str();
            if token == "--" {
                argv = &raw[index + 1..];
                break;
            }
            index += 1;
            let Some(value) = raw.get(index) else {
                fail(&format!("missing value after {token}"));
            };
            match token {
                "--workspace" => workspace = Some(value.clone()),
                "--temp" => temp = Some(value.clone()),
                "--mode" => mode = Some(value.clone()),
                "--write-sid" => write_sid = Some(value.clone()),
                "--temp-write-sid" => parsed_temp_write_sid = Some(value.clone()),
                _ => fail(&format!("unknown argument: {token}")),
            }
            index += 1;
        }
        let Some(workspace) = workspace else {
            fail("missing --workspace");
        };
        let Some(temp) = temp else {
            fail("missing --temp");
        };
        let mode_word = mode.as_deref().unwrap_or("");
        let mode = match SandboxMode::parse(mode_word) {
            Some(SandboxMode::ReadOnly) => ConfinedMode::ReadOnly,
            Some(SandboxMode::WorkspaceWrite) => ConfinedMode::WorkspaceWrite,
            _ => fail(&format!("unknown mode: {mode_word}")),
        };
        let Some(command) = argv.first() else {
            fail("missing command after --");
        };
        ParsedArgs {
            workspace,
            temp,
            mode,
            write_sid,
            temp_write_sid: parsed_temp_write_sid,
            command: command.clone(),
            args: argv[1..].to_vec(),
        }
    }

    fn require_directory(label: &str, path: &str) {
        // Both directories are validated in both modes: a provider bug
        // that passes a bogus root must fail loudly at the runner
        // boundary, never mid-child.
        match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => {}
            _ => fail(&format!("{label} is not an existing directory: {path}")),
        }
    }

    /// Sets one environment variable through the Win32 call so the
    /// child's inherited block carries it (SetEnvironmentVariableW
    /// mutates THIS process's block).
    fn set_environment_variable(name: &str, value: &str) {
        let wide_name: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let wide_value: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: both wide strings outlive the call; the call only
        // mutates the caller's environment block.
        let set = unsafe {
            windows_sys::Win32::System::Environment::SetEnvironmentVariableW(
                wide_name.as_ptr(),
                wide_value.as_ptr(),
            )
        };
        if set == 0 {
            let code = fr_sandbox::win::ffi::last_error();
            fail(&format!(
                "SetEnvironmentVariableW {name} failed (Win32 {code})"
            ));
        }
    }

    /// The exit-code mirror is full-width on Windows (the dsh empirical
    /// note): a child that exits with an NTSTATUS is read back as the
    /// full u32 and the parent observes exactly that value.
    fn run() -> i32 {
        let raw: Vec<String> = std::env::args().skip(1).collect();
        let parsed = parse_args(&raw);
        require_directory("--workspace", &parsed.workspace);
        require_directory("--temp", &parsed.temp);

        let seam_managed = parsed.write_sid.is_some() || parsed.temp_write_sid.is_some();
        if parsed.mode == ConfinedMode::ReadOnly && seam_managed {
            fail("read-only does not accept --write-sid or --temp-write-sid");
        }
        if parsed.mode == ConfinedMode::WorkspaceWrite
            && parsed.write_sid.is_some() != parsed.temp_write_sid.is_some()
        {
            fail("workspace-write requires --write-sid and --temp-write-sid together");
        }
        if parsed.mode == ConfinedMode::WorkspaceWrite {
            if let Err(error) = fr_sandbox::path_boundary::assert_temp_root_outside_workspace(
                &parsed.workspace,
                &parsed.temp,
            ) {
                fail(&error);
            }
        }

        // Ignore this process's own CTRL+C: the confined child (same
        // console) keeps handling its own; the runner must survive to
        // revoke grants and mirror the child's exit code.
        // SAFETY: a null handler with add=1 clears the handler table.
        if unsafe { windows_sys::Win32::System::Console::SetConsoleCtrlHandler(None, 1) } == 0 {
            let code = fr_sandbox::win::ffi::last_error();
            fail(&format!("SetConsoleCtrlHandler failed (Win32 {code})"));
        }

        let mut owned_temp_dir: Option<std::path::PathBuf> = None;
        let mut sandbox_options = AclSandboxOptions {
            writable_dirs: if parsed.mode == ConfinedMode::WorkspaceWrite {
                vec![parsed.workspace.clone()]
            } else {
                Vec::new()
            },
            mode: parsed.mode,
            manage_dacls: !seam_managed,
            ..AclSandboxOptions::default()
        };
        if parsed.mode == ConfinedMode::WorkspaceWrite {
            let expected_write_sid = workspace_write_sid(&parsed.workspace);
            if seam_managed {
                if parsed.write_sid.as_deref() != Some(expected_write_sid.as_str()) {
                    fail("--write-sid does not match --workspace");
                }
                sandbox_options.temp_dir = Some(parsed.temp.clone());
                let expected_temp_sid = temp_write_sid(&parsed.temp);
                if parsed.temp_write_sid.as_deref() != Some(expected_temp_sid.as_str()) {
                    fail("--temp-write-sid does not match --temp");
                }
                sandbox_options.write_sid = parsed.write_sid.clone();
                sandbox_options.temp_write_sid = parsed.temp_write_sid.clone();
            } else {
                let private = std::env::temp_dir().join(format!("fr-{}", std::process::id()));
                if let Err(error) = std::fs::create_dir(&private) {
                    fail(&format!(
                        "cannot create the private temp directory: {error}"
                    ));
                }
                let private = private.to_string_lossy().into_owned();
                owned_temp_dir = Some(std::path::PathBuf::from(&private));
                sandbox_options.temp_dir = Some(private.clone());
                sandbox_options.write_sid = Some(expected_write_sid);
                sandbox_options.temp_write_sid = Some(temp_write_sid(&private));
            }
        }

        let mut sandbox = match AclSandbox::new(&sandbox_options) {
            Ok(sandbox) => sandbox,
            Err(error) => fail(&error),
        };
        if let Err(error) = sandbox.init() {
            if let Some(dir) = &owned_temp_dir {
                let _ignored = std::fs::remove_dir_all(dir);
            }
            fail(&error);
        }

        if let Some(private) = &owned_temp_dir {
            let private = private.to_string_lossy().into_owned();
            set_environment_variable("TMP", &private);
            set_environment_variable("TEMP", &private);
        }

        let control: Option<HANDLE> = if std::env::var(CONTROL_ENV).as_deref() == Ok("pipe") {
            match fr_sandbox::win::spawn::control_handle_from_fd() {
                Ok(handle) => Some(handle),
                Err(error) => {
                    if let Err(cleanup) = sandbox.dispose() {
                        eprintln!("{RUNNER_SIGNATURE}: cleanup: {cleanup}");
                    }
                    if let Some(dir) = &owned_temp_dir {
                        let _ignored = std::fs::remove_dir_all(dir);
                    }
                    fail(&error.to_string());
                }
            }
        } else {
            None
        };

        let spawn_options = fr_sandbox::win::sandbox::SpawnOptions {
            command: &parsed.command,
            args: &parsed.args,
            cwd: None,
            stdio: fr_sandbox::win::sandbox::Stdio::Inherit,
            control,
        };
        let child = match sandbox.spawn(&spawn_options) {
            Ok(child) => child,
            Err(error) => {
                if let Err(cleanup) = sandbox.dispose() {
                    eprintln!("{RUNNER_SIGNATURE}: cleanup: {cleanup}");
                }
                if let Some(dir) = &owned_temp_dir {
                    let _ignored = std::fs::remove_dir_all(dir);
                }
                fail(&error);
            }
        };
        // After a successful spawn the child owns the control pipe's
        // read end; this process's copy of the descriptor closes.
        if control.is_some() {
            fr_sandbox::win::spawn::close_control_fd();
        }
        let result = child.wait();

        // Cleanup failures must not mask the child's exit code: report
        // and keep going.
        if let Err(cleanup) = sandbox.dispose() {
            eprintln!("{RUNNER_SIGNATURE}: cleanup: {cleanup}");
        }
        if let Some(dir) = &owned_temp_dir {
            if let Err(cleanup) = std::fs::remove_dir_all(dir) {
                eprintln!("{RUNNER_SIGNATURE}: cleanup: {cleanup}");
            }
        }
        match result {
            Ok(settled) => settled.exit_code as i32,
            Err(error) => {
                eprintln!("{RUNNER_SIGNATURE}: {error}");
                RUNNER_FAILURE_EXIT
            }
        }
    }

    pub(crate) fn main() {
        std::process::exit(run());
    }
}

#[cfg(windows)]
fn main() {
    runner::main();
}

#[cfg(not(windows))]
const fn main() {
    // The runner exists only on Windows hosts (the wasm-crate
    // cfg-gating judgment: other targets build an empty shell so the
    // workspace and cross-compile gates stay green).
}
