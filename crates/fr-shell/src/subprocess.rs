// Rust guideline compliant 2026-10-29
//! Process plumbing for the local subprocess service (dsh
//! `subprocess-local/src/{spawn,output}.ts` + the seam types).
//!
//! Fully specified spawns with per-stream stdio dispositions, tail-keep
//! collection with private spill files, provider-owned managed-range
//! termination, and the platform containment ladder. Q16a-2A tiers the
//! range owner: Linux prefers a user-systemd transient scope, Windows a
//! Job Object, and every platform falls back to the detached process
//! group with a TERM→KILL ladder (the dsh `fallbackOwner`). This module
//! ships the ladder arm and the selector seam; the scope and Job arms
//! join the same selector within this slice. POSIX paths stage TERM
//! before KILL; Windows terminates the tree immediately via
//! `taskkill /T /F`.

use std::collections::HashMap;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use fr_timeout::MAX_TIMER_DELAY_MS;

use crate::service::js_number;

/// Credential-shaped environment names are NOT forwarded to children
/// (dsh `SENSITIVE_ENV_PATTERN`, case-insensitive).
const SENSITIVE_KEY_WORDS: [&str; 4] = ["KEY", "PASSWORD", "SECRET", "TOKEN"];

/// The managed child environment prefix this harness strips from the
/// ambient base (PRD 19.2 条 25: the dsh `DSH_` scrub under the FR_
/// ruling).
pub const FR_ENV_PREFIX: &str = "FR_";

/// One captured stream: the (possibly truncated) text plus recovery info
/// (dsh `CollectedOutput`).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CollectedOutput {
    /// Collected text — the tail of the stream when truncated.
    pub text: String,
    /// True when bytes were dropped from `text`.
    pub truncated: bool,
    /// Path to a file holding the complete stream, when truncated and
    /// available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spill_path: Option<String>,
}

/// stdin disposition (dsh `SubprocessStdinMode`): `/dev/null`, or bytes
/// written then closed.
#[derive(Debug, Clone)]
pub enum StdinMode {
    /// Leave fd 0 on `/dev/null`.
    Ignore,
    /// Write the bytes and close (the batch shape).
    Data(String),
}

/// Spill storage for one collected stream; `None` means tail-only
/// collection (dsh `SubprocessCollect.spill`).
#[derive(Debug, Clone)]
pub struct SpillCap {
    /// Whole-stream byte cap; a larger stream discards its now-incomplete
    /// spill.
    pub max_bytes: u64,
}

/// Bounded in-memory collection for one output stream (dsh
/// `SubprocessCollect`).
#[derive(Debug, Clone)]
pub struct SubprocessCollect {
    /// In-memory cap in bytes; overflow keeps the tail.
    pub max_bytes: u64,
    /// Full-stream spill file; `None` disables spilling entirely.
    pub spill: Option<SpillCap>,
}

/// stdout/stderr disposition (dsh `SubprocessOutputMode`). The raw
/// `'pipe'` mode is an in-process dsh capability with no M1 consumer —
/// the wire face accepts `inherit` and collect objects only.
#[derive(Debug, Clone)]
pub enum OutputMode {
    /// Pass the parent's descriptor through.
    Inherit,
    /// Buffer boundedly with offset-based reads.
    Collect(SubprocessCollect),
}

/// Per-stream stdio dispositions, all explicit (dsh `SubprocessStdio`).
#[derive(Debug, Clone)]
pub struct Stdio {
    /// stdin disposition.
    pub stdin: StdinMode,
    /// stdout disposition.
    pub stdout: OutputMode,
    /// stderr disposition.
    pub stderr: OutputMode,
}

/// A fully-specified spawn request (dsh `SubprocessSpawnSpec`). This seam
/// applies no defaults: every disposition, limit, and directory is
/// explicit.
#[derive(Debug, Clone)]
pub struct SpawnSpec {
    /// Executable and arguments; `argv[0]` is the program. Never
    /// shell-interpreted here.
    pub argv: Vec<String>,
    /// Working directory for the child.
    pub cwd: String,
    /// Per-stream stdio dispositions.
    pub stdio: Stdio,
    /// Positive finite grace period in milliseconds for the termination
    /// ladder and the collected-pipe drain.
    pub grace_ms: f64,
    /// Explicit environment entries merged onto the scrubbed parent base;
    /// `None` is a tombstone that removes an ordinary ambient entry.
    pub env: Option<HashMap<String, Option<String>>>,
}

/// Exit facts of one closed process — the close-event vocabulary (dsh
/// `SubprocessOutcome`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Outcome {
    /// Exit code; `None` when the process died from a signal.
    pub exit_code: Option<i32>,
    /// Terminating signal name (e.g. `SIGTERM`); `None` on normal exit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
}

/// One incremental read from a collected stream (dsh
/// `SubprocessOutputRead`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OutputRead {
    /// Stream text from the requested offset (the whole retained tail
    /// when lossy).
    pub text: String,
    /// Whole-stream offset to resume from.
    pub next_offset: u64,
    /// True when the requested offset slid out of the in-memory tail
    /// window.
    pub lossy: bool,
    /// Path to the full-stream spill file, when one remains intact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spill_path: Option<String>,
}

/// The per-process private spill directory (dsh `privateSpillDir`):
/// created once per process, never recreated; a cleaner removing it while
/// empty only degrades the next spill to the in-memory tail.
fn private_spill_dir() -> &'static str {
    static DIR: OnceLock<String> = OnceLock::new();
    DIR.get_or_init(|| {
        let base = std::env::temp_dir();
        let candidate = loop {
            let suffix = uuid::Uuid::new_v4().simple().to_string()[..12].to_owned();
            let path: PathBuf = base.join(format!("fr-subprocess-{suffix}"));
            match std::fs::create_dir(&path) {
                Ok(()) => break path,
                Err(_) if base.is_dir() => {}
                Err(_) => break base.join(format!("fr-subprocess-fallback-{suffix}")),
            }
        };
        let _ignored = std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o700));
        candidate.to_string_lossy().into_owned()
    })
}

/// The monotonically increasing spill-file counter (dsh `spillCounter`).
static SPILL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Collects one stream with a bounded in-memory tail (dsh
/// `OutputCollector`).
///
/// On first overflow a spill file is created and every chunk (including
/// those already collected) is appended there while the full stream
/// remains within the spill cap; spilling is best-effort and a failure
/// degrades to the in-memory tail.
#[derive(Debug)]
pub struct OutputCollector {
    inner: Mutex<CollectorState>,
    /// In-memory tail cap in bytes.
    max_bytes: u64,
    /// Stream label used in spill file names.
    label: &'static str,
    /// Spill storage; `None` disables spilling entirely.
    spill_cap: Option<u64>,
}

#[derive(Debug, Default)]
struct CollectorState {
    chunks: Vec<Vec<u8>>,
    bytes: u64,
    dropped: bool,
    spill_file: Option<String>,
    spill_fd: Option<std::fs::File>,
    spill_disabled: bool,
    total: u64,
}

impl OutputCollector {
    /// Creates one collector.
    #[must_use]
    pub fn new(max_bytes: u64, label: &'static str, spill: Option<SpillCap>) -> Self {
        Self {
            inner: Mutex::new(CollectorState::default()),
            max_bytes,
            label,
            spill_cap: spill.map(|spill| spill.max_bytes),
        }
    }

    /// Ingests one stream chunk, counting it toward the whole-stream
    /// total (the dsh `push` semantics: spill on first overflow, tail
    /// keeps the last `max_bytes` byte-exact).
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the chunk ingestion and spill decision are one atomic collector mutation"
    )]
    pub fn push(&self, chunk: &[u8]) {
        let mut state = recover(&self.inner);
        let chunk_len = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
        state.total = state.total.saturating_add(chunk_len);
        let overflows = state.bytes.saturating_add(chunk_len) > self.max_bytes;
        if let Some(spill_max) = self.spill_cap
            && !state.spill_disabled
            && (overflows || state.spill_fd.is_some())
        {
            Self::spill_all(&mut state, spill_max, chunk, self.label);
        }
        state.chunks.push(chunk.to_vec());
        state.bytes = state.bytes.saturating_add(chunk_len);
        while state.bytes > self.max_bytes {
            let Some(head_len) = state.chunks.first().map(Vec::len) else {
                break;
            };
            let head_len = u64::try_from(head_len).unwrap_or(u64::MAX);
            let excess = state.bytes - self.max_bytes;
            if head_len <= excess {
                let head = state.chunks.remove(0);
                state.bytes = state
                    .bytes
                    .saturating_sub(u64::try_from(head.len()).unwrap_or(0));
            } else {
                // Trim the head so the retained window is byte-exact at
                // the cap: keep the tail portion of the head chunk.
                if let Some(head) = state.chunks.first_mut() {
                    let keep = usize::try_from(head_len - excess).unwrap_or(0);
                    let start = head.len().saturating_sub(keep);
                    *head = head[start..].to_vec();
                }
                state.bytes = state.bytes.saturating_sub(excess);
            }
            state.dropped = true;
        }
    }

    /// Opens the spill file lazily and appends the chunk (and any prior
    /// chunks once); every filesystem failure discards the spill and
    /// collection continues (the dsh contained-failure shape).
    fn spill_all(state: &mut CollectorState, spill_max: u64, chunk: &[u8], label: &'static str) {
        if state.total > spill_max {
            Self::discard_spill(state);
            return;
        }
        let result = (|| -> std::io::Result<()> {
            if state.spill_fd.is_none() {
                let counter = SPILL_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
                let pid = std::process::id();
                let hex = hex6();
                let file = std::path::Path::new(private_spill_dir())
                    .join(format!("fr-subprocess-{pid}-{counter}-{hex}-{label}.log"));
                // The create-new mode fails on any existing path, symlink
                // or not; the path is published only after the open
                // succeeded.
                let mut fd = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&file)?;
                std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
                for prior in &state.chunks {
                    fd.write_all(prior)?;
                }
                state.spill_file = Some(file.to_string_lossy().into_owned());
                state.spill_fd = Some(fd);
            }
            if let Some(fd) = state.spill_fd.as_mut() {
                fd.write_all(chunk)?;
            }
            Ok(())
        })();
        if result.is_err() {
            Self::discard_spill(state);
        }
    }

    /// Stops spilling and removes the file once it can no longer hold the
    /// complete stream.
    fn discard_spill(state: &mut CollectorState) {
        state.spill_fd = None;
        state.spill_file = None;
        state.spill_disabled = true;
    }

    /// Incremental read in whole-stream byte coordinates (the dsh
    /// `readFrom`).
    #[must_use]
    pub fn read_from(&self, from_byte: u64) -> OutputRead {
        let state = recover(&self.inner);
        let window_start = state.total.saturating_sub(state.bytes);
        let lossy = from_byte < window_start;
        let concatenated = state.chunks.concat();
        let slice: Vec<u8> = if lossy {
            concatenated
        } else {
            let skip = usize::try_from(from_byte - window_start).unwrap_or(0);
            let skip = skip.min(concatenated.len());
            concatenated[skip..].to_vec()
        };
        OutputRead {
            text: String::from_utf8_lossy(&slice).into_owned(),
            next_offset: state.total,
            lossy,
            spill_path: state.spill_file.clone(),
        }
    }

    /// Closes the spill file once the stream has ended; a failed close
    /// stops advertising the path while every read keeps working.
    pub fn seal(&self) {
        let mut state = recover(&self.inner);
        state.spill_fd = None;
    }

    /// Seals and returns the final output (the dsh `finalize`).
    #[must_use]
    pub fn finalize(&self) -> CollectedOutput {
        self.seal();
        let state = recover(&self.inner);
        CollectedOutput {
            text: String::from_utf8_lossy(&state.chunks.concat()).into_owned(),
            truncated: state.dropped,
            spill_path: state.spill_file.clone(),
        }
    }
}

/// Six random hex characters for one spill file name.
fn hex6() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..6].to_owned()
}

/// Locks one mutex, recovering from poisoning (the fr-tools `recover`
/// judgment).
fn recover<T>(lock: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    lock.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The ambient parent environment minus credential-shaped names and minus
/// all `FR_*` names — the canonical base every harness child starts from
/// (dsh `scrubbedParentEnv` under the FR_ ruling).
///
/// Both scrubs match case-insensitively.
#[must_use]
pub fn scrubbed_parent_env() -> HashMap<String, String> {
    let mut env = HashMap::new();
    for (key, value) in std::env::vars() {
        let upper = key.to_uppercase();
        let sensitive = SENSITIVE_KEY_WORDS.iter().any(|word| upper.contains(word));
        if !sensitive && !upper.starts_with(FR_ENV_PREFIX) {
            env.insert(key, value);
        }
    }
    env
}

/// Builds a child environment: explicit caller entries override the
/// scrubbed parent base; `None` tombstones remove an ordinary ambient
/// entry (dsh `childEnv`).
#[must_use]
pub fn child_env<S: std::hash::BuildHasher>(
    extra: Option<&HashMap<String, Option<String>, S>>,
) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = scrubbed_parent_env();
    if let Some(extra) = extra {
        for (key, value) in extra {
            match value {
                Some(value) => {
                    env.insert(key.clone(), value.clone());
                }
                None => {
                    env.remove(key);
                }
            }
        }
    }
    env
}

/// Validates the synchronous portion of one spawn request (the dsh
/// `validateSubprocessSpec` texts verbatim).
///
/// # Errors
///
/// The grace/argv validation, or a signal already aborted before spawn.
pub fn validate_spawn_spec(spec: &SpawnSpec, abort_reason: Option<String>) -> Result<(), String> {
    if !spec.grace_ms.is_finite() || spec.grace_ms <= 0.0 || spec.grace_ms > MAX_TIMER_DELAY_MS {
        return Err(format!(
            "subprocess graceMs must be a positive finite number no greater than {}",
            js_number(MAX_TIMER_DELAY_MS)
        ));
    }
    if let Some(reason) = abort_reason {
        return Err(format!("aborted before spawn: {reason}"));
    }
    match spec.argv.first() {
        Some(program) if !program.is_empty() => Ok(()),
        _ => Err("invalid argv: expected a non-empty program name at argv[0]".to_owned()),
    }
}

/// Sends `sig` to a detached POSIX process group; never fails (delivery
/// races process exit, so a missing pid is a no-op).
pub fn kill_group(pid: i32, sig: i32) {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: a raw libc group-signal call with a validated pid; the
        // failure paths (ESRCH) are the no-op the contract asks for.
        let _ignored = unsafe { libc::kill(-pid, sig) };
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (pid, sig);
    }
}

/// Terminates one Windows process tree with `taskkill /PID <pid> /T /F`
/// (contained like POSIX group signalling).
fn taskkill_process_tree(pid: u32) {
    let _ignored = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Whether any live process still belongs to the POSIX process group
/// (the dsh `/proc` member probe, Linux only).
#[cfg(target_os = "linux")]
#[must_use]
pub fn linux_group_has_live_members(process_group_id: i32) -> bool {
    let Ok(proc_dir) = std::fs::read_dir("/proc") else {
        return true;
    };
    for entry in proc_dir.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // pgrp is field 5; the comm field may contain spaces and parens,
        // so parse from after the last closing paren.
        let Some(rest) = stat.rsplit_once(')').map(|(_, rest)| rest) else {
            continue;
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        // fields[0] = state (field 3), fields[2] = pgrp (field 5).
        if let Some(pgrp) = fields.get(2)
            && let Ok(pgrp) = pgrp.parse::<i32>()
            && pgrp == process_group_id
        {
            return true;
        }
    }
    false
}

/// The settled exit facts of one spawn (guarded publication).
#[derive(Debug, Default)]
struct OutcomeSlot {
    outcome: Option<Outcome>,
    failed: Option<String>,
}

/// The live subprocess and its provider-managed process range (the
/// fallback-tier dsh `LocalSubprocessHandle`).
pub struct SubprocessHandle {
    pid: u32,
    state: Arc<HandleState>,
    collected: Option<(Arc<OutputCollector>, Arc<OutputCollector>)>,
}

struct HandleState {
    /// The direct child's exit facts once settled.
    outcome: Mutex<OutcomeSlot>,
    /// Waiters wake on outcome publication.
    outcome_ready: Condvar,
    /// Termination ladder started (idempotent).
    termination_started: AtomicBool,
    /// The managed range confirmed empty.
    range_gone: AtomicBool,
    /// POSIX group leader pid (`-pid` signalling; negative on Windows).
    group_pid: i32,
    grace_ms: f64,
    direct_settled: AtomicBool,
    is_windows: bool,
    /// The root pid (the group leader in the fallback tier).
    pid: u32,
    /// The spawned child's wait thread.
    waiter: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl std::fmt::Debug for SubprocessHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubprocessHandle")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

impl SubprocessHandle {
    /// The collected stdout reader, when stdout was a collect disposition.
    #[must_use]
    pub fn stdout_collector(&self) -> Option<&Arc<OutputCollector>> {
        self.collected.as_ref().map(|(stdout, _)| stdout)
    }

    /// The collected stderr reader, when stderr was a collect disposition.
    #[must_use]
    pub fn stderr_collector(&self) -> Option<&Arc<OutputCollector>> {
        self.collected.as_ref().map(|(_, stderr)| stderr)
    }

    /// Begins the provider's termination procedure on the managed range:
    /// TERM, then a KILL escalation after `graceMs` (Windows terminates
    /// the tree immediately). Idempotent.
    ///
    /// # Panics
    ///
    /// Never in practice: the ladder thread's builder only fails on OS
    /// resource exhaustion, which the plugin contract treats as
    /// unreachable (the dsh thread-spawn expectation).
    pub fn terminate(&self) {
        if self.state.range_gone.load(Ordering::Acquire)
            || self.state.termination_started.swap(true, Ordering::AcqRel)
        {
            return;
        }
        let state = Arc::clone(&self.state);
        std::thread::Builder::new()
            .name("fr-shell-range-kill".to_owned())
            .spawn(move || run_termination_ladder(&state))
            .expect("spawning the termination ladder never fails under the plugin contract");
    }

    /// Synchronous force-termination of the current tree without timers
    /// (the host-exit path).
    pub fn terminate_for_host_exit(&self) {
        if self.state.is_windows {
            taskkill_process_tree(self.pid);
        } else {
            kill_group(self.state.group_pid, libc::SIGKILL);
        }
    }

    /// Whether the managed range still holds a live member.
    #[must_use]
    fn range_alive(&self) -> bool {
        if self.state.range_gone.load(Ordering::Acquire) {
            return false;
        }
        if self.state.is_windows {
            let slot = self.state.outcome.lock().expect("outcome lock");
            return slot.outcome.is_none() && slot.failed.is_none();
        }
        // SAFETY: a raw liveness probe (`kill(-pid, 0)`) with the group
        // pid recorded at spawn.
        let probe = unsafe { libc::kill(self.state.group_pid, 0) };
        if probe != 0 {
            return false;
        }
        if self.state.direct_settled.load(Ordering::Acquire)
            && !linux_group_has_live_members(-self.state.group_pid)
        {
            return false;
        }
        true
    }

    /// Waits until the managed range is empty (the dsh `waitForExit`
    /// 15 ms poll cadence; the deadline bounds the wait).
    #[must_use]
    pub fn wait_for_exit(&self, deadline: Option<Instant>) -> bool {
        loop {
            if !self.range_alive() {
                return true;
            }
            if let Some(deadline) = deadline
                && Instant::now() >= deadline
            {
                return false;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    /// Blocks until the spawned command's exit facts settle (the direct
    /// child, not the whole range).
    ///
    /// # Errors
    ///
    /// The spawn failure (the dsh `done` rejection).
    pub fn done(&self) -> Result<Outcome, String> {
        let mut slot = recover(&self.state.outcome);
        loop {
            if let Some(outcome) = slot.outcome.clone() {
                return Ok(outcome);
            }
            if let Some(failure) = slot.failed.clone() {
                return Err(failure);
            }
            slot = self
                .state
                .outcome_ready
                .wait(slot)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Seals both collectors (reads after exit never point at a
    /// still-open file).
    pub fn seal_collectors(&self) {
        if let Some((stdout, stderr)) = &self.collected {
            stdout.seal();
            stderr.seal();
        }
    }
}

/// The TERM→KILL ladder against one range (the dsh
/// `terminateWithReason` shape; the shared timer clears when the range
/// exit is observed).
fn run_termination_ladder(state: &Arc<HandleState>) {
    if state.range_gone.load(Ordering::Acquire) {
        return;
    }
    if state.is_windows {
        taskkill_process_tree(state.pid);
    } else {
        kill_group(state.group_pid, libc::SIGTERM);
    }
    let grace = Duration::from_millis(crate::jobs_local::ms_u64(state.grace_ms));
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline && !state.range_gone.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(15));
    }
    if state.range_gone.load(Ordering::Acquire) {
        return;
    }
    if state.is_windows {
        taskkill_process_tree(state.pid);
    } else {
        kill_group(state.group_pid, libc::SIGKILL);
    }
}

/// Maps a raw Unix signal number to the Node signal name vocabulary.
#[cfg(unix)]
#[must_use]
pub fn signal_name(signal: i32) -> String {
    match signal {
        1 => "SIGHUP".to_owned(),
        2 => "SIGINT".to_owned(),
        3 => "SIGQUIT".to_owned(),
        4 => "SIGILL".to_owned(),
        5 => "SIGTRAP".to_owned(),
        6 => "SIGABRT".to_owned(),
        7 => "SIGBUS".to_owned(),
        8 => "SIGFPE".to_owned(),
        9 => "SIGKILL".to_owned(),
        10 => "SIGUSR1".to_owned(),
        11 => "SIGSEGV".to_owned(),
        12 => "SIGUSR2".to_owned(),
        13 => "SIGPIPE".to_owned(),
        14 => "SIGALRM".to_owned(),
        15 => "SIGTERM".to_owned(),
        16 => "SIGSTKFLT".to_owned(),
        17 => "SIGCHLD".to_owned(),
        18 => "SIGCONT".to_owned(),
        19 => "SIGSTOP".to_owned(),
        20 => "SIGTSTP".to_owned(),
        21 => "SIGTTIN".to_owned(),
        22 => "SIGTTOU".to_owned(),
        23 => "SIGURG".to_owned(),
        24 => "SIGXCPU".to_owned(),
        25 => "SIGXFSZ".to_owned(),
        26 => "SIGVTALRM".to_owned(),
        27 => "SIGPROF".to_owned(),
        28 => "SIGWINCH".to_owned(),
        29 => "SIGIO".to_owned(),
        30 => "SIGPWR".to_owned(),
        31 => "SIGSYS".to_owned(),
        other => other.to_string(),
    }
}

/// The containment tier selected for one launch (Q16a-2A; the scope and
/// Job arms join this selector within the slice).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainmentTier {
    /// The detached process group with a TERM→KILL ladder (the dsh
    /// `fallbackOwner`).
    ProcessGroup,
}

/// Selects the containment tier for one launch (the dsh
/// `selectContainmentMode` position; the Linux scope probe and the
/// Windows Job probe land with their arms).
#[must_use]
pub const fn select_containment_tier() -> ContainmentTier {
    ContainmentTier::ProcessGroup
}

/// Builds the detached launch command for one spawn spec: the scrubbed
/// environment, the per-stream stdio dispositions, and (POSIX) the own
/// process group.
fn prepare_command(spec: &SpawnSpec) -> std::process::Command {
    let program = spec.argv[0].clone();
    let mut command = std::process::Command::new(&program);
    command
        .args(&spec.argv[1..])
        .current_dir(&spec.cwd)
        .env_clear();
    for (key, value) in child_env(spec.env.as_ref()) {
        command.env(key, value);
    }
    #[cfg(unix)]
    command.process_group(0);
    match &spec.stdio.stdin {
        StdinMode::Ignore => {
            command.stdin(std::process::Stdio::null());
        }
        StdinMode::Data(_) => {
            command.stdin(std::process::Stdio::piped());
        }
    }
    match &spec.stdio.stdout {
        OutputMode::Inherit => {
            command.stdout(std::process::Stdio::inherit());
        }
        OutputMode::Collect(_) => {
            command.stdout(std::process::Stdio::piped());
        }
    }
    match &spec.stdio.stderr {
        OutputMode::Inherit => {
            command.stderr(std::process::Stdio::inherit());
        }
        OutputMode::Collect(_) => {
            command.stderr(std::process::Stdio::piped());
        }
    }
    command
}

/// The group-leader pid cast with its platform reason (attributes
/// cannot ride expressions).
#[expect(
    clippy::cast_possible_wrap,
    reason = "the group leader pid fits i32 on every kernel that hands out pids"
)]
const fn group_pid_of(pid: u32) -> i32 {
    pid as i32
}

/// Spawns one collector drain thread over a piped stream (the dsh
/// collector wiring; the heap buffer keeps the stack small).
fn spawn_collector<R: std::io::Read + Send + 'static>(
    label: &'static str,
    thread_name: &str,
    max_bytes: u64,
    spill: Option<SpillCap>,
    mut stream: R,
) -> Arc<OutputCollector> {
    let collector = Arc::new(OutputCollector::new(max_bytes, label, spill));
    let sink = Arc::clone(&collector);
    std::thread::Builder::new()
        .name(thread_name.to_owned())
        .spawn(move || {
            let mut buffer = vec![0_u8; 65_536];
            loop {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => sink.push(&buffer[..read]),
                }
            }
        })
        .expect("spawning a collector thread never fails under the plugin contract");
    collector
}

/// Spawns one fully-specified subprocess in the selected containment tier
/// and binds the common lifecycle.
///
/// # Errors
///
/// The synchronous validation texts, or the spawn failure itself.
///
/// # Panics
///
pub fn spawn(spec: &SpawnSpec, abort_reason: Option<String>) -> Result<SubprocessHandle, String> {
    validate_spawn_spec(spec, abort_reason)?;
    let tier = select_containment_tier();
    debug_assert_eq!(tier, ContainmentTier::ProcessGroup);
    let mut child = prepare_command(spec)
        .spawn()
        .map_err(|error| error.to_string())?;
    let pid = child.id();
    let collected = drain_stdin_and_wire_collectors(spec, &mut child);
    let state = Arc::new(new_handle_state(pid, spec.grace_ms));

    // The direct-outcome thread: waits the child, then the bounded pipe
    // drain (the dsh `pipeDrainTimer`), then publishes the outcome.
    let waiter = spawn_waiter(&state, child, collected.clone());
    if let Some(waiter) = waiter {
        *recover(&state.waiter) = Some(waiter);
    }

    Ok(SubprocessHandle {
        pid,
        state,
        collected,
    })
}

/// Writes the batch stdin (best-effort, EPIPE contained) and spawns the
/// two collector drain threads for collect-mode streams.
fn drain_stdin_and_wire_collectors(
    spec: &SpawnSpec,
    child: &mut std::process::Child,
) -> Option<(Arc<OutputCollector>, Arc<OutputCollector>)> {
    // Batch stdin is written and closed up front; write errors (EPIPE)
    // are best-effort.
    if let (StdinMode::Data(data), Some(stdin)) = (&spec.stdio.stdin, child.stdin.take()) {
        let mut stdin = stdin;
        let _ignored = stdin.write_all(data.as_bytes());
        drop(stdin);
    }

    // Reader threads push stream bytes into the collectors; the two
    // sides share one drain helper.
    let stdout_pipe = child.stdout.take();
    let stdout_collector = match (&spec.stdio.stdout, stdout_pipe) {
        (OutputMode::Collect(collect), Some(stream)) => Some(spawn_collector(
            "stdout",
            "fr-shell-collect-stdout",
            collect.max_bytes,
            collect.spill.clone(),
            stream,
        )),
        _ => None,
    };
    let stderr_pipe = child.stderr.take();
    let stderr_collector = match (&spec.stdio.stderr, stderr_pipe) {
        (OutputMode::Collect(collect), Some(stream)) => Some(spawn_collector(
            "stderr",
            "fr-shell-collect-stderr",
            collect.max_bytes,
            collect.spill.clone(),
            stream,
        )),
        _ => None,
    };
    match (stdout_collector, stderr_collector) {
        (Some(stdout), Some(stderr)) => Some((stdout, stderr)),
        (Some(stdout), None) => Some((stdout, Arc::new(OutputCollector::new(1, "stderr", None)))),
        (None, Some(stderr)) => Some((Arc::new(OutputCollector::new(1, "stdout", None)), stderr)),
        (None, None) => None,
    }
}

/// The shared per-handle state for one launch.
fn new_handle_state(pid: u32, grace_ms: f64) -> HandleState {
    let is_windows = cfg!(windows);
    HandleState {
        outcome: Mutex::new(OutcomeSlot::default()),
        outcome_ready: Condvar::new(),
        termination_started: AtomicBool::new(false),
        range_gone: AtomicBool::new(false),
        group_pid: if is_windows { -1 } else { group_pid_of(pid) },
        grace_ms,
        direct_settled: AtomicBool::new(false),
        is_windows,
        pid,
        waiter: Mutex::new(None),
    }
}

/// Spawns the direct-outcome thread: waits the child, then the bounded
/// pipe drain (the dsh `pipeDrainTimer`), then publishes the outcome.
fn spawn_waiter(
    state: &Arc<HandleState>,
    mut child: std::process::Child,
    collected: Option<(Arc<OutputCollector>, Arc<OutputCollector>)>,
) -> Option<std::thread::JoinHandle<()>> {
    let wait_state = Arc::clone(state);
    std::thread::Builder::new()
        .name("fr-shell-child-wait".to_owned())
        .spawn(move || {
            let outcome = match child.wait() {
                Ok(status) => status.signal().map_or_else(
                    || Outcome {
                        exit_code: status.code(),
                        signal: None,
                    },
                    |signal| Outcome {
                        exit_code: None,
                        signal: Some(signal_name(signal)),
                    },
                ),
                Err(error) => {
                    let mut slot = recover(&wait_state.outcome);
                    slot.failed = Some(error.to_string());
                    drop(slot);
                    wait_state.outcome_ready.notify_all();
                    return;
                }
            };
            // Bounded pipe drain: give the collectors the grace period to
            // finish reading, then seal (the dsh drain boundary).
            let deadline = Instant::now()
                + Duration::from_millis(crate::jobs_local::ms_u64(wait_state.grace_ms));
            if let Some((stdout, stderr)) = &collected {
                while Instant::now() < deadline {
                    let stdout_done = reader_idle(stdout);
                    let stderr_done = reader_idle(stderr);
                    if stdout_done && stderr_done {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                stdout.seal();
                stderr.seal();
            }
            wait_state.direct_settled.store(true, Ordering::Release);
            let mut slot = recover(&wait_state.outcome);
            slot.outcome = Some(outcome);
            drop(slot);
            wait_state.outcome_ready.notify_all();
        })
        .expect("spawning the child wait thread never fails under the plugin contract")
        .into()
}

/// Whether one collector's reader thread finished (the tail stopped
/// growing across a poll window).
fn reader_idle(collector: &Arc<OutputCollector>) -> bool {
    let first = collector.read_from(0).next_offset;
    std::thread::sleep(Duration::from_millis(5));
    collector.read_from(0).next_offset == first
}
