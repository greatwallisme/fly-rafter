// Rust guideline compliant 2026-10-29
//! The shell executors (dsh `shell` + `bash-local` + `bash-sandbox` +
//! `pwsh-local` + `pwsh-sandbox`).
//!
//! One execution model serves all four dsh executor variants: the shell
//! kind selects the argv and environment overrides, and the sandbox
//! wrapper layers confinement on top. Q16a-3A (pwsh registered on every
//! platform) plus the single dsh `ctx.shell` slot both resolve to a
//! per-kind executor table: the bash and pwsh executors mount side by
//! side, and the wire payloads carry the kind selector. The mounted
//! variants are the sandbox ones (every dsh bundle ships the confining
//! executors; the local variants are custom-deployment mounts) — the
//! bundle-level variant expression stays the batch-2 装配裁决.
//!
//! The foreground projection ([`ShellExecution::result`]) keeps the dsh
//! split: infrastructure failures reject while every other outcome
//! resolves, and a confined runner failure throws the fail-closed
//! sandbox-unavailable refusal. The deadline is a fused signal armed
//! with push-based timer threads: unlike the fr-timeout blocking face
//! (check()-based), the executor must kill the process while the caller
//! is parked in a watcher, so the timer aborts the fused signal itself.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use fr_sandbox::diagnostics::{
    classify_runner_failure, is_runner_spawn_failure, matches_signature,
};
use fr_sandbox::escalation::{escalation_hint_marker, sandbox_denial_marker};
use fr_sandbox::seam::{
    ConfinedArgv, SandboxEnforcement, SandboxExecutionPolicy, SandboxMode, SandboxPolicy,
};
use serde::{Deserialize, Serialize};

use crate::jobs_local::ms_u64;
use crate::jobs_types::{JobChannel, JobOutcome, JobOutputSource, JobSourceRead, JobStatus};
use crate::shell_types::AbortSignal;
use crate::subprocess::{
    CollectedOutput, OutputCollector, OutputRead, SpawnFailure, SpawnSpec, SpillCap, StdinMode,
    Stdio,
};

/// The two shell kinds (dsh's two executor families; Q16a-3A mounts both
/// on every platform).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShellKind {
    /// The POSIX shell (`bash -c`).
    Bash,
    /// PowerShell 7 (or the 5.1 last-resort on Windows).
    Pwsh,
}

impl ShellKind {
    /// Parses the wire word.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "bash" => Some(Self::Bash),
            "pwsh" => Some(Self::Pwsh),
            _ => None,
        }
    }

    /// The wire word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Pwsh => "pwsh",
        }
    }

    /// The executor validation prefix (dsh's two executor packages).
    #[must_use]
    pub const fn executor_label(self) -> &'static str {
        match self {
            Self::Bash => "bash-local",
            Self::Pwsh => "pwsh-local",
        }
    }

    /// The job kind the absorbed start registers (dsh `JobKindMap`).
    #[must_use]
    pub const fn job_kind(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Pwsh => "pwsh",
        }
    }

    /// The model-friendly environment overrides (dsh `ENV_OVERRIDES`:
    /// `TERM=dumb` is a POSIX concept and rides the bash arm only).
    #[must_use]
    pub const fn env_overrides(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Bash => &[
                ("NO_COLOR", "1"),
                ("TERM", "dumb"),
                ("PAGER", "cat"),
                ("GIT_PAGER", "cat"),
            ],
            Self::Pwsh => &[("NO_COLOR", "1"), ("PAGER", "cat"), ("GIT_PAGER", "cat")],
        }
    }
}

/// The expiry policy at the resolved timeout (dsh `ShellExpiryPolicy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShellExpiryPolicy {
    /// Kill the command and classify the result `timedOut`.
    Kill,
    /// Arm no deadline: the caller's signal and [`ShellExecution::kill`]
    /// are the only stop paths.
    None,
}

/// A caller's execution request (dsh `ShellExecRequest`): `workdir` and
/// `timeoutMs` are optional and filled by resolve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellExecRequest {
    /// The command source for the kind's shell form.
    pub command: String,
    /// Working directory override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    /// Timeout override in milliseconds (implementations cap it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<f64>,
    /// Deadline policy at the resolved timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_expiry: Option<ShellExpiryPolicy>,
    /// Foreground stdout capture budget in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_max_bytes: Option<f64>,
    /// Bytes to write to the command's stdin, then close.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    /// Ordinary environment entries, merged after the credential scrub.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    /// The managed FR_* snapshot; merges after `env` so an ordinary
    /// entry can never displace a managed fact (the dsh `dshEnv` slot
    /// under the Q16a-1A FR_ ruling).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fr_env: Option<HashMap<String, String>>,
    /// Fully resolved per-call sandbox policy; sandboxing executors
    /// default it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_policy: Option<SandboxExecutionPolicy>,
}

/// A resolved execution spec (dsh `ShellExecSpec`).
///
/// Resolve fills and caps the required fields. The in-process abort
/// signal never crosses the wire: job-backed execution binds the job
/// controller's signal at start, and the wire round-trip drops it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellExecSpec {
    /// The command source for the kind's shell form.
    pub command: String,
    /// Resolved working directory.
    pub workdir: String,
    /// Resolved timeout in milliseconds.
    pub timeout_ms: f64,
    /// Deadline policy at the resolved timeout (resolve defaults it to
    /// kill).
    pub on_expiry: ShellExpiryPolicy,
    /// Resolved stdout capture budget in bytes; stderr keeps the
    /// executor's own output cap.
    pub stdout_max_bytes: f64,
    /// The in-process abort signal (never serialized).
    #[serde(skip)]
    pub signal: Option<Arc<AbortSignal>>,
    /// Bytes to write to stdin before closing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    /// Ordinary environment entries carried through from the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    /// The managed FR_* snapshot; merges after `env`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fr_env: Option<HashMap<String, String>>,
    /// Resolved sandbox policy; ignored by executors that do not
    /// confine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_policy: Option<SandboxExecutionPolicy>,
}

/// Sandbox facts for one run, present when a sandboxing executor handled
/// it (dsh `ShellSandboxInfo`).
///
/// Reported independently of exit status so callers distinguish command
/// failures from policy denials and runner failures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellSandboxInfo {
    /// The mode the command actually ran under.
    pub mode: SandboxMode,
    /// Whether the sandbox denied a file operation.
    pub denied: bool,
    /// How completely the selected runner enforced the requested mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforcement: Option<SandboxEnforcement>,
    /// Whether the sandbox runner failed before the command could run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_failed: Option<bool>,
}

/// The outcome of a foreground run (dsh `ShellRunResult`), including the
/// timeout-during-preparation shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellRunResult {
    /// Exit code; `None` when preparation expired or the process died
    /// from a signal.
    pub exit_code: Option<i32>,
    /// Terminating signal name, or `None` when none was reported.
    pub signal: Option<String>,
    /// True when the executor's own timeout was the first cause to cut
    /// the command short (mutually exclusive with `aborted`).
    pub timed_out: bool,
    /// True when the caller's cancellation was the first cause.
    pub aborted: bool,
    /// The effective timeout applied to this run.
    pub timeout_ms: f64,
    /// Captured stdout (the tail when truncated).
    pub stdout: CollectedOutput,
    /// Captured stderr (the tail when truncated).
    pub stderr: CollectedOutput,
    /// Sandbox execution facts, absent for an unsandboxed run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<ShellSandboxInfo>,
}

/// The executor knobs (dsh `Config` shared by all four variants; the dsh
/// volatile live-reload has no fr runtime counterpart — configuration
/// lands once at activation).
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// Default working directory for commands (the dsh default:
    /// `process.cwd()`).
    pub cwd: Option<String>,
    /// Default timeout in milliseconds.
    pub timeout_ms: f64,
    /// Upper bound for per-call timeout overrides.
    pub max_timeout_ms: f64,
    /// Per-stream in-memory output cap; overflow spills to a temp file.
    pub max_output_bytes: f64,
    /// Per-stream spill-file cap; larger streams retain only their
    /// in-memory tail.
    pub max_spill_bytes: f64,
    /// Grace period for kill escalation and inherited pipes.
    pub grace_ms: f64,
    /// Explicit pwsh executable, trusted as-is; absent resolves through
    /// the platform probe.
    pub pwsh_path: Option<String>,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            cwd: None,
            timeout_ms: 120_000.0,
            max_timeout_ms: 600_000.0,
            max_output_bytes: 64_000.0,
            max_spill_bytes: 64.0 * 1024.0 * 1024.0,
            grace_ms: 3_000.0,
            pwsh_path: None,
        }
    }
}

/// One rejected configuration field (the dsh `assertPositiveFinite`
/// family).
fn assert_positive_finite(prefix: &str, name: &str, value: f64) -> Result<(), String> {
    if !value.is_finite() || value <= 0.0 {
        return Err(format!("{prefix}: {name} must be a positive finite number"));
    }
    Ok(())
}

/// Rejects a configuration this executor could not run with (the dsh
/// `assertServiceableBashConfig`/`assertServiceablePwshConfig` texts
/// verbatim).
///
/// The schema expresses neither the positivity constraints nor the timer
/// bound, so a stored value that cannot be used fails at the next
/// command.
///
/// # Errors
///
/// The `bash-local:`/`pwsh-local:` validation family.
pub fn assert_serviceable(kind: ShellKind, config: &ExecutorConfig) -> Result<(), String> {
    let prefix = kind.executor_label();
    for (name, value) in [
        ("timeoutMs", config.timeout_ms),
        ("maxTimeoutMs", config.max_timeout_ms),
        ("maxOutputBytes", config.max_output_bytes),
        ("maxSpillBytes", config.max_spill_bytes),
        ("graceMs", config.grace_ms),
    ] {
        assert_positive_finite(prefix, name, value)?;
    }
    if config.grace_ms > fr_timeout::MAX_TIMER_DELAY_MS {
        return Err(format!(
            "{prefix}: graceMs must be no greater than {}",
            crate::service::js_number(fr_timeout::MAX_TIMER_DELAY_MS)
        ));
    }
    Ok(())
}

/// The Windows environment probe inputs for pwsh resolution (dsh reads
/// `process.env` directly; the explicit struct keeps the probe a pure
/// function of its inputs on every platform).
#[derive(Debug, Default, Clone)]
pub struct WindowsEnvProbe {
    /// `ProgramFiles`.
    pub program_files: Option<String>,
    /// `SystemRoot`.
    pub system_root: Option<String>,
    /// `PATH`.
    pub path: Option<String>,
}

/// The candidate `pwsh` executables in resolution order (dsh
/// `candidatePwshPaths`).
///
/// The order is the PowerShell 7 install, then PATH entries such as the
/// Microsoft Store install — quotes stripped — then the Windows
/// PowerShell 5.1 last resort.
#[must_use]
pub fn candidate_pwsh_paths(probe: &WindowsEnvProbe) -> Vec<String> {
    let program_files = probe
        .program_files
        .clone()
        .unwrap_or_else(|| r"C:\Program Files".to_owned());
    let system_root = probe
        .system_root
        .clone()
        .unwrap_or_else(|| r"C:\Windows".to_owned());
    let mut candidates = vec![format!("{program_files}\\PowerShell\\7\\pwsh.exe")];
    for entry in probe.path.iter().flat_map(|path| path.split(';')) {
        let trimmed = entry.trim().trim_matches('"');
        if trimmed.is_empty() {
            continue;
        }
        candidates.push(format!("{trimmed}\\pwsh.exe"));
    }
    candidates.push(format!(
        "{system_root}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"
    ));
    candidates
}

/// Whether a candidate can be spawned (dsh `candidateExists`: lstat opens
/// the entry itself instead of following reparse points, so the Store
/// app execution alias is seen; a real directory never matches).
fn candidate_exists(candidate: &str) -> bool {
    std::fs::symlink_metadata(candidate)
        .is_ok_and(|metadata| metadata.is_file() || metadata.file_type().is_symlink())
}

/// Resolves the pwsh executable this executor spawns (dsh
/// `resolvePwshPath`: a configured path is trusted as-is; Windows probes
/// the well-known locations; every other platform resolves `pwsh`
/// through PATH).
#[must_use]
pub fn resolve_pwsh_path(
    configured: Option<&str>,
    probe: Option<&WindowsEnvProbe>,
    is_windows: bool,
) -> String {
    if let Some(configured) = configured.filter(|word| !word.is_empty()) {
        return configured.to_owned();
    }
    if is_windows && let Some(probe) = probe {
        for candidate in candidate_pwsh_paths(probe) {
            if candidate_exists(&candidate) {
                return candidate;
            }
        }
    }
    "pwsh".to_owned()
}

/// The live Windows probe over the process environment.
#[must_use]
fn live_windows_probe() -> WindowsEnvProbe {
    WindowsEnvProbe {
        program_files: std::env::var("ProgramFiles").ok(),
        system_root: std::env::var("SystemRoot").ok(),
        path: std::env::var("PATH").ok(),
    }
}

/// The executor's own timeout reason code (dsh arms its deadline with
/// the same code; only this reason classifies `timedOut`).
const TIMEOUT_CODE: &str = "BASH_TIMEOUT";

/// The lifecycle of one process (dsh `ShellProcessStatus`): settled
/// exactly once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStatus {
    /// The process is live or its settlement watcher has not run.
    Running,
    /// The command exited with a code.
    Completed,
    /// Any signal termination, the caller's cancellation, a timeout
    /// kill, or an infrastructure failure settles the handle here.
    Killed,
}

/// A fused cancellation signal plus its push-based timer threads (the
/// dsh `deadline(signal, timeoutMs, 'BASH_TIMEOUT')` fusion in thread
/// shape).
///
/// The timer and the upstream bridge abort the fused signal themselves,
/// so the kill lands while the caller parks in a watcher.
#[derive(Debug)]
pub struct ArmedDeadline {
    /// The disarm latch both timer threads poll.
    stop: Arc<std::sync::atomic::AtomicBool>,
    /// The timer threads, joined at disarm.
    timers: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl ArmedDeadline {
    /// Arms the deadline: one upstream bridge (the caller's signal into
    /// the fused signal) and one expiry timer, both parked in bounded
    /// slices and both joined at disarm.
    fn arm(upstream: Option<Arc<AbortSignal>>, timeout_ms: f64, fused: &Arc<AbortSignal>) -> Self {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut timers = Vec::new();
        if let Some(upstream) = upstream {
            let stop = Arc::clone(&stop);
            let fused = Arc::clone(fused);
            timers.push(
                std::thread::Builder::new()
                    .name("fr-shell-deadline-upstream".to_owned())
                    .spawn(move || {
                        while !stop.load(std::sync::atomic::Ordering::Acquire) {
                            if upstream.is_aborted() {
                                fused.abort(upstream.reason_text());
                                return;
                            }
                            std::thread::park_timeout(Duration::from_millis(15));
                        }
                    })
                    .expect(
                        "spawning the deadline bridge thread never fails under the plugin contract",
                    ),
            );
        }
        let timer_stop = Arc::clone(&stop);
        let timer_fused = Arc::clone(fused);
        // The dsh timer library caps a delay at the largest supported
        // timer delay; the same cap bounds the armed expiry here.
        let capped = ms_u64(timeout_ms.min(fr_timeout::MAX_TIMER_DELAY_MS));
        timers.push(
            std::thread::Builder::new()
                .name("fr-shell-deadline-timer".to_owned())
                .spawn(move || {
                    let expiry = Instant::now() + Duration::from_millis(capped);
                    while !timer_stop.load(std::sync::atomic::Ordering::Acquire) {
                        if Instant::now() >= expiry {
                            timer_fused.abort(TIMEOUT_CODE);
                            return;
                        }
                        std::thread::park_timeout(Duration::from_millis(15));
                    }
                })
                .expect("spawning the deadline timer thread never fails under the plugin contract"),
        );
        Self {
            stop,
            timers: Mutex::new(timers),
        }
    }

    /// Disarms the deadline: both threads wake and exit, joined here.
    fn disarm(&self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        let mut timers = self
            .timers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for timer in timers.drain(..) {
            timer.thread().unpark();
            let _ = timer.join();
        }
    }
}

/// The kill-on-abort watcher join handle and stop latch (settled at
/// settlement like the deadline).
#[derive(Debug)]
struct KillWatch {
    stop: Arc<std::sync::atomic::AtomicBool>,
    join: std::thread::JoinHandle<()>,
}

impl KillWatch {
    /// Stops and joins the watcher.
    fn release(self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        self.join.thread().unpark();
        let _ = self.join.join();
    }
}

/// The first-cause classification facts one execution keeps (the dsh
/// `classify` closure over the fused deadline).
#[derive(Clone, Debug)]
pub struct Classification {
    /// The expiry policy the resolve stamped.
    pub expiry: ShellExpiryPolicy,
    /// The fused (or caller) spawn signal.
    pub signal: Option<Arc<AbortControllerAlias>>,
    /// The effective timeout, echoed into the result.
    pub timeout_ms: f64,
}

/// The fused-signal alias: `AbortSignal` under this module's vocabulary.
type AbortControllerAlias = AbortSignal;

impl Classification {
    /// Whether the executor's own timeout was the first cause (only the
    /// kill policy arms the timer, so only that arm can time out).
    #[must_use]
    pub fn timed_out(&self) -> bool {
        matches!(self.expiry, ShellExpiryPolicy::Kill)
            && self
                .signal
                .as_ref()
                .is_some_and(|signal| signal.is_aborted() && signal.reason_text() == TIMEOUT_CODE)
    }

    /// Whether the caller's cancellation was the first cause.
    #[must_use]
    pub fn aborted(&self) -> bool {
        self.signal
            .as_ref()
            .is_some_and(|signal| signal.is_aborted())
            && !self.timed_out()
    }
}

/// The spawn-step facts one execution's watcher acts on.
enum WatchStep {
    /// The subprocess spawned; its exit settles the handle.
    Live(Arc<crate::subprocess::SubprocessHandle>),
    /// Preparation outlived the deadline: the executor's own timed-out
    /// shape (exit facts `null`/`null`).
    PreparedTimeout,
    /// The spawn refused: the settled-killed infrastructure-failure
    /// shape.
    Failed(SpawnFailure),
}

/// The typed provider rejection (dsh's rejection value): the OS spawn
/// error keeps its kind for runner attribution, everything else is text.
#[derive(Debug)]
enum ProviderError {
    /// The OS refused to start the program.
    Io(std::io::Error),
    /// A validation refusal or any other failure text.
    Text(String),
}

impl ProviderError {
    fn from_spawn(failure: SpawnFailure) -> Self {
        match failure {
            SpawnFailure::Io(error) => Self::Io(error),
            SpawnFailure::Invalid(text) => Self::Text(text),
        }
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Text(text) => f.write_str(text),
        }
    }
}

/// The per-process confinement facts a sandboxing executor installs
/// before the handle can settle (dsh's `processFacts` map entry; fr
/// stores them in the process itself — one process, one fact set).
#[derive(Debug, Clone)]
pub(crate) struct SandboxFacts {
    /// The mode this execution ran under.
    pub mode: SandboxMode,
    /// Enforcement completeness reported by the confine wrap.
    pub enforcement: SandboxEnforcement,
    /// The backend's denial dialect.
    pub denial_signatures: Vec<String>,
    /// Structured runner-failure evidence rules.
    pub runner_failure_rules: Vec<fr_sandbox::seam::RunnerFailureRule>,
    /// The confinement runner program (spawn-failure attribution).
    pub runner_program: Option<String>,
    /// The caller-owned workdir the spawn used.
    pub workdir: String,
}

/// The infrastructure failure a rejected spawn leaves on the handle
/// (dsh `providerFailure`).
struct ProviderFailure {
    detail: String,
    note: String,
}

/// The mutable per-process state under one mutex.
struct ExecutionState {
    status: ProcessStatus,
    exit_code: Option<i32>,
    signal: Option<String>,
    /// Whether the settlement watcher finished its whole chain.
    done: bool,
}

/// The shared process state (the dsh `ShellExecution` handle).
struct ExecutionInner {
    state: Mutex<ExecutionState>,
    /// Settled watchers wake here.
    settled: Condvar,
    /// Captured stdout.
    stdout: Arc<OutputCollector>,
    /// Captured stderr.
    stderr: Arc<OutputCollector>,
    /// The infrastructure failure, once the spawn rejected.
    provider: Mutex<Option<ProviderFailure>>,
    /// The live subprocess (absent for a preparation timeout or a
    /// refused spawn).
    handle: Mutex<Option<Arc<crate::subprocess::SubprocessHandle>>>,
    /// The armed deadline, disarmed at settlement.
    armed: Mutex<Option<ArmedDeadline>>,
    /// The kill-on-abort watcher: the fused (or caller) signal aborting
    /// terminates the managed range (the dsh provider's signal leg).
    kill_watch: Mutex<Option<KillWatch>>,
    /// The confinement facts awaiting settlement stamping.
    facts: Mutex<Option<SandboxFacts>>,
    /// The settled sandbox facts (dsh `proc.sandbox`).
    sandbox: Mutex<Option<ShellSandboxInfo>>,
    /// The foreground projection's decoration (the dsh danger-arm
    /// `decorateResult` replacement).
    decorated: Mutex<Option<ShellSandboxInfo>>,
    /// The classified runner-failure line (the foreground
    /// sandbox-unavailable refusal detail).
    runner_failure: Mutex<Option<String>>,
    /// The first-cause classification facts.
    classification: Classification,
}

impl std::fmt::Debug for ExecutionInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionInner").finish_non_exhaustive()
    }
}

/// One shell execution (dsh `ShellExecution`).
///
/// The live process plus its foreground projection: job-backed consumers
/// read the observed streams, await settlement, and project the outcome;
/// the batch-2 promote path reads the foreground result.
#[derive(Clone, Debug)]
pub struct ShellExecution {
    inner: Arc<ExecutionInner>,
}

impl ShellExecution {
    /// Builds one execution over its classification and collectors and
    /// starts the settlement watcher.
    fn new(
        classification: Classification,
        stdout: Arc<OutputCollector>,
        stderr: Arc<OutputCollector>,
        handle: Option<Arc<crate::subprocess::SubprocessHandle>>,
        armed: Option<ArmedDeadline>,
        facts_slot: Option<Arc<Mutex<Option<SandboxFacts>>>>,
        step: WatchStep,
    ) -> Arc<Self> {
        let facts = facts_slot.and_then(|slot| {
            slot.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
        });
        let kill_signal = classification.signal.clone();
        let execution = Arc::new(Self {
            inner: Arc::new(ExecutionInner {
                state: Mutex::new(ExecutionState {
                    status: ProcessStatus::Running,
                    exit_code: None,
                    signal: None,
                    done: false,
                }),
                settled: Condvar::new(),
                stdout,
                stderr,
                provider: Mutex::new(None),
                handle: Mutex::new(handle),
                armed: Mutex::new(armed),
                kill_watch: Mutex::new(None),
                facts: Mutex::new(facts),
                sandbox: Mutex::new(None),
                decorated: Mutex::new(None),
                runner_failure: Mutex::new(None),
                classification,
            }),
        });
        if let Some(signal) = kill_signal {
            let latch = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let watch_stop = Arc::clone(&latch);
            let watch_inner = Arc::clone(&execution.inner);
            let join = std::thread::Builder::new()
                .name("fr-shell-kill-watch".to_owned())
                .spawn(move || {
                    while !watch_stop.load(std::sync::atomic::Ordering::Acquire) {
                        if signal.is_aborted() {
                            if let Some(handle) = recover(&watch_inner.handle).as_ref() {
                                handle.terminate();
                            }
                            return;
                        }
                        std::thread::park_timeout(Duration::from_millis(15));
                    }
                })
                .expect("spawning the kill watcher never fails under the plugin contract");
            *recover(&execution.inner.kill_watch) = Some(KillWatch { stop: latch, join });
        }
        let watcher_inner = Arc::clone(&execution.inner);
        std::thread::Builder::new()
            .name("fr-shell-execution-watch".to_owned())
            .spawn(move || watch(&watcher_inner, step))
            .expect("spawning the execution watcher never fails under the plugin contract");
        execution
    }

    /// The process lifecycle state.
    #[must_use]
    pub fn status(&self) -> ProcessStatus {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .status
    }

    /// The exit code once settled (absent while running or signal-killed).
    #[must_use]
    pub fn exit_code(&self) -> Option<i32> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .exit_code
    }

    /// The terminating signal name once settled.
    #[must_use]
    pub fn signal(&self) -> Option<String> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .signal
            .clone()
    }

    /// The settled sandbox facts (stamped at settlement only).
    #[must_use]
    pub fn sandbox(&self) -> Option<ShellSandboxInfo> {
        self.inner
            .sandbox
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Whether the settlement watcher finished (the dsh `done` promise
    /// fulfilled).
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .done
    }

    /// Blocks until the settlement watcher finished (the dsh `await
    /// process.done`).
    pub fn done(&self) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !state.done {
            state = self
                .inner
                .settled
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        drop(state);
    }

    /// Terminates the provider-managed range; `false` when the process
    /// had already finished (idempotent, the dsh `kill()`).
    pub fn kill(&self) -> bool {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.status != ProcessStatus::Running {
            return false;
        }
        state.status = ProcessStatus::Killed;
        drop(state);
        if let Some(handle) = self
            .inner
            .handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            handle.terminate();
        }
        true
    }

    /// Reads captured stdout from a byte offset without consuming (the
    /// dsh `observed.stdout` reader).
    #[must_use]
    pub fn observed_stdout(&self, from: u64) -> OutputRead {
        self.inner.stdout.read_from(from)
    }

    /// Reads captured stderr from a byte offset without consuming; a
    /// rejected spawn serves its stage-neutral note as the whole stream
    /// (the dsh `observedStderr` wrap).
    #[must_use]
    pub fn observed_stderr(&self, from: u64) -> OutputRead {
        let provider = self
            .inner
            .provider
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(failure) = provider.as_ref() {
            let bytes = failure.note.as_bytes();
            let start = from.min(bytes.len() as u64);
            let start = usize::try_from(start).unwrap_or(usize::MAX);
            return OutputRead {
                text: String::from_utf8_lossy(&bytes[start..]).into_owned(),
                next_offset: bytes.len() as u64,
                lossy: false,
                spill_path: None,
            };
        }
        drop(provider);
        self.inner.stderr.read_from(from)
    }

    /// Installs the foreground projection's sandbox decoration (the dsh
    /// danger-arm `decorateResult`).
    pub(crate) fn set_result_sandbox(&self, info: ShellSandboxInfo) {
        *self
            .inner
            .decorated
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(info);
    }

    /// The foreground projection (dsh `result()`): infrastructure
    /// failures reject while every other outcome resolves, and a
    /// confined runner failure throws the fail-closed refusal.
    ///
    /// # Errors
    ///
    /// The provider failure detail, or the sandbox-unavailable refusal
    /// when the confinement runner failed.
    pub fn result(&self) -> Result<ShellRunResult, String> {
        self.done();
        if let Some(provider) = self
            .inner
            .provider
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            return Err(provider.detail.clone());
        }
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stdout = final_output(&self.inner.stdout);
        let stderr = final_output(&self.inner.stderr);
        // The confined classification: runner failure outranks denial
        // because the command did not run (the dsh decorateResult map
        // and mapError arms).
        if let Some(facts) = self
            .inner
            .facts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            let runner_line =
                classify_runner_failure(state.exit_code, &stderr.text, &facts.runner_failure_rules);
            if let Some(line) = runner_line {
                return Err(fr_sandbox::seam::sandbox_unavailable_text(
                    confined_mode_of(facts.mode),
                    Some(line.as_str()),
                ));
            }
            let denied = matches_signature(state.exit_code, &stderr.text, &facts.denial_signatures);
            return Ok(ShellRunResult {
                exit_code: state.exit_code,
                signal: state.signal.clone(),
                timed_out: self.inner.classification.timed_out(),
                aborted: self.inner.classification.aborted(),
                timeout_ms: self.inner.classification.timeout_ms,
                stdout,
                stderr,
                sandbox: Some(ShellSandboxInfo {
                    mode: facts.mode,
                    denied,
                    enforcement: Some(facts.enforcement),
                    runner_failed: None,
                }),
            });
        }
        let decorated = self
            .inner
            .decorated
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let sandbox = decorated.or_else(|| self.sandbox());
        Ok(ShellRunResult {
            exit_code: state.exit_code,
            signal: state.signal.clone(),
            timed_out: self.inner.classification.timed_out(),
            aborted: self.inner.classification.aborted(),
            timeout_ms: self.inner.classification.timeout_ms,
            stdout,
            stderr,
            sandbox,
        })
    }

    /// Projects the settled process onto the job-outcome vocabulary
    /// (dsh `processOutcome`): a nonzero command exit is reported, not
    /// failed, exactly like the foreground rendering; sandbox facts join
    /// the terminal detail.
    #[must_use]
    pub fn process_outcome(&self, escalation_modes: &[SandboxMode]) -> JobOutcome {
        self.done();
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let base = if state.status == ProcessStatus::Killed {
            let detail = state.signal.clone().map_or_else(
                || "killed before exit".to_owned(),
                |signal| format!("signal: {signal}"),
            );
            JobOutcome {
                status: JobStatus::Killed,
                detail: Some(detail),
                result: None,
            }
        } else {
            JobOutcome {
                status: JobStatus::Completed,
                detail: Some(format!("exit code: {}", state.exit_code.unwrap_or(0))),
                result: None,
            }
        };
        drop(state);
        let notes = sandbox_notes(self.sandbox().as_ref(), escalation_modes);
        if notes.is_empty() {
            return base;
        }
        JobOutcome {
            detail: Some(format!(
                "{}; {}",
                base.detail.unwrap_or_default(),
                notes.join(" ")
            )),
            ..base
        }
    }
}

/// The settlement watcher (the dsh `done` callbacks in thread shape).
fn watch(inner: &Arc<ExecutionInner>, step: WatchStep) {
    match step {
        WatchStep::Live(handle) => match handle.done() {
            Ok(outcome) => fulfill(inner, &outcome),
            Err(text) => rejected(inner, true, &ProviderError::Text(text)),
        },
        WatchStep::PreparedTimeout => fulfill(
            inner,
            &crate::subprocess::Outcome {
                exit_code: None,
                signal: None,
            },
        ),
        WatchStep::Failed(failure) => rejected(inner, false, &ProviderError::from_spawn(failure)),
    }
}

/// The fulfilled arm: stamps exit facts, classifies the sandbox stamp,
/// disarms, and releases the done waiters.
fn fulfill(inner: &Arc<ExecutionInner>, outcome: &crate::subprocess::Outcome) {
    {
        let mut state = lock_state(inner);
        if state.status == ProcessStatus::Running {
            state.status = if inner
                .classification
                .signal
                .as_ref()
                .is_some_and(|signal| signal.is_aborted())
                || outcome.signal.is_some()
            {
                ProcessStatus::Killed
            } else {
                ProcessStatus::Completed
            };
        }
        state.exit_code = outcome.exit_code;
        state.signal.clone_from(&outcome.signal);
    }
    stamp_sandbox(inner, false, None);
    disarm(inner);
    release(inner);
}

/// The rejected arm: a live handle whose rejection follows this
/// execution's own termination reports its terminal outcome; anything
/// else is the settled-killed infrastructure failure whose note serves
/// every stderr reader.
fn rejected(inner: &Arc<ExecutionInner>, was_live: bool, error: &ProviderError) {
    let own_termination = was_live && {
        let state = lock_state(inner);
        state.status == ProcessStatus::Killed
            || inner
                .classification
                .signal
                .as_ref()
                .is_some_and(|signal| signal.is_aborted())
    };
    if own_termination {
        lock_state(inner).status = ProcessStatus::Killed;
        stamp_sandbox(inner, false, None);
        disarm(inner);
        release(inner);
        return;
    }
    let detail = error.to_string();
    *lock_provider(inner) = Some(ProviderFailure {
        note: format!("subprocess failed before reporting an outcome: {detail}"),
        detail,
    });
    lock_state(inner).status = ProcessStatus::Killed;
    stamp_sandbox(inner, true, Some(error));
    disarm(inner);
    release(inner);
}

/// Stamps the settled sandbox facts before the done waiters release
/// (dsh `onProcessDone`): a provider rejection attributes the runner
/// only when the error independently names the runner program;
/// otherwise settled runner failure outranks denial-like diagnostics.
fn stamp_sandbox(
    inner: &Arc<ExecutionInner>,
    provider_rejected: bool,
    provider_error: Option<&ProviderError>,
) {
    let Some(facts) = lock_facts(inner).take() else {
        return;
    };
    let exit_code = lock_state(inner).exit_code;
    let stderr_text = if provider_rejected {
        lock_provider(inner)
            .as_ref()
            .map(|failure| failure.note.clone())
            .unwrap_or_default()
    } else {
        inner.stderr.read_from(0).text
    };
    let runner_failed = if provider_rejected {
        matches!(
            provider_error,
            Some(ProviderError::Io(error))
                if is_runner_spawn_failure(
                    error,
                    facts.runner_program.as_deref(),
                    std::path::Path::new(&facts.workdir),
                )
        )
    } else {
        let line = classify_runner_failure(exit_code, &stderr_text, &facts.runner_failure_rules);
        if let Some(line) = line {
            *lock_runner_failure(inner) = Some(line);
        }
        lock_runner_failure(inner).is_some()
    };
    let denied =
        !runner_failed && matches_signature(exit_code, &stderr_text, &facts.denial_signatures);
    *lock_sandbox(inner) = Some(ShellSandboxInfo {
        mode: facts.mode,
        denied,
        enforcement: Some(facts.enforcement),
        runner_failed: runner_failed.then_some(true),
    });
}

/// Disarms the deadline and releases the kill watcher at settlement
/// (the dsh `Symbol.dispose`).
fn disarm(inner: &Arc<ExecutionInner>) {
    let armed = lock_armed(inner).take();
    if let Some(armed) = armed {
        armed.disarm();
    }
    let watch = recover(&inner.kill_watch).take();
    if let Some(watch) = watch {
        watch.release();
    }
}

/// Releases the done waiters.
fn release(inner: &Arc<ExecutionInner>) {
    {
        let mut state = lock_state(inner);
        state.done = true;
    }
    inner.settled.notify_all();
}

/// Sandbox facts worth the terminal detail (dsh `sandboxNotes`): a
/// runner that never ran the command, or a denial (with the escalation
/// hint this composition offers).
#[must_use]
fn sandbox_notes(
    sandbox: Option<&ShellSandboxInfo>,
    escalation_modes: &[SandboxMode],
) -> Vec<String> {
    let Some(sandbox) = sandbox else {
        return Vec::new();
    };
    if sandbox.runner_failed == Some(true) {
        return vec![format!(
            "[sandbox: the sandbox runner itself failed under {} mode — the command did not run; this is a sandbox problem, not a command failure]",
            sandbox.mode.as_str()
        )];
    }
    if sandbox.denied {
        let mut notes = vec![sandbox_denial_marker(sandbox.mode)];
        if !escalation_modes.is_empty() {
            notes.push(escalation_hint_marker("command"));
        }
        return notes;
    }
    Vec::new()
}

/// Projects a settled collect-mode reader into the final output shape
/// (dsh `finalOutput`).
fn final_output(reader: &OutputCollector) -> CollectedOutput {
    let read = reader.read_from(0);
    CollectedOutput {
        text: read.text,
        truncated: read.lossy,
        spill_path: read.spill_path,
    }
}

/// The confined counterpart of an execution mode (the `confine_from`
/// narrowing without the danger arm).
#[must_use]
const fn confined_mode_of(mode: SandboxMode) -> fr_sandbox::seam::ConfinedMode {
    match mode {
        SandboxMode::ReadOnly | SandboxMode::DangerFullAccess => {
            fr_sandbox::seam::ConfinedMode::ReadOnly
        }
        SandboxMode::WorkspaceWrite => fr_sandbox::seam::ConfinedMode::WorkspaceWrite,
    }
}

/// Locks one execution mutex, recovering from poisoning (the fr-tools
/// `recover` judgment).
fn recover<T>(lock: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    lock.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn lock_state(inner: &Arc<ExecutionInner>) -> std::sync::MutexGuard<'_, ExecutionState> {
    recover(&inner.state)
}

fn lock_provider(
    inner: &Arc<ExecutionInner>,
) -> std::sync::MutexGuard<'_, Option<ProviderFailure>> {
    recover(&inner.provider)
}

fn lock_armed(inner: &Arc<ExecutionInner>) -> std::sync::MutexGuard<'_, Option<ArmedDeadline>> {
    recover(&inner.armed)
}

fn lock_facts(inner: &Arc<ExecutionInner>) -> std::sync::MutexGuard<'_, Option<SandboxFacts>> {
    recover(&inner.facts)
}

fn lock_sandbox(
    inner: &Arc<ExecutionInner>,
) -> std::sync::MutexGuard<'_, Option<ShellSandboxInfo>> {
    recover(&inner.sandbox)
}

fn lock_runner_failure(inner: &Arc<ExecutionInner>) -> std::sync::MutexGuard<'_, Option<String>> {
    recover(&inner.runner_failure)
}

/// The one-slot execution face the wire service dispatches through (dsh
/// `ShellExecutor`).
pub trait ShellExecutorFace: Send + Sync {
    /// The configured default sandbox mode (dsh `sandboxMode`): `None`
    /// for an executor that never confines.
    fn sandbox_mode(&self) -> Option<SandboxMode>;
    /// Applies implementation defaults and caps to one request.
    ///
    /// # Errors
    ///
    /// The config validation family, or the timeout clamp refusal.
    fn resolve(&self, request: ShellExecRequest) -> Result<ShellExecSpec, String>;
    /// Executes one resolved spec.
    ///
    /// # Errors
    ///
    /// A prepare failure that is not the deadline's own timeout.
    fn execute(&self, spec: &ShellExecSpec) -> Result<Arc<ShellExecution>, String>;
}

/// The preparation closure one sandboxed execution runs before the spawn
/// (the dsh async-prepare arm).
type PrepareClosure = Box<dyn FnOnce(&AbortSignal) -> Result<Vec<String>, String> + Send>;

/// The exact argv (or the confinement preparation) one execution runs.
pub(crate) enum ArgvOrPrepare {
    /// The exact argv.
    Static(Vec<String>),
    /// Preparation using the execution cancellation signal (the dsh
    /// async-prepare arm; the confine call rides here).
    Prepare(PrepareClosure),
}

/// The local shell executor (dsh `LocalBashExecutor`/`PwshLocalExecutor`
/// in one kind-parameterized shape).
///
/// Bounded output, spill files, managed-range termination, and
/// quiescence are the subprocess module's mechanics; this executor
/// supplies their configured budgets per spawn.
pub struct LocalExecutor {
    kind: ShellKind,
    config: ExecutorConfig,
    /// The resolved pwsh executable (probed once at construction; the
    /// dsh volatile re-probe tracks live config changes fr does not
    /// have).
    pwsh_path: String,
}

impl std::fmt::Debug for LocalExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalExecutor")
            .field("kind", &self.kind)
            .field("config", &self.config)
            .field("pwsh_path", &self.pwsh_path)
            .finish()
    }
}

impl LocalExecutor {
    /// Creates one executor over the fixed configuration; the pwsh path
    /// resolves immediately (dsh resolves at construction too).
    #[must_use]
    pub fn new(kind: ShellKind, config: ExecutorConfig) -> Self {
        let pwsh_path = resolve_pwsh_path(
            config.pwsh_path.as_deref(),
            Some(&live_windows_probe()),
            cfg!(windows),
        );
        Self {
            kind,
            config,
            pwsh_path,
        }
    }

    /// The kind this executor serves.
    #[must_use]
    pub const fn kind(&self) -> ShellKind {
        self.kind
    }

    /// The pwsh invocation argv (dsh `argv(spec)`): the command rides as
    /// ONE argv element — PowerShell itself parses the text, so there is
    /// no shell-quoting layer.
    #[must_use]
    pub fn command_argv(&self, spec: &ShellExecSpec) -> Vec<String> {
        match self.kind {
            ShellKind::Bash => vec!["bash".to_owned(), "-c".to_owned(), spec.command.clone()],
            ShellKind::Pwsh => vec![
                self.pwsh_path.clone(),
                "-NoLogo".to_owned(),
                "-NoProfile".to_owned(),
                "-NonInteractive".to_owned(),
                "-Command".to_owned(),
                format!("{}{}", ENCODING_PREAMBLE, spec.command),
            ],
        }
    }

    /// Applies implementation defaults and caps to one request (dsh
    /// `resolve`).
    ///
    /// # Errors
    ///
    /// The config validation family, the timeout clamp refusal, or the
    /// stdout-cap validation.
    pub fn resolve(&self, request: ShellExecRequest) -> Result<ShellExecSpec, String> {
        assert_serviceable(self.kind, &self.config)?;
        let timeout_ms = fr_timeout::clamp_timeout(
            request.timeout_ms,
            self.config.timeout_ms,
            self.config.max_timeout_ms,
            &format!("{}: request.timeoutMs", self.kind.executor_label()),
        )
        .map_err(|error| error.message().to_owned())?;
        let stdout_max_bytes = request
            .stdout_max_bytes
            .unwrap_or(self.config.max_output_bytes);
        assert_positive_finite(
            self.kind.executor_label(),
            "request.stdoutMaxBytes",
            stdout_max_bytes,
        )?;
        Ok(ShellExecSpec {
            command: request.command,
            workdir: request
                .workdir
                .or_else(|| self.config.cwd.clone())
                .unwrap_or_else(current_dir),
            timeout_ms,
            on_expiry: request.on_expiry.unwrap_or(ShellExpiryPolicy::Kill),
            stdout_max_bytes,
            signal: None,
            stdin: request.stdin,
            env: request.env,
            fr_env: request.fr_env,
            sandbox_policy: request.sandbox_policy,
        })
    }

    /// Executes the public command (dsh `execute`).
    ///
    /// # Errors
    ///
    /// A prepare failure that is not the deadline's own timeout.
    pub fn execute(&self, spec: &ShellExecSpec) -> Result<Arc<ShellExecution>, String> {
        let argv = self.command_argv(spec);
        self.execute_argv(spec, ArgvOrPrepare::Static(argv), None)
    }

    /// Executes an explicit argv with the lifecycle, environment,
    /// output, deadline, and cancellation semantics of this executor
    /// (dsh `executeArgv`); the confining wrapper replaces the public
    /// command's shell argv at the execution boundary.
    ///
    /// # Errors
    ///
    /// A prepare failure that is not the deadline's own timeout
    /// (deadline-armed executions settle as the timed-out shape
    /// instead).
    pub(crate) fn execute_argv(
        &self,
        spec: &ShellExecSpec,
        argv_or_prepare: ArgvOrPrepare,
        facts_slot: Option<Arc<Mutex<Option<SandboxFacts>>>>,
    ) -> Result<Arc<ShellExecution>, String> {
        // Deadline wiring by expiry policy: one fused deadline combines
        // timeout and upstream cancellation; only this executor's
        // timeout reason counts as timedOut, outer deadlines as aborts.
        let (spawn_signal, armed) = match spec.on_expiry {
            ShellExpiryPolicy::Kill => {
                let fused = AbortSignal::new();
                let armed = ArmedDeadline::arm(spec.signal.clone(), spec.timeout_ms, &fused);
                (Some(fused), Some(armed))
            }
            ShellExpiryPolicy::None => (spec.signal.clone(), None),
        };
        let own_timeout = |signal: &AbortSignal| {
            matches!(spec.on_expiry, ShellExpiryPolicy::Kill)
                && signal.is_aborted()
                && signal.reason_text() == TIMEOUT_CODE
        };
        let (argv, preparation_timed_out) = match argv_or_prepare {
            ArgvOrPrepare::Static(argv) => (argv, false),
            ArgvOrPrepare::Prepare(prepare) => {
                let signal = spawn_signal.clone().unwrap_or_else(AbortSignal::new);
                match prepare(&signal) {
                    Ok(argv) => {
                        if signal.is_aborted() && !own_timeout(&signal) {
                            disarm_if_any(armed.as_ref());
                            return Err(signal.reason_text());
                        }
                        let timed_out = signal.is_aborted() && own_timeout(&signal);
                        (argv, timed_out)
                    }
                    Err(error) => {
                        if signal.is_aborted() && own_timeout(&signal) {
                            (Vec::new(), true)
                        } else {
                            disarm_if_any(armed.as_ref());
                            return Err(error);
                        }
                    }
                }
            }
        };
        // A synchronous spawn throw is contained into the same
        // settled-killed shape as an asynchronous spawn rejection, so
        // execute itself never fails for a spawn problem.
        let step = if preparation_timed_out {
            WatchStep::PreparedTimeout
        } else {
            let abort_reason = spawn_signal
                .as_ref()
                .filter(|signal| signal.is_aborted())
                .map(|signal| signal.reason_text());
            match crate::subprocess::spawn_detailed(&self.spawn_spec(spec, argv), abort_reason) {
                Ok(handle) => WatchStep::Live(Arc::new(handle)),
                Err(failure) => WatchStep::Failed(failure),
            }
        };
        let shared_handle = match &step {
            WatchStep::Live(handle) => Some(Arc::clone(handle)),
            _ => None,
        };
        let (stdout, stderr) = match &step {
            WatchStep::Live(handle) => (
                handle
                    .stdout_collector()
                    .cloned()
                    .unwrap_or_else(empty_collector),
                handle
                    .stderr_collector()
                    .cloned()
                    .unwrap_or_else(empty_collector),
            ),
            _ => (empty_collector(), empty_collector()),
        };
        Ok(ShellExecution::new(
            Classification {
                expiry: spec.on_expiry,
                signal: spawn_signal,
                timeout_ms: spec.timeout_ms,
            },
            stdout,
            stderr,
            shared_handle,
            armed,
            facts_slot,
            step,
        ))
    }

    /// Maps one resolved spec plus its argv onto a fully-specified
    /// subprocess spawn (dsh `spawnSpec`): the trusted managed snapshot
    /// merges last so it beats both the caller env and the terminal
    /// overrides.
    #[must_use]
    fn spawn_spec(&self, spec: &ShellExecSpec, argv: Vec<String>) -> SpawnSpec {
        let mut env: HashMap<String, Option<String>> = HashMap::new();
        for (key, value) in self.kind.env_overrides() {
            env.insert((*key).to_owned(), Some((*value).to_owned()));
        }
        if let Some(entries) = &spec.env {
            for (key, value) in entries {
                env.insert(key.clone(), Some(value.clone()));
            }
        }
        if let Some(entries) = &spec.fr_env {
            for (key, value) in entries {
                env.insert(key.clone(), Some(value.clone()));
            }
        }
        SpawnSpec {
            argv,
            cwd: spec.workdir.clone(),
            stdio: Stdio {
                stdin: spec
                    .stdin
                    .clone()
                    .map_or(StdinMode::Ignore, StdinMode::Data),
                stdout: crate::subprocess::OutputMode::Collect(
                    crate::subprocess::SubprocessCollect {
                        max_bytes: bytes_u64(spec.stdout_max_bytes),
                        spill: Some(SpillCap {
                            max_bytes: bytes_u64(self.config.max_spill_bytes),
                        }),
                    },
                ),
                stderr: crate::subprocess::OutputMode::Collect(
                    crate::subprocess::SubprocessCollect {
                        max_bytes: bytes_u64(self.config.max_output_bytes),
                        spill: Some(SpillCap {
                            max_bytes: bytes_u64(self.config.max_spill_bytes),
                        }),
                    },
                ),
            },
            grace_ms: self.config.grace_ms,
            env: Some(env),
        }
    }
}

/// The UTF-8 output pinning prepended to every pwsh command (dsh
/// `ENCODING_PREAMBLE`; the statements ride line 1 after `; ` separators
/// so PowerShell error line numbers stay accurate).
pub const ENCODING_PREAMBLE: &str = "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); $OutputEncoding = [System.Text.UTF8Encoding]::new($false); ";

/// The validated positive-finite byte cap cast (attributes cannot ride
/// expressions; the helper carries them).
#[expect(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "the caller validated the cap as a positive finite number; byte caps quantize to whole bytes"
)]
#[expect(clippy::missing_const_for_fn, reason = "the cast is not const-stable")]
#[must_use]
pub fn bytes_u64(value: f64) -> u64 {
    value as u64
}

/// The default working directory (dsh `process.cwd()`).
fn current_dir() -> String {
    std::env::current_dir().map_or_else(
        |_| ".".to_owned(),
        |path| path.to_string_lossy().into_owned(),
    )
}

/// An empty collector stands in for a collect disposition the provider
/// dropped (unreachable by the seam contract; the dsh defensive arm).
fn empty_collector() -> Arc<OutputCollector> {
    Arc::new(OutputCollector::new(1, "stdout", None))
}

/// Disarms an armed deadline during an early prepare failure (the dsh
/// disarm-before-throw).
fn disarm_if_any(armed: Option<&ArmedDeadline>) {
    if let Some(armed) = armed {
        armed.disarm();
    }
}

/// The confinement seam over the `'sandbox'` provider (dsh
/// `ctx.sandbox.confine`).
pub trait SandboxConfine: Send + Sync {
    /// Wraps one argv confined under the per-call policy, or fails
    /// closed.
    ///
    /// # Errors
    ///
    /// The provider's fail-closed refusal (the sandbox-unavailable text)
    /// or the transport failure.
    fn confine(&self, argv: &[String], policy: &SandboxPolicy) -> Result<ConfinedArgv, String>;
}

/// The default-policy seam over `'sandbox-policy'` (dsh
/// `ctx.sandboxPolicy`).
pub trait PolicyResolve: Send + Sync {
    /// Resolves the deployment's complete standing policy (dsh
    /// `resolve()`).
    ///
    /// # Errors
    ///
    /// The transport failure.
    fn resolve_policy(&self) -> Result<SandboxExecutionPolicy, String>;
    /// Reads the deployment's default mode (dsh `defaultMode`).
    ///
    /// # Errors
    ///
    /// The transport failure.
    fn default_mode(&self) -> Result<SandboxMode, String>;
}

/// The sandbox-consuming executor (dsh `SandboxBashExecutor`/
/// `SandboxPwshExecutor` in one shape).
///
/// It wraps the exact local argv through the confinement seam, inherits
/// the local process mechanics, and reports the selected mode,
/// enforcement, and denial facts. Positive runner-executable evidence
/// identifies a broken confinement runner: the foreground projection
/// throws the fail-closed refusal, while background processes carry
/// `runnerFailed`.
pub struct SandboxExecutor {
    local: LocalExecutor,
    confine: Arc<dyn SandboxConfine>,
    policy: Arc<dyn PolicyResolve>,
    /// The capability fact the schema advertisement reads (dsh reads it
    /// at construction; actual executions carry their resolved per-call
    /// policy).
    mode: SandboxMode,
}

impl std::fmt::Debug for SandboxExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxExecutor")
            .field("kind", &self.local.kind)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl SandboxExecutor {
    /// Creates one sandbox executor; the default mode reads once (dsh
    /// reads `ctx.sandboxPolicy.defaultMode` at construction; a failing
    /// read refuses the mount).
    ///
    /// # Errors
    ///
    /// The default-mode read failure.
    pub fn new(
        local: LocalExecutor,
        confine: Arc<dyn SandboxConfine>,
        policy: Arc<dyn PolicyResolve>,
    ) -> Result<Self, String> {
        let mode = policy.default_mode()?;
        Ok(Self {
            local,
            confine,
            policy,
            mode,
        })
    }

    /// The kind this executor serves.
    #[must_use]
    pub const fn kind(&self) -> ShellKind {
        self.local.kind
    }

    /// Stamps a complete per-call policy onto the spec: tool calls
    /// supply the calling session's resolved mode and root; lower-level
    /// callers fall back to the deployment policy (dsh `resolve`).
    ///
    /// # Errors
    ///
    /// The local resolve family, or the default-policy read.
    pub fn resolve(&self, request: ShellExecRequest) -> Result<ShellExecSpec, String> {
        let mut spec = self.local.resolve(request.clone())?;
        spec.sandbox_policy = Some(match request.sandbox_policy {
            Some(policy) => policy,
            None => self.policy.resolve_policy()?,
        });
        Ok(spec)
    }

    /// Executes one resolved spec (dsh `execute`): the danger arm runs
    /// direct and decorates, the confined arm wraps the exact argv
    /// through the seam and installs the per-process facts.
    ///
    /// # Errors
    ///
    /// A confine refusal that is not the deadline's own timeout (the
    /// job-backed path settles it as the job's failure detail).
    pub fn execute(&self, spec: &ShellExecSpec) -> Result<Arc<ShellExecution>, String> {
        let Some(policy) = spec.sandbox_policy.clone() else {
            return Err(format!(
                "{}: the resolved spec carries no sandbox policy",
                self.local.kind.executor_label()
            ));
        };
        if policy.mode == SandboxMode::DangerFullAccess {
            let execution = self.local.execute(spec)?;
            execution.set_result_sandbox(ShellSandboxInfo {
                mode: policy.mode,
                denied: false,
                enforcement: None,
                runner_failed: None,
            });
            return Ok(execution);
        }
        let Some(confined_policy) = SandboxPolicy::confine_from(&policy) else {
            return Err(format!(
                "{}: the resolved policy cannot narrow to a confined form",
                self.local.kind.executor_label()
            ));
        };
        let argv_seed = self.local.command_argv(spec);
        let workdir = spec.workdir.clone();
        let mode = policy.mode;
        let seam = Arc::clone(&self.confine);
        let facts_slot: Arc<Mutex<Option<SandboxFacts>>> = Arc::new(Mutex::new(None));
        let facts_writer = Arc::clone(&facts_slot);
        let prepare = Box::new(move |signal: &AbortSignal| {
            let prepared = seam.confine(&argv_seed, &confined_policy)?;
            if signal.is_aborted() {
                return Err(signal.reason_text());
            }
            *recover(&facts_writer) = Some(SandboxFacts {
                mode,
                enforcement: prepared.enforcement,
                denial_signatures: prepared.denial_signatures.clone(),
                runner_failure_rules: prepared.runner_failure_rules.clone(),
                runner_program: prepared.argv.first().cloned(),
                workdir,
            });
            Ok(prepared.argv)
        });
        self.local
            .execute_argv(spec, ArgvOrPrepare::Prepare(prepare), Some(facts_slot))
    }
}

impl ShellExecutorFace for LocalExecutor {
    fn sandbox_mode(&self) -> Option<SandboxMode> {
        None
    }

    fn resolve(&self, request: ShellExecRequest) -> Result<ShellExecSpec, String> {
        Self::resolve(self, request)
    }

    fn execute(&self, spec: &ShellExecSpec) -> Result<Arc<ShellExecution>, String> {
        Self::execute(self, spec)
    }
}

impl ShellExecutorFace for SandboxExecutor {
    fn sandbox_mode(&self) -> Option<SandboxMode> {
        Some(self.mode)
    }

    fn resolve(&self, request: ShellExecRequest) -> Result<ShellExecSpec, String> {
        Self::resolve(self, request)
    }

    fn execute(&self, spec: &ShellExecSpec) -> Result<Arc<ShellExecution>, String> {
        Self::execute(self, spec)
    }
}

/// The shared slot one absorbed job's watcher publishes its process
/// into (the dsh tool closure's `proc` variable).
pub type ProcSlot = Arc<Mutex<Option<Arc<ShellExecution>>>>;

/// The process's non-consuming stream readers as registry pull sources
/// (dsh `processSources`).
///
/// They bind lazily because the process spawns inside the starter, after
/// the registry admitted the job; a read before the spawn yields
/// nothing, and a rejected spawn's stderr reader carries the provider's
/// note.
#[must_use]
pub fn process_sources(slot: &ProcSlot) -> Vec<(Option<JobChannel>, JobOutputSource)> {
    vec![
        (
            Some(JobChannel::Stdout),
            stream_source(Arc::clone(slot), false),
        ),
        (
            Some(JobChannel::Stderr),
            stream_source(Arc::clone(slot), true),
        ),
    ]
}

/// Binds one observed reader behind the lazy slot.
fn stream_source(slot: ProcSlot, stderr: bool) -> JobOutputSource {
    Box::new(move |from: u64| {
        let process = {
            let guard = recover(&slot);
            guard.as_ref().map(Arc::clone)
        };
        let Some(process) = process.as_deref() else {
            return JobSourceRead {
                text: String::new(),
                next_offset: from,
                lossy: false,
                spill_path: None,
            };
        };
        let read = if stderr {
            process.observed_stderr(from)
        } else {
            process.observed_stdout(from)
        };
        JobSourceRead {
            text: read.text,
            next_offset: read.next_offset,
            lossy: read.lossy,
            spill_path: read.spill_path,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bash_executor() -> LocalExecutor {
        LocalExecutor::new(ShellKind::Bash, ExecutorConfig::default())
    }

    fn resolve_bash(spec: serde_json::Value) -> ShellExecSpec {
        let request: ShellExecRequest = serde_json::from_value(spec).expect("request");
        bash_executor().resolve(request).expect("resolve")
    }

    /// The five config fields and the timer bound validate under the
    /// kind's own prefix, at the next command.
    #[test]
    fn config_validation_texts() {
        for kind in [ShellKind::Bash, ShellKind::Pwsh] {
            let prefix = kind.executor_label();
            let config = ExecutorConfig {
                timeout_ms: 0.0,
                ..ExecutorConfig::default()
            };
            assert_eq!(
                assert_serviceable(kind, &config).unwrap_err(),
                format!("{prefix}: timeoutMs must be a positive finite number")
            );
            let config = ExecutorConfig {
                max_timeout_ms: f64::NAN,
                ..ExecutorConfig::default()
            };
            assert_eq!(
                assert_serviceable(kind, &config).unwrap_err(),
                format!("{prefix}: maxTimeoutMs must be a positive finite number")
            );
            let config = ExecutorConfig {
                max_output_bytes: -1.0,
                ..ExecutorConfig::default()
            };
            assert_eq!(
                assert_serviceable(kind, &config).unwrap_err(),
                format!("{prefix}: maxOutputBytes must be a positive finite number")
            );
            let config = ExecutorConfig {
                max_spill_bytes: f64::INFINITY,
                ..ExecutorConfig::default()
            };
            assert_eq!(
                assert_serviceable(kind, &config).unwrap_err(),
                format!("{prefix}: maxSpillBytes must be a positive finite number")
            );
            let config = ExecutorConfig {
                grace_ms: fr_timeout::MAX_TIMER_DELAY_MS * 2.0,
                ..ExecutorConfig::default()
            };
            assert_eq!(
                assert_serviceable(kind, &config).unwrap_err(),
                format!("{prefix}: graceMs must be no greater than 2147483647")
            );
        }
    }

    /// Resolve fills the defaults, caps the override, and validates the
    /// stdout budget.
    #[test]
    fn resolve_defaults_clamp_and_stdout_cap() {
        let spec = resolve_bash(serde_json::json!({ "command": "true" }));
        assert_eq!(spec.workdir, current_dir());
        assert_eq!(spec.timeout_ms, 120_000.0);
        assert_eq!(spec.on_expiry, ShellExpiryPolicy::Kill);
        assert_eq!(spec.stdout_max_bytes, 64_000.0);
        let spec =
            resolve_bash(serde_json::json!({ "command": "true", "timeout_ms": 1_000_000.0 }));
        assert_eq!(spec.timeout_ms, 600_000.0, "the override caps at max");
        let error = bash_executor()
            .resolve(
                serde_json::from_value(serde_json::json!({
                    "command": "true",
                    "timeout_ms": 0.0,
                }))
                .expect("request"),
            )
            .unwrap_err();
        assert_eq!(
            error,
            "bash-local: request.timeoutMs must be a positive finite number"
        );
        let error = bash_executor()
            .resolve(
                serde_json::from_value(serde_json::json!({
                    "command": "true",
                    "stdout_max_bytes": 0.0,
                }))
                .expect("request"),
            )
            .unwrap_err();
        assert_eq!(
            error,
            "bash-local: request.stdoutMaxBytes must be a positive finite number"
        );
    }

    /// The two kinds pin their exact invocation argv; the pwsh command
    /// rides as one element behind the UTF-8 pinning preamble.
    #[test]
    fn argv_forms_pin_the_two_kinds() {
        let bash = bash_executor();
        let spec = resolve_bash(serde_json::json!({ "command": "echo hi" }));
        assert_eq!(bash.command_argv(&spec), vec!["bash", "-c", "echo hi"]);
        let pwsh = LocalExecutor::new(ShellKind::Pwsh, ExecutorConfig::default());
        assert_eq!(
            pwsh.command_argv(&spec),
            vec![
                "pwsh",
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!("{ENCODING_PREAMBLE}echo hi"),
            ],
        );
    }

    /// The spawn environment layers overrides, caller env, then the
    /// managed snapshot, so a caller entry can never displace a managed
    /// fact.
    #[test]
    fn env_layers_merge_overrides_then_env_then_managed() {
        let bash = bash_executor();
        let spec = resolve_bash(serde_json::json!({
            "command": "true",
            "env": { "NO_COLOR": "caller", "ONLY_ENV": "e" },
            "fr_env": { "NO_COLOR": "managed", "FR_SESSION_ID": "s" },
        }));
        let spawn = bash.spawn_spec(&spec, vec!["bash".to_owned()]);
        let env = spawn.env.expect("explicit env");
        assert_eq!(
            env.get("NO_COLOR").and_then(Option::as_deref),
            Some("managed")
        );
        assert_eq!(env.get("ONLY_ENV").and_then(Option::as_deref), Some("e"));
        assert_eq!(env.get("TERM").and_then(Option::as_deref), Some("dumb"));
        assert_eq!(
            env.get("FR_SESSION_ID").and_then(Option::as_deref),
            Some("s")
        );
        let pwsh_spec: ShellExecRequest = serde_json::from_value(serde_json::json!({
            "command": "true",
        }))
        .expect("request");
        let pwsh = LocalExecutor::new(ShellKind::Pwsh, ExecutorConfig::default());
        let spawn = pwsh.spawn_spec(&pwsh.resolve(pwsh_spec).expect("resolve"), Vec::new());
        let env = spawn.env.expect("explicit env");
        assert!(env.contains_key("NO_COLOR"));
        assert!(
            !env.contains_key("TERM"),
            "TERM=dumb is a POSIX concept and rides the bash arm only"
        );
    }

    /// The kill policy's timeout is the first cause: the command dies by
    /// SIGTERM and the result classifies timedOut.
    #[test]
    fn timeout_kill_classifies_timed_out() {
        let spec = resolve_bash(serde_json::json!({ "command": "sleep 5", "timeout_ms": 200.0 }));
        let execution = bash_executor().execute(&spec).expect("execute");
        let result = execution.result().expect("result");
        assert!(result.timed_out);
        assert!(!result.aborted);
        assert_eq!(result.signal.as_deref(), Some("SIGTERM"));
        assert_eq!(result.exit_code, None);
    }

    /// The caller's cancellation is the first cause when it wins the
    /// fused deadline race.
    #[test]
    fn caller_abort_classifies_aborted() {
        let signal = AbortSignal::new();
        let spec = resolve_bash(serde_json::json!({ "command": "sleep 5" }));
        let mut spec = spec;
        spec.signal = Some(Arc::clone(&signal));
        let execution = bash_executor().execute(&spec).expect("execute");
        signal.abort("tool call aborted");
        let result = execution.result().expect("result");
        assert!(result.aborted);
        assert!(!result.timed_out);
    }

    /// The none policy arms no deadline; the handle's kill is the stop
    /// path and classifies neither timedOut nor aborted.
    #[test]
    fn none_expiry_arms_no_deadline() {
        let spec = resolve_bash(serde_json::json!({
            "command": "sleep 5",
            "on_expiry": "none",
        }));
        let execution = bash_executor().execute(&spec).expect("execute");
        assert!(execution.kill(), "the first kill terminates");
        assert!(!execution.kill(), "the second kill is a no-op");
        let result = execution.result().expect("result");
        assert!(!result.timed_out);
        assert!(!result.aborted);
        assert_eq!(result.signal.as_deref(), Some("SIGTERM"));
    }

    /// A rejected spawn settles the handle killed and serves its
    /// stage-neutral note as the whole stderr stream.
    #[test]
    fn rejected_spawn_serves_the_note_on_stderr() {
        let spec = resolve_bash(
            serde_json::json!({ "command": "true", "workdir": "/definitely/not/a/dir" }),
        );
        let execution = bash_executor().execute(&spec).expect("execute");
        assert!(execution.result().is_err(), "the projection rejects");
        let read = execution.observed_stderr(0);
        assert!(
            read.text
                .starts_with("subprocess failed before reporting an outcome: "),
            "the note is the whole stream, got {:?}",
            read.text
        );
    }

    /// The foreground result carries the canonical stream split.
    #[test]
    fn foreground_result_splits_the_streams() {
        let spec = resolve_bash(serde_json::json!({
            "command": "echo out; echo err >&2",
        }));
        let execution = bash_executor().execute(&spec).expect("execute");
        let result = execution.result().expect("result");
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.signal, None);
        assert!(result.stdout.text.starts_with("out"));
        assert!(result.stderr.text.starts_with("err"));
        assert!(!result.stdout.truncated);
    }

    /// The terminal outcome maps killed and completed onto the job
    /// vocabulary with the sandbox notes appended.
    #[test]
    fn process_outcome_texts() {
        let spec = resolve_bash(serde_json::json!({ "command": "exit 3" }));
        let execution = bash_executor().execute(&spec).expect("execute");
        let outcome = execution.process_outcome(&[]);
        assert_eq!(outcome.status, JobStatus::Completed);
        assert_eq!(outcome.detail.as_deref(), Some("exit code: 3"));

        let spec = resolve_bash(serde_json::json!({ "command": "sleep 5", "timeout_ms": 150.0 }));
        let execution = bash_executor().execute(&spec).expect("execute");
        let outcome = execution.process_outcome(&[]);
        assert_eq!(outcome.status, JobStatus::Killed);
        assert_eq!(outcome.detail.as_deref(), Some("signal: SIGTERM"));
    }
}
