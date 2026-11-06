//! The one-shot runner (the dsh `headless/src/index.ts` port over the
//! fr wire).
//!
//! Behavior contract: parse the startup row, read the task (argument or
//! stdin), resolve the Agent (fresh identity or exact adoption with the
//! four refusals plus the live refusal and the resume re-check), drive the
//! task to quiescence, stream provider reasoning to stderr (default mode),
//! flush the session, summarize the durable log for the final answer, and
//! request process exit through the `headless/exit` event. With `--json`
//! the run projects newline-delimited events instead of the final text.
//!
//! Quiescence rides the decision-46 face (the `agent/status` event stream
//! plus a non-blocking query — a blocking require would violate the
//! shell's fast-path invariant): the runner consumes its own bus queue and
//! parks on it, so the whenIdle semantics are the event-driven twin of the
//! dsh `activityDone` do-while — wait for the claim (the Running flip)
//! after a followup, then the Idle commit. The `waitForChildren` lane is
//! structurally absent in M1 (no subagent runtime), so the seq-stability
//! double-check always holds on the first iteration; the loop shape stays
//! for wire parity.
//!
//! Known lane caveat: the pump is an advisory lane, so the JSON
//! projection's catch-up gate waits for the store's authoritative length
//! instead of trusting a flush marker (cross-name bus ordering is not
//! guaranteed). A driver thread that dies without an Idle commit (an
//! uncontained panic) parks the runner forever, exactly like the dsh
//! promise never settling — containment is the driver's own contract.

use std::collections::VecDeque;
use std::io::{IsTerminal, Write as _};
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;

use fr_agent_loop::assistant_stream::AssistantStreamFrame;
use fr_agent_loop::outbound::Outbound;
use fr_agent_loop::phase::AgentStatus;
use fr_wire::call_config::Failure;
use fr_wire::event::{SessionEvent, SessionHeader, TurnEndReason, TurnEndReasonKind};
use fr_wire::services::agent;
use fr_wire::services::session_persistence as persistence;
use fr_wire::services::session_query;

use crate::adopt::{assert_adoptable, refusals};
use crate::help_text::HEADLESS_HELP;
use crate::json_stream::{JsonProjection, JsonSink, bound_json_line};
use crate::reasoning::ReasoningStream;
use crate::startup::{StartupOutcome, StartupValues, parse};

/// The one decoded bus event the runner consumes.
#[derive(Debug)]
pub enum BusEvent {
    /// One `agent/status` flip (payload `{"sessionId","status"}`).
    Status {
        /// The session whose loop flipped.
        session_id: String,
        /// The new status.
        status: AgentStatus,
    },
    /// One `agent/error` boundary failure (the Q4B payload).
    Failure {
        /// The session whose boundary reported the failure.
        session_id: String,
        /// The session log seq at the failure (the consumer's `atOffset`).
        seq: u64,
        /// The structured failure facts.
        error: Failure,
    },
    /// One `agent/assistant-stream` bus batch (payload `{"frames":[...]}`).
    Frames(Vec<AssistantStreamFrame>),
    /// One `session/event` pump record (payload `{"sessionId","event"}`).
    SessionEvent {
        /// The session the event belongs to.
        session_id: String,
        /// The full event document.
        event: SessionEvent,
    },
}

/// The runner's event queue: the dylib `on_event` face pushes, the runner
/// thread pops (and parks on the condition when it needs the next event).
///
/// The `close` face breaks every wait at teardown: the runner is a
/// long-lived waiter on composition events, and the owner's coordinated
/// shutdown signals it before the join (the dsh twin: an early process
/// dispose while settlement is pending makes the runner return quietly).
#[derive(Debug, Default)]
pub struct RunnerBus {
    inner: Mutex<RunnerBusState>,
    signal: Condvar,
}

#[derive(Debug, Default)]
struct RunnerBusState {
    queue: VecDeque<BusEvent>,
    closed: bool,
}

impl RunnerBus {
    /// An empty queue.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(RunnerBusState {
                queue: VecDeque::new(),
                closed: false,
            }),
            signal: Condvar::new(),
        }
    }

    /// Pushes one decoded event and wakes the parked runner.
    ///
    /// # Panics
    ///
    /// Panics if the queue lock is poisoned (a holder panicked).
    pub fn push(&self, event: BusEvent) {
        {
            let mut state = self.inner.lock().expect("runner bus lock poisoned");
            state.queue.push_back(event);
        }
        self.signal.notify_all();
    }

    /// Closes the bus: every current and future wait drains the remaining
    /// events and then ends with [`None`] (the teardown's wake-before-join
    /// discipline).
    ///
    /// # Panics
    ///
    /// Panics if the queue lock is poisoned (a holder panicked).
    pub fn close(&self) {
        {
            let mut state = self.inner.lock().expect("runner bus lock poisoned");
            state.closed = true;
        }
        self.signal.notify_all();
    }

    /// Pops the next event, parking until one arrives; `None` once the
    /// bus is closed and drained.
    ///
    /// # Panics
    ///
    /// Panics if the queue lock is poisoned (a holder panicked).
    pub fn pop_wait(&self) -> Option<BusEvent> {
        let mut state = self.inner.lock().expect("runner bus lock poisoned");
        loop {
            if let Some(event) = state.queue.pop_front() {
                return Some(event);
            }
            if state.closed {
                return None;
            }
            state = self.signal.wait(state).expect("runner bus wait poisoned");
        }
    }
}

/// The runner's outbound faces: the copied vtable seam plus its own bus
/// queue (the `on_event` fan-in target).
#[derive(Debug, Clone)]
pub struct RunnerDeps {
    /// The activation's outbound seam (require + the exit emit).
    pub outbound: Outbound,
    /// The event queue the shell's `on_event` feeds.
    pub bus: Arc<RunnerBus>,
}

/// One cross-plugin service call, flattened to the failure text the dsh
/// error carries (`Error.message`).
fn require(
    deps: &RunnerDeps,
    service: &str,
    method: u32,
    payload: &[u8],
) -> Result<Vec<u8>, String> {
    deps.outbound
        .require(service, method, payload)
        .map_err(|error| error.to_string())
}

/// Encodes one JSON payload, failing loud on the encode (serde over
/// owned values cannot fail in practice; the arm keeps the face total).
fn encode(value: &serde_json::Value, what: &str) -> Result<Vec<u8>, String> {
    serde_json::to_vec(value).map_err(|error| format!("{what} payload encode failed: {error}"))
}

/// The model selection facts the runner reads from the relayed snapshot
/// (the host's Q1A bake; the dsh `agentDefaultModel.currentSelection`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelSelection {
    /// The provider route key.
    pub provider: Option<String>,
    /// The model id.
    pub model: Option<String>,
    /// The thinking effort row (`reasoning-effort` on the facts face).
    pub reasoning_effort: Option<String>,
}

impl ModelSelection {
    /// Reads the selection out of the facts document's `model` section.
    #[must_use]
    pub fn from_facts(facts: &serde_json::Value) -> Self {
        let model = facts.get("model");
        let text = |key: &str| {
            model
                .and_then(|section| section.get(key))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        Self {
            provider: text("provider"),
            model: text("model"),
            reasoning_effort: text("reasoning-effort"),
        }
    }
}

/// The run's final facts: the answer text and the turn outcome.
#[derive(Debug)]
pub struct RunOutcome {
    /// The last non-empty committed assistant text.
    pub text: String,
    /// The last `turn/end` reason on the run's slice.
    pub reason: Option<TurnEndReason>,
}

/// How one run ends: the exit code, or a quiet stop (the composition
/// tore down under the runner — the dsh `settlement pending` return; no
/// output faces, no exit request, the thread just ends).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveEnd {
    /// The run completed: request this exit code.
    Code(u32),
    /// The composition tore down under the runner: stop quietly.
    Shutdown,
}

/// One observation snapshot the adoption checks read.
struct Observation {
    /// The session header.
    header: SessionHeader,
    /// The durable events the snapshot carries.
    events: Vec<SessionEvent>,
}

/// Decodes one observation reply (`{"source","header","inheritedEventCount","events",...}`).
fn decode_observation(reply: &[u8]) -> Result<Observation, String> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Reply {
        header: SessionHeader,
        events: Vec<SessionEvent>,
    }
    let reply: Reply =
        serde_json::from_slice(reply).map_err(|error| format!("observation decode: {error}"))?;
    Ok(Observation {
        header: reply.header,
        events: reply.events,
    })
}

/// Decodes one log read reply (`{"events","total","header",...}`).
fn decode_read(reply: &[u8]) -> Result<(Vec<SessionEvent>, u64), String> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Reply {
        events: Vec<SessionEvent>,
        total: u64,
    }
    let reply: Reply =
        serde_json::from_slice(reply).map_err(|error| format!("log read decode: {error}"))?;
    Ok((reply.events, reply.total))
}

/// Reads the session's durable log length (the dsh `session.seq` read).
fn read_total(deps: &RunnerDeps, session_id: &str) -> Result<u64, String> {
    let payload = encode(&serde_json::json!({ "id": session_id }), "log read")?;
    let reply = require(
        deps,
        persistence::SERVICE_NAME,
        persistence::METHOD_SESSION_READ,
        &payload,
    )?;
    let (_, total) = decode_read(&reply)?;
    Ok(total)
}

/// Resolves the Agent for one run by adopting the persisted Session with
/// the requested id: the two presence double-checks, the live refusal,
/// the observation refusal family, the resume, and the post-lease
/// re-check (the dsh `resolveAgent` order, wording verbatim).
fn resolve_agent(
    deps: &RunnerDeps,
    session_id: &str,
    selection: &ModelSelection,
    cwd: &str,
) -> Result<(), String> {
    // Resuming promises the caller a log a later process can continue; a
    // composition without the persistence face would lose the history at
    // exit, so the miscomposed profile fails loud before the resume.
    if deps
        .outbound
        .require(
            persistence::SERVICE_NAME,
            persistence::METHOD_DESCRIBE,
            b"{}",
        )
        .is_err()
    {
        return Err(refusals::PERSISTENCE_REQUIRED.to_owned());
    }
    // A later process holds no live Agent and has to find the id through
    // the query service, so every `--session-id` run requires it.
    if deps
        .outbound
        .require(
            session_query::SERVICE_NAME,
            session_query::METHOD_DESCRIBE,
            b"{}",
        )
        .is_err()
    {
        return Err(refusals::QUERY_REQUIRED.to_owned());
    }
    let live = live_probe(deps, session_id)?;
    if live {
        // The adoptability rules run first so a real mismatch is named
        // instead of the generic refusal (the dsh order).
        let observation = observe(deps, session_id)?;
        assert_adoptable(&observation.header, &observation.events, session_id, cwd)?;
        return Err(refusals::live_agent(session_id));
    }
    let observation = match observe(deps, session_id) {
        Ok(observation) => observation,
        Err(error) => {
            if error.contains("SESSION_QUERY_SESSION_NOT_FOUND") {
                // `--session-id` resumes a conversation that already
                // exists; starting a new one is the no-id path, which
                // generates its own identity. Creating the requested id
                // here would turn a typo into a brand-new empty history
                // the caller believes it is continuing.
                return Err(refusals::not_found(session_id));
            }
            return Err(error);
        }
    };
    assert_adoptable(&observation.header, &observation.events, session_id, cwd)?;
    resume_driver(deps, session_id, selection)?;
    // The observation is a snapshot: another writer may have appended
    // before this process took the write lease. Re-check the log the
    // resume actually attached, now that no other process can append.
    let recheck = observe(deps, session_id)?;
    assert_adoptable(&recheck.header, &recheck.events, session_id, cwd)?;
    Ok(())
}

/// Probes the driver registry without minting a mailbox (the no-create
/// live read, the dsh `agents.get` face).
fn live_probe(deps: &RunnerDeps, session_id: &str) -> Result<bool, String> {
    #[derive(serde::Deserialize)]
    struct Reply {
        live: bool,
    }
    let payload = encode(
        &serde_json::json!({ "sessionId": session_id }),
        "live probe",
    )?;
    let reply = require(deps, agent::SERVICE_NAME, agent::METHOD_LIVE, &payload)?;
    let reply: Reply =
        serde_json::from_slice(&reply).map_err(|error| format!("live probe decode: {error}"))?;
    Ok(reply.live)
}

/// Reads one exact observation through the session-query face.
fn observe(deps: &RunnerDeps, session_id: &str) -> Result<Observation, String> {
    let payload = encode(&serde_json::json!({ "id": session_id }), "observe")?;
    let reply = require(
        deps,
        session_query::SERVICE_NAME,
        session_query::METHOD_OBSERVE,
        &payload,
    )?;
    decode_observation(&reply)
}

/// Resumes the session driver over the agent face (the Q9A payload).
fn resume_driver(
    deps: &RunnerDeps,
    session_id: &str,
    selection: &ModelSelection,
) -> Result<(), String> {
    let payload = encode(
        &serde_json::json!({
            "sessionId": session_id,
            "provider": selection.provider,
            "model": selection.model,
            "reasoningEffort": selection.reasoning_effort,
        }),
        "resume",
    )?;
    let reply = require(deps, agent::SERVICE_NAME, agent::METHOD_RESUME, &payload)?;
    expect_session_handle(&reply, session_id, "resume")
}

/// Creates the session driver over the agent face (the Q9A payload with
/// the workspace root).
fn create_driver(
    deps: &RunnerDeps,
    session_id: &str,
    cwd: &str,
    selection: &ModelSelection,
) -> Result<(), String> {
    let payload = encode(
        &serde_json::json!({
            "sessionId": session_id,
            "cwd": cwd,
            "provider": selection.provider,
            "model": selection.model,
            "reasoningEffort": selection.reasoning_effort,
        }),
        "create",
    )?;
    let reply = require(deps, agent::SERVICE_NAME, agent::METHOD_CREATE, &payload)?;
    expect_session_handle(&reply, session_id, "create")
}

/// Validates the lifecycle reply carries the session handle (the spec's
/// session-handle face).
fn expect_session_handle(reply: &[u8], session_id: &str, what: &str) -> Result<(), String> {
    let reply: serde_json::Value =
        serde_json::from_slice(reply).map_err(|error| format!("{what} reply decode: {error}"))?;
    let echoed = reply
        .get("sessionId")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("{what} reply carries no sessionId"))?;
    if echoed != session_id {
        return Err(format!(
            "{what} reply names session \"{echoed}\", expected \"{session_id}\""
        ));
    }
    Ok(())
}

/// The runner's own view of the consumed stream: the status mirror, the
/// post-followup claim latch, the failure tracker, and the projection
/// catch-up counters. All state flows through this one consumer, so the
/// whenIdle wait is race-free by construction (events arrive in order per
/// name and every observation happens on the consuming thread).
#[derive(Debug, Default)]
struct Observer {
    /// The last `agent/status` seen for the session.
    status: Option<AgentStatus>,
    /// Whether a Running flip was seen since the followup was submitted.
    running_latched: bool,
    /// The uncured `agent/error` (the dsh failure tracker: cleared by the
    /// first same-session `turn/end` at or past the failure offset).
    failure: Option<(Failure, u64)>,
    /// The highest session-event seq consumed (the catch-up gate).
    max_seq_seen: Option<u64>,
}

impl Observer {
    /// Absorbs one bus event, feeding the sinks it owns.
    fn absorb<O: JsonSink, E: JsonSink>(
        &mut self,
        event: BusEvent,
        session_id: &str,
        first_seq: u64,
        sinks: &mut Sinks<'_, O>,
        err: &mut E,
    ) {
        match event {
            BusEvent::Status {
                session_id: subject,
                status,
            } => {
                if subject != session_id {
                    return;
                }
                if status == AgentStatus::Running {
                    self.running_latched = true;
                }
                self.status = Some(status);
            }
            BusEvent::Failure {
                session_id: subject,
                seq,
                error,
            } => {
                if subject != session_id {
                    return;
                }
                self.failure = Some((error, seq));
            }
            BusEvent::Frames(frames) => {
                let Some(reasoning) = sinks.reasoning.as_mut() else {
                    return;
                };
                for frame in &frames {
                    reasoning.observe(frame, err);
                }
            }
            BusEvent::SessionEvent {
                session_id: subject,
                event,
            } => {
                if subject != session_id {
                    return;
                }
                let seq = event.seq.get();
                self.max_seq_seen = Some(self.max_seq_seen.map_or(seq, |seen| seen.max(seq)));
                // The failure cure: the first same-session terminal at or
                // past the failure offset (the dsh tracker).
                if event.event_type() == "turn/end"
                    && let Some((_, at_offset)) = self.failure
                    && seq >= at_offset
                {
                    self.failure = None;
                }
                // The projection folds only the run's slice (the events
                // appended after the runner captured `first_seq`): the
                // seq gate is the async-lane twin of the dsh subscribe
                // point.
                if seq >= first_seq
                    && let Some(projection) = sinks.projection.as_mut()
                {
                    projection.observe(&event);
                }
            }
        }
    }

    /// Parks the runner until the turn cycle the followup started has
    /// committed idle (the event-driven `whenIdle`): the no-news state is
    /// idle (the driver starts idle and only ever publishes flips), so
    /// before any activity is pending this returns immediately; after a
    /// followup it waits for the claim first, then the idle commit.
    /// Returns `false` when the bus closed (the composition is tearing
    /// down): every caller aborts the run quietly.
    fn wait_turn_cycle<O: JsonSink, E: JsonSink>(
        &mut self,
        deps: &RunnerDeps,
        session_id: &str,
        expect_activity: bool,
        first_seq: u64,
        sinks: &mut Sinks<'_, O>,
        err: &mut E,
    ) -> bool {
        while {
            let idle = self.status.is_none_or(|status| status == AgentStatus::Idle);
            !idle || (expect_activity && !self.running_latched)
        } {
            let Some(event) = deps.bus.pop_wait() else {
                return false;
            };
            self.absorb(event, session_id, first_seq, sinks, err);
        }
        true
    }
}

/// The per-run output sinks over one borrowed stdout face.
struct Sinks<'a, O: JsonSink> {
    /// The `--json` projection (`None` in default mode).
    projection: Option<JsonProjection<&'a mut O>>,
    /// The default-mode reasoning projection (`None` in `--json` mode).
    reasoning: Option<ReasoningStream>,
}

/// The process stdout face (the runner is the sole stdout writer).
#[derive(Clone, Default)]
struct StdoutSink;

impl JsonSink for StdoutSink {
    fn write(&mut self, chunk: &str) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(chunk.as_bytes());
        let _ = out.flush();
    }
}

/// The process stderr face.
struct StderrSink;

impl JsonSink for StderrSink {
    fn write(&mut self, chunk: &str) {
        let mut out = std::io::stderr().lock();
        let _ = out.write_all(chunk.as_bytes());
        let _ = out.flush();
    }
}

/// The process-facing output pair (the dsh `HeadlessIo` twin). The real
/// runner uses the stdio faces; tests substitute captures.
#[derive(Debug)]
pub struct Faces<O: JsonSink, E: JsonSink> {
    /// The stdout face (the answer or the event stream).
    pub out: O,
    /// The stderr face (reasoning, usage text, failure lines).
    pub err: E,
}

/// Aggregates the last assistant text and turn outcome over one run's
/// slice of the durable log (the dsh `summarize`; a hole in the slice is
/// a loud error, not a silent skip).
///
/// # Errors
///
/// Fails when the log read leaves a gap inside the run's slice (the dsh
/// `headless summary cannot read seq` refusal).
fn summarize(events: &[SessionEvent], first_seq: u64, total: u64) -> Result<RunOutcome, String> {
    let mut started = false;
    let mut text = String::new();
    let mut reason: Option<TurnEndReason> = None;
    let mut selected = 0_u64;
    for event in events.iter().filter(|event| event.seq.get() >= first_seq) {
        selected += 1;
        if matches!(&event.data, fr_wire::event::SessionEventData::TurnStart(_)) {
            started = true;
            continue;
        }
        if !started {
            continue;
        }
        if let fr_wire::event::SessionEventData::AssistantMessage(message) = &event.data
            && let fr_wire::Message::Known(message) = &message.message
            && let fr_wire::KnownMessage::Assistant { content, .. } = message.as_ref()
        {
            let joined = content
                .iter()
                .filter_map(|block| match block {
                    fr_wire::ContentBlock::Known(fr_wire::KnownContentBlock::Text {
                        text, ..
                    }) => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>();
            if !joined.is_empty() {
                text = joined;
            }
        }
        if let fr_wire::event::SessionEventData::TurnEnd(turn_end) = &event.data {
            reason = Some(turn_end.reason.clone());
        }
    }
    // The dsh reader walks `firstSeq..length` by offset and throws on a
    // hole; the equivalent integrity check over the seq-filtered slice.
    let expected = total.saturating_sub(first_seq);
    if selected != expected {
        return Err(format!(
            "headless summary cannot read the run slice: {selected} events read below captured length {total} from {first_seq}"
        ));
    }
    Ok(RunOutcome { text, reason })
}

/// The exit request event (the 退出轨 ruling; the host's watcher listens
/// for exactly this name — the fr crate owns the listening side, so the
/// name is pinned here and in the host's own face pin).
pub const EXIT_EVENT: &str = "headless/exit";

/// Requests the process exit through the exit event (the plugin-to-host
/// push face). A failed emit means the bus is already gone (the host is
/// tearing down or dead), so the process exits directly — fail-closed,
/// never a silent hang.
fn request_exit(deps: &RunnerDeps, code: u32) {
    let payload = serde_json::json!({ "code": code }).to_string();
    if deps.outbound.emit(EXIT_EVENT, payload.as_bytes()).is_err() {
        std::process::exit(code.cast_signed());
    }
}

/// Runs one startup row to completion: the runner-level task resolution,
/// the session resolution, the run interval, and the output faces.
/// Returns the process exit code, or the failure message the dsh `fail`
/// twin reports.
#[expect(
    clippy::too_many_lines,
    reason = "the body is the dsh run() flow one arm per face; splitting it would hide the dsh order"
)]
fn run_values<O: JsonSink + Clone, E: JsonSink>(
    deps: &RunnerDeps,
    values: &StartupValues,
    selection: &ModelSelection,
    read_stdin: impl FnOnce() -> Result<String, String>,
    io: &mut Faces<O, E>,
) -> Result<DriveEnd, String> {
    let Faces { out, err } = io;
    // The answer face for the default mode: cloned before the projection
    // can consume the stdout face, so both faces reach the same stream
    // (the capture sinks share the buffer; the stdio unit is stateless).
    let mut answer_face = out.clone();
    // A cordis-style overlay could set the row directly and bypass the
    // CLI trim check, so the same public setting must fail here rather
    // than become a blank identity.
    if values
        .session_id
        .as_ref()
        .is_some_and(|session_id| session_id.trim().is_empty())
    {
        return Err("headless-runner: sessionId must not be blank".to_owned());
    }
    let task = match values.task.as_deref() {
        // The bare `-` defers to stdin (the dsh run face); None is the
        // omitted argument.
        Some("-") | None => read_stdin()?,
        Some(task) => task.to_owned(),
    };
    if task.trim().is_empty() {
        return Err("a task is required, for example: fr headless \"run the tests\"".to_owned());
    }
    let cwd = match std::env::current_dir() {
        Ok(dir) => dir.to_string_lossy().into_owned(),
        Err(error) => return Err(format!("working directory resolve failed: {error}")),
    };
    let session_id = values
        .session_id
        .clone()
        .unwrap_or_else(|| format!("session-{}", uuid::Uuid::new_v4()));
    match &values.session_id {
        None => create_driver(deps, &session_id, &cwd, selection)?,
        Some(requested) => {
            if requested != &session_id {
                return Err("session identity resolution diverged".to_owned());
            }
            resolve_agent(deps, requested, selection, &cwd)?;
        }
    }
    // The first `whenIdle`: a fresh or resumed driver starts idle, so
    // this returns immediately (the dsh pre-followup await).
    let mut observer = Observer::default();
    let mut sinks = Sinks {
        projection: None,
        reasoning: None,
    };
    if !observer.wait_turn_cycle(deps, &session_id, false, 0, &mut sinks, err) {
        return Ok(DriveEnd::Shutdown);
    }
    if values.session_id.is_some() {
        // The resume-time check read a snapshot; an overlay could still
        // have appended a preset selection between it and the interval
        // this run now owns, so re-read the log before submitting the
        // task (the dsh order).
        let recheck = observe(deps, &session_id)?;
        assert_adoptable(&recheck.header, &recheck.events, &session_id, &cwd)?;
    }
    let first_seq = read_total(deps, &session_id)?;
    if values.json {
        sinks.projection = Some(JsonProjection::new(out, &session_id, &cwd));
    } else {
        sinks.reasoning = Some(ReasoningStream::new());
    }
    // The run interval, with the reasoning stop as its twin finally.
    let activity: Result<Option<DriveEnd>, String> = (|| {
        let message = serde_json::json!({
            "sessionId": session_id,
            "message": {
                "role": "user",
                "id": uuid::Uuid::new_v4().to_string(),
                "content": [{ "type": "text", "text": task }],
                "source": { "kind": "user" },
            },
        });
        let payload = encode(&message, "followup")?;
        require(deps, agent::SERVICE_NAME, agent::METHOD_FOLLOWUP, &payload)?;
        let mut first = true;
        loop {
            if !observer.wait_turn_cycle(deps, &session_id, first, first_seq, &mut sinks, err) {
                return Ok(Some(DriveEnd::Shutdown));
            }
            first = false;
            // Child settlement can wake the parent before the child wait
            // observes it; M1 has no subagent runtime (the children lane
            // is structurally absent), so the stability check always
            // holds on the first pass — the loop shape stays for parity.
            let idle_seq = read_total(deps, &session_id)?;
            if observer.status == Some(AgentStatus::Idle)
                && read_total(deps, &session_id)? == idle_seq
            {
                break;
            }
        }
        Ok(None)
    })();
    // The inner finally: terminate an unterminated reasoning line.
    if let Some(reasoning) = sinks.reasoning.as_mut() {
        reasoning.close(err);
    }
    if activity? == Some(DriveEnd::Shutdown) {
        return Ok(DriveEnd::Shutdown);
    }
    let collected = flush_and_collect(deps, &mut observer, &mut sinks, &session_id, first_seq, err);
    let result = match collected {
        Ok(None) => Ok(DriveEnd::Shutdown),
        Ok(Some(outcome)) => {
            // Take the projection out so the answer write and the
            // terminal frame share the stdout face without overlapping
            // borrows; the json arm disposes before the drop.
            let projection = sinks.projection.take();
            if values.json {
                let Some(mut projection) = projection else {
                    return Err("the json projection is not installed".to_owned());
                };
                projection.finish(&outcome.text);
                projection.dispose();
            } else {
                JsonSink::write(&mut answer_face, &(outcome.text.clone() + "\n"));
            }
            if let Some(TurnEndReason::Known(TurnEndReasonKind::Error { error })) = &outcome.reason
            {
                JsonSink::write(err, &format!("fr: {}: {}\n", error.code, error.message));
            }
            let code = match &outcome.reason {
                Some(TurnEndReason::Known(TurnEndReasonKind::Completed)) => 0,
                _ => 1,
            };
            Ok(DriveEnd::Code(code))
        }
        Err(message) => Err(message),
    };
    // The outer finally (the throw paths): the projection stops observing.
    if let Some(mut projection) = sinks.projection.take() {
        projection.dispose();
    }
    result
}

/// The post-quiescence collection face: durability, the projection
/// catch-up gate, the uncured-failure throw, and the summary. The answer
/// output and the sink disposal stay with the caller (the dsh face
/// order: finish, then the error line, then the outer finally).
fn flush_and_collect<O: JsonSink, E: JsonSink>(
    deps: &RunnerDeps,
    observer: &mut Observer,
    sinks: &mut Sinks<'_, O>,
    session_id: &str,
    first_seq: u64,
    err: &mut E,
) -> Result<Option<RunOutcome>, String> {
    let payload = encode(&serde_json::json!({ "id": session_id }), "flush")?;
    require(
        deps,
        persistence::SERVICE_NAME,
        persistence::METHOD_SESSION_FLUSH,
        &payload,
    )?;
    // The projection catch-up gate: the pump is an advisory lane, so the
    // runner waits until the consumed slice reaches the store's
    // authoritative length before the terminal frame projects.
    let total = read_total(deps, session_id)?;
    while observer
        .max_seq_seen
        .map_or(total > 0, |seen| seen + 1 < total)
    {
        let Some(event) = deps.bus.pop_wait() else {
            return Ok(None);
        };
        observer.absorb(event, session_id, first_seq, sinks, err);
    }
    // An uncured boundary failure ends the run with that failure (the
    // dsh `throw failure.error`; the fail face reports the message).
    if let Some((error, _)) = &observer.failure {
        return Err(error.message.clone());
    }
    let payload = encode(&serde_json::json!({ "id": session_id }), "log read")?;
    let reply = require(
        deps,
        persistence::SERVICE_NAME,
        persistence::METHOD_SESSION_READ,
        &payload,
    )?;
    let (events, total) = decode_read(&reply)?;
    summarize(&events, first_seq, total).map(Some)
}

/// The process face: parse the startup row, read stdin when asked, run,
/// and request the exit. This is the runner thread's body (the stdio
/// faces); tests drive the same shape with capture faces.
pub fn run<O: JsonSink + Clone, E: JsonSink>(
    argv: &[String],
    facts: &serde_json::Value,
    deps: &RunnerDeps,
    tty: bool,
    io: &mut Faces<O, E>,
    read_stdin: impl FnOnce() -> Result<String, String>,
) {
    let outcome = parse(argv, tty);
    match outcome {
        StartupOutcome::Help => {
            JsonSink::write(&mut io.out, HEADLESS_HELP);
            request_exit(deps, 0);
        }
        StartupOutcome::Usage(usage) => {
            if usage.json_requested {
                let line = bound_json_line(
                    &serde_json::json!({ "type": "error", "message": usage.message }),
                    crate::json_stream::MAX_STRING_BYTES,
                    crate::json_stream::MAX_EVENT_BYTES,
                );
                JsonSink::write(&mut io.out, &format!("{line}\n"));
            }
            JsonSink::write(&mut io.err, &format!("{}\n", usage.message));
            request_exit(deps, 1);
        }
        StartupOutcome::Values(values) => {
            let selection = ModelSelection::from_facts(facts);
            let result = run_values(deps, &values, &selection, read_stdin, io);
            match result {
                // The composition tore down under the runner: the dsh
                // quiet return (no output faces, no exit request — the
                // teardown owns the process from here).
                Ok(DriveEnd::Shutdown) => {}
                Ok(DriveEnd::Code(code)) => request_exit(deps, code),
                Err(message) => {
                    if values.json {
                        let line = bound_json_line(
                            &serde_json::json!({ "type": "error", "message": message }),
                            crate::json_stream::MAX_STRING_BYTES,
                            crate::json_stream::MAX_EVENT_BYTES,
                        );
                        JsonSink::write(&mut io.out, &format!("{line}\n"));
                    }
                    JsonSink::write(&mut io.err, &format!("fr: {message}\n"));
                    request_exit(deps, 1);
                }
            }
        }
    }
}

/// The stdio entry the runner thread calls.
pub fn run_stdio(argv: &[String], facts: &serde_json::Value, deps: &RunnerDeps) {
    let read_stdin = || -> Result<String, String> {
        use std::io::Read as _;
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|error| format!("stdin read failed: {error}"))?;
        Ok(text)
    };
    let mut io = Faces {
        out: StdoutSink,
        err: StderrSink,
    };
    run(
        argv,
        facts,
        deps,
        !std::io::stdin().is_terminal(),
        &mut io,
        read_stdin,
    );
}

/// Decodes one `agent/status` payload (`{"sessionId","status"}`).
///
/// # Errors
///
/// Returns the decode refusal for a malformed payload.
pub fn decode_status(payload: &[u8]) -> Result<BusEvent, String> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Reply {
        session_id: String,
        status: AgentStatus,
    }
    let reply: Reply =
        serde_json::from_slice(payload).map_err(|error| format!("status decode: {error}"))?;
    Ok(BusEvent::Status {
        session_id: reply.session_id,
        status: reply.status,
    })
}

/// Decodes one `agent/error` payload (the Q4B `{sessionId,turn,step,error,seq}`;
/// the runner consumes the identity, the offset, and the facts).
///
/// # Errors
///
/// Returns the decode refusal for a malformed payload.
pub fn decode_failure(payload: &[u8]) -> Result<BusEvent, String> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Reply {
        session_id: String,
        seq: u64,
        error: Failure,
    }
    let reply: Reply =
        serde_json::from_slice(payload).map_err(|error| format!("error decode: {error}"))?;
    Ok(BusEvent::Failure {
        session_id: reply.session_id,
        seq: reply.seq,
        error: reply.error,
    })
}

/// Decodes one `agent/assistant-stream` bus batch (`{"frames":[...]}`).
///
/// # Errors
///
/// Returns the decode refusal for a malformed payload.
pub fn decode_frames(payload: &[u8]) -> Result<BusEvent, String> {
    #[derive(serde::Deserialize)]
    struct Reply {
        frames: Vec<AssistantStreamFrame>,
    }
    let reply: Reply =
        serde_json::from_slice(payload).map_err(|error| format!("frames decode: {error}"))?;
    Ok(BusEvent::Frames(reply.frames))
}

/// Decodes one `session/event` pump record (`{"sessionId","event"}`).
///
/// # Errors
///
/// Returns the decode refusal for a malformed payload.
pub fn decode_session_event(payload: &[u8]) -> Result<BusEvent, String> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Reply {
        session_id: String,
        event: SessionEvent,
    }
    let reply: Reply = serde_json::from_slice(payload)
        .map_err(|error| format!("session event decode: {error}"))?;
    Ok(BusEvent::SessionEvent {
        session_id: reply.session_id,
        event: reply.event,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        BusEvent, DriveEnd, Faces, ModelSelection, RunnerBus, RunnerDeps, decode_failure,
        decode_frames, decode_session_event, decode_status, run, run_values, summarize,
    };
    use crate::json_stream::JsonSink;
    use crate::startup::StartupValues;
    use fr_agent_loop::outbound::Outbound;
    use fr_wire::event::{SessionEvent, SessionHeader, TurnEndReasonKind};
    use fr_wire::services::{agent, session_persistence as persistence, session_query};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// The scripted host: `(service, method)` handlers plus the recorded
    /// emits (the fake `HostExports` the runner's vtable calls land on).
    type Script = Box<dyn Fn(&[u8]) -> Result<Vec<u8>, String> + Send>;
    type ScriptTable = Mutex<HashMap<(String, u32), Script>>;

    struct ScriptedHost {
        handlers: ScriptTable,
        emits: Mutex<Vec<(String, Vec<u8>)>>,
    }

    impl ScriptedHost {
        fn new() -> Self {
            Self {
                handlers: Mutex::new(HashMap::new()),
                emits: Mutex::new(Vec::new()),
            }
        }

        fn on(
            &self,
            service: &str,
            method: u32,
            handler: impl Fn(&[u8]) -> Result<Vec<u8>, String> + Send + 'static,
        ) {
            self.handlers
                .lock()
                .expect("handlers lock")
                .insert((service.to_owned(), method), Box::new(handler));
        }

        fn emitted(&self) -> Vec<(String, serde_json::Value)> {
            self.emits
                .lock()
                .expect("emits lock")
                .iter()
                .map(|(name, payload)| {
                    let payload =
                        serde_json::from_slice(payload).unwrap_or(serde_json::Value::Null);
                    (name.clone(), payload)
                })
                .collect()
        }
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "the scripted handler runs against the table's own lock; the table is static within one test case"
    )]
    unsafe extern "C" fn scripted_require(
        host: *mut std::ffi::c_void,
        service: *const u8,
        service_len: usize,
        method: u32,
        in_data: *const u8,
        in_len: usize,
        out: *mut u8,
        cap: usize,
        out_len: *mut usize,
    ) -> i32 {
        // SAFETY: the host handle is the `ScriptedHost` the test installed
        // in the vtable; the raw pointer stays valid for the whole case.
        let host = unsafe { &*(host.cast::<ScriptedHost>()) };
        // SAFETY: the vtable contract passes valid slices for their lengths.
        let service = unsafe { std::slice::from_raw_parts(service, service_len) };
        let service = String::from_utf8_lossy(service).to_string();
        let payload: &[u8] = if in_len == 0 {
            &[]
        } else {
            // SAFETY: the vtable contract passes `in_len` readable bytes
            // at `in_data` (null/0 pairs with the empty slice).
            unsafe { std::slice::from_raw_parts(in_data, in_len) }
        };
        let handlers = host.handlers.lock().expect("handlers lock");
        let Some(handler) = handlers
            .get(&(service.clone(), method))
            .map(|handler| handler.as_ref())
        else {
            let detail = format!("unscripted call {service}/{method}");
            // SAFETY: host buffer contract.
            let _ = unsafe { dougong_abi::write_out(detail.as_bytes(), out, cap, out_len) };
            return dougong_abi::STATUS_FAILED;
        };
        match handler(payload) {
            Ok(reply) => {
                // SAFETY: host buffer contract.
                if unsafe { dougong_abi::write_out(&reply, out, cap, out_len) } {
                    dougong_abi::STATUS_OK
                } else {
                    dougong_abi::STATUS_CAPACITY
                }
            }
            Err(text) => {
                let body = serde_json::json!({ "error": text }).to_string();
                // SAFETY: host buffer contract.
                let _ = unsafe { dougong_abi::write_out(body.as_bytes(), out, cap, out_len) };
                dougong_abi::STATUS_FAILED
            }
        }
    }

    unsafe extern "C" fn scripted_emit(
        host: *mut std::ffi::c_void,
        event: *const u8,
        event_len: usize,
        payload: *const u8,
        payload_len: usize,
    ) -> i32 {
        // SAFETY: the host handle is the `ScriptedHost` the test installed
        // in the vtable.
        let host = unsafe { &*(host.cast::<ScriptedHost>()) };
        // SAFETY: the vtable contract passes a valid event slice.
        let event = unsafe { std::slice::from_raw_parts(event, event_len) }.to_vec();
        // SAFETY: the vtable contract passes a valid payload slice.
        let payload = unsafe { std::slice::from_raw_parts(payload, payload_len) }.to_vec();
        host.emits
            .lock()
            .expect("emits lock")
            .push((String::from_utf8_lossy(&event).to_string(), payload));
        dougong_abi::STATUS_OK
    }

    unsafe extern "C" fn quiet_grant(
        _host: *mut std::ffi::c_void,
        _target: *const u8,
        _target_len: usize,
        _granted: *const u8,
        _granted_len: usize,
    ) -> i32 {
        dougong_abi::STATUS_OK
    }

    unsafe extern "C" fn noop_dispatch(
        _host: *mut std::ffi::c_void,
        _event: *const u8,
        _event_len: usize,
        _mode: u32,
        _payload: *const u8,
        _payload_len: usize,
        _out: *mut u8,
        _cap: usize,
        _out_len: *mut usize,
    ) -> i32 {
        dougong_abi::STATUS_FAILED
    }

    /// The runner faces over one scripted host plus its bus.
    fn rig(host: &Arc<ScriptedHost>) -> (RunnerDeps, Arc<RunnerBus>) {
        let bus = Arc::new(RunnerBus::new());
        let outbound = Outbound::new(dougong_abi::HostExports {
            grant: quiet_grant,
            dispatch: noop_dispatch,
            // SAFETY: the raw pointer rides the vtable only; the test
            // holds the owning `Arc` for the whole case.
            host_handle: Arc::as_ptr(host).cast_mut().cast::<std::ffi::c_void>(),
            require: scripted_require,
            emit: scripted_emit,
            realm_spawn: crate::plugin_shell::quiet_realm_spawn,
            realm_retire: crate::plugin_shell::quiet_realm_retire,
            realm_require: crate::plugin_shell::quiet_realm_require,
        });
        (
            RunnerDeps {
                outbound,
                bus: Arc::clone(&bus),
            },
            bus,
        )
    }

    /// One durable event decoded from its full wire envelope.
    fn event(seq: u64, kind: &str, data: &serde_json::Value) -> SessionEvent {
        serde_json::from_value(serde_json::json!({
            "type": kind,
            "seq": seq,
            "time": 0,
            "data": data,
        }))
        .expect("fixture event")
    }

    fn header_with_cwd(cwd: Option<&str>) -> SessionHeader {
        serde_json::from_value(serde_json::json!({
            "version": 4,
            "id": "session-fixture",
            "createdAt": 0,
            "cwd": cwd,
            "isSeeded": false,
            "delegationDepth": 0,
        }))
        .expect("fixture header")
    }

    /// The capture faces over shared buffers.
    fn capture_faces() -> Faces<CaptureSink, CaptureSink> {
        Faces {
            out: CaptureSink::default(),
            err: CaptureSink::default(),
        }
    }

    /// A string-capturing sink whose clones share one buffer.
    #[derive(Clone, Default, Debug)]
    struct CaptureSink(Arc<Mutex<String>>);

    impl JsonSink for CaptureSink {
        fn write(&mut self, chunk: &str) {
            self.0.lock().expect("capture lock").push_str(chunk);
        }
    }

    fn captured(sink: &CaptureSink) -> String {
        sink.0.lock().expect("capture lock").clone()
    }

    fn values_of(argv: &[String]) -> StartupValues {
        match crate::startup::parse(argv, true) {
            crate::startup::StartupOutcome::Values(values) => values,
            other => panic!("expected values, got {other:?}"),
        }
    }

    fn current_cwd() -> String {
        std::env::current_dir()
            .expect("test cwd")
            .to_string_lossy()
            .into_owned()
    }

    const SESSION: &str = "session-fixture";

    /// The followup arm's body: the claim, the durable slice (with the
    /// optional boundary failure and its cure), then the idle commit.
    fn followup_arm(
        bus: &RunnerBus,
        events: &Mutex<Vec<SessionEvent>>,
        payload: &[u8],
        failure: Option<&(u64, String, String)>,
        terminal: bool,
        expected_task: &str,
    ) -> Result<Vec<u8>, String> {
        let request: serde_json::Value =
            serde_json::from_slice(payload).map_err(|error| format!("decode: {error}"))?;
        assert_eq!(
            request["message"]["content"][0]["text"].as_str(),
            Some(expected_task),
            "the followup carries the task text"
        );
        // The created identity is the runner's own fresh uuid (the dsh
        // no-id path), so the turn rides the payload's session id.
        let session_id = request["sessionId"]
            .as_str()
            .ok_or("no sessionId")?
            .to_owned();
        // The turn runs: the claim, the durable slice, then idle.
        bus.push(BusEvent::Status {
            session_id: session_id.clone(),
            status: fr_agent_loop::phase::AgentStatus::Running,
        });
        let mut slice = vec![
            event(0, "turn/start", &serde_json::json!({ "turn": 1 })),
            event(
                1,
                "user/message",
                &serde_json::json!({
                    "turn": 1,
                    "message": {
                        "role": "user",
                        "id": "u1",
                        "content": [{ "type": "text", "text": "run the tests" }],
                        "source": { "kind": "user" },
                    },
                }),
            ),
            event(
                2,
                "assistant/message",
                &serde_json::json!({
                    "turn": 1,
                    "step": 1,
                    "message": {
                        "role": "assistant",
                        "id": "a1",
                        "content": [{ "type": "text", "text": "all green" }],
                        "source": { "kind": "model", "provider": "p", "model": "m" },
                    },
                    "stream": [],
                }),
            ),
        ];
        // A contained boundary failure leaves the turn terminal-less (the
        // dsh tracker's uncured shape); a healthy run commits its
        // terminal, which also cures any earlier failure at or past it.
        if terminal {
            slice.push(event(
                3,
                "turn/end",
                &serde_json::json!({
                    "turn": 1,
                    "reason": { "kind": "completed" },
                }),
            ));
        }
        *events.lock().expect("events lock") = slice.clone();
        // The bus order mirrors the live turn: the slice up to the
        // failure boundary, the boundary error, then the terminal (the
        // cure), then the idle commit.
        let pre_terminal = if failure.is_some() && terminal {
            3
        } else {
            slice.len()
        };
        for item in &slice[..pre_terminal] {
            bus.push(BusEvent::SessionEvent {
                session_id: session_id.clone(),
                event: item.clone(),
            });
        }
        if let Some((seq, code, message)) = failure {
            bus.push(
                decode_failure(
                    &serde_json::to_vec(&serde_json::json!({
                        "sessionId": session_id,
                        "turn": 1,
                        "step": 1,
                        "error": { "message": message, "code": code },
                        "seq": seq,
                    }))
                    .expect("error payload"),
                )
                .expect("decode"),
            );
        }
        for item in &slice[pre_terminal..] {
            bus.push(BusEvent::SessionEvent {
                session_id: session_id.clone(),
                event: item.clone(),
            });
        }
        bus.push(BusEvent::Status {
            session_id,
            status: fr_agent_loop::phase::AgentStatus::Idle,
        });
        Ok(serde_json::to_vec(&serde_json::json!({ "queued": true })).expect("reply"))
    }

    /// Installs the happy-path lifecycle script: CREATE echoes the id,
    /// FOLLOWUP runs the turn (status flips + the four durable events),
    /// FLUSH participates, READ serves the live log length.
    fn script_happy_turn(
        host: &ScriptedHost,
        bus: &Arc<RunnerBus>,
        events: &Arc<Mutex<Vec<SessionEvent>>>,
        failure: Option<(u64, &str, &str)>,
        terminal: bool,
    ) {
        host.on(agent::SERVICE_NAME, agent::METHOD_CREATE, |payload| {
            let request: serde_json::Value =
                serde_json::from_slice(payload).map_err(|error| format!("decode: {error}"))?;
            let id = request
                .get("sessionId")
                .and_then(serde_json::Value::as_str)
                .ok_or("no sessionId")?
                .to_owned();
            Ok(serde_json::to_vec(&serde_json::json!({ "sessionId": id })).expect("reply"))
        });
        let followup_bus = Arc::clone(bus);
        let followup_events = Arc::clone(events);
        let failure =
            failure.map(|(seq, code, message)| (seq, code.to_owned(), message.to_owned()));
        host.on(
            agent::SERVICE_NAME,
            agent::METHOD_FOLLOWUP,
            move |payload| {
                followup_arm(
                    &followup_bus,
                    &followup_events,
                    payload,
                    failure.as_ref(),
                    terminal,
                    "run the tests",
                )
            },
        );
        host.on(
            persistence::SERVICE_NAME,
            persistence::METHOD_SESSION_FLUSH,
            |_| Ok(br#"{"participated": true}"#.to_vec()),
        );
        let read_events = Arc::clone(events);
        host.on(
            persistence::SERVICE_NAME,
            persistence::METHOD_SESSION_READ,
            move |_| {
                let (items, total) = {
                    let events = read_events.lock().expect("events lock");
                    let items: Vec<serde_json::Value> = events
                        .iter()
                        .map(|event| serde_json::to_value(event).expect("event encode"))
                        .collect();
                    (items, events.len())
                };
                Ok(serde_json::to_vec(&serde_json::json!({
                    "events": items,
                    "total": total,
                    "header": header_with_cwd(Some("cwd")),
                    "inheritedEventCount": 0,
                }))
                .expect("read reply"))
            },
        );
    }

    fn empty_observation_script(host: &ScriptedHost, live: bool) {
        host.on(agent::SERVICE_NAME, agent::METHOD_LIVE, move |_| {
            Ok(serde_json::to_vec(&serde_json::json!({ "live": live })).expect("reply"))
        });
        host.on(
            session_query::SERVICE_NAME,
            session_query::METHOD_DESCRIBE,
            |_| Ok(br"{}".to_vec()),
        );
        host.on(
            persistence::SERVICE_NAME,
            persistence::METHOD_DESCRIBE,
            |_| Ok(br"{}".to_vec()),
        );
        host.on(
            session_query::SERVICE_NAME,
            session_query::METHOD_OBSERVE,
            |_| {
                Ok(serde_json::to_vec(&serde_json::json!({
                    "source": "live",
                    "header": header_with_cwd(Some(&current_cwd())),
                    "inheritedEventCount": 0,
                    "events": [],
                }))
                .expect("observe reply"))
            },
        );
    }

    #[test]
    fn create_path_drives_to_completion_and_prints_the_answer() {
        let host = Arc::new(ScriptedHost::new());
        let (deps, bus) = rig(&host);
        let events = Arc::new(Mutex::new(Vec::new()));
        script_happy_turn(&host, &bus, &events, None, true);
        let facts = serde_json::json!({
            "model": { "provider": "deepseek-official", "model": "deepseek-flash" },
        });
        let values = values_of(&["run the tests".to_owned()]);
        let selection = ModelSelection::from_facts(&facts);
        let mut io = capture_faces();
        let code = run_values(&deps, &values, &selection, || Ok(String::new()), &mut io)
            .expect("the run completes");
        assert_eq!(code, DriveEnd::Code(0), "completed exits zero");
        assert_eq!(captured(&io.out), "all green\n");
        assert_eq!(captured(&io.err), "");
    }

    #[test]
    fn json_mode_projects_the_frames_and_the_final_answer() {
        let host = Arc::new(ScriptedHost::new());
        let (deps, bus) = rig(&host);
        let events = Arc::new(Mutex::new(Vec::new()));
        script_happy_turn(&host, &bus, &events, None, true);
        let values = values_of(&["--json".to_owned(), "run the tests".to_owned()]);
        assert!(values.json);
        let selection = ModelSelection::default();
        let mut io = capture_faces();
        let code = run_values(&deps, &values, &selection, || Ok(String::new()), &mut io)
            .expect("the run completes");
        assert_eq!(code, DriveEnd::Code(0));
        let stream = captured(&io.out);
        let lines: Vec<serde_json::Value> = stream
            .lines()
            .map(|line| serde_json::from_str(line).expect("json line"))
            .collect();
        assert_eq!(
            lines.first().expect("frames")["type"],
            "session",
            "the opening frame"
        );
        let opened = lines.first().expect("frames")["sessionId"]
            .as_str()
            .expect("session id");
        assert!(
            opened.starts_with("session-"),
            "the fresh identity: {opened}"
        );
        let texts: Vec<&str> = lines
            .iter()
            .filter(|frame| frame["type"] == "text")
            .map(|frame| frame["text"].as_str().expect("text"))
            .collect();
        assert_eq!(texts, ["all green"], "the committed text projects once");
        assert_eq!(lines.last().expect("frames")["type"], "final");
        assert_eq!(lines.last().expect("frames")["text"], "all green");
    }

    #[test]
    fn reasoning_streams_to_stderr_in_default_mode() {
        let host = Arc::new(ScriptedHost::new());
        let (deps, bus) = rig(&host);
        let events = Arc::new(Mutex::new(Vec::new()));
        script_happy_turn(&host, &bus, &events, None, true);
        // One reasoning burst rides the bus ahead of the turn slice: the
        // followup handler pushes the frames, then runs the shared arm.
        let reasoning_bus = Arc::clone(&bus);
        let events_for_frames = Arc::clone(&events);
        host.on(
            agent::SERVICE_NAME,
            agent::METHOD_FOLLOWUP,
            move |payload| {
                let frames = decode_frames(
                    &serde_json::to_vec(&serde_json::json!({ "frames": [
                    { "type": "chunk", "attemptId": "att-1", "revision": 1, "index": 0, "time": 0,
                      "chunk": { "type": "reasoning-delta", "index": 0, "text": "thinking" } },
                ] }))
                    .expect("frames payload"),
                )
                .expect("decode");
                reasoning_bus.push(frames);
                followup_arm(
                    &reasoning_bus,
                    &events_for_frames,
                    payload,
                    None,
                    true,
                    "run the tests",
                )
            },
        );
        let values = values_of(&["run the tests".to_owned()]);
        let mut io = capture_faces();
        let code = run_values(
            &deps,
            &values,
            &ModelSelection::default(),
            || Ok(String::new()),
            &mut io,
        )
        .expect("the run completes");
        assert_eq!(code, DriveEnd::Code(0));
        assert!(captured(&io.err).starts_with("fr: reasoning:\n"));
        assert!(captured(&io.err).contains("thinking"));
    }

    #[test]
    fn uncured_agent_error_fails_the_run() {
        let host = Arc::new(ScriptedHost::new());
        let (deps, bus) = rig(&host);
        let events = Arc::new(Mutex::new(Vec::new()));
        // The failure at seq 2 with no terminal at or past it.
        script_happy_turn(
            &host,
            &bus,
            &events,
            Some((2, "boom-code", "the model exploded")),
            false,
        );
        let mut io = capture_faces();
        // Through the run() face so the fail output and the exit request
        // pin together (the dsh `fail` twin).
        run(
            &["run the tests".to_owned()],
            &serde_json::json!({}),
            &deps,
            true,
            &mut io,
            || Ok(String::new()),
        );
        assert_eq!(captured(&io.out), "", "no answer text prints");
        assert_eq!(captured(&io.err), "fr: the model exploded\n");
        let exits: Vec<u32> = host
            .emitted()
            .iter()
            .filter(|(name, _)| name == "headless/exit")
            .map(|(_, payload)| {
                u32::try_from(payload["code"].as_u64().expect("code")).expect("code")
            })
            .collect();
        assert_eq!(exits, [1]);
    }

    #[test]
    fn cured_agent_error_completes_the_run() {
        let host = Arc::new(ScriptedHost::new());
        let (deps, bus) = rig(&host);
        let events = Arc::new(Mutex::new(Vec::new()));
        // The terminal at seq 3 (>= the failure offset) cures the tracker.
        script_happy_turn(
            &host,
            &bus,
            &events,
            Some((2, "boom-code", "the model exploded")),
            true,
        );
        let values = values_of(&["run the tests".to_owned()]);
        let mut io = capture_faces();
        let code = run_values(
            &deps,
            &values,
            &ModelSelection::default(),
            || Ok(String::new()),
            &mut io,
        )
        .expect("the cured failure completes");
        assert_eq!(code, DriveEnd::Code(0));
        assert_eq!(captured(&io.out), "all green\n");
    }

    #[test]
    fn resume_path_adopts_and_rechecks() {
        let host = Arc::new(ScriptedHost::new());
        let (deps, bus) = rig(&host);
        let events = Arc::new(Mutex::new(Vec::new()));
        empty_observation_script(&host, false);
        script_happy_turn(&host, &bus, &events, None, true);
        host.on(agent::SERVICE_NAME, agent::METHOD_RESUME, |payload| {
            let request: serde_json::Value =
                serde_json::from_slice(payload).map_err(|error| format!("decode: {error}"))?;
            let id = request["sessionId"]
                .as_str()
                .ok_or("no sessionId")?
                .to_owned();
            Ok(serde_json::to_vec(&serde_json::json!({ "sessionId": id })).expect("reply"))
        });
        let values = values_of(&[
            "--session-id".to_owned(),
            SESSION.to_owned(),
            "run the tests".to_owned(),
        ]);
        let mut io = capture_faces();
        let code = run_values(
            &deps,
            &values,
            &ModelSelection::default(),
            || Ok(String::new()),
            &mut io,
        )
        .expect("the adoption completes");
        assert_eq!(code, DriveEnd::Code(0));
    }

    #[test]
    fn missing_session_maps_to_the_omission_refusal() {
        let host = Arc::new(ScriptedHost::new());
        let (deps, _bus) = rig(&host);
        empty_observation_script(&host, false);
        host.on(
            session_query::SERVICE_NAME,
            session_query::METHOD_OBSERVE,
            |_| Err("SESSION_QUERY_SESSION_NOT_FOUND: session \"ghost\" not found".to_owned()),
        );
        let values = values_of(&[
            "--session-id".to_owned(),
            "ghost".to_owned(),
            "run the tests".to_owned(),
        ]);
        let mut io = capture_faces();
        let error = run_values(
            &deps,
            &values,
            &ModelSelection::default(),
            || Ok(String::new()),
            &mut io,
        )
        .expect_err("a missing session refuses");
        assert_eq!(
            error,
            "session \"ghost\" does not exist; omit --session-id to start a new Session"
        );
    }

    #[test]
    fn live_session_refuses_after_the_adoptability_rules() {
        let host = Arc::new(ScriptedHost::new());
        let (deps, _bus) = rig(&host);
        empty_observation_script(&host, true);
        let values = values_of(&[
            "--session-id".to_owned(),
            SESSION.to_owned(),
            "run the tests".to_owned(),
        ]);
        let mut io = capture_faces();
        let error = run_values(
            &deps,
            &values,
            &ModelSelection::default(),
            || Ok(String::new()),
            &mut io,
        )
        .expect_err("a live session refuses");
        assert_eq!(
            error,
            format!(
                "session \"{SESSION}\" is live in this process, so the one-shot runner cannot own an exclusive run interval"
            )
        );
    }

    #[test]
    fn preset_mismatch_is_named_before_the_live_refusal() {
        let host = Arc::new(ScriptedHost::new());
        let (deps, _bus) = rig(&host);
        host.on(agent::SERVICE_NAME, agent::METHOD_LIVE, |_| {
            Ok(br#"{"live": true}"#.to_vec())
        });
        host.on(
            session_query::SERVICE_NAME,
            session_query::METHOD_DESCRIBE,
            |_| Ok(br"{}".to_vec()),
        );
        host.on(
            persistence::SERVICE_NAME,
            persistence::METHOD_DESCRIBE,
            |_| Ok(br"{}".to_vec()),
        );
        host.on(
            session_query::SERVICE_NAME,
            session_query::METHOD_OBSERVE,
            |_| {
                Ok(serde_json::to_vec(&serde_json::json!({
                    "source": "live",
                    "header": header_with_cwd(Some(&current_cwd())),
                    "inheritedEventCount": 0,
                    "events": [serde_json::json!({
                        "type": "agent-preset/selected",
                        "seq": 0,
                        "time": 0,
                        "data": { "agentPreset": "compact" },
                    })],
                }))
                .expect("observe reply"))
            },
        );
        let values = values_of(&[
            "--session-id".to_owned(),
            SESSION.to_owned(),
            "run the tests".to_owned(),
        ]);
        let mut io = capture_faces();
        let error = run_values(
            &deps,
            &values,
            &ModelSelection::default(),
            || Ok(String::new()),
            &mut io,
        )
        .expect_err("a preset session refuses");
        assert!(
            error.contains("runs under agent preset \"compact\""),
            "{error}"
        );
    }

    #[test]
    fn persistence_absence_fails_the_double_check() {
        let host = Arc::new(ScriptedHost::new());
        let (deps, _bus) = rig(&host);
        host.on(
            session_query::SERVICE_NAME,
            session_query::METHOD_DESCRIBE,
            |_| Ok(br"{}".to_vec()),
        );
        let values = values_of(&[
            "--session-id".to_owned(),
            SESSION.to_owned(),
            "run the tests".to_owned(),
        ]);
        let mut io = capture_faces();
        let error = run_values(
            &deps,
            &values,
            &ModelSelection::default(),
            || Ok(String::new()),
            &mut io,
        )
        .expect_err("the persistence check refuses");
        assert_eq!(
            error,
            "headless --session-id requires the sessionPersistence service; the Session would not survive this process"
        );
    }

    #[test]
    fn blank_session_id_and_blank_task_fail_verbatim() {
        let host = Arc::new(ScriptedHost::new());
        let (deps, _bus) = rig(&host);
        // The runner-level checks guard the overlay path (the CLI parse
        // rejects blanks with its own usage face), so the rows arrive
        // constructed directly.
        let values = StartupValues {
            task: Some("run the tests".to_owned()),
            session_id: Some("   ".to_owned()),
            json: false,
        };
        let mut io = capture_faces();
        let error = run_values(
            &deps,
            &values,
            &ModelSelection::default(),
            || Ok(String::new()),
            &mut io,
        )
        .expect_err("blank identity");
        assert_eq!(error, "headless-runner: sessionId must not be blank");

        let values = StartupValues {
            task: Some("   ".to_owned()),
            session_id: None,
            json: false,
        };
        let mut io = capture_faces();
        let error = run_values(
            &deps,
            &values,
            &ModelSelection::default(),
            || Ok(String::new()),
            &mut io,
        )
        .expect_err("blank task");
        assert_eq!(
            error,
            "a task is required, for example: fr headless \"run the tests\""
        );
    }

    #[test]
    fn run_face_reports_help_usage_and_failures() {
        let host = Arc::new(ScriptedHost::new());
        let (deps, _bus) = rig(&host);
        let facts = serde_json::json!({});
        // Help.
        let mut io = capture_faces();
        run(&["--help".to_owned()], &facts, &deps, true, &mut io, || {
            Ok(String::new())
        });
        assert!(captured(&io.out).starts_with("Usage:"));
        assert_eq!(captured(&io.err), "");
        let exits: Vec<u32> = host
            .emitted()
            .iter()
            .filter(|(name, _)| name == "headless/exit")
            .map(|(_, payload)| {
                u32::try_from(payload["code"].as_u64().expect("code")).expect("code")
            })
            .collect();
        assert_eq!(exits.last(), Some(&0));
        // Usage error.
        let mut io = capture_faces();
        run(&[], &facts, &deps, true, &mut io, || Ok(String::new()));
        assert!(captured(&io.err).starts_with("error: "));
        // The blank task at the CLI layer surfaces the usage face (the
        // runner-level wording guards the overlay path, the blank test).
        let mut io = capture_faces();
        run(&["   ".to_owned()], &facts, &deps, true, &mut io, || {
            Ok(String::new())
        });
        assert_eq!(
            captured(&io.err),
            "error: a task is required, for example: fr headless \"run the tests\"\n"
        );
    }

    #[test]
    fn json_usage_error_also_emits_the_error_event() {
        let host = Arc::new(ScriptedHost::new());
        let (deps, _bus) = rig(&host);
        let mut io = capture_faces();
        run(
            &["--json".to_owned()],
            &serde_json::json!({}),
            &deps,
            true,
            &mut io,
            || Ok(String::new()),
        );
        let stream = captured(&io.out);
        let line: serde_json::Value = serde_json::from_str(stream.trim()).expect("json line");
        assert_eq!(line["type"], "error");
        assert!(captured(&io.err).starts_with("error: "));
    }

    #[test]
    fn summarize_takes_the_last_nonempty_text_and_the_final_reason() {
        let events = vec![
            event(0, "turn/start", &serde_json::json!({ "turn": 1 })),
            event(
                1,
                "assistant/message",
                &serde_json::json!({
                    "turn": 1, "step": 1,
                    "message": { "role": "assistant", "id": "a", "content": [], "source": { "kind": "model", "provider": "p", "model": "m" } }, "stream": [],
                }),
            ),
            event(
                2,
                "assistant/message",
                &serde_json::json!({
                    "turn": 1, "step": 2,
                    "message": { "role": "assistant", "id": "b", "content": [
                        { "type": "text", "text": "first " },
                        { "type": "text", "text": "answer" },
                    ], "source": { "kind": "model", "provider": "p", "model": "m" } }, "stream": [],
                }),
            ),
            event(
                3,
                "turn/end",
                &serde_json::json!({ "turn": 1, "reason": { "kind": "completed" } }),
            ),
        ];
        let outcome = summarize(&events, 0, 4).expect("the slice is intact");
        assert_eq!(outcome.text, "first answer");
        assert!(matches!(
            outcome.reason,
            Some(fr_wire::event::TurnEndReason::Known(
                TurnEndReasonKind::Completed
            ))
        ));
        // A later turn's non-empty text wins.
        let events = vec![
            event(0, "turn/start", &serde_json::json!({ "turn": 1 })),
            event(
                1,
                "assistant/message",
                &serde_json::json!({
                    "turn": 1, "step": 1,
                    "message": { "role": "assistant", "id": "a", "content": [{ "type": "text", "text": "stale" }], "source": { "kind": "model", "provider": "p", "model": "m" } }, "stream": [],
                }),
            ),
            event(
                2,
                "turn/end",
                &serde_json::json!({ "turn": 1, "reason": { "kind": "completed" } }),
            ),
            event(3, "turn/start", &serde_json::json!({ "turn": 2 })),
            event(
                4,
                "assistant/message",
                &serde_json::json!({
                    "turn": 2, "step": 1,
                    "message": { "role": "assistant", "id": "c", "content": [{ "type": "text", "text": "fresh" }], "source": { "kind": "model", "provider": "p", "model": "m" } }, "stream": [],
                }),
            ),
            event(
                5,
                "turn/end",
                &serde_json::json!({ "turn": 2, "reason": { "kind": "completed" } }),
            ),
        ];
        let outcome = summarize(&events, 0, 6).expect("the slice is intact");
        assert_eq!(outcome.text, "fresh");
    }

    #[test]
    fn stdin_task_flows_when_the_argument_is_the_bare_hyphen() {
        let host = Arc::new(ScriptedHost::new());
        let (deps, bus) = rig(&host);
        let events = Arc::new(Mutex::new(Vec::new()));
        script_happy_turn(&host, &bus, &events, None, true);
        // The bare `-` hands the task to stdin: the runner-level followup
        // carries the piped text (the arm asserts the exact task).
        let stdin_bus = Arc::clone(&bus);
        let stdin_events = Arc::clone(&events);
        host.on(
            agent::SERVICE_NAME,
            agent::METHOD_FOLLOWUP,
            move |payload| {
                followup_arm(
                    &stdin_bus,
                    &stdin_events,
                    payload,
                    None,
                    true,
                    "the piped task",
                )
            },
        );
        let values = values_of(&["-".to_owned()]);
        assert_eq!(values.task.as_deref(), Some("-"), "the bare hyphen");
        let mut io = capture_faces();
        let code = run_values(
            &deps,
            &values,
            &ModelSelection::default(),
            || Ok("the piped task".to_owned()),
            &mut io,
        )
        .expect("the piped run completes");
        assert_eq!(code, DriveEnd::Code(0));
    }

    #[test]
    fn given_task_skips_the_stdin_read() {
        let host = Arc::new(ScriptedHost::new());
        let (deps, bus) = rig(&host);
        let events = Arc::new(Mutex::new(Vec::new()));
        script_happy_turn(&host, &bus, &events, None, true);
        let values = values_of(&["run the tests".to_owned()]);
        let mut io = capture_faces();
        // The reader face fails the run if it is ever touched: the run
        // completing proves the argument short-circuited stdin.
        let code = run_values(
            &deps,
            &values,
            &ModelSelection::default(),
            || Err("stdin must not be read".to_owned()),
            &mut io,
        )
        .expect("the argument run completes");
        assert_eq!(code, DriveEnd::Code(0));
    }

    #[test]
    fn uncured_json_run_emits_the_error_event_without_final() {
        let host = Arc::new(ScriptedHost::new());
        let (deps, bus) = rig(&host);
        let events = Arc::new(Mutex::new(Vec::new()));
        script_happy_turn(
            &host,
            &bus,
            &events,
            Some((2, "boom-code", "the model exploded")),
            false,
        );
        let mut io = capture_faces();
        // Through the run() face: the fail face owns the error event line.
        run(
            &["--json".to_owned(), "run the tests".to_owned()],
            &serde_json::json!({}),
            &deps,
            true,
            &mut io,
            || Ok(String::new()),
        );
        // The fail face: the error event line, never a final frame.
        let stream = captured(&io.out);
        let lines: Vec<serde_json::Value> = stream
            .lines()
            .map(|line| serde_json::from_str(line).expect("json line"))
            .collect();
        let kinds: Vec<&str> = lines
            .iter()
            .filter_map(|frame| frame["type"].as_str())
            .collect();
        assert!(kinds.contains(&"error"), "the error event prints: {stream}");
        assert!(!kinds.contains(&"final"), "no final frames: {stream}");
    }

    #[test]
    fn bus_decoders_pin_the_four_payload_shapes() {
        let status = decode_status(br#"{"sessionId":"s","status":"running"}"#).expect("status");
        let BusEvent::Status { session_id, status } = status else {
            panic!("status shape");
        };
        assert_eq!(session_id, "s");
        assert_eq!(status, fr_agent_loop::phase::AgentStatus::Running);
        let failure = decode_failure(
            br#"{"sessionId":"s","turn":1,"step":0,"error":{"message":"m","code":"c"},"seq":5}"#,
        )
        .expect("failure");
        let BusEvent::Failure { seq, error, .. } = failure else {
            panic!("failure shape");
        };
        assert_eq!(seq, 5);
        assert_eq!(error.message, "m");
        let frames = decode_frames(
            br#"{"frames":[{"type":"start","attemptId":"att-1","revision":1,"turn":1,"step":1}]}"#,
        )
        .expect("frames");
        assert!(matches!(frames, BusEvent::Frames(_)));
        let session_event = decode_session_event(
            br#"{"sessionId":"s","event":{"type":"turn/start","seq":0,"time":0,"data":{"turn":1}}}"#,
        )
        .expect("session event");
        let BusEvent::SessionEvent { session_id, event } = session_event else {
            panic!("session event shape");
        };
        assert_eq!(session_id, "s");
        assert_eq!(event.seq.get(), 0);
    }
}
