// Rust guideline compliant 2026-10-27
//! The loop's fused agent-event face (dsh `core/agent/dispatch.ts` port,
//! scoped to the events the loop dispatches).
//!
//! dsh couples the agent subject to a scope carrier over the cordis event
//! bus, so in-process middleware and cross-plugin observers share one
//! dispatch. The dougong kernel bus has no return path for plugin emits
//! (and the CPS waterfall is listener-side only), so the fr face splits the
//! same semantics in two: an in-process listener registry (the middleware
//! face; the waterfalls and serial dispatches live here) and an
//! [`Outbound::emit`] mirror for every emit-mode face (the observer face
//! decision 46 reconstructs `whenIdle` from). M1 has no cross-plugin
//! middleware (the `llm-retry` extension point is outside the M1 crate
//! set), so the registry is the complete face; a registration wire joins
//! with the Phase 14/18 assembly.
//!
//! Containment (dsh emit): every listener failure is contained per
//! listener through the warn sink, so a notification can neither veto
//! lifecycle progress nor starve a later observer. dsh distinguishes a
//! synchronous throw from a returned-promise rejection (`listener threw`
//! vs `listener rejected`); Rust listeners report failure through
//! [`Result`], so only the `threw` text has a live arm here. A panic stays
//! a panic (M-PANIC-IS-STOP: a panicking listener is a programming error).
//!
//! The dsh payloads carry the turn's `AbortSignal`; the fr driver thread
//! owns the abort latch and polls it at exactly the dsh `throwIfAborted`
//! sites, so the in-process payloads omit the structurally inert signal.

use std::error::Error;
use std::sync::Arc;

use fr_llm::adapter::LlmCallConfig;
use fr_llm::retry_policy::RetryPolicy;
use fr_wire::Failure;
use fr_wire::message::Message;

use crate::assistant_stream::AssistantStreamFrame;
use crate::outbound::Outbound;
use crate::phase::AgentStatus;

/// Observer event: the loop's status flipped (decision 46 face).
pub const EVENT_STATUS: &str = "agent/status";
/// Observer event: the loop reported one failure at its live boundary.
pub const EVENT_ERROR: &str = "agent/error";
/// Observer event: one assistant attempt frame.
pub const EVENT_ASSISTANT_STREAM: &str = "agent/assistant-stream";
/// Observer event: one inbox message was claimed for a turn.
pub const EVENT_INBOX_CLAIMED: &str = "agent/inbox/claimed";
/// Observer event: one inbox message became pending.
pub const EVENT_INBOX_INSERTED: &str = "agent/inbox/inserted";
/// Observer event: one pending inbox message was dropped.
pub const EVENT_INBOX_DISCARDED: &str = "agent/inbox/discarded";
/// Middleware event (waterfall): the pre-step decision.
pub const EVENT_PRE_STEP: &str = "agent/pre-step";
/// Middleware event (waterfall): the request config proposal.
pub const EVENT_REQUEST: &str = "agent/request";
/// Middleware event (waterfall): the failed-attempt retry ruling.
pub const EVENT_REQUEST_ERROR: &str = "agent/request-error";
/// Middleware event (serial): a turn is about to end.
pub const EVENT_TURN_STOPPING: &str = "agent/turn-stopping";

/// The mirror batch ceiling.
///
/// A full buffer flushes regardless of frame kind, so a mid-attempt
/// chunk stream never waits past this many frames for its boundary
/// crossing (start and end frames flush immediately).
pub const STREAM_MIRROR_BATCH: usize = 32;

/// A listener failure as the dispatch faces report it.
pub type ListenerError = Box<dyn Error + Send + Sync>;

/// Where contained listener failures go.
pub type WarnSink = Arc<dyn Fn(&str) + Send + Sync>;

/// The `agent/status` payload.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusPayload {
    /// The loop's status.
    pub status: AgentStatus,
}

/// The batched `agent/assistant-stream` mirror payload: every bus
/// crossing carries a batch (PRD 22 hot-path countermeasure — the
/// in-process listeners stay per-frame, the boundary batches).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantStreamBatchPayload {
    /// The frames, in emission order.
    pub frames: Vec<AssistantStreamFrame>,
}

/// The `agent/error` payload: the failure at its live boundary.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorPayload {
    /// The turn the failure belongs to.
    pub turn: u64,
    /// The step the failure belongs to.
    pub step: u64,
    /// The structured failure facts.
    pub error: Failure,
}

/// The `agent/assistant-stream` payload.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantStreamPayload {
    /// The attempt frame.
    pub frame: AssistantStreamFrame,
}

/// The `agent/inbox/claimed` payload.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxClaimedPayload {
    /// The session whose inbox claimed the message (the dsh payload
    /// carries the agent object itself; the honest wire serialization
    /// names the session).
    pub session_id: String,
    /// The message claimed for the turn.
    pub message: Message,
    /// The turn that owns the claimed batch.
    pub turn: u64,
}

/// The `agent/inbox/inserted` and `agent/inbox/discarded` payload.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxMessagePayload {
    /// The message the splice lifecycle event names.
    pub message: Message,
}

/// The `agent/pre-step` payload: the claimed batch proposed for one step.
#[derive(Debug, Clone, PartialEq)]
pub struct PreStepPayload {
    /// The claimed input batch.
    pub messages: Vec<Message>,
    /// The turn the step belongs to.
    pub turn: u64,
    /// The step's number within the turn.
    pub step: u64,
}

/// The `agent/pre-step` ruling.
#[derive(Debug, Clone, PartialEq)]
pub enum PreStepDecision {
    /// The input is refused; the turn ends `blocked`.
    Reject,
    /// The step runs with this batch.
    Enter {
        /// The model-visible input batch.
        messages: Vec<Message>,
        /// Whether this step opens a request series.
        starts_request_series: bool,
    },
}

/// The `agent/request` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestPayload {
    /// The turn the request belongs to.
    pub turn: u64,
    /// The step the request belongs to.
    pub step: u64,
}

/// The `agent/request-error` payload: one failed attempt's facts.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestErrorPayload {
    /// The turn the attempt belonged to.
    pub turn: u64,
    /// The step the attempt belonged to.
    pub step: u64,
    /// The provider route that failed.
    pub provider: String,
    /// The structured failure facts.
    pub failure: Failure,
    /// The prepared call's declared retry policy, when one was bound.
    pub retry_policy: Option<RetryPolicy>,
}

/// The `agent/request-error` ruling: dsh `{ kind: 'retry' } | undefined`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestErrorAction {
    /// Retry the attempt.
    Retry,
}

/// The `agent/turn-stopping` payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnStoppingPayload {
    /// The turn about to end.
    pub turn: u64,
}

type StatusListener = Box<dyn Fn(AgentStatus) -> Result<(), ListenerError> + Send + Sync>;
type ErrorListener = Box<dyn Fn(&ErrorPayload) -> Result<(), ListenerError> + Send + Sync>;
type AssistantStreamListener =
    Box<dyn Fn(&AssistantStreamPayload) -> Result<(), ListenerError> + Send + Sync>;
type ClaimedListener = Box<dyn Fn(&InboxClaimedPayload) -> Result<(), ListenerError> + Send + Sync>;
type InboxMessageListener =
    Box<dyn Fn(&InboxMessagePayload) -> Result<(), ListenerError> + Send + Sync>;

/// One pre-step continuation: the rest of the chain down to the default.
pub type PreStepNext<'a> = Box<dyn FnOnce(PreStepPayload) -> PreStepDecision + 'a>;
/// The pre-step chain's innermost default.
pub type PreStepTail<'a> = Box<dyn FnOnce(PreStepPayload) -> PreStepDecision + 'a>;
/// A pre-step listener; dropping `next` without returning a decision of
/// its own vetoes the rest of the chain.
pub type PreStepListener =
    Box<dyn for<'a> Fn(PreStepPayload, PreStepNext<'a>) -> PreStepDecision + Send + Sync>;

/// One request-config continuation.
pub type RequestNext<'a> = Box<dyn FnOnce(RequestPayload) -> LlmCallConfig + 'a>;
/// The request chain's innermost default (the seed config).
pub type RequestTail<'a> = Box<dyn FnOnce(RequestPayload) -> LlmCallConfig + 'a>;
/// A request listener proposing the config.
pub type RequestListener =
    Box<dyn for<'a> Fn(RequestPayload, RequestNext<'a>) -> LlmCallConfig + Send + Sync>;

/// One retry-ruling continuation.
pub type RequestErrorNext<'a> =
    Box<dyn FnOnce(RequestErrorPayload) -> Option<RequestErrorAction> + 'a>;
/// The retry chain's innermost default (dsh: no retry).
pub type RequestErrorTail<'a> =
    Box<dyn FnOnce(RequestErrorPayload) -> Option<RequestErrorAction> + 'a>;
/// A retry ruling listener.
pub type RequestErrorListener = Box<
    dyn for<'a> Fn(RequestErrorPayload, RequestErrorNext<'a>) -> Option<RequestErrorAction>
        + Send
        + Sync,
>;

type TurnStoppingListener =
    Box<dyn Fn(TurnStoppingPayload) -> Result<(), ListenerError> + Send + Sync>;

/// Runs one waterfall chain: the first listener is the outermost frame;
/// each listener either returns its own value (vetoing the rest) or
/// continues with a possibly transformed payload.
fn waterfall_chain<'a, F, P: 'a, V: 'a>(
    listeners: &'a [F],
    payload: P,
    tail: Box<dyn FnOnce(P) -> V + 'a>,
) -> V
where
    F: Fn(P, Box<dyn FnOnce(P) -> V + 'a>) -> V,
{
    match listeners.split_first() {
        Some((head, rest)) => {
            let next: Box<dyn FnOnce(P) -> V + 'a> =
                Box::new(move |payload| waterfall_chain(rest, payload, tail));
            head(payload, next)
        }
        None => tail(payload),
    }
}

/// The dispatch face one driver owns. Registration happens before the
/// driver thread starts; dispatch is `&self` thereafter.
pub struct AgentEvents {
    outbound: Outbound,
    warn: WarnSink,
    pending_frames: std::sync::Mutex<Vec<AssistantStreamPayload>>,
    status: Vec<StatusListener>,
    error: Vec<ErrorListener>,
    assistant_stream: Vec<AssistantStreamListener>,
    claimed: Vec<ClaimedListener>,
    inserted: Vec<InboxMessageListener>,
    discarded: Vec<InboxMessageListener>,
    turn_stopping: Vec<TurnStoppingListener>,
    pre_step: Vec<PreStepListener>,
    request: Vec<RequestListener>,
    request_error: Vec<RequestErrorListener>,
}

impl std::fmt::Debug for AgentEvents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentEvents")
            .field("outbound", &self.outbound)
            .field("status listeners", &self.status.len())
            .field("error listeners", &self.error.len())
            .field("assistant-stream listeners", &self.assistant_stream.len())
            .field("claimed listeners", &self.claimed.len())
            .field("inserted listeners", &self.inserted.len())
            .field("discarded listeners", &self.discarded.len())
            .field("turn-stopping listeners", &self.turn_stopping.len())
            .field("pre-step listeners", &self.pre_step.len())
            .field("request listeners", &self.request.len())
            .field("request-error listeners", &self.request_error.len())
            .finish_non_exhaustive()
    }
}

impl AgentEvents {
    /// Builds the face over the activation's outbound seam.
    #[must_use]
    pub fn new(outbound: Outbound, warn: WarnSink) -> Self {
        Self {
            outbound,
            warn,
            pending_frames: std::sync::Mutex::new(Vec::new()),
            status: Vec::new(),
            error: Vec::new(),
            assistant_stream: Vec::new(),
            claimed: Vec::new(),
            inserted: Vec::new(),
            discarded: Vec::new(),
            turn_stopping: Vec::new(),
            pre_step: Vec::new(),
            request: Vec::new(),
            request_error: Vec::new(),
        }
    }

    /// The face's outbound seam (the same activation vtable the driver's
    /// service clients must use).
    #[must_use]
    pub const fn outbound(&self) -> Outbound {
        self.outbound
    }

    /// Registers a status listener.
    pub fn on_status(&mut self, listener: StatusListener) {
        self.status.push(listener);
    }

    /// Registers a failure listener.
    pub fn on_error(&mut self, listener: ErrorListener) {
        self.error.push(listener);
    }

    /// Registers an assistant-stream listener.
    pub fn on_assistant_stream(&mut self, listener: AssistantStreamListener) {
        self.assistant_stream.push(listener);
    }

    /// Registers an inbox-claim listener.
    pub fn on_inbox_claimed(&mut self, listener: ClaimedListener) {
        self.claimed.push(listener);
    }

    /// Registers an inbox-insertion listener.
    pub fn on_inbox_inserted(&mut self, listener: InboxMessageListener) {
        self.inserted.push(listener);
    }

    /// Registers an inbox-discard listener.
    pub fn on_inbox_discarded(&mut self, listener: InboxMessageListener) {
        self.discarded.push(listener);
    }

    /// Registers a turn-stopping listener (serial face).
    pub fn on_turn_stopping(&mut self, listener: TurnStoppingListener) {
        self.turn_stopping.push(listener);
    }

    /// Registers a pre-step listener (waterfall face).
    pub fn on_pre_step(&mut self, listener: PreStepListener) {
        self.pre_step.push(listener);
    }

    /// Registers a request listener (waterfall face).
    pub fn on_request(&mut self, listener: RequestListener) {
        self.request.push(listener);
    }

    /// Registers a retry-ruling listener (waterfall face).
    pub fn on_request_error(&mut self, listener: RequestErrorListener) {
        self.request_error.push(listener);
    }

    /// Publishes one status flip.
    pub fn emit_status(&self, status: AgentStatus) {
        let payload = StatusPayload { status };
        for listener in &self.status {
            if let Err(error) = listener(payload.status) {
                self.warn_listener_failure(EVENT_STATUS, &error);
            }
        }
        self.mirror(EVENT_STATUS, &payload);
    }

    /// Publishes one failure at its live boundary.
    pub fn emit_error(&self, payload: &ErrorPayload) {
        for listener in &self.error {
            if let Err(error) = listener(payload) {
                self.warn_listener_failure(EVENT_ERROR, &error);
            }
        }
        self.mirror(EVENT_ERROR, payload);
    }

    /// Publishes one assistant attempt frame.
    ///
    /// The in-process listeners receive every frame (dsh parity); the bus
    /// mirror batches — one `{"frames": [...]}` crossing per full buffer,
    /// attempt boundary, or explicit flush (PRD 22 hot-path
    /// countermeasure; the driver flushes at its park points).
    pub fn emit_assistant_stream(&self, payload: &AssistantStreamPayload) {
        for listener in &self.assistant_stream {
            if let Err(error) = listener(payload) {
                self.warn_listener_failure(EVENT_ASSISTANT_STREAM, &error);
            }
        }
        let boundary = matches!(
            payload.frame,
            AssistantStreamFrame::Start { .. } | AssistantStreamFrame::End { .. }
        );
        let mut pending = self
            .pending_frames
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.push(payload.clone());
        if boundary || pending.len() >= STREAM_MIRROR_BATCH {
            let batch = AssistantStreamBatchPayload {
                frames: std::mem::take(&mut *pending)
                    .into_iter()
                    .map(|p| p.frame)
                    .collect(),
            };
            drop(pending);
            self.mirror(EVENT_ASSISTANT_STREAM, &batch);
        }
    }

    /// Flushes the pending stream frames through the bus mirror (the
    /// driver's park points: no frame waits on a parked loop).
    pub fn flush_stream_mirror(&self) {
        let mut pending = self
            .pending_frames
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pending.is_empty() {
            return;
        }
        let batch = AssistantStreamBatchPayload {
            frames: std::mem::take(&mut *pending)
                .into_iter()
                .map(|p| p.frame)
                .collect(),
        };
        drop(pending);
        self.mirror(EVENT_ASSISTANT_STREAM, &batch);
    }

    /// Publishes one inbox claim.
    pub fn emit_inbox_claimed(&self, payload: &InboxClaimedPayload) {
        for listener in &self.claimed {
            if let Err(error) = listener(payload) {
                self.warn_listener_failure(EVENT_INBOX_CLAIMED, &error);
            }
        }
        self.mirror(EVENT_INBOX_CLAIMED, payload);
    }

    /// Publishes one inbox insertion.
    pub fn emit_inbox_inserted(&self, payload: &InboxMessagePayload) {
        for listener in &self.inserted {
            if let Err(error) = listener(payload) {
                self.warn_listener_failure(EVENT_INBOX_INSERTED, &error);
            }
        }
        self.mirror(EVENT_INBOX_INSERTED, payload);
    }

    /// Publishes one inbox discard.
    pub fn emit_inbox_discarded(&self, payload: &InboxMessagePayload) {
        for listener in &self.discarded {
            if let Err(error) = listener(payload) {
                self.warn_listener_failure(EVENT_INBOX_DISCARDED, &error);
            }
        }
        self.mirror(EVENT_INBOX_DISCARDED, payload);
    }

    /// Dispatches the turn-stopping serial event: listeners run in
    /// registration order and the first failure aborts the chain.
    ///
    /// # Errors
    ///
    /// Returns the first listener failure; remaining listeners do not run.
    pub fn serial_turn_stopping(&self, payload: TurnStoppingPayload) -> Result<(), ListenerError> {
        for listener in &self.turn_stopping {
            listener(payload)?;
        }
        Ok(())
    }

    /// Dispatches the pre-step waterfall over the default continuation.
    #[must_use]
    pub fn waterfall_pre_step<'a>(
        &'a self,
        payload: PreStepPayload,
        default: PreStepTail<'a>,
    ) -> PreStepDecision {
        waterfall_chain(&self.pre_step, payload, default)
    }

    /// Dispatches the request waterfall over the seed continuation.
    #[must_use]
    pub fn waterfall_request<'a>(
        &'a self,
        payload: RequestPayload,
        seed: RequestTail<'a>,
    ) -> LlmCallConfig {
        waterfall_chain(&self.request, payload, seed)
    }

    /// Dispatches the retry waterfall over the no-retry default.
    #[must_use]
    pub fn waterfall_request_error<'a>(
        &'a self,
        payload: RequestErrorPayload,
        default: RequestErrorTail<'a>,
    ) -> Option<RequestErrorAction> {
        waterfall_chain(&self.request_error, payload, default)
    }

    fn warn_listener_failure(&self, name: &str, error: &ListenerError) {
        (self.warn)(&format!("agent event \"{name}\" listener threw: {error}"));
    }

    /// Mirrors one emit-mode face onto the kernel bus (best effort; the
    /// bus promises no ordering, and a rejection cannot veto the loop).
    fn mirror<P: serde::Serialize>(&self, name: &str, payload: &P) {
        match serde_json::to_vec(payload) {
            Ok(bytes) => {
                if let Err(failure) = self.outbound.emit(name, &bytes) {
                    (self.warn)(&format!(
                        "agent event \"{name}\" bus mirror failed with status {}",
                        failure.status
                    ));
                }
            }
            Err(error) => {
                (self.warn)(&format!(
                    "agent event \"{name}\" payload encode failed: {error}"
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AgentEvents, AssistantStreamPayload, EVENT_STATUS, ErrorPayload, InboxClaimedPayload,
        InboxMessagePayload, ListenerError, PreStepDecision, PreStepPayload, RequestErrorAction,
        RequestErrorPayload, RequestPayload,
    };
    use crate::outbound::Outbound;
    use crate::phase::AgentStatus;
    use dougong_abi::{HostExports, STATUS_OK};
    use fr_llm::adapter::LlmCallConfig;
    use fr_wire::message::Message;
    use std::error::Error;
    use std::ffi::c_void;
    use std::sync::{Arc, Mutex};

    fn fail(message: &'static str) -> ListenerError {
        Box::<dyn Error + Send + Sync>::from(message)
    }

    #[derive(Default)]
    struct WarnLog(Mutex<Vec<String>>);

    fn face() -> (AgentEvents, Arc<WarnLog>) {
        face_with(quiet_outbound())
    }

    fn face_with(outbound: Outbound) -> (AgentEvents, Arc<WarnLog>) {
        let log = Arc::new(WarnLog::default());
        let sink_log = Arc::clone(&log);
        let events = AgentEvents::new(
            outbound,
            Arc::new(move |text: &str| {
                sink_log.0.lock().expect("warn lock").push(text.to_owned());
            }),
        );
        (events, log)
    }

    fn quiet_outbound() -> Outbound {
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
        Outbound::new(HostExports {
            grant: crate::outbound::quiet_grant,
            host_handle: std::ptr::null_mut(),
            require: quiet_require,
            emit: quiet_emit,
        })
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

    #[test]
    fn emit_contains_failures_and_keeps_later_listeners() {
        let (mut events, log) = face();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_first = Arc::clone(&seen);
        let seen_second = Arc::clone(&seen);
        events.on_status(Box::new(move |_status| {
            seen_first.lock().expect("test lock").push("first");
            Err(fail("boom"))
        }));
        events.on_status(Box::new(move |_status| {
            seen_second.lock().expect("test lock").push("second");
            Ok(())
        }));
        events.emit_status(AgentStatus::Running);
        assert_eq!(*seen.lock().expect("test lock"), ["first", "second"]);
        assert_eq!(
            log.0.lock().expect("test lock").as_slice(),
            [format!(
                "agent event \"{EVENT_STATUS}\" listener threw: boom"
            )]
        );
    }

    #[test]
    fn emit_mirrors_wire_payloads_onto_the_bus() {
        type Bus = Mutex<Vec<(String, serde_json::Value)>>;
        unsafe extern "C" fn recording_emit(
            host: *mut c_void,
            event: *const u8,
            event_len: usize,
            payload: *const u8,
            payload_len: usize,
        ) -> i32 {
            // SAFETY: the vtable contract passes valid readable slices for
            // this call; the handle aliases the test-owned recorder box.
            let bus = unsafe { &*(host.cast::<Bus>()) };
            // SAFETY: the vtable contract passes one valid readable slice
            // per argument for this call.
            let name = unsafe {
                String::from_utf8_lossy(std::slice::from_raw_parts(event, event_len)).into_owned()
            };
            // SAFETY: same per-call validity.
            let value =
                serde_json::from_slice(unsafe { std::slice::from_raw_parts(payload, payload_len) })
                    .expect("mirrored payload is JSON");
            bus.lock().expect("test lock").push((name, value));
            STATUS_OK
        }
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
        let bus: *mut Bus = Box::into_raw(Box::new(Bus::default()));
        let outbound = Outbound::new(HostExports {
            grant: crate::outbound::quiet_grant,
            host_handle: bus.cast::<c_void>(),
            require: quiet_require,
            emit: recording_emit,
        });
        let (events, _log) = face_with(outbound);
        events.emit_status(AgentStatus::Running);
        events.emit_inbox_claimed(&InboxClaimedPayload {
            session_id: "s1".to_owned(),
            message: user_message("task"),
            turn: 4,
        });
        // SAFETY: the driver-thread pattern joins its threads before the
        // handle dies; here dispatch already returned synchronously.
        let recorded = (*unsafe { Box::from_raw(bus) })
            .into_inner()
            .expect("test bus not poisoned");
        let names: Vec<String> = recorded.iter().map(|(name, _)| name.clone()).collect();
        assert_eq!(
            names,
            ["agent/status".to_owned(), "agent/inbox/claimed".to_owned()]
        );
        assert_eq!(recorded[0].1, serde_json::json!({ "status": "running" }));
        assert_eq!(recorded[1].1["turn"], 4);
        assert_eq!(recorded[1].1["message"]["id"], "msg-task");
    }

    #[test]
    fn waterfall_without_listeners_runs_the_default() {
        let (events, _log) = face();
        let decision = events.waterfall_pre_step(
            PreStepPayload {
                messages: vec![user_message("task")],
                turn: 1,
                step: 1,
            },
            Box::new(|payload| PreStepDecision::Enter {
                messages: payload.messages,
                starts_request_series: false,
            }),
        );
        assert_eq!(
            decision,
            PreStepDecision::Enter {
                messages: vec![user_message("task")],
                starts_request_series: false,
            }
        );
        let action = events.waterfall_request_error(
            RequestErrorPayload {
                turn: 1,
                step: 1,
                provider: "p".to_owned(),
                failure: serde_json::from_value(serde_json::json!({
                    "message": "boom", "code": "TRANSPORT"
                }))
                .expect("fixture failure"),
                retry_policy: None,
            },
            Box::new(|_payload| None),
        );
        assert_eq!(action, None);
    }

    #[test]
    fn waterfall_veto_returns_without_the_tail() {
        let (mut events, _log) = face();
        events.on_pre_step(Box::new(|_payload, _next| PreStepDecision::Reject));
        let decision = events.waterfall_pre_step(
            PreStepPayload {
                messages: vec![],
                turn: 2,
                step: 1,
            },
            Box::new(|_payload| {
                panic!("a vetoed chain must not reach the default");
            }),
        );
        assert_eq!(decision, PreStepDecision::Reject);
    }

    #[test]
    fn waterfall_transforms_payloads_in_registration_order() {
        let (mut events, _log) = face();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_outer = Arc::clone(&seen);
        let seen_inner = Arc::clone(&seen);
        events.on_pre_step(Box::new(move |payload, next| {
            seen_outer
                .lock()
                .expect("test lock")
                .push(format!("outer saw turn {}", payload.turn));
            let mut transformed = payload;
            transformed.turn += 10;
            next(transformed)
        }));
        events.on_pre_step(Box::new(move |payload, next| {
            seen_inner
                .lock()
                .expect("test lock")
                .push(format!("inner saw turn {}", payload.turn));
            next(payload)
        }));
        let decision = events.waterfall_pre_step(
            PreStepPayload {
                messages: vec![],
                turn: 1,
                step: 1,
            },
            Box::new(|_payload| PreStepDecision::Enter {
                messages: vec![],
                starts_request_series: true,
            }),
        );
        assert_eq!(
            *seen.lock().expect("test lock"),
            [
                "outer saw turn 1".to_owned(),
                "inner saw turn 11".to_owned()
            ]
        );
        assert_eq!(
            decision,
            PreStepDecision::Enter {
                messages: vec![],
                starts_request_series: true,
            }
        );
    }

    #[test]
    fn waterfall_request_returns_the_listener_config() {
        let (mut events, _log) = face();
        events.on_request(Box::new(|_payload, _next| LlmCallConfig {
            provider: "deepseek".to_owned(),
            model: "m1".to_owned(),
            ..LlmCallConfig::default()
        }));
        let config = events.waterfall_request(
            RequestPayload { turn: 1, step: 2 },
            Box::new(|_payload| LlmCallConfig {
                provider: "seed".to_owned(),
                model: "m0".to_owned(),
                ..LlmCallConfig::default()
            }),
        );
        assert_eq!(config.provider, "deepseek");
        assert_eq!(config.model, "m1");
    }

    #[test]
    fn request_error_waterfall_rules_a_retry() {
        let (mut events, _log) = face();
        events.on_request_error(Box::new(|_payload, _next| Some(RequestErrorAction::Retry)));
        let action = events.waterfall_request_error(
            RequestErrorPayload {
                turn: 1,
                step: 1,
                provider: "p".to_owned(),
                failure: serde_json::from_value(serde_json::json!({
                    "message": "rate limited", "code": "RATE_LIMIT"
                }))
                .expect("fixture failure"),
                retry_policy: None,
            },
            Box::new(|_payload| None),
        );
        assert_eq!(action, Some(RequestErrorAction::Retry));
    }

    #[test]
    fn serial_stops_at_the_first_failure() {
        let (mut events, _log) = face();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_first = Arc::clone(&seen);
        let seen_second = Arc::clone(&seen);
        events.on_turn_stopping(Box::new(move |payload| {
            seen_first.lock().expect("test lock").push(payload.turn);
            Err(fail("listener one failed"))
        }));
        events.on_turn_stopping(Box::new(move |payload| {
            seen_second.lock().expect("test lock").push(payload.turn);
            Ok(())
        }));
        let error = events
            .serial_turn_stopping(super::TurnStoppingPayload { turn: 7 })
            .expect_err("first listener fails");
        assert_eq!(error.to_string(), "listener one failed");
        assert_eq!(*seen.lock().expect("test lock"), [7_u64]);
    }

    #[test]
    fn error_and_inbox_payloads_carry_the_wire_shapes() {
        // The payload types are the wire contract for the bus mirror; their
        // serialized shapes pin here without a bus round-trip.
        let error: ErrorPayload = ErrorPayload {
            turn: 3,
            step: 2,
            error: serde_json::from_value(serde_json::json!({
                "message": "boom", "code": "UNKNOWN"
            }))
            .expect("fixture failure"),
        };
        assert_eq!(
            serde_json::to_value(&error).expect("serializes"),
            serde_json::json!({
                "turn": 3, "step": 2,
                "error": { "message": "boom", "code": "UNKNOWN" }
            })
        );
        let claimed = InboxClaimedPayload {
            session_id: "s1".to_owned(),
            message: user_message("task"),
            turn: 4,
        };
        let value = serde_json::to_value(&claimed).expect("serializes");
        assert_eq!(value["sessionId"], "s1");
        assert_eq!(value["turn"], 4);
        assert_eq!(value["message"]["id"], "msg-task");
        let inserted = InboxMessagePayload {
            message: user_message("queued"),
        };
        let value = serde_json::to_value(&inserted).expect("serializes");
        assert_eq!(value["message"]["content"][0]["text"], "queued");
        let _stream = AssistantStreamPayload {
            frame: crate::assistant_stream::AssistantStreamFrame::Start {
                attempt_id: fr_wire::ids::LlmAttemptId::new("s1:1"),
                revision: 1,
                turn: 1,
                step: 1,
            },
        };
    }

    /// One recording bus mirror: `(event, payload)` per crossing.
    type MirrorLog = Arc<std::sync::Mutex<Vec<(String, Vec<u8>)>>>;
    /// The mirror's inner log (the raw handle's pointee — `Arc::into_raw`
    /// hands out the inner allocation, not the `Arc` header).
    type MirrorEntry = std::sync::Mutex<Vec<(String, Vec<u8>)>>;

    /// The mirror host's require arm: the mirror never requires.
    unsafe extern "C" fn recording_require(
        _host: *mut std::ffi::c_void,
        _service: *const u8,
        _service_len: usize,
        _method: u32,
        _in_data: *const u8,
        _in_len: usize,
        _out_buf: *mut u8,
        _cap: usize,
        _out_len: *mut usize,
    ) -> i32 {
        panic!("the mirror host never requires");
    }

    /// The mirror host's emit arm: appends `(event, payload)` to the
    /// handle's log.
    ///
    /// # Safety
    ///
    /// `host` must alias a live [`MirrorEntry`] for the call; the slices
    /// must be valid for their lengths per the vtable contract.
    unsafe extern "C" fn recording_emit(
        host: *mut std::ffi::c_void,
        event: *const u8,
        event_len: usize,
        payload: *const u8,
        payload_len: usize,
    ) -> i32 {
        // SAFETY: the caller (the vtable test double) passes the handle
        // this fn documents.
        let log = unsafe { &*(host.cast::<MirrorEntry>()) };
        // SAFETY: same contract for the slices.
        let event = unsafe {
            String::from_utf8_lossy(std::slice::from_raw_parts(event, event_len)).into_owned()
        };
        // SAFETY: same contract for the payload slice.
        let payload = unsafe { std::slice::from_raw_parts(payload, payload_len) }.to_vec();
        log.lock().expect("mirror log").push((event, payload));
        STATUS_OK
    }

    fn recording_events() -> (AgentEvents, MirrorLog) {
        let log: MirrorLog = Arc::new(std::sync::Mutex::new(Vec::new()));
        let handle = Arc::into_raw(Arc::clone(&log));
        let events = AgentEvents::new(
            crate::outbound::Outbound::new(HostExports {
                grant: crate::outbound::quiet_grant,
                host_handle: handle.cast_mut().cast::<std::ffi::c_void>(),
                require: recording_require,
                emit: recording_emit,
            }),
            Arc::new(|_warning: &str| {}),
        );
        (events, log)
    }

    /// The batching mirror: chunks accumulate, a full buffer or an
    /// attempt boundary flushes, and the batch payload preserves emission
    /// order — the PRD 22 hot-path countermeasure.
    #[test]
    fn stream_mirror_batches_frames_across_the_boundary() {
        use crate::assistant_stream::AssistantStreamFrame;
        use fr_wire::StreamChunk;
        let (events, log) = recording_events();
        let frame = |index: u64| AssistantStreamFrame::Chunk {
            attempt_id: fr_wire::ids::LlmAttemptId::new("s1:1"),
            revision: 1,
            index,
            time: 1_700_000_000_000,
            chunk: Box::new(StreamChunk::Known(
                fr_wire::stream::KnownStreamChunk::TextDelta {
                    index: 0,
                    text: format!("chunk {index}"),
                },
            )),
        };
        // Fill below the ceiling, then flush explicitly.
        for index in 0..super::STREAM_MIRROR_BATCH - 1 {
            events.emit_assistant_stream(&AssistantStreamPayload {
                frame: frame(index as u64),
            });
        }
        assert!(
            log.lock().expect("mirror log").is_empty(),
            "below the ceiling nothing crosses"
        );
        events.flush_stream_mirror();
        let crossings = log.lock().expect("mirror log").clone();
        assert_eq!(crossings.len(), 1, "one batched crossing");
        let payload: serde_json::Value =
            serde_json::from_slice(&crossings[0].1).expect("batch json");
        assert_eq!(
            payload["frames"].as_array().map(Vec::len),
            Some(super::STREAM_MIRROR_BATCH - 1)
        );
        assert_eq!(
            payload["frames"][0]["chunk"]["text"], "chunk 0",
            "emission order preserved"
        );

        // The ceiling flushes without an explicit call; the end frame
        // flushes the remainder immediately.
        for index in 0..super::STREAM_MIRROR_BATCH {
            events.emit_assistant_stream(&AssistantStreamPayload {
                frame: frame(index as u64),
            });
        }
        assert_eq!(
            log.lock().expect("mirror log").len(),
            2,
            "the explicit batch plus the full-buffer flush"
        );
        events.emit_assistant_stream(&AssistantStreamPayload {
            frame: AssistantStreamFrame::End {
                attempt_id: fr_wire::ids::LlmAttemptId::new("s1:1"),
                revision: 1,
                index: 0,
                outcome: crate::assistant_stream::FrameOutcome::Abandoned,
            },
        });
        let crossings = log.lock().expect("mirror log").clone();
        assert_eq!(crossings.len(), 3, "the end frame flushed the remainder");
        assert_eq!(crossings[1].0, "agent/assistant-stream");
    }

    /// The M1 dispatch benchmark floor: the in-process registry plus the
    /// batched mirror sustain a chunk rate far above any provider's with
    /// margin (a generous CI-safe floor — the recorded figure prints for
    /// the acceptance review).
    #[test]
    fn stream_dispatch_meets_the_m1_benchmark_floor() {
        use crate::assistant_stream::AssistantStreamFrame;
        use fr_wire::StreamChunk;
        use std::time::Instant;
        let (mut events, log) = recording_events();
        let seen = Arc::new(std::sync::Mutex::new(0_u64));
        let sink = Arc::clone(&seen);
        events.on_assistant_stream(Box::new(move |_payload| {
            *sink.lock().expect("counter") += 1;
            Ok(())
        }));
        let chunks = 100_000_u64;
        let payload = AssistantStreamPayload {
            frame: AssistantStreamFrame::Chunk {
                attempt_id: fr_wire::ids::LlmAttemptId::new("s1:1"),
                revision: 1,
                index: 0,
                time: 1_700_000_000_000,
                chunk: Box::new(StreamChunk::Known(
                    fr_wire::stream::KnownStreamChunk::TextDelta {
                        index: 0,
                        text: "x".to_owned(),
                    },
                )),
            },
        };
        let started = Instant::now();
        for _ in 0..chunks {
            events.emit_assistant_stream(&payload);
        }
        events.flush_stream_mirror();
        let elapsed = started.elapsed();
        assert_eq!(
            *seen.lock().expect("counter"),
            chunks,
            "the listener saw every frame"
        );
        // The floor: 100k frames must dispatch (listener + batched mirror)
        // well inside five seconds — three orders of magnitude above a
        // generous provider chunk rate, safe on loaded CI runners.
        assert!(
            elapsed.as_secs_f64() < 5.0,
            "100k frames dispatched in {elapsed:?}"
        );
        let crossings = log.lock().expect("mirror log").len();
        // Throughput rides the assertion message on failure; the passing
        // case stays silent (the hook discipline: no debug prints).
        let _ = (chunks, crossings);
    }
}
