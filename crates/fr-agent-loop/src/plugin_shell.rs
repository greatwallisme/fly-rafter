// Rust guideline compliant 2026-10-27
//! The C-ABI dylib plugin shell (PRD 4.5): fr-agent-loop's artifact
//! surface over the dougong dynamic channel.
//!
//! Kernel-family edge: this module and [`crate::outbound`] are the only
//! `dougong-abi` consumers; the rest of the crate stays a plain rlib.
//!
//! This is the first fr plugin that retains the ABI v3 outbound vtable:
//! `setup` copies `HostExports` by value into the shell state (callable
//! until `teardown` returns) and loop threads receive [`Outbound`] copies
//! built from it. Inbound agent-service entries are
//! quick-enqueue-then-wake (PRD decision 46, Q79-A): a host `call` never
//! runs control flow; the session's loop thread drains its mailbox and
//! owns every durable mutation (inbox splice persistence, abort latch).
//!
//! The event listeners obey the on-event fast-path invariant: no
//! `require`, no blocking — parse the notification, raise the session's
//! one-bit pending wake, return. `llm/chunk-ready` carries
//! `{session_id, generation_id}`; `tools/settled` carries only the
//! settling call's token (no session routing), so its wake fans out to
//! every session mailbox. Ordering, duplication, and loss are all
//! harmless because the loop thread pulls the real facts from its
//! authoritative sources — the provider's per-generation buffer and the
//! tools settlement queue — and the wake is a single pending bit, not a
//! fact cache (Q84-B2).
//!
//! Thread discipline: every plugin-owned thread is joined before
//! `teardown` returns (vtable calls after `teardown` are documented
//! undefined behavior). The driver-body batch fills the driver registry;
//! this shell performs the shutdown-broadcast-and-join half.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};

use dougong_abi::{ABI_VERSION, HostExports, STATUS_CAPACITY, STATUS_FAILED, STATUS_OK, write_out};
use fr_wire::message::Message;
use serde::Deserialize;

use crate::agent_events::AgentEvents;
use crate::driver::{AgentOptions, LoopDriver};
use crate::driver_factory::{production_deps, spawn_driver, validated_max_tokens};
use crate::inbox::InboxTarget;
use crate::outbound::Outbound;
use crate::session_reads::SessionReadsClient;

/// The provided service name consumers call, and the seven method
/// selectors of the `'agent'` face — the constants live in the
/// shell-free [`crate::agent_service`] module (dependent artifacts share
/// them without pulling this shell's FFI export object into their
/// link); the re-exports keep this path stable.
pub use crate::agent_service::{
    METHOD_CANCEL, METHOD_CREATE, METHOD_DESCRIBE, METHOD_FOLLOWUP, METHOD_INJECT, METHOD_LIVE,
    METHOD_MAINTENANCE_BEGIN, METHOD_MAINTENANCE_END, METHOD_MAINTENANCE_POLL, METHOD_RESUME,
    METHOD_SEND, METHOD_STATUS, METHOD_STEER, SERVICE_NAME,
};

/// The dylib artifact's plugin name.
pub const PLUGIN_NAME: &str = "fr-agent-loop";

/// Event id the descriptor's listen map binds to `llm/chunk-ready`.
pub const EV_CHUNK_READY: u32 = 1;

/// The provider stream notification event name (Q84-B2).
pub const EVENT_CHUNK_READY: &str = "llm/chunk-ready";

/// Event id the descriptor's listen map binds to `tools/settled`.
pub const EV_TOOLS_SETTLED: u32 = 2;

/// The tool settle notification event name (the Q84-B2 pattern applied to
/// the tools fabric; the tools service emits it from its dispatch worker).
pub const EVENT_TOOLS_SETTLED: &str = "tools/settled";

/// The descriptor the loader consumes: provides the `agent` service,
/// injects the five consumed services, listens for the provider stream
/// and tool settle notifications. The version is sourced from crate
/// metadata so it cannot drift from the artifact.
const DESCRIPTOR: &[u8] = concat!(
    "{\"name\":\"fr-agent-loop\",\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\",\"provide\":[\"agent\"],\"inject\":[\"llm\",\"tools\",\"system-prompt\",",
    "\"session-persistence\"],\"listen\":{\"llm/chunk-ready\":1,\"tools/settled\":2}}"
)
.as_bytes();

/// The durable abort-cause taxonomy (dsh agent.ts `abortedCancelCause`):
/// the four shapes a cancel cause takes on the session log. The wire
/// authority is the fr-session slice A/B event vocabulary.
pub use fr_wire::event::CancelCause;

/// One queued inbound command. Durable application — the inbox splice via
/// [`crate::inbox::plan_splice`], its persistence, abort-latch arming —
/// belongs to the session's loop thread; the shell only enqueues.
#[derive(Debug, Clone, PartialEq)]
pub enum InboundCommand {
    /// An input headed for an inbox queue; `wakeup` mirrors dsh `send`'s
    /// flag (steer and followup wake the driver, inject does not).
    Input {
        /// Which inbox queue the input targets.
        target: InboxTarget,
        /// The queued user message.
        message: Message,
        /// Whether the entry wakes the driver.
        wakeup: bool,
    },
    /// A cancellation request.
    Cancel {
        /// The durable cause taxonomy value.
        cause: CancelCause,
        /// Whether pending inbox work survives the cancel.
        keep_inbox: bool,
    },
    /// The remote maintenance window's convergence rider (the wire face
    /// of dsh `runMaintenance`'s `finally`): the window is already closed
    /// shell-side; the driver commits the idle phase and replays a
    /// latched wake. A no-op when the window closed before the driver
    /// ever mirrored it.
    MaintenanceEnd,
}

/// Why the loop's retry-backoff park ended (the `wait_retry_backoff`
/// outcome: the dsh `cancellableDelay` boolean widened to its three
/// sources).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryWait {
    /// The scheduled delay elapsed.
    Deadline,
    /// A queued cancel command was peeked.
    Aborted,
    /// The shell requested shutdown.
    Shutdown,
}

/// The per-session mailbox shared between the host-facing shell and the
/// session's loop thread.
///
/// One lock pairs with one wake condition, so inbound commands and stream
/// notifications share a single wait point.
#[derive(Debug)]
pub struct SessionMailbox {
    /// The activation's outbound seam copy the driver thread uses for
    /// `require`/`emit` (handed over at creation; `Copy`).
    outbound: Outbound,
    /// The loop's status snapshot the query face reads (decision 46); the
    /// driver's phase machine commits through this same shared cell, so
    /// the query face and the driver can never disagree.
    status: std::sync::Arc<crate::phase::StatusCell>,
    /// The declared agent route (dsh `AgentOptions.provider`/`.model`,
    /// fixed at construction): the query face's additive route fields
    /// (the 17f Q1 ruling). First write wins.
    route: Mutex<Option<RouteFacts>>,
    /// The remote maintenance window (the wire face of dsh
    /// `runMaintenance`): the shell's begin/poll/end faces and the
    /// driver's cancel arm share this one marker, and the driver mirrors
    /// it into its phase machine at its park points.
    window: Mutex<Option<WindowState>>,
    /// The transition mutex: the serialization domain shared by
    /// `maintenance_begin` (busy check + window insert) and the driver's
    /// idle-to-running commit (window mirror + phase commit). Holding it
    /// across both halves is what makes the admission atomic — without
    /// it, a turn starting between the status read and the window insert
    /// would run beside an open window and both sides would pass their
    /// checks (the dsh busy contract would break). Lock order: this
    /// mutex is always the outermost lock; `window` and `inner` may be
    /// taken while it is held, never the reverse.
    transition: Mutex<()>,
    inner: Mutex<MailboxInner>,
    wake: Condvar,
}

/// The declared route facts (dsh `agent.options`; both `None`-able —
/// empty means "not declared").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteFacts {
    /// The provider route key.
    pub provider: Option<String>,
    /// The model id.
    pub model: Option<String>,
}

/// One open maintenance window: the capability token the remote job
/// echoes back, plus the abort flag the driver's cancel arm sets.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowState {
    token: String,
    aborted: bool,
}

#[derive(Debug)]
struct MailboxInner {
    commands: VecDeque<InboundCommand>,
    /// The one-bit pending wake (Q84-B2): a signal that "a data source
    /// the driver polls has new facts", never a fact cache. Producers
    /// set it; `wait_step` consumes it one-shot, so a stale wake costs
    /// exactly one extra poll of the authoritative sources and never a
    /// busy loop.
    pending_wake: bool,
    shutdown: bool,
}

impl SessionMailbox {
    fn new(outbound: Outbound) -> Self {
        Self {
            outbound,
            status: std::sync::Arc::new(crate::phase::StatusCell::new()),
            route: Mutex::new(None),
            window: Mutex::new(None),
            transition: Mutex::new(()),
            inner: Mutex::new(MailboxInner {
                commands: VecDeque::new(),
                pending_wake: false,
                shutdown: false,
            }),
            wake: Condvar::new(),
        }
    }

    /// Returns the session driver's outbound seam copy.
    #[must_use]
    pub const fn outbound(&self) -> Outbound {
        self.outbound
    }

    /// The shared status snapshot the driver's phase machine commits
    /// through (the query face reads the same cell).
    #[must_use]
    pub fn status_cell(&self) -> std::sync::Arc<crate::phase::StatusCell> {
        std::sync::Arc::clone(&self.status)
    }

    /// Reads the loop's status snapshot (the query face's answer).
    #[must_use]
    pub fn status(&self) -> crate::phase::AgentStatus {
        self.status.load()
    }

    /// Publishes the declared agent route (dsh `AgentOptions`, fixed at
    /// construction): the first write wins and later writes are ignored,
    /// so a re-creation cannot reconfigure a live agent.
    ///
    /// # Panics
    ///
    /// Panics if a shared lock is poisoned (a holder panicked).
    pub fn set_route(&self, provider: Option<String>, model: Option<String>) {
        let mut route = self.route.lock().expect("route lock poisoned");
        if route.is_none() {
            *route = Some(RouteFacts { provider, model });
        }
    }

    /// Reads the declared route (the query face's additive fields).
    #[must_use]
    ///
    /// # Panics
    ///
    /// Panics if a shared lock is poisoned (a holder panicked).
    pub fn route(&self) -> Option<RouteFacts> {
        self.route.lock().expect("route lock poisoned").clone()
    }

    /// Opens the remote maintenance window (the dsh `runMaintenance`
    /// busy check): refuses while a turn runs (the shared status cell)
    /// or another window (in-process or remote) is open, then mints the
    /// capability token the remote job echoes back. The driver mirrors
    /// the window into its phase machine at its next park point.
    ///
    /// The busy check and the window insert run under the transition
    /// mutex, the same domain the driver's idle-to-running commit takes:
    /// "see an idle status, open a window" and "see an empty window,
    /// commit running" are therefore atomic against each other and can
    /// never interleave into a running turn beside an open window.
    ///
    /// # Errors
    ///
    /// Returns the verbatim dsh busy rejection when work is active.
    ///
    /// # Panics
    ///
    /// Panics if a shared lock is poisoned (a holder panicked).
    pub fn maintenance_begin(&self, session_id: &str) -> Result<String, String> {
        let _transition = self.transition.lock().expect("transition lock poisoned");
        if self.status() == crate::phase::AgentStatus::Running {
            return Err(format!("agent \"{session_id}\" already has active work"));
        }
        let mut window = self.window.lock().expect("window lock poisoned");
        if window.is_some() {
            return Err(format!("agent \"{session_id}\" already has active work"));
        }
        let token = uuid::Uuid::new_v4().to_string();
        *window = Some(WindowState {
            token: token.clone(),
            aborted: false,
        });
        drop(window);
        Ok(token)
    }

    /// Answers the window's abort flag (the wire face of the dsh job's
    /// `AbortSignal`): the driver's cancel arm sets it mid-window.
    ///
    /// # Errors
    ///
    /// Returns a rejection for an unknown or stale token.
    ///
    /// # Panics
    ///
    /// Panics if a shared lock is poisoned (a holder panicked).
    pub fn maintenance_poll(&self, token: &str) -> Result<bool, String> {
        let window = self.window.lock().expect("window lock poisoned");
        match window.as_ref() {
            Some(state) if state.token == token => Ok(state.aborted),
            _ => Err("no matching maintenance window".to_owned()),
        }
    }

    /// Closes the window shell-side and reports whether the closer owns
    /// it (the convergence command still follows: the driver commits the
    /// idle phase and replays a latched wake).
    ///
    /// # Errors
    ///
    /// Returns a rejection for an unknown or stale token.
    ///
    /// # Panics
    ///
    /// Panics if a shared lock is poisoned (a holder panicked).
    pub fn maintenance_end(&self, token: &str) -> Result<(), String> {
        let mut window = self.window.lock().expect("window lock poisoned");
        match window.as_ref() {
            Some(state) if state.token == token => {
                *window = None;
                drop(window);
                Ok(())
            }
            _ => {
                drop(window);
                Err("no matching maintenance window".to_owned())
            }
        }
    }

    /// Whether a maintenance window is open (the driver's mirror guard).
    #[must_use]
    ///
    /// # Panics
    ///
    /// Panics if a shared lock is poisoned (a holder panicked).
    pub fn window_active(&self) -> bool {
        self.window.lock().expect("window lock poisoned").is_some()
    }

    /// Arms the window's abort flag (the driver's cancel arm; the dsh
    /// signal's mid-flight face — the remote job's poll observes it).
    ///
    /// # Panics
    ///
    /// Panics if a shared lock is poisoned (a holder panicked).
    pub fn mark_window_aborted(&self) {
        if let Some(state) = self.window.lock().expect("window lock poisoned").as_mut() {
            state.aborted = true;
        }
    }

    /// Queues one inbound command and wakes the driver when the command
    /// calls for it: waking inputs (`wakeup: true`) and cancels notify;
    /// a non-waking injection stays quiet (dsh `inject` never initiates
    /// driver activity). A cancel must wake the driver because the abort
    /// latch is applied loop-side in this architecture, unlike dsh's
    /// synchronous signal (Q79-A structural consequence).
    ///
    /// # Panics
    ///
    /// Panics if the mailbox lock is poisoned (a holder panicked).
    pub fn enqueue(&self, command: InboundCommand) {
        let wakes = matches!(
            &command,
            InboundCommand::Input { wakeup: true, .. }
                | InboundCommand::Cancel { .. }
                | InboundCommand::MaintenanceEnd
        );
        {
            let mut inner = self.inner.lock().expect("mailbox lock poisoned");
            inner.commands.push_back(command);
        }
        if wakes {
            self.wake.notify_all();
        }
    }

    /// Raises the session's one-bit pending wake (a `llm/chunk-ready`
    /// announced itself: a buffered batch may now be drainable).
    /// Ordering, duplication, and loss are all harmless (Q84-B2).
    ///
    /// # Panics
    ///
    /// Panics if the mailbox lock is poisoned (a holder panicked).
    pub fn notify_chunks(&self) {
        self.notify_wake();
    }

    /// Raises the session's one-bit pending wake (a dispatched tool call
    /// settled: a polled dispatch may now have its outcome).
    ///
    /// # Panics
    ///
    /// Panics if the mailbox lock is poisoned (a holder panicked).
    pub fn notify_tool_settled(&self) {
        self.notify_wake();
    }

    /// Raises the one-bit pending wake and wakes the parked driver.
    ///
    /// # Panics
    ///
    /// Panics if the mailbox lock is poisoned (a holder panicked).
    fn notify_wake(&self) {
        {
            let mut inner = self.inner.lock().expect("mailbox lock poisoned");
            inner.pending_wake = true;
        }
        self.wake.notify_all();
    }

    /// Marks the mailbox shut down and wakes the driver so it can exit;
    /// `teardown` broadcasts this to every session before joining.
    ///
    /// # Panics
    ///
    /// Panics if the mailbox lock is poisoned (a holder panicked).
    pub fn request_shutdown(&self) {
        {
            let mut inner = self.inner.lock().expect("mailbox lock poisoned");
            inner.shutdown = true;
        }
        self.wake.notify_all();
    }

    /// Takes every queued command, in enqueue order.
    ///
    /// # Panics
    ///
    /// Panics if the mailbox lock is poisoned (a holder panicked).
    pub fn drain_commands(&self) -> Vec<InboundCommand> {
        let mut inner = self.inner.lock().expect("mailbox lock poisoned");
        inner.commands.drain(..).collect()
    }

    /// Absorbs a pending wake without acting on it (the idle driver
    /// consumes no notifications — Q84-B2 loss is harmless). Returns
    /// whether a wake was pending.
    ///
    /// # Panics
    ///
    /// Panics if the mailbox lock is poisoned (a holder panicked).
    pub fn absorb_wake(&self) -> bool {
        let mut inner = self.inner.lock().expect("mailbox lock poisoned");
        std::mem::replace(&mut inner.pending_wake, false)
    }

    /// Parks the calling driver thread until an inbound command or the
    /// shutdown flag arrives (the idle wait point). A stray notification
    /// wakes the thread, which finds no command and parks again — the
    /// pending wake bit is not a wait-idle condition; the run loop
    /// absorbs strays at the idle boundary (Q84-B2 loss is harmless).
    ///
    /// # Panics
    ///
    /// Panics if the mailbox lock is poisoned (a holder panicked).
    pub fn wait_idle(&self) {
        let mut inner = self.inner.lock().expect("mailbox lock poisoned");
        while inner.commands.is_empty() && !inner.shutdown {
            inner = self.wake.wait(inner).expect("mailbox wait");
        }
        drop(inner);
    }

    /// Parks the calling driver thread until any wake source arrives — a
    /// command, the one-bit pending wake, or the shutdown flag (the
    /// in-step wait point: chunk batches, tool settlements, and cancels
    /// all proceed). The wake bit is consumed one-shot on exit: a stale
    /// wake costs the loop exactly one extra poll of its authoritative
    /// data sources, never a busy spin.
    ///
    /// # Panics
    ///
    /// Panics if the mailbox lock is poisoned (a holder panicked).
    pub fn wait_step(&self) {
        let mut inner = self.inner.lock().expect("mailbox lock poisoned");
        while !inner.pending_wake && inner.commands.is_empty() && !inner.shutdown {
            inner = self.wake.wait(inner).expect("mailbox wait");
        }
        inner.pending_wake = false;
        drop(inner);
    }

    /// Parks the calling driver thread until the retry-backoff deadline,
    /// a queued cancel command, or the shutdown flag (the loop-side twin
    /// of the dsh `cancellableDelay`: the two-stage carrier keeps the
    /// wait on the loop thread, so cancel breaks through immediately).
    /// Commands are peeked, never consumed — the run loop applies them
    /// after the wait ends. Stray notifications (chunk wakes with no
    /// in-flight stream) re-arm the remaining wait.
    ///
    /// # Panics
    ///
    /// Panics if the mailbox lock is poisoned (a holder panicked).
    pub fn wait_retry_backoff(&self, deadline: std::time::Instant) -> RetryWait {
        let mut inner = self.inner.lock().expect("mailbox lock poisoned");
        loop {
            if inner
                .commands
                .iter()
                .any(|command| matches!(command, InboundCommand::Cancel { .. }))
            {
                return RetryWait::Aborted;
            }
            if inner.shutdown {
                return RetryWait::Shutdown;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return RetryWait::Deadline;
            }
            let (guard, _timed_out) = self
                .wake
                .wait_timeout(inner, deadline - now)
                .expect("mailbox wait");
            inner = guard;
        }
    }

    /// Runs one step with the transition mutex held — the serialization
    /// domain shared with `maintenance_begin`. The driver's idle-to-running
    /// commit takes this so its "mirror the window, then commit running"
    /// half is atomic against "check the status, then open the window";
    /// the only work under the mutex is an atomic cell write and the
    /// phase commit's fire-and-forget status emit (decision 46: emit
    /// never blocks).
    ///
    /// # Panics
    ///
    /// Panics if the transition lock is poisoned (a holder panicked).
    pub fn with_turn_transition<R>(&self, step: impl FnOnce() -> R) -> R {
        let _transition = self.transition.lock().expect("transition lock poisoned");
        step()
    }

    /// Reports whether shutdown was requested.
    ///
    /// # Panics
    ///
    /// Panics if the mailbox lock is poisoned (a holder panicked).
    pub fn is_shutdown(&self) -> bool {
        self.inner.lock().expect("mailbox lock poisoned").shutdown
    }
}

/// The activation-scoped session registry: the copied vtable (as an
/// [`Outbound`] seam) plus one mailbox per session.
///
/// The shell creates a mailbox on first contact; the driver thread
/// attaches through the same helper when the session starts, so both
/// sides share one queue per session and every mailbox carries the same
/// outbound seam.
#[derive(Debug)]
pub struct SessionMap {
    outbound: Outbound,
    sessions: Mutex<HashMap<String, Arc<SessionMailbox>>>,
}

impl SessionMap {
    /// Builds the registry for one activation.
    #[must_use]
    pub fn new(outbound: Outbound) -> Self {
        Self {
            outbound,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Raises the one-bit tool-settle wake on every session mailbox. The
    /// `tools/settled` event carries only the settling call's token — no
    /// session routing — and the on-event fast path never calls
    /// `require` (decision 46), so the wake fans out: a driver without
    /// pending settlements absorbs it with one empty poll of its own
    /// settlement queue and parks again (Q84-B2: duplication and loss
    /// are harmless for a one-bit wake).
    ///
    /// # Panics
    ///
    /// Panics if the session map lock is poisoned (a holder panicked).
    pub fn notify_tool_settled(&self) {
        let sessions = self.sessions.lock().expect("session map lock poisoned");
        for mailbox in sessions.values() {
            mailbox.notify_tool_settled();
        }
    }
}

/// Returns the session's mailbox, creating it (seeded with the
/// activation's outbound seam) on first contact.
///
/// # Panics
///
/// Panics if the session map lock is poisoned (a holder panicked).
pub fn session_mailbox(map: &SessionMap, session_id: &str) -> Arc<SessionMailbox> {
    let mut sessions = map.sessions.lock().expect("session map lock poisoned");
    let mailbox = Arc::clone(
        sessions
            .entry(session_id.to_owned())
            .or_insert_with(|| Arc::new(SessionMailbox::new(map.outbound))),
    );
    drop(sessions);
    mailbox
}

/// `send` payload: full control over target and wake (dsh `send`).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendRequest {
    session_id: String,
    message: Message,
    target: InboxTarget,
    wakeup: bool,
}

/// `steer` / `inject` / `followup` payload: the entry fixes target and
/// wake (dsh: steer = next-step + wake, inject = next-step + no wake,
/// followup = next-turn + wake).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MessageRequest {
    session_id: String,
    message: Message,
}

/// `cancel` payload (dsh `cancel(cause, { keepInbox })`).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelRequest {
    session_id: String,
    cause: CancelCause,
    #[serde(default)]
    keep_inbox: bool,
}

/// `status` payload (the decision-46 query face).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StatusRequest {
    session_id: String,
}

/// `maintenance-begin` payload (the remote job's window admission).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionRequest {
    session_id: String,
}

/// `maintenance-poll` / `maintenance-end` payload: the window's
/// capability token echo.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WindowRequest {
    session_id: String,
    token: String,
}

/// `create` payload (the dsh `agents.create` wire projection; the route
/// fields are the Q9A ruling — the four dsh `agentOptions` fields).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateRequest {
    session_id: String,
    cwd: String,
    provider: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<fr_wire::ReasoningEffortId>,
    max_tokens: Option<u64>,
}

/// `resume` payload (the dsh `agents.resume` wire projection; the route
/// fields are the Q9A ruling — the four dsh `agentOptions` fields).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResumeRequest {
    session_id: String,
    provider: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<fr_wire::ReasoningEffortId>,
    max_tokens: Option<u64>,
}

fn parse<T: serde::de::DeserializeOwned>(payload: &[u8], entry: &str) -> Result<T, String> {
    serde_json::from_slice(payload).map_err(|error| format!("{entry} payload is invalid: {error}"))
}

/// Serves one agent-service method invocation: a pure (JSON-in, JSON-out)
/// function over the session map so the shell surface stays testable.
///
/// # Errors
///
/// Returns the failure text the host surfaces as `STATUS_FAILED`.
pub fn dispatch(method: u32, payload: &[u8], sessions: &SessionMap) -> Result<Vec<u8>, String> {
    match method {
        METHOD_SEND => {
            let request: SendRequest = parse(payload, "send")?;
            session_mailbox(sessions, &request.session_id).enqueue(InboundCommand::Input {
                target: request.target,
                message: request.message,
                wakeup: request.wakeup,
            });
        }
        METHOD_STEER => {
            let request: MessageRequest = parse(payload, "steer")?;
            session_mailbox(sessions, &request.session_id).enqueue(InboundCommand::Input {
                target: InboxTarget::NextStep,
                message: request.message,
                wakeup: true,
            });
        }
        METHOD_INJECT => {
            let request: MessageRequest = parse(payload, "inject")?;
            session_mailbox(sessions, &request.session_id).enqueue(InboundCommand::Input {
                target: InboxTarget::NextStep,
                message: request.message,
                wakeup: false,
            });
        }
        METHOD_FOLLOWUP => {
            let request: MessageRequest = parse(payload, "followup")?;
            session_mailbox(sessions, &request.session_id).enqueue(InboundCommand::Input {
                target: InboxTarget::NextTurn,
                message: request.message,
                wakeup: true,
            });
        }
        METHOD_CANCEL => {
            let request: CancelRequest = parse(payload, "cancel")?;
            session_mailbox(sessions, &request.session_id).enqueue(InboundCommand::Cancel {
                cause: request.cause,
                keep_inbox: request.keep_inbox,
            });
        }
        METHOD_DESCRIBE => {
            return Ok(serde_json::to_vec(&serde_json::json!({
                "name": PLUGIN_NAME,
                "service": SERVICE_NAME,
                "injects": [
                    "llm", "tools", "system-prompt", "session-persistence"
                ],
                "abiVersion": ABI_VERSION,
            }))
            .unwrap_or_default());
        }
        METHOD_STATUS => {
            let request: StatusRequest = parse(payload, "status")?;
            let mailbox = session_mailbox(sessions, &request.session_id);
            let status = mailbox.status();
            let route = mailbox.route();
            let mut reply = serde_json::json!({ "status": status });
            if let Some(route) = route {
                // Additive route fields (the 17f Q1 ruling): the dsh
                // summarizer reads `agent.options` as its third target
                // fallback; `None` stays absent (the dsh `undefined`).
                if let Some(provider) = route.provider {
                    reply["provider"] = serde_json::Value::String(provider);
                }
                if let Some(model) = route.model {
                    reply["model"] = serde_json::Value::String(model);
                }
            }
            return Ok(serde_json::to_vec(&reply).unwrap_or_default());
        }
        METHOD_MAINTENANCE_BEGIN => {
            let request: SessionRequest = parse(payload, "maintenance-begin")?;
            let token = session_mailbox(sessions, &request.session_id)
                .maintenance_begin(&request.session_id)?;
            return Ok(
                serde_json::to_vec(&serde_json::json!({ "token": token })).unwrap_or_default()
            );
        }
        METHOD_MAINTENANCE_POLL => {
            let request: WindowRequest = parse(payload, "maintenance-poll")?;
            let aborted =
                session_mailbox(sessions, &request.session_id).maintenance_poll(&request.token)?;
            return Ok(
                serde_json::to_vec(&serde_json::json!({ "aborted": aborted })).unwrap_or_default(),
            );
        }
        METHOD_MAINTENANCE_END => {
            let request: WindowRequest = parse(payload, "maintenance-end")?;
            let mailbox = session_mailbox(sessions, &request.session_id);
            mailbox.maintenance_end(&request.token)?;
            mailbox.enqueue(InboundCommand::MaintenanceEnd);
        }
        _ => return Err(format!("unknown agent method {method}")),
    }
    Ok(serde_json::to_vec(&serde_json::json!({ "queued": true })).unwrap_or_default())
}

/// Serves the lifecycle arms (CREATE/RESUME/LIVE — the driver-spawning
/// faces over the full shell state). Returns `None` for every other
/// method so the `call` entry falls through to the pure quick-enqueue
/// face ([`dispatch`]).
///
/// # Errors
///
/// Returns the failure text the host surfaces as `STATUS_FAILED`.
fn dispatch_lifecycle(
    method: u32,
    payload: &[u8],
    state: &ShellState,
) -> Option<Result<Vec<u8>, String>> {
    match method {
        METHOD_CREATE => Some(create_session_driver(state, payload)),
        METHOD_RESUME => Some(resume_session_driver(state, payload)),
        METHOD_LIVE => Some(live_probe(state, payload)),
        _ => None,
    }
}
/// The CREATE arm (the dsh `agents.create` wire face): guard the
/// driver registry, create the session through the persistence face,
/// assemble the production deps, restore-fold the driver, and spawn its
/// thread into the registry. The reply carries the session identity
/// (the spec's session-handle face).
///
/// A failure past the persistence create does not roll the session
/// back: the dsh rollback unwinds through cordis effects, and the wire
/// face has no session-dispose arm — the fail-closed shape keeps the
/// persisted log, so a restart resumes cleanly.
///
/// # Errors
///
/// Returns the registry, persistence, restore, or spawn refusal.
fn create_session_driver(state: &ShellState, payload: &[u8]) -> Result<Vec<u8>, String> {
    let request: CreateRequest = parse(payload, "create")?;
    guard_driver_registry(state, &request.session_id)?;
    let outbound = state.sessions.outbound;
    let create_body = serde_json::to_vec(&serde_json::json!({
        "id": request.session_id,
        "meta": { "cwd": request.cwd },
    }))
    .map_err(|error| format!("create payload encode failed: {error}"))?;
    let reply = outbound
        .require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_SESSION_CREATE,
            &create_body,
        )
        .map_err(|error| format!("session create failed: {error}"))?;
    let created: serde_json::Value = serde_json::from_slice(&reply)
        .map_err(|error| format!("session create reply decode failed: {error}"))?;
    let created_id = created
        .get("sessionId")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "session create reply carries no sessionId".to_owned())?;
    if created_id != request.session_id {
        return Err(format!(
            "session create reply names session \"{created_id}\", expected \"{}\"",
            request.session_id
        ));
    }
    attach_session_driver(
        state,
        &request.session_id,
        &request.cwd,
        AgentOptions {
            provider: request.provider,
            model: request.model,
            reasoning_effort: request.reasoning_effort,
            max_tokens: validated_max_tokens(request.max_tokens)?,
        },
    )
}
/// The RESUME arm (the dsh `agents.resume` wire face): guard the
/// registry, take the persistence write claim and crash-repair closers
/// (method 6 = the full `resumeWith` chain), read the session header
/// for the workspace path, and attach the restored driver.
///
/// # Errors
///
/// Returns the registry, persistence, header-read, restore, or spawn
/// refusal.
fn resume_session_driver(state: &ShellState, payload: &[u8]) -> Result<Vec<u8>, String> {
    let request: ResumeRequest = parse(payload, "resume")?;
    guard_driver_registry(state, &request.session_id)?;
    let outbound = state.sessions.outbound;
    let resume_body = serde_json::to_vec(&serde_json::json!({ "id": request.session_id }))
        .map_err(|error| format!("resume payload encode failed: {error}"))?;
    outbound
        .require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_SESSION_RESUME,
            &resume_body,
        )
        .map_err(|error| format!("session resume failed: {error}"))?;
    let cwd = SessionReadsClient::new(outbound, &request.session_id)
        .session_header()
        .map_err(|error| format!("session header read failed: {error}"))?
        .cwd
        .unwrap_or_default();
    attach_session_driver(
        state,
        &request.session_id,
        &cwd,
        AgentOptions {
            provider: request.provider,
            model: request.model,
            reasoning_effort: request.reasoning_effort,
            max_tokens: validated_max_tokens(request.max_tokens)?,
        },
    )
}
/// Builds the production driver over the created/claimed session,
/// spawns its thread, and registers the handle. The mailbox attaches
/// through the same helper the inbound faces use, so a send queued
/// before the attach drains into the live driver (decision 46).
///
/// # Errors
///
/// Returns the restore or spawn refusal.
fn attach_session_driver(
    state: &ShellState,
    session_id: &str,
    cwd: &str,
    options: AgentOptions,
) -> Result<Vec<u8>, String> {
    let mailbox = session_mailbox(&state.sessions, session_id);
    let events = Arc::new(AgentEvents::new(
        state.sessions.outbound,
        Arc::new(|warning: &str| eprintln!("fr: agent-loop: {warning}")),
    ));
    let driver = LoopDriver::new(
        session_id,
        options,
        Arc::clone(&mailbox),
        Arc::clone(&events),
        production_deps(state.sessions.outbound, session_id, cwd),
    )
    .map_err(|error| format!("driver restore failed: {error}"))?;
    let handle = spawn_driver(session_id, driver)?;
    state
        .drivers
        .lock()
        .expect("driver registry lock poisoned")
        .insert(session_id.to_owned(), handle);
    Ok(serde_json::to_vec(&serde_json::json!({
        "sessionId": session_id,
    }))
    .unwrap_or_default())
}

/// Refuses a second driver for one session id (the dsh registry insert
/// guard, index.ts:469; the registry check precedes all persistence
/// I/O).
///
/// # Errors
///
/// Returns the dsh verbatim replacement refusal.
fn guard_driver_registry(state: &ShellState, session_id: &str) -> Result<(), String> {
    if state
        .drivers
        .lock()
        .expect("driver registry lock poisoned")
        .contains_key(session_id)
    {
        return Err(format!("agent \"{session_id}\" is already registered"));
    }
    Ok(())
}

/// The LIVE arm (the dsh `agents.get` liveness read): reports whether a
/// driver thread is attached for the id. It never touches the session
/// map, so a probe cannot mint a mailbox (the spec's no-create pin).
///
/// # Errors
///
/// Returns the payload decode refusal.
fn live_probe(state: &ShellState, payload: &[u8]) -> Result<Vec<u8>, String> {
    let request: SessionRequest = parse(payload, "live")?;
    let live = state
        .drivers
        .lock()
        .expect("driver registry lock poisoned")
        .contains_key(&request.session_id);
    Ok(serde_json::to_vec(&serde_json::json!({ "live": live })).unwrap_or_default())
}

/// Handles one `llm/chunk-ready` notification (the on-event fast path:
/// no `require`, no blocking — PRD decision 46).
///
/// Raises the session's one-bit pending wake; the generation id
/// validates the wire shape but is never consumed — the loop pulls real
/// chunks from the provider's buffer.
///
/// # Errors
///
/// Returns the failure text for a malformed notification payload.
pub fn handle_chunk_ready(payload: &[u8], sessions: &SessionMap) -> Result<(), String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Notification {
        session_id: String,
        /// Pinned for wire-shape validation only (Q84-B2: the wake is a
        /// one-bit signal; the id is never consumed).
        #[expect(
            dead_code,
            reason = "wire-shape pin: the id validates the payload but is never consumed"
        )]
        generation_id: String,
    }
    let notification: Notification = serde_json::from_slice(payload)
        .map_err(|error| format!("{EVENT_CHUNK_READY} payload is invalid: {error}"))?;
    let Notification { session_id, .. } = notification;
    session_mailbox(sessions, &session_id).notify_chunks();
    Ok(())
}

/// Handles one `tools/settled` notification (the on-event fast path: no
/// `require`, no blocking — PRD decision 46).
///
/// Fans the one-bit pending wake out to every session mailbox: the
/// payload carries only the settling call's token — no session routing
/// — so each parked driver receives the wake and re-polls its own
/// settlement queue; a driver without pending settlements absorbs it
/// with one empty poll (Q84-B2: duplication and loss are harmless for a
/// one-bit wake).
///
/// # Errors
///
/// Returns the failure text for a malformed notification payload.
pub fn handle_tools_settled(payload: &[u8], sessions: &SessionMap) -> Result<(), String> {
    #[derive(Deserialize)]
    struct Notification {
        /// Pinned for wire-shape validation only (Q84-B2: the wake is a
        /// one-bit signal; the token routes nothing).
        #[expect(
            dead_code,
            reason = "wire-shape pin: the token validates the payload but routes nothing"
        )]
        token: u64,
    }
    let _notification: Notification = serde_json::from_slice(payload)
        .map_err(|error| format!("{EVENT_TOOLS_SETTLED} payload is invalid: {error}"))?;
    sessions.notify_tool_settled();
    Ok(())
}

struct ShellState {
    /// The activation-scoped registry: the copied vtable (retained as the
    /// [`Outbound`] seam) and the per-session mailboxes.
    sessions: SessionMap,
    /// Loop-thread handles by session id; `teardown` joins every entry
    /// before the state (and with it the vtable) is released.
    drivers: Mutex<HashMap<String, std::thread::JoinHandle<()>>>,
}

/// Broadcasts shutdown to every session mailbox, then joins every loop
/// thread. The broadcast happens before the joins so a parked driver
/// observes the flag and exits instead of keeping the join waiting; a
/// driver that panicked is already loud, so its join result is dropped
/// rather than re-panicking through teardown.
///
/// # Panics
///
/// Panics if either lock is poisoned (a holder panicked).
fn shutdown_and_join(
    map: &SessionMap,
    drivers: &Mutex<HashMap<String, std::thread::JoinHandle<()>>>,
) {
    let mailboxes: Vec<Arc<SessionMailbox>> = map
        .sessions
        .lock()
        .expect("session map lock poisoned")
        .values()
        .cloned()
        .collect();
    for mailbox in &mailboxes {
        mailbox.request_shutdown();
    }
    let drained: Vec<std::thread::JoinHandle<()>> = drivers
        .lock()
        .expect("driver registry lock poisoned")
        .drain()
        .map(|(_session, handle)| handle)
        .collect();
    for driver in drained {
        let _ = driver.join();
    }
}

unsafe extern "C" fn plugin_name(out: *mut u8, cap: usize, len: *mut usize) -> i32 {
    // SAFETY: the host guarantees a `cap`-sized out-buffer and a writable
    // `len` slot (export-table contract).
    if unsafe { write_out(PLUGIN_NAME.as_bytes(), out, cap, len) } {
        STATUS_OK
    } else {
        STATUS_CAPACITY
    }
}

unsafe extern "C" fn descriptor(out: *mut u8, cap: usize, len: *mut usize) -> i32 {
    // SAFETY: same host buffer contract as `plugin_name`.
    if unsafe { write_out(DESCRIPTOR, out, cap, len) } {
        STATUS_OK
    } else {
        STATUS_CAPACITY
    }
}

// ABI v3 hands the outbound `HostExports` vtable here. Unlike the
// fr-session and provider shells, this plugin consumes outbound calls, so
// the vtable is copied by value and retained for the activation's
// lifetime; every thread holding a copy (or an `Outbound` built from it)
// is joined before `teardown` returns (v3 thread discipline).
unsafe extern "C" fn setup(host: *const HostExports) -> *mut std::os::raw::c_void {
    if host.is_null() {
        // Without the vtable the loop threads cannot reach any consumed
        // service: fail the activation loudly instead of half-alive.
        return std::ptr::null_mut();
    }
    // SAFETY: the host exports pointer is valid for the duration of
    // `setup`; the vtable is copied by value (v3 contract).
    let host = unsafe { *host };
    let outbound = Outbound::new(host);
    // The D31 grant behind the completion-notice machine: the tools
    // plugin's notice worker requires `agent` status/inject/followup
    // through the grant (runtime grants, not a static inject — an absent
    // consumer fails the require immediately instead of parking a worker
    // thread; the 16a watcher judgment). The target must be committed,
    // so the assembler loads `fr-tools-builtin` before this artifact; a
    // composition without the consumer keeps the loop alive — the offer
    // lapses and the notice machine skips fail-closed.
    let _ignored = outbound.grant("builtin-tools", crate::agent_service::SERVICE_NAME);
    Box::into_raw(Box::new(ShellState {
        sessions: SessionMap::new(outbound),
        drivers: Mutex::new(HashMap::new()),
    }))
    .cast::<std::os::raw::c_void>()
}

unsafe extern "C" fn call(
    handle: *mut std::os::raw::c_void,
    method: u32,
    in_data: *const u8,
    in_len: usize,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    if handle.is_null() {
        return STATUS_FAILED;
    }
    // SAFETY: the host passes back a handle previously returned by `setup`
    // and not yet released via `teardown`.
    let state = unsafe { &mut *(handle.cast::<ShellState>()) };
    // The host contract delivers `in_data` valid for `in_len` readable
    // bytes; an empty payload arrives as null/0 and never dereferences the
    // pointer, which `from_raw_parts` requires non-null even for a
    // zero-length slice.
    let payload: &[u8] = if in_len == 0 || in_data.is_null() {
        &[]
    } else {
        // SAFETY: `in_len` is nonzero and `in_data` non-null here, valid
        // for `in_len` readable bytes per the host contract.
        unsafe { std::slice::from_raw_parts(in_data, in_len) }
    };
    let reply = match dispatch_lifecycle(method, payload, state)
        .unwrap_or_else(|| dispatch(method, payload, &state.sessions))
    {
        Ok(reply) => reply,
        Err(message) => {
            let body = serde_json::json!({ "error": message }).to_string();
            // SAFETY: host buffer contract.
            let _ = unsafe { write_out(body.as_bytes(), out, cap, out_len) };
            return STATUS_FAILED;
        }
    };
    // SAFETY: host buffer contract.
    if unsafe { write_out(&reply, out, cap, out_len) } {
        STATUS_OK
    } else {
        STATUS_CAPACITY
    }
}

// The two listened events (`llm/chunk-ready`, `tools/settled` — both
// emit dispatch): raise the one-bit wake and return. No `require`, no
// blocking — the on-event fast-path invariant.
unsafe extern "C" fn on_event(
    handle: *mut std::os::raw::c_void,
    event: u32,
    _mode: u32,
    in_data: *const u8,
    in_len: usize,
    _next_available: u32,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    if handle.is_null() {
        return STATUS_FAILED;
    }
    // SAFETY: live handle per the export-table contract.
    let state = unsafe { &mut *(handle.cast::<ShellState>()) };
    // Same empty-payload shape as `call` (null/0 pairs).
    let payload: &[u8] = if in_len == 0 || in_data.is_null() {
        &[]
    } else {
        // SAFETY: `in_len` is nonzero and `in_data` non-null here, valid
        // for `in_len` readable bytes per the host contract.
        unsafe { std::slice::from_raw_parts(in_data, in_len) }
    };
    let handled = match event {
        EV_CHUNK_READY => handle_chunk_ready(payload, &state.sessions),
        EV_TOOLS_SETTLED => handle_tools_settled(payload, &state.sessions),
        _ => return STATUS_FAILED,
    };
    match handled {
        Ok(()) => {
            // SAFETY: host buffer contract; an empty JSON object reply.
            if unsafe { write_out(b"{}", out, cap, out_len) } {
                STATUS_OK
            } else {
                STATUS_CAPACITY
            }
        }
        Err(message) => {
            let body = serde_json::json!({ "error": message }).to_string();
            // SAFETY: host buffer contract.
            let _ = unsafe { write_out(body.as_bytes(), out, cap, out_len) };
            STATUS_FAILED
        }
    }
}

// No waterfall listeners are declared, so the host never defers into
// `on_next`.
const unsafe extern "C" fn on_next(
    _handle: *mut std::os::raw::c_void,
    _event: u32,
    _chain_status: i32,
    _data: *const u8,
    _len: usize,
    _out: *mut u8,
    _cap: usize,
    _out_len: *mut usize,
) -> i32 {
    STATUS_FAILED
}

unsafe extern "C" fn teardown(handle: *mut std::os::raw::c_void) {
    if handle.is_null() {
        return;
    }
    // SAFETY: the handle was produced by `setup` and torn down exactly once.
    let state = unsafe { Box::from_raw(handle.cast::<ShellState>()) };
    shutdown_and_join(&state.sessions, &state.drivers);
}

mod export;

#[cfg(test)]
mod tests {
    use super::{
        CancelCause, InboundCommand, METHOD_CANCEL, METHOD_CREATE, METHOD_DESCRIBE,
        METHOD_FOLLOWUP, METHOD_INJECT, METHOD_LIVE, METHOD_MAINTENANCE_BEGIN,
        METHOD_MAINTENANCE_END, METHOD_MAINTENANCE_POLL, METHOD_RESUME, METHOD_SEND, METHOD_STATUS,
        METHOD_STEER, PLUGIN_NAME, SessionMap, ShellState, dispatch, dispatch_lifecycle,
        handle_chunk_ready, handle_tools_settled, session_mailbox, shutdown_and_join,
    };
    use crate::inbox::InboxTarget;
    use crate::outbound::Outbound;
    use dougong_abi::{HostExports, STATUS_FAILED, STATUS_OK};
    use fr_wire::message::Message;
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::sync::{Arc, Mutex};

    // Quiet test doubles so tests can build a vtable without a host.
    unsafe extern "C" fn quiet_require(
        _host: *mut c_void,
        _service: *const u8,
        _service_len: usize,
        _method: u32,
        _in_data: *const u8,
        _in_len: usize,
        _out_buf: *mut u8,
        _cap: usize,
        _out_len: *mut usize,
    ) -> i32 {
        STATUS_OK
    }

    unsafe extern "C" fn quiet_emit(
        _host: *mut c_void,
        _event: *const u8,
        _event_len: usize,
        _payload: *const u8,
        _payload_len: usize,
    ) -> i32 {
        STATUS_OK
    }

    fn empty_map() -> SessionMap {
        SessionMap::new(Outbound::new(HostExports {
            grant: crate::outbound::quiet_grant,
            dispatch: crate::outbound::noop_dispatch,
            host_handle: std::ptr::null_mut(),
            require: quiet_require,
            emit: quiet_emit,
        }))
    }

    fn user_message(text: &str) -> Message {
        serde_json::from_value(serde_json::json!({
            "role": "user",
            "id": format!("msg-{text}"),
            "content": [{ "type": "text", "text": text }],
            "source": { "kind": "user" },
        }))
        .expect("fixture message")
    }

    fn input_payload(message_text: &str) -> serde_json::Value {
        serde_json::json!({
            "sessionId": "s1",
            "message": {
                "role": "user",
                "id": format!("msg-{message_text}"),
                "content": [{ "type": "text", "text": message_text }],
                "source": { "kind": "user" },
            },
        })
    }

    #[test]
    fn descriptor_declares_the_loop_surface() {
        let parsed: serde_json::Value = serde_json::from_slice(super::DESCRIPTOR).expect("json");
        assert_eq!(parsed["name"], PLUGIN_NAME);
        assert_eq!(parsed["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(parsed["provide"], serde_json::json!(["agent"]));
        assert_eq!(
            parsed["inject"],
            serde_json::json!(["llm", "tools", "system-prompt", "session-persistence"])
        );
        assert_eq!(
            parsed["listen"],
            serde_json::json!({ "llm/chunk-ready": 1, "tools/settled": 2 })
        );
    }

    #[test]
    fn four_entries_queue_inputs_with_their_targets() {
        let map = empty_map();

        let mut send = input_payload("task");
        send["target"] = serde_json::json!("next-step");
        send["wakeup"] = serde_json::json!(true);
        dispatch(METHOD_SEND, send.to_string().as_bytes(), &map).expect("send");

        let steer = input_payload("steer");
        dispatch(METHOD_STEER, steer.to_string().as_bytes(), &map).expect("steer");

        let inject = input_payload("inject");
        dispatch(METHOD_INJECT, inject.to_string().as_bytes(), &map).expect("inject");

        let followup = input_payload("followup");
        dispatch(METHOD_FOLLOWUP, followup.to_string().as_bytes(), &map).expect("followup");

        assert_eq!(
            session_mailbox(&map, "s1").drain_commands(),
            vec![
                InboundCommand::Input {
                    target: InboxTarget::NextStep,
                    message: user_message("task"),
                    wakeup: true,
                },
                InboundCommand::Input {
                    target: InboxTarget::NextStep,
                    message: user_message("steer"),
                    wakeup: true,
                },
                InboundCommand::Input {
                    target: InboxTarget::NextStep,
                    message: user_message("inject"),
                    wakeup: false,
                },
                InboundCommand::Input {
                    target: InboxTarget::NextTurn,
                    message: user_message("followup"),
                    wakeup: true,
                },
            ]
        );
    }

    #[test]
    fn cancel_queues_the_durable_cause() {
        let map = empty_map();
        let payload = serde_json::json!({
            "sessionId": "s1",
            "cause": { "kind": "hook", "reason": "policy veto" },
            "keepInbox": true,
        });
        dispatch(METHOD_CANCEL, payload.to_string().as_bytes(), &map).expect("cancel");
        assert_eq!(
            session_mailbox(&map, "s1").drain_commands(),
            vec![InboundCommand::Cancel {
                cause: CancelCause::Hook {
                    reason: "policy veto".to_owned(),
                },
                keep_inbox: true,
            }]
        );
    }

    #[test]
    fn describe_and_unknown_methods() {
        let map = empty_map();
        let reply = dispatch(METHOD_DESCRIBE, b"{}", &map).expect("describe");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(reply["name"], PLUGIN_NAME);
        assert_eq!(reply["service"], "agent");
        let descriptor: serde_json::Value =
            serde_json::from_slice(super::DESCRIPTOR).expect("json");
        assert_eq!(reply["injects"], descriptor["inject"]);
        assert_eq!(reply["abiVersion"], dougong_abi::ABI_VERSION);

        assert!(
            dispatch(999, b"{}", &map)
                .expect_err("unknown method")
                .contains("unknown agent method 999")
        );
        let broken = input_payload("x");
        drop(broken);
        assert!(
            dispatch(METHOD_SEND, b"not json", &map)
                .expect_err("bad json")
                .contains("send payload is invalid")
        );
    }

    #[test]
    fn status_query_reads_the_shared_snapshot() {
        let map = empty_map();
        let payload = serde_json::json!({ "sessionId": "s1" });
        let reply = dispatch(METHOD_STATUS, payload.to_string().as_bytes(), &map).expect("status");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(reply, serde_json::json!({ "status": "idle" }));
        // A fresh session answers without materializing a driver: the cell
        // is seeded by the mailbox itself.
        let fresh = serde_json::json!({ "sessionId": "never-seen" });
        let reply = dispatch(METHOD_STATUS, fresh.to_string().as_bytes(), &map).expect("status");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(reply, serde_json::json!({ "status": "idle" }));
        assert!(
            dispatch(METHOD_STATUS, b"not json", &map)
                .expect_err("bad json")
                .contains("status payload is invalid")
        );
    }

    #[test]
    fn chunk_ready_wakes_the_session_and_coalesces_duplicates() {
        let map = empty_map();
        for generation in ["g1", "g1", "g2"] {
            let payload = serde_json::json!({
                "sessionId": "s7",
                "generationId": generation,
            });
            handle_chunk_ready(payload.to_string().as_bytes(), &map).expect("notification");
        }
        let other = serde_json::json!({ "sessionId": "s8", "generationId": "g9" });
        handle_chunk_ready(other.to_string().as_bytes(), &map).expect("other session");

        // The one-bit wake: repeated notifications coalesce into one
        // pending bit, consumed one-shot (Q84-B2 — a signal, not a fact
        // cache).
        let s7 = session_mailbox(&map, "s7");
        assert!(s7.absorb_wake());
        assert!(!s7.absorb_wake());
        let s8 = session_mailbox(&map, "s8");
        assert!(s8.absorb_wake());
        assert!(!s8.absorb_wake());
        assert_eq!(super::EV_CHUNK_READY, 1);
        assert_eq!(super::EVENT_CHUNK_READY, "llm/chunk-ready");

        assert!(
            handle_chunk_ready(br#"{"sessionId": "s7"}"#, &map)
                .expect_err("missing generation id")
                .contains("llm/chunk-ready payload is invalid")
        );
    }

    #[test]
    fn tools_settled_fans_the_wake_out_to_every_session() {
        let map = empty_map();
        // Both mailboxes exist before the event (the driver contacts its
        // mailbox at session start).
        let s7 = session_mailbox(&map, "s7");
        let s8 = session_mailbox(&map, "s8");

        handle_tools_settled(br#"{"token": 7}"#, &map).expect("notification");

        // No session routing in the payload, so both parked drivers get
        // the wake; each consumes it one-shot.
        assert!(s7.absorb_wake());
        assert!(!s7.absorb_wake());
        assert!(s8.absorb_wake());
        assert!(!s8.absorb_wake());
        assert_eq!(super::EV_TOOLS_SETTLED, 2);
        assert_eq!(super::EVENT_TOOLS_SETTLED, "tools/settled");

        assert!(
            handle_tools_settled(br#"{"sessionId": "s7"}"#, &map)
                .expect_err("missing token")
                .contains("tools/settled payload is invalid")
        );
    }

    #[test]
    fn mailboxes_carry_the_activation_outbound_seam() {
        let map = empty_map();
        let first = session_mailbox(&map, "s1");
        let again = session_mailbox(&map, "s1");
        assert!(Arc::ptr_eq(&first, &again));
        let _outbound = first.outbound();
        let _outbound_two = session_mailbox(&map, "s2").outbound();
    }

    #[test]
    fn shutdown_wakes_and_joins_the_driver_thread() {
        let map = empty_map();
        let mailbox = session_mailbox(&map, "s1");
        let parked = Arc::clone(&mailbox);
        let driver = std::thread::spawn(move || {
            // The wait shape the driver body uses: park on the mailbox's
            // single wake condition until shutdown is requested (private
            // field access is legal inside this module tree).
            let mut guard = parked.inner.lock().expect("driver lock");
            while !guard.shutdown {
                guard = parked.wake.wait(guard).expect("driver wait");
            }
            drop(guard);
        });
        let mut drivers = HashMap::new();
        drivers.insert("s1".to_owned(), driver);
        let drivers = Mutex::new(drivers);
        shutdown_and_join(&map, &drivers);
        assert!(mailbox.is_shutdown());
        assert_eq!(mailbox.drain_commands(), [] as [super::InboundCommand; 0]);
    }
    #[test]
    fn service_face_constants_match_the_wire_table() {
        use super::{
            METHOD_CANCEL, METHOD_DESCRIBE, METHOD_FOLLOWUP, METHOD_INJECT, METHOD_SEND,
            METHOD_STATUS, METHOD_STEER, SERVICE_NAME,
        };
        use fr_wire::services::agent as wire;
        assert_eq!(SERVICE_NAME, wire::SERVICE_NAME);
        assert_eq!(METHOD_SEND, wire::METHOD_SEND);
        assert_eq!(METHOD_STEER, wire::METHOD_STEER);
        assert_eq!(METHOD_INJECT, wire::METHOD_INJECT);
        assert_eq!(METHOD_FOLLOWUP, wire::METHOD_FOLLOWUP);
        assert_eq!(METHOD_CANCEL, wire::METHOD_CANCEL);
        assert_eq!(METHOD_DESCRIBE, wire::METHOD_DESCRIBE);
        assert_eq!(METHOD_STATUS, wire::METHOD_STATUS);
    }

    /// The query face carries the declared route as additive fields, and
    /// omits them while unset (the dsh `agent.options` fallback facts).
    #[test]
    fn status_reply_carries_the_declared_route() {
        let map = empty_map();
        let mailbox = session_mailbox(&map, "s1");
        mailbox.set_route(Some("deepseek".to_owned()), Some("m1".to_owned()));
        let reply = dispatch(METHOD_STATUS, br#"{"sessionId":"s1"}"#, &map).expect("status");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(reply["status"], "idle");
        assert_eq!(reply["provider"], "deepseek");
        assert_eq!(reply["model"], "m1");

        let unset = session_mailbox(&map, "s2");
        unset.set_route(None, None);
        let reply = dispatch(METHOD_STATUS, br#"{"sessionId":"s2"}"#, &map).expect("status");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(reply["status"], "idle");
        assert!(reply.get("provider").is_none());
        assert!(reply.get("model").is_none());
    }

    /// The remote maintenance window: begin mints the capability token,
    /// poll answers the abort flag the cancel arm sets, end closes and
    /// rides the convergence command, and stale tokens fail closed.
    #[test]
    fn maintenance_window_faces_pin_the_wire_semantics() {
        let map = empty_map();
        let mailbox = session_mailbox(&map, "s1");

        let reply =
            dispatch(METHOD_MAINTENANCE_BEGIN, br#"{"sessionId":"s1"}"#, &map).expect("begin");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        let token = reply["token"].as_str().expect("token").to_owned();

        let reply = dispatch(
            METHOD_MAINTENANCE_POLL,
            format!(r#"{{"sessionId":"s1","token":"{token}"}}"#)
                .into_bytes()
                .as_slice(),
            &map,
        )
        .expect("poll");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(reply["aborted"], false);

        mailbox.mark_window_aborted();
        let reply = dispatch(
            METHOD_MAINTENANCE_POLL,
            format!(r#"{{"sessionId":"s1","token":"{token}"}}"#)
                .into_bytes()
                .as_slice(),
            &map,
        )
        .expect("poll after cancel");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(reply["aborted"], true);

        dispatch(
            METHOD_MAINTENANCE_END,
            format!(r#"{{"sessionId":"s1","token":"{token}"}}"#)
                .into_bytes()
                .as_slice(),
            &map,
        )
        .expect("end");
        let drained = mailbox.drain_commands();
        assert_eq!(drained.len(), 1, "the convergence command rides the queue");
        assert!(matches!(drained[0], InboundCommand::MaintenanceEnd));

        let error = dispatch(
            METHOD_MAINTENANCE_END,
            format!(r#"{{"sessionId":"s1","token":"{token}"}}"#)
                .into_bytes()
                .as_slice(),
            &map,
        )
        .expect_err("stale end");
        assert_eq!(error, "no matching maintenance window");
        let error = dispatch(
            METHOD_MAINTENANCE_POLL,
            format!(r#"{{"sessionId":"s1","token":"{token}"}}"#)
                .into_bytes()
                .as_slice(),
            &map,
        )
        .expect_err("stale poll");
        assert_eq!(error, "no matching maintenance window");
    }

    /// The busy rejection is verbatim while a turn runs (the dsh
    /// `runMaintenance` throw).
    #[test]
    fn maintenance_begin_refuses_while_a_turn_runs() {
        let map = empty_map();
        let mailbox = session_mailbox(&map, "s1");
        let outbound = Outbound::new(HostExports {
            grant: crate::outbound::quiet_grant,
            dispatch: crate::outbound::identity_dispatch,
            host_handle: std::ptr::null_mut(),
            require: quiet_require,
            emit: quiet_emit,
        });
        let events = crate::agent_events::AgentEvents::new(
            outbound,
            std::sync::Arc::new(|_warning: &str| {}),
        );
        let cell = mailbox.status_cell();
        let _machine = crate::phase::PhaseMachine::new(
            "s1",
            crate::phase::Phase::Running {
                abort: crate::phase::AbortLatch::new(),
                turn: 1,
                step: 0,
                wake_requested: false,
            },
            std::sync::Arc::new(events),
            std::sync::Arc::clone(&cell),
        );
        let error =
            dispatch(METHOD_MAINTENANCE_BEGIN, br#"{"sessionId":"s1"}"#, &map).expect_err("busy");
        assert_eq!(error, "agent \"s1\" already has active work");
    }
    // --- lifecycle arms: real serve round trips against the real
    // fr-session dispatch (the session_client integration judgment).

    /// Removes the lifecycle integration tests' temporary backend root.
    struct LifecycleTempRoot(std::path::PathBuf);

    impl Drop for LifecycleTempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Routes one require into the real fr-session service dispatch
    /// (the `session_client` integration judgment).
    unsafe extern "C" fn real_persistence_require(
        host: *mut c_void,
        _service: *const u8,
        _service_len: usize,
        method: u32,
        in_data: *const u8,
        in_len: usize,
        out_buf: *mut u8,
        cap: usize,
        out_len: *mut usize,
    ) -> i32 {
        // SAFETY: the handle aliases the test-owned service box.
        let service = unsafe { &*(host.cast::<Mutex<fr_session::service::SessionService>>()) };
        // SAFETY: the payload slice is valid for this call per the vtable
        // contract.
        let payload = unsafe { std::slice::from_raw_parts(in_data, in_len) };
        let mut service = service.lock().expect("service lock");
        match fr_session::service::dispatch(method, payload, &mut service) {
            Ok(reply) => {
                // SAFETY: the caller-owned buffer contract.
                if unsafe { dougong_abi::write_out(&reply, out_buf, cap, out_len) } {
                    STATUS_OK
                } else {
                    dougong_abi::STATUS_CAPACITY
                }
            }
            Err(text) => {
                let body = serde_json::json!({ "error": text }).to_string();
                // SAFETY: same caller-owned buffer contract.
                let _ = unsafe { dougong_abi::write_out(body.as_bytes(), out_buf, cap, out_len) };
                STATUS_FAILED
            }
        }
    }

    /// Builds the shell state over a real fr-session service (the raw
    /// service box stays alive for the driver thread's requires; the
    /// caller reclaims it after joining the driver).
    fn lifecycle_state(
        root: &std::path::Path,
    ) -> (ShellState, *mut Mutex<fr_session::service::SessionService>) {
        let mut vocabulary = fr_session::vocabulary::EventVocabulary::core();
        vocabulary
            .declare(&["agent/inbox/spliced".to_owned()])
            .expect("declare the loop's extension type");
        let service = fr_session::service::SessionService::new(
            root,
            vocabulary,
            Arc::new(|| 1_700_000_000_000_i64),
            Arc::new(|_warning: String| {}),
        )
        .expect("assemble the real service");
        let service = Box::new(Mutex::new(service));
        let handle = Box::into_raw(service);
        let outbound = Outbound::new(HostExports {
            grant: crate::outbound::quiet_grant,
            dispatch: crate::outbound::noop_dispatch,
            host_handle: handle.cast::<c_void>(),
            require: real_persistence_require,
            emit: quiet_emit,
        });
        let state = ShellState {
            sessions: SessionMap::new(outbound),
            drivers: Mutex::new(HashMap::new()),
        };
        (state, handle)
    }

    /// Shuts the session's driver down and joins it, then reclaims the
    /// raw service box (the v3 thread discipline in test form).
    fn lifecycle_teardown(
        state: &ShellState,
        service: *mut Mutex<fr_session::service::SessionService>,
    ) {
        let mailboxes: Vec<_> = state
            .sessions
            .sessions
            .lock()
            .expect("session map lock")
            .values()
            .cloned()
            .collect();
        for mailbox in mailboxes {
            mailbox.request_shutdown();
        }
        let handles: Vec<_> = state
            .drivers
            .lock()
            .expect("driver registry lock")
            .drain()
            .map(|(_id, handle)| handle)
            .collect();
        for handle in handles {
            let _ = handle.join();
        }
        // SAFETY: every vtable user joined above; reclaimed exactly once.
        drop(unsafe { Box::from_raw(service) });
    }

    /// The CREATE arm: persistence create, driver spawn, the reply's
    /// session-handle face, and the liveness flip (spec: create 臂建会话
    /// 并 spawn 驱动).
    #[test]
    fn create_arm_spawns_a_session_driver() {
        let root = std::env::temp_dir().join(format!(
            "fr-agent-loop-b3-create-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let _guard = LifecycleTempRoot(root.clone());
        let (state, service) = lifecycle_state(&root);

        let payload = serde_json::json!({
            "sessionId": "s1",
            "cwd": "/repo",
            "provider": "deepseek-official",
            "model": "deepseek-chat",
        });
        let reply = dispatch_lifecycle(METHOD_CREATE, payload.to_string().as_bytes(), &state)
            .expect("the create arm serves")
            .expect("the create arm succeeds");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("reply json");
        assert_eq!(reply["sessionId"], "s1");

        let live = dispatch_lifecycle(METHOD_LIVE, br#"{"sessionId":"s1"}"#, &state)
            .expect("the live arm serves")
            .expect("the live arm succeeds");
        let live: serde_json::Value = serde_json::from_slice(&live).expect("live json");
        assert_eq!(live["live"], true);

        // The mailbox attached at the spawn; the registry holds the
        // thread and the persistence store holds the session.
        assert!(
            state
                .sessions
                .sessions
                .lock()
                .expect("session map lock")
                .contains_key("s1")
        );
        let store_read = {
            // SAFETY: the raw service box aliases the vtable handle; it
            // outlives this read (reclaimed in `lifecycle_teardown`).
            let service_ref = unsafe { &*service };
            let mut service = service_ref.lock().expect("service lock");
            fr_session::service::dispatch(
                fr_session::service::METHOD_SESSION_READ,
                br#"{"id":"s1"}"#,
                &mut service,
            )
            .expect("the created session is live in the store")
        };
        let store_read: serde_json::Value = serde_json::from_slice(&store_read).expect("read json");
        assert_eq!(store_read["header"]["cwd"], "/repo");

        lifecycle_teardown(&state, service);
    }

    /// The LIVE probe reports not-live for an unknown id and mints no
    /// mailbox entry (spec: live 探测不建邮箱).
    #[test]
    fn live_probe_reports_without_creating_mailboxes() {
        let state = ShellState {
            sessions: empty_map(),
            drivers: Mutex::new(HashMap::new()),
        };
        let reply = dispatch_lifecycle(METHOD_LIVE, br#"{"sessionId":"ghost"}"#, &state)
            .expect("the live arm serves")
            .expect("the live arm succeeds");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("live json");
        assert_eq!(reply["live"], false);
        assert!(
            state
                .sessions
                .sessions
                .lock()
                .expect("session map lock")
                .is_empty()
        );
    }

    /// The RESUME arm: adopt a persisted session from a previous
    /// process (a first service instance seeded the backend and torn
    /// down), restore over its log, spawn the driver, and read the
    /// seeded history back (spec: resume 臂接续持久化会话).
    #[test]
    fn resume_arm_adopts_the_persisted_session() {
        let root = std::env::temp_dir().join(format!(
            "fr-agent-loop-b3-resume-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let _guard = LifecycleTempRoot(root.clone());
        let mut vocabulary = fr_session::vocabulary::EventVocabulary::core();
        vocabulary
            .declare(&["agent/inbox/spliced".to_owned()])
            .expect("declare the loop's extension type");

        // The previous process: create the session with its cwd, append
        // one user message, and release every write claim.
        {
            let service = fr_session::service::SessionService::new(
                &root,
                vocabulary.clone(),
                Arc::new(|| 1_700_000_000_000_i64),
                Arc::new(|_warning: String| {}),
            )
            .expect("assemble the seeding service");
            let mut service = service;
            fr_session::service::dispatch(
                fr_session::service::METHOD_SESSION_CREATE,
                br#"{"id":"r1","meta":{"cwd":"/repo"}}"#,
                &mut service,
            )
            .expect("seed create");
            fr_session::service::dispatch(
                fr_session::service::METHOD_SESSION_APPEND,
                br#"{"id":"r1","type":"user/message","data":{"role":"user","id":"usr-1","content":[{"type":"text","text":"seed"}],"source":{"kind":"user"}},"surfaceOp":"append"}"#,
                &mut service,
            )
            .expect("seed append");
            service.teardown().expect("release the write claims");
        }

        let (state, service) = lifecycle_state(&root);
        let reply = dispatch_lifecycle(METHOD_RESUME, br#"{"sessionId":"r1"}"#, &state)
            .expect("the resume arm serves")
            .expect("the resume arm succeeds");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("reply json");
        assert_eq!(reply["sessionId"], "r1");

        let live = dispatch_lifecycle(METHOD_LIVE, br#"{"sessionId":"r1"}"#, &state)
            .expect("the live arm serves")
            .expect("the live arm succeeds");
        let live: serde_json::Value = serde_json::from_slice(&live).expect("live json");
        assert_eq!(live["live"], true);

        // The adopted session's seeded history reads back through the
        // same face the driver folded at construction.
        let read = {
            // SAFETY: the raw service box aliases the vtable handle; it
            // outlives this read (reclaimed in `lifecycle_teardown`).
            let service_ref = unsafe { &*service };
            let mut service = service_ref.lock().expect("service lock");
            fr_session::service::dispatch(
                fr_session::service::METHOD_SESSION_READ,
                br#"{"id":"r1"}"#,
                &mut service,
            )
            .expect("the adopted session is live in the store")
        };
        let read: serde_json::Value = serde_json::from_slice(&read).expect("read json");
        assert_eq!(read["total"], 1);
        assert_eq!(read["header"]["cwd"], "/repo");
        assert_eq!(read["events"][0]["data"]["id"], "usr-1");

        // A second RESUME refuses before any I/O (the registry guard's
        // dsh verbatim replacement text).
        let error = dispatch_lifecycle(METHOD_RESUME, br#"{"sessionId":"r1"}"#, &state)
            .expect("the resume arm serves")
            .expect_err("the second resume refuses");
        assert_eq!(error, "agent \"r1\" is already registered");

        lifecycle_teardown(&state, service);
    }
}
