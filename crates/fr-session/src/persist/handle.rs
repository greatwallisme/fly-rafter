//! The JSONL session handle runtime (dsh storage.ts `JsonlSessionHandle`).
//!
//! A handle is a write/read channel onto one stored log whose mutations
//! serialize on a per-handle worker — the Rust analogue of the per-handle
//! promise chain — with the routed live write-behind buffer draining
//! through the same serialization. Deliberately provider-local to this
//! backend.

use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc};
use std::time::Duration;

use fr_wire::event::{SessionEvent, SessionHeader};
use fr_wire::ids::SessionId;

use super::PersistError;
use super::backend::JsonlBackend;
use super::lease::SessionWriteLease;
use super::scheduler::{BatchScheduler, TimerTarget};

/// Log access granted by an open (dsh `SessionAccess`): `write` is the
/// session's single mutator which also reads its own log; `read` only
/// observes and never takes ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAccess {
    /// Read-write: the session's single mutator.
    Write,
    /// Observation only; works while another handle or process holds write.
    Read,
}

/// One persistence event slice returned by [`JsonlSessionHandle::read`]
/// (dsh `SessionHandleReadResult`; the dsh `eventState` aliasing flag has
/// no Rust counterpart — values here are always owned copies).
#[derive(Debug, Clone)]
pub struct HandleRead {
    /// Event values from the valid contiguous logical log.
    pub events: Vec<SessionEvent>,
}

/// Mutable per-handle log state handed to a freshly constructed handle
/// (dsh `StorageHandleState`); a write handle is its session's single
/// mutator.
pub(crate) struct StorageHandleState {
    /// The stored next-seq (the logical end this handle knows).
    pub(crate) cursor: u64,
    /// Whether the session has a durable artifact yet.
    pub(crate) materialized: bool,
    /// Torn-tail truncation point, consumed by the first new append.
    pub(crate) torn_truncate_to: Option<u64>,
    /// Complete events recovered from a torn final frame; the first
    /// mutation rewrites them durably.
    pub(crate) recovered_tail: Option<Vec<SessionEvent>>,
    /// Exact fork-inherited prefix length stored with the log.
    pub(crate) inherited_event_count: u64,
    /// The validated stored prefix from an open, served to reads until the
    /// first append (write) or until the artifact resolves (read).
    pub(crate) primed: Option<Vec<SessionEvent>>,
    /// The cross-process write lock; a create handle passes `None` and
    /// acquires it lazily at first materialization.
    pub(crate) lease: Option<SessionWriteLease>,
}

/// The worker-owned mutation state (the promise chain's closure state in
/// dsh): only the handle worker touches it.
struct HandleCore {
    cursor: u64,
    materialized: bool,
    torn_truncate_to: Option<u64>,
    recovered_tail: Option<Vec<SessionEvent>>,
    lease: Option<SessionWriteLease>,
}

/// The read-visible state shared between the worker and reader threads:
/// mutations are tiny field swaps, never I/O under the lock.
struct SharedReadState {
    /// The validated stored prefix served to reads until invalidated.
    primed: Option<Vec<SessionEvent>>,
    /// The longest log length this handle ever observed (monotonic view).
    observed_length: u64,
    /// Whether the session has a durable artifact yet (read-path mirror).
    materialized: bool,
}

/// One serialized operation executed by the handle worker (one dsh chain
/// turn per op).
enum Op {
    Append {
        batch: Vec<SessionEvent>,
        reply: OpReply,
    },
    Flush {
        reply: OpReply,
    },
    Drain {
        reply: OpReply,
    },
    /// The timer-fired drain: failures go to the warn hook, not a caller.
    DrainBackground,
    Close {
        slot: Arc<CloseSlot>,
    },
}

type OpReply = mpsc::Sender<Result<(), PersistError>>;

/// The read-path dispatch computed from one shared-state probe (dsh
/// read's branch ladder, storage.ts:129-155).
enum ReadPlan {
    /// A write handle reads its own verified stored prefix.
    PrimedWrite,
    /// A read handle probes whether a current artifact superseded its
    /// prepared prefix.
    PrimedReadProbe,
    /// An unmaterialized create handle owns an empty detached log.
    UnmaterializedWrite,
    /// Resolve the current artifact (or pending/absent rules).
    Resolve,
}

/// The shared close outcome (dsh `closing ??=`: every close caller observes
/// the one body's settlement).
struct CloseSlot {
    done: Mutex<Option<Result<(), PersistError>>>,
    signal: Condvar,
}

impl CloseSlot {
    const fn new() -> Self {
        Self {
            done: Mutex::new(None),
            signal: Condvar::new(),
        }
    }

    /// Records the outcome once (first completion wins) and wakes waiters.
    fn complete(&self, outcome: Result<(), PersistError>) {
        let mut done = lock(&self.done);
        if done.is_none() {
            *done = Some(outcome);
        }
        drop(done);
        self.signal.notify_all();
    }

    /// Waits for the one close body's settlement; poll-guarded against a
    /// lost wake-up between the worker reply and this waiter's wait.
    fn wait(&self) -> Result<(), PersistError> {
        let mut done = lock(&self.done);
        loop {
            if let Some(outcome) = done.as_ref() {
                return outcome.clone();
            }
            let (guard, _) = self
                .signal
                .wait_timeout(done, Duration::from_millis(200))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            done = guard;
        }
    }
}

/// The shared handle inner: every field is either immutable after
/// construction or one of the tiny synchronized cells; the mutation state
/// itself lives exclusively in the worker ([`HandleCore`]).
pub(crate) struct HandleInner {
    /// Process-unique identity for tracker bookkeeping.
    pub(crate) uid: u64,
    pub(crate) id: SessionId,
    pub(crate) access: SessionAccess,
    header: SessionHeader,
    inherited_event_count: u64,
    backend: Arc<JsonlBackend>,
    timer: Arc<TimerTarget>,
    scheduler: BatchScheduler,
    closing: AtomicBool,
    drain_paused: AtomicBool,
    buffered: Mutex<Vec<SessionEvent>>,
    shared: Mutex<SharedReadState>,
    ops_tx: mpsc::Sender<Op>,
    close_slot: Mutex<Option<Arc<CloseSlot>>>,
    /// The warn hook for background drain failures (dsh `ctx.logger.warn`).
    report: Option<Arc<dyn Fn(String) + Send + Sync>>,
}

impl fmt::Debug for JsonlSessionHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JsonlSessionHandle")
            .field("id", &self.inner.id)
            .field("access", &self.inner.access)
            .finish_non_exhaustive()
    }
}

/// The JSONL session handle (dsh `JsonlSessionHandle`).
///
/// All operations serialize through the per-handle worker; `read` never
/// observes a shorter log than a prior read on this handle, and `close`
/// is idempotent with one shared outcome.
#[derive(Clone)]
pub struct JsonlSessionHandle {
    inner: Arc<HandleInner>,
}

impl JsonlSessionHandle {
    /// Constructs the handle and spawns its worker. Called only by the
    /// backend's `create`/`open`.
    pub(crate) fn new(
        backend: Arc<JsonlBackend>,
        header: SessionHeader,
        access: SessionAccess,
        state: StorageHandleState,
        scheduler: BatchScheduler,
        report: Option<Arc<dyn Fn(String) + Send + Sync>>,
    ) -> Result<Self, PersistError> {
        let id = header.id.clone();
        let shared = SharedReadState {
            materialized: state.materialized,
            primed: state.primed,
            observed_length: 0,
        };
        let core = HandleCore {
            cursor: state.cursor,
            materialized: state.materialized,
            torn_truncate_to: state.torn_truncate_to,
            recovered_tail: state.recovered_tail,
            lease: state.lease,
        };
        let (ops_tx, ops_rx) = mpsc::channel();
        let timer_tx = ops_tx.clone();
        let timer = Arc::new(TimerTarget::new(Box::new(move || {
            // A send failure means the worker is gone (all handles dropped):
            // nothing remains to drain.
            let _ = timer_tx.send(Op::DrainBackground);
        })));
        let inner = Arc::new(HandleInner {
            uid: next_handle_uid(),
            id,
            access,
            inherited_event_count: state.inherited_event_count,
            header,
            backend,
            timer,
            scheduler,
            closing: AtomicBool::new(false),
            drain_paused: AtomicBool::new(false),
            buffered: Mutex::new(Vec::new()),
            shared: Mutex::new(shared),
            ops_tx,
            close_slot: Mutex::new(None),
            report,
        });
        let worker_name = format!("fr-session-handle-{}", inner.id.as_str());
        let worker_inner = Arc::clone(&inner);
        let mut worker_core = core;
        std::thread::Builder::new()
            .name(worker_name)
            .spawn(move || {
                // The worker owns the queue and the mutation state; when
                // every sender drops, recv fails and the core drops with
                // its lease (dsh process-death semantics).
                while let Ok(op) = ops_rx.recv() {
                    worker_dispatch(&worker_inner, &mut worker_core, op);
                }
            })
            .map_err(PersistError::Io)?;
        Ok(Self { inner })
    }

    /// The stored session this handle addresses.
    #[must_use]
    pub fn id(&self) -> &SessionId {
        &self.inner.id
    }

    /// The immutable stored header, fixed at create/open.
    #[must_use]
    pub fn header(&self) -> &SessionHeader {
        &self.inner.header
    }

    /// Whether this handle may mutate the log.
    #[must_use]
    pub fn access(&self) -> SessionAccess {
        self.inner.access
    }

    /// Exact fork-inherited prefix length stored with this session's log.
    #[must_use]
    pub fn inherited_event_count(&self) -> u64 {
        self.inner.inherited_event_count
    }

    /// The shared inner for tracker bookkeeping.
    pub(crate) fn inner_arc(&self) -> Arc<HandleInner> {
        Arc::clone(&self.inner)
    }

    /// Re-wraps a tracked inner (teardown and flush sweeps).
    pub(crate) const fn from_inner(inner: Arc<HandleInner>) -> Self {
        Self { inner }
    }
}

impl JsonlSessionHandle {
    /// Reads a slice of the valid contiguous logical log (dsh `read`): the
    /// slice is a legal log prefix segment — a torn physical tail is never
    /// returned — and repeated reads never observe an older state than a
    /// prior read on this handle. The closed-handle refusal precedes
    /// argument validation.
    ///
    /// # Errors
    ///
    /// Returns `HandleClosed` after `close`, `InvalidInput` for an offset
    /// or length beyond the safe-integer domain, and the storage refusals.
    pub fn read(&self, offset: u64, length: u64) -> Result<HandleRead, PersistError> {
        self.assert_open("read")?;
        // dsh validates `Number.isSafeInteger`; the u64 domain only leaves
        // the upper bound to refuse.
        for (name, value) in [("read offset", offset), ("read length", length)] {
            if value > fr_wire::ids::SAFE_INTEGER_MAX {
                return Err(PersistError::InvalidInput(format!(
                    "{name} must be a non-negative safe integer, got {value}"
                )));
            }
        }
        let plan = {
            let shared = lock(&self.inner.shared);
            if shared.primed.is_some() {
                match self.inner.access {
                    SessionAccess::Write => ReadPlan::PrimedWrite,
                    SessionAccess::Read => ReadPlan::PrimedReadProbe,
                }
            } else if self.inner.access == SessionAccess::Write && !shared.materialized {
                ReadPlan::UnmaterializedWrite
            } else {
                ReadPlan::Resolve
            }
        };
        match plan {
            // A write handle reads its own verified stored prefix until its
            // first append clears it (dsh `readPrimed` on the write arm).
            ReadPlan::PrimedWrite => {
                let mut shared = lock(&self.inner.shared);
                let events = shared
                    .primed
                    .as_ref()
                    .map_or_else(Vec::new, <Vec<SessionEvent>>::clone);
                Ok(Self::slice_primed(
                    &mut shared.observed_length,
                    events,
                    offset,
                    length,
                ))
            }
            // A read handle keeps its prepared prefix only while no
            // current-generation artifact resolves; the first resolution
            // discards the prefix and rescans (dsh read-arm primed probe).
            ReadPlan::PrimedReadProbe => {
                if self
                    .inner
                    .backend
                    .resolve_current_log(&self.inner.id)?
                    .is_some()
                {
                    lock(&self.inner.shared).primed = None;
                    self.read_current(offset, length)
                } else {
                    let mut shared = lock(&self.inner.shared);
                    let events = shared
                        .primed
                        .as_ref()
                        .map_or_else(Vec::new, <Vec<SessionEvent>>::clone);
                    Ok(Self::slice_primed(
                        &mut shared.observed_length,
                        events,
                        offset,
                        length,
                    ))
                }
            }
            // An unmaterialized create handle owns an empty detached log.
            ReadPlan::UnmaterializedWrite => Ok(HandleRead { events: Vec::new() }),
            ReadPlan::Resolve => match self.inner.backend.resolve_current_log(&self.inner.id)? {
                Some(path) => self.read_current_at(&path, offset, length),
                None if self.inner.backend.has_pending_session(&self.inner.id) => {
                    Ok(HandleRead { events: Vec::new() })
                }
                None => Err(PersistError::NotFound(format!(
                    "session \"{}\" not found",
                    self.inner.id.as_str()
                ))),
            },
        }
    }

    /// Appends one contiguous batch continuing the current logical end (dsh
    /// `append`): the batch is validated-and-snapshotted here, before
    /// queueing, so the checked value is exactly the value persisted.
    ///
    /// # Errors
    ///
    /// Returns `HandleClosed`/`ReadOnly` refusals, `InvalidInput` for a
    /// batch that does not contiguously continue the stored log, and the
    /// storage write failures.
    pub fn append(&self, events: &[SessionEvent]) -> Result<(), PersistError> {
        self.assert_open("append")?;
        let batch = events.to_vec();
        self.call("append", move |reply| Op::Append { batch, reply })
    }

    /// The durability barrier (dsh `flush`): materializes the artifact when
    /// nothing has been appended yet, so an explicitly flushed empty
    /// session survives this process.
    ///
    /// # Errors
    ///
    /// Returns `HandleClosed`/`ReadOnly` refusals and the storage failures.
    pub fn flush(&self) -> Result<(), PersistError> {
        self.assert_open("flush")?;
        self.call("flush", |reply| Op::Flush { reply })
    }

    /// Durably drains the routed live buffer through the same serialization
    /// as explicit appends (dsh `drainLive`); a failure retains the batch
    /// in order and pauses timer-driven drains until the next drain.
    ///
    /// # Errors
    ///
    /// Returns the underlying write failure with the batch retained.
    pub fn drain_live(&self) -> Result<(), PersistError> {
        self.call("drain", |reply| Op::Drain { reply })
    }

    /// Buffers one published live session event (a persistence-owned copy)
    /// and arms the bounded batching window when idle (dsh `enqueueLive`).
    /// The routing installer is the only caller.
    pub fn enqueue_live(&self, event: &SessionEvent) {
        lock(&self.inner.buffered).push(event.clone());
        if self.inner.drain_paused.load(Ordering::Acquire) {
            return;
        }
        self.inner.timer.arm(&self.inner.scheduler);
    }

    /// Releases the handle (dsh `close`): idempotent with one shared
    /// outcome. A write handle first drains its routed live buffer, then
    /// frees the write lease, then drops its bookkeeping; a drain or
    /// lock-release failure still frees the claim, then fails — both
    /// failures together fail as one aggregate.
    ///
    /// # Errors
    ///
    /// Returns the aggregated drain/release failures.
    pub fn close(&self) -> Result<(), PersistError> {
        let first = lock(&self.inner.close_slot).as_ref().is_none();
        let slot = if first {
            let slot = Arc::new(CloseSlot::new());
            lock(&self.inner.close_slot).get_or_insert(Arc::clone(&slot));
            slot
        } else {
            lock(&self.inner.close_slot)
                .as_ref()
                .map_or_else(|| Arc::new(CloseSlot::new()), Arc::clone)
        };
        if first {
            self.inner.closing.store(true, Ordering::Release);
            let sender = self.inner.ops_tx.clone();
            if sender
                .send(Op::Close {
                    slot: Arc::clone(&slot),
                })
                .is_err()
            {
                // The worker is already gone (abandoned handle): nothing to
                // drain and the lease died with its owner.
                slot.complete(Ok(()));
            }
        }
        slot.wait()
    }

    fn assert_open(&self, operation: &str) -> Result<(), PersistError> {
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(closed_error(&self.inner.id, operation));
        }
        Ok(())
    }

    /// Serializes one public mutating operation: queues it behind the
    /// worker and waits for its settlement (one dsh chain turn).
    fn call(&self, operation: &str, build: impl FnOnce(OpReply) -> Op) -> Result<(), PersistError> {
        let (reply, receiver) = mpsc::channel();
        self.inner
            .ops_tx
            .send(build(reply))
            .map_err(|_send_error| closed_error(&self.inner.id, operation))?;
        receiver
            .recv()
            .map_err(|_recv_error| closed_error(&self.inner.id, operation))?
    }

    /// Reads one slice out of the prepared stored prefix, advancing the
    /// monotonic observed length (dsh `readPrimed`).
    fn slice_primed(
        observed_length: &mut u64,
        events: Vec<SessionEvent>,
        offset: u64,
        length: u64,
    ) -> HandleRead {
        let primed_length = events.len() as u64;
        if primed_length > *observed_length {
            *observed_length = primed_length;
        }
        HandleRead {
            events: slice_events(events, offset, length),
        }
    }

    /// Reads one slice from the current physical generation and enforces
    /// this handle's monotonic view (dsh `readCurrent`).
    fn read_current(&self, offset: u64, length: u64) -> Result<HandleRead, PersistError> {
        let Some(path) = self.inner.backend.resolve_current_log(&self.inner.id)? else {
            // The plan resolved a path moments ago; a concurrent eviction
            // falls back to the pending/absent rules of the resolve arm.
            if self.inner.backend.has_pending_session(&self.inner.id) {
                return Ok(HandleRead { events: Vec::new() });
            }
            return Err(PersistError::NotFound(format!(
                "session \"{}\" not found",
                self.inner.id.as_str()
            )));
        };
        self.read_current_at(&path, offset, length)
    }

    fn read_current_at(
        &self,
        path: &Path,
        offset: u64,
        length: u64,
    ) -> Result<HandleRead, PersistError> {
        let stored = self
            .inner
            .backend
            .read_stored_log_for_handle(&self.inner.id, path)?;
        let stored_length = stored.events.len() as u64;
        {
            let mut shared = lock(&self.inner.shared);
            if stored_length < shared.observed_length {
                return Err(PersistError::Corruption(format!(
                    "session \"{}\": stored log shrank below a previously observed prefix \
({stored_length} < {})",
                    self.inner.id.as_str(),
                    shared.observed_length
                )));
            }
            shared.observed_length = stored_length;
        }
        Ok(HandleRead {
            events: slice_events(stored.events, offset, length),
        })
    }
}

/// Slices one owned event vector to `[offset, offset + length)`.
fn slice_events(events: Vec<SessionEvent>, offset: u64, length: u64) -> Vec<SessionEvent> {
    let offset = usize::try_from(offset).unwrap_or(usize::MAX);
    let length = usize::try_from(length).unwrap_or(usize::MAX);
    events.into_iter().skip(offset).take(length).collect()
}

/// Process-unique handle identity counter (the tracker's bookkeeping key).
static NEXT_HANDLE_UID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_handle_uid() -> u64 {
    NEXT_HANDLE_UID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// The closed-handle refusal text (dsh `SessionHandleClosedError`).
fn closed_error(id: &SessionId, operation: &str) -> PersistError {
    PersistError::HandleClosed(format!(
        "session \"{}\": {operation} on a closed handle",
        id.as_str()
    ))
}

/// Locks one of the handle's tiny shared cells, recovering from poison: the
/// cells hold plain field swaps whose invariants survive a panic
/// mid-section.
pub(crate) fn lock<T>(cell: &Mutex<T>) -> MutexGuard<'_, T> {
    cell.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn worker_dispatch(inner: &HandleInner, core: &mut HandleCore, op: Op) {
    match op {
        Op::Append { batch, reply } => {
            // The worker-side refusal closes the enqueue-vs-close race the
            // way dsh's in-chain re-check does (storage.ts:364-370).
            let outcome = if inner.closing.load(Ordering::Acquire) {
                Err(closed_error(&inner.id, "append"))
            } else {
                persist_contiguous(inner, core, &batch)
            };
            let _ = reply.send(outcome);
        }
        Op::Flush { reply } => {
            let outcome = if inner.closing.load(Ordering::Acquire) {
                Err(closed_error(&inner.id, "flush"))
            } else {
                worker_flush(inner, core)
            };
            let _ = reply.send(outcome);
        }
        Op::Drain { reply } => {
            // Drains carry no closed-handle refusal (dsh `enqueueChain`):
            // close itself drains through the same body.
            let _ = reply.send(drain_buffered(inner, core));
        }
        Op::DrainBackground => {
            if let Err(error) = drain_buffered(inner, core) {
                let text = format!(
                    "session-persistence: background write for session \"{}\" failed \
(buffered events retained): {error}",
                    inner.id.as_str()
                );
                if let Some(report) = &inner.report {
                    report(text);
                }
            }
        }
        Op::Close { slot } => worker_close(inner, core, &slot),
    }
}

/// The shared durable-append body (dsh `persistContiguous`): ownership,
/// contiguity, torn-tail repair, storage write, state advance.
fn persist_contiguous(
    inner: &HandleInner,
    core: &mut HandleCore,
    batch: &[SessionEvent],
) -> Result<(), PersistError> {
    if inner.access != SessionAccess::Write {
        return Err(PersistError::ReadOnly(format!(
            "session \"{}\": append is not available on a read handle",
            inner.id.as_str()
        )));
    }
    if batch.is_empty() {
        return Ok(());
    }
    // Hold the cross-process lock before this session's first durable
    // write; a create handle keeps it through close even when the write
    // then fails (dsh `ensureLease`).
    if core.lease.is_none() {
        core.lease = Some(inner.backend.acquire_write_lease(&inner.header)?);
    }
    assert_contiguous(&inner.id, batch, core.cursor)?;
    // Commit any pending torn-tail repair first, clearing each step's state
    // only once it lands so a failed step retries on the next mutation.
    if let Some(truncate_to) = core.torn_truncate_to {
        inner
            .backend
            .truncate_torn_tail(&inner.header, truncate_to)?;
        core.torn_truncate_to = None;
    }
    if let Some(recovered) = core.recovered_tail.take()
        && !recovered.is_empty()
    {
        inner.backend.persist_batch(
            &inner.header,
            &recovered,
            core.materialized,
            inner.inherited_event_count,
        )?;
    }
    inner.backend.persist_batch(
        &inner.header,
        batch,
        core.materialized,
        inner.inherited_event_count,
    )?;
    core.materialized = true;
    core.cursor += batch.len() as u64;
    {
        let mut shared = lock(&inner.shared);
        shared.primed = None;
        shared.materialized = true;
        shared.observed_length = core.cursor;
    }
    Ok(())
}

/// The explicit flush body (dsh flush op): read handles refuse; an already
/// materialized session is durable on append resolution; an empty created
/// session materializes its header so the explicit flush survives.
fn worker_flush(inner: &HandleInner, core: &mut HandleCore) -> Result<(), PersistError> {
    if inner.access != SessionAccess::Write {
        return Err(PersistError::ReadOnly(format!(
            "session \"{}\": flush is not available on a read handle",
            inner.id.as_str()
        )));
    }
    if core.materialized {
        return Ok(());
    }
    if core.lease.is_none() {
        core.lease = Some(inner.backend.acquire_write_lease(&inner.header)?);
    }
    inner
        .backend
        .persist_header(&inner.header, inner.inherited_event_count)?;
    core.materialized = true;
    lock(&inner.shared).materialized = true;
    Ok(())
}

/// Durably drains the routed live buffer (dsh `drainBuffered`): one
/// spliced batch per chained turn so events landing mid-write coalesce
/// into the next one, in order; a failure re-inserts its batch at the
/// front, pauses timer-driven drains, and reports.
fn drain_buffered(inner: &HandleInner, core: &mut HandleCore) -> Result<(), PersistError> {
    inner.timer.disarm();
    inner.drain_paused.store(false, Ordering::Release);
    loop {
        let batch = std::mem::take(&mut *lock(&inner.buffered));
        if batch.is_empty() {
            return Ok(());
        }
        if let Err(error) = persist_contiguous(inner, core, &batch) {
            lock(&inner.buffered).splice(0..0, batch);
            inner.drain_paused.store(true, Ordering::Release);
            return Err(error);
        }
    }
}

/// The close body (dsh close, storage.ts:223-260): drain until a full pass
/// leaves the routed buffer empty, then free the lease; both failures
/// together fail as one aggregate with the dsh message.
fn worker_close(inner: &HandleInner, core: &mut HandleCore, slot: &Arc<CloseSlot>) {
    let mut drain_failure = None;
    loop {
        match drain_buffered(inner, core) {
            Ok(()) => {}
            Err(error) => {
                drain_failure = Some(error);
                break;
            }
        }
        if lock(&inner.buffered).is_empty() {
            break;
        }
    }
    let mut failures: Vec<PersistError> = Vec::new();
    if let Some(error) = drain_failure {
        failures.push(error);
    }
    if let Some(mut lease) = core.lease.take()
        && let Err(release_failure) = lease.release()
    {
        failures.push(release_failure);
    }
    let outcome = match failures.len() {
        0 => Ok(()),
        1 => Err(failures.remove(0)),
        _ => Err(PersistError::Aggregate {
            message: format!(
                "session \"{}\": close failed to drain and to release its write lock",
                inner.id.as_str()
            ),
            errors: failures,
        }),
    };
    inner.backend.release_handle(inner, core.materialized);
    slot.complete(outcome);
}

/// Refuses a batch that does not contiguously continue the stored log
/// (dsh `assertContiguous`, storage-contract.ts:145-151).
fn assert_contiguous(
    id: &SessionId,
    events: &[SessionEvent],
    cursor: u64,
) -> Result<(), PersistError> {
    for (index, event) in events.iter().enumerate() {
        let expected = cursor + index as u64;
        if event.seq.get() != expected {
            return Err(PersistError::InvalidInput(format!(
                "append seq mismatch for \"{}\": expected {expected} at index {index}, got {}",
                id.as_str(),
                event.seq.get()
            )));
        }
    }
    Ok(())
}

// Rust guideline compliant 2026-10-23
