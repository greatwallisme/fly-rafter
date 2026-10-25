//! The `LlmAdapter` seam, verbatim to dsh `dsh-llm` (types.ts
//! `GenerateOptions`, index.ts `LlmAdapter`/`PreparedAdapterCall`,
//! call-config.ts).
//!
//! The seam carries the provider trait with its one required method and
//! seven defaults, the request envelope, prepared-call generation binding
//! with the verbatim one-shot and config gates, and call-config equality.
//!
//! dsh splits the adapter-level `PreparedAdapterCall` from the
//! runtime-level `PreparedLlmCall`; the one-shot and
//! `callConfigEquals` gates live on the runtime face there. The runtime
//! registry lands in a later slice, so this seam library carries both gates
//! on [`PreparedAdapterCall`] with the dsh messages and the
//! `INVALID_PREPARED_CALL` code verbatim.
//!
//! dsh's `listModels`/`resolveModel`/`prepareCall` return promises whose
//! base implementations resolve immediately. The three defaults are AFIT
//! `async fn` (Q62B, 2026-10-25) so the signature shape mirrors dsh while
//! the bodies stay immediately-ready; the cost is that the trait is not
//! dyn-compatible, and the runtime registry (a later slice) dispatches
//! through generics or boxed futures.

use crate::error::INVALID_PREPARED_CALL_CODE;
use crate::retry_policy::RetryPolicy;
use fr_wire::call_config::{CallControls, SystemPromptUpdate, ToolUpdate};
use fr_wire::content::ImageAttachmentRef;
use fr_wire::ids::{MessageId, ReasoningEffortId, SessionId};
use fr_wire::message::KnownMessage;
use fr_wire::{ContentBlock, Message, StreamChunk, ToolHistory, ToolSchema};
use futures_core::Stream;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// What one model call is for (dsh `GenerateOptions.purpose`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallPurpose {
    /// A compaction request over a full session.
    Compaction,
    /// A session-title request.
    SessionTitle,
}

impl CallPurpose {
    /// The dsh wire value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compaction => "compaction",
            Self::SessionTitle => "session-title",
        }
    }
}

/// Caller cancellation for one model call: the query face of the dsh
/// `AbortSignal` the seam reads.
#[derive(Clone, Debug, Default)]
pub struct AbortSignal(Arc<AtomicBool>);

impl AbortSignal {
    /// A signal that has not aborted.
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    /// Mark the call as aborted.
    pub fn abort(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether the call was aborted.
    #[must_use]
    pub fn is_aborted(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// One request input: a durable conversation message, or a request-only
/// user input without durable identity (dsh `RequestMessage` = `Message` |
/// `RequestUserInput`).
#[derive(Debug, Clone, PartialEq)]
pub enum RequestMessage {
    /// A durable conversation message.
    Durable(Message),
    /// A request-only user input: role `user`, no id or source.
    UserInput {
        /// The model-facing blocks.
        content: Vec<ContentBlock>,
    },
}

impl RequestMessage {
    /// The model-facing blocks. Unknown-role messages (the forward-compat
    /// `Message::Other` face) report none: projection treats them opaque.
    #[must_use]
    pub fn content_blocks(&self) -> Vec<ContentBlock> {
        match self {
            Self::Durable(Message::Known(known)) => known.content().to_vec(),
            Self::Durable(Message::Other(_)) => Vec::new(),
            Self::UserInput { content } => content.clone(),
        }
    }

    /// Rebuild this message with replacement blocks. Unknown-role messages
    /// pass through unchanged; there is no dsh behavior to mirror inside a
    /// role dsh cannot represent.
    #[must_use]
    pub fn with_content_blocks(&self, content: Vec<ContentBlock>) -> Self {
        match self {
            Self::Durable(Message::Known(known)) => {
                Self::Durable(Message::Known(Box::new(rebuild_known(known, content))))
            }
            Self::Durable(Message::Other(_)) => self.clone(),
            Self::UserInput { .. } => Self::UserInput { content },
        }
    }

    /// The wire `role` tag; `None` for the forward-compat unknown-role face.
    #[must_use]
    pub fn role_name(&self) -> Option<&'static str> {
        match self {
            Self::Durable(Message::Known(known)) => Some(known.role_name()),
            Self::Durable(Message::Other(_)) => None,
            Self::UserInput { .. } => Some("user"),
        }
    }

    /// The durable identity, when this input carries one.
    #[must_use]
    pub fn id(&self) -> Option<MessageId> {
        match self {
            Self::Durable(Message::Known(known)) => Some(known.id().clone()),
            Self::Durable(Message::Other(_)) | Self::UserInput { .. } => None,
        }
    }
}

fn rebuild_known(known: &KnownMessage, content: Vec<ContentBlock>) -> KnownMessage {
    match known {
        KnownMessage::System {
            id, source, extra, ..
        } => KnownMessage::System {
            id: id.clone(),
            content,
            source: source.clone(),
            extra: extra.clone(),
        },
        KnownMessage::User {
            id, source, extra, ..
        } => KnownMessage::User {
            id: id.clone(),
            content,
            source: source.clone(),
            extra: extra.clone(),
        },
        KnownMessage::Assistant {
            id, source, extra, ..
        } => KnownMessage::Assistant {
            id: id.clone(),
            content,
            source: source.clone(),
            extra: extra.clone(),
        },
        KnownMessage::Developer {
            id, source, extra, ..
        } => KnownMessage::Developer {
            id: id.clone(),
            content,
            source: source.clone(),
            extra: extra.clone(),
        },
        KnownMessage::Tool {
            id,
            source,
            tool_call_id,
            is_error,
            extra,
            ..
        } => KnownMessage::Tool {
            id: id.clone(),
            content,
            source: source.clone(),
            tool_call_id: tool_call_id.clone(),
            is_error: *is_error,
            extra: extra.clone(),
        },
    }
}

/// The fully-assembled request an adapter streams (dsh `GenerateOptions`).
#[derive(Debug, Clone)]
pub struct GenerateOptions {
    /// Provider route key.
    pub provider: String,
    /// Model id.
    pub model: String,
    /// Adapter-owned reasoning effort for the exact model.
    pub reasoning_effort: Option<ReasoningEffortId>,
    /// Complete request history.
    pub messages: Vec<RequestMessage>,
    /// One-shot caller system prompt; adapters map it ahead of `messages`.
    pub system: Option<String>,
    /// Complete active tool declarations.
    pub tools: Option<Vec<ToolSchema>>,
    /// Session-folded declaration history for update-capable routes.
    pub tool_history: Option<ToolHistory>,
    /// Sampling temperature.
    pub temperature: Option<f64>,
    /// Per-request output cap.
    pub max_tokens: Option<u64>,
    /// Stop sequences; generation halts on any one.
    pub stop: Option<Vec<String>>,
    /// Caller cancellation; implementations must honor it.
    pub signal: Option<AbortSignal>,
    /// Session identity for replay cursor separation.
    pub session_id: Option<SessionId>,
    /// What this call is for.
    pub purpose: Option<CallPurpose>,
}

/// Provider, model, and sampling scalars of one conversation's requests,
/// plus the value-semantic mapping of dsh's process-local agent-loop
/// request mark (`AGENT_LOOP_REQUESTS` as a `bool` field).
///
/// `PartialEq` is deliberately not derived: equality is
/// [`call_config_equals`], which compares only configuration and never the
/// agent-loop mark.
#[derive(Debug, Clone, Default)]
pub struct LlmCallConfig {
    /// Provider route key.
    pub provider: String,
    /// Model id.
    pub model: String,
    /// Sampling controls.
    pub controls: CallControls,
    /// The agent-loop identity mark.
    pub agent_loop: bool,
}

/// Field-wise equality over [`LlmCallConfig`] (dsh `callConfigEquals`):
/// every field, including the `stop` list element-wise, matches. The
/// agent-loop mark is identity, not configuration, and never compares.
#[must_use]
pub fn call_config_equals(a: &LlmCallConfig, b: &LlmCallConfig) -> bool {
    a.provider == b.provider
        && a.model == b.model
        && a.controls.reasoning_effort == b.controls.reasoning_effort
        && a.controls.temperature == b.controls.temperature
        && a.controls.max_tokens == b.controls.max_tokens
        && a.controls.stop == b.controls.stop
}

/// Mark one call configuration as assembled by the agent loop (dsh
/// `markAgentLoopRequest`, value-semantic mapping).
pub const fn mark_agent_loop_request(config: &mut LlmCallConfig) {
    config.agent_loop = true;
}

/// Whether [`mark_agent_loop_request`] recorded this configuration (dsh
/// `isAgentLoopRequest`).
#[must_use]
pub const fn is_agent_loop_request(config: &LlmCallConfig) -> bool {
    config.agent_loop
}

/// Display metadata for one registered provider route (dsh
/// `LlmProviderInfo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInfo {
    /// Provider route key used by [`GenerateOptions::provider`].
    pub id: String,
    /// Human-readable provider name.
    pub name: String,
}

/// One provider model modality (dsh `ModelModality`); merge-extensible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelModality {
    /// Text input.
    Text,
    /// Image input.
    Image,
    /// An adapter-declared modality beyond the shared vocabulary.
    Other(String),
}

/// One model an adapter can advertise (dsh `LlmModelInfo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmModelInfo {
    /// Provider route that owns this entry.
    pub provider: String,
    /// Model id passed to [`GenerateOptions::model`].
    pub id: String,
    /// Human-readable model name.
    pub name: String,
    /// Optional user-facing distinction from similar models.
    pub description: Option<String>,
    /// Accepted request modalities; `None` means unknown, while an empty
    /// list is negative capability.
    pub input_modalities: Option<Vec<ModelModality>>,
}

/// Provider-owned context capacity for one exact route (dsh
/// `LlmModelContext`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelContext {
    /// Maximum combined request and response context in tokens.
    pub context_window: u64,
}

/// Display metadata for one adapter-owned reasoning effort (dsh
/// `LlmReasoningEffortInfo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningEffortInfo {
    /// Opaque stable value accepted by
    /// [`GenerateOptions::reasoning_effort`].
    pub id: ReasoningEffortId,
    /// Human-readable effort name.
    pub name: String,
    /// Optional user-facing distinction from similar efforts.
    pub description: Option<String>,
}

/// Selectable reasoning efforts for one exact route (dsh
/// `LlmModelReasoningInfo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelReasoningInfo {
    /// Supported efforts, least to greatest selectable reasoning.
    pub efforts: Vec<ReasoningEffortInfo>,
    /// Default materialized when callers omit an effort.
    pub default_effort: Option<ReasoningEffortId>,
}

/// Exact-route model metadata resolved by its owning adapter (dsh
/// `LlmResolvedModelInfo`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResolvedModelInfo {
    /// Provider route that owns this model.
    pub provider: String,
    /// Model id.
    pub id: String,
    /// Human-readable model name.
    pub name: String,
    /// Optional user-facing distinction from similar models.
    pub description: Option<String>,
    /// Accepted request modalities.
    pub input_modalities: Option<Vec<ModelModality>>,
    /// Provider-owned context capacity, when known.
    pub context: Option<ModelContext>,
    /// Output cap materialized when callers omit one.
    pub default_max_tokens: Option<u64>,
    /// Adapter-owned selectable reasoning levels, when exposed.
    pub reasoning: Option<ModelReasoningInfo>,
    /// Mid-conversation system prompt handling.
    pub system_prompt_update: Option<SystemPromptUpdate>,
    /// Mid-conversation tool declaration handling.
    pub tool_update: Option<ToolUpdate>,
}

/// The image dimensions and byte target one adapter sends (dsh
/// `ImageRequestTarget`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageRequestTarget {
    /// Requested width in pixels.
    pub width: u32,
    /// Requested height in pixels.
    pub height: u32,
    /// Encoded byte ceiling for the sent image.
    pub max_bytes: u64,
}

/// One image occurrence's price under one exact route (dsh
/// `LlmImageRequestPrice`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRequestPrice {
    /// Provider visual tokens for a retained occurrence; zero when only
    /// text represents it.
    pub visual_tokens: u64,
    /// Model-visible text sent for the occurrence.
    pub text: String,
}

/// Provider-side request-image pricing for one exact route (dsh
/// `LlmImageRequestPricing`): synchronous, no I/O.
pub trait ImageRequestPricing: Send + Sync {
    /// Price every image occurrence of one request projection, aligned by
    /// index with the image blocks.
    fn price_images(&self, images: &[ContentBlock]) -> Vec<ImageRequestPrice>;
}

/// Why a prepared call refused dispatch; both rejections carry the
/// [`INVALID_PREPARED_CALL_CODE`] code (dsh `INVALID_PREPARED_CALL`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreparedDispatchError {
    /// The call was already dispatched once.
    AlreadyDispatched,
    /// The request config drifted from the prepared config.
    ConfigChanged,
}

impl PreparedDispatchError {
    /// The stable machine-routing code.
    pub const CODE: &'static str = INVALID_PREPARED_CALL_CODE;

    /// The dsh message text.
    #[must_use]
    pub const fn message(&self) -> &'static str {
        match self {
            Self::AlreadyDispatched => "a prepared LLM call can only be dispatched once",
            Self::ConfigChanged => "prepared LLM call config changed before adapter dispatch",
        }
    }

    /// The stable machine-routing code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        Self::CODE
    }
}

impl std::fmt::Display for PreparedDispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for PreparedDispatchError {}

/// The generation-bound stream entry an adapter hands a prepared call.
pub type GenerationStream =
    Arc<dyn Fn(GenerateOptions) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> + Send + Sync>;

/// One adapter-owned model-resolution generation bound to its eventual
/// stream call. Deliberately not `Clone`: a generation handle cannot be
/// copied.
pub struct PreparedAdapterCall {
    /// Exact model metadata from the same generation as [`Self::dispatch`].
    model: ResolvedModelInfo,
    /// The frozen config this generation was prepared with.
    config: LlmCallConfig,
    /// Dispatch through that generation.
    stream: GenerationStream,
    /// The one-shot dispatch gate.
    dispatched: AtomicBool,
}

impl std::fmt::Debug for PreparedAdapterCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedAdapterCall")
            .field("model", &self.model)
            .field("config", &self.config)
            .field("dispatched", &self.dispatched.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl PreparedAdapterCall {
    /// Bind model metadata, the prepared config, and the generation's
    /// stream entry.
    pub fn new(model: ResolvedModelInfo, config: LlmCallConfig, stream: GenerationStream) -> Self {
        Self {
            model,
            config,
            stream,
            dispatched: AtomicBool::new(false),
        }
    }

    /// Exact model metadata from the same adapter generation as dispatch.
    #[must_use]
    pub const fn model(&self) -> &ResolvedModelInfo {
        &self.model
    }

    /// The frozen config this generation was prepared with.
    #[must_use]
    pub const fn config(&self) -> &LlmCallConfig {
        &self.config
    }

    /// Dispatch this call once through its generation. The request's
    /// config fields must match the prepared config; reuse or mismatch
    /// fails with [`PreparedDispatchError`].
    ///
    /// # Errors
    ///
    /// [`PreparedDispatchError::AlreadyDispatched`] on a second dispatch;
    /// [`PreparedDispatchError::ConfigChanged`] when the request config
    /// drifted. A config mismatch leaves the call consumable for a matching
    /// request, exactly as dsh orders its checks.
    pub fn dispatch(
        &self,
        options: GenerateOptions,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamChunk> + Send>>, PreparedDispatchError> {
        if self
            .dispatched
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(PreparedDispatchError::AlreadyDispatched);
        }
        if !call_config_equals(&call_config_of(&options), &self.config) {
            self.dispatched.store(false, Ordering::SeqCst);
            return Err(PreparedDispatchError::ConfigChanged);
        }
        Ok((self.stream)(options))
    }
}

/// The config view of one request (dsh compares the request structurally
/// against the prepared config; the agent-loop mark is identity and never
/// part of the comparison).
fn call_config_of(options: &GenerateOptions) -> LlmCallConfig {
    LlmCallConfig {
        provider: options.provider.clone(),
        model: options.model.clone(),
        controls: CallControls {
            reasoning_effort: options.reasoning_effort.clone(),
            temperature: options.temperature,
            max_tokens: options.max_tokens,
            stop: options.stop.clone(),
        },
        agent_loop: false,
    }
}

/// Provider-wire adapter for the harness message and stream vocabulary
/// (dsh `LlmAdapter`).
///
/// Register implementations with the runtime registry; every provider HTTP
/// request must include the attribution headers from
/// [`crate::attribution::attribution_headers`].
///
/// The three promise-shaped defaults are AFIT `async fn` (Q62B): the auto
/// trait bounds of the returned futures cannot be named, which makes the
/// trait not dyn-compatible — accepted by adjudication, dispatched through
/// generics or boxed futures at the runtime registry.
#[expect(
    async_fn_in_trait,
    reason = "Q62B: signature shape mirrors the dsh promise face; auto-trait-bound naming is consciously waived"
)]
pub trait LlmAdapter: Send + Sync + 'static {
    /// Stream one model call as raw chunks: the only required method.
    /// Implementations must honor `options.signal`.
    fn stream(&self, options: GenerateOptions) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>>;

    /// Describe one provider route owned by this adapter; the id must
    /// equal `provider`.
    fn provider_info(&self, provider: &str) -> ProviderInfo {
        ProviderInfo {
            id: provider.to_owned(),
            name: provider.to_owned(),
        }
    }

    /// The provider-owned retry policy captured with this route; `None`
    /// uses the normal defaults.
    fn provider_retry_policy(&self, _provider: &str) -> Option<RetryPolicy> {
        None
    }

    /// Provider-side request-image pricing for one exact route; `None`
    /// declares none. Must answer synchronously without I/O.
    fn image_request_pricing(
        &self,
        _provider: &str,
        _model: &str,
    ) -> Option<Arc<dyn ImageRequestPricing>> {
        None
    }

    /// The image dimensions and byte target this adapter sends; the
    /// default preserves normalized images.
    fn image_request_target(
        &self,
        _provider: &str,
        _model: &str,
        image: &ImageAttachmentRef,
    ) -> ImageRequestTarget {
        ImageRequestTarget {
            width: image.width,
            height: image.height,
            max_bytes: image.bytes,
        }
    }

    /// Models this adapter can currently advertise for one owned provider,
    /// in adapter-preferred order.
    async fn list_models(&self, _provider: &str) -> Vec<LlmModelInfo> {
        Vec::new()
    }

    /// Resolve all metadata available for one exact model; independent of
    /// the advisory catalog and of request routing.
    async fn resolve_model(&self, provider: &str, model: &str) -> ResolvedModelInfo {
        ResolvedModelInfo {
            provider: provider.to_owned(),
            id: model.to_owned(),
            name: model.to_owned(),
            ..ResolvedModelInfo::default()
        }
    }

    /// Bind exact model metadata and the eventual request dispatch to one
    /// adapter generation: settings changes between preparation and
    /// dispatch cannot combine one generation's capabilities with
    /// another's endpoint.
    async fn prepare_call(self: &Arc<Self>, provider: &str, model: &str) -> PreparedAdapterCall {
        let resolved = self.resolve_model(provider, model).await;
        let config = LlmCallConfig {
            provider: provider.to_owned(),
            model: model.to_owned(),
            controls: CallControls::default(),
            agent_loop: false,
        };
        let adapter = Arc::clone(self);
        PreparedAdapterCall::new(
            resolved,
            config,
            Arc::new(move |options| adapter.stream(options)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fr_wire::call_config::FinishReasonKind;
    use fr_wire::ids::ToolCallId;
    use fr_wire::{FinishReason, KnownStreamChunk};
    use std::sync::atomic::AtomicU64;
    use std::task::{Context, Poll, Waker};

    /// A stream that is instantly done; the gate tests only observe
    /// dispatch results.
    struct EmptyChunks;

    impl Stream for EmptyChunks {
        type Item = StreamChunk;

        fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Poll::Ready(None)
        }
    }

    fn boxed_empty() -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        Box::pin(EmptyChunks)
    }

    struct StaticAdapter;

    impl LlmAdapter for StaticAdapter {
        fn stream(
            &self,
            _options: GenerateOptions,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            boxed_empty()
        }
    }

    /// A dynamic adapter whose generation binds a materialized output cap;
    /// settings changes between preparation and dispatch must not combine
    /// one generation's capabilities with another's.
    struct DynamicAdapter {
        max_tokens: AtomicU64,
    }

    impl LlmAdapter for DynamicAdapter {
        fn stream(
            &self,
            _options: GenerateOptions,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            boxed_empty()
        }

        async fn prepare_call(
            self: &Arc<Self>,
            provider: &str,
            model: &str,
        ) -> PreparedAdapterCall {
            let resolved = self.resolve_model(provider, model).await;
            let config = LlmCallConfig {
                provider: provider.to_owned(),
                model: model.to_owned(),
                controls: CallControls {
                    max_tokens: Some(self.max_tokens.load(Ordering::SeqCst)),
                    ..CallControls::default()
                },
                agent_loop: false,
            };
            let adapter = Arc::clone(self);
            PreparedAdapterCall::new(
                resolved,
                config,
                Arc::new(move |options| adapter.stream(options)),
            )
        }
    }

    /// Minimal executor for the immediately-ready futures the seam's
    /// async defaults produce (no suspension inside); a real executor
    /// lands with the runtime registry slice.
    fn block_on<F: Future>(future: F) -> F::Output {
        let mut cx = Context::from_waker(Waker::noop());
        let mut future = Box::pin(future);
        loop {
            match future.as_mut().poll(&mut cx) {
                Poll::Ready(output) => return output,
                // The seam's async defaults never suspend; Pending would
                // mean a future this minimal executor cannot drive.
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    fn bare_options() -> GenerateOptions {
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

    fn options_with(max_tokens: u64) -> GenerateOptions {
        GenerateOptions {
            max_tokens: Some(max_tokens),
            ..bare_options()
        }
    }

    /// The bound scenario: with two preparations over one session, a
    /// handle from the stale generation cannot dispatch the current
    /// generation's request.
    #[test]
    fn prepare_call_rejects_stale_generation() {
        let adapter = Arc::new(DynamicAdapter {
            max_tokens: AtomicU64::new(100),
        });
        let stale = block_on(adapter.prepare_call("p", "m"));
        adapter.max_tokens.store(200, Ordering::SeqCst);
        let current = block_on(adapter.prepare_call("p", "m"));
        let Err(error) = stale.dispatch(options_with(200)) else {
            panic!("stale generation must not dispatch");
        };
        assert_eq!(error, PreparedDispatchError::ConfigChanged);
        assert_eq!(error.code(), INVALID_PREPARED_CALL_CODE);
        // The stale call stays consumable for its own generation's request.
        drop(
            stale
                .dispatch(options_with(100))
                .expect("matching generation dispatches"),
        );
        drop(
            current
                .dispatch(options_with(200))
                .expect("current generation dispatches"),
        );
    }

    /// The bound scenario: a second dispatch fails with
    /// `INVALID_PREPARED_CALL`, and a config drift fails with the dsh text
    /// without consuming the call.
    #[test]
    fn prepared_call_single_dispatch() {
        let adapter = Arc::new(StaticAdapter);
        let prepared = block_on(adapter.prepare_call("p", "m"));
        drop(prepared.dispatch(bare_options()).expect("first dispatch"));
        let Err(error) = prepared.dispatch(bare_options()) else {
            panic!("second dispatch must fail");
        };
        assert_eq!(error, PreparedDispatchError::AlreadyDispatched);
        assert_eq!(
            error.message(),
            "a prepared LLM call can only be dispatched once"
        );
        let drift = Arc::new(DynamicAdapter {
            max_tokens: AtomicU64::new(100),
        });
        let prepared = block_on(drift.prepare_call("p", "m"));
        let Err(error) = prepared.dispatch(options_with(200)) else {
            panic!("config drift must fail");
        };
        assert_eq!(error, PreparedDispatchError::ConfigChanged);
        assert_eq!(
            error.message(),
            "prepared LLM call config changed before adapter dispatch"
        );
        drop(
            prepared
                .dispatch(options_with(100))
                .expect("matching config dispatches"),
        );
    }

    /// The bound scenario: the purpose vocabulary carries exactly the two
    /// dsh values.
    #[test]
    fn generate_options_purpose_values() {
        let mut compaction = bare_options();
        compaction.purpose = Some(CallPurpose::Compaction);
        let mut title = bare_options();
        title.purpose = Some(CallPurpose::SessionTitle);
        assert_eq!(
            compaction.purpose.map(CallPurpose::as_str),
            Some("compaction")
        );
        assert_eq!(
            title.purpose.map(CallPurpose::as_str),
            Some("session-title")
        );
    }

    /// Equality is field-wise with element-wise `stop`; the agent-loop mark
    /// is identity and never compares.
    #[test]
    fn call_config_equals_compares_stop_element_wise() {
        let base = LlmCallConfig {
            provider: "p".to_owned(),
            model: "m".to_owned(),
            controls: CallControls {
                stop: Some(vec!["a".to_owned(), "b".to_owned()]),
                temperature: Some(0.5),
                ..CallControls::default()
            },
            agent_loop: false,
        };
        let mut marked = base.clone();
        mark_agent_loop_request(&mut marked);
        assert!(is_agent_loop_request(&marked));
        assert!(!is_agent_loop_request(&base));
        // The mark is identity, not configuration.
        assert!(call_config_equals(&base, &marked));
        let reordered = LlmCallConfig {
            controls: CallControls {
                stop: Some(vec!["b".to_owned(), "a".to_owned()]),
                temperature: Some(0.5),
                ..CallControls::default()
            },
            ..base.clone()
        };
        assert!(!call_config_equals(&base, &reordered));
        let unset = LlmCallConfig {
            controls: CallControls {
                stop: None,
                temperature: Some(0.5),
                ..CallControls::default()
            },
            ..base.clone()
        };
        assert!(!call_config_equals(&base, &unset));
    }

    fn chunk_json(value: StreamChunk) -> serde_json::Value {
        serde_json::to_value(value).unwrap_or_else(|_| serde_json::Value::Null)
    }

    fn sorted_keys(chunk: &serde_json::Value) -> Vec<String> {
        let mut names: Vec<String> = chunk
            .as_object()
            .expect("chunk object")
            .keys()
            .cloned()
            .collect();
        names.sort();
        names
    }

    /// The seven chunk variants' serde field names are verbatim to dsh
    /// types.ts:470-482.
    #[test]
    fn stream_chunk_wire_fields_pin_dsh_types() {
        let block_start = chunk_json(StreamChunk::Known(KnownStreamChunk::BlockStart {
            index: 0,
            block_type: "text".to_owned(),
        }));
        assert_eq!(block_start["type"], "block-start");
        assert_eq!(
            sorted_keys(&block_start),
            vec![
                "blockType".to_owned(),
                "index".to_owned(),
                "type".to_owned()
            ]
        );
        let text_delta = chunk_json(StreamChunk::Known(KnownStreamChunk::TextDelta {
            index: 0,
            text: "x".to_owned(),
        }));
        assert_eq!(text_delta["type"], "text-delta");
        assert_eq!(
            sorted_keys(&text_delta),
            vec!["index".to_owned(), "text".to_owned(), "type".to_owned()]
        );
        let reasoning_delta = chunk_json(StreamChunk::Known(KnownStreamChunk::ReasoningDelta {
            index: 0,
            text: "x".to_owned(),
        }));
        assert_eq!(reasoning_delta["type"], "reasoning-delta");
        assert_eq!(
            sorted_keys(&reasoning_delta),
            vec!["index".to_owned(), "text".to_owned(), "type".to_owned()]
        );
        let tool_delta = chunk_json(StreamChunk::Known(KnownStreamChunk::ToolCallDelta {
            index: 0,
            id: ToolCallId::new("c-1"),
            name: Some("bash".to_owned()),
            arguments_delta: "{".to_owned(),
        }));
        assert_eq!(tool_delta["type"], "tool-call-delta");
        assert_eq!(
            sorted_keys(&tool_delta),
            vec![
                "argumentsDelta".to_owned(),
                "id".to_owned(),
                "index".to_owned(),
                "name".to_owned(),
                "type".to_owned()
            ]
        );
        let unnamed = chunk_json(StreamChunk::Known(KnownStreamChunk::ToolCallDelta {
            index: 0,
            id: ToolCallId::new("c-1"),
            name: None,
            arguments_delta: "}".to_owned(),
        }));
        assert!(
            !unnamed
                .as_object()
                .expect("chunk object")
                .contains_key("name")
        );
        let block_end = chunk_json(StreamChunk::Known(KnownStreamChunk::BlockEnd {
            index: 0,
            block: fr_wire::ContentBlock::Known(fr_wire::KnownContentBlock::Text {
                text: "x".to_owned(),
            }),
        }));
        assert_eq!(block_end["type"], "block-end");
        assert_eq!(
            sorted_keys(&block_end),
            vec!["block".to_owned(), "index".to_owned(), "type".to_owned()]
        );
        let usage = chunk_json(StreamChunk::Known(KnownStreamChunk::Usage {
            usage: fr_wire::call_config::TokenUsage::default(),
        }));
        assert_eq!(usage["type"], "usage");
        assert_eq!(
            sorted_keys(&usage),
            vec!["type".to_owned(), "usage".to_owned()]
        );
        let finish = chunk_json(StreamChunk::Known(KnownStreamChunk::Finish {
            reason: FinishReason::Known(FinishReasonKind::Stop),
            replay_state: None,
        }));
        assert_eq!(finish["type"], "finish");
        assert_eq!(
            sorted_keys(&finish),
            vec!["reason".to_owned(), "type".to_owned()]
        );
        let replaying = chunk_json(StreamChunk::Known(KnownStreamChunk::Finish {
            reason: FinishReason::Known(FinishReasonKind::Stop),
            replay_state: Some(fr_wire::ReplayEnvelope::default()),
        }));
        assert_eq!(
            sorted_keys(&replaying),
            vec![
                "reason".to_owned(),
                "replayState".to_owned(),
                "type".to_owned()
            ]
        );
    }
}
