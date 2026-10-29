// Rust guideline compliant 2026-10-29
//! The three-band wire service (the `'subprocess'` low band, the
//! `'jobs'` 1000+ band, and the `'shell'` 2000+ band) over one export
//! table.
//!
//! The absorption judgment: the dsh bash tool's `startJob` lives here
//! because the process output-source closures cannot cross the wire.
//! `shell.start` registers the command as a job with lazy process
//! readers, the job pump drains the streams into the ring, and
//! `shell.result` serves the foreground projection the promote path's
//! settled-while-waiting arm needs. The dougong instance lock spans a
//! whole wire call, so every handler stays bounded: spawns return at
//! once, waits ride explicit timeouts, and the job wait face is the
//! slice-driven primitive the tool layer polls.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use fr_sandbox::seam::SandboxMode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::executors::{
    self, LocalExecutor, PolicyResolve, ProcSlot, SandboxConfine, SandboxExecutor,
    ShellExecRequest, ShellExecSpec, ShellExecutorFace, ShellKind, ShellRunResult, process_sources,
};
use crate::jobs_local::{ControllerGuard, JobHooks, JobSpec, LocalJobRegistry};
use crate::jobs_types::JobToolCall;
use crate::shell_types::AbortSignal;
use crate::subprocess::{Outcome, SpawnSpec, StdinMode, Stdio, SubprocessHandle};

/// Renders one finite number the way dsh `JSON.stringify` does for the
/// error texts.
///
/// Integers render without a decimal tail, everything else in Rust's
/// shortest round-trip form (the f64 Display judgment from the
/// fr-session stateVersion precedent).
#[must_use]
pub fn js_number(value: f64) -> String {
    if value.to_bits() & !(1_u64 << 63) == 0 {
        return "0".to_owned();
    }
    if value.fract() == 0.0 && value.abs() < 1e21 {
        let truncated = value.trunc();
        // Exact equality is the branch predicate itself: trunc() on an
        // integral value is the identity by construction.
        #[expect(
            clippy::float_cmp,
            reason = "trunc() on an integral value is the identity, so the equality is the branch's own definition"
        )]
        if truncated == value {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "the branch bounds the value under 1e21 and integral, so the i128 cast keeps every digit"
            )]
            let as_int = truncated as i128;
            return as_int.to_string();
        }
    }
    value.to_string()
}

/// The stdin disposition on the spawn wire (`"ignore"` or `{"data": …}`,
/// the dsh seam's two model-relevant shapes).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireStdinPlain {
    /// Leave stdin empty.
    Ignore,
}

/// The batch stdin body.
#[derive(Debug, Serialize, Deserialize)]
pub struct WireStdinData {
    /// The bytes written before the close.
    pub data: String,
}

/// The stdin wire shape (string or object).
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WireStdin {
    /// The `/dev/null` shape.
    Plain(WireStdinPlain),
    /// The write-then-close shape.
    Data(WireStdinData),
}

/// The passthrough output disposition on the spawn wire.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireOutputPlain {
    /// Pass the parent's descriptor through.
    Inherit,
}

/// The bounded-collect output disposition on the spawn wire.
#[derive(Debug, Serialize, Deserialize)]
pub struct WireCollect {
    /// In-memory cap in bytes; overflow keeps the tail.
    pub max_bytes: u64,
    /// Full-stream spill cap; absent disables spilling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spill: Option<crate::subprocess::SpillCap>,
}

/// The stdout/stderr wire shape (string or collect object).
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WireOutputMode {
    /// The passthrough shape.
    Plain(WireOutputPlain),
    /// The bounded-collect shape.
    Collect(WireCollect),
}

/// The per-stream dispositions on the spawn wire.
#[derive(Debug, Serialize, Deserialize)]
pub struct WireStdio {
    /// stdin disposition.
    pub stdin: WireStdin,
    /// stdout disposition.
    pub stdout: WireOutputMode,
    /// stderr disposition.
    pub stderr: WireOutputMode,
}

/// The spawn request on the wire (dsh `SubprocessSpawnSpec`).
#[derive(Debug, Serialize, Deserialize)]
pub struct WireSpawnSpec {
    /// Executable and arguments.
    pub argv: Vec<String>,
    /// Working directory for the child.
    pub cwd: String,
    /// Per-stream dispositions.
    pub stdio: WireStdio,
    /// Positive finite grace period in milliseconds.
    pub grace_ms: f64,
    /// Explicit environment entries over the scrubbed base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, Option<String>>>,
}

impl WireSpawnSpec {
    /// Lowers the wire shape onto the domain spawn spec.
    #[must_use]
    pub fn into_domain(self) -> SpawnSpec {
        let stdin = match self.stdio.stdin {
            WireStdin::Plain(_) => StdinMode::Ignore,
            WireStdin::Data(body) => StdinMode::Data(body.data),
        };
        let stdout = wire_output(self.stdio.stdout);
        let stderr = wire_output(self.stdio.stderr);
        SpawnSpec {
            argv: self.argv,
            cwd: self.cwd,
            stdio: Stdio {
                stdin,
                stdout,
                stderr,
            },
            grace_ms: self.grace_ms,
            env: self.env,
        }
    }
}

/// Lowers one output disposition.
const fn wire_output(mode: WireOutputMode) -> crate::subprocess::OutputMode {
    match mode {
        WireOutputMode::Plain(_) => crate::subprocess::OutputMode::Inherit,
        WireOutputMode::Collect(collect) => {
            crate::subprocess::OutputMode::Collect(crate::subprocess::SubprocessCollect {
                max_bytes: collect.max_bytes,
                spill: collect.spill,
            })
        }
    }
}

/// The foreground projection on the wire (the dsh tool's
/// `canonicalBashResult` DTO; camelCase is the dsh tool-output
/// vocabulary and rides the protocol exemption).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireRunResult {
    /// Exit code.
    pub exit_code: Option<i32>,
    /// Terminating signal.
    pub signal: Option<String>,
    /// First-cause timeout classification.
    pub timed_out: bool,
    /// First-cause cancellation classification.
    pub aborted: bool,
    /// The effective timeout.
    pub timeout_ms: f64,
    /// Captured stdout.
    pub stdout: WireCollected,
    /// Captured stderr.
    pub stderr: WireCollected,
    /// Sandbox execution facts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<WireSandboxInfo>,
}

/// One captured stream on the wire (dsh `CollectedOutput`).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireCollected {
    /// Stream text (the tail when truncated).
    pub text: String,
    /// Whether bytes were dropped.
    pub truncated: bool,
    /// The complete-stream file, when one remains.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spill_path: Option<String>,
}

/// Sandbox facts on the wire (dsh `ShellSandboxInfo`).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireSandboxInfo {
    /// The mode the command ran under.
    pub mode: SandboxMode,
    /// Whether a file operation was denied.
    pub denied: bool,
    /// Enforcement completeness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforcement: Option<fr_sandbox::seam::SandboxEnforcement>,
    /// Whether the runner failed before the command could run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_failed: Option<bool>,
}

impl From<ShellRunResult> for WireRunResult {
    fn from(result: ShellRunResult) -> Self {
        Self {
            exit_code: result.exit_code,
            signal: result.signal,
            timed_out: result.timed_out,
            aborted: result.aborted,
            timeout_ms: result.timeout_ms,
            stdout: wire_collected(result.stdout),
            stderr: wire_collected(result.stderr),
            sandbox: result.sandbox.map(|sandbox| WireSandboxInfo {
                mode: sandbox.mode,
                denied: sandbox.denied,
                enforcement: sandbox.enforcement,
                runner_failed: sandbox.runner_failed,
            }),
        }
    }
}

/// Lowers one captured stream onto the wire.
fn wire_collected(stream: crate::subprocess::CollectedOutput) -> WireCollected {
    WireCollected {
        text: stream.text,
        truncated: stream.truncated,
        spill_path: stream.spill_path,
    }
}

/// The one-shot completion oracle behind the registry hooks' `done` (the
/// dsh `Promise<JobOutcome>` in slot shape: the producer's run thread
/// stores, a late reader waits).
#[derive(Debug, Default)]
struct JobOutcomeSlot {
    outcome: Mutex<Option<crate::jobs_types::JobOutcome>>,
    ready: std::sync::Condvar,
}

impl JobOutcomeSlot {
    /// Stores the terminal outcome and wakes readers.
    fn store(&self, outcome: crate::jobs_types::JobOutcome) {
        let mut slot = recover(&self.outcome);
        *slot = Some(outcome);
        drop(slot);
        self.ready.notify_all();
    }

    /// Waits for and returns the terminal outcome (the never-rejecting
    /// done projection).
    fn load(&self) -> crate::jobs_types::JobOutcome {
        let mut slot = recover(&self.outcome);
        while slot.is_none() {
            slot = self
                .ready
                .wait(slot)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        slot.clone()
            .expect("the loop only exits when an outcome is stored")
    }
}

/// The absorbed process facts one started job keeps for the foreground
/// projection leg.
struct StartedJob {
    /// The slot the run watcher publishes the process into.
    slot: ProcSlot,
    /// The resolved workdir (the canonical result's `cwd`).
    cwd: String,
}

/// The three-band shell service: the registry, the subprocess handle
/// table, the per-kind executors, and the absorbed-process table.
pub struct ShellService {
    registry: Arc<LocalJobRegistry>,
    /// The controller attachment keeping `start` open (the dsh
    /// controller handshake; the M1 composition binds the registry and
    /// its controller in one artifact, so the dsh no-controller refusal
    /// text stays pinned but unreachable).
    _controller: ControllerGuard,
    subprocesses: Mutex<HashMap<String, Arc<SubprocessHandle>>>,
    executors: HashMap<ShellKind, Arc<dyn ShellExecutorFace>>,
    started: Mutex<HashMap<String, StartedJob>>,
    run_threads: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
}

impl std::fmt::Debug for ShellService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellService")
            .field("executors", &self.executors.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl ShellService {
    /// Assembles the service over the registry and the cross-plugin
    /// seams, mounting the sandbox executor variant for both kinds (the
    /// dsh bundle's standing mount; the local variants are the batch-2
    /// bundle-row override). The per-kind configurations ride the dsh
    /// bundle rows: bash carries `timeoutMs: 60000`, pwsh the package
    /// default.
    ///
    /// # Errors
    ///
    /// The default-mode read failure.
    pub fn new(
        registry: Arc<LocalJobRegistry>,
        confine: Arc<dyn SandboxConfine>,
        policy: Arc<dyn PolicyResolve>,
        configs: executors::ExecutorConfigs,
    ) -> Result<Self, String> {
        let executors = [ShellKind::Bash, ShellKind::Pwsh]
            .into_iter()
            .map(move |kind| {
                let config = if kind == ShellKind::Bash {
                    configs.bash.clone()
                } else {
                    configs.pwsh.clone()
                };
                let local = LocalExecutor::new(kind, config);
                let sandbox =
                    SandboxExecutor::new(local, Arc::clone(&confine), Arc::clone(&policy))?;
                Ok::<_, String>((kind, Arc::new(sandbox) as Arc<dyn ShellExecutorFace>))
            })
            .collect::<Result<HashMap<_, _>, String>>()?;
        let controller = registry.attach_controller();
        Ok(Self {
            registry,
            _controller: controller,
            subprocesses: Mutex::new(HashMap::new()),
            executors,
            started: Mutex::new(HashMap::new()),
            run_threads: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// The hosted registry (tests drive the jobs face directly).
    #[must_use]
    pub const fn registry(&self) -> &Arc<LocalJobRegistry> {
        &self.registry
    }

    /// The registered executor for one kind.
    #[must_use]
    pub fn executor(&self, kind: ShellKind) -> Option<&Arc<dyn ShellExecutorFace>> {
        self.executors.get(&kind)
    }

    /// Registers the command as a job and spawns it (the absorbed
    /// `startJob`): the job's output sources bind the process's observed
    /// readers lazily, the run watcher spawns inside the starter and
    /// settles the job with the process outcome, and the cancel leg
    /// aborts the controller and terminates the range.
    ///
    /// # Errors
    ///
    /// The registry's refusal family (admission, validation), an
    /// unmounted kind, or a failing run-thread spawn.
    pub fn start(
        &self,
        kind: ShellKind,
        spec: ShellExecSpec,
        command: String,
        owner: Option<String>,
        tool_call: Option<JobToolCall>,
        escalation_modes: Vec<SandboxMode>,
    ) -> Result<String, String> {
        let executor = Arc::clone(
            self.executors
                .get(&kind)
                .ok_or_else(|| format!("shell kind {} is not mounted", kind.as_str()))?,
        );
        let slot: ProcSlot = Arc::new(Mutex::new(None));
        let table_slot = Arc::clone(&slot);
        let controller = AbortSignal::new();
        let registry = Arc::clone(&self.registry);
        let run_threads = Arc::clone(&self.run_threads);
        let cwd = spec.workdir.clone();
        let job_spec = JobSpec {
            kind: kind.job_kind().to_owned(),
            label: command,
            owner,
            tool_call,
            output_limit_bytes: None,
            output: process_sources(&slot),
            run: Box::new(move |handle| {
                let job_id = handle.id().to_owned();
                let outcome_slot = Arc::new(JobOutcomeSlot::default());
                let run_registry = Arc::clone(&registry);
                let run_executor = Arc::clone(&executor);
                let run_slot = Arc::clone(&slot);
                let run_controller = Arc::clone(&controller);
                let run_outcome = Arc::clone(&outcome_slot);
                let run_threads = Arc::clone(&run_threads);
                let mut run_spec = spec;
                run_spec.signal = Some(Arc::clone(&run_controller));
                let join = std::thread::Builder::new()
                    .name("fr-shell-job-run".to_owned())
                    .spawn(move || {
                        let outcome = match run_executor.execute(&run_spec) {
                            Ok(process) => {
                                *recover(&run_slot) = Some(Arc::clone(&process));
                                if run_controller.is_aborted() {
                                    process.kill();
                                }
                                process.done();
                                process.process_outcome(&escalation_modes)
                            }
                            Err(error) => {
                                let orphaned =
                                    run_controller.is_aborted() && recover(&run_slot).is_none();
                                crate::jobs_types::JobOutcome {
                                    status: if orphaned {
                                        crate::jobs_types::JobStatus::Killed
                                    } else {
                                        crate::jobs_types::JobStatus::Failed
                                    },
                                    detail: Some(error),
                                    result: None,
                                }
                            }
                        };
                        run_outcome.store(outcome.clone());
                        run_registry.producer_settled(&job_id, outcome, true);
                    })
                    .map_err(|error| format!("job run thread spawn failed: {error}"))?;
                {
                    let mut threads = recover(&run_threads);
                    threads.retain(|thread| !thread.is_finished());
                    threads.push(join);
                }
                Ok(JobHooks {
                    cancel: Box::new(move |reason: Option<&str>| {
                        if !controller.is_aborted() {
                            match reason {
                                Some(reason) => controller.abort(reason),
                                None => controller.abort("aborted"),
                            }
                        }
                        if let Some(process) = recover(&slot).as_ref() {
                            process.kill();
                        }
                        Ok(())
                    }),
                    done: Box::new(move || outcome_slot.load()),
                })
            }),
        };
        let job_id = self.registry.start(job_spec)?;
        self.sweep_started();
        self.started
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                job_id.clone(),
                StartedJob {
                    slot: table_slot,
                    cwd,
                },
            );
        Ok(job_id)
    }

    /// Drops the absorbed-process entries whose job records left the
    /// registry (the dsh tool-closure lifetime; the table never
    /// outlives its job).
    fn sweep_started(&self) {
        let mut started = recover(&self.started);
        started.retain(|id, _| self.registry.contains(id));
    }

    /// The foreground projection of one absorbed job's process (the dsh
    /// `process.result()`; the caller invokes it after the job settled,
    /// so the read is fail-fast rather than blocking).
    ///
    /// # Errors
    ///
    /// An unknown job, an unsettled process, or the projection's own
    /// failure (infrastructure or sandbox-unavailable).
    pub fn process_result(&self, job_id: &str) -> Result<Value, String> {
        self.sweep_started();
        let entry = {
            let started = recover(&self.started);
            started
                .get(job_id)
                .map(|entry| (Arc::clone(&entry.slot), entry.cwd.clone()))
        }
        .ok_or_else(|| format!("unknown job {job_id}"))?;
        let process = {
            let slot = recover(&entry.0);
            slot.as_ref().map(Arc::clone)
        }
        .ok_or_else(|| format!("job {job_id} process did not start"))?;
        if !process.is_done() {
            return Err(format!("job {job_id} has not settled"));
        }
        let result: ShellRunResult = process.result()?;
        serde_json::to_value(WireRunResult::from(result))
            .map_err(|error| format!("shell result reply encode failed: {error}"))
    }

    /// Tears the service down: kills live subprocess ranges, cancels and
    /// awaits every job, then joins the run threads (the dsh
    /// composition-teardown shape).
    pub fn dispose(&self) {
        let handles: Vec<Arc<SubprocessHandle>> = {
            let mut table = recover(&self.subprocesses);
            table.drain().map(|(_, handle)| handle).collect()
        };
        for handle in handles {
            handle.terminate();
            let _ignored = handle.wait_for_exit(None);
        }
        self.registry.dispose_all();
        let threads: Vec<std::thread::JoinHandle<()>> = {
            let mut run = recover(&self.run_threads);
            run.drain(..).collect()
        };
        for thread in threads {
            let _ = thread.join();
        }
    }
}

/// Locks one mutex, recovering from poisoning (the fr-tools `recover`
/// judgment).
fn recover<T>(lock: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    lock.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The registered-command arguments on the start wire (dsh passes the
/// command as both the job label and the executed source).
#[derive(Debug, Deserialize)]
pub struct WireStartArgs {
    /// The command source.
    pub command: String,
}

/// The start request on the wire.
#[derive(Debug, Deserialize)]
pub struct WireStart {
    /// Which executor family runs the command.
    pub shell: ShellKind,
    /// The resolved spec (the resolve face's reply, round-tripped).
    pub spec: ShellExecSpec,
    /// The command arguments.
    pub args: WireStartArgs,
    /// The owning session id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// The creating tool invocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<JobToolCall>,
    /// The escalation targets this composition advertises (gates the
    /// denial hint in the terminal notes).
    #[serde(default)]
    pub escalation_modes: Vec<SandboxMode>,
}

impl ShellService {
    /// Serves one method against the three-band face: the low band
    /// routes to `'subprocess'`, the 1000+ band to `'jobs'`, and the
    /// 2000+ band to `'shell'` (one export table, the band judgment).
    ///
    /// # Errors
    ///
    /// The payload decode refusal, an unknown method, or the handler's
    /// own failure.
    pub fn serve(&self, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
        let request: Value = if payload.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(payload)
                .map_err(|error| format!("shell service payload must be JSON: {error}"))?
        };
        let reply = if method < fr_wire::services::jobs::METHOD_DESCRIBE {
            self.serve_subprocess(method, &request)?
        } else if method < fr_wire::services::shell::METHOD_DESCRIBE {
            self.serve_jobs(method, &request)?
        } else {
            self.serve_shell(method, &request)?
        };
        serde_json::to_vec(&reply)
            .map_err(|error| format!("shell service reply encode failed: {error}"))
    }

    /// Serves the `'subprocess'` low band.
    fn serve_subprocess(&self, method: u32, request: &Value) -> Result<Value, String> {
        match method {
            0 => Ok(json!({
                "plugin": crate::plugin_shell::PLUGIN_NAME,
                "service": "subprocess",
                "methods": ["describe", "spawn", "read", "wait", "terminate", "resolveExecutable"],
            })),
            1 => {
                let spec: WireSpawnSpec = serde_json::from_value(request.clone())
                    .map_err(|error| format!("subprocess spawn payload decode failed: {error}"))?;
                let handle = crate::subprocess::spawn(&spec.into_domain(), None)?;
                let id = uuid::Uuid::new_v4().simple().to_string();
                self.subprocesses
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(id.clone(), Arc::new(handle));
                Ok(json!({ "handle": id }))
            }
            2 => {
                let (handle, stream, from) = stream_request(request)?;
                let entry = {
                    let table = self
                        .subprocesses
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    table.get(&handle).map(Arc::clone)
                }
                .ok_or_else(|| format!("unknown subprocess handle {handle}"))?;
                let read = match stream.as_str() {
                    "stdout" => entry
                        .stdout_collector()
                        .ok_or_else(|| format!("handle {handle} has no collected stdout stream"))?
                        .read_from(from),
                    "stderr" => entry
                        .stderr_collector()
                        .ok_or_else(|| format!("handle {handle} has no collected stderr stream"))?
                        .read_from(from),
                    other => return Err(format!("unknown subprocess stream {other}")),
                };
                serde_json::to_value(read)
                    .map_err(|error| format!("subprocess read reply encode failed: {error}"))
            }
            3 => {
                let handle = require_str(request, "handle", "subprocess wait")?;
                let timeout_ms = request
                    .get("timeout_ms")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| "subprocess wait payload requires timeout_ms".to_owned())?;
                if !timeout_ms.is_finite() || timeout_ms <= 0.0 {
                    return Err(format!(
                        "invalid wait timeout: expected a positive number of milliseconds, got {}",
                        js_number(timeout_ms)
                    ));
                }
                let entry = {
                    let table = self
                        .subprocesses
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    table.get(&handle).map(Arc::clone)
                }
                .ok_or_else(|| format!("unknown subprocess handle {handle}"))?;
                let settled = entry
                    .wait_for_exit(Some(Instant::now() + Duration::from_millis(ms(timeout_ms))));
                if !settled {
                    return Ok(json!({ "settled": false }));
                }
                Ok(outcome_message(entry.settled_outcome()))
            }
            4 => {
                let handle = require_str(request, "handle", "subprocess terminate")?;
                let entry = {
                    let table = self
                        .subprocesses
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    table.get(&handle).map(Arc::clone)
                }
                .ok_or_else(|| format!("unknown subprocess handle {handle}"))?;
                entry.terminate();
                Ok(json!({}))
            }
            5 => {
                let command = require_str(request, "command", "subprocess resolveExecutable")?;
                let path = crate::subprocess::resolve_executable(&command)?;
                Ok(json!({ "path": path }))
            }
            other => Err(format!("unknown subprocess method {other}")),
        }
    }

    /// Serves the `'jobs'` 1000+ band (the registry's fence and
    /// validation texts ride through verbatim).
    fn serve_jobs(&self, method: u32, request: &Value) -> Result<Value, String> {
        match method {
            1000 => Ok(json!({
                "plugin": crate::plugin_shell::PLUGIN_NAME,
                "service": "jobs",
                "methods": [
                    "describe", "list", "get", "read", "readAt", "kill", "wait", "remove",
                    "killByToolCall",
                ],
            })),
            1001 => {
                let caller = opt_str(request, "caller");
                let jobs = self.registry.list(caller.as_deref());
                serde_json::to_value(jobs)
                    .map(|jobs| json!({ "jobs": jobs }))
                    .map_err(|error| format!("jobs reply encode failed: {error}"))
            }
            1002 => {
                let (id, caller) = job_request(request)?;
                let view = self.registry.get(&id, caller.as_deref())?;
                serde_json::to_value(view)
                    .map_err(|error| format!("jobs reply encode failed: {error}"))
            }
            1003 => {
                let (id, caller) = job_request(request)?;
                let read = self.registry.read(&id, caller.as_deref())?;
                serde_json::to_value(read)
                    .map_err(|error| format!("jobs reply encode failed: {error}"))
            }
            1004 => {
                let id = require_str(request, "id", "jobs readAt")?;
                let caller = opt_str(request, "caller");
                let from = request
                    .get("from")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| "jobs readAt payload requires from".to_owned())?;
                let read = self.registry.read_at(&id, from, caller.as_deref())?;
                serde_json::to_value(read)
                    .map_err(|error| format!("jobs reply encode failed: {error}"))
            }
            1005 => {
                let id = require_str(request, "id", "jobs kill")?;
                let caller = opt_str(request, "caller");
                let reason = opt_str(request, "reason");
                let result = self
                    .registry
                    .kill(&id, caller.as_deref(), reason.as_deref())?;
                Ok(json!({ "result": result }))
            }
            1006 => {
                let id = require_str(request, "id", "jobs wait")?;
                let caller = opt_str(request, "caller");
                let timeout_ms = request
                    .get("timeout_ms")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| "jobs wait payload requires timeout_ms".to_owned())?;
                let view = self.registry.wait(&id, timeout_ms, caller.as_deref())?;
                serde_json::to_value(view)
                    .map_err(|error| format!("jobs reply encode failed: {error}"))
            }
            1007 => {
                let (id, caller) = job_request(request)?;
                self.registry.remove(&id, caller.as_deref())?;
                Ok(json!({}))
            }
            1008 => {
                let call_id = require_str(request, "call_id", "jobs killByToolCall")?;
                let reason = opt_str(request, "reason");
                let result = self
                    .registry
                    .kill_by_tool_call(&call_id, reason.as_deref())?;
                Ok(json!({ "result": result }))
            }
            other => Err(format!("unknown jobs method {other}")),
        }
    }

    /// Serves the `'shell'` 2000+ band.
    fn serve_shell(&self, method: u32, request: &Value) -> Result<Value, String> {
        match method {
            2000 => Ok(json!({
                "plugin": crate::plugin_shell::PLUGIN_NAME,
                "service": "shell",
                "methods": ["describe", "sandboxMode", "resolve", "start", "result"],
            })),
            2001 => {
                let kind = shell_kind(request)?;
                let mode = self
                    .executors
                    .get(&kind)
                    .ok_or_else(|| format!("shell kind {} is not mounted", kind.as_str()))?
                    .sandbox_mode();
                Ok(json!({ "mode": mode }))
            }
            2002 => {
                let kind = shell_kind(request)?;
                let executor = self
                    .executors
                    .get(&kind)
                    .ok_or_else(|| format!("shell kind {} is not mounted", kind.as_str()))?;
                let request_value = request
                    .get("request")
                    .cloned()
                    .ok_or_else(|| "shell resolve payload requires request".to_owned())?;
                let parsed: ShellExecRequest = serde_json::from_value(request_value)
                    .map_err(|error| format!("shell resolve request decode failed: {error}"))?;
                let spec = executor.resolve(parsed)?;
                serde_json::to_value(spec)
                    .map(|spec| json!({ "spec": spec }))
                    .map_err(|error| format!("shell reply encode failed: {error}"))
            }
            2003 => {
                let start: WireStart = serde_json::from_value(request.clone())
                    .map_err(|error| format!("shell start payload decode failed: {error}"))?;
                let job_id = self.start(
                    start.shell,
                    start.spec,
                    start.args.command,
                    start.owner,
                    start.tool_call,
                    start.escalation_modes,
                )?;
                let workdir = self
                    .started
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&job_id)
                    .map(|entry| entry.cwd.clone())
                    .unwrap_or_default();
                Ok(json!({ "job_id": job_id, "workdir": workdir }))
            }
            2004 => {
                let job_id = require_str(request, "job_id", "shell result")?;
                self.process_result(&job_id)
            }
            other => Err(format!("unknown shell method {other}")),
        }
    }
}

/// Reads the shell-kind selector (the dsh single `ctx.shell` slot's
/// per-kind resolution; absent selects bash).
fn shell_kind(request: &Value) -> Result<ShellKind, String> {
    match request.get("shell") {
        None | Some(Value::Null) => Ok(ShellKind::Bash),
        Some(word) => ShellKind::parse(word.as_str().unwrap_or_default())
            .ok_or_else(|| format!("unknown shell kind {word}")),
    }
}

/// Reads the common `{id, caller?}` job request.
fn job_request(request: &Value) -> Result<(String, Option<String>), String> {
    let id = require_str(request, "id", "jobs")?;
    let caller = opt_str(request, "caller");
    Ok((id, caller))
}

/// Reads one required string field.
fn require_str(request: &Value, key: &str, face: &str) -> Result<String, String> {
    request
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{face} payload requires {key}"))
}

/// Reads one optional string field.
fn opt_str(request: &Value, key: &str) -> Option<String> {
    request.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Reads the `{handle, stream, from}` read request.
fn stream_request(request: &Value) -> Result<(String, String, u64), String> {
    let handle = require_str(request, "handle", "subprocess read")?;
    let stream = require_str(request, "stream", "subprocess read")?;
    let from = request
        .get("from")
        .and_then(Value::as_u64)
        .ok_or_else(|| "subprocess read payload requires from".to_owned())?;
    Ok((handle, stream, from))
}

/// The wait reply: settled carries the exit facts, unsettled does not.
fn outcome_message(outcome: Option<Outcome>) -> Value {
    match outcome {
        Some(outcome) => {
            let mut message = json!({ "settled": true });
            let object = message.as_object_mut().expect("a freshly built object");
            if let Some(exit_code) = outcome.exit_code {
                object.insert("exit_code".to_owned(), json!(exit_code));
            }
            if let Some(signal) = outcome.signal {
                object.insert("signal".to_owned(), json!(signal));
            }
            message
        }
        None => json!({ "settled": false }),
    }
}

/// The validated positive-millisecond cast (the jobs-local judgment).
#[expect(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "the handler validated the timeout as a positive finite number before the cast"
)]
#[expect(clippy::missing_const_for_fn, reason = "f64::max is not a const fn")]
fn ms(timeout_ms: f64) -> u64 {
    timeout_ms.max(0.0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs_local::{RegistryConfig, SessionLogAppend};
    use fr_sandbox::seam::{
        ConfinedArgv, SandboxEnforcement, SandboxExecutionPolicy, SandboxPolicy,
    };

    /// One fake session log recording the durable lifecycle records.
    #[derive(Debug, Default)]
    struct FakeLog {
        records: Mutex<Vec<(String, String, Value)>>,
    }

    impl SessionLogAppend for FakeLog {
        fn append(&self, session: &str, event_type: &str, data: &Value) -> Result<(), String> {
            self.records.lock().expect("lock").push((
                session.to_owned(),
                event_type.to_owned(),
                data.clone(),
            ));
            Ok(())
        }
    }

    /// The danger policy: the executor runs direct and decorates.
    #[derive(Debug)]
    struct DangerPolicy;

    impl PolicyResolve for DangerPolicy {
        fn resolve_policy(&self) -> Result<SandboxExecutionPolicy, String> {
            Ok(SandboxExecutionPolicy {
                mode: fr_sandbox::seam::SandboxMode::DangerFullAccess,
                workspace_root: "/ws".to_owned(),
                session_id: None,
            })
        }

        fn default_mode(&self) -> Result<SandboxMode, String> {
            Ok(fr_sandbox::seam::SandboxMode::DangerFullAccess)
        }
    }

    /// The confined policy with a scripted confinement wrap.
    #[derive(Debug)]
    struct ConfinedPolicy {
        argv: Vec<String>,
        signatures: Vec<String>,
    }

    impl PolicyResolve for ConfinedPolicy {
        fn resolve_policy(&self) -> Result<SandboxExecutionPolicy, String> {
            Ok(SandboxExecutionPolicy {
                mode: fr_sandbox::seam::SandboxMode::ReadOnly,
                workspace_root: "/ws".to_owned(),
                session_id: None,
            })
        }

        fn default_mode(&self) -> Result<SandboxMode, String> {
            Ok(fr_sandbox::seam::SandboxMode::ReadOnly)
        }
    }

    impl SandboxConfine for ConfinedPolicy {
        fn confine(
            &self,
            _argv: &[String],
            _policy: &SandboxPolicy,
        ) -> Result<ConfinedArgv, String> {
            Ok(ConfinedArgv {
                argv: self.argv.clone(),
                enforcement: SandboxEnforcement::Full,
                denial_signatures: self.signatures.clone(),
                runner_failure_rules: Vec::new(),
            })
        }
    }

    fn danger_service() -> ShellService {
        let registry =
            LocalJobRegistry::with_config(Arc::new(FakeLog::default()), RegistryConfig::default());
        ShellService::new(
            registry,
            Arc::new(ConfinedPolicy {
                argv: Vec::new(),
                signatures: Vec::new(),
            }),
            Arc::new(DangerPolicy),
            executors::ExecutorConfigs::default_configs(),
        )
        .expect("service")
    }

    fn resolve_request(command: &str) -> Value {
        json!({
            "shell": "bash",
            "request": { "command": command, "workdir": std::env::temp_dir().to_string_lossy() },
        })
    }

    /// The absorbed start registers a job whose pump drains the process
    /// streams; the terminal detail reports the exit and the result leg
    /// projects the canonical split.
    #[test]
    fn start_pumps_and_settles() {
        let service = danger_service();
        let reply: Value = serde_json::from_slice(
            &service
                .serve(
                    2002,
                    &serde_json::to_vec(&resolve_request("echo out; echo err >&2"))
                        .expect("encode"),
                )
                .expect("resolve"),
        )
        .expect("decode");
        let spec = reply["spec"].clone();
        let started: Value = serde_json::from_slice(
            &service
                .serve(
                    2003,
                    &serde_json::to_vec(&json!({
                        "shell": "bash",
                        "spec": spec,
                        "args": { "command": "echo out; echo err >&2" },
                        "owner": "s1",
                        "tool_call": { "call_id": "c1", "root_call_id": "c1" },
                    }))
                    .expect("encode"),
                )
                .expect("start"),
        )
        .expect("decode");
        let job_id = started["job_id"].as_str().expect("id").to_owned();
        assert!(started["workdir"].as_str().is_some());

        // The job settles promptly; the consuming read merges the
        // stderr section after stdout.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let view: Value = serde_json::from_slice(
                &service
                    .serve(
                        1002,
                        &serde_json::to_vec(&json!({ "id": job_id, "caller": "s1" }))
                            .expect("encode"),
                    )
                    .expect("get"),
            )
            .expect("decode");
            if view["status"] == "completed" {
                assert_eq!(view["detail"], "exit code: 0");
                break;
            }
            assert!(Instant::now() < deadline, "the job never settled");
            std::thread::sleep(Duration::from_millis(20));
        }
        let read: Value = serde_json::from_slice(
            &service
                .serve(
                    1003,
                    &serde_json::to_vec(&json!({ "id": job_id, "caller": "s1" })).expect("encode"),
                )
                .expect("read"),
        )
        .expect("decode");
        let chunks = read["chunks"].as_array().expect("chunks");
        let stdout_text: String = chunks
            .iter()
            .filter(|chunk| chunk["channel"] == "stdout")
            .map(|chunk| chunk["text"].as_str().unwrap_or_default())
            .collect();
        let stderr_text: String = chunks
            .iter()
            .filter(|chunk| chunk["channel"] == "stderr")
            .map(|chunk| chunk["text"].as_str().unwrap_or_default())
            .collect();
        assert!(stdout_text.starts_with("out"), "got {stdout_text:?}");
        assert!(stderr_text.starts_with("err"), "got {stderr_text:?}");
        assert_eq!(read["job"]["detail"], "exit code: 0");

        // The result leg projects the canonical foreground shape.
        let result: Value = serde_json::from_slice(
            &service
                .serve(
                    2004,
                    &serde_json::to_vec(&json!({ "job_id": job_id })).expect("encode"),
                )
                .expect("result"),
        )
        .expect("decode");
        assert_eq!(result["exitCode"], 0);
        assert_eq!(result["timedOut"], false);
        assert!(
            result["stdout"]["text"]
                .as_str()
                .expect("stdout")
                .starts_with("out")
        );
    }

    /// A kill settles the job killed and merges the recorded reason
    /// behind the process facts.
    #[test]
    fn kill_merges_the_recorded_reason() {
        let service = danger_service();
        let reply: Value = serde_json::from_slice(
            &service
                .serve(
                    2002,
                    &serde_json::to_vec(&resolve_request("sleep 30")).expect("encode"),
                )
                .expect("resolve"),
        )
        .expect("decode");
        let started: Value = serde_json::from_slice(
            &service
                .serve(
                    2003,
                    &serde_json::to_vec(&json!({
                        "shell": "bash",
                        "spec": reply["spec"],
                        "args": { "command": "sleep 30" },
                        "owner": "s1",
                    }))
                    .expect("encode"),
                )
                .expect("start"),
        )
        .expect("decode");
        let job_id = started["job_id"].as_str().expect("id").to_owned();
        // The kill must land on a live process: wait until the result
        // face stops reporting the not-yet-started refusal (the absorbed
        // process publishes only after the spawn returns; a kill that
        // wins that race settles the job without any signal fact).
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let live = match service.serve(
                2004,
                &serde_json::to_vec(&json!({ "job_id": job_id })).expect("encode"),
            ) {
                Err(error) => !error.contains("process did not start"),
                Ok(_) => true,
            };
            if live {
                break;
            }
            assert!(Instant::now() < deadline, "the job process never started");
            std::thread::sleep(Duration::from_millis(5));
        }
        let killed: Value = serde_json::from_slice(
            &service
                .serve(
                    1005,
                    &serde_json::to_vec(&json!({
                        "id": job_id, "caller": "s1", "reason": "tool call aborted",
                    }))
                    .expect("encode"),
                )
                .expect("kill"),
        )
        .expect("decode");
        assert_eq!(killed["result"], "requested");
        let view: Value = serde_json::from_slice(
            &service
                .serve(
                    1006,
                    &serde_json::to_vec(
                        &json!({ "id": job_id, "caller": "s1", "timeout_ms": 5000.0 }),
                    )
                    .expect("encode"),
                )
                .expect("wait"),
        )
        .expect("decode");
        assert_eq!(view["status"], "killed");
        assert_eq!(view["detail"], "signal: SIGTERM; tool call aborted");
    }

    /// A confined denial carries the marker notes; the escalation hint
    /// rides only when the composition advertises targets.
    #[test]
    fn denial_notes_and_escalation_hint_gate() {
        let registry = LocalJobRegistry::new(Arc::new(FakeLog::default()));
        let service = ShellService::new(
            registry,
            Arc::new(ConfinedPolicy {
                argv: vec![
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    "echo 'permission denied' >&2; exit 1".to_owned(),
                ],
                signatures: vec!["permission denied".to_owned()],
            }),
            Arc::new(ConfinedPolicy {
                argv: Vec::new(),
                signatures: Vec::new(),
            }),
            executors::ExecutorConfigs::default_configs(),
        )
        .expect("service");
        let resolve: Value = serde_json::from_slice(
            &service
                .serve(
                    2002,
                    &serde_json::to_vec(&resolve_request("echo blocked")).expect("encode"),
                )
                .expect("resolve"),
        )
        .expect("decode");
        let started: Value = serde_json::from_slice(
            &service
                .serve(
                    2003,
                    &serde_json::to_vec(&json!({
                        "shell": "bash",
                        "spec": resolve["spec"],
                        "args": { "command": "echo blocked" },
                        "escalation_modes": ["workspace-write"],
                    }))
                    .expect("encode"),
                )
                .expect("start"),
        )
        .expect("decode");
        let job_id = started["job_id"].as_str().expect("id").to_owned();
        let deadline = Instant::now() + Duration::from_secs(5);
        let detail = loop {
            let view: Value = serde_json::from_slice(
                &service
                    .serve(
                        1002,
                        &serde_json::to_vec(&json!({ "id": job_id })).expect("encode"),
                    )
                    .expect("get"),
            )
            .expect("decode");
            if view["status"] == "completed" {
                break view["detail"].as_str().expect("detail").to_owned();
            }
            assert!(Instant::now() < deadline, "the job never settled");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(
            detail,
            "exit code: 1; [sandbox: file access denied under read-only mode] [sandbox: escalation available — retry this exact command once with sandbox_permissions (the narrowest wider mode that suffices) + justification; the approval prompt asks the user]"
        );
    }

    /// The capability fact face reads the mounted executor's default
    /// mode; an unknown kind refuses.
    #[test]
    fn sandbox_mode_face_and_unknown_kind() {
        let service = danger_service();
        let mode: Value = serde_json::from_slice(
            &service
                .serve(
                    2001,
                    &serde_json::to_vec(&json!({ "shell": "bash" })).expect("encode"),
                )
                .expect("mode"),
        )
        .expect("decode");
        assert_eq!(mode["mode"], "danger-full-access");
        let error = service
            .serve(2001, br#"{"shell": "fish"}"#.as_slice())
            .unwrap_err();
        assert!(error.contains("unknown shell kind"), "got {error}");
    }

    /// The subprocess low band spawns, waits, and reads one real child.
    #[test]
    fn subprocess_face_round_trip() {
        let service = danger_service();
        let spawned: Value = serde_json::from_slice(
            &service
                .serve(
                    1,
                    &serde_json::to_vec(&json!({
                        "argv": ["/bin/sh", "-c", "echo hi"],
                        "cwd": std::env::temp_dir().to_string_lossy(),
                        "stdio": {
                            "stdin": "ignore",
                            "stdout": { "max_bytes": 4096 },
                            "stderr": { "max_bytes": 4096 },
                        },
                        "grace_ms": 1000.0,
                    }))
                    .expect("encode"),
                )
                .expect("spawn"),
        )
        .expect("decode");
        let handle = spawned["handle"].as_str().expect("handle").to_owned();
        let waited: Value = serde_json::from_slice(
            &service
                .serve(
                    3,
                    &serde_json::to_vec(&json!({ "handle": handle, "timeout_ms": 5000.0 }))
                        .expect("encode"),
                )
                .expect("wait"),
        )
        .expect("decode");
        assert_eq!(waited["settled"], true);
        assert_eq!(waited["exit_code"], 0);
        let read: Value = serde_json::from_slice(
            &service
                .serve(
                    2,
                    &serde_json::to_vec(
                        &json!({ "handle": handle, "stream": "stdout", "from": 0 }),
                    )
                    .expect("encode"),
                )
                .expect("read"),
        )
        .expect("decode");
        assert_eq!(read["text"], "hi\n");
        let executable: Value = serde_json::from_slice(
            &service
                .serve(5, br#"{"command": "sh"}"#.as_slice())
                .expect("resolve"),
        )
        .expect("decode");
        assert!(executable["path"].as_str().expect("path").ends_with("sh"));
    }

    /// The registry's fence and removal texts ride the jobs face
    /// verbatim.
    #[test]
    fn jobs_face_refusals_pin_the_texts() {
        let service = danger_service();
        assert_eq!(
            service
                .serve(1002, br#"{"id": "bash-nope"}"#.as_slice())
                .unwrap_err(),
            "unknown job bash-nope"
        );
        // A foreign caller cannot read another session's job, and a
        // live job refuses removal.
        let resolve: Value = serde_json::from_slice(
            &service
                .serve(
                    2002,
                    &serde_json::to_vec(&resolve_request("sleep 30")).expect("encode"),
                )
                .expect("resolve"),
        )
        .expect("decode");
        let started: Value = serde_json::from_slice(
            &service
                .serve(
                    2003,
                    &serde_json::to_vec(&json!({
                        "shell": "bash",
                        "spec": resolve["spec"],
                        "args": { "command": "sleep 30" },
                        "owner": "s1",
                    }))
                    .expect("encode"),
                )
                .expect("start"),
        )
        .expect("decode");
        let job_id = started["job_id"].as_str().expect("id").to_owned();
        assert_eq!(
            service
                .serve(
                    1002,
                    &serde_json::to_vec(&json!({ "id": job_id, "caller": "s2" })).expect("encode")
                )
                .unwrap_err(),
            format!("job {job_id} belongs to another session")
        );
        assert_eq!(
            service
                .serve(
                    1007,
                    &serde_json::to_vec(&json!({ "id": job_id, "caller": "s1" })).expect("encode")
                )
                .unwrap_err(),
            format!(
                "job {job_id} is still running; kill it and wait for settlement before removing it"
            )
        );
        service.dispose();
    }

    /// The caller-abort propagation face kills the job created by the
    /// named tool invocation; an unknown call id resolves idempotently.
    #[test]
    fn kill_by_tool_call_reaches_the_live_job() {
        let service = danger_service();
        let resolve: Value = serde_json::from_slice(
            &service
                .serve(
                    2002,
                    &serde_json::to_vec(&resolve_request("sleep 30")).expect("encode"),
                )
                .expect("resolve"),
        )
        .expect("decode");
        let started: Value = serde_json::from_slice(
            &service
                .serve(
                    2003,
                    &serde_json::to_vec(&json!({
                        "shell": "bash",
                        "spec": resolve["spec"],
                        "args": { "command": "sleep 30" },
                        "owner": "s1",
                        "tool_call": { "call_id": "c-abort", "root_call_id": "c-abort" },
                    }))
                    .expect("encode"),
                )
                .expect("start"),
        )
        .expect("decode");
        let job_id = started["job_id"].as_str().expect("id").to_owned();
        // The kill must land on a live process (the absorbed process
        // publishes after the spawn; see kill_merges_the_recorded_reason).
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let live = match service.serve(
                2004,
                &serde_json::to_vec(&json!({ "job_id": job_id })).expect("encode"),
            ) {
                Err(error) => !error.contains("process did not start"),
                Ok(_) => true,
            };
            if live {
                break;
            }
            assert!(Instant::now() < deadline, "the job process never started");
            std::thread::sleep(Duration::from_millis(5));
        }
        let killed: Value = serde_json::from_slice(
            &service
                .serve(
                    1008,
                    &serde_json::to_vec(&json!({
                        "call_id": "c-abort",
                        "reason": "tool call aborted",
                    }))
                    .expect("encode"),
                )
                .expect("kill by call"),
        )
        .expect("decode");
        assert_eq!(killed["result"], "requested");
        let view: Value = serde_json::from_slice(
            &service
                .serve(
                    1006,
                    &serde_json::to_vec(
                        &json!({ "id": job_id, "caller": "s1", "timeout_ms": 5000.0 }),
                    )
                    .expect("encode"),
                )
                .expect("wait"),
        )
        .expect("decode");
        assert_eq!(view["status"], "killed");
        let detail = view["detail"].as_str().expect("detail");
        assert!(detail.ends_with("tool call aborted"), "got {detail}");
        // An unknown call id is the expected lost race, not an error.
        let absent: Value = serde_json::from_slice(
            &service
                .serve(
                    1008,
                    &serde_json::to_vec(&json!({ "call_id": "c-gone" })).expect("encode"),
                )
                .expect("kill by call"),
        )
        .expect("decode");
        assert_eq!(absent["result"], "absent");
        service.dispose();
    }

    /// The bundle executor rows: the bash executor carries the
    /// `timeoutMs: 60000` config override; pwsh keeps the package
    /// default.
    #[test]
    fn bundle_rows_pin_the_per_kind_timeouts() {
        let service = ShellService::new(
            LocalJobRegistry::new(Arc::new(FakeLog::default())),
            Arc::new(ConfinedPolicy {
                argv: Vec::new(),
                signatures: Vec::new(),
            }),
            Arc::new(DangerPolicy),
            executors::ExecutorConfigs::bundle(),
        )
        .expect("service");
        let base =
            json!({ "command": "echo hi", "workdir": std::env::temp_dir().to_string_lossy() });
        let bash_reply: Value = serde_json::from_slice(
            &service
                .serve(
                    2002,
                    &serde_json::to_vec(&json!({
                        "shell": "bash",
                        "request": base,
                    }))
                    .expect("encode"),
                )
                .expect("resolve"),
        )
        .expect("decode");
        assert_eq!(bash_reply["spec"]["timeout_ms"], 60_000.0);
        let pwsh: Value = serde_json::from_slice(
            &service
                .serve(
                    2002,
                    &serde_json::to_vec(&json!({
                        "shell": "pwsh",
                        "request": base,
                    }))
                    .expect("encode"),
                )
                .expect("resolve"),
        )
        .expect("decode");
        assert_eq!(pwsh["spec"]["timeout_ms"], 120_000.0);
        service.dispose();
    }
}
