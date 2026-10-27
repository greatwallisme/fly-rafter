// Rust guideline compliant 2026-10-27
//! The loop driver body (dsh `agent.ts` `ReactLoopAgent` port).
//!
//! One [`LoopDriver`] owns one session's turn/step control flow. The
//! public entry (`send` and friends, dsh's four entries plus `cancel`)
//! arrive as queued mailbox commands applied by the driver itself — the
//! durable application points (inbox splice, abort latch) live loop-side
//! under Q79-A, unlike dsh's synchronous caller-thread mutations.
//!
//! Every dsh `throwIfAborted` site becomes one `LoopDriver::poll` mailbox
//! poll: drain and apply commands, fold an observed shutdown into a
//! disposed cancel, then surface a latched cause as
//! [`DriverError::Aborted`]. LLM streams consume through the Q84-B2
//! generation face (`begin`, notification wake, non-blocking `drain`);
//! dispatched tool calls settle the same way (decision 46's slow-work
//! invariant applied to the tools face). The seams themselves
//! (`SessionReads`, `SystemPromptFace`, `LlmFace`, `ToolsFace`) are the
//! Q81-A loop-side consumer wrappers, driven in tests by doubles until
//! the real services attach in Phases 14/15/18.
//!
//! Structurally absent dsh faces, each documented where it would have
//! appeared: `deepFreeze`/`WeakSet` request freezing (Rust value
//! semantics already make the derived request immutable),
//! `activityDone`/`whenIdle` awaiting (decision 46 rebuilds it as the
//! `agent/status` event plus the non-blocking status query), and the
//! initiator scope (initiator attribution travels explicitly in
//! payloads).

use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fr_llm::adapter::{GenerateOptions, LlmCallConfig, RequestMessage, ResolvedModelInfo};
use fr_llm::error::LlmError;
use fr_llm::retry_policy::RetryPolicy;
use fr_session::repair::{OpenTurnCloseCause, ToolCallRecovery};
use fr_session::session::canonical_header;
use fr_wire::call_config::{
    AdapterDefaults, CallConfig, CallControls, FinishReason, FinishReasonKind, SystemPromptUpdate,
    ToolHistory, ToolSchema,
};
use fr_wire::event::{
    AppendOp, AssistantAttemptData, AssistantMessageData, CancelCause, DeveloperMessageData,
    EpochHeader, RequestContextData, RequestContextHolder, RequestHeaderData, RequestHeaderReason,
    SessionEvent, SessionEventData, StepBoundsData, SurfaceOp, SystemMessageData, ToolCallData,
    ToolResultData, ToolResultError, TurnEndData, TurnEndReason, TurnEndReasonKind, TurnStartData,
    event_type,
};
use fr_wire::ids::{MessageId, SessionId, ToolCallId};
use fr_wire::message::{
    ContextSnapshotSection, KnownMessage, KnownMessageSource, Message, MessageSource,
};
use fr_wire::stream::StreamChunk;
use fr_wire::{ContentBlock, KnownContentBlock, RawObject, SessionSeq, TrueOnly};

use crate::agent_events::{
    AgentEvents, AssistantStreamPayload, ErrorPayload, PreStepDecision, PreStepPayload,
    RequestErrorAction, RequestErrorPayload, RequestPayload, TurnStoppingPayload,
};
use crate::assistant_stream::{
    AssistantStreamAttempt, FrameSink, RevisionAllocator, SettlementType,
};
use crate::inbox::InboxTarget;
use crate::loop_inbox::{InboxError, LoopInbox};
use crate::phase::{AbortLatch, Phase, PhaseMachine};
use crate::plugin_shell::{InboundCommand, SessionMailbox};
use crate::runtime_context::{
    RuntimeContextProjection, SystemPromptDecisionInput, SystemPromptNode, project_system_prompt,
};
use crate::session_client::{SessionClient, SessionClientError};
use crate::turn_boundary::fold_turn_boundary;

/// The loop's declared agent route (dsh `AgentOptions`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentOptions {
    /// The provider route key; empty means "not declared".
    pub provider: Option<String>,
    /// The model id; empty means "not declared".
    pub model: Option<String>,
    /// The adapter-owned reasoning effort for the exact model.
    pub reasoning_effort: Option<fr_wire::ReasoningEffortId>,
    /// The per-request output cap.
    pub max_tokens: Option<u64>,
}

/// The restored log the driver folds at construction (dsh reads the
/// projections' persisted state through the session service).
#[derive(Debug)]
pub struct RestoreView {
    /// The full event log, oldest first.
    pub events: Vec<SessionEvent>,
    /// The seqs currently on the ordered surface.
    pub surface: HashSet<SessionSeq>,
}

/// The loop-side read face over the session service (Q81-A): the wire
/// shapes the future service extension serves.
///
/// Every read crosses the plugin boundary because the session service
/// owns the authoritative folds — a loop-local fold would miss other
/// writers (compaction replacements in particular).
pub trait SessionReads: Send {
    /// The committed log and surface for restore folding.
    fn restore(&mut self) -> RestoreView;
    /// The `request/header` fold in force (dsh `session.requestHeader()`).
    fn request_header(&mut self) -> Option<EpochHeader>;
    /// The last `request/context` data (dsh `session.requestContext()`).
    fn request_context(&mut self) -> Option<RequestContextData>;
    /// The surface message projection (dsh `session.deriveMessages()`).
    fn derive_messages(&mut self) -> Vec<Message>;
    /// The session-folded tool declaration history.
    fn tool_history(&mut self) -> ToolHistory;
    /// The surface content generation counter.
    fn surface_generation(&mut self) -> u64;
    /// The `system/message` surface nodes in surface order, as the
    /// system-prompt projection reads them.
    fn system_prompt_nodes(&mut self) -> Vec<SystemPromptNode>;
}

/// One system-prompt assembly (dsh `PromptAssembly` reduced to the faces
/// the driver consumes: the rendered prompt, the context sections with
/// their joined text, and the assembled tool schemas).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptAssembly {
    /// The rendered prompt (dsh `renderPrompt(assembly)`), stable for the
    /// step's whole retry loop.
    pub rendered_prompt: String,
    /// The rendered context sections (dsh `renderContextSections`).
    pub context_sections: Vec<ContextSnapshotSection>,
    /// The joined context text (dsh `joinContextSections(sections)`).
    pub joined_sections: String,
    /// The assembled tool schemas.
    pub tools: Vec<ToolSchema>,
}

/// The loop-side system-prompt face (Q81-A; the real crate is Phase 15).
pub trait SystemPromptFace: Send {
    /// Assembles the prompt for one step boundary (dsh
    /// `systemPrompt.assemble`).
    ///
    /// # Errors
    ///
    /// Returns the assembly failure text.
    fn assemble(&mut self) -> Result<PromptAssembly, String>;
    /// Re-reads the volatile context sections for one attempt (dsh
    /// `systemPrompt.refreshContext`).
    ///
    /// # Errors
    ///
    /// Returns the refresh failure text.
    fn refresh_context(&mut self, assembly: &PromptAssembly) -> Result<PromptAssembly, String>;
}

/// The driver-visible facts of one prepared call (dsh `PreparedLlmCall`
/// minus the stream closure, which the B2 face carries by generation id).
#[derive(Debug, Clone)]
pub struct PreparedCall {
    /// The frozen config this generation was prepared with.
    pub config: LlmCallConfig,
    /// The exact-route model metadata (context window, prompt and tool
    /// update handling).
    pub model: ResolvedModelInfo,
    /// The adapter-materialized default flags, when the route supplied any.
    pub adapter_defaults: Option<AdapterDefaults>,
    /// The provider-owned retry policy bound to the route.
    pub retry_policy: Option<RetryPolicy>,
}

/// One drained batch of a generation's FIFO buffer (Q84-B2).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DrainedGeneration {
    /// The buffered chunks, in delivery order.
    pub chunks: Vec<StreamChunk>,
    /// Whether the terminal settlement was delivered (the stream ended).
    pub done: bool,
}

/// The loop-side LLM face (Q84-B2): `prepare_call` mirrors dsh
/// `llm.prepareCall`; the generation trio is the notification-and-pull
/// stream consumption.
///
/// Every method is a fast-path memory operation — the network flow
/// lives on the provider's worker threads.
pub trait LlmFace: Send {
    /// Resolves the route and binds one adapter generation (dsh
    /// `llm.prepareCall`).
    ///
    /// # Errors
    ///
    /// Returns the [`LlmError`] the runtime raised (including
    /// `NO_ADAPTER` for unregistered routes).
    fn prepare_call(&mut self, config: &LlmCallConfig) -> Result<PreparedCall, LlmError>;
    /// Starts one generation and returns its id; chunk-ready
    /// notifications for that id wake the driver (dsh
    /// `preparedCall.stream(request) ?? llm.stream(request)`).
    ///
    /// # Errors
    ///
    /// Returns the generation-start failure.
    fn begin(
        &mut self,
        request: &GenerateOptions,
        prepared: Option<&PreparedCall>,
    ) -> Result<String, LlmError>;
    /// Non-blockingly pulls one batch from the generation's buffer.
    ///
    /// # Errors
    ///
    /// Returns the drain failure.
    fn drain(&mut self, generation: &str) -> Result<DrainedGeneration, LlmError>;
    /// Flags the generation cancelled (best effort; the worker settles
    /// asynchronously per the dsh caller-abort semantics).
    ///
    /// # Errors
    ///
    /// Returns the abort-flag failure.
    fn abort(&mut self, generation: &str) -> Result<(), LlmError>;
}

/// One scheduled tool call as the tools face sees it (dsh
/// `ToolExecutionInput`).
///
/// The initiator travels explicitly in payloads under decision 46, and
/// the abort reach is the settle/abort face of the real tools fabric.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolExecution {
    /// The provider-issued call id.
    pub call_id: ToolCallId,
    /// The requested tool name.
    pub name: String,
    /// Parsed arguments: an object, or the raw text when it was not
    /// valid JSON (dsh `parseArguments`).
    pub arguments: serde_json::Value,
}

/// The live concurrency mode of one tool (dsh `executionMode`): parallel
/// calls share a bounded pool, everything else is an exclusive barrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExecutionMode {
    /// Safe to overlap with other parallel calls.
    Parallel,
    /// Runs alone; a single-call barrier.
    Exclusive,
}

/// One committed tool result (dsh `ToolExecutionResult`).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolExecutionResult {
    /// The model-facing result content.
    pub content: Vec<ContentBlock>,
    /// Whether the invocation failed.
    pub is_error: bool,
    /// The failure identity recorded outside the model content.
    pub error: Option<ToolResultError>,
    /// Tool-private presentation payload for replay.
    pub meta: Option<serde_json::Value>,
    /// Context messages accepted into the next step boundary's inbox.
    pub additional_contexts: Vec<Message>,
    /// Whether this result concludes the turn.
    pub concludes_turn: bool,
}

/// One prepared dispatch path (dsh the `prepare` ruling).
#[derive(Debug, Clone, PartialEq)]
pub enum ToolPrepared {
    /// Run through the scheduler; the outcome settles asynchronously.
    Dispatch {
        /// The (possibly transformed) execution input.
        exec: ToolExecution,
    },
    /// Settled at prepare; requires finalize before commit.
    PostResult {
        /// The execution input.
        exec: ToolExecution,
        /// The prepared result.
        result: ToolExecutionResult,
    },
    /// Settled at prepare; commits directly.
    FinalResult {
        /// The execution input.
        exec: ToolExecution,
        /// The prepared result.
        result: ToolExecutionResult,
    },
}

/// One settled dispatch (dsh the dispatch outcome).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSettlement {
    /// The dispatch's result.
    pub result: ToolExecutionResult,
    /// Whether the result still requires finalize (dsh
    /// `outcome.kind === 'post-result'`).
    pub needs_post: bool,
}

/// The loop-side tools face (Q81-A; the interface shape pinned here for
/// Phase 14): fast-path enqueue plus settle polling, mirroring the B2
/// pattern the decision-46 slow-work invariant prescribes.
pub trait ToolsFace: Send {
    /// The live concurrency mode of one call.
    fn execution_mode(&mut self, exec: &ToolExecution) -> ToolExecutionMode;
    /// Prepares one call (dsh `TOOL_RUNTIME_SCHEDULER.prepare`).
    ///
    /// # Errors
    ///
    /// Returns the scheduler failure text.
    fn prepare(&mut self, exec: &ToolExecution) -> Result<ToolPrepared, String>;
    /// Enqueues one dispatch; the outcome arrives later through
    /// [`ToolsFace::settled`] after a tool-settle notification wakes the
    /// driver.
    ///
    /// # Errors
    ///
    /// Returns the scheduler failure text.
    fn dispatch(&mut self, exec: &ToolExecution) -> Result<(), String>;
    /// Takes the settlements that have arrived, in settle order.
    fn settled(&mut self) -> Vec<(ToolCallId, ToolSettlement)>;
    /// Finalizes one post-result settlement (dsh
    /// `TOOL_RUNTIME_SCHEDULER.finalize`).
    ///
    /// # Errors
    ///
    /// Returns the scheduler failure text.
    fn finalize(
        &mut self,
        exec: &ToolExecution,
        result: ToolExecutionResult,
    ) -> Result<ToolExecutionResult, String>;
    /// Synchronously finishes one settlement (dsh
    /// `TOOL_RUNTIME_SCHEDULER.finish`).
    fn finish(&mut self, exec: &ToolExecution, result: ToolExecutionResult) -> ToolExecutionResult;
}

/// One failure traveling through the driver's control flow.
#[derive(Debug)]
pub enum DriverError {
    /// A cancel latched at a poll point (the dsh abort throw).
    Aborted(CancelCause),
    /// A durable append failed.
    Session(SessionClientError),
    /// An inbox mutation failed.
    Inbox(InboxError),
    /// A structured LLM failure.
    Llm(LlmError),
    /// A system-prompt seam failure.
    Prompt(String),
    /// A tools seam failure.
    Tool(String),
    /// A turn-stopping listener failed (dsh serial rejection).
    Listener(Box<dyn Error + Send + Sync>),
    /// A stream settlement's durable append failed after the stream
    /// itself failed (dsh `AggregateError` 'Assistant stream failed and
    /// its durable settlement was rejected').
    Settlement {
        /// The failure that ended the stream.
        source: Box<Self>,
        /// The failure that rejected the settlement.
        settlement: Box<Self>,
    },
    /// A step failed and its recovery results could not be recorded (dsh
    /// `AggregateError` 'Step failed and its pending tool results could
    /// not be recorded').
    StepRecovery {
        /// The step failure.
        step: Box<Self>,
        /// The recovery-append failure.
        recovery: Box<Self>,
    },
    /// Any other failure (verbatim dsh error texts live here).
    Text(String),
}

impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Aborted(cause) => write!(f, "agent loop cancelled: {cause:?}"),
            Self::Session(error) => write!(f, "{error}"),
            Self::Inbox(error) => write!(f, "{error}"),
            Self::Llm(error) => write!(f, "{error}"),
            Self::Prompt(text) | Self::Tool(text) | Self::Text(text) => f.write_str(text),
            Self::Listener(error) => write!(f, "{error}"),
            Self::Settlement { .. } => {
                f.write_str("Assistant stream failed and its durable settlement was rejected")
            }
            Self::StepRecovery { .. } => {
                f.write_str("Step failed and its pending tool results could not be recorded")
            }
        }
    }
}

impl Error for DriverError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Session(error) => Some(error),
            Self::Inbox(error) => Some(error),
            Self::Llm(error) => Some(error),
            Self::Listener(error) => Some(error.as_ref()),
            Self::Settlement { source, .. } | Self::StepRecovery { step: source, .. } => {
                Some(source.as_ref())
            }
            Self::Aborted(_) | Self::Prompt(_) | Self::Tool(_) | Self::Text(_) => None,
        }
    }
}

impl From<SessionClientError> for DriverError {
    fn from(error: SessionClientError) -> Self {
        Self::Session(error)
    }
}

impl From<InboxError> for DriverError {
    fn from(error: InboxError) -> Self {
        Self::Inbox(error)
    }
}

/// Rebuilds the [`LlmError`] a failed finish throws (dsh `new
/// LlmError(message, code, failure)`): the failure already passed the
/// constructor's validation when the adapter produced it.
fn llm_error(failure: &fr_wire::Failure) -> LlmError {
    LlmError::try_new(
        &failure.message,
        &failure.code,
        failure.status,
        failure.provider_retry_after_ms,
        failure
            .request_id
            .as_ref()
            .map(fr_wire::ProviderRequestId::as_str),
        failure.offload_images,
    )
    .expect("a stream failure re-passes the constructor validation it was born under")
}

/// Joins one error's display chain with `": "` (dsh `errorChain`).
fn error_chain(error: &dyn Error) -> String {
    let mut parts = vec![error.to_string()];
    let mut source = error.source();
    while let Some(error) = source {
        parts.push(error.to_string());
        source = error.source();
    }
    parts.join(": ")
}

/// The structured turn-end failure for one driver error (dsh turn catch:
/// an `LlmError` keeps its facts, anything else flattens under `UNKNOWN`).
fn turn_failure(error: &DriverError) -> fr_wire::Failure {
    match error {
        DriverError::Llm(llm) => llm.failure().clone(),
        other => fr_wire::Failure {
            message: error_chain(other),
            code: "UNKNOWN".to_owned(),
            status: None,
            provider_retry_after_ms: None,
            request_id: None,
            offload_images: None,
        },
    }
}

/// The step-end reasons a step can return (dsh `StepEndReason`).
type StepEndReason = TurnEndReasonKind;

/// One pre-step ruling with its assembly (dsh `PreparedStep`).
enum PreparedStep {
    /// The input was refused; the turn ends blocked.
    Reject,
    /// The step runs with this batch.
    Enter(EnterStep),
}

/// One attempt's admission decision inputs (dsh the system-prompt
/// project and context-admission parameters).
struct AdmissionInputs<'a> {
    rendered_prompt: &'a str,
    context: Option<&'a Message>,
    in_history: bool,
    starts_series: bool,
    first_attempt: bool,
    turn: u64,
    step: u64,
}

/// The enter ruling's payload.
struct EnterStep {
    messages: Vec<Message>,
    starts_request_series: bool,
    assembly: PromptAssembly,
}

/// How one `turn()` invocation ended for the outer loop (dsh `return
/// false` inside the try versus reaching the epilogue after a break).
enum TurnFlow {
    /// An early `return false`: the epilogue is skipped.
    EarlyFalse,
    /// The while loop broke: the epilogue decides whether to continue.
    Epilogue,
}

/// One scheduler group outcome (dsh `GroupOutcome`).
struct GroupOutcome {
    consumed: usize,
    aborted: bool,
    concluded: bool,
}

/// One settled slot awaiting model-order finalization (dsh `Slot`).
struct ToolSlot {
    exec: ToolExecution,
    result: ToolExecutionResult,
    needs_post: bool,
}

/// The mutable scheduler state of one group (dsh runGroup locals).
struct GroupState {
    slots: Vec<Option<ToolSlot>>,
    call_seqs: Vec<Option<SessionSeq>>,
    in_flight: Vec<usize>,
    next_to_start: usize,
    committed: usize,
    started: usize,
    aborted: bool,
    concluded: bool,
}

/// One planned tool call (dsh `PlannedCall`).
struct PlannedCall {
    id: ToolCallId,
    name: String,
    raw_arguments: String,
}

impl PlannedCall {
    fn exec(&self) -> ToolExecution {
        ToolExecution {
            call_id: self.id.clone(),
            name: self.name.clone(),
            arguments: parse_arguments(&self.raw_arguments),
        }
    }
}

/// Parses model arguments, preserving invalid JSON as text and mapping
/// empty input to `{}` (dsh `parseArguments`).
fn parse_arguments(raw: &str) -> serde_json::Value {
    if raw.is_empty() {
        return serde_json::json!({});
    }
    serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_owned()))
}

/// Removes adapter-derived values before plugins propose the next
/// request config (dsh `requestProposal`).
fn request_proposal(header: &EpochHeader) -> LlmCallConfig {
    let mut config = from_wire_config(&header.config);
    if let Some(defaults) = &header.adapter_defaults {
        if defaults.reasoning_effort.is_some() {
            config.controls.reasoning_effort = None;
        }
        if defaults.max_tokens.is_some() {
            config.controls.max_tokens = None;
        }
    }
    config
}

fn to_wire_config(config: &LlmCallConfig) -> CallConfig {
    CallConfig {
        provider: config.provider.clone(),
        model: config.model.clone(),
        controls: config.controls.clone(),
    }
}

fn from_wire_config(config: &CallConfig) -> LlmCallConfig {
    LlmCallConfig {
        provider: config.provider.clone(),
        model: config.model.clone(),
        controls: config.controls.clone(),
        agent_loop: false,
    }
}

/// The stable machine-routing code for a call cancelled before dispatch
/// (dsh-tools `TOOL_ABORTED_BEFORE_DISPATCH`).
pub const TOOL_ABORTED_BEFORE_DISPATCH: &str = "TOOL_ABORTED_BEFORE_DISPATCH";

/// The injected seams and clocks one driver consumes (Q81-A doubles in
/// tests; the real services attach in Phases 14/15/18).
pub struct DriverDeps {
    /// The session read face.
    pub reads: Box<dyn SessionReads>,
    /// The system-prompt face.
    pub prompt: Box<dyn SystemPromptFace>,
    /// The LLM generation face.
    pub llm: Box<dyn LlmFace>,
    /// The tools scheduling face.
    pub tools: Box<dyn ToolsFace>,
    /// The wall clock for stream chunk timestamps.
    pub clock: Box<dyn Fn() -> i64 + Send>,
    /// The stable identity source for created messages.
    pub next_id: Box<dyn Fn() -> MessageId + Send>,
    /// The volatile parallel-tool-call pool window (read per group).
    pub max_parallel_tool_calls: Box<dyn Fn() -> usize + Send>,
}

impl fmt::Debug for DriverDeps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The injected seams are not debug-printable.
        f.debug_struct("DriverDeps").finish_non_exhaustive()
    }
}

/// The one-session loop driver. Construct through [`LoopDriver::new`]
/// (which restores the durable state), then hand it to a thread running
/// [`LoopDriver::run`].
pub struct LoopDriver {
    session_id: SessionId,
    options: AgentOptions,
    mailbox: Arc<SessionMailbox>,
    machine: PhaseMachine,
    events: Arc<AgentEvents>,
    client: SessionClient,
    inbox: LoopInbox,
    runtime_context: RuntimeContextProjection,
    reads: Box<dyn SessionReads>,
    prompt: Box<dyn SystemPromptFace>,
    llm: Box<dyn LlmFace>,
    tools: Box<dyn ToolsFace>,
    clock: Box<dyn Fn() -> i64 + Send>,
    next_id: Box<dyn Fn() -> MessageId + Send>,
    max_parallel_tool_calls: Box<dyn Fn() -> usize + Send>,
    request_header_logged: bool,
    request_surface_generation: u64,
    assistant_attempt_counter: u64,
    assistant_stream_revision: Arc<AtomicU64>,
    /// The step-scoped tool recovery (dsh registers a `session/event`
    /// observer around each step; the driver observes its own committed
    /// appends at the same boundary).
    step_recovery: Option<ToolCallRecovery>,
    /// Whether a woken driver must run its kick (the in-thread stand-in
    /// for dsh `wakeDriver`'s async spawn).
    kick_pending: bool,
}

impl fmt::Debug for LoopDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The injected seams are not debug-printable; the identity and
        // request faces carry the useful state.
        f.debug_struct("LoopDriver")
            .field("session_id", &self.session_id)
            .field("options", &self.options)
            .field("machine", &self.machine)
            .field("request_header_logged", &self.request_header_logged)
            .field(
                "request_surface_generation",
                &self.request_surface_generation,
            )
            .field("assistant_attempt_counter", &self.assistant_attempt_counter)
            .field("kick_pending", &self.kick_pending)
            .finish_non_exhaustive()
    }
}

impl LoopDriver {
    /// Restores the driver over the session's committed log: turn
    /// boundary, inbox, and runtime-context retention all fold from the
    /// same events the live mutations write.
    ///
    /// # Errors
    ///
    /// Returns the first restore fold failure.
    pub fn new(
        session_id: &str,
        options: AgentOptions,
        mailbox: Arc<SessionMailbox>,
        events: Arc<AgentEvents>,
        mut deps: DriverDeps,
    ) -> Result<Self, DriverError> {
        let view = deps.reads.restore();
        let mut boundary = crate::turn_boundary::init();
        for event in &view.events {
            boundary = fold_turn_boundary(boundary, event);
        }
        let session_id = SessionId::new(session_id.to_owned());
        let client = SessionClient::new(mailbox.outbound(), session_id.as_str());
        let inbox = LoopInbox::restored(client.clone(), Arc::clone(&events), &view.events)?;
        let runtime_context =
            RuntimeContextProjection::restore(view.events.iter().rev(), &view.surface);
        let request_surface_generation = deps.reads.surface_generation();
        let machine = PhaseMachine::new(
            Phase::Idle {
                last_turn: boundary.last_turn,
            },
            Arc::clone(&events),
            mailbox.status_cell(),
        );
        let DriverDeps {
            reads,
            prompt,
            llm,
            tools,
            clock,
            next_id,
            max_parallel_tool_calls,
        } = deps;
        Ok(Self {
            session_id,
            options,
            mailbox,
            machine,
            events,
            client,
            inbox,
            runtime_context,
            reads,
            prompt,
            llm,
            tools,
            clock,
            next_id,
            max_parallel_tool_calls,
            request_header_logged: false,
            request_surface_generation,
            assistant_attempt_counter: 0,
            assistant_stream_revision: Arc::new(AtomicU64::new(0)),
            step_recovery: None,
            kick_pending: false,
        })
    }

    /// The drive loop (the thread body): park, apply queued commands,
    /// and run woken kicks until shutdown. A kick's convergence can
    /// re-request a wake, so the inner loop drains the flag before
    /// parking again.
    ///
    /// # Panics
    ///
    /// Panics if a mailbox lock is poisoned (a holder panicked).
    pub fn run(&mut self) {
        loop {
            self.mailbox.wait_idle();
            if self.mailbox.is_shutdown() {
                return;
            }
            // An idle driver consumes no notifications; dropping them here
            // keeps the queue bounded (Q84-B2 loss is harmless).
            drop(self.mailbox.drain_notifications());
            self.apply_commands();
            while self.kick_pending {
                self.kick_pending = false;
                self.kick();
                if self.mailbox.is_shutdown() {
                    return;
                }
            }
        }
    }

    /// Runs one structured job under the maintenance phase (dsh
    /// `runMaintenance`): new steps refuse while it runs, and a wake
    /// latched during the job replays once it converges. The job runs on
    /// the driver's own thread, so a cancel queued mid-job lands at its
    /// completion — the dsh synchronous signal face is structurally gone
    /// (Q79-A) and the latch the job observes stays live for future
    /// arms.
    ///
    /// # Errors
    ///
    /// Returns the job's failure, or the active-work rejection when the
    /// loop is not idle.
    pub fn run_maintenance<T>(
        &mut self,
        job: impl FnOnce(&AbortLatch) -> Result<T, DriverError>,
    ) -> Result<T, DriverError> {
        if !matches!(self.machine.phase(), Phase::Idle { .. }) {
            return Err(DriverError::Text(format!(
                "agent \"{}\" already has active work",
                self.session_id
            )));
        }
        let Phase::Idle { last_turn } = *self.machine.phase() else {
            unreachable!("the idle check above holds");
        };
        self.machine.commit(Phase::Maintenance {
            abort: AbortLatch::new(),
            last_turn,
            wake_requested: false,
        });
        // The entry doubles as a command application point: a wake queued
        // before the job latches behind the maintenance phase exactly as
        // dsh's synchronous send would.
        self.apply_commands();
        let result = {
            let Phase::Maintenance { abort, .. } = self.machine.phase_mut() else {
                unreachable!("committed above");
            };
            job(abort)
        };
        let (cause, wake_requested, last_turn) = match self.machine.phase() {
            Phase::Maintenance {
                abort,
                last_turn,
                wake_requested,
            } => (abort.aborted_cause().cloned(), *wake_requested, *last_turn),
            _ => unreachable!("the job cannot leave the maintenance phase"),
        };
        self.machine.commit(Phase::Idle { last_turn });
        let disposed = matches!(cause, Some(CancelCause::Disposed));
        if !disposed && wake_requested && self.inbox.has_pending() {
            self.wake_driver(false);
        }
        result
    }

    /// Applies every queued inbound command in enqueue order. A failed
    /// durable application reports through the loop's failure face: dsh
    /// surfaces these throws to the synchronous caller, which the
    /// queued-application architecture structurally cannot (the entry
    /// already returned "queued").
    fn apply_commands(&mut self) {
        for command in self.mailbox.drain_commands() {
            let result = match command {
                InboundCommand::Input {
                    target,
                    message,
                    wakeup,
                } => self.send(target, message, wakeup),
                InboundCommand::Cancel { cause, keep_inbox } => self.cancel(cause, keep_inbox),
            };
            if let Err(error) = result {
                drop(self.throw_error(error));
            }
        }
    }

    /// Queues one input (dsh `send`): waking input that cannot join an
    /// aborted activity starts the next turn instead — classified
    /// before the insertion so a reentrant cancel from a splice observer
    /// cannot reclassify it.
    fn send(
        &mut self,
        target: InboxTarget,
        message: Message,
        wakeup: bool,
    ) -> Result<(), DriverError> {
        let waking_after_abort = wakeup && self.non_idle_aborted();
        let resolved = if waking_after_abort {
            InboxTarget::NextTurn
        } else {
            target
        };
        self.inbox
            .splice(resolved, i64::MAX, 0, vec![message])
            .map_err(DriverError::Inbox)?;
        if wakeup {
            self.wake_driver(waking_after_abort);
        }
        Ok(())
    }

    /// Requests cancellation (dsh `cancel`): without `keep_inbox` the
    /// pending input is durably cleared and a latched wake forgotten;
    /// a non-idle loop's latch takes the cause (first cause wins).
    fn cancel(&mut self, cause: CancelCause, keep_inbox: bool) -> Result<(), DriverError> {
        if !keep_inbox {
            self.inbox.clear().map_err(DriverError::Inbox)?;
            self.set_wake_requested(false);
        }
        self.arm_latch(cause);
        Ok(())
    }

    /// Starts one driver, or latches its wake behind maintenance or an
    /// aborted activity (dsh `wakeDriver`): disposal never latches, so
    /// teardown waits on no model turn. The idle arm opens the running
    /// phase and marks the kick the run loop then executes.
    fn wake_driver(&mut self, wake_after_abort: bool) {
        if !matches!(self.machine.phase(), Phase::Idle { .. }) {
            let disposed = matches!(self.abort_cause(), Some(CancelCause::Disposed));
            let latching = matches!(self.machine.phase(), Phase::Maintenance { .. });
            if !disposed && (latching || wake_after_abort) {
                self.set_wake_requested(true);
            }
            return;
        }
        self.kick_pending = true;
        let Phase::Idle { last_turn } = *self.machine.phase() else {
            unreachable!("the idle check above holds");
        };
        self.machine.commit(Phase::Running {
            abort: AbortLatch::new(),
            turn: last_turn,
            step: 0,
            wake_requested: false,
        });
    }

    /// Runs turns until one ends without queued work (dsh `kick`):
    /// reported failures and cancellation are contained at this
    /// boundary; convergence replays a latched wake.
    fn kick(&mut self) {
        while matches!(self.turn(), Ok(true)) {}
        if let Phase::Running {
            turn,
            wake_requested,
            ..
        } = self.machine.phase()
        {
            let (turn, wake_requested) = (*turn, *wake_requested);
            self.machine.commit(Phase::Idle { last_turn: turn });
            if wake_requested && self.inbox.has_pending() {
                self.wake_driver(false);
            }
        }
    }

    /// One dsh `throwIfAborted` site: apply queued commands, fold an
    /// observed shutdown into a disposed cancel (the in-thread stand-in
    /// for dsh's factory-ownership abort), then surface a latched cause.
    fn poll(&mut self) -> Result<(), DriverError> {
        self.apply_commands();
        if self.mailbox.is_shutdown() {
            self.arm_latch(CancelCause::Disposed);
        }
        if let Some(cause) = self.abort_cause() {
            return Err(DriverError::Aborted(cause));
        }
        Ok(())
    }

    /// Reports one failure at its live boundary (dsh `throwError` emits
    /// then rethrows; the returned error is the rethrow).
    fn throw_error(&self, error: DriverError) -> DriverError {
        self.emit_error(&error);
        error
    }

    /// The emit half of dsh `throwError` (the position comes from the
    /// live phase).
    fn emit_error(&self, error: &DriverError) {
        let (turn, step) = match self.machine.phase() {
            Phase::Running { turn, step, .. } => (*turn, *step),
            Phase::Idle { last_turn } | Phase::Maintenance { last_turn, .. } => (*last_turn, 0),
        };
        self.events.emit_error(&ErrorPayload {
            turn,
            step,
            error: turn_failure(error),
        });
    }

    fn non_idle_aborted(&self) -> bool {
        self.abort_cause().is_some()
    }

    fn abort_cause(&self) -> Option<CancelCause> {
        match self.machine.phase() {
            Phase::Idle { .. } => None,
            Phase::Maintenance { abort, .. } | Phase::Running { abort, .. } => {
                abort.aborted_cause().cloned()
            }
        }
    }

    fn arm_latch(&mut self, cause: CancelCause) {
        match self.machine.phase_mut() {
            Phase::Idle { .. } => {}
            Phase::Maintenance { abort, .. } | Phase::Running { abort, .. } => abort.abort(cause),
        }
    }

    fn set_wake_requested(&mut self, value: bool) {
        if let Some(wake_requested) = self.wake_requested_slot() {
            *wake_requested = value;
        }
    }

    #[expect(
        clippy::missing_const_for_fn,
        reason = "phase_mut is not const-callable; the compiler is the source of truth"
    )]
    fn wake_requested_slot(&mut self) -> Option<&mut bool> {
        match self.machine.phase_mut() {
            Phase::Idle { .. } => None,
            Phase::Maintenance { wake_requested, .. } | Phase::Running { wake_requested, .. } => {
                Some(wake_requested)
            }
        }
    }

    /// The running position, with dsh's verbatim refusal for the caller
    /// that supplied one.
    fn running_position(&self, refusal: &str) -> Result<(u64, u64), DriverError> {
        match self.machine.phase() {
            Phase::Running { turn, step, .. } => Ok((*turn, *step)),
            _ => Err(DriverError::Text(format!(
                "agent \"{}\": {refusal}",
                self.session_id
            ))),
        }
    }

    /// Commits one typed event and feeds it to the observers (dsh's
    /// `session/event` observer: runtime-context retention always, the
    /// step's tool recovery while a step runs; the driver's own appends
    /// are the whole observable stream in this slice — cross-writer
    /// observation attaches with the Phase 18 fabric).
    fn commit<T: serde::Serialize>(
        &mut self,
        type_name: &str,
        data: &T,
        surface_op: Option<&SurfaceOp>,
        source_event_seqs: Option<&[SessionSeq]>,
    ) -> Result<SessionEvent, DriverError> {
        let value =
            serde_json::to_value(data).map_err(|error| DriverError::Text(error.to_string()))?;
        let event = self
            .client
            .append(type_name, &value, surface_op, source_event_seqs)?;
        self.runtime_context.observe(&event);
        if let Some(recovery) = &mut self.step_recovery {
            recovery.observe(&event);
        }
        Ok(event)
    }

    /// Opens one turn before claiming its first proposed step (dsh
    /// `turn`): the early poll escapes before any durable write; every
    /// later exit assigns a turn ending and appends `turn/end`.
    fn turn(&mut self) -> Result<bool, DriverError> {
        if !matches!(self.machine.phase(), Phase::Running { .. }) {
            return Err(self.throw_error(DriverError::Text(format!(
                "agent \"{}\": turn without driver reservation",
                self.session_id
            ))));
        }
        let (mut turn_number, _) = self.running_position("turn without driver reservation")?;
        self.poll()?;
        turn_number += 1;
        let turn = turn_number;
        self.commit(event_type::TURN_START, &TurnStartData { turn }, None, None)
            .map_err(|error| self.throw_error(error))?;
        if let Phase::Running { turn: current, .. } = self.machine.phase_mut() {
            *current = turn;
        }

        let mut turn_ends: Option<TurnEndReason> = None;
        let result = self.turn_body(turn, &mut turn_ends);
        let reason = match &result {
            Ok(_) => turn_ends
                .take()
                .expect("every completed turn assigns an ending"),
            Err(error) => {
                let reason = match error {
                    DriverError::Aborted(cause) => {
                        TurnEndReason::Known(TurnEndReasonKind::Aborted {
                            reason: cause.clone(),
                        })
                    }
                    other => TurnEndReason::Known(TurnEndReasonKind::Error {
                        error: turn_failure(other),
                    }),
                };
                self.emit_error(error);
                reason
            }
        };
        // finally: every exit appends the turn ending.
        let end = self.commit(
            event_type::TURN_END,
            &TurnEndData { turn, reason },
            None,
            None,
        );
        match (result, end) {
            (_, Err(error)) => Err(self.throw_error(error)),
            (Err(error), Ok(_)) => Err(error),
            (Ok(TurnFlow::EarlyFalse), Ok(_)) => Ok(false),
            (Ok(TurnFlow::Epilogue), Ok(_)) => {
                if !self.inbox.has_pending() {
                    return Ok(false);
                }
                // A fresh latch makes a wake latched on the old one
                // stale: the live driver claims the queue itself.
                self.arm_fresh_running_latch();
                Ok(true)
            }
        }
    }

    /// Re-arms the running phase for the next turn (dsh `turn`'s fresh
    /// `AbortController` epilogue, in place).
    fn arm_fresh_running_latch(&mut self) {
        if let Phase::Running {
            abort,
            wake_requested,
            step,
            ..
        } = self.machine.phase_mut()
        {
            *abort = AbortLatch::new();
            *wake_requested = false;
            *step = 0;
        }
    }

    /// The turn's step loop (dsh `turn`'s try block).
    fn turn_body(
        &mut self,
        turn: u64,
        turn_ends: &mut Option<TurnEndReason>,
    ) -> Result<TurnFlow, DriverError> {
        let mut target = InboxTarget::NextTurn;
        loop {
            self.poll()?;
            let step = match self.machine.phase() {
                Phase::Running { step, .. } => step + 1,
                _ => unreachable!("the running phase holds across the turn"),
            };
            let decision = self.pre_step(target, turn, step)?;
            let enter = match decision {
                PreparedStep::Reject => {
                    *turn_ends = Some(TurnEndReason::Known(TurnEndReasonKind::Blocked));
                    return Ok(TurnFlow::EarlyFalse);
                }
                PreparedStep::Enter(enter) => enter,
            };
            if turn_ends.is_some() && enter.messages.is_empty() {
                // A removed waking message or an enter decision rewritten
                // to empty still owns the boundary but spends no model
                // call: silently keep the recorded ending.
                return Ok(TurnFlow::Epilogue);
            }
            if matches!(self.machine.phase(), Phase::Running { step: 0, .. })
                && enter.messages.is_empty()
            {
                *turn_ends = Some(TurnEndReason::Known(TurnEndReasonKind::Completed));
                return Ok(TurnFlow::EarlyFalse);
            }
            self.poll()?;
            self.commit(
                event_type::STEP_START,
                &StepBoundsData { turn, step },
                None,
                None,
            )?;
            if let Phase::Running { step: current, .. } = self.machine.phase_mut() {
                *current = step;
            }
            self.step_recovery = Some(ToolCallRecovery::new(OpenTurnCloseCause::Interrupted));
            let step_result = self.step(&enter);
            let step_result = match step_result {
                Ok(end) => Ok(end),
                Err(error) => match self.recover_step_results() {
                    Ok(()) => Err(error),
                    Err(recovery_error) => Err(DriverError::StepRecovery {
                        step: Box::new(error),
                        recovery: Box::new(recovery_error),
                    }),
                },
            };
            // finally: stop the recovery observer, then close the step.
            self.step_recovery = None;
            if let Err(end_error) = self.commit(
                event_type::STEP_END,
                &StepBoundsData { turn, step },
                None,
                None,
            ) {
                return Err(self.throw_error(end_error));
            }
            let step_end = step_result?;
            // max-tokens stays sticky: a later completed step must not
            // downgrade the turn outcome.
            if !matches!(
                turn_ends,
                Some(TurnEndReason::Known(TurnEndReasonKind::MaxTokens))
            ) {
                *turn_ends = step_end.map(TurnEndReason::Known);
            }
            self.poll()?;
            if turn_ends.is_some() && self.inbox.next_step().is_empty() {
                self.events
                    .serial_turn_stopping(TurnStoppingPayload { turn })
                    .map_err(DriverError::Listener)?;
                self.poll()?;
            }
            if turn_ends.is_some() && self.inbox.next_step().is_empty() {
                return Ok(TurnFlow::Epilogue);
            }
            target = InboxTarget::NextStep;
        }
    }

    /// Appends the recovery results for a failed step's unanswered tool
    /// calls (dsh the catch-block `toolRecovery.results()` loop).
    fn recover_step_results(&mut self) -> Result<(), DriverError> {
        let Some(recovery) = self.step_recovery.take() else {
            return Ok(());
        };
        for event in recovery
            .results()
            .map_err(|error| DriverError::Text(error.to_string()))?
        {
            let data = match &event.data {
                SessionEventData::ToolResult(data) => serde_json::to_value(data)
                    .map_err(|error| DriverError::Text(error.to_string()))?,
                _ => continue,
            };
            let seqs = event.source_event_seqs.clone();
            self.commit(
                event_type::TOOL_RESULT,
                &data,
                Some(&SurfaceOp::Append(AppendOp)),
                seqs.as_deref(),
            )?;
        }
        Ok(())
    }

    /// Proposes one step boundary (dsh `preStep`): claim, assemble,
    /// waterfall, with abort checks around the assembled prompt.
    fn pre_step(
        &mut self,
        target: InboxTarget,
        turn: u64,
        step: u64,
    ) -> Result<PreparedStep, DriverError> {
        self.running_position("pre-step outside running phase")?;
        let claimed = self.inbox.claim(target, turn).map_err(DriverError::Inbox)?;
        let assembly = self.prompt.assemble().map_err(DriverError::Prompt)?;
        self.poll()?;
        let runtime_context = &self.runtime_context;
        let next_id = &self.next_id;
        let joined = assembly.joined_sections.clone();
        let sections = assembly.context_sections.clone();
        let decision = self.events.waterfall_pre_step(
            PreStepPayload {
                messages: claimed,
                turn,
                step,
            },
            Box::new(move |payload| {
                let context = runtime_context.project(&joined, &sections, next_id);
                let mut messages = payload.messages;
                if let Some(context) = context {
                    messages.push(context);
                }
                PreStepDecision::Enter {
                    messages,
                    starts_request_series: false,
                }
            }),
        );
        self.poll()?;
        Ok(match decision {
            PreStepDecision::Reject => PreparedStep::Reject,
            PreStepDecision::Enter {
                messages,
                starts_request_series,
            } => PreparedStep::Enter(EnterStep {
                messages,
                starts_request_series,
                assembly,
            }),
        })
    }

    /// Whether the assembled tool schemas differ from the logged request
    /// header's (dsh `toolsChanged`).
    fn tools_changed(&mut self, tools: &[ToolSchema]) -> bool {
        let Some(baseline) = self.reads.request_header() else {
            return false;
        };
        let candidate = canonical_header(&EpochHeader {
            config: baseline.config.clone(),
            adapter_defaults: baseline.adapter_defaults,
            tools: Some(tools.to_vec()),
        });
        baseline != candidate
    }

    /// Executes one admitted step (dsh `step`): render once, then retry
    /// the request loop until one attempt settles.
    fn step(&mut self, enter: &EnterStep) -> Result<Option<StepEndReason>, DriverError> {
        self.running_position("step outside running phase")?;
        let (turn, step) = self.running_position("step outside running phase")?;
        self.poll()?;
        let rendered_prompt = enter.assembly.rendered_prompt.clone();
        let mut first_attempt = true;
        loop {
            let (config, prepared) = self.prepare_request(turn, step)?;
            let current = self
                .prompt
                .refresh_context(&enter.assembly)
                .map_err(DriverError::Prompt)?;
            let context = self.runtime_context.project(
                &current.joined_sections,
                &current.context_sections,
                || (self.next_id)(),
            );
            self.poll()?;
            let starts_request_series = first_attempt && enter.starts_request_series;
            let prepared_ref = prepared.as_ref();
            let in_history = prepared_ref.is_some_and(|p| {
                p.model.system_prompt_update == Some(SystemPromptUpdate::InHistory)
            });
            let surface_generation = self.reads.surface_generation();
            let starts_series = starts_request_series
                || self.request_surface_generation != surface_generation
                || (prepared_ref.is_none_or(|p| p.model.tool_update.is_none())
                    && self.tools_changed(&enter.assembly.tools));
            self.admit_step_inputs(
                enter,
                &AdmissionInputs {
                    rendered_prompt: &rendered_prompt,
                    context: context.as_ref(),
                    in_history,
                    starts_series,
                    first_attempt,
                    turn,
                    step,
                },
            )?;
            first_attempt = false;
            let request = self.build_request(
                &config,
                prepared_ref,
                &enter.assembly.tools,
                turn,
                step,
                starts_request_series,
            )?;

            let mut live = self.begin_attempt(turn, step);
            let mut started = false;
            let stream_result =
                self.consume_stream(&request, prepared_ref, &mut live, &mut started);
            match stream_result {
                Ok(()) => {}
                Err(error) => {
                    if !started {
                        return Err(error);
                    }
                    let settled = self.settle_interrupted(turn, step, &request, &mut live);
                    if let Err(settlement_error) = settled {
                        return Err(DriverError::Settlement {
                            source: Box::new(error),
                            settlement: Box::new(settlement_error),
                        });
                    }
                    return Err(error);
                }
            }
            let attempt = self.finish_attempt(turn, step, &request, prepared_ref, &mut live);
            match attempt {
                Ok(AttemptEnd::Retry) => {}
                Ok(AttemptEnd::Done(end)) => return Ok(end),
                Err(error) => {
                    if !live.ended() {
                        live.abandon();
                    }
                    return Err(error);
                }
            }
        }
    }

    /// Commits one attempt's model-visible input (dsh step's admission
    /// block): the system-prompt projection's commits, then the claimed
    /// batch with the runtime-context snapshot admitted exactly once.
    fn admit_step_inputs(
        &mut self,
        enter: &EnterStep,
        admission: &AdmissionInputs<'_>,
    ) -> Result<(), DriverError> {
        let AdmissionInputs {
            rendered_prompt,
            context,
            in_history,
            starts_series,
            first_attempt,
            turn,
            step,
        } = *admission;
        let nodes = self.reads.system_prompt_nodes();
        let commits = project_system_prompt(
            &nodes,
            rendered_prompt,
            SystemPromptDecisionInput {
                in_history,
                starts_series,
            },
            || (self.next_id)(),
        );
        for commit in commits {
            self.commit(
                event_type::SYSTEM_MESSAGE,
                &SystemMessageData {
                    turn,
                    step,
                    message: commit.message.clone(),
                },
                Some(&commit.intent.surface_op),
                commit.intent.source_event_seqs.as_deref(),
            )?;
        }
        // The runtime-context snapshot is admitted exactly once per step:
        // at the first owned slot, or failing that after the loop
        // (retries never re-admit it).
        let mut context_admitted = false;
        if first_attempt {
            for message in &enter.messages {
                if is_runtime_context_message(message) {
                    if let (true, Some(snapshot)) = (!context_admitted, context) {
                        self.commit(
                            event_type::USER_MESSAGE,
                            snapshot,
                            Some(&SurfaceOp::Append(AppendOp)),
                            None,
                        )?;
                        context_admitted = true;
                    }
                } else {
                    self.commit(
                        event_type::USER_MESSAGE,
                        message,
                        Some(&SurfaceOp::Append(AppendOp)),
                        None,
                    )?;
                }
            }
        }
        if let (false, Some(snapshot)) = (context_admitted, context) {
            self.commit(
                event_type::USER_MESSAGE,
                snapshot,
                Some(&SurfaceOp::Append(AppendOp)),
                None,
            )?;
        }
        Ok(())
    }

    /// Resolves the request config and binds its adapter (dsh
    /// `prepareRequest`), tolerating `NO_ADAPTER` for middleware-proposed
    /// routes.
    fn prepare_request(
        &mut self,
        turn: u64,
        step: u64,
    ) -> Result<(LlmCallConfig, Option<PreparedCall>), DriverError> {
        let persisted = self.reads.request_header();
        let route = LlmCallConfig {
            provider: self.options.provider.clone().unwrap_or_default(),
            model: self.options.model.clone().unwrap_or_default(),
            controls: CallControls::default(),
            agent_loop: false,
        };
        // A loop instance starts from its declared route, restoring only
        // an explicit effort owned by that exact model.
        let persisted_effort = persisted
            .as_ref()
            .filter(|header| {
                header.config.provider == route.provider && header.config.model == route.model
            })
            .filter(|header| {
                header
                    .adapter_defaults
                    .as_ref()
                    .is_none_or(|defaults| defaults.reasoning_effort.is_none())
            })
            .and_then(|header| header.config.controls.reasoning_effort.clone());
        let effort = self.options.reasoning_effort.clone().or(persisted_effort);
        let seed = if self.request_header_logged {
            request_proposal(
                persisted
                    .as_ref()
                    .expect("the instance logged the header it now folds"),
            )
        } else {
            let mut seed = route;
            seed.controls.reasoning_effort = effort;
            seed.controls.max_tokens = self.options.max_tokens;
            seed
        };
        let proposed = self
            .events
            .waterfall_request(RequestPayload { turn, step }, Box::new(move |_| seed));
        self.poll()?;
        if proposed.provider.is_empty() || proposed.model.is_empty() {
            return Err(DriverError::Text(format!(
                "agent \"{}\" has no provider/model: set AgentOptions.provider and AgentOptions.model or supply both via the agent/request waterfall",
                self.session_id
            )));
        }
        match self.llm.prepare_call(&proposed) {
            Ok(prepared) => Ok((prepared.config.clone(), Some(prepared))),
            Err(error) if error.code() == "NO_ADAPTER" => Ok((proposed, None)),
            Err(error) => Err(DriverError::Llm(error)),
        }
    }

    /// Logs the resolved header snapshot (dsh buildRequest's three-state
    /// publication): the first request is `initial`/`resume`, a changed
    /// canonical header is `change` with the tool delta, and an unchanged
    /// header on a series boundary is `series` alone.
    fn log_request_header(
        &mut self,
        header: &EpochHeader,
        baseline: Option<&EpochHeader>,
        starts_series: bool,
    ) -> Result<Option<SessionSeq>, DriverError> {
        if !self.request_header_logged {
            let reason = if baseline.is_none() {
                RequestHeaderReason::Initial
            } else {
                RequestHeaderReason::Resume
            };
            let event = self.commit(
                event_type::REQUEST_HEADER,
                &RequestHeaderData {
                    header: header.clone(),
                    reason,
                    starts_series: starts_series.then_some(TrueOnly),
                },
                None,
                None,
            )?;
            self.request_header_logged = true;
            return Ok(Some(event.seq));
        }
        if baseline.is_none_or(|logged| logged != header) {
            let event = self.commit(
                event_type::REQUEST_HEADER,
                &RequestHeaderData {
                    header: header.clone(),
                    reason: RequestHeaderReason::Change,
                    starts_series: starts_series.then_some(TrueOnly),
                },
                None,
                None,
            )?;
            return Ok(Some(event.seq));
        }
        if starts_series {
            self.commit(
                event_type::REQUEST_HEADER,
                &RequestHeaderData {
                    header: header.clone(),
                    reason: RequestHeaderReason::Series,
                    starts_series: None,
                },
                None,
                None,
            )?;
        }
        Ok(None)
    }

    /// Logs the resolved envelope and derives the request from the
    /// admitted surface (dsh `buildRequest`). The derived messages are
    /// freshly read values; Rust ownership is the freeze.
    fn build_request(
        &mut self,
        config: &LlmCallConfig,
        prepared: Option<&PreparedCall>,
        tools: &[ToolSchema],
        turn: u64,
        step: u64,
        starts_request_series: bool,
    ) -> Result<GenerateOptions, DriverError> {
        let surface_generation = self.reads.surface_generation();
        let header = canonical_header(&EpochHeader {
            config: to_wire_config(config),
            adapter_defaults: prepared.and_then(|p| p.adapter_defaults),
            tools: (!tools.is_empty()).then(|| tools.to_vec()),
        });
        let baseline = self.reads.request_header();
        let starts_series =
            starts_request_series || self.request_surface_generation != surface_generation;
        let header_seq = self.log_request_header(&header, baseline.as_ref(), starts_series)?;
        if let (Some(baseline), Some(header_seq)) = (baseline.as_ref(), header_seq) {
            let previous: HashSet<&str> = baseline
                .tools
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|tool| tool.name.as_str())
                .collect();
            let current: HashSet<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
            let additions: Vec<ContentBlock> = tools
                .iter()
                .filter(|tool| !previous.contains(tool.name.as_str()))
                .map(|tool| {
                    ContentBlock::Known(KnownContentBlock::ToolAddition {
                        tool_name: tool.name.clone(),
                    })
                })
                .collect();
            let mut content = additions.clone();
            content.extend(
                baseline
                    .tools
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .filter(|tool| !current.contains(tool.name.as_str()))
                    .map(|tool| {
                        ContentBlock::Known(KnownContentBlock::ToolRemoval {
                            tool_name: tool.name.clone(),
                        })
                    }),
            );
            if !content.is_empty() {
                let message = developer_message((self.next_id)(), content);
                self.commit(
                    event_type::DEVELOPER_MESSAGE,
                    &DeveloperMessageData {
                        turn,
                        step,
                        message,
                        header_seq: (!additions.is_empty()).then_some(header_seq),
                    },
                    Some(&SurfaceOp::Append(AppendOp)),
                    None,
                )?;
            }
        }
        self.request_surface_generation = surface_generation;

        let request_context = RequestContextData {
            provider: config.provider.clone(),
            model: config.model.clone(),
            context_window: prepared.and_then(|p| p.model.context.map(|c| c.context_window)),
            system_prompt_update: prepared.and_then(|p| p.model.system_prompt_update),
        };
        if self.reads.request_context().as_ref() != Some(&request_context) {
            self.commit(
                event_type::REQUEST_CONTEXT,
                &RequestContextHolder {
                    context: request_context,
                },
                None,
                None,
            )?;
        }
        self.poll()?;
        let messages = self
            .reads
            .derive_messages()
            .into_iter()
            .map(RequestMessage::Durable)
            .collect();
        Ok(GenerateOptions {
            provider: config.provider.clone(),
            model: config.model.clone(),
            reasoning_effort: config.controls.reasoning_effort.clone(),
            messages,
            system: None,
            tools: header.tools,
            tool_history: Some(self.reads.tool_history()),
            temperature: config.controls.temperature,
            max_tokens: config.controls.max_tokens,
            stop: config.controls.stop.clone(),
            signal: None,
            session_id: Some(self.session_id.clone()),
            purpose: None,
        })
    }

    /// Builds one attempt frame publisher bound to this driver's counters
    /// (dsh the `AssistantStreamAttempt` construction).
    fn begin_attempt(&mut self, turn: u64, step: u64) -> AssistantStreamAttempt {
        self.assistant_attempt_counter += 1;
        let attempt = self.assistant_attempt_counter;
        let revision = Arc::clone(&self.assistant_stream_revision);
        let events = Arc::clone(&self.events);
        let next_revision: RevisionAllocator =
            Box::new(move || revision.fetch_add(1, Ordering::Relaxed) + 1);
        let emit: FrameSink = Box::new(move |frame| {
            events.emit_assistant_stream(&AssistantStreamPayload {
                frame: frame.clone(),
            });
        });
        AssistantStreamAttempt::new(&self.session_id, attempt, turn, step, next_revision, emit)
    }

    /// Consumes one generation (dsh the stream for-await): begin, start,
    /// then poll-drain-park until the terminal batch lands. On any
    /// failure the generation is best-effort aborted; the settlement
    /// below derives from the delivered prefix.
    fn consume_stream(
        &mut self,
        request: &GenerateOptions,
        prepared: Option<&PreparedCall>,
        live: &mut AssistantStreamAttempt,
        started: &mut bool,
    ) -> Result<(), DriverError> {
        let generation = self
            .llm
            .begin(request, prepared)
            .map_err(DriverError::Llm)?;
        let result = self.pump_generation(&generation, live, started);
        if result.is_err() {
            // Best effort: the worker settles its own accounting; the
            // caller-abort settlement semantics own the durable record.
            drop(self.llm.abort(&generation));
        }
        result
    }

    fn pump_generation(
        &mut self,
        generation: &str,
        live: &mut AssistantStreamAttempt,
        started: &mut bool,
    ) -> Result<(), DriverError> {
        self.poll()?;
        live.start();
        *started = true;
        loop {
            self.poll()?;
            let drained = self.llm.drain(generation).map_err(DriverError::Llm)?;
            let time = (self.clock)();
            for chunk in drained.chunks {
                live.push(time, chunk)
                    .map_err(|rejection| DriverError::Text(rejection.to_string()))?;
            }
            if drained.done {
                // dsh checks the signal once more after the loop ends.
                return self.poll();
            }
            self.mailbox.wait_step();
        }
    }

    /// Settles one interrupted or failed stream (dsh the catch arm):
    /// aborted with visible content commits an interrupted
    /// `assistant/message`, otherwise an `assistant/attempt`.
    fn settle_interrupted(
        &mut self,
        turn: u64,
        step: u64,
        request: &GenerateOptions,
        live: &mut AssistantStreamAttempt,
    ) -> Result<(), DriverError> {
        let aborted = self.abort_cause().is_some();
        if aborted {
            let content = live.interrupted_blocks();
            if !content.is_empty() {
                let message = assistant_message(
                    (self.next_id)(),
                    content,
                    &request.provider,
                    &request.model,
                    live,
                )?;
                let stream = live.stream();
                let usage = live.usage();
                let driver = &mut *self;
                live.settle(SettlementType::AssistantMessage, move || {
                    driver
                        .commit(
                            event_type::ASSISTANT_MESSAGE,
                            &AssistantMessageData {
                                turn,
                                step,
                                message,
                                stream,
                                usage,
                                interrupted: Some(TrueOnly),
                            },
                            Some(&SurfaceOp::Append(AppendOp)),
                            None,
                        )
                        .map(|event| event.seq)
                })?;
                return Ok(());
            }
        }
        let stream = live.stream();
        let driver = &mut *self;
        live.settle(SettlementType::AssistantAttempt, move || {
            driver
                .commit(
                    event_type::ASSISTANT_ATTEMPT,
                    &AssistantAttemptData { turn, step, stream },
                    None,
                    None,
                )
                .map(|event| event.seq)
        })?;
        Ok(())
    }

    /// Settles one completed stream (dsh the post-stream block): failed
    /// finishes settle an attempt and consult the retry waterfall;
    /// successful ones commit the message and continue into tools.
    fn finish_attempt(
        &mut self,
        turn: u64,
        step: u64,
        request: &GenerateOptions,
        prepared: Option<&PreparedCall>,
        live: &mut AssistantStreamAttempt,
    ) -> Result<AttemptEnd, DriverError> {
        let finish = live.finish();
        let failure = match &finish {
            FinishReason::Known(
                FinishReasonKind::Error { failure } | FinishReasonKind::Aborted { failure },
            ) => Some(failure.clone()),
            _ => None,
        };
        if let Some(failure) = failure {
            {
                let stream = live.stream();
                let driver = &mut *self;
                live.settle(SettlementType::AssistantAttempt, move || {
                    driver
                        .commit(
                            event_type::ASSISTANT_ATTEMPT,
                            &AssistantAttemptData { turn, step, stream },
                            None,
                            None,
                        )
                        .map(|event| event.seq)
                })?;
            }
            let action = self.events.waterfall_request_error(
                RequestErrorPayload {
                    turn,
                    step,
                    provider: request.provider.clone(),
                    failure: failure.clone(),
                    retry_policy: prepared.and_then(|p| p.retry_policy.clone()),
                },
                Box::new(|_| None),
            );
            self.poll()?;
            if action != Some(RequestErrorAction::Retry) {
                return Err(DriverError::Llm(llm_error(&failure)));
            }
            return Ok(AttemptEnd::Retry);
        }
        let message = assistant_message(
            (self.next_id)(),
            live.blocks()
                .map_err(|error| DriverError::Text(error.to_string()))?,
            &request.provider,
            &request.model,
            live,
        )?;
        {
            let settle_message = message.clone();
            let stream = live.stream();
            let usage = live.usage();
            let driver = &mut *self;
            live.settle(SettlementType::AssistantMessage, move || {
                driver
                    .commit(
                        event_type::ASSISTANT_MESSAGE,
                        &AssistantMessageData {
                            turn,
                            step,
                            message: settle_message,
                            stream,
                            usage,
                            interrupted: None,
                        },
                        Some(&SurfaceOp::Append(AppendOp)),
                        None,
                    )
                    .map(|event| event.seq)
            })?;
        }
        if matches!(finish, FinishReason::Known(FinishReasonKind::MaxTokens)) {
            return Ok(AttemptEnd::Done(Some(TurnEndReasonKind::MaxTokens)));
        }
        let tool_calls = tool_call_views(&message);
        if tool_calls.is_empty() {
            return Ok(AttemptEnd::Done(Some(TurnEndReasonKind::Completed)));
        }
        let concluded = self.execute_tool_calls(turn, step, &tool_calls)?;
        Ok(AttemptEnd::Done(
            concluded.then_some(TurnEndReasonKind::Completed),
        ))
    }
}

/// How one attempt ended for the step's retry loop.
enum AttemptEnd {
    /// The retry waterfall ruled a retry.
    Retry,
    /// The attempt settled; the step ends with this reason.
    Done(Option<StepEndReason>),
}

/// Whether one message is a runtime-context snapshot (the dsh source
/// kind rides `MessageSource::Other`).
fn is_runtime_context_message(message: &Message) -> bool {
    match message {
        Message::Known(known) => matches!(
            known.source(),
            MessageSource::Other(map) if map.get("kind").and_then(|v| v.as_str()) == Some(crate::runtime_context::SOURCE_KIND)
        ),
        Message::Other(_) => false,
    }
}

/// Builds one assistant message from assembled blocks (dsh
/// `createAssistantMessage`): the source carries the producing route and
/// the replay envelope when one survived assembly.
fn assistant_message(
    id: MessageId,
    content: Vec<ContentBlock>,
    provider: &str,
    model: &str,
    live: &AssistantStreamAttempt,
) -> Result<Message, DriverError> {
    let replay_state = live
        .replay_state()
        .map_err(|error| DriverError::Text(error.to_string()))?;
    Ok(Message::Known(Box::new(KnownMessage::Assistant {
        id,
        content,
        source: MessageSource::Known(KnownMessageSource::Model {
            provider: provider.to_owned(),
            model: model.to_owned(),
            replay_state: replay_state
                .map(serde_json::to_value)
                .transpose()
                .map_err(|error| DriverError::Text(error.to_string()))?,
            extra: RawObject::new(),
        }),
        extra: RawObject::new(),
    })))
}

/// Builds one developer message for the tool-registry source (dsh
/// `createDeveloperMessage`).
fn developer_message(id: MessageId, content: Vec<ContentBlock>) -> Message {
    let mut source = RawObject::new();
    source.insert("kind".to_owned(), serde_json::json!("tool-registry"));
    Message::Known(Box::new(KnownMessage::Developer {
        id,
        content,
        source: MessageSource::Other(source),
        extra: RawObject::new(),
    }))
}

/// Builds one tool-role result message (dsh `createToolResultMessage`).
fn tool_result_message(id: MessageId, call: &PlannedCall, result: &ToolExecutionResult) -> Message {
    Message::Known(Box::new(KnownMessage::Tool {
        id,
        content: result.content.clone(),
        source: MessageSource::Known(KnownMessageSource::Tool {
            call_id: call.id.clone(),
            extra: RawObject::new(),
        }),
        tool_call_id: call.id.clone(),
        is_error: result.is_error.then_some(TrueOnly),
        extra: RawObject::new(),
    }))
}

/// One tool-call block view extracted from assembled assistant content.
struct ToolCallView {
    id: ToolCallId,
    name: String,
    raw_arguments: String,
}

fn tool_call_views(message: &Message) -> Vec<ToolCallView> {
    let Message::Known(known) = message else {
        return Vec::new();
    };
    known
        .content()
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Known(KnownContentBlock::ToolCall {
                id,
                name,
                arguments,
            }) => Some(ToolCallView {
                id: id.clone(),
                name: name.clone(),
                raw_arguments: arguments.clone(),
            }),
            _ => None,
        })
        .collect()
}

impl LoopDriver {
    /// Schedules one assistant step's tool calls (dsh
    /// `executeToolCalls`): exclusive calls form barriers, parallel
    /// calls use a bounded rolling pool and are reclassified before
    /// start; abort records synthetic results for skipped calls.
    fn execute_tool_calls(
        &mut self,
        turn: u64,
        step: u64,
        tool_calls: &[ToolCallView],
    ) -> Result<bool, DriverError> {
        let planned: Vec<PlannedCall> = tool_calls
            .iter()
            .map(|view| PlannedCall {
                id: view.id.clone(),
                name: view.name.clone(),
                raw_arguments: view.raw_arguments.clone(),
            })
            .collect();
        let mut next = 0;
        let mut concluded = false;
        while next < planned.len() {
            // Commit before classifying again so registry changes affect
            // unstarted calls.
            let mode = self.tools.execution_mode(&planned[next].exec());
            let group_end = if mode == ToolExecutionMode::Parallel {
                planned.len()
            } else {
                next + 1
            };
            let outcome = self.run_group(turn, step, &planned[next..group_end], mode)?;
            next += outcome.consumed;
            if outcome.concluded {
                concluded = true;
            }
            if outcome.aborted {
                for call in &planned[next..] {
                    self.append_skipped_tool_call(turn, step, call)?;
                }
                return Ok(concluded);
            }
        }
        Ok(concluded)
    }

    /// Runs one exclusive barrier or parallel pool (dsh `runGroup`).
    fn run_group(
        &mut self,
        turn: u64,
        step: u64,
        group: &[PlannedCall],
        mode: ToolExecutionMode,
    ) -> Result<GroupOutcome, DriverError> {
        let max_parallel = (self.max_parallel_tool_calls)();
        let mut state = GroupState {
            slots: group.iter().map(|_| None).collect(),
            call_seqs: group.iter().map(|_| None).collect(),
            in_flight: Vec::new(),
            next_to_start: 0,
            committed: 0,
            started: 0,
            aborted: self.abort_cause().is_some(),
            concluded: false,
        };
        self.group_fill(turn, step, group, mode, max_parallel, &mut state)?;
        while !state.in_flight.is_empty() {
            self.mailbox.wait_step();
            self.apply_commands();
            if self.abort_cause().is_some() {
                state.aborted = true;
            }
            for (call_id, settlement) in self.tools.settled() {
                let Some(position) = state
                    .in_flight
                    .iter()
                    .position(|index| group[*index].id == call_id)
                else {
                    // A stale settle from an abandoned group: nothing
                    // consumes it here.
                    continue;
                };
                let index = state.in_flight.swap_remove(position);
                let exec = group[index].exec();
                state.slots[index] = Some(ToolSlot {
                    exec,
                    result: settlement.result,
                    needs_post: settlement.needs_post,
                });
            }
            self.group_commit_ready(turn, step, group, &mut state)?;
            if self.abort_cause().is_some() {
                state.aborted = true;
            }
            self.group_fill(turn, step, group, mode, max_parallel, &mut state)?;
        }
        if state.aborted {
            // Started calls and accepted context settle first; every
            // remaining model call then receives an ordered synthetic
            // result before the turn aborts.
            for call in &group[state.started..] {
                self.append_skipped_tool_call(turn, step, call)?;
            }
            return Ok(GroupOutcome {
                consumed: group.len(),
                aborted: true,
                concluded: state.concluded,
            });
        }
        if state.committed != state.started {
            return Err(DriverError::Text(
                "tool-call scheduler: uncommitted settled calls".to_owned(),
            ));
        }
        Ok(GroupOutcome {
            consumed: state.started,
            aborted: false,
            concluded: state.concluded,
        })
    }

    /// Starts calls while the pool window allows (dsh `fillPool`),
    /// reclassifying later calls after ordered commits so registry
    /// changes can create a barrier.
    fn group_fill(
        &mut self,
        turn: u64,
        step: u64,
        group: &[PlannedCall],
        mode: ToolExecutionMode,
        max_parallel: usize,
        state: &mut GroupState,
    ) -> Result<(), DriverError> {
        while !state.aborted
            && state.next_to_start < group.len()
            && state.in_flight.len() < max_parallel
        {
            let index = state.next_to_start;
            if index > 0
                && mode == ToolExecutionMode::Parallel
                && self.tools.execution_mode(&group[index].exec()) != ToolExecutionMode::Parallel
            {
                break;
            }
            // The durable call is appended before prepare so its result
            // can cite the seq.
            state.call_seqs[index] = Some(self.append_tool_call(turn, step, &group[index])?);
            state.started += 1;
            let exec = group[index].exec();
            match self.tools.prepare(&exec).map_err(DriverError::Tool)? {
                ToolPrepared::Dispatch { exec } => {
                    self.tools.dispatch(&exec).map_err(DriverError::Tool)?;
                    state.in_flight.push(index);
                }
                ToolPrepared::PostResult { exec, result } => {
                    state.slots[index] = Some(ToolSlot {
                        exec,
                        result,
                        needs_post: true,
                    });
                }
                ToolPrepared::FinalResult { exec, result } => {
                    state.slots[index] = Some(ToolSlot {
                        exec,
                        result,
                        needs_post: false,
                    });
                }
            }
            state.next_to_start += 1;
            self.group_commit_ready(turn, step, group, state)?;
            // Abort may arrive while pre-execute awaits.
            if self.abort_cause().is_some() {
                state.aborted = true;
            }
        }
        Ok(())
    }

    /// Commits settled results along the contiguous model-order prefix
    /// (dsh `commitReady`): post-result settlements finalize first, then
    /// the durable result lands with its call seq and any accepted
    /// context joins the next step boundary's inbox.
    fn group_commit_ready(
        &mut self,
        turn: u64,
        step: u64,
        group: &[PlannedCall],
        state: &mut GroupState,
    ) -> Result<(), DriverError> {
        while state.committed < group.len() {
            let Some(slot) = state.slots[state.committed].take() else {
                break;
            };
            let call = &group[state.committed];
            let result = if slot.needs_post {
                self.tools
                    .finalize(&slot.exec, slot.result)
                    .map_err(DriverError::Tool)?
            } else {
                self.tools.finish(&slot.exec, slot.result)
            };
            let call_seq =
                state.call_seqs[state.committed].expect("a started slot carries its tool/call seq");
            self.append_tool_result(turn, step, call, &result, call_seq)?;
            for context in result.additional_contexts {
                let len = i64::try_from(self.inbox.next_step().len())
                    .expect("an in-memory list length fits i64 on every supported target");
                self.inbox
                    .splice(InboxTarget::NextStep, len, 0, vec![context])
                    .map_err(DriverError::Inbox)?;
            }
            if result.concludes_turn {
                state.concluded = true;
            }
            state.committed += 1;
        }
        Ok(())
    }

    /// Appends the durable call/result pair for a model call skipped
    /// after cancellation (dsh `appendSkippedToolCall`).
    fn append_skipped_tool_call(
        &mut self,
        turn: u64,
        step: u64,
        call: &PlannedCall,
    ) -> Result<(), DriverError> {
        let call_seq = self.append_tool_call(turn, step, call)?;
        let result = ToolExecutionResult {
            content: vec![ContentBlock::Known(KnownContentBlock::Text {
                text: "Error: tool call aborted before dispatch".to_owned(),
            })],
            is_error: true,
            error: Some(ToolResultError {
                name: "AbortError".to_owned(),
                code: TOOL_ABORTED_BEFORE_DISPATCH.to_owned(),
                reason: None,
            }),
            meta: None,
            additional_contexts: Vec::new(),
            concludes_turn: false,
        };
        self.append_tool_result(turn, step, call, &result, call_seq)
    }

    /// Appends one `tool/call` and returns the seq its result must cite.
    fn append_tool_call(
        &mut self,
        turn: u64,
        step: u64,
        call: &PlannedCall,
    ) -> Result<SessionSeq, DriverError> {
        let event = self.commit(
            event_type::TOOL_CALL,
            &ToolCallData {
                turn,
                step,
                call_id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.raw_arguments.clone(),
            },
            None,
            None,
        )?;
        Ok(event.seq)
    }

    /// Appends one model-ordered result linked to its call event (dsh
    /// `appendToolResult`).
    fn append_tool_result(
        &mut self,
        turn: u64,
        step: u64,
        call: &PlannedCall,
        result: &ToolExecutionResult,
        call_seq: SessionSeq,
    ) -> Result<(), DriverError> {
        let message = tool_result_message((self.next_id)(), call, result);
        self.commit(
            event_type::TOOL_RESULT,
            &ToolResultData {
                turn,
                step,
                message,
                error: result.error.clone(),
                meta: result.meta.clone(),
            },
            Some(&SurfaceOp::Append(AppendOp)),
            Some(&[call_seq]),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AgentOptions, DrainedGeneration, DriverDeps, DriverError, LoopDriver, PreparedCall,
        PromptAssembly, RestoreView, SessionReads, SystemPromptFace, TOOL_ABORTED_BEFORE_DISPATCH,
        ToolExecution, ToolExecutionMode, ToolExecutionResult, ToolPrepared, ToolSettlement,
        ToolsFace,
    };
    use crate::agent_events::AgentEvents;
    use crate::outbound::Outbound;
    use crate::plugin_shell::{InboundCommand, SessionMap, session_mailbox};
    use dougong_abi::{HostExports, STATUS_OK};
    use fr_llm::adapter::{GenerateOptions, LlmCallConfig, ResolvedModelInfo};
    use fr_llm::error::LlmError;
    use fr_wire::event::{CancelCause, EpochHeader, SessionEventData, SurfaceOp};
    use fr_wire::ids::ToolCallId;
    use fr_wire::message::Message;
    use fr_wire::stream::{KnownStreamChunk, StreamChunk};
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// The in-memory session the fake provider and the read seam share:
    /// every append commits a typed event; the read faces fold over the
    /// committed log (the single-writer simplification of the service
    /// contract).
    struct TestSession {
        next_seq: u64,
        events: Vec<fr_wire::event::SessionEvent>,
        begins: usize,
    }

    impl TestSession {
        fn new() -> Self {
            Self {
                next_seq: 0,
                events: Vec::new(),
                begins: 0,
            }
        }

        fn append(&mut self, payload: &serde_json::Value) -> fr_wire::event::SessionEvent {
            let event = fr_wire::event::SessionEvent {
                seq: fr_wire::SessionSeq::new(self.next_seq)
                    .expect("fixture seqs stay within the safe-integer domain"),
                time: 1_700_000_000_000,
                data: decode_event(
                    payload["type"].as_str().unwrap_or_default(),
                    &payload["data"],
                ),
                ignorable: None,
                surface_op: payload
                    .get("surfaceOp")
                    .and_then(|v| serde_json::from_value(v.clone()).ok()),
                source_event_seqs: payload.get("sourceEventSeqs").and_then(|v| {
                    serde_json::from_value::<Vec<fr_wire::SessionSeq>>(v.clone()).ok()
                }),
            };
            self.next_seq += 1;
            self.events.push(event.clone());
            event
        }

        fn typed(&self, name: &str) -> Vec<&fr_wire::event::SessionEvent> {
            self.events
                .iter()
                .filter(|event| event.event_type() == name)
                .collect()
        }
    }

    /// Decodes one appended data payload back into typed event data (the
    /// fake folds only the faces the driver reads).
    fn decode_event(type_name: &str, data: &serde_json::Value) -> SessionEventData {
        match type_name {
            "turn/start" => SessionEventData::TurnStart(
                serde_json::from_value(data.clone()).expect("turn/start data"),
            ),
            "turn/end" => SessionEventData::TurnEnd(
                serde_json::from_value(data.clone()).expect("turn/end data"),
            ),
            "step/start" => SessionEventData::StepStart(
                serde_json::from_value(data.clone()).expect("step start data"),
            ),
            "step/end" => SessionEventData::StepEnd(
                serde_json::from_value(data.clone()).expect("step end data"),
            ),
            "user/message" => SessionEventData::UserMessage(
                serde_json::from_value(data.clone()).expect("user message"),
            ),
            "system/message" => SessionEventData::SystemMessage(
                serde_json::from_value(data.clone()).expect("system message"),
            ),
            "assistant/message" => SessionEventData::AssistantMessage(
                serde_json::from_value(data.clone()).expect("assistant message"),
            ),
            "assistant/attempt" => SessionEventData::AssistantAttempt(
                serde_json::from_value(data.clone()).expect("assistant attempt"),
            ),
            "tool/call" => {
                SessionEventData::ToolCall(serde_json::from_value(data.clone()).expect("tool call"))
            }
            "tool/result" => SessionEventData::ToolResult(
                serde_json::from_value(data.clone()).expect("tool result"),
            ),
            "request/header" => SessionEventData::RequestHeader(
                serde_json::from_value(data.clone()).expect("request header"),
            ),
            "request/context" => SessionEventData::RequestContext(
                serde_json::from_value(data.clone()).expect("request context"),
            ),
            "agent/inbox/spliced" => SessionEventData::Unknown(fr_wire::event::UnknownEventData {
                type_name: "agent/inbox/spliced".to_owned(),
                data: serde_json::from_value(data.clone()).expect("inbox splice"),
            }),
            other => SessionEventData::Unknown(fr_wire::event::UnknownEventData {
                type_name: other.to_owned(),
                data: serde_json::from_value(data.clone()).expect("unknown data"),
            }),
        }
    }

    /// Serves the append contract over the shared test session.
    unsafe extern "C" fn serving_require(
        host: *mut c_void,
        _service: *const u8,
        _service_len: usize,
        _method: u32,
        in_data: *const u8,
        in_len: usize,
        out_buf: *mut u8,
        cap: usize,
        out_len: *mut usize,
    ) -> i32 {
        // SAFETY: the vtable contract passes the test-owned box, valid
        // for the harness lifetime, and one readable payload slice.
        let session = unsafe { &*(host.cast::<Arc<Mutex<TestSession>>>()) };
        // SAFETY: `in_len` nonzero with `in_data` non-null, valid for
        // `in_len` readable bytes per the vtable contract.
        let payload: serde_json::Value = unsafe {
            serde_json::from_slice(std::slice::from_raw_parts(in_data, in_len))
                .expect("append request is JSON")
        };
        let event = session.lock().expect("test session lock").append(&payload);
        let reply = serde_json::json!({
            "event": {
                "type": payload["type"],
                "seq": event.seq.get(),
                "time": event.time,
                "data": payload["data"],
            }
        });
        let bytes = serde_json::to_vec(&reply).expect("reply serializes");
        // SAFETY: the vtable contract guarantees the caller-owned buffer.
        let written = unsafe { dougong_abi::write_out(&bytes, out_buf, cap, out_len) };
        if written {
            STATUS_OK
        } else {
            dougong_abi::STATUS_CAPACITY
        }
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

    /// The read-seam double over the same committed log.
    struct TestReads {
        session: Arc<Mutex<TestSession>>,
    }

    impl SessionReads for TestReads {
        fn restore(&mut self) -> RestoreView {
            let session = self.session.lock().expect("test session lock");
            let surface = surface_seqs(&session.events);
            RestoreView {
                events: session.events.clone(),
                surface,
            }
        }

        fn request_header(&mut self) -> Option<EpochHeader> {
            let session = self.session.lock().expect("test session lock");
            session
                .events
                .iter()
                .rev()
                .find_map(|event| match &event.data {
                    SessionEventData::RequestHeader(data) => Some(data.header.clone()),
                    _ => None,
                })
        }

        fn request_context(&mut self) -> Option<fr_wire::event::RequestContextData> {
            let session = self.session.lock().expect("test session lock");
            session
                .events
                .iter()
                .rev()
                .find_map(|event| match &event.data {
                    SessionEventData::RequestContext(data) => Some(data.context.clone()),
                    _ => None,
                })
        }

        fn derive_messages(&mut self) -> Vec<Message> {
            let session = self.session.lock().expect("test session lock");
            session
                .events
                .iter()
                .filter_map(|event| match &event.data {
                    SessionEventData::UserMessage(message)
                    | SessionEventData::AssistantMessage(fr_wire::event::AssistantMessageData {
                        message,
                        ..
                    }) if matches!(event.surface_op, Some(SurfaceOp::Append(_))) => {
                        Some(message.clone())
                    }
                    _ => None,
                })
                .collect()
        }

        fn tool_history(&mut self) -> fr_wire::call_config::ToolHistory {
            fr_wire::call_config::ToolHistory {
                tools: Vec::new(),
                updates: Vec::new(),
            }
        }

        fn surface_generation(&mut self) -> u64 {
            let session = self.session.lock().expect("test session lock");
            session
                .events
                .iter()
                .filter(|event| matches!(event.surface_op, Some(SurfaceOp::Replace(_))))
                .count() as u64
        }

        fn system_prompt_nodes(&mut self) -> Vec<crate::runtime_context::SystemPromptNode> {
            let session = self.session.lock().expect("test session lock");
            session
                .events
                .iter()
                .filter_map(|event| match &event.data {
                    SessionEventData::SystemMessage(data)
                        if matches!(event.surface_op, Some(SurfaceOp::Append(_))) =>
                    {
                        Some(crate::runtime_context::SystemPromptNode {
                            seq: event.seq,
                            text: single_text(&data.message),
                        })
                    }
                    _ => None,
                })
                .collect()
        }
    }

    fn surface_seqs(
        events: &[fr_wire::event::SessionEvent],
    ) -> std::collections::HashSet<fr_wire::SessionSeq> {
        events
            .iter()
            .filter(|event| matches!(event.surface_op, Some(SurfaceOp::Append(_))))
            .map(|event| event.seq)
            .collect()
    }

    fn single_text(message: &Message) -> Option<String> {
        let Message::Known(known) = message else {
            return None;
        };
        match known.content().first() {
            Some(fr_wire::ContentBlock::Known(fr_wire::KnownContentBlock::Text { text })) => {
                Some(text.clone())
            }
            _ => None,
        }
    }

    /// The prompt double: one fixed assembly (empty sections, no tools).
    struct FixedPrompt {
        text: &'static str,
    }

    impl SystemPromptFace for FixedPrompt {
        fn assemble(&mut self) -> Result<PromptAssembly, String> {
            Ok(self.assembly())
        }

        fn refresh_context(
            &mut self,
            _assembly: &PromptAssembly,
        ) -> Result<PromptAssembly, String> {
            Ok(self.assembly())
        }
    }

    impl FixedPrompt {
        fn assembly(&self) -> PromptAssembly {
            PromptAssembly {
                rendered_prompt: self.text.to_owned(),
                context_sections: Vec::new(),
                joined_sections: String::new(),
                tools: Vec::new(),
            }
        }
    }

    /// One scripted generation: the chunks buffered at `begin`, with an
    /// optional mid-stream cancel injected after the first drain.
    struct Script {
        chunks: Vec<StreamChunk>,
        cancel_after_first_batch: bool,
        /// A waking input injected right after the cancel (the
        /// waking-after-abort classification probe).
        followup_after_cancel: bool,
    }

    /// The LLM double (Q84-B2 face over scripted generations).
    struct ScriptedLlm {
        session: Arc<Mutex<TestSession>>,
        mailbox: Option<Arc<crate::plugin_shell::SessionMailbox>>,
        scripts: std::collections::VecDeque<Script>,
        prepared: Vec<LlmCallConfig>,
        in_history: bool,
    }

    impl ScriptedLlm {
        fn text_generation(text: &str) -> Script {
            Script {
                chunks: vec![
                    StreamChunk::Known(KnownStreamChunk::TextDelta {
                        index: 0,
                        text: text.to_owned(),
                    }),
                    finish_stop(),
                ],
                cancel_after_first_batch: false,
                followup_after_cancel: false,
            }
        }
    }

    /// One script whose answer requests `count` tool calls (the blocks
    /// the assembler folds into tool-call content).
    fn with_tool_calls(count: usize) -> Script {
        let mut chunks = Vec::new();
        for index in 0..u64::try_from(count).expect("fixture count fits u64") {
            let id = ToolCallId::new(format!("call-{index}"));
            chunks.push(StreamChunk::Known(KnownStreamChunk::BlockStart {
                index,
                block_type: "tool-call".to_owned(),
            }));
            chunks.push(StreamChunk::Known(KnownStreamChunk::ToolCallDelta {
                index,
                id: id.clone(),
                name: Some("bash".to_owned()),
                arguments_delta: "{}".to_owned(),
            }));
            chunks.push(StreamChunk::Known(KnownStreamChunk::BlockEnd {
                index,
                block: fr_wire::ContentBlock::Known(fr_wire::KnownContentBlock::ToolCall {
                    id,
                    name: "bash".to_owned(),
                    arguments: "{}".to_owned(),
                }),
            }));
        }
        chunks.push(StreamChunk::Known(KnownStreamChunk::Finish {
            reason: fr_wire::FinishReason::Known(fr_wire::call_config::FinishReasonKind::ToolCalls),
            replay_state: None,
        }));
        Script {
            chunks,
            cancel_after_first_batch: false,
            followup_after_cancel: false,
        }
    }

    fn finish_stop() -> StreamChunk {
        StreamChunk::Known(KnownStreamChunk::Finish {
            reason: fr_wire::FinishReason::Known(fr_wire::call_config::FinishReasonKind::Stop),
            replay_state: None,
        })
    }

    fn finish_error(message: &str) -> StreamChunk {
        StreamChunk::Known(KnownStreamChunk::Finish {
            reason: fr_wire::FinishReason::Known(fr_wire::call_config::FinishReasonKind::Error {
                failure: fr_wire::Failure {
                    message: message.to_owned(),
                    code: "SERVER".to_owned(),
                    status: Some(500),
                    provider_retry_after_ms: None,
                    request_id: None,
                    offload_images: None,
                },
            }),
            replay_state: None,
        })
    }

    impl super::LlmFace for ScriptedLlm {
        fn prepare_call(&mut self, config: &LlmCallConfig) -> Result<PreparedCall, LlmError> {
            self.prepared.push(config.clone());
            Ok(PreparedCall {
                config: config.clone(),
                model: ResolvedModelInfo {
                    system_prompt_update: self
                        .in_history
                        .then_some(fr_wire::call_config::SystemPromptUpdate::InHistory),
                    ..ResolvedModelInfo::default()
                },
                adapter_defaults: None,
                retry_policy: None,
            })
        }

        fn begin(
            &mut self,
            _request: &GenerateOptions,
            _prepared: Option<&PreparedCall>,
        ) -> Result<String, LlmError> {
            self.session.lock().expect("test session lock").begins += 1;
            let id = format!(
                "g{}",
                self.session.lock().expect("test session lock").begins
            );
            Ok(id)
        }

        fn drain(&mut self, generation: &str) -> Result<DrainedGeneration, LlmError> {
            let index: usize = generation[1..].parse().expect("generation index");
            let Some(script) = self.scripts.get_mut(index - 1) else {
                return Ok(DrainedGeneration::default());
            };
            if script.chunks.is_empty() {
                return Ok(DrainedGeneration {
                    chunks: Vec::new(),
                    done: true,
                });
            }
            let cancel_now = script.cancel_after_first_batch;
            let followup = script.followup_after_cancel;
            let take = if cancel_now { 1 } else { script.chunks.len() };
            let chunks: Vec<StreamChunk> = script.chunks.drain(..take).collect();
            let done = script.chunks.is_empty();
            if cancel_now {
                // Deliver the cancel with the first batch: the driver's
                // next poll applies it mid-stream, after the batch's
                // chunks are already visible.
                if let Some(mailbox) = &self.mailbox {
                    mailbox.enqueue(InboundCommand::Cancel {
                        cause: CancelCause::User,
                        keep_inbox: false,
                    });
                    if followup {
                        mailbox.enqueue(InboundCommand::Input {
                            target: crate::inbox::InboxTarget::NextStep,
                            message: user_message("wake", "after abort"),
                            wakeup: true,
                        });
                    }
                }
                script.cancel_after_first_batch = false;
            }
            Ok(DrainedGeneration { chunks, done })
        }

        fn abort(&mut self, _generation: &str) -> Result<(), LlmError> {
            Ok(())
        }
    }

    /// The tools double: configurable mode; dispatch settles through the
    /// mailbox notification so the scheduler's wait path is real.
    struct ScriptedTools {
        mailbox: Option<Arc<crate::plugin_shell::SessionMailbox>>,
        mode: ToolExecutionMode,
        results: std::collections::VecDeque<ToolExecutionResult>,
        settled: Vec<(ToolCallId, ToolSettlement)>,
        dispatches: Vec<ToolExecution>,
        /// Enqueue a cancel when the first dispatch runs (the tool-abort
        /// probe): later pool calls never start.
        cancel_on_dispatch: bool,
    }

    impl ToolsFace for ScriptedTools {
        fn execution_mode(&mut self, _exec: &ToolExecution) -> ToolExecutionMode {
            self.mode
        }

        fn prepare(&mut self, exec: &ToolExecution) -> Result<ToolPrepared, String> {
            Ok(ToolPrepared::Dispatch { exec: exec.clone() })
        }

        fn dispatch(&mut self, exec: &ToolExecution) -> Result<(), String> {
            self.dispatches.push(exec.clone());
            if self.cancel_on_dispatch {
                self.cancel_on_dispatch = false;
                if let Some(mailbox) = &self.mailbox {
                    mailbox.enqueue(InboundCommand::Cancel {
                        cause: CancelCause::User,
                        keep_inbox: false,
                    });
                }
            }
            let result = self.results.pop_front().unwrap_or(ToolExecutionResult {
                content: vec![fr_wire::ContentBlock::Known(
                    fr_wire::KnownContentBlock::Text {
                        text: "done".to_owned(),
                    },
                )],
                is_error: false,
                error: None,
                meta: None,
                additional_contexts: Vec::new(),
                concludes_turn: false,
            });
            self.settled.push((
                exec.call_id.clone(),
                ToolSettlement {
                    result,
                    needs_post: false,
                },
            ));
            if let Some(mailbox) = &self.mailbox {
                mailbox.notify_tool_settled(exec.call_id.clone());
            }
            Ok(())
        }

        fn settled(&mut self) -> Vec<(ToolCallId, ToolSettlement)> {
            std::mem::take(&mut self.settled)
        }

        fn finalize(
            &mut self,
            _exec: &ToolExecution,
            result: ToolExecutionResult,
        ) -> Result<ToolExecutionResult, String> {
            Ok(result)
        }

        fn finish(
            &mut self,
            _exec: &ToolExecution,
            result: ToolExecutionResult,
        ) -> ToolExecutionResult {
            result
        }
    }

    /// A lock-shared double handle: the driver consumes the boxed face,
    /// the test retains the Arc for inspection.
    struct SharedLlm(Arc<Mutex<ScriptedLlm>>);

    impl super::LlmFace for SharedLlm {
        fn prepare_call(&mut self, config: &LlmCallConfig) -> Result<PreparedCall, LlmError> {
            self.0.lock().expect("llm double lock").prepare_call(config)
        }

        fn begin(
            &mut self,
            request: &GenerateOptions,
            prepared: Option<&PreparedCall>,
        ) -> Result<String, LlmError> {
            self.0
                .lock()
                .expect("llm double lock")
                .begin(request, prepared)
        }

        fn drain(&mut self, generation: &str) -> Result<DrainedGeneration, LlmError> {
            self.0.lock().expect("llm double lock").drain(generation)
        }

        fn abort(&mut self, generation: &str) -> Result<(), LlmError> {
            self.0.lock().expect("llm double lock").abort(generation)
        }
    }

    struct SharedTools(Arc<Mutex<ScriptedTools>>);

    impl ToolsFace for SharedTools {
        fn execution_mode(&mut self, exec: &ToolExecution) -> ToolExecutionMode {
            self.0
                .lock()
                .expect("tools double lock")
                .execution_mode(exec)
        }

        fn prepare(&mut self, exec: &ToolExecution) -> Result<ToolPrepared, String> {
            self.0.lock().expect("tools double lock").prepare(exec)
        }

        fn dispatch(&mut self, exec: &ToolExecution) -> Result<(), String> {
            self.0.lock().expect("tools double lock").dispatch(exec)
        }

        fn settled(&mut self) -> Vec<(ToolCallId, ToolSettlement)> {
            self.0.lock().expect("tools double lock").settled()
        }

        fn finalize(
            &mut self,
            exec: &ToolExecution,
            result: ToolExecutionResult,
        ) -> Result<ToolExecutionResult, String> {
            self.0
                .lock()
                .expect("tools double lock")
                .finalize(exec, result)
        }

        fn finish(
            &mut self,
            exec: &ToolExecution,
            result: ToolExecutionResult,
        ) -> ToolExecutionResult {
            self.0
                .lock()
                .expect("tools double lock")
                .finish(exec, result)
        }
    }

    struct Harness {
        session: Arc<Mutex<TestSession>>,
        mailbox: Arc<crate::plugin_shell::SessionMailbox>,
        driver: LoopDriver,
        tools: Arc<Mutex<ScriptedTools>>,
    }

    fn user_message(id: &str, text: &str) -> Message {
        serde_json::from_value(serde_json::json!({
            "role": "user",
            "id": id,
            "content": [{ "type": "text", "text": text }],
            "source": { "kind": "user" },
        }))
        .expect("fixture message")
    }

    fn harness(scripts: Vec<Script>) -> Harness {
        harness_with(
            scripts,
            ToolExecutionMode::Parallel,
            false,
            Vec::new(),
            10,
            |_| {},
        )
    }

    fn harness_with(
        scripts: Vec<Script>,
        mode: ToolExecutionMode,
        in_history: bool,
        tool_results: Vec<ToolExecutionResult>,
        max_parallel: usize,
        setup: impl FnOnce(&mut AgentEvents),
    ) -> Harness {
        let session = Arc::new(Mutex::new(TestSession::new()));
        let session_handle = Box::into_raw(Box::new(Arc::clone(&session)));
        let outbound = Outbound::new(HostExports {
            host_handle: session_handle.cast::<c_void>(),
            require: serving_require,
            emit: quiet_emit,
        });
        let mut events = AgentEvents::new(outbound, Arc::new(|_warning: &str| {}));
        setup(&mut events);
        let events = Arc::new(events);
        let map = SessionMap::new(outbound);
        let mailbox = session_mailbox(&map, "s1");
        let reads = TestReads {
            session: Arc::clone(&session),
        };
        let next_counter = Arc::new(AtomicU64::new(0));
        let next_id = Box::new(move || {
            fr_wire::MessageId::new(format!(
                "id-{}",
                next_counter.fetch_add(1, Ordering::Relaxed)
            ))
        });
        let tools = Arc::new(Mutex::new(ScriptedTools {
            mailbox: Some(Arc::clone(&mailbox)),
            mode,
            results: tool_results.into(),
            settled: Vec::new(),
            dispatches: Vec::new(),
            cancel_on_dispatch: false,
        }));
        let llm = Arc::new(Mutex::new(ScriptedLlm {
            session: Arc::clone(&session),
            mailbox: Some(Arc::clone(&mailbox)),
            scripts: scripts.into(),
            prepared: Vec::new(),
            in_history,
        }));
        let driver = LoopDriver::new(
            "s1",
            AgentOptions {
                provider: Some("p".to_owned()),
                model: Some("m".to_owned()),
                reasoning_effort: None,
                max_tokens: None,
            },
            Arc::clone(&mailbox),
            Arc::clone(&events),
            DriverDeps {
                reads: Box::new(reads),
                prompt: Box::new(FixedPrompt { text: "system" }),
                llm: Box::new(SharedLlm(Arc::clone(&llm))),
                tools: Box::new(SharedTools(Arc::clone(&tools))),
                clock: Box::new(|| 1_700_000_000_000),
                next_id,
                max_parallel_tool_calls: Box::new(move || max_parallel),
            },
        )
        .expect("driver restores");
        Harness {
            session,
            mailbox,
            driver,
            tools,
        }
    }

    /// Spawns the driver thread; the guard requests shutdown and joins on
    /// drop so a failed assertion cannot strand the thread.
    struct DriverThread {
        mailbox: Arc<crate::plugin_shell::SessionMailbox>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl DriverThread {
        fn spawn(
            mut driver: LoopDriver,
            mailbox: Arc<crate::plugin_shell::SessionMailbox>,
        ) -> Self {
            let handle = std::thread::spawn(move || driver.run());
            Self {
                mailbox,
                handle: Some(handle),
            }
        }
    }

    impl Drop for DriverThread {
        fn drop(&mut self) {
            self.mailbox.request_shutdown();
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn self_events(session: &Arc<Mutex<TestSession>>) -> Vec<String> {
        session
            .lock()
            .expect("test session lock")
            .events
            .iter()
            .map(|event| event.event_type().to_owned())
            .collect()
    }

    /// Waits until the predicate holds over the committed log, or panics
    /// with the deadline message.
    fn wait_for(
        session: &Arc<Mutex<TestSession>>,
        predicate: impl Fn(&TestSession) -> bool,
        what: &str,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if predicate(&session.lock().expect("test session lock")) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let names: Vec<String> = self_events(session);
        panic!("timed out waiting for {what}; events: {names:?}");
    }

    fn turn_end_reason(session: &Arc<Mutex<TestSession>>, index: usize) -> serde_json::Value {
        let locked = session.lock().expect("test session lock");
        let reason =
            serde_json::to_value(&locked.typed("turn/end")[index].data).expect("reason serializes");
        drop(locked);
        reason
    }

    #[test]
    fn full_turn_commits_the_boundary_shape() {
        let harness = harness(vec![ScriptedLlm::text_generation("hello")]);
        let thread = DriverThread::spawn(harness.driver, Arc::clone(&harness.mailbox));
        harness.mailbox.enqueue(InboundCommand::Input {
            target: crate::inbox::InboxTarget::NextTurn,
            message: user_message("m1", "task"),
            wakeup: true,
        });
        wait_for(
            &harness.session,
            |session| !session.typed("turn/end").is_empty(),
            "the turn ends",
        );
        let session = harness.session.lock().expect("test session lock");
        let names: Vec<&str> = session
            .events
            .iter()
            .map(fr_wire::SessionEvent::event_type)
            .collect();
        assert!(names.contains(&"turn/start"));
        assert!(names.contains(&"step/start"));
        assert!(names.contains(&"user/message"));
        assert!(names.contains(&"assistant/message"));
        assert!(names.contains(&"step/end"));
        assert!(names.contains(&"turn/end"));
        drop(session);
        drop(thread);
        assert_eq!(
            turn_end_reason(&harness.session, 0),
            serde_json::json!({ "turn": 1, "reason": { "kind": "completed" } })
        );
    }

    #[test]
    fn step0_empty_turn_completes_without_model_calls() {
        // A pre-step listener that rewrites the batch to empty still owns
        // the initial boundary, but spends no model call (dsh agent.ts
        // 324-329).
        let harness = harness_with(
            Vec::new(),
            ToolExecutionMode::Parallel,
            false,
            Vec::new(),
            10,
            |events| {
                events.on_pre_step(Box::new(|_payload, _next| {
                    crate::agent_events::PreStepDecision::Enter {
                        messages: Vec::new(),
                        starts_request_series: false,
                    }
                }));
            },
        );
        let thread = DriverThread::spawn(harness.driver, Arc::clone(&harness.mailbox));
        harness.mailbox.enqueue(InboundCommand::Input {
            target: crate::inbox::InboxTarget::NextTurn,
            message: user_message("gone", "task"),
            wakeup: true,
        });
        wait_for(
            &harness.session,
            |session| !session.typed("turn/end").is_empty(),
            "the empty turn ends",
        );
        assert_eq!(
            turn_end_reason(&harness.session, 0),
            serde_json::json!({ "turn": 1, "reason": { "kind": "completed" } })
        );
        let session = harness.session.lock().expect("test session lock");
        assert_eq!(session.typed("assistant/message").len(), 0);
        assert_eq!(session.typed("assistant/attempt").len(), 0);
        drop(session);
        drop(thread);
    }

    #[test]
    fn pre_step_reject_ends_the_turn_blocked() {
        let harness = harness_with(
            Vec::new(),
            ToolExecutionMode::Parallel,
            false,
            Vec::new(),
            10,
            |events| {
                events.on_pre_step(Box::new(|_payload, _next| {
                    crate::agent_events::PreStepDecision::Reject
                }));
            },
        );
        let thread = DriverThread::spawn(harness.driver, Arc::clone(&harness.mailbox));
        harness.mailbox.enqueue(InboundCommand::Input {
            target: crate::inbox::InboxTarget::NextTurn,
            message: user_message("m1", "task"),
            wakeup: true,
        });
        wait_for(
            &harness.session,
            |session| !session.typed("turn/end").is_empty(),
            "the rejected turn ends",
        );
        assert_eq!(
            turn_end_reason(&harness.session, 0),
            serde_json::json!({ "turn": 1, "reason": { "kind": "blocked" } })
        );
        let session = harness.session.lock().expect("test session lock");
        assert_eq!(session.typed("step/start").len(), 0);
        drop(session);
        drop(thread);
    }

    #[test]
    fn error_attempt_settles_and_reports_the_failure() {
        let harness = harness(vec![Script {
            chunks: vec![finish_error("boom")],
            cancel_after_first_batch: false,
            followup_after_cancel: false,
        }]);
        let thread = DriverThread::spawn(harness.driver, Arc::clone(&harness.mailbox));
        harness.mailbox.enqueue(InboundCommand::Input {
            target: crate::inbox::InboxTarget::NextTurn,
            message: user_message("m1", "task"),
            wakeup: true,
        });
        wait_for(
            &harness.session,
            |session| !session.typed("turn/end").is_empty(),
            "the failed turn ends",
        );
        let session = harness.session.lock().expect("test session lock");
        assert_eq!(session.typed("assistant/attempt").len(), 1);
        drop(session);
        assert_eq!(
            turn_end_reason(&harness.session, 0),
            serde_json::json!({
                "turn": 1,
                "reason": {
                    "kind": "error",
                    "error": { "message": "boom", "code": "SERVER", "status": 500 }
                }
            })
        );
        drop(thread);
    }

    #[test]
    fn cancel_mid_stream_settles_the_interrupted_prefix() {
        let harness = harness(vec![Script {
            chunks: vec![
                StreamChunk::Known(KnownStreamChunk::TextDelta {
                    index: 0,
                    text: "partial ".to_owned(),
                }),
                StreamChunk::Known(KnownStreamChunk::TextDelta {
                    index: 0,
                    text: "content".to_owned(),
                }),
            ],
            cancel_after_first_batch: true,
            followup_after_cancel: false,
        }]);
        let thread = DriverThread::spawn(harness.driver, Arc::clone(&harness.mailbox));
        harness.mailbox.enqueue(InboundCommand::Input {
            target: crate::inbox::InboxTarget::NextTurn,
            message: user_message("m1", "task"),
            wakeup: true,
        });
        wait_for(
            &harness.session,
            |session| !session.typed("turn/end").is_empty(),
            "the interrupted turn ends",
        );
        let session = harness.session.lock().expect("test session lock");
        let interrupted: Vec<_> = session
            .events
            .iter()
            .filter(|event| event.event_type() == "assistant/message")
            .collect();
        assert_eq!(interrupted.len(), 1);
        let SessionEventData::AssistantMessage(data) = &interrupted[0].data else {
            panic!("assistant message decodes");
        };
        assert_eq!(data.interrupted, Some(fr_wire::TrueOnly));
        drop(session);
        assert_eq!(
            turn_end_reason(&harness.session, 0),
            serde_json::json!({
                "turn": 1,
                "reason": { "kind": "aborted", "reason": { "kind": "user" } }
            })
        );
        drop(thread);
    }

    #[test]
    fn waking_input_after_abort_starts_the_next_turn() {
        let harness = harness(vec![
            Script {
                chunks: vec![StreamChunk::Known(KnownStreamChunk::TextDelta {
                    index: 0,
                    text: "cut".to_owned(),
                })],
                cancel_after_first_batch: true,
                followup_after_cancel: true,
            },
            ScriptedLlm::text_generation("second"),
        ]);
        let thread = DriverThread::spawn(harness.driver, Arc::clone(&harness.mailbox));
        harness.mailbox.enqueue(InboundCommand::Input {
            target: crate::inbox::InboxTarget::NextTurn,
            message: user_message("m1", "task"),
            wakeup: true,
        });
        wait_for(
            &harness.session,
            |session| session.typed("turn/end").len() >= 2,
            "both turns end",
        );
        let session = harness.session.lock().expect("test session lock");
        // The followup was reclassified to next-turn: the second turn's
        // admitted input is the after-abort message.
        let second_user: Vec<&fr_wire::event::SessionEvent> = session
            .events
            .iter()
            .filter(|event| event.event_type() == "user/message")
            .collect();
        assert!(second_user.iter().any(|event| {
            matches!(&event.data, SessionEventData::UserMessage(message)
                if single_text(message).as_deref() == Some("after abort"))
        }));
        drop(session);
        drop(thread);
    }

    #[test]
    fn steer_during_a_turn_feeds_the_next_step() {
        let harness = harness(vec![
            with_tool_calls(1),
            ScriptedLlm::text_generation("done"),
        ]);
        let thread = DriverThread::spawn(harness.driver, Arc::clone(&harness.mailbox));
        harness.mailbox.enqueue(InboundCommand::Input {
            target: crate::inbox::InboxTarget::NextTurn,
            message: user_message("m1", "task"),
            wakeup: true,
        });
        // The steer lands once the first step's tool dispatch parks the
        // driver on its settle wait.
        wait_for(
            &harness.session,
            |session| !session.typed("tool/call").is_empty(),
            "the first tool call lands",
        );
        harness.mailbox.enqueue(InboundCommand::Input {
            target: crate::inbox::InboxTarget::NextStep,
            message: user_message("steer", "mid"),
            wakeup: true,
        });
        wait_for(
            &harness.session,
            |session| !session.typed("turn/end").is_empty(),
            "the turn ends",
        );
        let session = harness.session.lock().expect("test session lock");
        // Two steps: the tool step and the steered continuation.
        assert!(session.typed("step/start").len() >= 2);
        drop(session);
        drop(thread);
    }

    #[test]
    fn tool_abort_records_synthetic_results_for_unstarted_calls() {
        let harness = harness_with(
            vec![with_tool_calls(2)],
            ToolExecutionMode::Parallel,
            false,
            Vec::new(),
            1,
            |_| {},
        );
        // The pool window of one plus a cancel on the first dispatch
        // leaves the second call unstarted: it must receive the
        // synthetic skipped pair.
        let tools = Arc::clone(&harness.tools);
        tools.lock().expect("tools double lock").cancel_on_dispatch = true;
        let thread = DriverThread::spawn(harness.driver, Arc::clone(&harness.mailbox));
        harness.mailbox.enqueue(InboundCommand::Input {
            target: crate::inbox::InboxTarget::NextTurn,
            message: user_message("m1", "task"),
            wakeup: true,
        });
        wait_for(
            &harness.session,
            |session| !session.typed("turn/end").is_empty(),
            "the aborted tool turn ends",
        );
        let session = harness.session.lock().expect("test session lock");
        let results = session.typed("tool/result");
        assert_eq!(results.len(), 2);
        let SessionEventData::ToolResult(skipped) = &results[1].data else {
            panic!("tool result decodes");
        };
        assert!(skipped.error.is_some());
        assert_eq!(
            skipped.error.as_ref().expect("skipped identity").code,
            TOOL_ABORTED_BEFORE_DISPATCH
        );
        drop(session);
        drop(thread);
    }

    #[test]
    fn maintenance_refuses_active_work_and_replays_a_latched_wake() {
        let harness = harness(Vec::new());
        let mailbox = Arc::clone(&harness.mailbox);
        let mut driver = harness.driver;
        // A wake queued before the job latches behind the maintenance
        // phase (the entry applies it) and replays at convergence.
        mailbox.enqueue(InboundCommand::Input {
            target: crate::inbox::InboxTarget::NextTurn,
            message: user_message("m9", "queued"),
            wakeup: true,
        });
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ran_for_job = Arc::clone(&ran);
        let result = driver.run_maintenance(move |_signal| {
            ran_for_job.store(true, Ordering::Relaxed);
            Ok::<(), DriverError>(())
        });
        result.expect("maintenance job runs");
        assert!(ran.load(Ordering::Relaxed));
        // The replayed wake opened a running phase.
        assert_eq!(mailbox.status(), crate::phase::AgentStatus::Running);
        mailbox.request_shutdown();
    }

    #[test]
    fn teardown_converges_when_the_provider_never_delivers() {
        let harness = harness(vec![Script {
            chunks: vec![StreamChunk::Known(KnownStreamChunk::TextDelta {
                index: 0,
                text: "stuck".to_owned(),
            })],
            // The generation never reaches done: the driver parks in the
            // stream wait until the shutdown broadcast aborts it.
            cancel_after_first_batch: false,
            followup_after_cancel: false,
        }]);
        let exited = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let exited_for_thread = Arc::clone(&exited);
        let mailbox = Arc::clone(&harness.mailbox);
        let mut driver = harness.driver;
        let handle = std::thread::spawn(move || {
            driver.run();
            exited_for_thread.store(true, Ordering::Relaxed);
        });
        mailbox.enqueue(InboundCommand::Input {
            target: crate::inbox::InboxTarget::NextTurn,
            message: user_message("m1", "task"),
            wakeup: true,
        });
        wait_for(
            &harness.session,
            |session| !session.typed("step/start").is_empty(),
            "the stuck step opens",
        );
        mailbox.request_shutdown();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !exited.load(Ordering::Relaxed) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            exited.load(Ordering::Relaxed),
            "the driver exits promptly after the shutdown broadcast"
        );
        let _ = handle.join();
    }

    #[test]
    fn repeated_requests_keep_the_unchanged_header() {
        let harness = harness(vec![
            ScriptedLlm::text_generation("one"),
            ScriptedLlm::text_generation("two"),
        ]);
        let thread = DriverThread::spawn(harness.driver, Arc::clone(&harness.mailbox));
        for id in ["m1", "m2"] {
            harness.mailbox.enqueue(InboundCommand::Input {
                target: crate::inbox::InboxTarget::NextTurn,
                message: user_message(id, "task"),
                wakeup: true,
            });
        }
        wait_for(
            &harness.session,
            |session| session.typed("turn/end").len() >= 2,
            "both turns end",
        );
        let session = harness.session.lock().expect("test session lock");
        // Both turns ran to completion, and the unchanged route/tools
        // logged exactly one request/header (the initial).
        assert_eq!(session.typed("turn/end").len(), 2);
        assert_eq!(session.typed("request/header").len(), 1);
        drop(session);
        drop(thread);
    }
}
