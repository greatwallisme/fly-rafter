// Rust guideline compliant 2026-11-01
//! The persistent shell PTY backend over the subprocess terminal
//! primitive, the shared confinement policy, and provider-owned session
//! cleanup (dsh `terminal-bash`, one module in [`crate`] per PRD 4.5).
//!
//! The dsh plugin injects `terminals`/`sandboxPolicy`/`subprocess`/
//! `workingDirectory` services; the fr backend lives in the same crate
//! as the registry and the terminal primitive, so those injections fold
//! into direct calls and the [`crate::executors::SandboxConfine`] /
//! [`crate::executors::PolicyResolve`] faces. The dsh abort-signal
//! races fold into the synchronous startup bound (the send machinery's
//! own `timeout_ms` deadline); a spawn cannot be interrupted mid-flight
//! once the backend runs — the registry's pending-spawn fence plus the
//! publish-time re-checks stay the sole abort surface (the batch-2
//! sync-port deviation, now with the backend-concrete texts).
//!
//! Deviations (the sync-port record):
//! - `DSH_SHELL`/`DSH_SESSION_ID/DSH_PTY_SESSION_ID` ride the fr brand
//!   table to `FR_SHELL`/`FR_SESSION_ID`/`FR_PTY_SESSION_ID` (the FR_
//!   env hard rule; the values stay verbatim).
//! - The fence observes the standing policy at spawn time instead of
//!   intercepting a runtime mode-change dispatch: fr M1 has no runtime
//!   mode-switch dispatcher, so the per-owner last-observed-mode guard
//!   fires on the next spawn observation while activity exists.
//! - An absent `cwd` falls back to the process working directory (the
//!   dsh workingDirectory.ensure fallback would need the
//!   fr-tools-builtin seam across the dependency direction); the six
//!   tools and the persistent pair always resolve cwd in-process, so
//!   this stays the composition-fallback arm.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

use fr_sandbox::seam::{SandboxExecutionPolicy, SandboxMode, SandboxPolicy};

use crate::executors::{ENCODING_PREAMBLE, PolicyResolve, SandboxConfine};
use crate::inspect::ProcFs;
use crate::terminal_registry::{
    BackendSpawnError, BackendSpawnSpec, TerminalBackend, TerminalRegistry,
};
use crate::terminal_sanitize::CONTROLLED_PROMPT;
use crate::terminal_session::{
    LocalPtySession, SendResult, SessionConfig, ShellDialect, WaitReason,
};

/// The baked composition judgment for the terminal group (the
/// `BAKED_STR_REPLACE_EDITOR` precedent): the dsh base bundle assembles
/// no terminal rows, so the backend mounts only under the opt-in.
pub const BAKED_TERMINAL_BACKEND: bool = false;

/// The bash dialect default executable.
pub const DEFAULT_BASH_SHELL: &str = "/bin/bash";
/// The bash dialect default arguments (interactive, profile-free).
pub const DEFAULT_BASH_ARGS: [&str; 3] = ["--noprofile", "--norc", "-i"];
/// The pwsh dialect default arguments (interactive host, profile-free).
pub const DEFAULT_PWSH_ARGS: [&str; 2] = ["-NoLogo", "-NoProfile"];

/// The validated backend configuration (the dsh `ResolvedConfig` plus
/// the registry type; the resolved numerics land in
/// [`SessionConfig`]).
#[derive(Debug, Clone)]
pub struct ResolvedBackendConfig {
    /// The backend registry type (default `shell`).
    pub backend_type: String,
    /// The resolved session configuration.
    pub session: SessionConfig,
}

/// The raw backend configuration rows (the dsh `Config` optional
/// fields; `None` selects the dsh default).
#[derive(Debug, Clone, Default)]
pub struct RawBackendConfig {
    /// The backend registry type.
    pub backend_type: Option<String>,
    /// The interactive shell dialect.
    pub shell_dialect: Option<ShellDialect>,
    /// The interactive shell executable.
    pub shell_path: Option<String>,
    /// The shell arguments.
    pub shell_args: Option<Vec<String>>,
    /// Terminal rows.
    pub rows: Option<u16>,
    /// Terminal columns.
    pub cols: Option<u16>,
    /// Maximum retained logical lines.
    pub scrollback_lines: Option<usize>,
    /// Maximum retained UTF-8 bytes.
    pub scrollback_max_bytes: Option<usize>,
    /// Maximum bytes returned by one read or settled viewport.
    pub max_read_bytes: Option<usize>,
    /// Readiness polling interval in milliseconds.
    pub poll_interval_ms: Option<u64>,
    /// Delay before the exact syscall probes in milliseconds.
    pub exact_probe_after_ms: Option<u64>,
    /// Silence duration that yields inferred-idle in milliseconds.
    pub idle_silence_ms: Option<u64>,
    /// Extra wait beyond the silence bound once a prompt marker was
    /// seen.
    pub handoff_grace_ms: Option<u64>,
    /// Extra wait for a pending prompt tail in milliseconds.
    pub prompt_tail_grace_ms: Option<u64>,
    /// Absolute bound for one send and the complete pwsh startup
    /// sequence in milliseconds.
    pub timeout_ms: Option<u64>,
    /// Grace before teardown escalates in milliseconds.
    pub dispose_grace_ms: Option<u64>,
}

/// Resolves the effective per-dialect shell specification (the dsh
/// `resolveConfig`): an unset or empty `shell_path`/`shell_args`
/// selects the dialect defaults, a non-empty explicit value wins.
///
/// The pwsh path resolution failure fails the resolve (the dsh
/// apply-time throw).
///
/// # Errors
///
/// The pwsh default-path resolution failure.
pub fn resolve_config(raw: &RawBackendConfig) -> Result<ResolvedBackendConfig, String> {
    let dialect = raw.shell_dialect.unwrap_or(ShellDialect::Bash);
    let path_default = |fallback: &str| {
        raw.shell_path
            .as_ref()
            .filter(|path| !path.is_empty())
            .cloned()
            .unwrap_or_else(|| fallback.to_owned())
    };
    let args_default = |fallback: &[&str]| -> Vec<String> {
        raw.shell_args
            .as_ref()
            .filter(|args| !args.is_empty())
            .cloned()
            .unwrap_or_else(|| fallback.iter().map(|word| (*word).to_owned()).collect())
    };
    let (shell_path, shell_args) = match dialect {
        ShellDialect::Bash => (
            path_default(DEFAULT_BASH_SHELL),
            args_default(&DEFAULT_BASH_ARGS),
        ),
        ShellDialect::Pwsh => {
            // The dialect default resolution runs only when the
            // explicit path is absent (the dsh lazy ternary).
            let explicit = raw
                .shell_path
                .as_ref()
                .filter(|path| !path.is_empty())
                .cloned();
            let path = match explicit {
                Some(path) => path,
                None => resolve_pwsh_path()?,
            };
            (path, args_default(&DEFAULT_PWSH_ARGS))
        }
    };
    Ok(ResolvedBackendConfig {
        backend_type: raw
            .backend_type
            .clone()
            .filter(|word| !word.is_empty())
            .unwrap_or_else(|| "shell".to_owned()),
        session: SessionConfig {
            shell_dialect: dialect,
            shell_path,
            shell_args,
            rows: raw.rows.unwrap_or(40),
            cols: raw.cols.unwrap_or(160),
            scrollback_lines: raw.scrollback_lines.unwrap_or(10_000),
            scrollback_max_bytes: raw.scrollback_max_bytes.unwrap_or(4 * 1024 * 1024),
            max_read_bytes: raw.max_read_bytes.unwrap_or(256 * 1024),
            poll_interval_ms: raw.poll_interval_ms.unwrap_or(50),
            exact_probe_after_ms: raw.exact_probe_after_ms.unwrap_or(150),
            idle_silence_ms: raw.idle_silence_ms.unwrap_or(3_000),
            handoff_grace_ms: raw.handoff_grace_ms.unwrap_or(500),
            prompt_tail_grace_ms: raw.prompt_tail_grace_ms.unwrap_or(0),
            timeout_ms: raw.timeout_ms.unwrap_or(30_000),
            dispose_grace_ms: raw.dispose_grace_ms.unwrap_or(3_000),
        },
    })
}

/// Resolves the pwsh executable from `PATH` (the dsh `resolvePwshPath`).
///
/// # Errors
///
/// The resolution failure text.
pub fn resolve_pwsh_path() -> Result<String, String> {
    crate::subprocess::resolve_executable("pwsh")
        .map_err(|error| format!("terminal-bash: pwsh resolution failed: {error}"))
}

/// Asserts every effective numeric field is positive — except
/// `prompt_tail_grace_ms`, whose zero is the no-extension value — and
/// that the bounds compose.
///
/// The texts quote the dsh camelCase key names verbatim (the dsh
/// `validateConfig`).
///
/// # Errors
///
/// The dsh validation failure texts.
pub fn validate_config(config: &ResolvedBackendConfig) -> Result<(), String> {
    if config.backend_type.is_empty() {
        return Err("terminal-bash: backendType must be non-empty".to_owned());
    }
    if config.session.shell_path.is_empty() {
        return Err("terminal-bash: shellPath must be non-empty".to_owned());
    }
    let fields: [(&str, u64); 11] = [
        ("rows", u64::from(config.session.rows)),
        ("cols", u64::from(config.session.cols)),
        ("scrollbackLines", config.session.scrollback_lines as u64),
        (
            "scrollbackMaxBytes",
            config.session.scrollback_max_bytes as u64,
        ),
        ("maxReadBytes", config.session.max_read_bytes as u64),
        ("pollIntervalMs", config.session.poll_interval_ms),
        ("exactProbeAfterMs", config.session.exact_probe_after_ms),
        ("idleSilenceMs", config.session.idle_silence_ms),
        ("handoffGraceMs", config.session.handoff_grace_ms),
        ("timeoutMs", config.session.timeout_ms),
        ("disposeGraceMs", config.session.dispose_grace_ms),
    ];
    for (name, value) in fields {
        if value == 0 {
            return Err(format!(
                "terminal-bash: {name} must be a positive safe integer"
            ));
        }
    }
    if config.session.max_read_bytes > config.session.scrollback_max_bytes {
        return Err("terminal-bash: maxReadBytes must not exceed scrollbackMaxBytes".to_owned());
    }
    if config.session.handoff_grace_ms < config.session.poll_interval_ms {
        return Err(
            "terminal-bash: handoffGraceMs must be at least pollIntervalMs so one readiness poll runs inside the grace window"
                .to_owned(),
        );
    }
    if config.session.prompt_tail_grace_ms != 0
        && config.session.prompt_tail_grace_ms < config.session.poll_interval_ms
    {
        return Err(
            "terminal-bash: promptTailGraceMs must be zero or at least pollIntervalMs so a nonzero tolerance contains one readiness poll"
                .to_owned(),
        );
    }
    Ok(())
}

/// Builds the child environment overrides (the dsh
/// `childEnvironment`): the terminal scrubbed base plus the deliberate
/// terminal-specific entries.
///
/// The pwsh renderer stays quiet and the bash marker protocol
/// installs.
#[must_use]
pub fn child_environment(spec: &BackendSpawnSpec, dialect: ShellDialect) -> Vec<(String, String)> {
    let mut common = vec![
        ("TERM".to_owned(), "dumb".to_owned()),
        ("PAGER".to_owned(), "cat".to_owned()),
        ("GIT_PAGER".to_owned(), "cat".to_owned()),
        ("FR_SHELL".to_owned(), "1".to_owned()),
        ("FR_SESSION_ID".to_owned(), spec.owner.clone()),
        ("FR_PTY_SESSION_ID".to_owned(), spec.session_id.clone()),
    ];
    if dialect == ShellDialect::Pwsh {
        // pwsh ignores PS1/PROMPT_COMMAND; its prompt is installed by
        // the startup bootstrap instead, and NO_COLOR keeps the
        // renderer quiet.
        common.push(("NO_COLOR".to_owned(), "1".to_owned()));
        return common;
    }
    common.push(("PS1".to_owned(), CONTROLLED_PROMPT.to_owned()));
    // Re-asserting PS1 after the marker keeps prompt readiness working
    // when a command overwrote the shell variable: bash runs
    // PROMPT_COMMAND before rendering each prompt, so an override never
    // survives to the next prompt.
    common.push((
        "PROMPT_COMMAND".to_owned(),
        format!("printf \"\\033]133;D;%s\\007\" \"$?\"; PS1='{CONTROLLED_PROMPT}'"),
    ));
    common.push((
        "BASH_SILENCE_DEPRECATION_WARNING".to_owned(),
        "1".to_owned(),
    ));
    common
}

/// The pwsh prompt function that emits the shared OSC `133;D;` + BEL
/// marker before every prompt, mirroring bash's `PROMPT_COMMAND`.
///
/// `[char]27`/`[char]7` build the control bytes at runtime because raw
/// ESC characters in submitted input are unreliable under `PSReadLine`.
#[must_use]
pub fn pwsh_prompt_setup() -> String {
    format!(
        "function prompt {{ [Console]::Write([char]27 + ']133;D;' + [int]$LASTEXITCODE + [char]7); '{CONTROLLED_PROMPT}' }}"
    )
}

/// The sandbox-mode fence refusal (the dsh `ensureSandboxModeFence`
/// throw text, verbatim).
#[must_use]
pub fn fence_refusal(current: &str, next: &str) -> String {
    format!(
        "cannot change sandbox mode from \"{current}\" to \"{next}\" while persistent terminal sessions are open or being created; wait for creation to settle and close them first"
    )
}

/// One spawn-time fence observation (the sync-port record): the dsh
/// fence rejects the mode-change dispatch itself; fr observes the
/// standing policy at each spawn and refuses while activity exists.
fn observe_fence(
    fences: &Mutex<HashMap<String, String>>,
    registry: &TerminalRegistry,
    owner: &str,
    mode_word: &str,
) -> Result<(), BackendSpawnError> {
    let mut fences = fences
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(current) = fences.get(owner)
        && current != mode_word
        && registry.has_owner_activity(owner)
    {
        return Err(BackendSpawnError {
            message: fence_refusal(current, mode_word),
            cleanup: None,
        });
    }
    fences.insert(owner.to_owned(), mode_word.to_owned());
    drop(fences);
    Ok(())
}

/// The local shell backend registered under the configured type (the
/// dsh `BashTerminalBackend`).
pub struct BashTerminalBackend {
    type_word: String,
    config: ResolvedBackendConfig,
    confine: Arc<dyn SandboxConfine>,
    policy: Arc<dyn PolicyResolve>,
    registry: Arc<TerminalRegistry>,
    fences: Mutex<HashMap<String, String>>,
}

impl std::fmt::Debug for BashTerminalBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BashTerminalBackend")
            .field("type", &self.type_word)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl BashTerminalBackend {
    /// Assembles the backend over the resolved configuration and the
    /// confinement seams.
    #[must_use]
    pub fn new(
        config: ResolvedBackendConfig,
        confine: Arc<dyn SandboxConfine>,
        policy: Arc<dyn PolicyResolve>,
        registry: Arc<TerminalRegistry>,
    ) -> Self {
        Self {
            type_word: config.backend_type.clone(),
            config,
            confine,
            policy,
            registry,
            fences: Mutex::new(HashMap::new()),
        }
    }

    /// The backend spawn body (the dsh `spawn` without the abort-signal
    /// races; the fences fold into the registry's pending-spawn checks).
    ///
    /// # Errors
    ///
    /// The fence refusal, the confinement refusal, the terminal spawn
    /// failure, or the startup failure with its cleanup secondary.
    pub fn spawn_session(
        &self,
        spec: &BackendSpawnSpec,
    ) -> Result<Arc<LocalPtySession>, BackendSpawnError> {
        let dialect = self.config.session.shell_dialect;
        let standing = self
            .policy
            .resolve_policy()
            .map_err(|error| BackendSpawnError {
                message: format!("terminal-bash: policy resolution failed: {error}"),
                cleanup: None,
            })?;
        observe_fence(
            &self.fences,
            &self.registry,
            &spec.owner,
            standing.mode.as_str(),
        )?;
        let argv = self.spawn_argv(&standing)?;
        let cwd = spec.cwd.clone().or_else(|| {
            std::env::current_dir()
                .map(|path| path.to_string_lossy().into_owned())
                .ok()
        });
        // The config bounds the value far below the f64 mantissa, so
        // the cast keeps every digit (the bounded-cast carve-out).
        #[expect(
            clippy::cast_precision_loss,
            reason = "dispose_grace_ms is bounded by the config validation, well inside the f64 mantissa"
        )]
        let grace = self.config.session.dispose_grace_ms as f64;
        let terminal = crate::terminal::spawn_terminal(
            &crate::terminal::TerminalSpawnSpec {
                argv,
                cwd,
                env: child_environment(spec, dialect),
                rows: self.config.session.rows,
                cols: self.config.session.cols,
                grace_ms: grace,
            },
            Arc::new(ProcFs),
        )
        .map_err(|error| BackendSpawnError {
            message: error,
            cleanup: None,
        })?;
        let session = LocalPtySession::new(terminal, self.config.session.clone());
        match startup_session(&session, dialect, self.config.session.timeout_ms) {
            Ok(()) => Ok(session),
            Err(error) => {
                // Reject only after the unpublished resources reach
                // quiescence (the dsh rejectAfterStartupCleanup).
                let slot = session.close();
                let cleanup = crate::terminal_session::wait_close(&slot)
                    .err()
                    .map(|error| format!("terminal-bash: startup cleanup failed: {error}"));
                Err(BackendSpawnError {
                    message: error,
                    cleanup,
                })
            }
        }
    }

    /// Wraps the argv under the confinement seam unless the standing
    /// mode is `danger-full-access` (the dsh `spawnArgv`).
    ///
    /// # Errors
    ///
    /// The confinement seam's fail-closed refusal.
    fn spawn_argv(
        &self,
        standing: &SandboxExecutionPolicy,
    ) -> Result<Vec<String>, BackendSpawnError> {
        let argv = [
            vec![self.config.session.shell_path.clone()],
            self.config.session.shell_args.clone(),
        ]
        .concat();
        if standing.mode == SandboxMode::DangerFullAccess {
            return Ok(argv);
        }
        let Some(confined_policy) = SandboxPolicy::confine_from(standing) else {
            return Err(BackendSpawnError {
                message: "terminal-bash: confinement policy unavailable".to_owned(),
                cleanup: None,
            });
        };
        self.confine
            .confine(&argv, &confined_policy)
            .map(|confined| confined.argv)
            .map_err(|error| BackendSpawnError {
                message: format!("terminal-bash: confinement failed: {error}"),
                cleanup: None,
            })
    }
}

impl TerminalBackend for BashTerminalBackend {
    fn backend_type(&self) -> &str {
        &self.type_word
    }

    fn spawn(
        &self,
        spec: &BackendSpawnSpec,
    ) -> Result<Arc<dyn crate::terminal_registry::BackendSession>, BackendSpawnError> {
        self.spawn_session(spec).map(|session| session as _)
    }
}

/// The startup sequence (the dsh `startupSession`): bash rides the
/// empty-text initialize whose viewport becomes the MOTD; pwsh cannot
/// install its prompt from the environment, so the bootstrap rides the
/// send loop under one absolute deadline, and only backend stdin-read
/// evidence counts as readiness.
///
/// # Errors
///
/// The dsh startup failure texts.
fn startup_session(
    session: &Arc<LocalPtySession>,
    dialect: ShellDialect,
    timeout_ms: u64,
) -> Result<(), String> {
    if dialect == ShellDialect::Bash {
        return session.initialize();
    }
    let preamble = format!("{ENCODING_PREAMBLE}{}", pwsh_prompt_setup());
    let deadline = Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let mut viewport = String::new();
    loop {
        let first = viewport.is_empty();
        let text = if first {
            preamble.clone()
        } else {
            String::new()
        };
        let operation = session.start_send(&text, first)?;
        let result: SendResult = operation.done()?;
        match result.wait_reason {
            WaitReason::SessionExit => {
                return Err("PTY shell exited during startup".to_owned());
            }
            WaitReason::Timeout => {
                let _ignored = operation.cancel();
                return Err("PTY shell did not reach readiness before startup timeout".to_owned());
            }
            WaitReason::StdinRead => {
                session.set_motd(result.viewport);
                return Ok(());
            }
            WaitReason::InferredIdle => {}
        }
        viewport = result.viewport;
        if Instant::now() >= deadline {
            return Err("PTY shell did not reach readiness before startup timeout".to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BashTerminalBackend, RawBackendConfig, child_environment, fence_refusal, observe_fence,
        pwsh_prompt_setup, resolve_config, validate_config,
    };
    use crate::executors::PolicyResolve;
    use crate::terminal_registry::TerminalRegistry;
    use crate::terminal_session::ShellDialect;
    use fr_sandbox::seam::{SandboxExecutionPolicy, SandboxMode};
    use std::sync::Arc;

    /// The standing danger-full-access policy (the backend skips
    /// confinement under it).
    struct DangerPolicy;

    impl PolicyResolve for DangerPolicy {
        fn resolve_policy(&self) -> Result<SandboxExecutionPolicy, String> {
            Ok(SandboxExecutionPolicy {
                mode: SandboxMode::DangerFullAccess,
                workspace_root: "/".to_owned(),
                session_id: None,
            })
        }

        fn default_mode(&self) -> Result<SandboxMode, String> {
            Ok(SandboxMode::DangerFullAccess)
        }
    }

    /// A confinement test double; the danger policy never reaches it.
    struct NoConfine;

    impl crate::executors::SandboxConfine for NoConfine {
        fn confine(
            &self,
            _argv: &[String],
            _policy: &fr_sandbox::seam::SandboxPolicy,
        ) -> Result<fr_sandbox::seam::ConfinedArgv, String> {
            Err("the test policy never confines".to_owned())
        }
    }

    fn raw_with(dialect: ShellDialect) -> RawBackendConfig {
        RawBackendConfig {
            shell_dialect: Some(dialect),
            ..RawBackendConfig::default()
        }
    }

    #[test]
    fn resolve_config_defaults_to_the_bash_dialect_bundle() {
        let resolved = resolve_config(&RawBackendConfig::default()).expect("resolve");
        assert_eq!(resolved.backend_type, "shell");
        assert_eq!(resolved.session.shell_dialect, ShellDialect::Bash);
        assert_eq!(resolved.session.shell_path, "/bin/bash");
        assert_eq!(
            resolved.session.shell_args,
            vec!["--noprofile", "--norc", "-i"]
        );
        assert_eq!(resolved.session.rows, 40);
        assert_eq!(resolved.session.cols, 160);
        assert_eq!(resolved.session.scrollback_lines, 10_000);
        assert_eq!(resolved.session.scrollback_max_bytes, 4 * 1024 * 1024);
        assert_eq!(resolved.session.max_read_bytes, 256 * 1024);
        assert_eq!(resolved.session.idle_silence_ms, 3_000);
        assert_eq!(resolved.session.timeout_ms, 30_000);
    }

    #[test]
    fn resolve_config_prefers_explicit_values_and_pins_pwsh_resolution() {
        // The pwsh dialect default path resolves from PATH; a host
        // without pwsh fails the resolve (the dsh apply-time throw).
        let resolved = resolve_config(&raw_with(ShellDialect::Pwsh));
        match resolved {
            Ok(resolved) => {
                assert_eq!(resolved.session.shell_args, vec!["-NoLogo", "-NoProfile"]);
            }
            Err(error) => {
                assert!(error.starts_with("terminal-bash: pwsh resolution failed:"));
            }
        }
        let mut pinned = raw_with(ShellDialect::Pwsh);
        pinned.shell_path = Some("/usr/bin/env".to_owned());
        pinned.shell_args = Some(vec!["-Custom".to_owned()]);
        let resolved = resolve_config(&pinned).expect("explicit path and args win");
        assert_eq!(resolved.session.shell_path, "/usr/bin/env");
        assert_eq!(resolved.session.shell_args, vec!["-Custom"]);
    }

    #[test]
    fn validate_config_pins_the_verbatim_refusals() {
        let mut resolved = resolve_config(&RawBackendConfig::default()).expect("resolve");
        validate_config(&resolved).expect("defaults validate");
        resolved.backend_type = String::new();
        assert_eq!(
            validate_config(&resolved).unwrap_err(),
            "terminal-bash: backendType must be non-empty"
        );
        resolved = resolve_config(&RawBackendConfig::default()).expect("resolve");
        resolved.session.shell_path = String::new();
        assert_eq!(
            validate_config(&resolved).unwrap_err(),
            "terminal-bash: shellPath must be non-empty"
        );
        resolved = resolve_config(&RawBackendConfig::default()).expect("resolve");
        resolved.session.poll_interval_ms = 0;
        assert_eq!(
            validate_config(&resolved).unwrap_err(),
            "terminal-bash: pollIntervalMs must be a positive safe integer"
        );
        resolved = resolve_config(&RawBackendConfig::default()).expect("resolve");
        resolved.session.max_read_bytes = resolved.session.scrollback_max_bytes + 1;
        assert_eq!(
            validate_config(&resolved).unwrap_err(),
            "terminal-bash: maxReadBytes must not exceed scrollbackMaxBytes"
        );
        resolved = resolve_config(&RawBackendConfig::default()).expect("resolve");
        resolved.session.handoff_grace_ms = resolved.session.poll_interval_ms - 1;
        assert_eq!(
            validate_config(&resolved).unwrap_err(),
            "terminal-bash: handoffGraceMs must be at least pollIntervalMs so one readiness poll runs inside the grace window"
        );
        resolved = resolve_config(&RawBackendConfig::default()).expect("resolve");
        resolved.session.prompt_tail_grace_ms = resolved.session.poll_interval_ms - 1;
        assert_eq!(
            validate_config(&resolved).unwrap_err(),
            "terminal-bash: promptTailGraceMs must be zero or at least pollIntervalMs so a nonzero tolerance contains one readiness poll"
        );
        resolved = resolve_config(&RawBackendConfig::default()).expect("resolve");
        resolved.session.prompt_tail_grace_ms = resolved.session.poll_interval_ms;
        validate_config(&resolved).expect("zero and at-poll tails both pass");
    }

    #[test]
    fn child_environment_installs_the_dialect_protocols() {
        let spec = crate::terminal_registry::BackendSpawnSpec {
            session_id: "pty-1".to_owned(),
            owner: "agent-1".to_owned(),
            backend_type: "shell".to_owned(),
            name: None,
            cwd: None,
        };
        let bash = child_environment(&spec, ShellDialect::Bash);
        let find = |key: &str| {
            bash.iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(find("TERM").as_deref(), Some("dumb"));
        assert_eq!(find("PAGER").as_deref(), Some("cat"));
        assert_eq!(find("GIT_PAGER").as_deref(), Some("cat"));
        assert_eq!(find("FR_SHELL").as_deref(), Some("1"));
        assert_eq!(find("FR_SESSION_ID").as_deref(), Some("agent-1"));
        assert_eq!(find("FR_PTY_SESSION_ID").as_deref(), Some("pty-1"));
        assert_eq!(find("PS1").as_deref(), Some("fr> "));
        let prompt_command = find("PROMPT_COMMAND").expect("prompt command");
        assert!(prompt_command.contains("\\033]133;D;%s\\007"));
        assert!(prompt_command.contains("PS1='fr> '"));
        assert_eq!(
            find("BASH_SILENCE_DEPRECATION_WARNING").as_deref(),
            Some("1")
        );
        let pwsh = child_environment(&spec, ShellDialect::Pwsh);
        assert!(
            pwsh.iter()
                .any(|(name, value)| name == "NO_COLOR" && value == "1")
        );
        assert!(!pwsh.iter().any(|(name, _)| name == "PS1"));
        assert!(!pwsh.iter().any(|(name, _)| name == "PROMPT_COMMAND"));
    }

    #[test]
    fn pwsh_prompt_setup_emits_the_marker_function() {
        let setup = pwsh_prompt_setup();
        assert!(setup.starts_with("function prompt { [Console]::Write([char]27 + ']133;D;'"));
        assert!(setup.contains("[int]$LASTEXITCODE + [char]7"));
        assert!(setup.contains("'fr> '"));
    }

    #[test]
    fn fence_refusal_rides_verbatim() {
        assert_eq!(
            fence_refusal("read-only", "workspace-write"),
            "cannot change sandbox mode from \"read-only\" to \"workspace-write\" while persistent terminal sessions are open or being created; wait for creation to settle and close them first"
        );
    }

    /// One immediately-settling fake session (the registry-test fake
    /// shape, minimized for the fence's activity criterion).
    struct QuietSession;

    impl crate::terminal_registry::BackendSession for QuietSession {
        fn pid(&self) -> Option<u32> {
            None
        }

        fn status(&self) -> crate::terminal_session::SessionStatus {
            crate::terminal_session::SessionStatus::Running
        }

        fn motd(&self) -> String {
            String::new()
        }

        fn read(
            &self,
            _offset: Option<usize>,
            _count: Option<usize>,
        ) -> Result<crate::terminal_session::ReadPage, String> {
            Err("the fence test never reads".to_owned())
        }

        fn signal(&self, _name: &str) -> Result<i64, String> {
            Err("the fence test never signals".to_owned())
        }

        fn start_send(
            self: Arc<Self>,
            _text: &str,
            _submit: bool,
        ) -> Result<Arc<crate::terminal_session::SendOperation>, String> {
            Err("the fence test never sends".to_owned())
        }

        fn close(self: Arc<Self>, _reason: &str) -> Arc<crate::terminal_session::CloseSlot> {
            let slot = Arc::new(crate::terminal_session::CloseSlot::default());
            *slot.result.lock().expect("close lock") = Some(Ok(()));
            slot.ready.notify_all();
            slot
        }
    }

    /// One fake backend publishing quiet sessions under `shell`.
    struct QuietBackend;

    impl crate::terminal_registry::TerminalBackend for QuietBackend {
        fn backend_type(&self) -> &'static str {
            "shell"
        }

        fn spawn(
            &self,
            _spec: &crate::terminal_registry::BackendSpawnSpec,
        ) -> Result<Arc<dyn crate::terminal_registry::BackendSession>, super::BackendSpawnError>
        {
            Ok(Arc::new(QuietSession))
        }
    }

    #[test]
    fn fence_rejects_only_mode_changes_under_activity() {
        let registry = Arc::new(TerminalRegistry::new(Box::new(|_owner: &str| true)));
        registry
            .register_backend(Arc::new(QuietBackend))
            .expect("the first registration never duplicates");
        let fences = std::sync::Mutex::new(std::collections::HashMap::new());
        observe_fence(&fences, &registry, "a1", "read-only").expect("first observation");
        // The activity source: one published session for the owner.
        registry
            .spawn(
                "a1",
                &crate::terminal_registry::SpawnRequest {
                    backend_type: "shell".to_owned(),
                    name: None,
                    cwd: None,
                },
            )
            .expect("the quiet backend publishes");
        // Same mode under activity: no refusal.
        observe_fence(&fences, &registry, "a1", "read-only").expect("same mode");
        // A mode change with no activity: the observation records.
        observe_fence(&fences, &registry, "empty-owner", "workspace-write").expect("no activity");
        // A mode change under activity: the verbatim refusal.
        let error =
            observe_fence(&fences, &registry, "a1", "workspace-write").expect_err("refused");
        assert_eq!(error.message, fence_refusal("read-only", "workspace-write"));
    }

    #[test]
    fn backend_spawns_and_publishes_one_real_bash_session() {
        let registry = Arc::new(TerminalRegistry::new(Box::new(|_owner: &str| true)));
        let mut raw = raw_with(ShellDialect::Bash);
        raw.rows = Some(24);
        raw.cols = Some(80);
        raw.scrollback_lines = Some(500);
        raw.poll_interval_ms = Some(10);
        raw.exact_probe_after_ms = Some(60);
        raw.idle_silence_ms = Some(250);
        raw.handoff_grace_ms = Some(40);
        raw.timeout_ms = Some(4_000);
        raw.dispose_grace_ms = Some(250);
        let resolved = resolve_config(&raw).expect("resolve");
        let backend = Arc::new(BashTerminalBackend::new(
            resolved,
            Arc::new(NoConfine),
            Arc::new(DangerPolicy),
            Arc::clone(&registry),
        ));
        registry
            .register_backend(backend)
            .expect("the first registration never duplicates");
        let result = registry
            .spawn(
                "agent-1",
                &crate::terminal_registry::SpawnRequest {
                    backend_type: "shell".to_owned(),
                    name: None,
                    cwd: Some(std::env::temp_dir().to_string_lossy().into_owned()),
                },
            )
            .expect("the interactive bash reaches readiness");
        assert_eq!(result.backend_type, "shell");
        assert!(result.session_id.starts_with("pty-"));
        assert_eq!(
            result.status,
            crate::terminal_session::SessionStatus::Running
        );
        let sessions = registry.list("agent-1");
        assert_eq!(sessions.len(), 1);
        let closed = registry
            .kill("agent-1", &result.session_id, "test done")
            .expect("the close settles");
        assert!(closed);
        assert_eq!(registry.list("agent-1").len(), 0);
    }
}
