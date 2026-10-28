// Rust guideline compliant 2026-10-28
//! The B2 generation hub: notify-and-pull stream consumption over the
//! dougong dynamic channel (Q84-B2, PRD decision 46).
//!
//! One [`GenerationService`] hosts the per-generation FIFO buffers of
//! every live model call. `begin` freezes the request snapshot, starts
//! the source's stream on a provider-owned worker thread, and returns a
//! generation id immediately — zero network waits inside the service
//! method. Every buffer append (terminal settlement included) emits one
//! tiny [`CHUNK_READY_EVENT`] notification `{sessionId, generationId}`;
//! order, duplication, and loss are all harmless because the semantic
//! content is only "pull now". The consumer wakes and drains one batch
//! non-blockingly through `drain`; `abort` flags the generation and the
//! worker settles per the dsh caller-abort semantics (the adapters'
//! existing signal mapping carries the settlement chunks).
//!
//! Fast-path invariant (PRD decision 46): `prepare_call`/`begin`/`drain`/
//! `abort` are memory-level operations; the network flow lives on the
//! source's worker threads. Method numbers and payload shapes are fr
//! fabric naming (`snake_case` Rust, `camelCase` JSON), not protocol wire.
//!
//! The shells hosting this service: the `fr-llm` artifact (the `llm`
//! service face) and each provider artifact (Q85-C: the same helper
//! hooked per provider shell; routing between the two faces is the
//! runtime-registry slice). Adapter registration over the channel is
//! that slice's protocol, so a fresh service starts with an empty source
//! registry and `prepare_call`/`begin` fail `NO_ADAPTER` until sources
//! are registered (tests register in-process adapters through
//! [`AdapterSource`]).

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::{Context, Poll, Waker};

use fr_wire::content::TrueOnly;
use fr_wire::{AdapterDefaults, StreamChunk};
use futures_core::Stream;

use crate::adapter::{
    AbortSignal, CallPurpose, GenerateOptions, LlmAdapter, LlmCallConfig, ModelModality,
    RequestMessage, ResolvedModelInfo,
};
use crate::adapter::{call_config_equals, call_config_of};
use crate::error::{LlmError, TRANSPORT_CODE};
use crate::retry_policy::{Backoff, RetryPolicy};

/// Method selector: resolve one route and mint a prepared-call binding
/// (`{config}` → [`PrepareCallReply`]).
pub const METHOD_PREPARE_CALL: u32 = 2;

/// Method selector: start one generation (`{request, preparedId?}` →
/// `{generationId}`).
pub const METHOD_BEGIN: u32 = 3;

/// Method selector: pull one buffered batch (`{generationId}` →
/// `{sessionId?, generationId, chunks, done}`).
pub const METHOD_DRAIN: u32 = 4;

/// Method selector: flag one generation cancelled (`{generationId}` →
/// `{}`); the worker settles asynchronously.
pub const METHOD_ABORT: u32 = 5;

/// The notification event every buffer append emits (decision 46); the
/// payload is `{sessionId?, generationId}` — tiny by contract.
pub const CHUNK_READY_EVENT: &str = "llm/chunk-ready";

/// The `llm` service name this vocabulary belongs to (the injected seam
/// PRD 8.1 pins for the loop).
pub const SERVICE_NAME: &str = "llm";

/// The unregistered-route failure code (dsh error taxonomy,
/// `no adapter registered for provider "<p>"` index.ts:1018).
const NO_ADAPTER_CODE: &str = "NO_ADAPTER";

/// Generation id prefix (`gen-<n>`, monotonic per service); fr fabric
/// naming, the dsh `call-<index>` fallback id is the naming precedent.
const GENERATION_PREFIX: &str = "gen-";

/// Prepared-call id prefix (`prep-<n>`, monotonic per service).
const PREPARED_PREFIX: &str = "prep-";

// ---------------------------------------------------------------------------
// Wire payloads (camelCase JSON; the signal field is structurally absent —
// cancellation travels through METHOD_ABORT, never the request snapshot).
// ---------------------------------------------------------------------------

/// The wire face of [`GenerateOptions`] (`signal` structurally absent:
/// the hub owns the abort signal and injects it before dispatch).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireGenerateOptions {
    /// Provider route key.
    pub provider: String,
    /// Model id.
    pub model: String,
    /// Adapter-owned reasoning effort for the exact model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<fr_wire::ReasoningEffortId>,
    /// Complete request history.
    pub messages: Vec<WireRequestMessage>,
    /// One-shot caller system prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Complete active tool declarations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<fr_wire::ToolSchema>>,
    /// Session-folded declaration history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_history: Option<fr_wire::ToolHistory>,
    /// Sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Per-request output cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Stop sequences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    /// Session identity for replay cursor separation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<fr_wire::SessionId>,
    /// What this call is for (`compaction` | `session-title`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
}

/// The wire face of [`RequestMessage`]: internally tagged by `kind`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum WireRequestMessage {
    /// A durable conversation message.
    Durable {
        /// The message itself.
        message: fr_wire::Message,
    },
    /// A request-only user input without durable identity.
    UserInput {
        /// The model-facing blocks.
        content: Vec<fr_wire::ContentBlock>,
    },
}

/// The wire face of [`ResolvedModelInfo`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireModelInfo {
    /// Provider route that owns this model.
    pub provider: String,
    /// Model id.
    pub id: String,
    /// Human-readable model name.
    pub name: String,
    /// Optional user-facing distinction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Accepted request modalities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<WireModality>>,
    /// Context capacity, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<WireModelContext>,
    /// Output cap materialized when callers omit one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_max_tokens: Option<u64>,
    /// Selectable reasoning efforts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<WireModelReasoning>,
    /// Mid-conversation system prompt handling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_update: Option<fr_wire::call_config::SystemPromptUpdate>,
    /// Mid-conversation tool declaration handling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_update: Option<fr_wire::call_config::ToolUpdate>,
}

/// The wire face of [`ModelModality`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WireModality {
    /// Text input.
    Text,
    /// Image input.
    Image,
    /// An adapter-declared modality beyond the shared vocabulary.
    Other(String),
}

/// The wire face of the context capacity (`{contextWindow}`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireModelContext {
    /// Maximum combined request and response context in tokens.
    pub context_window: u64,
}

/// The wire face of the selectable reasoning efforts.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireModelReasoning {
    /// Supported efforts, least to greatest.
    pub efforts: Vec<WireEffortInfo>,
    /// Default materialized when callers omit an effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<fr_wire::ReasoningEffortId>,
}

/// The wire face of one selectable reasoning effort.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireEffortInfo {
    /// Opaque stable value.
    pub id: fr_wire::ReasoningEffortId,
    /// Human-readable effort name.
    pub name: String,
    /// Optional user-facing distinction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The wire face of [`RetryPolicy`] (always explicit on the wire; the
/// values were validated when the policy was resolved).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireRetryPolicy {
    /// `normal` (bounded) or `always`.
    pub mode: String,
    /// Maximum retries per step (`normal` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u64>,
    /// Retryable codes (`normal` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable_codes: Option<Vec<String>>,
    /// Validated backoff facts.
    pub backoff: WireBackoff,
}

/// The wire face of [`Backoff`].
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireBackoff {
    /// Initial delay.
    pub initial_delay_ms: u64,
    /// Delay ceiling.
    pub max_delay_ms: u64,
    /// Symmetric jitter ratio.
    pub jitter_ratio: f64,
}

/// The `prepare_call` request body.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareCallRequest {
    /// Provider route key and optional controls.
    pub config: fr_wire::CallConfig,
}

/// The `prepare_call` reply body.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareCallReply {
    /// The minted binding id `begin` references.
    pub prepared_id: String,
    /// The resolved config (defaults materialized, frozen).
    pub config: fr_wire::CallConfig,
    /// Exact-route model metadata.
    pub model: WireModelInfo,
    /// Effective fields the adapter resolution supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_defaults: Option<AdapterDefaults>,
    /// The route's frozen retry policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_policy: Option<WireRetryPolicy>,
}

/// The `begin` request body.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeginRequest {
    /// The frozen request snapshot (no signal on the wire).
    pub request: WireGenerateOptions,
    /// The prepared-call binding to dispatch through, when prepared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_id: Option<String>,
}

/// The `begin` reply body.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeginReply {
    /// The started generation's id.
    pub generation_id: String,
}

/// The `drain` request body.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DrainRequest {
    /// The generation to pull.
    pub generation_id: String,
}

/// The `drain` reply body.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DrainReply {
    /// The session the generation was started for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The generation these chunks belong to.
    pub generation_id: String,
    /// The buffered chunks, in delivery order.
    pub chunks: Vec<StreamChunk>,
    /// Whether the terminal settlement was delivered.
    pub done: bool,
}

/// The `abort` request body.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbortRequest {
    /// The generation to flag cancelled.
    pub generation_id: String,
}

/// The `llm/chunk-ready` notification payload.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkReadyPayload {
    /// The session the generation was started for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The generation that has buffered output.
    pub generation_id: String,
}

// ---------------------------------------------------------------------------
// Conversions between the seam types and the wire faces.
// ---------------------------------------------------------------------------

impl From<RequestMessage> for WireRequestMessage {
    fn from(message: RequestMessage) -> Self {
        match message {
            RequestMessage::Durable(message) => Self::Durable { message },
            RequestMessage::UserInput { content } => Self::UserInput { content },
        }
    }
}

impl From<WireRequestMessage> for RequestMessage {
    fn from(message: WireRequestMessage) -> Self {
        match message {
            WireRequestMessage::Durable { message } => Self::Durable(message),
            WireRequestMessage::UserInput { content } => Self::UserInput { content },
        }
    }
}

impl From<GenerateOptions> for WireGenerateOptions {
    fn from(options: GenerateOptions) -> Self {
        Self {
            provider: options.provider,
            model: options.model,
            reasoning_effort: options.reasoning_effort,
            messages: options.messages.into_iter().map(Into::into).collect(),
            system: options.system,
            tools: options.tools,
            tool_history: options.tool_history,
            temperature: options.temperature,
            max_tokens: options.max_tokens,
            stop: options.stop,
            // `signal` never crosses the wire: the hub mints the abort
            // signal and injects it before dispatch (B2).
            session_id: options.session_id,
            purpose: options.purpose.map(|purpose| purpose.as_str().to_owned()),
        }
    }
}

impl From<WireGenerateOptions> for GenerateOptions {
    fn from(options: WireGenerateOptions) -> Self {
        Self {
            provider: options.provider,
            model: options.model,
            reasoning_effort: options.reasoning_effort,
            messages: options.messages.into_iter().map(Into::into).collect(),
            system: options.system,
            tools: options.tools,
            tool_history: options.tool_history,
            temperature: options.temperature,
            max_tokens: options.max_tokens,
            stop: options.stop,
            signal: None,
            session_id: options.session_id,
            purpose: options.purpose.and_then(|purpose| match purpose.as_str() {
                "compaction" => Some(CallPurpose::Compaction),
                "session-title" => Some(CallPurpose::SessionTitle),
                _ => None,
            }),
        }
    }
}

impl From<ModelModality> for WireModality {
    fn from(modality: ModelModality) -> Self {
        match modality {
            ModelModality::Text => Self::Text,
            ModelModality::Image => Self::Image,
            ModelModality::Other(value) => Self::Other(value),
        }
    }
}

impl From<WireModality> for ModelModality {
    fn from(modality: WireModality) -> Self {
        match modality {
            WireModality::Text => Self::Text,
            WireModality::Image => Self::Image,
            WireModality::Other(value) => Self::Other(value),
        }
    }
}

impl From<&ResolvedModelInfo> for WireModelInfo {
    fn from(model: &ResolvedModelInfo) -> Self {
        Self {
            provider: model.provider.clone(),
            id: model.id.clone(),
            name: model.name.clone(),
            description: model.description.clone(),
            input_modalities: model
                .input_modalities
                .as_ref()
                .map(|modalities| modalities.iter().cloned().map(Into::into).collect()),
            context: model.context.map(|context| WireModelContext {
                context_window: context.context_window,
            }),
            default_max_tokens: model.default_max_tokens,
            reasoning: model
                .reasoning
                .as_ref()
                .map(|reasoning| WireModelReasoning {
                    efforts: reasoning
                        .efforts
                        .iter()
                        .map(|effort| WireEffortInfo {
                            id: effort.id.clone(),
                            name: effort.name.clone(),
                            description: effort.description.clone(),
                        })
                        .collect(),
                    default_effort: reasoning.default_effort.clone(),
                }),
            system_prompt_update: model.system_prompt_update,
            tool_update: model.tool_update,
        }
    }
}

impl From<WireModelInfo> for ResolvedModelInfo {
    fn from(model: WireModelInfo) -> Self {
        Self {
            provider: model.provider,
            id: model.id,
            name: model.name,
            description: model.description,
            input_modalities: model
                .input_modalities
                .map(|modalities| modalities.into_iter().map(Into::into).collect()),
            context: model.context.map(|context| crate::adapter::ModelContext {
                context_window: context.context_window,
            }),
            default_max_tokens: model.default_max_tokens,
            reasoning: model
                .reasoning
                .map(|reasoning| crate::adapter::ModelReasoningInfo {
                    efforts: reasoning
                        .efforts
                        .into_iter()
                        .map(|effort| crate::adapter::ReasoningEffortInfo {
                            id: effort.id,
                            name: effort.name,
                            description: effort.description,
                        })
                        .collect(),
                    default_effort: reasoning.default_effort,
                }),
            system_prompt_update: model.system_prompt_update,
            tool_update: model.tool_update,
        }
    }
}

impl From<&RetryPolicy> for WireRetryPolicy {
    fn from(policy: &RetryPolicy) -> Self {
        match policy {
            RetryPolicy::Normal {
                max_retries,
                retryable_codes,
                backoff,
            } => Self {
                mode: "normal".to_owned(),
                max_retries: Some(*max_retries),
                retryable_codes: Some(retryable_codes.clone()),
                backoff: backoff.into(),
            },
            RetryPolicy::Always { backoff } => Self {
                mode: "always".to_owned(),
                max_retries: None,
                retryable_codes: None,
                backoff: backoff.into(),
            },
        }
    }
}

impl From<&Backoff> for WireBackoff {
    fn from(backoff: &Backoff) -> Self {
        Self {
            initial_delay_ms: backoff.initial_delay_ms,
            max_delay_ms: backoff.max_delay_ms,
            jitter_ratio: backoff.jitter_ratio,
        }
    }
}

// ---------------------------------------------------------------------------
// Service error face.
// ---------------------------------------------------------------------------

/// One failed `'llm'`-service method call.
///
/// `Llm` failures carry the dsh taxonomy over the channel (the shell
/// replies `STATUS_FAILED` + `{"llmError": <failure>}`); `Text` failures
/// are fabric protocol errors (unknown ids, malformed payloads, shutting
/// down) replied as `{"error": <text>}`.
#[derive(Debug)]
pub enum ServiceError {
    /// A provider-neutral failure with the stable code taxonomy.
    Llm(LlmError),
    /// A fabric protocol failure.
    Text(String),
}

impl From<LlmError> for ServiceError {
    fn from(error: LlmError) -> Self {
        Self::Llm(error)
    }
}

/// Builds one `INVALID_PREPARED_CALL` failure (dsh message verbatim).
fn prepared_call_error(message: &str) -> LlmError {
    LlmError::try_new(
        message,
        crate::error::INVALID_PREPARED_CALL_CODE,
        None,
        None,
        None,
        None,
    )
    .expect("the static prepared-call rejection is impossible")
}

/// Builds the validated transport-arm failure for an outbound host
/// rejection the taxonomy has no finer mapping for.
fn transport_error(message: &str) -> LlmError {
    LlmError::try_new(message, TRANSPORT_CODE, None, None, None, None)
        .expect("the static transport rejection is impossible")
}

// ---------------------------------------------------------------------------
// The notification seam.
// ---------------------------------------------------------------------------

/// Emits one chunk-ready notification; `(session, generation)`. The
/// notifications are lossy by contract, so implementations never fail
/// the caller (Q84-B2: order, duplication, and loss are all harmless).
pub type Notify = Arc<dyn Fn(Option<&str>, &str) + Send + Sync>;

/// A notification sink that drops every notification (tests and the
/// no-vtable arm).
#[must_use]
pub fn quiet_notify() -> Notify {
    Arc::new(|_session: Option<&str>, _generation: &str| {})
}

/// The emit-only view of one copied `HostExports` vtable.
///
/// Publishes [`CHUNK_READY_EVENT`] from any plugin thread (the ABI
/// contract allows concurrent vtable calls; the copy is valid until this
/// activation's `teardown` returns, and the shell joins every worker
/// thread holding a copy before then).
#[derive(Debug, Clone, Copy)]
pub struct EmitVtable {
    host: dougong_abi::HostExports,
}

// SAFETY: the ABI vtable contract states both entries are callable from
// any plugin thread, concurrently; the copy stays valid until this
// activation's `teardown` returns, which the hosting shell guarantees by
// joining the hub's workers first.
unsafe impl Send for EmitVtable {}

// SAFETY: sharing one copy only issues concurrent vtable calls, which the
// same contract explicitly allows.
unsafe impl Sync for EmitVtable {}

impl EmitVtable {
    /// Wraps a vtable copied at `setup`.
    #[must_use]
    pub const fn new(host: dougong_abi::HostExports) -> Self {
        Self { host }
    }

    /// Builds the notification seam publishing [`CHUNK_READY_EVENT`].
    ///
    /// Notification failures are dropped by design: the payload is
    /// advisory ("pull now") and the next append or the step boundary
    /// compensates for any loss.
    #[must_use]
    pub fn notify_seam(&self) -> Notify {
        let emit = *self;
        Arc::new(move |session: Option<&str>, generation: &str| {
            emit.publish(session, generation);
        })
    }

    fn publish(&self, session: Option<&str>, generation: &str) {
        let payload = ChunkReadyPayload {
            session_id: session.map(str::to_owned),
            generation_id: generation.to_owned(),
        };
        let Ok(body) = serde_json::to_vec(&payload) else {
            return;
        };
        let event = CHUNK_READY_EVENT.as_bytes();
        // SAFETY: the copied vtable is live until this activation's
        // `teardown` returns; the shell joins every worker holding a copy
        // before then (dylib thread discipline).
        let _status = unsafe {
            (self.host.emit)(
                self.host.host_handle,
                event.as_ptr(),
                event.len(),
                body.as_ptr(),
                body.len(),
            )
        };
    }
}

// ---------------------------------------------------------------------------
// The per-generation FIFO and the worker-side stream bridge.
// ---------------------------------------------------------------------------

/// One generation's buffered output: the worker appends, the consumer
/// drains. `done` lands in the same batch as the final chunks.
#[derive(Debug, Default)]
struct Fifo {
    state: Mutex<FifoState>,
}

#[derive(Debug, Default)]
struct FifoState {
    chunks: Vec<StreamChunk>,
    done: bool,
}

impl Fifo {
    /// Buffers one chunk.
    fn push(&self, chunk: StreamChunk) {
        if let Ok(mut state) = self.state.lock() {
            state.chunks.push(chunk);
        }
    }

    /// Marks the terminal settlement.
    fn finish(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.done = true;
        }
    }

    /// Takes the buffered batch. The entry itself stays until the
    /// consumer observes `done` (the drain path retires it).
    fn take(&self) -> (Vec<StreamChunk>, bool) {
        match self.state.lock() {
            Ok(mut state) => (std::mem::take(&mut state.chunks), state.done),
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                (std::mem::take(&mut state.chunks), state.done)
            }
        }
    }
}

/// The condvar gate a stream's registered waker signals: `wake` sets the
/// flag and notifies, the driver's wait consumes it. The flag is reset
/// before every poll so a wake racing the poll is never lost.
struct WakeGate {
    notified: Mutex<bool>,
    ready: std::sync::Condvar,
}

impl std::task::Wake for WakeGate {
    fn wake(self: Arc<Self>) {
        if let Ok(mut notified) = self.notified.lock() {
            *notified = true;
        }
        self.ready.notify_all();
    }
}

/// Locks the gate's flag, recovering a poisoned lock: the flag is a bool
/// no panic path mutates mid-operation, so recovery is lossless.
fn lock_notified(gate: &WakeGate) -> std::sync::MutexGuard<'_, bool> {
    gate.notified
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Drives one boxed stream to completion on the calling worker thread:
/// every chunk goes to `on_chunk`, the terminal settlement to
/// `on_finish`. A `Pending` poll parks on the gate's condvar until the
/// stream's registered waker fires (the three first-party streams are
/// channel-backed: their own senders wake on every chunk and on close).
fn drive_stream(
    mut stream: Pin<Box<dyn Stream<Item = StreamChunk> + Send>>,
    mut on_chunk: impl FnMut(StreamChunk),
    on_finish: impl FnOnce(),
) {
    let gate = Arc::new(WakeGate {
        notified: Mutex::new(false),
        ready: std::sync::Condvar::new(),
    });
    let waker = Waker::from(Arc::clone(&gate));
    let mut context = Context::from_waker(&waker);
    loop {
        *lock_notified(&gate) = false;
        match stream.as_mut().poll_next(&mut context) {
            Poll::Ready(Some(chunk)) => on_chunk(chunk),
            Poll::Ready(None) => {
                on_finish();
                return;
            }
            // The flag was armed before the poll; a wake that arrived
            // between the poll and this lock is already visible and the
            // predicate loop re-polls instead of sleeping.
            Poll::Pending => {
                let notified = lock_notified(&gate);
                if !*notified {
                    // The wait releases the lock while parked and regains
                    // it on wake; the guard drops at the arm's end before
                    // the loop re-polls.
                    let _woken = gate
                        .ready
                        .wait_while(notified, |flag| !*flag)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The source seam (the adapter face the service dispatches through).
// ---------------------------------------------------------------------------

/// What one prepared call resolved (dsh `PreparedLlmCall`'s metadata
/// fields).
#[derive(Debug, Clone)]
pub struct PreparedRecord {
    /// Exact-route model metadata from the preparing generation.
    pub model: ResolvedModelInfo,
    /// Effective fields adapter resolution supplied rather than the
    /// caller's proposal.
    pub adapter_defaults: Option<AdapterDefaults>,
    /// The route's frozen retry policy.
    pub retry_policy: Option<RetryPolicy>,
}

/// The chunk-production seam one registered route serves: both methods
/// are memory-level operations — `stream` starts the provider-owned
/// worker (the adapters' `spawn_generation`) and returns immediately.
pub trait GenerationSource: Send + Sync {
    /// Resolves the route for one config (the adapter's `prepare_call`):
    /// exact model metadata plus the adapter-defaults rule.
    ///
    /// # Errors
    ///
    /// Returns the [`LlmError`] the route raised.
    fn prepare(&self, config: &LlmCallConfig) -> Result<(PreparedRecord, LlmCallConfig), LlmError>;

    /// Starts one model call and returns its stream (the network flow
    /// runs on the source's worker threads; implementations must honor
    /// `options.signal`).
    fn stream(&self, options: GenerateOptions) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>>;
}

/// The dsh runtime `prepareCall` default-fill arm (index.ts:976-986):
/// proposed controls the caller left empty are materialized from the
/// model's defaults, and each materialization flags the matching
/// `adapterDefaults` entry. The full `resolveCallWithInfo` validation
/// arm lands with the runtime-registry slice; the adapters' own
/// stream-time validation covers the failure surface meanwhile.
fn materialize_defaults(
    config: &LlmCallConfig,
    model: &ResolvedModelInfo,
) -> (LlmCallConfig, AdapterDefaults) {
    let mut controls = config.controls.clone();
    let mut adapter_defaults = AdapterDefaults::default();
    if controls.reasoning_effort.is_none()
        && let Some(effort) = model
            .reasoning
            .as_ref()
            .and_then(|reasoning| reasoning.default_effort.clone())
    {
        controls.reasoning_effort = Some(effort);
        adapter_defaults.reasoning_effort = Some(TrueOnly);
    }
    if controls.max_tokens.is_none()
        && let Some(cap) = model.default_max_tokens
    {
        controls.max_tokens = Some(cap);
        adapter_defaults.max_tokens = Some(TrueOnly);
    }
    (
        LlmCallConfig {
            provider: config.provider.clone(),
            model: config.model.clone(),
            controls,
            agent_loop: config.agent_loop,
        },
        adapter_defaults,
    )
}

/// The [`GenerationSource`] over any [`LlmAdapter`].
///
/// `prepare` polls the AFIT `prepare_call` future once with the noop
/// waker — the three first-party adapters resolve immediately (local
/// reads, Q62B) — and `stream` delegates to the adapter's trait stream.
pub struct AdapterSource<A: LlmAdapter> {
    adapter: Arc<A>,
}

impl<A: LlmAdapter> AdapterSource<A> {
    /// Wraps one shared adapter.
    #[must_use]
    pub const fn new(adapter: Arc<A>) -> Self {
        Self { adapter }
    }
}

impl<A: LlmAdapter> std::fmt::Debug for AdapterSource<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The adapter face carries no Debug bound; the source identity is
        // the type name plus the shared-pointer address.
        f.debug_struct("AdapterSource")
            .field("adapter", &Arc::as_ptr(&self.adapter))
            .finish()
    }
}

impl<A: LlmAdapter> GenerationSource for AdapterSource<A> {
    fn prepare(&self, config: &LlmCallConfig) -> Result<(PreparedRecord, LlmCallConfig), LlmError> {
        let mut future = std::pin::pin!(self.adapter.prepare_call(&config.provider, &config.model));
        let prepared = match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(prepared) => prepared,
            Poll::Pending => {
                return Err(transport_error(
                    "adapter prepare_call did not resolve synchronously",
                ));
            }
        };
        let model = prepared.model().clone();
        let (resolved_config, adapter_defaults) = materialize_defaults(config, &model);
        Ok((
            PreparedRecord {
                model,
                adapter_defaults: Some(adapter_defaults),
                retry_policy: self.adapter.provider_retry_policy(&config.provider),
            },
            resolved_config,
        ))
    }

    fn stream(&self, options: GenerateOptions) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        self.adapter.stream(options)
    }
}

// ---------------------------------------------------------------------------
// The hub and the service.
// ---------------------------------------------------------------------------

/// One generation the hub tracks until its terminal drain (or shutdown)
/// retires it.
struct Generation {
    session: Option<String>,
    signal: AbortSignal,
    fifo: Arc<Fifo>,
    worker: Option<std::thread::JoinHandle<()>>,
}

/// The B2 generation hub: per-generation FIFO buffers behind memory-level
/// service methods. Hosted by the shells through [`GenerationService`].
struct GenerationHub {
    state: Mutex<HubState>,
    notify: Notify,
    counter: AtomicU64,
}

#[derive(Default)]
struct HubState {
    generations: HashMap<String, Generation>,
    retired: Vec<std::thread::JoinHandle<()>>,
    stopping: bool,
}

impl GenerationHub {
    fn new(notify: Notify) -> Self {
        Self {
            state: Mutex::new(HubState::default()),
            notify,
            counter: AtomicU64::new(0),
        }
    }

    /// Runs one stream to completion on a fresh worker thread, appending
    /// every chunk to the generation's FIFO and notifying per append
    /// (the terminal settlement included).
    fn spawn(
        &self,
        session: Option<String>,
        signal: AbortSignal,
        stream: Pin<Box<dyn Stream<Item = StreamChunk> + Send>>,
    ) -> String {
        let generation_id = format!(
            "{GENERATION_PREFIX}{}",
            self.counter.fetch_add(1, Ordering::Relaxed)
        );
        let fifo = Arc::new(Fifo::default());
        let notify = Arc::clone(&self.notify);
        let notified_id = generation_id.clone();
        let notified_session = session.clone();
        let push_fifo = Arc::clone(&fifo);
        let finish_fifo = Arc::clone(&fifo);
        let spawned = std::thread::Builder::new()
            .name("fr-llm-generation".to_owned())
            .spawn(move || {
                drive_stream(
                    stream,
                    |chunk| {
                        push_fifo.push(chunk);
                        notify(notified_session.as_deref(), &notified_id);
                    },
                    || {
                        finish_fifo.finish();
                        notify(notified_session.as_deref(), &notified_id);
                    },
                );
            });
        match spawned {
            Ok(worker) => {
                let mut state = lock(&self.state);
                state.generations.insert(
                    generation_id.clone(),
                    Generation {
                        session,
                        signal,
                        fifo,
                        worker: Some(worker),
                    },
                );
            }
            Err(error) => {
                // The worker never started: settle the FIFO with the
                // transport failure so the consumer's drain converges
                // instead of waiting for a batch that never comes.
                fifo.push(fr_error_chunk(&transport_error(&format!(
                    "failed to start generation worker thread: {error}"
                ))));
                fifo.finish();
                let mut state = lock(&self.state);
                state.generations.insert(
                    generation_id.clone(),
                    Generation {
                        session,
                        signal,
                        fifo,
                        worker: None,
                    },
                );
            }
        }
        generation_id
    }

    /// Pulls one buffered batch. A batch that observes `done` retires the
    /// generation (the worker already finished; the handle joins at
    /// shutdown).
    fn drain(&self, generation_id: &str) -> Result<DrainReply, ServiceError> {
        let mut state = lock(&self.state);
        let Some(generation) = state.generations.get_mut(generation_id) else {
            return Err(ServiceError::Text(format!(
                "unknown generation \"{generation_id}\""
            )));
        };
        let (chunks, done) = generation.fifo.take();
        let session = generation.session.clone();
        if done
            && let Some(generation) = state.generations.remove(generation_id)
            && let Some(worker) = generation.worker
        {
            state.retired.push(worker);
        }
        drop(state);
        Ok(DrainReply {
            session_id: session,
            generation_id: generation_id.to_owned(),
            chunks,
            done,
        })
    }

    /// Flags one generation cancelled (best effort: unknown and settled
    /// ids are no-ops, matching the loop's best-effort abort).
    fn abort(&self, generation_id: &str) {
        let state = lock(&self.state);
        if let Some(generation) = state.generations.get(generation_id) {
            generation.signal.abort();
        }
    }

    /// Stops new generations, flags every live one cancelled, and joins
    /// every worker (live and retired). The join is unbounded by
    /// adjudication (Q84-B2: detaching races the vtable-lifetime UB; the
    /// adapters' watchdogs and timeouts settle stuck flows themselves).
    fn shutdown(&self) {
        let mut state = lock(&self.state);
        state.stopping = true;
        let mut handles = std::mem::take(&mut state.retired);
        for (_, generation) in state.generations.drain() {
            generation.signal.abort();
            if let Some(worker) = generation.worker {
                handles.push(worker);
            }
        }
        drop(state);
        for handle in handles {
            // A worker that panicked already finished its accounting; the
            // join result is diagnostic only.
            drop(handle.join());
        }
    }

    /// Whether the hub stopped accepting generations.
    fn is_stopping(&self) -> bool {
        lock(&self.state).stopping
    }
}

/// Locks a hub mutex, recovering a poisoned lock: the guarded state is
/// plain data whose invariants no panic path breaks mid-operation.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Builds the adapter-private terminal failure record (Q73-A verbatim:
/// `{"type":"fr-error","message":...,"code":...}`) for a worker that
/// never started.
fn fr_error_chunk(error: &LlmError) -> StreamChunk {
    let failure = error.failure();
    let mut record = serde_json::Map::new();
    record.insert(
        "type".to_owned(),
        serde_json::Value::String("fr-error".to_owned()),
    );
    record.insert(
        "message".to_owned(),
        serde_json::Value::String(failure.message.clone()),
    );
    record.insert(
        "code".to_owned(),
        serde_json::Value::String(failure.code.clone()),
    );
    StreamChunk::Other(record)
}

/// One prepared-call binding (dsh `PreparedLlmCall`'s one-shot dispatch
/// gates, service-side): the registration snapshot travels with the
/// binding, exactly as dsh captures it in the prepared closure.
struct PreparedBinding {
    source: Arc<dyn GenerationSource>,
    config: LlmCallConfig,
    dispatched: AtomicBool,
}

/// The `'llm'`-face service the shells host: the source registry (the
/// runtime-registry slice owns the channel-side registration protocol),
/// the prepared bindings, and the generation hub.
pub struct GenerationService {
    sources: Mutex<HashMap<String, Arc<dyn GenerationSource>>>,
    prepared: Mutex<HashMap<String, Arc<PreparedBinding>>>,
    prepared_counter: AtomicU64,
    hub: GenerationHub,
}

/// The registry size for the Debug face (a poisoned lock still reports).
fn map_len<V>(map: &Mutex<HashMap<String, V>>) -> usize {
    map.lock().map_or(0, |entries| entries.len())
}

impl std::fmt::Debug for GenerationService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GenerationService")
            .field("sources", &map_len(&self.sources))
            .field("prepared", &map_len(&self.prepared))
            .finish_non_exhaustive()
    }
}

impl GenerationService {
    /// Builds the service with the notification seam every buffer append
    /// fires.
    #[must_use]
    pub fn new(notify: Notify) -> Self {
        Self {
            sources: Mutex::new(HashMap::new()),
            prepared: Mutex::new(HashMap::new()),
            prepared_counter: AtomicU64::new(0),
            hub: GenerationHub::new(notify),
        }
    }

    /// Registers one route's source (the channel-side registration
    /// protocol is the runtime-registry slice; in-process registration
    /// is the test and assembly seam).
    pub fn register_source(&self, provider: &str, source: Arc<dyn GenerationSource>) {
        lock(&self.sources).insert(provider.to_owned(), source);
    }

    /// Resolves one route and mints a prepared-call binding (dsh runtime
    /// `prepareCall`).
    ///
    /// # Errors
    ///
    /// [`LlmError`] `NO_ADAPTER` for an unregistered route (dsh
    /// index.ts:1018 verbatim), or the route's own preparation failure.
    pub fn prepare_call(
        &self,
        config: &LlmCallConfig,
    ) -> Result<(String, PreparedRecord, LlmCallConfig), LlmError> {
        let source = self.source_of(&config.provider)?;
        let (record, resolved_config) = source.prepare(config)?;
        let prepared_id = format!(
            "{PREPARED_PREFIX}{}",
            self.prepared_counter.fetch_add(1, Ordering::Relaxed)
        );
        lock(&self.prepared).insert(
            prepared_id.clone(),
            Arc::new(PreparedBinding {
                source,
                config: resolved_config.clone(),
                dispatched: AtomicBool::new(false),
            }),
        );
        Ok((prepared_id, record, resolved_config))
    }

    /// Starts one generation and returns its id.
    ///
    /// # Errors
    ///
    /// [`ServiceError::Llm`] `NO_ADAPTER` for an unprepared,
    /// unregistered route, or `INVALID_PREPARED_CALL` when the binding
    /// was already dispatched or the request config drifted (dsh
    /// index.ts:997-1006 verbatim, ordering included: a config mismatch
    /// leaves the binding consumable for a matching request).
    /// [`ServiceError::Text`] for unknown bindings and the shutting-down
    /// arm.
    pub fn begin(
        &self,
        mut request: GenerateOptions,
        prepared_id: Option<&str>,
    ) -> Result<String, ServiceError> {
        if self.hub.is_stopping() {
            return Err(ServiceError::Text(
                "llm service is shutting down".to_owned(),
            ));
        }
        // The wire carries no signal (B2): the hub mints one, injects it
        // into the request snapshot, and the adapter's existing abort
        // mapping carries the dsh caller-abort settlement.
        let session = request.session_id.as_ref().map(|id| id.as_str().to_owned());
        let signal = AbortSignal::new();
        request.signal = Some(signal.clone());
        let stream = if let Some(prepared_id) = prepared_id {
            let binding = lock(&self.prepared)
                .get(prepared_id)
                .cloned()
                .ok_or_else(|| {
                    ServiceError::Text(format!("unknown prepared call \"{prepared_id}\""))
                })?;
            if binding
                .dispatched
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return Err(ServiceError::Llm(prepared_call_error(
                    "a prepared LLM call can only be dispatched once",
                )));
            }
            if !call_config_equals(&call_config_of(&request), &binding.config) {
                binding.dispatched.store(false, Ordering::SeqCst);
                return Err(ServiceError::Llm(prepared_call_error(
                    "prepared LLM call config changed before adapter dispatch",
                )));
            }
            binding.source.stream(request)
        } else {
            let source = self
                .source_of(&request.provider)
                .map_err(ServiceError::Llm)?;
            source.stream(request)
        };
        Ok(self.hub.spawn(session, signal, stream))
    }

    /// Pulls one buffered batch.
    ///
    /// # Errors
    ///
    /// [`ServiceError::Text`] for an unknown generation.
    pub fn drain(&self, generation_id: &str) -> Result<DrainReply, ServiceError> {
        self.hub.drain(generation_id)
    }

    /// Flags one generation cancelled (best effort; the worker settles
    /// asynchronously per the dsh caller-abort semantics).
    pub fn abort(&self, generation_id: &str) {
        self.hub.abort(generation_id);
    }

    /// Stops the service: no new generations, every live one cancelled,
    /// every worker joined (the teardown discipline the shells call
    /// before returning).
    pub fn shutdown(&self) {
        self.hub.shutdown();
    }

    fn source_of(&self, provider: &str) -> Result<Arc<dyn GenerationSource>, LlmError> {
        lock(&self.sources).get(provider).cloned().ok_or_else(|| {
            LlmError::try_new(
                &format!("no adapter registered for provider \"{provider}\""),
                NO_ADAPTER_CODE,
                None,
                None,
                None,
                None,
            )
            .expect("the static no-adapter rejection is impossible")
        })
    }
}

/// Converts one wire policy back to the seam face. The values were
/// validated when the policy was first resolved; the round trip trusts a
/// first-party service reply.
#[must_use]
pub fn from_wire_retry_policy(policy: &WireRetryPolicy) -> RetryPolicy {
    let backoff = Backoff {
        initial_delay_ms: policy.backoff.initial_delay_ms,
        max_delay_ms: policy.backoff.max_delay_ms,
        jitter_ratio: policy.backoff.jitter_ratio,
    };
    if policy.mode == "always" {
        RetryPolicy::Always { backoff }
    } else {
        RetryPolicy::Normal {
            max_retries: policy.max_retries.unwrap_or(5),
            retryable_codes: policy.retryable_codes.clone().unwrap_or_default(),
            backoff,
        }
    }
}

// ---------------------------------------------------------------------------
// The shell-facing dispatch: one pure function over the service.
// ---------------------------------------------------------------------------

/// Converts one config to its wire face (the agent-loop mark is identity
/// and never crosses the wire).
#[must_use]
pub fn to_wire_call_config(config: &LlmCallConfig) -> fr_wire::CallConfig {
    fr_wire::CallConfig {
        provider: config.provider.clone(),
        model: config.model.clone(),
        controls: config.controls.clone(),
    }
}

/// Converts one wire config to the seam face (no mark on the wire).
#[must_use]
pub fn from_wire_call_config(config: &fr_wire::CallConfig) -> LlmCallConfig {
    LlmCallConfig {
        provider: config.provider.clone(),
        model: config.model.clone(),
        controls: config.controls.clone(),
        agent_loop: false,
    }
}

/// Serves one `'llm'`-face method against the service: a pure
/// (JSON-in, JSON-out) function so every hosting shell shares one
/// dispatch table (the fr-llm shell and the three provider shells,
/// Q85-C).
///
/// # Errors
///
/// [`ServiceError::Llm`] carries the dsh taxonomy (the shell replies
/// `STATUS_FAILED` with `{"llmError": <failure>}`); [`ServiceError::Text`]
/// covers fabric protocol failures (unknown method or ids, malformed
/// payloads, the shutting-down arm) replied as `{"error": <text>}`.
pub fn serve(
    service: &GenerationService,
    method: u32,
    payload: &[u8],
) -> Result<Vec<u8>, ServiceError> {
    match method {
        METHOD_PREPARE_CALL => {
            let request: PrepareCallRequest = decode_payload(method, payload)?;
            let (prepared_id, record, resolved) = service
                .prepare_call(&from_wire_call_config(&request.config))
                .map_err(ServiceError::Llm)?;
            encode_reply(&PrepareCallReply {
                prepared_id,
                config: to_wire_call_config(&resolved),
                model: WireModelInfo::from(&record.model),
                adapter_defaults: record.adapter_defaults,
                retry_policy: record.retry_policy.as_ref().map(Into::into),
            })
        }
        METHOD_BEGIN => {
            let request: BeginRequest = decode_payload(method, payload)?;
            let options = GenerateOptions::from(request.request);
            let generation_id = service.begin(options, request.prepared_id.as_deref())?;
            encode_reply(&BeginReply { generation_id })
        }
        METHOD_DRAIN => {
            let request: DrainRequest = decode_payload(method, payload)?;
            encode_reply(&service.drain(&request.generation_id)?)
        }
        METHOD_ABORT => {
            let request: AbortRequest = decode_payload(method, payload)?;
            service.abort(&request.generation_id);
            encode_reply(&serde_json::json!({}))
        }
        _ => Err(ServiceError::Text(format!(
            "unknown llm service method {method}"
        ))),
    }
}

fn decode_payload<T: serde::de::DeserializeOwned>(
    method: u32,
    payload: &[u8],
) -> Result<T, ServiceError> {
    serde_json::from_slice(payload)
        .map_err(|error| ServiceError::Text(format!("invalid method {method} payload: {error}")))
}

fn encode_reply<T: serde::Serialize>(reply: &T) -> Result<Vec<u8>, ServiceError> {
    serde_json::to_vec(reply)
        .map_err(|error| ServiceError::Text(format!("reply encoding failed: {error}")))
}

#[cfg(test)]
mod tests {
    use super::GenerationSource;
    use super::{
        AbortRequest, AdapterSource, BeginReply, BeginRequest, CHUNK_READY_EVENT, DrainReply,
        DrainRequest, EmitVtable, GenerateOptions, GenerationService, METHOD_ABORT, METHOD_BEGIN,
        METHOD_DRAIN, Notify, PreparedRecord, ServiceError, WireGenerateOptions, WireModelInfo,
        serve,
    };
    use crate::adapter::{
        AbortSignal, LlmAdapter, LlmCallConfig, ModelContext, ModelReasoningInfo,
        PreparedAdapterCall, RequestMessage, ResolvedModelInfo,
    };
    use crate::retry_policy::{Backoff, RetryPolicy};
    use dougong_abi::{HostExports, STATUS_OK};
    use fr_wire::call_config::{FinishReason, FinishReasonKind};
    use fr_wire::{CallControls, StreamChunk};
    use futures_core::Stream;
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Waker};
    use std::thread::sleep;
    use std::time::Duration;

    fn text(text: &str) -> StreamChunk {
        StreamChunk::Known(fr_wire::stream::KnownStreamChunk::TextDelta {
            index: 0,
            text: text.to_owned(),
        })
    }

    fn finish() -> StreamChunk {
        StreamChunk::Known(fr_wire::stream::KnownStreamChunk::Finish {
            reason: FinishReason::Known(FinishReasonKind::Stop),
            replay_state: None,
        })
    }

    /// A stream that yields its chunks immediately (every poll Ready).
    struct LoadedStream {
        chunks: VecDeque<StreamChunk>,
    }

    impl Stream for LoadedStream {
        type Item = StreamChunk;

        fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<StreamChunk>> {
            Poll::Ready(self.chunks.pop_front())
        }
    }

    fn loaded(chunks: Vec<StreamChunk>) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        Box::pin(LoadedStream {
            chunks: chunks.into(),
        })
    }

    /// The channel-backed interactive stream (the three providers'
    /// `ChannelStream` shape: waker registered, then `try_recv`).
    struct TestChannelStream {
        receiver: Receiver<StreamChunk>,
        waker: Arc<Mutex<Option<Waker>>>,
        closed: bool,
    }

    impl Stream for TestChannelStream {
        type Item = StreamChunk;

        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<StreamChunk>> {
            let this = self.get_mut();
            if this.closed {
                return Poll::Ready(None);
            }
            if let Ok(mut slot) = this.waker.lock() {
                *slot = Some(cx.waker().clone());
            }
            match this.receiver.try_recv() {
                Ok(chunk) => Poll::Ready(Some(chunk)),
                Err(TryRecvError::Empty) => Poll::Pending,
                Err(TryRecvError::Disconnected) => {
                    this.closed = true;
                    Poll::Ready(None)
                }
            }
        }
    }

    fn channel_stream() -> (
        Sender<StreamChunk>,
        Pin<Box<dyn Stream<Item = StreamChunk> + Send>>,
    ) {
        let (sender, receiver) = channel();
        let stream = Box::pin(TestChannelStream {
            receiver,
            waker: Arc::new(Mutex::new(None)),
            closed: false,
        });
        (sender, stream)
    }

    fn wake(waker: &Arc<Mutex<Option<Waker>>>) {
        if let Ok(mut slot) = waker.lock()
            && let Some(waker) = slot.take()
        {
            waker.wake();
        }
    }

    /// A stream that ends only when the request's abort signal fires (a
    /// ticker thread re-wakes the registered waker so the drive loop
    /// re-polls after the flag lands; the real adapters' worker threads
    /// play this role).
    struct AbortWatchStream {
        signal: AbortSignal,
        waker: Arc<Mutex<Option<Waker>>>,
        done: bool,
    }

    impl Stream for AbortWatchStream {
        type Item = StreamChunk;

        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<StreamChunk>> {
            let this = self.get_mut();
            if this.done {
                return Poll::Ready(None);
            }
            if this.signal.is_aborted() {
                this.done = true;
                return Poll::Ready(Some(finish()));
            }
            if let Ok(mut slot) = this.waker.lock() {
                *slot = Some(cx.waker().clone());
            }
            Poll::Pending
        }
    }

    /// A source whose streams the test scripts through senders.
    struct ScriptSource {
        streams: Mutex<Vec<Pin<Box<dyn Stream<Item = StreamChunk> + Send>>>>,
    }

    impl ScriptSource {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                streams: Mutex::new(Vec::new()),
            })
        }

        fn push(&self, stream: Pin<Box<dyn Stream<Item = StreamChunk> + Send>>) {
            self.streams.lock().expect("stream queue").push(stream);
        }
    }

    impl super::GenerationSource for ScriptSource {
        fn prepare(
            &self,
            config: &LlmCallConfig,
        ) -> Result<(PreparedRecord, LlmCallConfig), crate::error::LlmError> {
            Ok((
                PreparedRecord {
                    model: model_info(),
                    adapter_defaults: None,
                    retry_policy: None,
                },
                config.clone(),
            ))
        }

        fn stream(
            &self,
            _options: GenerateOptions,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            self.streams
                .lock()
                .expect("stream queue")
                .pop()
                .expect("scripted stream is queued")
        }
    }

    fn model_info() -> ResolvedModelInfo {
        ResolvedModelInfo {
            provider: "p".to_owned(),
            id: "m".to_owned(),
            name: "m".to_owned(),
            context: Some(ModelContext {
                context_window: 128,
            }),
            ..ResolvedModelInfo::default()
        }
    }

    fn request() -> GenerateOptions {
        GenerateOptions {
            provider: "p".to_owned(),
            model: "m".to_owned(),
            reasoning_effort: None,
            messages: Vec::new(),
            system: None,
            tools: None,
            tool_history: None,
            temperature: None,
            max_tokens: None,
            stop: None,
            signal: None,
            session_id: None,
            purpose: None,
        }
    }

    /// Polls `drain` until the terminal batch lands (bounded; the
    /// non-blocking face means tests drive the settle loop the loop
    /// thread parks through).
    fn drain_until_done(service: &GenerationService, id: &str) -> DrainReply {
        let mut chunks = Vec::new();
        for _ in 0..500 {
            let batch = service.drain(id).expect("drain in settle loop");
            chunks.extend(batch.chunks);
            if batch.done {
                return DrainReply {
                    session_id: batch.session_id,
                    generation_id: batch.generation_id,
                    chunks,
                    done: true,
                };
            }
            sleep(Duration::from_millis(2));
        }
        panic!("generation did not settle within the test budget");
    }

    /// The notification log: `(session, generation)` per append.
    type NotifyLog = Arc<Mutex<Vec<(Option<String>, String)>>>;

    fn recording_notify() -> (Notify, NotifyLog) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&log);
        let notify: Notify = Arc::new(move |session: Option<&str>, generation: &str| {
            sink.lock()
                .expect("notify log")
                .push((session.map(str::to_owned), generation.to_owned()));
        });
        (notify, log)
    }

    /// begin/drain round trip: batches keep delivery order, the terminal
    /// batch carries `done`, and the drained generation retires (a later
    /// drain is an unknown id).
    #[test]
    fn begin_drain_round_trips_in_order() {
        let (notify, _log) = recording_notify();
        let service = GenerationService::new(notify);
        let source = ScriptSource::new();
        let (sender, stream) = channel_stream();
        source.push(stream);
        service.register_source("p", source);

        let id = service.begin(request(), None).expect("begin");
        // A drain before any chunk lands answers an empty, not-done batch
        // (notification loss tolerance: the consumer polls freely).
        let first = service.drain(&id).expect("drain before chunks");
        assert_eq!(first.chunks, Vec::<StreamChunk>::new());
        assert!(!first.done);

        sender.send(text("a")).expect("send a");
        sender.send(text("b")).expect("send b");
        sender.send(finish()).expect("send settlement");
        drop(sender);
        let batch = drain_until_done(&service, &id);
        assert_eq!(
            batch.chunks,
            vec![text("a"), text("b"), finish()],
            "terminal batch carries the settlement chunk and done"
        );
        let error = service.drain(&id).expect_err("retired generation");
        assert!(
            matches!(error, ServiceError::Text(ref text) if text.contains("unknown generation"))
        );
    }

    /// Every buffer append emits one notification, the terminal
    /// settlement included (decision 46).
    #[test]
    fn notifications_fire_per_append_and_at_settlement() {
        let (notify, log) = recording_notify();
        let service = GenerationService::new(notify);
        let source = ScriptSource::new();
        source.push(loaded(vec![text("a"), text("b"), finish()]));
        service.register_source("p", source);

        let id = service.begin(request(), None).expect("begin");
        sleep(Duration::from_millis(20));
        let batch = service.drain(&id).expect("drain");
        assert!(batch.done);
        drop(service);
        let (count, all_for_generation) = {
            let log = log.lock().expect("notify log");
            (
                log.len(),
                log.iter().all(|(_, generation)| generation == &id),
            )
        };
        assert_eq!(count, 4, "one notification per append plus settlement");
        assert!(all_for_generation);
    }

    /// The wire carries no signal: the hub mints one and the source sees
    /// it in the request (abort drives the adapter's own settlement).
    /// A source capturing the request's abort signal and ending only on
    /// it (the ticker thread re-wakes the stream's registered waker; the
    /// real adapters' worker threads play this role).
    struct WatchingSource {
        seen: Arc<Mutex<Option<AbortSignal>>>,
    }

    impl super::GenerationSource for WatchingSource {
        fn prepare(
            &self,
            config: &LlmCallConfig,
        ) -> Result<(PreparedRecord, LlmCallConfig), crate::error::LlmError> {
            Ok((
                PreparedRecord {
                    model: ResolvedModelInfo::default(),
                    adapter_defaults: None,
                    retry_policy: None,
                },
                config.clone(),
            ))
        }
        fn stream(
            &self,
            options: GenerateOptions,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            let signal = options.signal.expect("hub injected a signal");
            *self.seen.lock().expect("seen") = Some(signal.clone());
            let waker: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));
            let ticker_waker = Arc::clone(&waker);
            let thread_signal = signal.clone();
            std::thread::spawn(move || {
                loop {
                    if thread_signal.is_aborted() {
                        wake(&ticker_waker);
                        return;
                    }
                    sleep(Duration::from_millis(2));
                }
            });
            Box::pin(AbortWatchStream {
                signal,
                waker,
                done: false,
            })
        }
    }

    #[test]
    fn abort_flags_the_signal_the_source_observes() {
        let (notify, _log) = recording_notify();
        let service = GenerationService::new(notify);
        let seen = Arc::new(Mutex::new(None::<AbortSignal>));
        service.register_source(
            "p",
            Arc::new(WatchingSource {
                seen: Arc::clone(&seen),
            }),
        );

        let id = service.begin(request(), None).expect("begin");
        service.abort(&id);
        service.abort("gen-unknown");
        let batch = drain_until_done(&service, &id);
        assert!(batch.done);
        assert_eq!(batch.chunks, vec![finish()]);
        let observed = seen.lock().expect("seen").clone();
        assert!(observed.expect("signal observed").is_aborted());
    }

    /// Shutdown cancels every live generation, joins the workers, and
    /// refuses new ones.
    #[test]
    fn shutdown_cancels_and_joins_and_refuses() {
        let (notify, _log) = recording_notify();
        let service = GenerationService::new(notify);
        let source = ScriptSource::new();
        let (sender, stream) = channel_stream();
        source.push(stream);
        service.register_source("p", source);
        let id = service.begin(request(), None).expect("begin");
        drop(sender);

        service.shutdown();
        let error = service.begin(request(), None).expect_err("stopped service");
        assert!(matches!(error, ServiceError::Text(ref text) if text.contains("shutting down")));
        // Shutdown is final: the generation entries retire with the join
        // (a host makes no service calls after teardown returns).
        let error = service.drain(&id).expect_err("retired at shutdown");
        assert!(
            matches!(error, ServiceError::Text(ref text) if text.contains("unknown generation"))
        );
    }

    /// The prepared-call gates verbatim: second dispatch refuses, a
    /// config drift refuses and leaves the binding consumable for a
    /// matching request.
    #[test]
    fn prepared_gates_match_dsh_verbatim() {
        let (notify, _log) = recording_notify();
        let service = GenerationService::new(notify);
        let source = ScriptSource::new();
        source.push(loaded(vec![finish()]));
        source.push(loaded(vec![finish()]));
        service.register_source("p", source);

        let config = LlmCallConfig {
            provider: "p".to_owned(),
            model: "m".to_owned(),
            controls: CallControls::default(),
            agent_loop: false,
        };
        let (prepared_id, _record, _resolved) = service.prepare_call(&config).expect("prepare");

        let drifted = GenerateOptions {
            temperature: Some(0.5),
            ..request()
        };
        let error = service
            .begin(drifted, Some(&prepared_id))
            .expect_err("config drift");
        assert!(
            matches!(error, ServiceError::Llm(ref failure) if failure.message()
                == "prepared LLM call config changed before adapter dispatch")
        );

        let id = service
            .begin(request(), Some(&prepared_id))
            .expect("matching dispatch");
        let batch = drain_until_done(&service, &id);
        assert!(batch.done);

        let error = service
            .begin(request(), Some(&prepared_id))
            .expect_err("second dispatch");
        assert!(
            matches!(error, ServiceError::Llm(ref failure) if failure.message()
                == "a prepared LLM call can only be dispatched once")
        );

        let error = service
            .begin(request(), Some("prep-unknown"))
            .expect_err("unknown binding");
        assert!(matches!(error, ServiceError::Text(_)));
    }

    /// The unregistered route refuses with the dsh message and code.
    #[test]
    fn unregistered_route_refuses_no_adapter() {
        let service = GenerationService::new(super::quiet_notify());
        let error = service
            .prepare_call(&LlmCallConfig {
                provider: "missing".to_owned(),
                model: "m".to_owned(),
                controls: CallControls::default(),
                agent_loop: false,
            })
            .expect_err("no source");
        assert_eq!(error.code(), "NO_ADAPTER");
        assert_eq!(
            error.message(),
            "no adapter registered for provider \"missing\""
        );

        let mut options = request();
        options.provider = "missing".to_owned();
        let error = service.begin(options, None).expect_err("bare begin");
        assert!(matches!(error, ServiceError::Llm(ref failure) if failure.code() == "NO_ADAPTER"));
    }

    /// The serve dispatch round-trips the JSON contracts of the trio.
    #[test]
    fn serve_round_trips_the_trio_contracts() {
        let (notify, _log) = recording_notify();
        let service = GenerationService::new(notify);
        let source = ScriptSource::new();
        source.push(loaded(vec![text("a"), finish()]));
        service.register_source("p", source);

        let begin = serde_json::to_vec(&BeginRequest {
            request: WireGenerateOptions::from(request()),
            prepared_id: None,
        })
        .expect("encode begin");
        let reply: BeginReply =
            serde_json::from_slice(&serve(&service, METHOD_BEGIN, &begin).expect("serve begin"))
                .expect("decode begin reply");
        assert!(reply.generation_id.starts_with("gen-"));

        let drain = serde_json::to_vec(&DrainRequest {
            generation_id: reply.generation_id.clone(),
        })
        .expect("encode drain");
        sleep(Duration::from_millis(20));
        let batch: DrainReply =
            serde_json::from_slice(&serve(&service, METHOD_DRAIN, &drain).expect("serve drain"))
                .expect("decode drain reply");
        assert!(batch.done);
        assert_eq!(batch.generation_id, reply.generation_id);
        assert_eq!(batch.chunks.len(), 2);

        let abort = serde_json::to_vec(&AbortRequest {
            generation_id: reply.generation_id,
        })
        .expect("encode abort");
        let body = serve(&service, METHOD_ABORT, &abort).expect("serve abort");
        assert_eq!(body, b"{}");

        let error = serve(&service, 99, b"{}").expect_err("unknown method");
        assert!(
            matches!(error, ServiceError::Text(ref text) if text.contains("unknown llm service method 99"))
        );
        let error = serve(&service, METHOD_BEGIN, b"not json").expect_err("malformed payload");
        assert!(
            matches!(error, ServiceError::Text(ref text) if text.contains("invalid method 3 payload"))
        );
    }

    /// The llm-shaped failure surfaces as the Llm arm (the shell wraps it
    /// as `{"llmError": ...}`).
    #[test]
    fn serve_surfaces_llm_failures_on_the_llm_arm() {
        let service = GenerationService::new(super::quiet_notify());
        let begin = serde_json::to_vec(&BeginRequest {
            request: WireGenerateOptions {
                provider: "missing".to_owned(),
                model: "m".to_owned(),
                reasoning_effort: None,
                messages: Vec::new(),
                system: None,
                tools: None,
                tool_history: None,
                temperature: None,
                max_tokens: None,
                stop: None,
                session_id: None,
                purpose: None,
            },
            prepared_id: None,
        })
        .expect("encode begin");
        let error = serve(&service, METHOD_BEGIN, &begin).expect_err("unregistered route");
        assert!(matches!(error, ServiceError::Llm(ref failure) if failure.code() == "NO_ADAPTER"));
    }

    /// The emit vtable publishes the chunk-ready payload through the
    /// copied `HostExports` emit entry.
    #[test]
    fn emit_vtable_publishes_chunk_ready() {
        // The emit entry is a plain fn: the log travels through the host
        // handle, the FakeSession pattern (session_client.rs judgment).
        type EmitLog = Mutex<Vec<(String, Vec<u8>)>>;
        unsafe extern "C" fn recording_emit(
            host: *mut std::ffi::c_void,
            event: *const u8,
            event_len: usize,
            payload: *const u8,
            payload_len: usize,
        ) -> i32 {
            // SAFETY: the handle aliases the test-owned log for this call.
            let log = unsafe { &*(host.cast::<EmitLog>()) };
            // SAFETY: the vtable contract passes valid readable slices for
            // this call.
            let event = unsafe {
                String::from_utf8_lossy(std::slice::from_raw_parts(event, event_len)).into_owned()
            };
            // SAFETY: same per-call validity.
            let payload = unsafe { std::slice::from_raw_parts(payload, payload_len) }.to_vec();
            log.lock().expect("emit log").push((event, payload));
            STATUS_OK
        }
        let seen: Arc<EmitLog> = Arc::new(Mutex::new(Vec::new()));
        let handle: *const EmitLog = Arc::into_raw(Arc::clone(&seen));
        let vtable = EmitVtable::new(HostExports {
            host_handle: handle.cast_mut().cast::<std::ffi::c_void>(),
            require: unreachable_require,
            emit: recording_emit,
            grant: unreachable_grant,
        });
        vtable.notify_seam()(Some("s1"), "gen-0");
        let log = seen.lock().expect("emit log");
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, CHUNK_READY_EVENT);
        assert_eq!(
            String::from_utf8_lossy(&log[0].1),
            "{\"sessionId\":\"s1\",\"generationId\":\"gen-0\"}"
        );
    }

    unsafe extern "C" fn unreachable_grant(
        _host: *mut std::ffi::c_void,
        _target: *const u8,
        _target_len: usize,
        _granted: *const u8,
        _granted_len: usize,
    ) -> i32 {
        panic!("grant is not part of this test's surface")
    }

    unsafe extern "C" fn unreachable_require(
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
        panic!("emit vtable never requires");
    }

    /// The [`GenerateOptions`] wire face drops the signal and keeps every
    /// other field (round-trip identity).
    #[test]
    fn generate_options_wire_round_trips_without_signal() {
        let options = GenerateOptions {
            provider: "p".to_owned(),
            model: "m".to_owned(),
            reasoning_effort: None,
            messages: vec![
                RequestMessage::Durable(fr_wire::Message::Known(Box::new(
                    fr_wire::KnownMessage::User {
                        id: fr_wire::MessageId::new("msg-1"),
                        content: vec![fr_wire::ContentBlock::Known(
                            fr_wire::KnownContentBlock::Text {
                                text: "hello".to_owned(),
                            },
                        )],
                        source: fr_wire::MessageSource::Known(fr_wire::KnownMessageSource::User {
                            extra: serde_json::Map::default(),
                        }),
                        extra: serde_json::Map::default(),
                    },
                ))),
                RequestMessage::UserInput {
                    content: Vec::new(),
                },
            ],
            system: Some("system".to_owned()),
            tools: None,
            tool_history: None,
            temperature: Some(0.25),
            max_tokens: Some(64),
            stop: Some(vec!["\n".to_owned()]),
            signal: Some(AbortSignal::new()),
            session_id: Some(fr_wire::SessionId::new("s1")),
            purpose: Some(crate::adapter::CallPurpose::Compaction),
        };
        let wire = WireGenerateOptions::from(options.clone());
        assert_eq!(
            serde_json::to_value(&wire).expect("wire json")["sessionId"],
            "s1"
        );
        let back = GenerateOptions::from(wire);
        assert_eq!(back.provider, options.provider);
        assert_eq!(back.messages.len(), 2);
        assert_eq!(back.purpose, options.purpose);
        assert!(
            back.signal.is_none(),
            "the wire face never carries a signal"
        );
    }

    /// The model-info wire face round-trips the metadata the loop's
    /// header derivation consumes.
    #[test]
    fn model_info_wire_round_trips() {
        let model = ResolvedModelInfo {
            provider: "p".to_owned(),
            id: "m".to_owned(),
            name: "Model".to_owned(),
            description: Some("desc".to_owned()),
            input_modalities: Some(vec![crate::adapter::ModelModality::Text]),
            context: Some(ModelContext {
                context_window: 128,
            }),
            default_max_tokens: Some(4096),
            reasoning: Some(ModelReasoningInfo {
                efforts: Vec::new(),
                default_effort: Some(fr_wire::ReasoningEffortId::new("high")),
            }),
            system_prompt_update: None,
            tool_update: None,
        };
        let wire = WireModelInfo::from(&model);
        let json = serde_json::to_value(&wire).expect("wire json");
        assert_eq!(json["defaultMaxTokens"], 4096);
        assert_eq!(json["context"]["contextWindow"], 128);
        let back = ResolvedModelInfo::from(wire);
        assert_eq!(back, model);
    }

    /// The [`AdapterSource`] face: the AFIT `prepare_call` future resolves
    /// under one noop-waker poll, and the adapter-defaults rule
    /// materializes exactly the dsh pair.
    #[test]
    fn adapter_source_materializes_defaults() {
        struct TestAdapter;
        impl LlmAdapter for TestAdapter {
            fn stream(
                &self,
                _options: GenerateOptions,
            ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
                loaded(vec![finish()])
            }

            fn prepare_call(
                self: &Arc<Self>,
                _provider: &str,
                _model: &str,
            ) -> impl Future<Output = PreparedAdapterCall> {
                std::future::ready(PreparedAdapterCall::new(
                    ResolvedModelInfo {
                        provider: "p".to_owned(),
                        id: "m".to_owned(),
                        name: "m".to_owned(),
                        default_max_tokens: Some(4096),
                        reasoning: Some(ModelReasoningInfo {
                            efforts: Vec::new(),
                            default_effort: Some(fr_wire::ReasoningEffortId::new("high")),
                        }),
                        ..ResolvedModelInfo::default()
                    },
                    LlmCallConfig {
                        provider: "p".to_owned(),
                        model: "m".to_owned(),
                        controls: CallControls::default(),
                        agent_loop: false,
                    },
                    Arc::new(|_options| loaded(vec![finish()])),
                ))
            }

            fn provider_retry_policy(&self, _provider: &str) -> Option<RetryPolicy> {
                Some(RetryPolicy::Always {
                    backoff: Backoff {
                        initial_delay_ms: 500,
                        max_delay_ms: 10_000,
                        jitter_ratio: 0.1,
                    },
                })
            }
        }
        let source = AdapterSource::new(Arc::new(TestAdapter));
        let config = LlmCallConfig {
            provider: "p".to_owned(),
            model: "m".to_owned(),
            controls: CallControls::default(),
            agent_loop: false,
        };
        let (record, resolved) = source.prepare(&config).expect("prepare");
        assert_eq!(
            resolved
                .controls
                .reasoning_effort
                .as_ref()
                .map(fr_wire::ReasoningEffortId::as_str),
            Some("high")
        );
        assert_eq!(resolved.controls.max_tokens, Some(4096));
        let defaults = record.adapter_defaults.expect("defaults present");
        assert!(defaults.reasoning_effort.is_some());
        assert!(defaults.max_tokens.is_some());
        assert!(record.retry_policy.is_some());

        // Explicit proposals stay untouched and flag nothing.
        let explicit = LlmCallConfig {
            provider: "p".to_owned(),
            model: "m".to_owned(),
            controls: CallControls {
                reasoning_effort: Some(fr_wire::ReasoningEffortId::new("low")),
                temperature: None,
                max_tokens: Some(16),
                stop: None,
            },
            agent_loop: false,
        };
        let (record, resolved) = source.prepare(&explicit).expect("prepare explicit");
        assert_eq!(
            resolved
                .controls
                .reasoning_effort
                .as_ref()
                .map(fr_wire::ReasoningEffortId::as_str),
            Some("low")
        );
        assert_eq!(resolved.controls.max_tokens, Some(16));
        let defaults = record.adapter_defaults.expect("defaults present");
        assert!(defaults.reasoning_effort.is_none());
        assert!(defaults.max_tokens.is_none());
    }
}
