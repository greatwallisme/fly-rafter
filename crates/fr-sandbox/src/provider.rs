// Rust guideline compliant 2026-10-29
//! The local sandbox backend (dsh `sandbox-local/src/index.ts`).
//!
//! It selects the platform runner chain (Linux bwrap then Landlock;
//! macOS Seatbelt; Windows the ACL restricted-token runner), probes
//! competing candidates once, and reports each wrap's enforcement and
//! stderr classification facts. Missing or unusable confinement fails
//! closed rather than returning the original argv.
//!
//! The windows-acl rung additionally owns the write grants (Q99-B full
//! port, `#[cfg(windows)]`): the write SID is the per-workspace identity
//! while every live session receives a random private temp directory
//! and its own derived capability; the workspace-root ACE materializes
//! once per workspace per server lifetime and stands, the private-temp
//! ACEs are revoked on dispose.

use std::io::Read;
use std::sync::Arc;
use std::sync::Mutex;

use crate::profiles::bwrap_profile_args;
use crate::profiles::landlock_profile_args;
use crate::profiles::seatbelt_profile_args;
use crate::roots::canonical_path;
use crate::seam::ConfinedArgv;
use crate::seam::ConfinedMode;
use crate::seam::RunnerFailureRule;
use crate::seam::SandboxEnforcement;
use crate::seam::SandboxPolicy;
use crate::seam::SandboxUnavailableError;

/// One rung of a platform chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerKind {
    /// The bubblewrap mount-profile runner (Linux first preference).
    Bwrap,
    /// The Landlock self-restrict-then-exec launcher (`fr-landlock-run`).
    Landlock,
    /// The macOS `sandbox-exec` SBPL profile runner.
    Seatbelt,
    /// The Windows ACL restricted-token runner (`fr-windows-acl-run`).
    WindowsAcl,
}

/// The launcher's own failure exit (the CLI contract, distinct from the
/// windows-acl runner's 127).
pub const LANDLOCK_LAUNCHER_FAILURE_EXIT: i32 = 125;

/// The windows-acl runner's documented failure exit (its own contract).
pub const WINDOWS_ACL_RUNNER_FAILURE_EXIT: i32 = 127;

/// The runner chain per platform (dsh `PLATFORM_CHAINS`).
///
/// Selection is by platform FIRST, probes second — a platform's chain
/// is probed in preference order only when it has more than one
/// candidate; a sole candidate is selected directly, and its
/// execution-time refusal remains the fail-closed end.
#[must_use]
pub fn platform_chain(platform: &str) -> &'static [RunnerKind] {
    match platform {
        "linux" => &[RunnerKind::Bwrap, RunnerKind::Landlock],
        "darwin" => &[RunnerKind::Seatbelt],
        "win32" => &[RunnerKind::WindowsAcl],
        _ => &[],
    }
}

/// Maps the host OS spelling onto the chain table's platform keys.
#[must_use]
pub fn host_platform_key() -> &'static str {
    match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

/// Enforcement completeness a rung claims when selected WITHOUT a probe (a chain of one; dsh `STATIC_ENFORCEMENT`).
///
/// `landlock` is listed for
/// the table's totality but is unreachable without a probe (the Linux
/// chain only ever selects it through its probe, whose report
/// distinguishes full from per-ABI-partial).
#[must_use]
pub const fn static_enforcement(runner: RunnerKind) -> SandboxEnforcement {
    match runner {
        RunnerKind::Bwrap | RunnerKind::Landlock | RunnerKind::Seatbelt => SandboxEnforcement::Full,
        // NTFS hard links alias one file object across paths, reads stay
        // unconfined, and an AppContainer-ACL'd tree is unreadable to the
        // Low-integrity child: the backend enforces the remaining
        // ACL-addressable surface but must not advertise the absolute
        // promise.
        RunnerKind::WindowsAcl => SandboxEnforcement::Partial,
    }
}

/// The denial dialect each runner's kernel speaks (dsh
/// `DENIAL_SIGNATURES`): the case-insensitive stderr substrings a
/// denied file effect produces under it, carried on every wrap.
#[must_use]
pub fn denial_signatures(runner: RunnerKind) -> Vec<String> {
    let words: &[&str] = match runner {
        RunnerKind::Bwrap => &["read-only file system"],
        RunnerKind::Landlock => &["permission denied"],
        RunnerKind::Seatbelt => &["operation not permitted"],
        // pwsh/.NET: "Access to the path '...' is denied."; cmd: "Access
        // is denied."; Node EACCES: "permission denied"; EPERM:
        // "operation not permitted".
        RunnerKind::WindowsAcl => &[
            "access is denied",
            "access to the path",
            "permission denied",
            "operation not permitted",
        ],
    };
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// The dialect an operator-supplied `runner_command` speaks (bwrap-
/// compatible profile arguments appended; dsh `DENIAL_SIGNATURES.
/// runnerCommand`).
#[must_use]
pub fn runner_command_denial_signatures() -> Vec<String> {
    ["read-only file system", "permission denied"]
        .iter()
        .map(|word| (*word).to_owned())
        .collect()
}

/// Runner-owned fatal diagnostics (dsh `RUNNER_FAILURE_RULES`).
///
/// Bubblewrap's current fatal paths exit 1 but its public contract
/// reserves no status, and sandbox-exec publishes no launcher-failure
/// status; those backends remain signature-only. The landlock rule is
/// exit-gated on 125 and the windows-acl rule on 127, so a confined
/// command that merely prints the signature is never misclassified as
/// "the command did not run".
#[must_use]
pub fn runner_failure_rules(runner: RunnerKind) -> Vec<RunnerFailureRule> {
    match runner {
        RunnerKind::Bwrap => vec![RunnerFailureRule {
            allowed_exit_codes: None,
            fatal_signatures: vec!["bwrap: ".to_owned()],
            informational_lines: None,
        }],
        RunnerKind::Landlock => vec![RunnerFailureRule {
            allowed_exit_codes: Some(vec![LANDLOCK_LAUNCHER_FAILURE_EXIT]),
            fatal_signatures: vec!["landlock-run: ".to_owned()],
            informational_lines: Some(vec![
                "landlock-run: partial enforcement (older Landlock ABI)".to_owned(),
            ]),
        }],
        RunnerKind::Seatbelt => vec![RunnerFailureRule {
            allowed_exit_codes: None,
            fatal_signatures: vec!["sandbox-exec: ".to_owned()],
            informational_lines: None,
        }],
        RunnerKind::WindowsAcl => vec![RunnerFailureRule {
            allowed_exit_codes: Some(vec![WINDOWS_ACL_RUNNER_FAILURE_EXIT]),
            fatal_signatures: vec!["windows-acl-run: ".to_owned()],
            informational_lines: None,
        }],
    }
}

/// One probe's verdict (the entry package's `LandlockEnforcement`,
/// generalized to every rung's probe shape).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeVerdict {
    /// The rung enforces every promised file effect.
    Full,
    /// An older kernel ABI governs only a subset (still confined for
    /// everything it supports).
    Partial,
    /// Nothing can be enforced on this host; the rung is skipped.
    Unusable,
}

/// Plugin config (dsh sandbox-local `Config`): all optional, the
/// defaults live in [`ProviderConfig::default`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfig {
    /// Override the runner argv; bwrap-compatible profile arguments are
    /// appended. A non-empty override asserts full enforcement and
    /// skips built-in selection and probing. A runner that starts but
    /// refuses its profile must be identifiable by
    /// [`ProviderConfig::runner_failure_signatures`].
    pub runner_command: Vec<String>,
    /// Case-insensitive stderr substrings emitted when a configured
    /// runner command refuses its profile before executing the wrapped
    /// command. Required and non-empty with `runner_command`; rejected
    /// without it. Each entry is a non-empty, single-line, case-
    /// insensitive substring.
    pub runner_failure_signatures: Vec<String>,
    /// Positive timeout for each functional probe; zero would mean
    /// unbounded to a naive waiter.
    pub probe_timeout_ms: u64,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            runner_command: Vec::new(),
            runner_failure_signatures: Vec::new(),
            probe_timeout_ms: 5_000,
        }
    }
}

impl ProviderConfig {
    /// Validates the pairing rules (dsh constructor refusals, verbatim).
    ///
    /// # Errors
    ///
    /// Returns the verbatim dsh refusal for the first violated rule.
    pub fn validated(&self) -> Result<(), String> {
        if self.runner_command.is_empty() && !self.runner_failure_signatures.is_empty() {
            return Err("sandbox-local: runnerFailureSignatures requires runnerCommand".to_owned());
        }
        if !self.runner_command.is_empty() && self.runner_failure_signatures.is_empty() {
            return Err(
                "sandbox-local: runnerCommand requires at least one runnerFailureSignatures entry"
                    .to_owned(),
            );
        }
        if self
            .runner_failure_signatures
            .iter()
            .any(|signature| signature.trim().is_empty() || signature.contains(['\r', '\n']))
        {
            return Err(
                "sandbox-local: runnerFailureSignatures entries must be non-empty single-line \
strings"
                    .to_owned(),
            );
        }
        // A probe bound must be positive: an unvalidated 0 would silently
        // mean "unbounded" — the opposite of what the field promises.
        if self.probe_timeout_ms == 0 {
            return Err(
                "sandbox-local: probeTimeoutMs must be a positive finite number".to_owned(),
            );
        }
        Ok(())
    }
}

/// A probe injection hook (the dsh `SandboxInternals` test seams).
pub type ProbeFn = Arc<dyn Fn() -> bool + Send + Sync>;
/// A Landlock probe injection hook (receives the launcher path).
pub type LandlockProbeFn = Arc<dyn Fn(&str) -> ProbeVerdict + Send + Sync>;
/// A temp-directory removal injection hook.
pub type RmTempDirFn = Arc<dyn Fn(&str) + Send + Sync>;

/// Test hook: inject probe verdicts, fake launchers, or a platform
/// without real runners (dsh `SandboxInternals`).
#[derive(Clone, Default)]
pub struct SandboxInternals {
    /// Replaces the platform key for chain selection (exercise any
    /// platform's chain from any host).
    pub platform: Option<String>,
    /// Replaces the platform's chain wholesale (walk mechanics).
    pub chain: Option<Vec<RunnerKind>>,
    /// Replaces the functional bwrap probe (the Linux chain's first
    /// rung).
    pub probe_bwrap: Option<ProbeFn>,
    /// Replaces the functional Landlock launcher probe (the Linux
    /// chain's second rung).
    pub probe_landlock: Option<LandlockProbeFn>,
    /// Replaces the functional Seatbelt probe (the darwin chain's sole
    /// rung).
    pub probe_seatbelt: Option<ProbeFn>,
    /// Replaces the functional windows-acl probe (the win32 chain's
    /// sole rung).
    pub probe_windows_acl: Option<ProbeFn>,
    /// Replaces the resolved launcher path (a fake launcher script).
    pub landlock_launcher: Option<String>,
    /// Replaces the `sandbox-exec` executable the probe and wraps
    /// invoke.
    pub seatbelt_exec: Option<String>,
    /// Replaces the resolved windows-acl runner argv prefix (a fake
    /// runner).
    pub windows_acl_runner_args: Option<Vec<String>>,
    /// Replaces the private-temp-directory removal at provider dispose
    /// (a throwing fake exercises the cleanup-failure path).
    pub rm_temp_dir: Option<RmTempDirFn>,
}

impl std::fmt::Debug for SandboxInternals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxInternals")
            .field("platform", &self.platform)
            .field("chain", &self.chain)
            .field("landlock_launcher", &self.landlock_launcher)
            .field("seatbelt_exec", &self.seatbelt_exec)
            .field("windows_acl_runner_args", &self.windows_acl_runner_args)
            .field(
                "probes_injected",
                &(
                    self.probe_bwrap.is_some(),
                    self.probe_landlock.is_some(),
                    self.probe_seatbelt.is_some(),
                    self.probe_windows_acl.is_some(),
                    self.rm_temp_dir.is_some(),
                ),
            )
            .finish()
    }
}

/// The chain's verdict: which runner confines, and how completely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectedRunner {
    /// The selected rung.
    pub runner: RunnerKind,
    /// The enforcement completeness it achieves.
    pub enforcement: SandboxEnforcement,
}

/// The `confine` refusal shapes: the fail-closed backend gap, or a
/// structurally unsupported arm (a non-Windows build reaching the
/// windows-acl session grants).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfineFailure {
    /// No backend on this host can enforce the requested mode.
    Unavailable(SandboxUnavailableError),
    /// The arm exists only in another platform build.
    Unsupported(String),
}

impl From<SandboxUnavailableError> for ConfineFailure {
    fn from(error: SandboxUnavailableError) -> Self {
        Self::Unavailable(error)
    }
}

impl std::fmt::Display for ConfineFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(error) => write!(f, "{error}"),
            Self::Unsupported(reason) => f.write_str(reason),
        }
    }
}

/// Runs one probe child to settlement under a deadline; `None` means a
/// spawn failure or a timeout kill (both probe `unusable`, exactly like
/// a failed `spawnSync`).
fn child_exit_status(program: &str, args: &[String], timeout_ms: u64) -> Option<i32> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return status.code();
        }
        if std::time::Instant::now() >= deadline {
            let _ignored = child.kill();
            let _ignored = child.wait();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Runs one probe child capturing stdout (the Landlock probe reads
/// enforcement completeness from the single report line).
fn child_exit_status_with_stdout(
    program: &str,
    args: &[String],
    timeout_ms: u64,
) -> Option<(i32, String)> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let status = loop {
        if let Ok(Some(status)) = child.try_wait() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ignored = child.kill();
            break child.wait().ok()?;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    // The probe prints exactly one short line; the pipe buffer holds it
    // and the writer closed at exit, so the drain cannot block.
    let mut stdout = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ignored = pipe.read_to_string(&mut stdout);
    }
    status.code().map(|code| (code, stdout))
}

/// Probe whether `bwrap` can create the profile (dsh
/// `defaultProbeBwrap`).
fn default_probe_bwrap(timeout_ms: u64) -> bool {
    let policy = SandboxPolicy {
        mode: ConfinedMode::ReadOnly,
        workspace_root: "/".to_owned(),
        session_id: None,
    };
    let mut args = vec!["bwrap".to_owned()];
    args.extend(bwrap_profile_args(&policy));
    args.push("--".to_owned());
    args.push("true".to_owned());
    child_exit_status("bwrap", &args[1..], timeout_ms) == Some(0)
}

/// Functional Seatbelt probe (dsh `defaultProbeSeatbelt`): apply the
/// real `read-only` profile through `sandbox-exec -p` and run `true`
/// under it — exit 0 means the kernel accepted and enforced the
/// profile. A missing `sandbox-exec` (every non-macOS host) fails the
/// spawn and probes unusable, exactly like the other rungs' absent
/// binaries.
fn default_probe_seatbelt(seatbelt_exec: &str, timeout_ms: u64) -> bool {
    let policy = SandboxPolicy {
        mode: ConfinedMode::ReadOnly,
        workspace_root: "/".to_owned(),
        session_id: None,
    };
    let mut args = vec![seatbelt_exec.to_owned()];
    args.extend(seatbelt_profile_args(&policy));
    args.push("--".to_owned());
    args.push("true".to_owned());
    child_exit_status(seatbelt_exec, &args[1..], timeout_ms) == Some(0)
}

/// Functional Landlock probe (the entry package's `probe`):
/// `fr-landlock-run --probe` builds and enforces a maximal ruleset in a
/// short-lived child and exits 0 only when the running kernel actually
/// enforces it; the one report line distinguishes complete from
/// per-ABI-subset enforcement. A failed or timed-out spawn probes
/// unusable.
fn default_probe_landlock(launcher: &str, timeout_ms: u64) -> ProbeVerdict {
    let Some((code, stdout)) =
        child_exit_status_with_stdout(launcher, &["--probe".to_owned()], timeout_ms)
    else {
        return ProbeVerdict::Unusable;
    };
    if code != 0 {
        return ProbeVerdict::Unusable;
    }
    if stdout.contains("partially enforced") {
        ProbeVerdict::Partial
    } else {
        ProbeVerdict::Full
    }
}

/// Functional windows-acl probe (dsh `defaultProbeWindowsAcl`): run the
/// runner in read-only mode (zero grants, no ACL mutation) around
/// `cmd /c exit 0` — exit 0 means the runner created the restricted
/// token and spawned the child under it.
fn default_probe_windows_acl(runner_invocation: &[String], timeout_ms: u64) -> bool {
    let Some(program) = runner_invocation.first() else {
        return false;
    };
    let mut args: Vec<String> = runner_invocation[1..].to_vec();
    let tempdir = std::env::temp_dir().to_string_lossy().into_owned();
    args.extend([
        "--workspace".to_owned(),
        tempdir.clone(),
        "--temp".to_owned(),
        tempdir,
        "--mode".to_owned(),
        "read-only".to_owned(),
        "--".to_owned(),
        "cmd".to_owned(),
        "/c".to_owned(),
        "exit".to_owned(),
        "0".to_owned(),
    ]);
    child_exit_status(program, &args, timeout_ms) == Some(0)
}

/// The cached verdict of the chain walk (the dsh
/// `selectedRunner | undefined` pair as one explicit state).
#[derive(Debug, Clone, Copy)]
enum ChainCache {
    /// No confined wrap has needed the verdict yet.
    Unresolved,
    /// The selected rung and its enforcement.
    Selected(SelectedRunner),
    /// The platform chain or every probe refused; confine fails closed.
    Unavailable,
}

/// The local process-sandbox provider (dsh `LocalSandboxProvider`).
/// Caches the chain verdict and, on the windows-acl rung, the write
/// grants; the one-time probes spawn nothing else.
#[derive(Debug)]
pub struct LocalSandboxProvider {
    runner_command: Option<Vec<String>>,
    configured_runner_failure_signatures: Vec<String>,
    probe_timeout_ms: u64,
    /// Cached chain verdict: [`ChainCache::Unresolved`] until the
    /// first confined wrap needs it.
    selected: Mutex<ChainCache>,
    /// Server-lifetime write grants (windows-acl rung): the standing
    /// workspace-root grant per workspace and the revocable
    /// private-temp grant per live session/workspace pair.
    #[cfg(windows)]
    workspace_grants: Mutex<std::collections::HashMap<String, crate::win::grant::AclWriteGrant>>,
    #[cfg(windows)]
    temp_capabilities: Mutex<std::collections::HashMap<String, crate::win::TempCapability>>,
    /// Test hooks (mirrors the dsh `internals` object).
    pub internals: SandboxInternals,
}

impl LocalSandboxProvider {
    /// Builds the provider over validated config.
    ///
    /// # Errors
    ///
    /// Returns the verbatim validation refusals (the dsh constructor
    /// throws).
    pub fn new(config: &ProviderConfig, internals: SandboxInternals) -> Result<Self, String> {
        config.validated()?;
        Ok(Self {
            runner_command: (!config.runner_command.is_empty())
                .then(|| config.runner_command.clone()),
            configured_runner_failure_signatures: config.runner_failure_signatures.clone(),
            probe_timeout_ms: config.probe_timeout_ms,
            selected: Mutex::new(ChainCache::Unresolved),
            #[cfg(windows)]
            workspace_grants: Mutex::new(std::collections::HashMap::new()),
            #[cfg(windows)]
            temp_capabilities: Mutex::new(std::collections::HashMap::new()),
            internals,
        })
    }

    /// Wraps `argv` in the selected runner's invocation for `policy`
    /// (dsh `confine`): the configured `runner_command` when present
    /// (the operator's assertion, no probe), else the platform chain's
    /// runner speaking its own profile dialect.
    ///
    /// # Errors
    ///
    /// Fails closed with [`ConfineFailure::Unavailable`] when the
    /// platform has no usable runner; the command never runs.
    pub fn confine(
        &self,
        argv: &[String],
        policy: &SandboxPolicy,
    ) -> Result<ConfinedArgv, ConfineFailure> {
        let policy = SandboxPolicy {
            workspace_root: canonical_path(&policy.workspace_root),
            ..policy.clone()
        };
        if let Some(runner_command) = &self.runner_command {
            let mut wrapped = runner_command.clone();
            wrapped.extend(bwrap_profile_args(&policy));
            wrapped.push("--".to_owned());
            wrapped.extend_from_slice(argv);
            return Ok(ConfinedArgv {
                argv: wrapped,
                enforcement: SandboxEnforcement::Full,
                denial_signatures: runner_command_denial_signatures(),
                runner_failure_rules: vec![RunnerFailureRule {
                    allowed_exit_codes: None,
                    fatal_signatures: self.configured_runner_failure_signatures.clone(),
                    informational_lines: None,
                }],
            });
        }
        let selected = self.select_runner(policy.mode)?;
        let mut runner_argv = self.runner_argv(selected.runner, &policy)?;
        runner_argv.push("--".to_owned());
        runner_argv.extend_from_slice(argv);
        Ok(ConfinedArgv {
            argv: runner_argv,
            enforcement: selected.enforcement,
            denial_signatures: denial_signatures(selected.runner),
            runner_failure_rules: runner_failure_rules(selected.runner),
        })
    }

    /// Resolves which runner confines commands, once, for the
    /// provider's lifetime; fail closed when the platform has no chain
    /// or no candidate passes.
    fn select_runner(&self, mode: ConfinedMode) -> Result<SelectedRunner, ConfineFailure> {
        let mut cached = self.selected.lock().expect("selected runner lock");
        if matches!(*cached, ChainCache::Unresolved) {
            *cached = self
                .chain_verdict()
                .map_or(ChainCache::Unavailable, ChainCache::Selected);
        }
        match *cached {
            ChainCache::Selected(runner) => Ok(runner),
            ChainCache::Unresolved | ChainCache::Unavailable => Err(ConfineFailure::Unavailable(
                SandboxUnavailableError::new(mode),
            )),
        }
    }

    /// Walks this platform's chain (dsh `chainVerdict`): sole candidate
    /// unprobed, several probed in order, none usable → `None`.
    fn chain_verdict(&self) -> Option<SelectedRunner> {
        let chain: &[RunnerKind] = if let Some(chain) = &self.internals.chain {
            chain
        } else {
            let fallback = host_platform_key();
            platform_chain(self.internals.platform.as_deref().unwrap_or(fallback))
        };
        let first = chain.first()?;
        // A sole candidate needs no arbitration; its execution-time
        // refusal still fails closed.
        if chain.len() == 1 {
            return Some(SelectedRunner {
                runner: *first,
                enforcement: static_enforcement(*first),
            });
        }
        for runner in chain {
            let enforcement = match self.probe_runner(*runner) {
                ProbeVerdict::Unusable => continue,
                ProbeVerdict::Full => SandboxEnforcement::Full,
                ProbeVerdict::Partial => SandboxEnforcement::Partial,
            };
            return Some(SelectedRunner {
                runner: *runner,
                enforcement,
            });
        }
        None
    }

    /// One rung's functional probe (each at most once, via the chain
    /// walk; dsh `probeRunner`).
    fn probe_runner(&self, runner: RunnerKind) -> ProbeVerdict {
        match runner {
            RunnerKind::Bwrap => self.internals.probe_bwrap.as_ref().map_or_else(
                || bool_probe_verdict(default_probe_bwrap(self.probe_timeout_ms)),
                |probe| bool_probe_verdict(probe()),
            ),
            RunnerKind::Landlock => self.internals.probe_landlock.as_ref().map_or_else(
                || default_probe_landlock(&self.landlock_launcher(), self.probe_timeout_ms),
                |probe| probe(&self.landlock_launcher()),
            ),
            RunnerKind::Seatbelt => {
                let exec = self.seatbelt_exec();
                self.internals.probe_seatbelt.as_ref().map_or_else(
                    || bool_probe_verdict(default_probe_seatbelt(&exec, self.probe_timeout_ms)),
                    |probe| bool_probe_verdict(probe()),
                )
            }
            RunnerKind::WindowsAcl => self.internals.probe_windows_acl.as_ref().map_or_else(
                || {
                    windows_probe_verdict(default_probe_windows_acl(
                        &self.windows_acl_runner_invocation(),
                        self.probe_timeout_ms,
                    ))
                },
                |probe| windows_probe_verdict(probe()),
            ),
        }
    }

    /// The selected rung's runner invocation (program + profile
    /// arguments) for one policy.
    fn runner_argv(
        &self,
        runner: RunnerKind,
        policy: &SandboxPolicy,
    ) -> Result<Vec<String>, ConfineFailure> {
        match runner {
            RunnerKind::Bwrap => {
                let mut argv = vec!["bwrap".to_owned()];
                argv.extend(bwrap_profile_args(policy));
                Ok(argv)
            }
            RunnerKind::Landlock => {
                let mut argv = vec![self.landlock_launcher()];
                argv.extend(landlock_profile_args(policy));
                Ok(argv)
            }
            RunnerKind::Seatbelt => {
                let mut argv = vec![self.seatbelt_exec()];
                argv.extend(seatbelt_profile_args(policy));
                Ok(argv)
            }
            RunnerKind::WindowsAcl => self.windows_acl_runner_argv(policy),
        }
    }

    /// The Landlock launcher to probe and exec (test hook over the
    /// resolved one). The default resolves beside the running
    /// executable — the deployment unit that ships this provider ships
    /// the launcher with it. Existence is deliberately NOT checked:
    /// the probe is the single availability signal (a missing binary
    /// probes unusable exactly like an unenforcing kernel).
    fn landlock_launcher(&self) -> String {
        if let Some(launcher) = &self.internals.landlock_launcher {
            return launcher.clone();
        }
        default_landlock_launcher_path()
    }

    /// The `sandbox-exec` executable to probe and exec (test hook over
    /// the system one).
    fn seatbelt_exec(&self) -> String {
        self.internals
            .seatbelt_exec
            .clone()
            .unwrap_or_else(|| "sandbox-exec".to_owned())
    }

    /// The windows-acl runner argv prefix: the resolved sibling binary
    /// of the running executable (a future native-exe runner keeps the
    /// same argv contract and only swaps these entries). Existence is
    /// deliberately not checked (the probe/refusal contract governs).
    fn windows_acl_runner_invocation(&self) -> Vec<String> {
        if let Some(args) = &self.internals.windows_acl_runner_args {
            return args.clone();
        }
        vec![default_windows_acl_runner_path()]
    }

    /// The windows-acl runner argv for one policy (dsh
    /// `windowsAclRunnerArgv`): with a calling session under
    /// workspace-write, the grants are materialized once per provider
    /// lifetime and the runner receives `--write-sid` plus
    /// `--temp-write-sid` and grants nothing itself. Agentless or
    /// read-only calls pass the ambient temp root and no SID flags: the
    /// runner creates and removes a random private child directory for
    /// that one invocation.
    fn windows_acl_runner_argv(
        &self,
        policy: &SandboxPolicy,
    ) -> Result<Vec<String>, ConfineFailure> {
        let tempdir = std::env::temp_dir().to_string_lossy().into_owned();
        let agentless = |tempdir: String| -> Vec<String> {
            let mut argv = self.windows_acl_runner_invocation();
            argv.extend([
                "--workspace".to_owned(),
                policy.workspace_root.clone(),
                "--temp".to_owned(),
                tempdir,
                "--mode".to_owned(),
                policy.mode.as_str().to_owned(),
            ]);
            argv
        };
        match policy.session_id.as_deref() {
            None => Ok(agentless(tempdir)),
            Some(_) if policy.mode == ConfinedMode::ReadOnly => Ok(agentless(tempdir)),
            #[cfg(windows)]
            Some(session_id) => {
                let (temp_dir, temp_write_sid) =
                    self.materialize_acl_grant(session_id, &policy.workspace_root)?;
                let mut argv = self.windows_acl_runner_invocation();
                argv.extend([
                    "--workspace".to_owned(),
                    policy.workspace_root.clone(),
                    "--temp".to_owned(),
                    temp_dir,
                    "--mode".to_owned(),
                    policy.mode.as_str().to_owned(),
                    "--write-sid".to_owned(),
                    crate::win::workspace_write_sid(&policy.workspace_root),
                    "--temp-write-sid".to_owned(),
                    temp_write_sid,
                ]);
                Ok(argv)
            }
            #[cfg(not(windows))]
            // The session-grant arm exists only in Windows builds; every
            // other build refuses the arm structurally.
            Some(_) => Err(ConfineFailure::Unsupported(
                "sandbox: the windows-acl session grants exist only in Windows builds".to_owned(),
            )),
        }
    }

    /// Materializes one workspace-write policy's ACEs once per provider
    /// lifetime (dsh `materializeAclGrant`, Windows builds only);
    /// returns the pair's private temp directory and its write SID.
    #[cfg(windows)]
    fn materialize_acl_grant(
        &self,
        session_id: &str,
        workspace_root: &str,
    ) -> Result<(String, String), ConfineFailure> {
        crate::win::materialize_grant(
            session_id,
            workspace_root,
            &self.workspace_grants,
            &self.temp_capabilities,
            &self.internals,
        )
        .map_err(ConfineFailure::Unsupported)
    }

    /// Disposes every write grant (provider teardown; dsh
    /// `revokeAclGrants`): the revocable temp ACEs are revoked, the
    /// private temp directories are removed, and every SID allocation
    /// is freed; the standing workspace ACEs stay (the reuse cache).
    /// Cleanup failures are reported, not thrown. On non-Windows
    /// builds this is a no-op.
    ///
    /// # Errors
    ///
    /// Returns every cleanup failure joined, if any occurred.
    #[cfg_attr(
        not(windows),
        expect(
            clippy::missing_const_for_fn,
            reason = "the Windows arm calls the runtime grant teardown; this build's arm is a no-op"
        )
    )]
    pub fn dispose(&self) -> Result<(), String> {
        #[cfg(windows)]
        {
            return crate::win::revoke_grants(
                &self.workspace_grants,
                &self.temp_capabilities,
                &self.internals,
            );
        }
        #[cfg(not(windows))]
        {
            Ok(())
        }
    }
}

/// Maps a boolean probe onto the verdict shape (passing bwrap and
/// Seatbelt probes are always full enforcement — their profiles govern
/// every promised file effect by construction).
const fn bool_probe_verdict(passed: bool) -> ProbeVerdict {
    if passed {
        ProbeVerdict::Full
    } else {
        ProbeVerdict::Unusable
    }
}

/// Maps the windows-acl probe (always partial for its documented
/// hard-link, unconfined-read, and `AppContainer` boundaries).
const fn windows_probe_verdict(passed: bool) -> ProbeVerdict {
    if passed {
        ProbeVerdict::Partial
    } else {
        ProbeVerdict::Unusable
    }
}

/// The launcher binary's file name beside the running executable.
#[cfg(not(windows))]
const LANDLOCK_BIN_NAME: &str = "fr-landlock-run";
/// The launcher binary's file name beside the running executable.
#[cfg(windows)]
const LANDLOCK_BIN_NAME: &str = "fr-landlock-run.exe";

/// Resolves the launcher path beside the running executable.
///
/// The deployment unit that ships this provider ships the launcher
/// with it; never cwd-relative (a spawnable relative path would hand
/// cwd control over which binary confines).
#[must_use]
pub fn default_landlock_launcher_path() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(LANDLOCK_BIN_NAME)))
        .map_or_else(
            || LANDLOCK_BIN_NAME.to_owned(),
            |path| path.to_string_lossy().into_owned(),
        )
}

/// The windows-acl runner binary's file name beside the running
/// executable.
#[cfg(not(windows))]
const WINDOWS_ACL_BIN_NAME: &str = "fr-windows-acl-run";
/// The windows-acl runner binary's file name beside the running
/// executable.
#[cfg(windows)]
const WINDOWS_ACL_BIN_NAME: &str = "fr-windows-acl-run.exe";

/// Resolves the windows-acl runner path beside the running executable.
#[must_use]
pub fn default_windows_acl_runner_path() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(WINDOWS_ACL_BIN_NAME)))
        .map_or_else(
            || WINDOWS_ACL_BIN_NAME.to_owned(),
            |path| path.to_string_lossy().into_owned(),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(mode: ConfinedMode) -> SandboxPolicy {
        SandboxPolicy {
            mode,
            workspace_root: "/ws".to_owned(),
            session_id: None,
        }
    }

    fn argv() -> Vec<String> {
        vec!["bash".to_owned(), "-c".to_owned(), "true".to_owned()]
    }

    #[test]
    fn config_pairing_rules_refuse_verbatim() {
        let mut config = ProviderConfig {
            runner_failure_signatures: vec!["boom".to_owned()],
            ..ProviderConfig::default()
        };
        assert_eq!(
            config.validated().unwrap_err(),
            "sandbox-local: runnerFailureSignatures requires runnerCommand"
        );
        config.runner_command = vec!["my-runner".to_owned()];
        config.runner_failure_signatures = Vec::new();
        assert_eq!(
            config.validated().unwrap_err(),
            "sandbox-local: runnerCommand requires at least one runnerFailureSignatures entry"
        );
        config.runner_failure_signatures = vec!["  ".to_owned(), "x".to_owned()];
        assert_eq!(
            config.validated().unwrap_err(),
            "sandbox-local: runnerFailureSignatures entries must be non-empty single-line strings"
        );
        config.runner_failure_signatures = vec!["two\nlines".to_owned()];
        assert_eq!(
            config.validated().unwrap_err(),
            "sandbox-local: runnerFailureSignatures entries must be non-empty single-line strings"
        );
        config.runner_failure_signatures = vec!["runner: ".to_owned()];
        config.probe_timeout_ms = 0;
        assert_eq!(
            config.validated().unwrap_err(),
            "sandbox-local: probeTimeoutMs must be a positive finite number"
        );
        config.probe_timeout_ms = 5_000;
        config.validated().expect("paired config is fine");
    }

    #[test]
    fn a_configured_runner_command_asserts_full_enforcement_unprobed() {
        let provider = LocalSandboxProvider::new(
            &ProviderConfig {
                runner_command: vec!["my-runner".to_owned(), "--stage".to_owned()],
                runner_failure_signatures: vec!["my-runner: ".to_owned()],
                probe_timeout_ms: 5_000,
            },
            SandboxInternals::default(),
        )
        .expect("provider");
        let confined = provider
            .confine(&argv(), &policy(ConfinedMode::ReadOnly))
            .expect("wrapped");
        assert_eq!(
            confined.argv,
            [
                "my-runner",
                "--stage",
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--unshare-pid",
                "--proc",
                "/proc",
                "--die-with-parent",
                "--",
                "bash",
                "-c",
                "true"
            ]
        );
        assert_eq!(confined.enforcement, SandboxEnforcement::Full);
        assert_eq!(
            confined.denial_signatures,
            [
                "read-only file system".to_owned(),
                "permission denied".to_owned()
            ]
        );
        assert_eq!(
            confined.runner_failure_rules,
            vec![RunnerFailureRule {
                allowed_exit_codes: None,
                fatal_signatures: vec!["my-runner: ".to_owned()],
                informational_lines: None,
            }]
        );
    }

    #[test]
    fn the_linux_chain_probes_in_preference_order() {
        let provider = LocalSandboxProvider::new(
            &ProviderConfig::default(),
            SandboxInternals {
                probe_bwrap: Some(Arc::new(|| false)),
                probe_landlock: Some(Arc::new(|_launcher| ProbeVerdict::Full)),
                landlock_launcher: Some("/opt/fake/landlock-run".to_owned()),
                ..SandboxInternals::default()
            },
        )
        .expect("provider");
        let confined = provider
            .confine(&argv(), &policy(ConfinedMode::ReadOnly))
            .expect("wrapped");
        assert_eq!(
            confined.argv.first().map(String::as_str),
            Some("/opt/fake/landlock-run")
        );
        assert_eq!(confined.enforcement, SandboxEnforcement::Full);
        assert_eq!(confined.denial_signatures, ["permission denied".to_owned()]);
    }

    #[test]
    fn the_linux_chain_degrades_to_a_partial_landlock() {
        let provider = LocalSandboxProvider::new(
            &ProviderConfig::default(),
            SandboxInternals {
                probe_bwrap: Some(Arc::new(|| false)),
                probe_landlock: Some(Arc::new(|_launcher| ProbeVerdict::Partial)),
                landlock_launcher: Some("/opt/fake/landlock-run".to_owned()),
                ..SandboxInternals::default()
            },
        )
        .expect("provider");
        let confined = provider
            .confine(&argv(), &policy(ConfinedMode::WorkspaceWrite))
            .expect("wrapped");
        assert_eq!(confined.enforcement, SandboxEnforcement::Partial);
        assert_eq!(
            confined.argv,
            [
                "/opt/fake/landlock-run",
                "--ro",
                "/",
                "--rw",
                "/dev/null",
                "--rw",
                "/tmp",
                "--rw",
                "/ws",
                "--",
                "bash",
                "-c",
                "true"
            ]
        );
        assert_eq!(
            confined.runner_failure_rules,
            vec![RunnerFailureRule {
                allowed_exit_codes: Some(vec![125]),
                fatal_signatures: vec!["landlock-run: ".to_owned()],
                informational_lines: Some(vec![
                    "landlock-run: partial enforcement (older Landlock ABI)".to_owned()
                ]),
            }]
        );
    }

    #[test]
    fn a_sole_candidate_is_selected_without_probing() {
        for (platform, runner, prefix) in [
            ("darwin", RunnerKind::Seatbelt, "sandbox-exec"),
            (
                "win32",
                RunnerKind::WindowsAcl,
                "C:/fake/fr-windows-acl-run.exe",
            ),
        ] {
            let provider = LocalSandboxProvider::new(
                &ProviderConfig::default(),
                SandboxInternals {
                    platform: Some(platform.to_owned()),
                    seatbelt_exec: Some("sandbox-exec".to_owned()),
                    windows_acl_runner_args: Some(vec![prefix.to_owned()]),
                    ..SandboxInternals::default()
                },
            )
            .expect("provider");
            let confined = provider
                .confine(&argv(), &policy(ConfinedMode::ReadOnly))
                .expect("wrapped");
            assert_eq!(confined.argv.first().map(String::as_str), Some(prefix));
            // darwin claims full; the ACL rung always reports partial.
            assert_eq!(
                confined.enforcement,
                static_enforcement(runner),
                "sole candidate carries its static enforcement"
            );
        }
    }

    #[test]
    fn an_empty_chain_or_all_unusable_probes_fails_closed() {
        let provider = LocalSandboxProvider::new(
            &ProviderConfig::default(),
            SandboxInternals {
                platform: Some("sunos".to_owned()),
                ..SandboxInternals::default()
            },
        )
        .expect("provider");
        let refusal = provider
            .confine(&argv(), &policy(ConfinedMode::ReadOnly))
            .unwrap_err();
        assert!(
            matches!(refusal, ConfineFailure::Unavailable(_)),
            "no chain refuses closed: {refusal}"
        );

        let provider = LocalSandboxProvider::new(
            &ProviderConfig::default(),
            SandboxInternals {
                probe_bwrap: Some(Arc::new(|| false)),
                probe_landlock: Some(Arc::new(|_launcher| ProbeVerdict::Unusable)),
                ..SandboxInternals::default()
            },
        )
        .expect("provider");
        let refusal = provider
            .confine(&argv(), &policy(ConfinedMode::WorkspaceWrite))
            .unwrap_err();
        let ConfineFailure::Unavailable(error) = refusal else {
            panic!("an all-unusable chain refuses closed");
        };
        assert_eq!(
            error.to_string(),
            crate::seam::sandbox_unavailable_text(ConfinedMode::WorkspaceWrite, None)
        );
        assert_eq!(crate::seam::SANDBOX_UNAVAILABLE, "SANDBOX_UNAVAILABLE");
    }

    #[test]
    fn the_verdict_is_cached_across_calls() {
        let probes = Arc::new(Mutex::new(0u32));
        let counting = {
            let probes = Arc::clone(&probes);
            move || {
                *probes.lock().expect("lock") += 1;
                false
            }
        };
        let provider = LocalSandboxProvider::new(
            &ProviderConfig::default(),
            SandboxInternals {
                probe_landlock: Some(Arc::new(|_launcher| ProbeVerdict::Full)),
                probe_bwrap: Some(Arc::new(counting)),
                ..SandboxInternals::default()
            },
        )
        .expect("provider");
        provider
            .confine(&argv(), &policy(ConfinedMode::ReadOnly))
            .expect("first");
        provider
            .confine(&argv(), &policy(ConfinedMode::ReadOnly))
            .expect("second");
        assert_eq!(*probes.lock().expect("lock"), 1, "the bwrap probe ran once");
    }

    #[test]
    fn the_seatbelt_wrap_carries_the_sbpl_profile() {
        let provider = LocalSandboxProvider::new(
            &ProviderConfig::default(),
            SandboxInternals {
                platform: Some("darwin".to_owned()),
                ..SandboxInternals::default()
            },
        )
        .expect("provider");
        let confined = provider
            .confine(&argv(), &policy(ConfinedMode::WorkspaceWrite))
            .expect("wrapped");
        assert_eq!(
            confined.argv.first().map(String::as_str),
            Some("sandbox-exec")
        );
        let joined = confined.argv[1..].join(" ");
        assert!(
            joined.starts_with("-p (version 1) (allow default) (deny file-write*)"),
            "the SBPL profile rides the wrap: {joined}"
        );
        assert!(
            joined.ends_with("-- bash -c true"),
            "the separator and command follow: {joined}"
        );
        assert_eq!(
            confined.denial_signatures,
            ["operation not permitted".to_owned()]
        );
    }

    #[test]
    fn the_windows_acl_agentless_wrap_carries_the_three_segments() {
        let provider = LocalSandboxProvider::new(
            &ProviderConfig::default(),
            SandboxInternals {
                platform: Some("win32".to_owned()),
                windows_acl_runner_args: Some(vec!["C:/fake/fr-windows-acl-run.exe".to_owned()]),
                ..SandboxInternals::default()
            },
        )
        .expect("provider");
        let confined = provider
            .confine(&argv(), &policy(ConfinedMode::WorkspaceWrite))
            .expect("wrapped");
        assert_eq!(
            confined.argv,
            [
                "C:/fake/fr-windows-acl-run.exe",
                "--workspace",
                "/ws",
                "--temp",
                std::env::temp_dir().to_string_lossy().as_ref(),
                "--mode",
                "workspace-write",
                "--",
                "bash",
                "-c",
                "true",
            ]
        );
        assert_eq!(confined.enforcement, SandboxEnforcement::Partial);
        assert_eq!(
            confined.runner_failure_rules,
            vec![RunnerFailureRule {
                allowed_exit_codes: Some(vec![127]),
                fatal_signatures: vec!["windows-acl-run: ".to_owned()],
                informational_lines: None,
            }]
        );
    }

    #[test]
    fn the_windows_acl_session_grant_arm_is_windows_only() {
        let provider = LocalSandboxProvider::new(
            &ProviderConfig::default(),
            SandboxInternals {
                platform: Some("win32".to_owned()),
                windows_acl_runner_args: Some(vec!["C:/fake/fr-windows-acl-run.exe".to_owned()]),
                ..SandboxInternals::default()
            },
        )
        .expect("provider");
        let mut sessioned = policy(ConfinedMode::WorkspaceWrite);
        sessioned.session_id = Some("s1".to_owned());
        let refusal = provider.confine(&argv(), &sessioned).unwrap_err();
        if cfg!(windows) {
            assert!(
                refusal.to_string().contains("fr-windows-acl-run")
                    || refusal.to_string().contains("workspace"),
                "the Windows build materializes grants: {refusal}"
            );
        } else {
            assert_eq!(
                refusal.to_string(),
                "sandbox: the windows-acl session grants exist only in Windows builds"
            );
        }
    }

    #[test]
    fn the_workspace_root_is_canonicalized_into_the_wrap() {
        let provider = LocalSandboxProvider::new(
            &ProviderConfig::default(),
            SandboxInternals {
                platform: Some("darwin".to_owned()),
                ..SandboxInternals::default()
            },
        )
        .expect("provider");
        let mut policy = policy(ConfinedMode::WorkspaceWrite);
        policy.workspace_root = "/tmp/.".to_owned();
        let confined = provider.confine(&argv(), &policy).expect("wrapped");
        let joined = confined.argv.join(" ");
        assert!(
            joined.contains(&format!("(subpath \"{}\")", canonical_path("/tmp/."))),
            "the canonical root is granted, not the as-spelled one: {joined}"
        );
    }
}
