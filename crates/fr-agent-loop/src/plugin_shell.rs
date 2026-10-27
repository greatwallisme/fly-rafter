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
//! The `llm/chunk-ready` listener obeys the on-event fast-path invariant:
//! no `require`, no blocking — parse the notification, park the generation
//! id, wake the driver, return. Notifications carry only
//! `{session_id, generation_id}`; ordering, duplication, and loss are all
//! harmless because the loop thread batch-pulls the real chunks from the
//! provider's per-generation buffer (Q84-B2).
//!
//! Thread discipline: every plugin-owned thread is joined before
//! `teardown` returns (vtable calls after `teardown` are documented
//! undefined behavior). The driver-body batch fills the driver registry;
//! this shell performs the shutdown-broadcast-and-join half.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};

use dougong_abi::{
    ABI_VERSION, HostExports, PluginExports, STATUS_CAPACITY, STATUS_FAILED, STATUS_OK, write_out,
};
use fr_wire::message::Message;
use serde::Deserialize;

use crate::inbox::InboxTarget;
use crate::outbound::Outbound;

/// The provided service name consumers call.
pub const SERVICE_NAME: &str = "agent";

/// The dylib artifact's plugin name.
pub const PLUGIN_NAME: &str = "fr-agent-loop";

/// Method selector: queue one input with full target and wake control
/// (`{sessionId, message, target, wakeup}`; dsh `send`).
pub const METHOD_SEND: u32 = 0;

/// Method selector: queue one steering input (`{sessionId, message}`;
/// target next-step, wakes the driver; dsh `steer`).
pub const METHOD_STEER: u32 = 1;

/// Method selector: queue one non-waking injection (`{sessionId,
/// message}`; target next-step, no wake; dsh `inject`).
pub const METHOD_INJECT: u32 = 2;

/// Method selector: queue one follow-up input (`{sessionId, message}`;
/// target next-turn, wakes the driver; dsh `followup`).
pub const METHOD_FOLLOWUP: u32 = 3;

/// Method selector: request cancellation (`{sessionId, cause,
/// keepInbox?}`; dsh `cancel`).
pub const METHOD_CANCEL: u32 = 4;

/// Method selector: shell metadata echo (`{}`).
pub const METHOD_DESCRIBE: u32 = 5;

/// Method selector: query the session's loop status
/// (`{sessionId}` → `{status}`).
///
/// Decision 46: the non-blocking query face `whenIdle` reconstructs from;
/// a blocking wait-for-idle require would violate the fast-path
/// invariant.
pub const METHOD_STATUS: u32 = 6;

/// Event id the descriptor's listen map binds to `llm/chunk-ready`.
pub const EV_CHUNK_READY: u32 = 1;

/// The provider stream notification event name (Q84-B2).
pub const EVENT_CHUNK_READY: &str = "llm/chunk-ready";

/// The descriptor the loader consumes: provides the `agent` service,
/// injects the four consumed services, listens for the provider stream
/// notification. The version is sourced from crate metadata so it cannot
/// drift from the artifact.
const DESCRIPTOR: &[u8] = concat!(
    "{\"name\":\"fr-agent-loop\",\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\",\"provide\":[\"agent\"],\"inject\":[\"llm\",\"tools\",\"system-prompt\",",
    "\"session-persistence\"],\"listen\":{\"llm/chunk-ready\":1}}"
)
.as_bytes();

/// The durable abort-cause taxonomy (dsh agent.ts `abortedCancelCause`):
/// the four shapes a cancel cause takes on the session log. The wire
/// authority is the fr-session slice A/B event vocabulary.
pub use fr_wire::event::CancelCause;

/// One parked wake source besides inbound commands (decision 46: slow work
/// lives on provider-owned worker threads and announces itself; the loop
/// thread parks on this mailbox and pulls).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverNotification {
    /// `llm/chunk-ready`: the named generation's FIFO buffer has new
    /// chunks (Q84-B2; ordering, duplication, and loss are all harmless).
    Chunks(String),
    /// A dispatched tool call settled; poll the tools face for its outcome
    /// (the Phase 14 bridge shape — tool execution is slow work on the
    /// tools plugin's worker threads).
    ToolSettled(fr_wire::ToolCallId),
}

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
    inner: Mutex<MailboxInner>,
    wake: Condvar,
}

#[derive(Debug)]
struct MailboxInner {
    commands: VecDeque<InboundCommand>,
    notifications: VecDeque<DriverNotification>,
    shutdown: bool,
}

impl SessionMailbox {
    fn new(outbound: Outbound) -> Self {
        Self {
            outbound,
            status: std::sync::Arc::new(crate::phase::StatusCell::new()),
            inner: Mutex::new(MailboxInner {
                commands: VecDeque::new(),
                notifications: VecDeque::new(),
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
            InboundCommand::Input { wakeup: true, .. } | InboundCommand::Cancel { .. }
        );
        {
            let mut inner = self.inner.lock().expect("mailbox lock poisoned");
            inner.commands.push_back(command);
        }
        if wakes {
            self.wake.notify_all();
        }
    }

    /// Parks one `llm/chunk-ready` notification and wakes the driver (a
    /// buffered batch may now be drainable). Ordering, duplication, and
    /// loss are all harmless (Q84-B2).
    ///
    /// # Panics
    ///
    /// Panics if the mailbox lock is poisoned (a holder panicked).
    pub fn notify_chunks(&self, generation_id: String) {
        self.notify(DriverNotification::Chunks(generation_id));
    }

    /// Parks one tool-settle notification and wakes the driver (a polled
    /// dispatch may now have its outcome).
    ///
    /// # Panics
    ///
    /// Panics if the mailbox lock is poisoned (a holder panicked).
    pub fn notify_tool_settled(&self, call_id: fr_wire::ToolCallId) {
        self.notify(DriverNotification::ToolSettled(call_id));
    }

    /// Parks one notification and wakes the driver.
    ///
    /// # Panics
    ///
    /// Panics if the mailbox lock is poisoned (a holder panicked).
    fn notify(&self, notification: DriverNotification) {
        {
            let mut inner = self.inner.lock().expect("mailbox lock poisoned");
            inner.notifications.push_back(notification);
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

    /// Takes every parked chunk notification, in arrival order.
    ///
    /// # Panics
    ///
    /// Panics if the mailbox lock is poisoned (a holder panicked).
    pub fn drain_notifications(&self) -> Vec<DriverNotification> {
        let mut inner = self.inner.lock().expect("mailbox lock poisoned");
        inner.notifications.drain(..).collect()
    }

    /// Parks the calling driver thread until an inbound command or the
    /// shutdown flag arrives (the idle wait point). A stray notification
    /// wakes the thread, which finds no command and parks again — a
    /// notification no live step consumes is lost by design (Q84-B2).
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
    /// command, a notification, or the shutdown flag (the in-step wait
    /// point: chunk batches, tool settlements, and cancels all proceed).
    ///
    /// # Panics
    ///
    /// Panics if the mailbox lock is poisoned (a holder panicked).
    pub fn wait_step(&self) {
        let mut inner = self.inner.lock().expect("mailbox lock poisoned");
        while inner.commands.is_empty() && inner.notifications.is_empty() && !inner.shutdown {
            inner = self.wake.wait(inner).expect("mailbox wait");
        }
        drop(inner);
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
            let status = session_mailbox(sessions, &request.session_id).status();
            return Ok(
                serde_json::to_vec(&serde_json::json!({ "status": status })).unwrap_or_default()
            );
        }
        _ => return Err(format!("unknown agent method {method}")),
    }
    Ok(serde_json::to_vec(&serde_json::json!({ "queued": true })).unwrap_or_default())
}

/// Handles one `llm/chunk-ready` notification: park the generation id on
/// the session's mailbox and wake the driver. Fast-path only — no
/// `require`, no blocking (PRD decision 46 on-event invariant).
///
/// # Errors
///
/// Returns the failure text for a malformed notification payload.
pub fn handle_chunk_ready(payload: &[u8], sessions: &SessionMap) -> Result<(), String> {
    #[derive(Deserialize)]
    struct Notification {
        session_id: String,
        generation_id: String,
    }
    let notification: Notification = serde_json::from_slice(payload)
        .map_err(|error| format!("{EVENT_CHUNK_READY} payload is invalid: {error}"))?;
    session_mailbox(sessions, &notification.session_id).notify_chunks(notification.generation_id);
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
    Box::into_raw(Box::new(ShellState {
        sessions: SessionMap::new(Outbound::new(host)),
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
    let reply = match dispatch(method, payload, &state.sessions) {
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

// The only listened event is `llm/chunk-ready` (emit dispatch): park the
// notification and wake the driver. No `require`, no blocking — the
// on-event fast-path invariant.
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
    if handle.is_null() || event != EV_CHUNK_READY {
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
    match handle_chunk_ready(payload, &state.sessions) {
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

/// Export table for the fr-agent-loop dylib artifact; the host resolves
/// this symbol by name at load time.
#[unsafe(no_mangle)]
pub static DOUGONG_PLUGIN_EXPORTS: PluginExports = PluginExports {
    abi_version: ABI_VERSION,
    name: plugin_name,
    descriptor,
    setup,
    call,
    on_event,
    on_next,
    teardown,
};

#[cfg(test)]
mod tests {
    use super::{
        CancelCause, InboundCommand, METHOD_CANCEL, METHOD_DESCRIBE, METHOD_FOLLOWUP,
        METHOD_INJECT, METHOD_SEND, METHOD_STATUS, METHOD_STEER, PLUGIN_NAME, SessionMap, dispatch,
        handle_chunk_ready, session_mailbox, shutdown_and_join,
    };
    use crate::inbox::InboxTarget;
    use crate::outbound::Outbound;
    use dougong_abi::{HostExports, STATUS_OK};
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
            serde_json::json!({ "llm/chunk-ready": 1 })
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
        assert_eq!(reply["abiVersion"], 3);

        assert!(
            dispatch(9, b"{}", &map)
                .expect_err("unknown method")
                .contains("unknown agent method 9")
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
    fn chunk_ready_routes_by_session_and_tolerates_duplicates() {
        let map = empty_map();
        for generation in ["g1", "g1", "g2"] {
            let payload = serde_json::json!({
                "session_id": "s7",
                "generation_id": generation,
            });
            handle_chunk_ready(payload.to_string().as_bytes(), &map).expect("notification");
        }
        let other = serde_json::json!({ "session_id": "s8", "generation_id": "g9" });
        handle_chunk_ready(other.to_string().as_bytes(), &map).expect("other session");

        assert_eq!(
            session_mailbox(&map, "s7").drain_notifications(),
            vec![
                super::DriverNotification::Chunks("g1".to_owned()),
                super::DriverNotification::Chunks("g1".to_owned()),
                super::DriverNotification::Chunks("g2".to_owned()),
            ]
        );
        assert_eq!(
            session_mailbox(&map, "s8").drain_notifications(),
            vec![super::DriverNotification::Chunks("g9".to_owned())]
        );
        assert_eq!(super::EV_CHUNK_READY, 1);
        assert_eq!(super::EVENT_CHUNK_READY, "llm/chunk-ready");

        assert!(
            handle_chunk_ready(br#"{"session_id": "s7"}"#, &map)
                .expect_err("missing generation id")
                .contains("llm/chunk-ready payload is invalid")
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
}
