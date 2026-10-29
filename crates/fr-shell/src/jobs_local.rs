// Rust guideline compliant 2026-10-29
//! The in-memory background-job registry (dsh `jobs-local/src/index.ts`).
//!
//! Lifecycle state, the bounded output ring, and the model cursor stay in
//! memory; fresh projections and chunk copies are handed out, never live
//! state. M1 structural facts (the agents-registry absence): owned-job
//! access is fenced by the owner session id string only — dsh's live-agent
//! resolution (`resolveOwner`) and the owner-disposal cancel leg have no
//! M1 counterpart and are unreachable for tool-originated jobs (the
//! caller is the live session itself). Teardown cancels and awaits every
//! job. The dsh logger-warn shapes are dropped (the fr-sandbox teardown
//! precedent — no plugin logger seam in M1).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use uuid::Uuid;

use crate::jobs_types::JOB_REGISTERED_EVENT;
use crate::jobs_types::JOB_SETTLED_EVENT;
use crate::jobs_types::JobAppendOptions;
use crate::jobs_types::JobChannel;
use crate::jobs_types::JobOutcome;
use crate::jobs_types::JobOutputRead;
use crate::jobs_types::JobOutputSource;
use crate::jobs_types::JobRead;
use crate::jobs_types::JobRegisteredData;
use crate::jobs_types::JobSettleCause;
use crate::jobs_types::JobSettledData;
use crate::jobs_types::JobSourceRead;
use crate::jobs_types::JobStatus;
use crate::jobs_types::JobToolCall;
use crate::jobs_types::JobView;
use crate::pump::PumpSink;
use crate::pump::PumpStop;
use crate::pump::start_pump;
use crate::ring::OutputRing;

/// Default maximum number of active jobs in one exact-owner bucket (dsh
/// `DEFAULT_MAX_CONCURRENT_JOBS_PER_OWNER`).
pub const DEFAULT_MAX_CONCURRENT_JOBS_PER_OWNER: usize = 10;
/// Default live ring retention per job, in UTF-8 bytes.
pub const DEFAULT_RETAIN_BYTES: u64 = 256 * 1024;
/// Default ring retention kept after settlement, in UTF-8 bytes.
pub const DEFAULT_SETTLED_RETAIN_BYTES: u64 = 16 * 1024;
/// Default poll interval for pull sources, in milliseconds.
pub const DEFAULT_PUMP_POLL_MS: u64 = 150;

/// Registry configuration (dsh `Config`, schemastery defaults applied by
/// the caller).
#[derive(Debug, Clone)]
pub struct RegistryConfig {
    /// Maximum `running` plus `stopping` jobs per exact owner or in the
    /// shared unowned bucket.
    pub max_concurrent_jobs_per_owner: usize,
    /// Live ring retention per job in UTF-8 bytes.
    pub retain_bytes: u64,
    /// Ring retention kept after a job settles, in UTF-8 bytes.
    pub settled_retain_bytes: u64,
    /// Poll interval for a job's pull sources, in milliseconds.
    pub pump_poll_ms: u64,
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            max_concurrent_jobs_per_owner: DEFAULT_MAX_CONCURRENT_JOBS_PER_OWNER,
            retain_bytes: DEFAULT_RETAIN_BYTES,
            settled_retain_bytes: DEFAULT_SETTLED_RETAIN_BYTES,
            pump_poll_ms: DEFAULT_PUMP_POLL_MS,
        }
    }
}

/// The session-log seam for durable lifecycle records (the
/// `'session-persistence'` append face; a failure contains and never
/// blocks the registry — the dsh logger-warn shape).
pub trait SessionLogAppend: Send + Sync {
    /// Appends one plugin record to the owner session's log.
    ///
    /// # Errors
    ///
    /// Returns the transport failure; the registry contains it.
    fn append(
        &self,
        session: &str,
        event_type: &str,
        data: &serde_json::Value,
    ) -> Result<(), String>;
}

/// The producer face of one registered job (dsh `JobHandle`).
///
/// All methods are synchronous; writes staged inside the starter are
/// retained and become visible with the registration commit; after
/// settlement writes drop instead of failing.
pub struct JobHandle {
    registry: Arc<LocalJobRegistry>,
    id: String,
}

impl std::fmt::Debug for JobHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobHandle")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl JobHandle {
    /// The registry-issued id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Appends one chunk to the output ring.
    pub fn append(&self, text: &str, options: JobAppendOptions) {
        self.registry.producer_append(&self.id, text, options);
    }

    /// Replaces the live progress line; settlement clears it.
    pub fn update_progress(&self, line: &str) {
        self.registry.producer_progress(&self.id, line);
    }
}

/// Starts the producer's work after preflight and returns its hooks
/// (the dsh `JobSpec.run` shape).
pub type JobStarter = Box<dyn FnOnce(&JobHandle) -> Result<JobHooks, String> + Send>;

/// Requests termination for one producer; a failure propagates to the
/// killer without changing job state (the dsh `JobHooks.cancel` shape).
pub type ProducerCancel = Box<dyn Fn(Option<&str>) -> Result<(), String> + Send + Sync>;

/// Hooks through which the runtime controls and observes producer work
/// (dsh `JobHooks`).
pub struct JobHooks {
    /// Requests termination; must be idempotent. A failure propagates to
    /// the killer without changing job state.
    pub cancel: ProducerCancel,
    /// Resolves the producer's terminal outcome.
    pub done: Box<dyn FnOnce() -> JobOutcome + Send>,
}

impl std::fmt::Debug for JobHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The closures render as their static names (the hand-rolled
        // Debug judgment of fr-tools' DefineTool).
        f.debug_struct("JobHooks").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for JobSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The closures render as their static names (the hand-rolled
        // Debug judgment of fr-tools' DefineTool).
        f.debug_struct("JobSpec")
            .field("kind", &self.kind)
            .field("label", &self.label)
            .field("owner", &self.owner)
            .field("tool_call", &self.tool_call)
            .field("output_limit_bytes", &self.output_limit_bytes)
            .field("output_sources", &self.output.len())
            .finish_non_exhaustive()
    }
}

/// Producer declaration passed to [`LocalJobRegistry::start`] (dsh
/// `JobSpec`).
pub struct JobSpec {
    /// Producer kind — also the id prefix.
    pub kind: String,
    /// One-line model-facing label.
    pub label: String,
    /// Owning session id; access is fenced by it.
    pub owner: Option<String>,
    /// Creating tool invocation, retained in the durable records.
    pub tool_call: Option<JobToolCall>,
    /// Optional UTF-8 byte cap for each complete model-facing completion
    /// notice or output read.
    pub output_limit_bytes: Option<f64>,
    /// Pull sources the registry pumps into the ring at its own cadence,
    /// with their channel labels.
    pub output: Vec<(Option<JobChannel>, JobOutputSource)>,
    /// Starts the work after preflight; a failure leaves nothing
    /// registered.
    pub run: JobStarter,
}

/// The registry's mutable per-job record (never handed out).
struct TrackedJob {
    id: String,
    tool_call: Option<JobToolCall>,
    kind: String,
    label: String,
    output_limit_bytes: Option<f64>,
    /// Exact lifecycle owner session id.
    owner: Option<String>,
    cancel: Option<ProducerCancel>,
    status: JobStatus,
    ring: OutputRing,
    /// The model's consuming cursor; offset reads never move it.
    model_cursor: u64,
    /// Whether the first post-settlement read handed out `result`.
    result_delivered: bool,
    /// Whether the starter returned (the registration commit).
    committed: bool,
    /// Live progress line until settlement clears it.
    progress: Option<String>,
    /// Terminal reason; a recorded kill reason is merged in at
    /// settlement.
    detail: Option<String>,
    result: Option<String>,
    started_at: f64,
    finished_at: Option<f64>,
    /// Reason recorded by `kill`, merged into a `killed` settlement.
    kill_reason: Option<String>,
    /// Set once a kill or teardown cancel ran.
    settle_cause: Option<JobSettleCause>,
    /// The pump stop latch, when the spec named pull sources.
    pump: Option<PumpStop>,
}

impl std::fmt::Debug for TrackedJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The cancel closure renders as its static name (the hand-rolled
        // Debug judgment of fr-tools' DefineTool).
        f.debug_struct("TrackedJob")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("label", &self.label)
            .field("owner", &self.owner)
            .field("status", &self.status)
            .field("model_cursor", &self.model_cursor)
            .field("committed", &self.committed)
            .field("started_at", &self.started_at)
            .field("finished_at", &self.finished_at)
            .finish_non_exhaustive()
    }
}

/// The shared registry state under one mutex.
struct RegistryState {
    store: HashMap<String, TrackedJob>,
    /// Attached controllers (the dsh handshake; one count serves every
    /// owner in the flat M1 scope).
    controllers: usize,
}

impl std::fmt::Debug for RegistryState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryState")
            .field("jobs", &self.store.len())
            .field("controllers", &self.controllers)
            .finish_non_exhaustive()
    }
}

/// The spill-file table of one pumped job, shared between the pump sink
/// and the projection.
type SpillTable = Arc<Mutex<Vec<Option<String>>>>;

/// The in-memory `jobs` registry (dsh `LocalJobRegistry`).
pub struct LocalJobRegistry {
    state: Mutex<RegistryState>,
    /// Waiters park here; settlement and teardown notify all.
    settled: Condvar,
    /// Per-job spill tables of pumped jobs (projection reads; keyed by id
    /// while the job record lives).
    spills: Mutex<HashMap<String, SpillTable>>,
    /// Pump threads of pumped jobs, joined at settlement and removal.
    pumps: Mutex<HashMap<String, std::thread::JoinHandle<()>>>,
    sessions: Arc<dyn SessionLogAppend>,
    config: RegistryConfig,
    clock: Box<dyn Fn() -> f64 + Send + Sync>,
}

impl std::fmt::Debug for LocalJobRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalJobRegistry")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// The epoch-milliseconds clock (dsh `Date.now()`).
fn epoch_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |duration| epoch_ms_from(duration.as_millis()))
}

/// The cast with its documented JS-parity reason (attributes cannot ride
/// expressions; the helper carries them).
#[expect(
    clippy::cast_precision_loss,
    reason = "dsh Date.now() parity: epoch milliseconds lose sub-precision past 2^53, as in JS"
)]
#[expect(
    clippy::missing_const_for_fn,
    reason = "the numeric cast is const-compatible but the sibling helpers keep one uniform non-const shape"
)]
fn epoch_ms_from(millis: u128) -> f64 {
    millis as f64
}

/// The validated positive-millisecond cast (attributes cannot ride
/// expressions; the helper carries the JS-parity reason).
#[expect(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "the caller validated the bound as a positive finite JS number; the millisecond truncation is the dsh timer parity, and f64::max is not const"
)]
#[expect(clippy::missing_const_for_fn, reason = "f64::max is not a const fn")]
#[must_use]
pub fn ms_u64(ms: f64) -> u64 {
    ms.max(0.0) as u64
}

/// The validated non-negative safe-integer offset cast (the wire face
/// rejects anything outside that range before this runs).
#[expect(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "the wire face validates a non-negative safe integer before the cast; JS parity keeps the value whole"
)]
#[expect(clippy::missing_const_for_fn, reason = "f64::max is not a const fn")]
#[must_use]
pub fn offset_u64(offset: f64) -> u64 {
    offset.max(0.0) as u64
}

/// Locks one mutex, recovering from poisoning (the fr-tools
/// `recover` judgment: a panicked member leaves the registry usable;
/// the dsh runtime has no poisoning concept to mirror).
fn recover<T>(lock: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    lock.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The per-job sink the pump writes through (ring appends plus spill
/// tracking).
struct RegistrySink {
    registry: Arc<LocalJobRegistry>,
    id: String,
    spill_paths: SpillTable,
}

impl PumpSink for RegistrySink {
    fn append(&self, text: &str, options: JobAppendOptions) {
        self.registry.pump_append(&self.id, text, options);
    }

    fn spill(&self, index: usize, path: Option<String>) {
        let mut paths = recover(&self.spill_paths);
        if index >= paths.len() {
            paths.resize(index + 1, None);
        }
        paths[index] = path;
    }
}

/// Containment for one pull source: the first failure exhausts it (the
/// dsh `guardSource` shape; the dsh logger-warn text drops).
struct GuardedSource {
    failed: std::sync::atomic::AtomicBool,
}

impl Default for GuardedSource {
    fn default() -> Self {
        Self {
            failed: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl GuardedSource {
    fn wrap(source: JobOutputSource) -> (Arc<Self>, JobOutputSource) {
        let guard = Arc::new(Self::default());
        let flag = Arc::clone(&guard);
        let boxed: JobOutputSource = Box::new(move |from: u64| {
            if flag.failed.load(std::sync::atomic::Ordering::Acquire) {
                return JobSourceRead {
                    text: String::new(),
                    next_offset: from,
                    lossy: false,
                    spill_path: None,
                };
            }
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| source(from))).unwrap_or_else(
                |_| {
                    flag.failed
                        .store(true, std::sync::atomic::Ordering::Release);
                    JobSourceRead {
                        text: String::new(),
                        next_offset: from,
                        lossy: false,
                        spill_path: None,
                    }
                },
            )
        });
        (guard, boxed)
    }
}

impl LocalJobRegistry {
    /// Creates the registry over one session-log seam.
    #[must_use]
    pub fn new(sessions: Arc<dyn SessionLogAppend>) -> Arc<Self> {
        Self::with_config(sessions, RegistryConfig::default())
    }

    /// Creates the registry with explicit configuration (the dsh
    /// schemastery-defaulted knobs).
    #[must_use]
    pub fn with_config(sessions: Arc<dyn SessionLogAppend>, config: RegistryConfig) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(RegistryState {
                store: HashMap::new(),
                controllers: 0,
            }),
            settled: Condvar::new(),
            spills: Mutex::new(HashMap::new()),
            pumps: Mutex::new(HashMap::new()),
            sessions,
            config,
            clock: Box::new(epoch_ms),
        })
    }

    /// Overrides the clock (tests inject a fixed timeline).
    pub fn set_clock(&mut self, clock: Box<dyn Fn() -> f64 + Send + Sync>) {
        self.clock = clock;
    }

    /// Attaches one job controller (the dsh handshake; `start` refuses
    /// work while no controller is attached). Returns the disposer.
    pub fn attach_controller(self: &Arc<Self>) -> ControllerGuard {
        let registry = Arc::clone(self);
        {
            let mut state = recover(&self.state);
            state.controllers += 1;
        }
        ControllerGuard {
            registry: Some(registry),
        }
    }

    /// Preflights access, validation, and admission, then starts and
    /// atomically registers the work. Any preflight rejection leaves no
    /// job id or execution resource; a failing starter leaves nothing
    /// registered.
    ///
    /// # Errors
    ///
    /// Returns the dsh refusal texts verbatim: the missing controller,
    /// the kind/label/limit validation, or the per-owner admission cap.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "each registry mutation is one atomic critical section under the lock (the dsh single-threaded event-loop equivalence)"
    )]
    pub fn start(self: &Arc<Self>, spec: JobSpec) -> Result<String, String> {
        {
            let state = recover(&self.state);
            if state.controllers == 0 {
                return Err(
                    "background jobs unavailable: no job controller serves this agent (load @deepseek-ai/dsh-tool-jobs in its composition)"
                        .to_owned(),
                );
            }
        }
        if spec.kind.is_empty() {
            return Err("invalid job kind: expected a non-empty string".to_owned());
        }
        if spec.label.is_empty() {
            return Err("invalid job label: expected a non-empty string".to_owned());
        }
        if let Some(limit) = spec.output_limit_bytes
            && (!limit.is_finite()
                || limit <= 0.0
                || limit.fract() != 0.0
                || limit > 9_007_199_254_740_991.0)
        {
            return Err(format!(
                "invalid outputLimitBytes: expected a positive safe integer, got {}",
                crate::service::js_number(limit)
            ));
        }
        let active = self.active_job_count(spec.owner.as_deref());
        if active >= self.config.max_concurrent_jobs_per_owner {
            return Err(format!(
                "background job limit reached for this owner (limit: {}); use job_kill to stop an unneeded job, wait for it to finish, then retry",
                self.config.max_concurrent_jobs_per_owner
            ));
        }

        // The producer uses the same short UUID identity as controls and
        // durable lifecycle records; a collision against the live store
        // retries.
        let id = {
            let mut generated;
            loop {
                let uuid = Uuid::new_v4().simple().to_string();
                generated = format!("{}-{}", spec.kind, uuid.get(..16).unwrap_or(&uuid));
                let state = recover(&self.state);
                if !state.store.contains_key(&generated) {
                    break;
                }
            }
            generated
        };

        let job = TrackedJob {
            id: id.clone(),
            tool_call: spec.tool_call.clone(),
            kind: spec.kind,
            label: spec.label,
            output_limit_bytes: spec.output_limit_bytes,
            owner: spec.owner,
            cancel: None,
            status: JobStatus::Running,
            ring: OutputRing::new(),
            model_cursor: 0,
            result_delivered: false,
            committed: false,
            progress: None,
            detail: None,
            result: None,
            started_at: (self.clock)(),
            finished_at: None,
            kill_reason: None,
            settle_cause: None,
            pump: None,
        };
        {
            let mut state = recover(&self.state);
            state.store.insert(id.clone(), job);
        }
        let handle = JobHandle {
            registry: Arc::clone(self),
            id: id.clone(),
        };

        // The starter runs against the pre-committed record: ring writes
        // land, nothing signals. A failure removes the record entirely.
        let hooks = match (spec.run)(&handle) {
            Ok(hooks) => hooks,
            Err(error) => {
                let mut state = recover(&self.state);
                state.store.remove(&id);
                return Err(error);
            }
        };
        {
            let mut state = recover(&self.state);
            match state.store.get_mut(&id) {
                Some(job) => {
                    job.cancel = Some(hooks.cancel);
                    job.committed = true;
                }
                None => return Err(format!("job {id} vanished during its starter")),
            }
        }
        self.record_session_event(&id);

        // Registration is complete and cannot fail from here: the
        // record, the durable lifecycle event, and (when sources were
        // named) the pump are live. The registered event precedes the
        // pump's first drain (the dsh ordering judgment).
        if !spec.output.is_empty() {
            self.begin_pump(&id, spec.output);
        }
        Ok(id)
    }

    /// Starts the pull pump over one job's sources and records its
    /// handle, spill table, and stop latch (the dsh `startPump` wiring;
    /// the registered event precedes the first drain).
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the pump handle, spill table, and stop latch commit as one atomic registration section"
    )]
    fn begin_pump(self: &Arc<Self>, id: &str, output: Vec<(Option<JobChannel>, JobOutputSource)>) {
        let stop = PumpStop::new();
        let spill_table: SpillTable = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::new(RegistrySink {
            registry: Arc::clone(self),
            id: id.to_owned(),
            spill_paths: Arc::clone(&spill_table),
        });
        let guarded: Vec<(Option<JobChannel>, JobOutputSource)> = output
            .into_iter()
            .map(|(channel, source)| {
                let (_guard, boxed) = GuardedSource::wrap(source);
                (channel, boxed)
            })
            .collect();
        let thread = start_pump(guarded, sink, self.config.pump_poll_ms, stop.clone());
        // The pump bookkeeping is the registration commit: one atomic
        // section across the three tables (the dsh single-writer shape).
        {
            let mut state = recover(&self.state);
            if let Some(job) = state.store.get_mut(id) {
                job.pump = Some(stop);
            }
            recover(&self.spills).insert(id.to_owned(), spill_table);
            recover(&self.pumps).insert(id.to_owned(), thread);
        }
    }

    /// Whether a job record with this id exists (an existential probe
    /// for the shell service's absorbed-process table sweep; the
    /// ownership fence stays on the accessor methods).
    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        recover(&self.state).store.contains_key(id)
    }

    /// Lists caller-owned and unowned jobs in registration order.
    #[must_use]
    pub fn list(&self, caller: Option<&str>) -> Vec<JobView> {
        let mut views: Vec<JobView> = {
            let state = recover(&self.state);
            state
                .store
                .values()
                .filter(|job| job.owner.is_none() || job.owner.as_deref() == caller)
                .map(|job| Self::projection(job, None))
                .collect()
        };
        for view in &mut views {
            view.output.spill_paths = self.spill_paths_for(&view.id);
        }
        views
    }

    /// Projects one job without changing its cursor.
    ///
    /// # Errors
    ///
    /// Unknown or foreign job (the dsh fence texts verbatim).
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the projection reads the borrowed record under the lock"
    )]
    pub fn get(&self, id: &str, caller: Option<&str>) -> Result<JobView, String> {
        let spill_paths = self.spill_paths_for(id);
        let mut state = recover(&self.state);
        let job = Self::expect(&mut state, id, caller)?;
        Ok(Self::projection(job, spill_paths))
    }

    /// Consumes the ring from the model cursor; the result rides the
    /// first read after settlement. A terminal read drops the settled
    /// stream to the settled cap.
    ///
    /// # Errors
    ///
    /// Unknown or foreign job.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the cursor advance and the settled trim are one atomic read under the lock"
    )]
    pub fn read(&self, id: &str, caller: Option<&str>) -> Result<JobRead, String> {
        let spill_paths = self.spill_paths_for(id);
        let settled_retain = self.config.settled_retain_bytes;
        let mut state = recover(&self.state);
        let job = Self::expect(&mut state, id, caller)?;
        let read = job.ring.read_from(job.model_cursor);
        job.model_cursor = job.ring.total();
        let result = if job.status.is_terminal() && !job.result_delivered {
            job.result.clone()
        } else {
            None
        };
        if result.is_some() {
            job.result_delivered = true;
        }
        if job.status.is_terminal() {
            let floor = job.ring.total().saturating_sub(job.model_cursor);
            job.ring.trim(settled_retain.max(floor));
        }
        Ok(JobRead {
            chunks: read.chunks,
            lossy: read.lossy,
            result,
            job: Self::projection(job, spill_paths),
        })
    }

    /// Reads retained output at an absolute offset without moving the
    /// model cursor.
    ///
    /// # Errors
    ///
    /// A negative or non-integer offset outside the safe-integer range,
    /// or an unknown or foreign job.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the offset read borrows the record under the lock"
    )]
    pub fn read_at(
        &self,
        id: &str,
        from: f64,
        caller: Option<&str>,
    ) -> Result<JobOutputRead, String> {
        if !from.is_finite() || from < 0.0 || from.fract() != 0.0 || from > 9_007_199_254_740_991.0
        {
            return Err(format!(
                "invalid output read offset: expected a non-negative safe integer, got {}",
                crate::service::js_number(from)
            ));
        }
        let mut state = recover(&self.state);
        let job = Self::expect(&mut state, id, caller)?;
        Ok(job.ring.read_from(offset_u64(from)))
    }

    /// Requests cancellation, then marks the job stopping. A producer
    /// cancel failure propagates without changing job state.
    ///
    /// # Errors
    ///
    /// Unknown or foreign job, or a failing producer cancel.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the producer cancel runs under the registry lock: the dsh event loop serializes the same callback against the same record"
    )]
    pub fn kill(
        &self,
        id: &str,
        caller: Option<&str>,
        reason: Option<&str>,
    ) -> Result<&'static str, String> {
        let mut state = recover(&self.state);
        let job = Self::expect(&mut state, id, caller)?;
        if job.status.is_terminal() {
            return Ok("already-finished");
        }
        // Cancel first so a failure leaves lifecycle state unchanged.
        if let Some(cancel) = job.cancel.as_ref() {
            cancel(reason)?;
        }
        job.status = JobStatus::Stopping;
        // Last writer wins on purpose: the detail reports the latest kill
        // intent.
        if let Some(reason) = reason {
            job.kill_reason = Some(reason.to_owned());
        }
        job.settle_cause = Some(JobSettleCause::Kill);
        Ok("requested")
    }

    /// Waits for settlement or the bounded timeout without cancelling the
    /// job (dsh `wait`; the wire face drives it in bounded slices).
    ///
    /// # Errors
    ///
    /// An invalid timeout, or an unknown or foreign job.
    pub fn wait(&self, id: &str, timeout_ms: f64, caller: Option<&str>) -> Result<JobView, String> {
        if !timeout_ms.is_finite() || timeout_ms <= 0.0 {
            return Err(format!(
                "invalid wait timeout: expected a positive number of milliseconds, got {}",
                crate::service::js_number(timeout_ms)
            ));
        }
        let deadline = std::time::Instant::now() + Duration::from_millis(ms_u64(timeout_ms));
        let spill_paths = self.spill_paths_for(id);
        let mut guard = recover(&self.state);
        loop {
            {
                let job = Self::expect(&mut guard, id, caller)?;
                if job.status.is_terminal() {
                    return Ok(Self::projection(job, spill_paths));
                }
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                let job = Self::expect(&mut guard, id, caller)?;
                return Ok(Self::projection(job, spill_paths));
            }
            let (next, _timeout) = self
                .settled
                .wait_timeout(guard, deadline - now)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard = next;
        }
    }

    /// Drops one settled job's record from the visible set.
    ///
    /// # Errors
    ///
    /// A live, unknown, or foreign job (the dsh text verbatim).
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the terminal check and the record drop are one atomic removal under the lock"
    )]
    pub fn remove(&self, id: &str, caller: Option<&str>) -> Result<(), String> {
        let mut state = recover(&self.state);
        {
            let job = Self::expect(&mut state, id, caller)?;
            if !job.status.is_terminal() {
                return Err(format!(
                    "job {id} is still {}; kill it and wait for settlement before removing it",
                    job.status.as_str()
                ));
            }
        }
        drop(state);
        self.join_pump(id);
        let mut state = recover(&self.state);
        state.store.remove(id);
        recover(&self.spills).remove(id);
        Ok(())
    }

    /// The producer's terminal outcome: wakes and joins the pump's final
    /// drain, then records the first-wins settlement (dsh
    /// `producerDone.then(settle)`).
    pub fn producer_settled(
        self: &Arc<Self>,
        id: &str,
        outcome: JobOutcome,
        producer_reported: bool,
    ) {
        self.join_pump(id);
        let cause = {
            let state = recover(&self.state);
            state
                .store
                .get(id)
                .and_then(|job| job.settle_cause)
                .unwrap_or(JobSettleCause::Producer)
        };
        self.settle(id, outcome, cause, producer_reported);
    }

    /// Cancels every live job with the teardown reason, waits for each
    /// settlement, and drops every record (dsh `disposeAll`; the dsh
    /// cancel-threw force-fail shape has no failing cancels here — the
    /// contained failure leaves the join bounded by the wait deadline).
    #[expect(
        clippy::significant_drop_tightening,
        reason = "each teardown step (cancel, wait, drop) is one atomic section under the lock"
    )]
    ///
    /// # Panics
    pub fn dispose_all(self: &Arc<Self>) {
        let ids: Vec<String> = {
            let state = recover(&self.state);
            state
                .store
                .values()
                .filter(|job| !job.status.is_terminal())
                .map(|job| job.id.clone())
                .collect()
        };
        for id in &ids {
            {
                let mut state = recover(&self.state);
                if let Some(job) = state.store.get_mut(id) {
                    if job.status.is_terminal() {
                        continue;
                    }
                    job.settle_cause = Some(JobSettleCause::Teardown);
                    if let Some(cancel) = job.cancel.as_ref() {
                        let _ignored = cancel(Some("jobs service disposed"));
                    }
                    job.status = JobStatus::Stopping;
                }
            }
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            let mut guard = recover(&self.state);
            loop {
                match guard.store.get(id) {
                    Some(job) if !job.status.is_terminal() => {}
                    _ => break,
                }
                let now = std::time::Instant::now();
                if now >= deadline {
                    break;
                }
                let (next, _timeout) = self
                    .settled
                    .wait_timeout(guard, deadline - now)
                    .expect("registry lock");
                guard = next;
            }
        }
        let all: Vec<String> = {
            let state = recover(&self.state);
            state.store.keys().cloned().collect()
        };
        for id in all {
            self.join_pump(&id);
            let mut state = recover(&self.state);
            state.store.remove(&id);
            recover(&self.spills).remove(&id);
        }
    }

    /// Releases one job's pump stop latch and joins the thread after
    /// waking it for the final drain (the latch release precedes the
    /// join: the pump exits only through it).
    ///
    /// # Panics
    ///
    /// Never: a panicked pump thread's payload is discarded (the dsh
    /// contained-listener shape).
    fn join_pump(&self, id: &str) {
        let thread = recover(&self.pumps).remove(id);
        if let Some(thread) = thread {
            {
                let mut state = recover(&self.state);
                if let Some(job) = state.store.get_mut(id)
                    && let Some(stop) = job.pump.take()
                {
                    stop.release();
                }
            }
            thread.thread().unpark();
            let _ = thread.join();
        }
    }

    /// Counts authoritative active records for one exact owner or the
    /// shared unowned bucket.
    fn active_job_count(&self, owner: Option<&str>) -> usize {
        let state = recover(&self.state);
        state
            .store
            .values()
            .filter(|job| {
                job.owner.as_deref() == owner
                    && matches!(job.status, JobStatus::Running | JobStatus::Stopping)
            })
            .count()
    }

    /// Looks up a job and enforces the caller fence (dsh `expect` +
    /// `assertAccess`: an unowned job is open, and a caller-less view can
    /// never match an owned one).
    fn expect<'s>(
        state: &'s mut RegistryState,
        id: &str,
        caller: Option<&str>,
    ) -> Result<&'s mut TrackedJob, String> {
        let Some(job) = state.store.get_mut(id) else {
            return Err(format!("unknown job {id}"));
        };
        if let Some(owner) = job.owner.as_deref()
            && owner != caller.unwrap_or("")
        {
            return Err(format!("job {id} belongs to another session"));
        }
        Ok(job)
    }

    /// Reads one job's deduplicated spill table (the dsh projection's
    /// spill paths), taken under its own lock so no job mutation runs
    /// while the registry lock is held.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the table guard lives exactly across the deduplicated snapshot"
    )]
    fn spill_paths_for(&self, id: &str) -> Option<Vec<String>> {
        let table = recover(&self.spills);
        let paths = recover(table.get(id)?);
        let deduped: std::collections::BTreeSet<String> = paths.iter().flatten().cloned().collect();
        (!deduped.is_empty()).then(|| deduped.into_iter().collect())
    }

    /// Projects a fresh read-only view from the mutable record (pure —
    /// the spill table rides in, taken under its own lock).
    fn projection(job: &TrackedJob, spill_paths: Option<Vec<String>>) -> JobView {
        JobView {
            id: job.id.clone(),
            kind: job.kind.clone(),
            label: job.label.clone(),
            owner: job.owner.clone(),
            output_limit_bytes: job.output_limit_bytes,
            status: job.status,
            progress: job.progress.clone(),
            detail: job.detail.clone(),
            started_at: job.started_at,
            finished_at: job.finished_at,
            output: crate::jobs_types::JobOutputFacts {
                total: job.ring.total(),
                earliest: job.ring.earliest(),
                spill_paths,
            },
        }
    }

    /// Producer append: against a settled job the write drops; starter
    /// writes land without waking anyone (the commit publishes them).
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the append is one atomic ring mutation under the lock"
    )]
    fn producer_append(&self, id: &str, text: &str, options: JobAppendOptions) {
        let mut state = recover(&self.state);
        let Some(job) = state.store.get_mut(id) else {
            return;
        };
        if job.committed && job.status.is_terminal() {
            return;
        }
        let retain = self.config.retain_bytes;
        let _ = job.ring.append(text, options, retain);
    }

    /// Pump append (silent after settlement — the dsh pump drain shape).
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the append is one atomic ring mutation under the lock"
    )]
    fn pump_append(&self, id: &str, text: &str, options: JobAppendOptions) {
        let mut state = recover(&self.state);
        let Some(job) = state.store.get_mut(id) else {
            return;
        };
        if job.status.is_terminal() {
            return;
        }
        let retain = self.config.retain_bytes;
        let _ = job.ring.append(text, options, retain);
    }

    /// Producer progress write against a settled job drops.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the progress write is one atomic field mutation under the lock"
    )]
    fn producer_progress(&self, id: &str, line: &str) {
        let mut state = recover(&self.state);
        let Some(job) = state.store.get_mut(id) else {
            return;
        };
        if job.committed && job.status.is_terminal() {
            return;
        }
        job.progress = Some(line.to_owned());
    }

    /// Records the first terminal outcome, releases the pump, and marks
    /// the settlement (dsh `settle`; the event announcements have no M1
    /// subscriber face).
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the first-wins settlement is one atomic critical section under the lock"
    )]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the outcome fields land on the record inside the first-wins settlement"
    )]
    fn settle(
        &self,
        id: &str,
        outcome: JobOutcome,
        cause: JobSettleCause,
        producer_reported: bool,
    ) {
        let settled_event = {
            let mut state = recover(&self.state);
            let Some(job) = state.store.get_mut(id) else {
                return;
            };
            if job.status.is_terminal() {
                return;
            }
            job.status = outcome.status;
            // A killed settlement carries the recorded kill reason in its
            // detail: producer facts first. A job that outran its kill
            // request keeps the producer detail alone.
            if outcome.status == JobStatus::Killed {
                job.detail = match (outcome.detail.clone(), job.kill_reason.clone()) {
                    (Some(detail), Some(reason)) => Some(format!("{detail}; {reason}")),
                    (None, Some(reason)) => Some(reason),
                    (detail, None) => detail,
                };
            } else {
                job.detail.clone_from(&outcome.detail);
            }
            job.progress = None;
            job.result.clone_from(&outcome.result);
            job.finished_at = Some((self.clock)());
            if let Some(stop) = job.pump.take() {
                stop.release();
            }
            // Settlement ends the stream: trim to the settled cap, but
            // never below the bytes the model cursor has not consumed.
            let floor = job.ring.total().saturating_sub(job.model_cursor);
            job.ring.trim(self.config.settled_retain_bytes.max(floor));
            (job.owner.is_some()).then(|| JobSettledData {
                registered: JobRegisteredData {
                    job_id: job.id.clone(),
                    owner: job.owner.clone().unwrap_or_default(),
                    kind: job.kind.clone(),
                    label: job.label.clone(),
                    started_at: job.started_at,
                    tool_call: job.tool_call.clone(),
                },
                finished_at: job.finished_at.unwrap_or_default(),
                status: outcome.status,
                cause,
                producer_reported,
                detail: job.detail.clone(),
            })
        };
        if let Some(data) = settled_event
            && let Ok(payload) = serde_json::to_value(&data)
        {
            let _ignored =
                self.sessions
                    .append(&data.registered.owner, JOB_SETTLED_EVENT, &payload);
        }
        self.settled.notify_all();
    }

    /// Publishes the committed registration facts into the owner
    /// session's log (dsh `recordSessionEvent`; a failure contains).
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the record read and the payload build are one atomic snapshot under the lock"
    )]
    fn record_session_event(&self, id: &str) {
        let has_owner = {
            let state = recover(&self.state);
            state.store.get(id).is_some_and(|job| job.owner.is_some())
        };
        if !has_owner {
            return;
        }
        let recorded = {
            let state = recover(&self.state);
            let Some(job) = state.store.get(id) else {
                return;
            };
            let Some(owner) = job.owner.clone() else {
                return;
            };
            let payload = serde_json::to_value(&JobRegisteredData {
                job_id: job.id.clone(),
                owner: owner.clone(),
                kind: job.kind.clone(),
                label: job.label.clone(),
                started_at: job.started_at,
                tool_call: job.tool_call.clone(),
            })
            .ok();
            payload.map(|payload| (owner, payload))
        };
        if let Some((session, payload)) = recorded {
            let _ignored = self
                .sessions
                .append(&session, JOB_REGISTERED_EVENT, &payload);
        }
    }
}

/// The controller attachment disposer: dropping it detaches the
/// controller (the dsh effect-scoped cleanup shape).
pub struct ControllerGuard {
    registry: Option<Arc<LocalJobRegistry>>,
}

impl std::fmt::Debug for ControllerGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControllerGuard").finish_non_exhaustive()
    }
}

impl Drop for ControllerGuard {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.take() {
            let mut state = registry.state.lock().expect("registry lock");
            state.controllers = state.controllers.saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// One recording session log (the durable lifecycle records).
    #[derive(Debug, Default)]
    struct RecordingLog {
        records: std::sync::Mutex<Vec<(String, String)>>,
    }

    impl SessionLogAppend for RecordingLog {
        fn append(&self, session: &str, event_type: &str, _data: &Value) -> Result<(), String> {
            self.records
                .lock()
                .expect("lock")
                .push((session.to_owned(), event_type.to_owned()));
            Ok(())
        }
    }

    fn registry() -> Arc<LocalJobRegistry> {
        LocalJobRegistry::new(Arc::new(RecordingLog::default()))
    }

    fn noop_hooks() -> JobHooks {
        JobHooks {
            cancel: Box::new(|_reason| Ok(())),
            done: Box::new(|| JobOutcome {
                status: JobStatus::Completed,
                detail: Some("exit code: 0".to_owned()),
                result: None,
            }),
        }
    }

    /// The admission refusal family rides the dsh texts verbatim: the
    /// missing controller, the kind/label/limit validation, and the
    /// per-owner cap.
    #[test]
    fn admission_refusals_pin_the_texts() {
        let registry = registry();
        let spec = || JobSpec {
            kind: "bash".to_owned(),
            label: "label".to_owned(),
            owner: Some("s1".to_owned()),
            tool_call: None,
            output_limit_bytes: None,
            output: Vec::new(),
            run: Box::new(|_handle| Ok(noop_hooks())),
        };
        assert_eq!(
            registry.start(spec()).unwrap_err(),
            "background jobs unavailable: no job controller serves this agent (load @deepseek-ai/dsh-tool-jobs in its composition)"
        );
        let _controller = registry.attach_controller();
        let mut variant = spec();
        variant.kind = String::new();
        assert_eq!(
            registry.start(variant).unwrap_err(),
            "invalid job kind: expected a non-empty string"
        );
        let mut variant = spec();
        variant.label = String::new();
        assert_eq!(
            registry.start(variant).unwrap_err(),
            "invalid job label: expected a non-empty string"
        );
        let mut variant = spec();
        variant.output_limit_bytes = Some(0.5);
        assert_eq!(
            registry.start(variant).unwrap_err(),
            "invalid outputLimitBytes: expected a positive safe integer, got 0.5"
        );
        // The per-owner cap counts active records per exact owner.
        let limited = LocalJobRegistry::with_config(
            Arc::new(RecordingLog::default()),
            RegistryConfig {
                max_concurrent_jobs_per_owner: 1,
                ..RegistryConfig::default()
            },
        );
        let _controller = limited.attach_controller();
        let first = limited.start(spec()).expect("first job");
        assert_eq!(
            limited.start(spec()).unwrap_err(),
            "background job limit reached for this owner (limit: 1); use job_kill to stop an unneeded job, wait for it to finish, then retry"
        );
        limited.producer_settled(
            &first,
            JobOutcome {
                status: JobStatus::Completed,
                detail: None,
                result: None,
            },
            true,
        );
        assert!(
            limited.start(spec()).is_ok(),
            "a settled job frees the slot"
        );
    }

    /// The access fence, the wait validation, and the removal rule ride
    /// the dsh texts verbatim; an unowned job is open to every caller.
    #[test]
    fn fence_wait_and_removal_rules() {
        let registry = registry();
        let _controller = registry.attach_controller();
        let spec = |owner: Option<String>| JobSpec {
            kind: "bash".to_owned(),
            label: "label".to_owned(),
            owner,
            tool_call: None,
            output_limit_bytes: None,
            output: Vec::new(),
            run: Box::new(|_handle| Ok(noop_hooks())),
        };
        let owned = registry
            .start(spec(Some("s1".to_owned())))
            .expect("owned job");
        let unowned = registry.start(spec(None)).expect("unowned job");
        assert_eq!(
            registry.get(&owned, None).unwrap_err(),
            format!("job {owned} belongs to another session")
        );
        registry
            .get(&owned, Some("s1"))
            .expect("the owned job is open to its owner");
        assert!(
            registry.get(&unowned, None).is_ok(),
            "an unowned job is open"
        );
        assert_eq!(
            registry.wait(&owned, 0.0, Some("s1")).unwrap_err(),
            "invalid wait timeout: expected a positive number of milliseconds, got 0"
        );
        assert_eq!(
            registry.remove(&owned, Some("s1")).unwrap_err(),
            format!(
                "job {owned} is still running; kill it and wait for settlement before removing it"
            )
        );
    }

    /// Settlement is first-wins; a recorded kill reason merges behind
    /// the producer detail; the result rides the first post-settlement
    /// read only; the settled trim keeps every byte the model cursor
    /// has not consumed.
    #[test]
    fn settlement_merges_trim_and_result_once() {
        let registry = LocalJobRegistry::with_config(
            Arc::new(RecordingLog::default()),
            RegistryConfig {
                settled_retain_bytes: 64,
                ..RegistryConfig::default()
            },
        );
        let _controller = registry.attach_controller();
        let id = registry
            .start(JobSpec {
                kind: "bash".to_owned(),
                label: "label".to_owned(),
                owner: Some("s1".to_owned()),
                tool_call: None,
                output_limit_bytes: None,
                output: Vec::new(),
                run: Box::new(|_handle| Ok(noop_hooks())),
            })
            .expect("job");
        // The producer writes, the killer records its reason, then the
        // producer settles: the merged detail keeps both.
        registry
            .kill(&id, Some("s1"), Some("tool call aborted"))
            .expect("kill");
        registry.producer_settled(
            &id,
            JobOutcome {
                status: JobStatus::Killed,
                detail: Some("signal: SIGTERM".to_owned()),
                result: None,
            },
            true,
        );
        let view = registry.get(&id, Some("s1")).expect("view");
        assert_eq!(view.status, JobStatus::Killed);
        assert_eq!(
            view.detail.as_deref(),
            Some("signal: SIGTERM; tool call aborted")
        );
        assert_eq!(
            registry.kill(&id, Some("s1"), None).expect("kill"),
            "already-finished"
        );

        // The settled trim keeps the unconsumed floor on a fresh job:
        // a live producer writes, settlement closes the stream, and the
        // reads consume exactly once.
        let id = registry
            .start(JobSpec {
                kind: "bash".to_owned(),
                label: "label".to_owned(),
                owner: Some("s1".to_owned()),
                tool_call: None,
                output_limit_bytes: None,
                output: Vec::new(),
                run: Box::new(|_handle| Ok(noop_hooks())),
            })
            .expect("trim job");
        let handle = JobHandle {
            registry: Arc::clone(&registry),
            id: id.clone(),
        };
        let payload = "x".repeat(1000);
        handle.append(&payload, JobAppendOptions::default());
        registry.producer_settled(
            &id,
            JobOutcome {
                status: JobStatus::Completed,
                detail: Some("exit code: 0".to_owned()),
                result: Some("value".to_owned()),
            },
            true,
        );
        let first = registry.read(&id, Some("s1")).expect("read");
        assert_eq!(
            first
                .chunks
                .iter()
                .map(|chunk| chunk.text.len())
                .sum::<usize>(),
            1000,
            "the unconsumed floor survives the settled trim"
        );
        assert_eq!(
            first.result.as_deref(),
            Some("value"),
            "the result rides the first read"
        );
        let second = registry.read(&id, Some("s1")).expect("read");
        assert!(second.chunks.is_empty(), "the cursor consumed the stream");
        assert_eq!(second.result, None, "the result is one-shot");
    }

    /// The session log records the registration and the settlement for
    /// owned jobs only.
    #[test]
    fn session_records_land_for_owned_jobs() {
        let log = Arc::new(RecordingLog::default());
        let registry = LocalJobRegistry::new(Arc::clone(&log) as Arc<dyn SessionLogAppend>);
        let _controller = registry.attach_controller();
        let spec = |owner: Option<String>| JobSpec {
            kind: "bash".to_owned(),
            label: "label".to_owned(),
            owner,
            tool_call: None,
            output_limit_bytes: None,
            output: Vec::new(),
            run: Box::new(|_handle| Ok(noop_hooks())),
        };
        let owned = registry.start(spec(Some("s1".to_owned()))).expect("owned");
        let unowned = registry.start(spec(None)).expect("unowned");
        registry.producer_settled(
            &owned,
            JobOutcome {
                status: JobStatus::Completed,
                detail: None,
                result: None,
            },
            true,
        );
        registry.producer_settled(
            &unowned,
            JobOutcome {
                status: JobStatus::Completed,
                detail: None,
                result: None,
            },
            true,
        );
        let kinds: Vec<String> = log
            .records
            .lock()
            .expect("lock")
            .iter()
            .map(|(session, event)| format!("{session}/{event}"))
            .collect();
        assert!(
            kinds.contains(&"s1/job/registered".to_owned()),
            "got {kinds:?}"
        );
        assert!(
            kinds.contains(&"s1/job/settled".to_owned()),
            "got {kinds:?}"
        );
        assert_eq!(
            kinds.len(),
            2,
            "an unowned job records nothing, got {kinds:?}"
        );
    }
}
