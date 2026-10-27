//! The `OpenAI`-family provider adapter (dsh `llm-pi-ai` `adapter.ts`):
//! the two wire protocols' execution shell over the hand-declared route
//! profiles.
//!
//! One operation captures the profiles once, refuses what the route
//! cannot serve (the `stop` face, route ownership, the exact model, the
//! reasoning effort, the credential), and runs the full pipeline —
//! history-shape admission, request-image preparation with the base64
//! offload bound, transcript folding, both protocols' conversion, the
//! streaming POST, and SSE translation — on a plugin-owned worker thread
//! (Q69-B) exactly as the dsh generator's async executor does.
//!
//! Error channel (Q73-A): the pi-ai stream delivers failures as terminal
//! error *events*, which settle as one `usage` plus one `finish(kind
//! error|aborted)` chunk pair (the `fail_settle` face); only
//! adapter-level failures (admission refusals, the idle watchdog, caller
//! cancellation) escape as the terminal fr-error record. The outer
//! mapping order mirrors dsh `streamWithSnapshot`'s catch: the watchdog
//! timeout wins, then caller cancellation, then the failure's own face.
//!
//! Branding (Q76-B): the pi-ai diagnostic texts carry the
//! `fr-llm-openai` prefix while every code value stays verbatim.
//!
//! The replay-degrade observation hook of the dsh options is not carried:
//! the fr conversion layers degrade unusable replay state silently (their
//! documented face), so a callback seam here would observe nothing.

use crate::codes::{
    NO_ADAPTER_CODE, UNKNOWN_MODEL_CODE, UNSUPPORTED_CONTENT_CODE, UNSUPPORTED_OPTION_CODE,
    UNSUPPORTED_REASONING_EFFORT_CODE,
};
use crate::compat::{Api, ResolvedCompat, SessionAffinityFormat};
use crate::config::{CacheRetention, ResolvedRouteProfile};
use crate::convert::{ConvertContext, convert_messages};
use crate::error_body::{format_provider_error, normalize as normalize_error_body};
use crate::estimate::clamp_max_tokens_to_context;
use crate::history_transform::{IdNormalization, RouteIdentity, Turn, transform_history};
use crate::models::{LevelMapping, Modality, ModelDescriptor, ThinkingLevel};
use crate::params::{BuildParamsOptions, build_params};
use crate::responses::{
    ResponsesConvertContext, ResponsesParamsOptions, build_params_responses, is_chat_gpt_sign_in,
};
use crate::responses_stream::ResponsesTranslator;
use crate::settle::{Settlement, settle_failure};
use crate::sse::{SseFrame, SseParser};
use crate::transcript::fold_transcript;
use crate::translate::Translator;
use fr_attachment::AttachmentRead;
use fr_llm::adapter::{
    AbortSignal, GenerateOptions, LlmAdapter, LlmCallConfig, LlmModelInfo, ModelContext,
    ModelModality, ModelReasoningInfo, PreparedAdapterCall, ProviderInfo, ReasoningEffortInfo,
    RequestMessage, ResolvedModelInfo,
};
use fr_llm::api_key::{ApiKeyRejection, normalize_api_key};
use fr_llm::attribution::attribution_headers;
use fr_llm::error::{
    ABORTED_CODE, IMAGE_OFFLOAD_REQUIRED_CODE, INVALID_CREDENTIAL_CODE, LlmError,
    MISSING_CREDENTIAL_CODE, TIMEOUT_CODE, TRANSPORT_CODE,
};
use fr_llm::projection::{
    ImageRepresentation, ImageRequestBudget, content_has_image, offloaded_image_text,
    project_offloaded_images, request_image_dimensions, required_image_offload,
};
use fr_net::http::{Fetch, HttpMethod, HttpRequest};
use fr_timeout::{IdleWatchdog, idle_watchdog, timeout_of};
use fr_wire::call_config::{FinishReason, FinishReasonKind, TokenUsage};
use fr_wire::ids::ReasoningEffortId;
use fr_wire::{
    AttachmentId, ContentBlock, ImageAttachmentRef, KnownContentBlock, KnownStreamChunk,
    StreamChunk,
};
use futures_core::Stream;
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::pin::Pin;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

/// The idle-watchdog's recorded reason code (dsh
/// `'LLM_STREAM_IDLE_TIMEOUT'` verbatim).
const LLM_STREAM_IDLE_TIMEOUT: &str = "LLM_STREAM_IDLE_TIMEOUT";
/// The terminal error record's type tag (Q73-A; adapter-private value
/// domain of the merge-extensible `StreamChunk::Other` face).
const FR_ERROR_TYPE: &str = "fr-error";
/// The read window of the response body loop: deadline checks and
/// activity pulses happen at most one window apart.
const READ_WINDOW_BYTES: usize = 8192;
/// The completions resource path (npm SDK `resources/chat/completions`).
const CHAT_COMPLETIONS_PATH: &str = "/chat/completions";
/// The responses resource path (npm SDK `resources/responses`).
const RESPONSES_PATH: &str = "/responses";
/// The SDK connection-failure message the settle face classifies
/// (`APIConnectionError`, verbatim; fr-net's own texts never surface).
const CONNECTION_ERROR_MESSAGE: &str = "Connection error.";
/// The SDK request-timeout message (`APIConnectionTimeoutError`,
/// verbatim), the `timeout-ms` deadline's settle face.
const REQUEST_TIMED_OUT_MESSAGE: &str = "Request timed out.";
/// The pi-ai terminal-gate abort throw (completions.ts:687 verbatim).
const REQUEST_ABORTED_MESSAGE: &str = "Request was aborted";
/// The ChatGPT-usage hint appended to the subscription-sharing quota
/// failure (responses.ts:34 `CHATGPT_USAGE_URL` verbatim).
const CHATGPT_USAGE_HINT: &str = "\nCheck your ChatGPT usage: https://chatgpt.com/settings/usage";
/// The ambient cache-retention override knob (pi-ai `PI_CACHE_RETENTION`,
/// a provider-side env name kept verbatim by the D10 env-only face).
const PI_CACHE_RETENTION_ENV: &str = "PI_CACHE_RETENTION";

/// The current validated profiles by provider route (dsh
/// `PiAiAdapterOptions.profiles`: called once per operation).
pub type ProfilesCallback = Arc<dyn Fn() -> BTreeMap<String, ResolvedRouteProfile> + Send + Sync>;

/// Resolve the credential for one already-resolved profile (dsh
/// `PiAiAdapterOptions.resolveApiKey`): `None` defers to the ambient
/// header face, while a named reference that misses fails loud.
pub type ResolveApiKeyCallback =
    Arc<dyn Fn(&str, &ResolvedRouteProfile) -> Result<Option<String>, LlmError> + Send + Sync>;

/// Bridge one attachment reference into the execution world's access
/// facts (dsh `PiAiAdapterOptions.resolveImageAccess`).
pub type ImageAccessCallback = Arc<
    dyn Fn(&ImageAttachmentRef) -> Option<fr_llm::projection::ImageAttachmentAccess> + Send + Sync,
>;

/// The adapter's wall clock (request-deadline arithmetic).
pub type NowMsCallback = Arc<dyn Fn() -> u64 + Send + Sync>;

/// Constructor options for [`OpenAiAdapter`] (dsh `PiAiAdapterOptions`
/// restricted to the fr M1 face: the credential store and auth context
/// are the env-only seam, D10).
#[derive(Clone)]
pub struct OpenAiAdapterOptions {
    /// Current validated profiles by provider route; called once per
    /// operation and frozen for that operation.
    pub profiles: ProfilesCallback,
    /// The credential resolution, once per stream call.
    pub resolve_api_key: ResolveApiKeyCallback,
    /// The transport seam every provider HTTP call goes through.
    pub fetch: Arc<dyn Fetch>,
    /// The optional durable attachment service, resolved at request time.
    pub attachments: Option<Arc<dyn AttachmentRead + Send + Sync>>,
    /// The offloaded-image access facts for placeholder text.
    pub image_access: Option<ImageAccessCallback>,
    /// The adapter's wall clock.
    pub now_ms: NowMsCallback,
}

impl std::fmt::Debug for OpenAiAdapterOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiAdapterOptions")
            .field("profiles", &"<callback>")
            .field("resolve_api_key", &"<callback>")
            .field("fetch", &"<Fetch>")
            .field("attachments", &self.attachments.is_some())
            .field("image_access", &self.image_access.is_some())
            .field("now_ms", &"<callback>")
            .finish()
    }
}

/// The env-only credential arm over an injectable environment (D10; dsh
/// `resolveApiKey`, environment face).
///
/// A named reference resolves through the launch environment, a miss
/// fails loud, and a supplied key must pass the shared header-safety
/// check.
///
/// # Errors
/// [`MISSING_CREDENTIAL_CODE`] when a named reference resolves to
/// nothing, and [`INVALID_CREDENTIAL_CODE`] for a blank or header-unsafe
/// key (texts adapted per the Q66-B precedent).
pub fn resolve_env_api_key(
    env: &dyn Fn(&str) -> Option<String>,
    provider: &str,
    profile: &ResolvedRouteProfile,
) -> Result<Option<String>, LlmError> {
    // A profile naming no credential at all defers to the ambient header
    // face (getClientApiKey decides below).
    let Some(reference) = profile.api_key_env.as_deref() else {
        return Ok(None);
    };
    let Some(hit) = env(reference).filter(|value| !value.is_empty()) else {
        return Err(missing_credential(provider, reference));
    };
    assert_usable_api_key(&hit, reference).map(Some)
}

/// `assertUsableApiKey` (dsh dsh-llm `index.ts:151-163`) with the fr
/// adaptation of the writer hints (Q66-B judgment: the web Models page
/// reads as the fly-rafter configuration).
fn assert_usable_api_key(raw: &str, reference: &str) -> Result<String, LlmError> {
    normalize_api_key(raw).map_err(|rejection| {
        let message = match rejection {
            ApiKeyRejection::Empty => format!(
                "fr-llm-openai: the API key resolved from {reference} is blank; set {reference} \
                 to the raw key in the fly-rafter configuration, or export it in the launching \
                 environment"
            ),
            ApiKeyRejection::IllegalCharacters => format!(
                "fr-llm-openai: the API key resolved from {reference} contains characters no \
                 HTTP header can carry; set {reference} to the raw key alone in the \
                 fly-rafter configuration"
            ),
        };
        static_error(&message, INVALID_CREDENTIAL_CODE)
    })
}

/// The named-reference miss (dsh `index.ts:205-211` adapted: the
/// credentials-service writer hint reads as the configuration face).
fn missing_credential(provider: &str, reference: &str) -> LlmError {
    static_error(
        &format!(
            "fr-llm-openai: no credential for provider route \"{provider}\"; its profile \
             resolves {reference}, which is not set — set {reference} in the fly-rafter \
             configuration, or export it in the launching environment, and remove api-key-env \
             only if this provider should authenticate from the ambient environment"
        ),
        MISSING_CREDENTIAL_CODE,
    )
}

/// The per-operation frozen plan: one profile, its exact model, the
/// validated reasoning effort, and the resolved credential (dsh captures
/// the same facts from one immutable snapshot before its first await).
struct RequestPlan {
    profile: ResolvedRouteProfile,
    model: ModelDescriptor,
    /// `None` is `off` (dsh `profileOptions` maps `off` to `undefined`).
    effort: Option<ThinkingLevel>,
    api_key: Option<String>,
}

/// Parse one reasoning-effort id into its pi-ai level (the seam's opaque
/// string domain meets the seven-level vocabulary).
fn parse_level(raw: &str) -> Option<ThinkingLevel> {
    ThinkingLevel::ALL
        .iter()
        .copied()
        .find(|level| level.as_str() == raw)
}

/// The selectable levels of one model (dsh `getSupportedThinkingLevels`):
/// a pinned-null level is unsupported, a declared-valueless `off` (the
/// fr `SendNothing`, dsh's absent entry) stays selectable, `xhigh`/`max`
/// need an explicit wire spelling, and a non-reasoning model offers
/// `off` alone.
fn supported_levels(model: &ModelDescriptor) -> Vec<ThinkingLevel> {
    if !model.reasoning {
        return vec![ThinkingLevel::Off];
    }
    ThinkingLevel::ALL
        .iter()
        .copied()
        .filter(|level| {
            matches!(
                model
                    .thinking_level_map
                    .as_ref()
                    .and_then(|map| map.get(level)),
                Some(LevelMapping::SendNothing | LevelMapping::Wire(_))
            )
        })
        .collect()
}

/// Validate the merged effort without pi-ai's clamp (dsh
/// `resolveReasoningLevel`): the harness effort wins over the profile
/// default, an unsupported or unparseable level refuses with the verbatim
/// message, and `off` maps to the no-reasoning dispatch.
fn resolve_reasoning_level(
    model: &ModelDescriptor,
    effort: Option<&ReasoningEffortId>,
    profile_default: Option<ThinkingLevel>,
) -> Result<Option<ThinkingLevel>, LlmError> {
    let merged = effort.map_or(profile_default, |raw| parse_level(raw.as_str()));
    let Some(level) = merged else {
        return Ok(None);
    };
    if !supported_levels(model).contains(&level) {
        return Err(static_error(
            &format!(
                "fr-llm-openai provider \"{}\" model \"{}\" does not support reasoning effort \
                 \"{}\"",
                model.provider,
                model.id,
                effort.map_or(level.as_str(), ReasoningEffortId::as_str)
            ),
            UNSUPPORTED_REASONING_EFFORT_CODE,
        ));
    }
    Ok((level != ThinkingLevel::Off).then_some(level))
}

/// The profile default this exact model can actually take, for
/// describing it (dsh `describableReasoningLevel`: silent `None` instead
/// of the request-path refusal).
fn describable_level(
    model: &ModelDescriptor,
    profile_default: Option<ThinkingLevel>,
) -> Option<ThinkingLevel> {
    profile_default.filter(|level| supported_levels(model).contains(level))
}

/// The selectable-efforts metadata (dsh `reasoningInfo`): a non-reasoning
/// model reports no reasoning field at all — offering a lone `off` would
/// advertise a control byte-identical to naming no effort.
fn reasoning_info(
    model: &ModelDescriptor,
    default_level: Option<ThinkingLevel>,
) -> Option<ModelReasoningInfo> {
    if !model.reasoning {
        return None;
    }
    let efforts = supported_levels(model)
        .into_iter()
        .map(|level| ReasoningEffortInfo {
            id: ReasoningEffortId::new(level.as_str()),
            name: capitalize(level.as_str()),
            description: None,
        })
        .collect();
    Some(ModelReasoningInfo {
        efforts,
        default_effort: default_level.map(|level| ReasoningEffortId::new(level.as_str())),
    })
}

/// `${word.charAt(0).toUpperCase()}${word.slice(1)}`.
fn capitalize(word: &str) -> String {
    let mut characters = word.chars();
    characters.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + characters.as_str()
    })
}

/// The exact-route metadata (dsh `modelInfo`): the configured output cap
/// alone is a request default, and the update modes are absent (Q74-B
/// baseline folding).
fn model_info(profile: &ResolvedRouteProfile, model: &ModelDescriptor) -> ResolvedModelInfo {
    let default_level = describable_level(model, profile.reasoning);
    ResolvedModelInfo {
        provider: model.provider.clone(),
        id: model.id.clone(),
        name: model.name.clone(),
        description: None,
        input_modalities: Some(modality_faces(&model.input)),
        context: Some(ModelContext {
            context_window: model.context_window,
        }),
        default_max_tokens: profile
            .catalog
            .configured_max_tokens
            .get(&model.id)
            .copied(),
        reasoning: reasoning_info(model, default_level),
        system_prompt_update: None,
        tool_update: None,
    }
}

fn modality_faces(input: &[Modality]) -> Vec<ModelModality> {
    input
        .iter()
        .map(|modality| match modality {
            Modality::Text => ModelModality::Text,
            Modality::Image => ModelModality::Image,
        })
        .collect()
}

/// The `OpenAI`-family provider adapter over the hand-declared route
/// profiles (dsh `PiAiAdapter`).
pub struct OpenAiAdapter {
    options: OpenAiAdapterOptions,
}

impl OpenAiAdapter {
    /// Bind the adapter's operation seams.
    #[must_use]
    pub const fn new(options: OpenAiAdapterOptions) -> Self {
        Self { options }
    }

    /// One operation's plan from a frozen profiles view (dsh
    /// `profileOf`/`modelOf`/`resolveReasoningLevel`/`resolveApiKey`, in
    /// the dsh order).
    fn plan_of(
        options: &OpenAiAdapterOptions,
        profiles: &BTreeMap<String, ResolvedRouteProfile>,
        request: &GenerateOptions,
    ) -> Result<RequestPlan, LlmError> {
        if request.stop.is_some() {
            return Err(static_error(
                "fr-llm-openai does not support GenerateOptions.stop",
                UNSUPPORTED_OPTION_CODE,
            ));
        }
        let profile = profiles.get(&request.provider).ok_or_else(|| {
            static_error(
                &format!(
                    "fr-llm-openai adapter does not own provider \"{}\"",
                    request.provider
                ),
                NO_ADAPTER_CODE,
            )
        })?;
        // The dsh deferred-mode `modelErrors` face has no fr arm: this
        // crate validates profiles strictly, so the only failure left is
        // the undeclared model itself.
        let model = profile
            .catalog
            .models
            .iter()
            .find(|model| model.id == request.model)
            .ok_or_else(|| {
                static_error(
                    &format!(
                        "fr-llm-openai provider \"{}\" has no configured model \"{}\"",
                        request.provider, request.model
                    ),
                    UNKNOWN_MODEL_CODE,
                )
            })?
            .clone();
        let effort =
            resolve_reasoning_level(&model, request.reasoning_effort.as_ref(), profile.reasoning)?;
        let api_key = (options.resolve_api_key)(&request.provider, profile)?;
        Ok(RequestPlan {
            profile: profile.clone(),
            model,
            effort,
            api_key,
        })
    }

    /// The request-image target of one route (dsh `imageRequestTarget`):
    /// the undeclared-model rejection carries the verbatim message and
    /// code, so the registry slice dispatches through this fallible face.
    ///
    /// # Errors
    /// [`NO_ADAPTER_CODE`] for an unowned route and
    /// [`UNKNOWN_MODEL_CODE`] for an undeclared model.
    pub fn request_image_target(
        &self,
        provider: &str,
        model: &str,
        image: &ImageAttachmentRef,
    ) -> Result<fr_llm::adapter::ImageRequestTarget, LlmError> {
        let profiles = (self.options.profiles)();
        let profile = profiles.get(provider).ok_or_else(|| {
            static_error(
                &format!("fr-llm-openai adapter does not own provider \"{provider}\""),
                NO_ADAPTER_CODE,
            )
        })?;
        if !profile
            .catalog
            .models
            .iter()
            .any(|candidate| candidate.id == model)
        {
            return Err(static_error(
                &format!(
                    "fr-llm-openai provider \"{provider}\" has no configured model \"{model}\""
                ),
                UNKNOWN_MODEL_CODE,
            ));
        }
        Ok(route_image_target(image, profile))
    }
}

/// The dimensions and byte target one route applies to every request
/// image (dsh `requestImageTarget` over the route budgets).
fn route_image_target(
    image: &ImageAttachmentRef,
    profile: &ResolvedRouteProfile,
) -> fr_llm::adapter::ImageRequestTarget {
    let dimensions = request_image_dimensions(
        image.width,
        image.height,
        profile.request_image_pixel_budget,
    );
    fr_llm::adapter::ImageRequestTarget {
        width: dimensions.width,
        height: dimensions.height,
        max_bytes: profile.request_image_max_bytes,
    }
}

impl LlmAdapter for OpenAiAdapter {
    /// Stream one model call: the plan is resolved from the current
    /// profiles once, then the request runs on a plugin-owned worker
    /// thread (dsh `stream` capturing one snapshot per call).
    fn stream(&self, options: GenerateOptions) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        let profiles = (self.options.profiles)();
        match Self::plan_of(&self.options, &profiles, &options) {
            Ok(plan) => spawn_generation(self.options.clone(), options, plan),
            Err(error) => terminal_error_stream(&error),
        }
    }

    /// The configured display name, not the route key (dsh
    /// `providerInfo`; an unowned route degrades to the key).
    fn provider_info(&self, provider: &str) -> ProviderInfo {
        let name = (self.options.profiles)().get(provider).map_or_else(
            || provider.to_owned(),
            |profile| profile.display_name.clone(),
        );
        ProviderInfo {
            id: provider.to_owned(),
            name,
        }
    }

    /// The route's frozen retry policy (dsh `providerRetryPolicy`).
    fn provider_retry_policy(&self, provider: &str) -> Option<fr_llm::retry_policy::RetryPolicy> {
        (self.options.profiles)()
            .get(provider)
            .map(|profile| profile.retry_policy.clone())
    }

    /// The route's hand-declared catalog (dsh `listModels`): a route
    /// whose named credential misses offers nothing. The dsh throws
    /// (`NO_ADAPTER`, non-credential refusals) cannot cross this trait
    /// face and degrade to the empty list; the registry slice carries
    /// the fallible face.
    fn list_models(&self, provider: &str) -> impl Future<Output = Vec<LlmModelInfo>> {
        let profiles = (self.options.profiles)();
        let models = match profiles.get(provider) {
            Some(profile) if (self.options.resolve_api_key)(provider, profile).is_ok() => profile
                .catalog
                .models
                .iter()
                .map(|model| LlmModelInfo {
                    provider: provider.to_owned(),
                    id: model.id.clone(),
                    name: model.name.clone(),
                    description: None,
                    input_modalities: Some(modality_faces(&model.input)),
                })
                .collect(),
            _ => Vec::new(),
        };
        std::future::ready(models)
    }

    /// The exact-route metadata (dsh `resolveModel`); an admission
    /// refusal cannot cross the trait face and degrades to the bare
    /// identity metadata (deepseek-crate judgment).
    fn resolve_model(
        &self,
        provider: &str,
        model: &str,
    ) -> impl Future<Output = ResolvedModelInfo> {
        let profiles = (self.options.profiles)();
        let resolved = match snapshot_of(&profiles, provider, model) {
            Ok((profile, model)) => model_info(profile, model),
            Err(_) => ResolvedModelInfo {
                provider: provider.to_owned(),
                id: model.to_owned(),
                name: model.to_owned(),
                ..ResolvedModelInfo::default()
            },
        };
        std::future::ready(resolved)
    }

    /// Bind exact model metadata and the dispatch to one frozen profiles
    /// generation (dsh `prepareCall`): a settings change between
    /// preparation and dispatch cannot mix generations, and an admission
    /// refusal surfaces at dispatch time as the terminal fr-error record.
    fn prepare_call(
        self: &Arc<Self>,
        provider: &str,
        model: &str,
    ) -> impl Future<Output = PreparedAdapterCall> {
        let profiles = (self.options.profiles)();
        let resolved = match snapshot_of(&profiles, provider, model) {
            Ok((profile, model)) => model_info(profile, model),
            Err(_) => ResolvedModelInfo {
                provider: provider.to_owned(),
                id: model.to_owned(),
                name: model.to_owned(),
                ..ResolvedModelInfo::default()
            },
        };
        let config = LlmCallConfig {
            provider: provider.to_owned(),
            model: model.to_owned(),
            ..LlmCallConfig::default()
        };
        let options = self.options.clone();
        let frozen = profiles;
        std::future::ready(PreparedAdapterCall::new(
            resolved,
            config,
            Arc::new(
                move |request| match Self::plan_of(&options, &frozen, &request) {
                    Ok(plan) => spawn_generation(options.clone(), request, plan),
                    Err(error) => terminal_error_stream(&error),
                },
            ),
        ))
    }
}

/// One frozen profiles view's profile/model pair (dsh `modelOf`'s two
/// refusals inlined).
fn snapshot_of<'a>(
    profiles: &'a BTreeMap<String, ResolvedRouteProfile>,
    provider: &str,
    model: &str,
) -> Result<(&'a ResolvedRouteProfile, &'a ModelDescriptor), LlmError> {
    let profile = profiles.get(provider).ok_or_else(|| {
        static_error(
            &format!("fr-llm-openai adapter does not own provider \"{provider}\""),
            NO_ADAPTER_CODE,
        )
    })?;
    profile
        .catalog
        .models
        .iter()
        .find(|candidate| candidate.id == model)
        .map(|model| (profile, model))
        .ok_or_else(|| {
            static_error(
                &format!(
                    "fr-llm-openai provider \"{provider}\" has no configured model \"{model}\""
                ),
                UNKNOWN_MODEL_CODE,
            )
        })
}

/// Run one generation's request on a plugin-owned worker thread (Q69-B)
/// and stream its chunks from the receiver. The worker owns the full
/// `Result` channel of the execution and, on every adapter-level
/// failure, sends the terminal fr-error record before closing the
/// channel (Q73-A; deepseek-crate judgment, thread name adapted).
fn spawn_generation(
    options: OpenAiAdapterOptions,
    request: GenerateOptions,
    plan: RequestPlan,
) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
    let (sender, receiver) = channel::<StreamChunk>();
    let signal: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));
    let wake_signal = Arc::clone(&signal);
    let worker_sender = sender.clone();
    let spawned = std::thread::Builder::new()
        .name("fr-llm-openai".to_owned())
        .spawn(move || {
            let outcome = run_stream(&options, &request, &plan, &worker_sender);
            if let Err(error) = outcome {
                let _ = worker_sender.send(fr_error_record(&error));
            }
            drop(worker_sender);
            wake(&wake_signal);
        });
    if spawned.is_err() {
        let _ = sender.send(fr_error_record(&transport_wrapped()));
        drop(sender);
    }
    Box::pin(ChannelStream {
        receiver,
        signal,
        closed: false,
    })
}

/// A stream that carries one admission refusal record.
fn terminal_error_stream(error: &LlmError) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
    let (sender, receiver) = channel();
    let _ = sender.send(fr_error_record(error));
    drop(sender);
    Box::pin(ChannelStream {
        receiver,
        signal: Arc::new(Mutex::new(None)),
        closed: false,
    })
}

/// The `futures_core::Stream` face of one worker's chunk channel: each
/// poll registers the waker before a `try_recv`, so a chunk sent (or the
/// channel close) after a `Pending` return re-wakes the consumer.
struct ChannelStream {
    receiver: Receiver<StreamChunk>,
    signal: Arc<Mutex<Option<Waker>>>,
    closed: bool,
}

impl Stream for ChannelStream {
    type Item = StreamChunk;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<StreamChunk>> {
        let this = self.get_mut();
        if this.closed {
            return Poll::Ready(None);
        }
        if let Ok(mut slot) = this.signal.lock() {
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

fn wake(signal: &Mutex<Option<Waker>>) {
    if let Ok(mut slot) = signal.lock()
        && let Some(waker) = slot.take()
    {
        waker.wake();
    }
}

/// The adapter-private terminal error record (Q73-A): the field names
/// are verbatim and the value domain is owned by this adapter.
fn fr_error_record(error: &LlmError) -> StreamChunk {
    let mut record = serde_json::Map::new();
    record.insert(
        "type".to_owned(),
        serde_json::Value::String(FR_ERROR_TYPE.to_owned()),
    );
    record.insert(
        "message".to_owned(),
        serde_json::Value::String(error.message().to_owned()),
    );
    record.insert(
        "code".to_owned(),
        serde_json::Value::String(error.code().to_owned()),
    );
    StreamChunk::Other(record)
}

/// Why one request execution failed: the two dsh failure faces. A
/// pipeline failure inside the pi-ai executor settles as the terminal
/// error event (usage plus finish), while an adapter-level `LlmError`
/// throws out and becomes the fr-error record.
struct EventFailure {
    /// The composed error message (`formatProviderError` plus the
    /// protocol arms).
    message: String,
    /// The accumulated usage at failure (zeros before any chunk).
    usage: TokenUsage,
    /// The content blocks produced before the failure.
    blocks_len: usize,
}

enum RequestFailure {
    /// An adapter-level `LlmError` passthrough (admission refusals, the
    /// offload bound, the transport wrap of attachment failures).
    Direct(LlmError),
    /// The pi-ai error-event face.
    Event(EventFailure),
}

impl RequestFailure {
    /// The pre-request shape of the event face: no usage, no blocks.
    fn event(message: impl Into<String>) -> Self {
        Self::Event(EventFailure {
            message: message.into(),
            usage: TokenUsage::default(),
            blocks_len: 0,
        })
    }
}

/// The dsh `streamWithSnapshot` mapping: one watchdog-armed execution
/// whose failure settles by face — the watchdog timeout wins first, then
/// caller cancellation over a direct failure, then the event face (where
/// caller cancellation rewrites the settlement stop, dsh `toStreamChunks`
/// error arm).
fn run_stream(
    options: &OpenAiAdapterOptions,
    request: &GenerateOptions,
    plan: &RequestPlan,
    sender: &Sender<StreamChunk>,
) -> Result<(), LlmError> {
    let watchdog = idle_watchdog(
        None,
        plan.profile.stream_idle_timeout_ms,
        LLM_STREAM_IDLE_TIMEOUT,
    )
    .map_err(|_rejected| transport_wrapped())?;
    match run_request(options, request, plan, &watchdog, sender) {
        Ok(()) => Ok(()),
        Err(failure) => {
            if timeout_of(watchdog.signal().reason(), Some(LLM_STREAM_IDLE_TIMEOUT)).is_some() {
                Err(static_error(
                    &format!(
                        "fr-llm-openai stream idle timeout after {}ms",
                        plan.profile.stream_idle_timeout_ms
                    ),
                    TIMEOUT_CODE,
                ))
            } else {
                match failure {
                    RequestFailure::Direct(error) => {
                        if caller_aborted(request) {
                            Err(static_error(
                                "fr-llm-openai request aborted by caller",
                                ABORTED_CODE,
                            ))
                        } else {
                            Err(error)
                        }
                    }
                    RequestFailure::Event(event) => {
                        // Caller cancellation rewrites the settlement stop
                        // (dsh `toStreamChunks`); the message keeps the
                        // pipeline's own face.
                        let stop: &'static str = if caller_aborted(request) {
                            "aborted"
                        } else {
                            "error"
                        };
                        settle_event(&event, stop, &plan.model, sender);
                        Ok(())
                    }
                }
            }
        }
    }
}

/// Emit the error event's two chunks: the accumulated usage, then the
/// settled finish (dsh `toStreamChunks` error arm; no replay state).
fn settle_event(
    event: &EventFailure,
    stop: &'static str,
    model: &ModelDescriptor,
    sender: &Sender<StreamChunk>,
) {
    let settlement = Settlement {
        stop,
        error_message: Some(event.message.clone()),
        model: model.id.clone(),
        provider: model.provider.clone(),
        blocks_len: event.blocks_len,
        input_tokens: usage_input_with_cache_read(&event.usage),
        output_tokens: event.usage.output_tokens,
        context_window: Some(model.context_window),
    };
    let _ = sender.send(StreamChunk::Known(KnownStreamChunk::Usage {
        usage: event.usage,
    }));
    let _ = sender.send(StreamChunk::Known(KnownStreamChunk::Finish {
        reason: settle_failure(&settlement),
        replay_state: None,
    }));
}

/// The overflow comparison value: uncached input plus cache-read (dsh
/// `input + cacheRead`).
fn usage_input_with_cache_read(usage: &TokenUsage) -> u64 {
    usage
        .cache_read_tokens
        .unwrap_or_default()
        .saturating_add(usage.input_tokens)
}

/// The history-preparation face (dsh `streamWithSnapshot`'s image gates
/// over `toPiContext`): the two model/service gates, the three
/// history-shape refusals, the deterministic request-image versions, the
/// base64 offload bound, and the offloaded-occurrence projection. The
/// returned request carries the projected messages for the transcript
/// fold; the versions feed the conversion seam.
fn prepare_history(
    options: &OpenAiAdapterOptions,
    request: &GenerateOptions,
    plan: &RequestPlan,
) -> Result<
    (
        GenerateOptions,
        HashMap<AttachmentId, fr_attachment::request_image::RequestImageAttachment>,
    ),
    RequestFailure,
> {
    let contains_image = request
        .messages
        .iter()
        .any(|message| content_has_image(&message.content_blocks()));
    if contains_image && !plan.model.input.contains(&Modality::Image) {
        return Err(RequestFailure::Direct(static_error(
            &format!(
                "fr-llm-openai model \"{}\" does not support image input",
                plan.model.id
            ),
            UNSUPPORTED_CONTENT_CODE,
        )));
    }
    let attachments: Option<&dyn AttachmentRead> = if contains_image {
        let Some(store) = options.attachments.as_deref() else {
            return Err(RequestFailure::Direct(static_error(
                "fr-llm-openai image input requires the durable attachment service",
                UNSUPPORTED_CONTENT_CODE,
            )));
        };
        Some(store)
    } else {
        None
    };
    assert_supported_history(&request.messages)?;
    let versions = match attachments {
        Some(store) => resolve_versions(
            store,
            &request.messages,
            &plan.profile,
            request.signal.as_ref(),
        )?,
        None => HashMap::new(),
    };
    // The base64 bound: only `max-bytes` carries (dsh
    // `requiredImageOffload` with no count limit or quanta).
    let budget = ImageRequestBudget {
        representation: ImageRepresentation::Base64,
        max_bytes: Some(plan.profile.max_request_image_bytes),
        max_images: None,
        byte_quantum: None,
        count_quantum: None,
    };
    let offload = required_image_offload(&request.messages, &budget, |reference| {
        versions
            .get(&reference.attachment_id)
            .map_or(reference.bytes, |version| version.bytes)
    });
    if offload > 0 {
        return Err(RequestFailure::Direct(
            LlmError::try_new(
                &format!(
                    "fr-llm-openai request images exceed the {}-byte base64 bound; {offload} \
                     more oldest occurrence(s) must be offloaded.",
                    plan.profile.max_request_image_bytes
                ),
                IMAGE_OFFLOAD_REQUIRED_CODE,
                None,
                None,
                None,
                Some(offload),
            )
            .expect("offload fields satisfy the LlmError invariants"),
        ));
    }
    let access = options.image_access.clone();
    let substitute = |reference: &ImageAttachmentRef| {
        offloaded_image_text(
            reference,
            access
                .as_ref()
                .and_then(|resolve| resolve(reference))
                .as_ref(),
        )
    };
    let projected_request = GenerateOptions {
        messages: project_offloaded_images(&request.messages, &substitute),
        ..request.clone()
    };
    Ok((projected_request, versions))
}

/// One full request (the dsh adapter try block over `toPiContext` and
/// the pi-ai stream executor): history admission, image preparation, the
/// wire conversion, the POST, and the SSE translation.
fn run_request(
    options: &OpenAiAdapterOptions,
    request: &GenerateOptions,
    plan: &RequestPlan,
    watchdog: &IdleWatchdog,
    sender: &Sender<StreamChunk>,
) -> Result<(), RequestFailure> {
    let (projected_request, versions) = prepare_history(options, request, plan)?;
    // The effective credential (dsh `getClientApiKey`): the resolved key
    // wins, an authorization header the route supplies stands on its
    // own, and nothing else serves the request.
    let key_face = match plan.api_key.as_deref().filter(|key| !key.is_empty()) {
        Some(key) => KeyFace::Bearer(key),
        None if has_auth_header(&plan.profile.headers) => KeyFace::HeaderAuth,
        None => {
            return Err(RequestFailure::event(format!(
                "No API key for provider: {}",
                plan.model.provider
            )));
        }
    };
    match plan.model.api {
        Api::OpenaiCompletions => run_completions(
            options,
            request,
            plan,
            &projected_request,
            &versions,
            &key_face,
            watchdog,
            sender,
        ),
        Api::OpenaiResponses => run_responses(
            options,
            request,
            plan,
            &projected_request,
            &versions,
            &key_face,
            watchdog,
            sender,
        ),
    }
}

/// The history-shape admission of the image-capable context face (dsh
/// `assertSupportedHistory`, three refusals). Unknown-role messages
/// project no blocks, so no check can fire on them.
///
/// # Errors
/// [`UNSUPPORTED_CONTENT_CODE`] for tool-change blocks outside a
/// developer message, an image in any other in-history role, or a
/// non-text system message.
fn assert_supported_history(messages: &[RequestMessage]) -> Result<(), RequestFailure> {
    for message in messages {
        let Some(role) = message.role_name() else {
            continue;
        };
        let blocks = message.content_blocks();
        let shape_rejection = |detail: String| {
            RequestFailure::Direct(static_error(&detail, UNSUPPORTED_CONTENT_CODE))
        };
        if role != "developer"
            && blocks.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::Known(
                        KnownContentBlock::ToolAddition { .. }
                            | KnownContentBlock::ToolRemoval { .. }
                    )
                )
            })
        {
            return Err(shape_rejection(
                "Tool-change blocks require developer role".to_owned(),
            ));
        }
        if role != "user" && role != "tool" && content_has_image(&blocks) {
            return Err(shape_rejection(format!(
                "fr-llm-openai cannot represent an image in an in-history {role} message"
            )));
        }
        if role == "system"
            && blocks
                .iter()
                .any(|block| !matches!(block, ContentBlock::Known(KnownContentBlock::Text { .. })))
        {
            return Err(shape_rejection(
                "fr-llm-openai cannot represent non-text system messages".to_owned(),
            ));
        }
    }
    Ok(())
}

/// Resolve every retained image reference of one request into its
/// deterministic request version under the route target (dsh
/// `prepareRequestImages`).
///
/// # Errors
/// A failed read wraps as the transport failure: dsh propagates the raw
/// `AttachmentError` out of the adapter, which the fr face has no cause
/// chain for (deepseek-crate judgment).
fn resolve_versions(
    attachments: &dyn AttachmentRead,
    messages: &[RequestMessage],
    profile: &ResolvedRouteProfile,
    signal: Option<&AbortSignal>,
) -> Result<
    HashMap<AttachmentId, fr_attachment::request_image::RequestImageAttachment>,
    RequestFailure,
> {
    let mut versions = HashMap::new();
    for message in messages {
        for block in message.content_blocks() {
            let ContentBlock::Known(KnownContentBlock::Image { attachment, .. }) = &block else {
                continue;
            };
            if versions.contains_key(&attachment.attachment_id) {
                continue;
            }
            let target = route_image_target(attachment, profile);
            match attachments.read_image_request(attachment, target, signal) {
                Ok(version) => {
                    versions.insert(attachment.attachment_id.clone(), version);
                }
                Err(_failed) => return Err(RequestFailure::Direct(transport_wrapped())),
            }
        }
    }
    Ok(versions)
}

/// Which authorization face the request carries.
enum KeyFace<'a> {
    /// `Authorization: Bearer <key>` (the SDK's own header).
    Bearer(&'a str),
    /// A route-supplied authorization header stands on its own (dsh
    /// `getClientApiKey`'s `"unused"` arm: the SDK never overwrites a
    /// name the caller already set).
    HeaderAuth,
}

/// Whether the route headers carry their own authorization (dsh
/// `hasHeader`: case-insensitive over `authorization` and
/// `cf-aig-authorization`, a trimmed non-empty value).
fn has_auth_header(headers: &[(String, String)]) -> bool {
    headers.iter().any(|(name, value)| {
        (name.eq_ignore_ascii_case("authorization")
            || name.eq_ignore_ascii_case("cf-aig-authorization"))
            && !value.trim().is_empty()
    })
}

/// The request headers (dsh `createClient` plus the SDK's own auth and
/// content negotiation): attribution first and collision-immune, the
/// session-affinity trio per protocol gate, the bearer credential, the
/// JSON content negotiation and content type, and the route headers
/// merged last so they override everything but attribution. The accept
/// value is the SDK's unconditional `application/json` (it never
/// negotiates `text/event-stream`: the body's `stream` flag decides the
/// response face, client.js:1206). No `x-stainless-*` telemetry (19.2
/// batch 5 candidate) and no `github-copilot` dynamic headers (D9).
fn build_headers(
    api: Api,
    send_session_affinity_headers: bool,
    session_affinity_format: SessionAffinityFormat,
    cache_session_id: Option<&str>,
    key_face: &KeyFace<'_>,
    route_headers: &[(String, String)],
) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = attribution_headers()
        .iter()
        .map(|(name, value)| ((*name).to_owned(), value.clone()))
        .collect();
    if let Some(session) = cache_session_id {
        let gated = match api {
            Api::OpenaiCompletions => send_session_affinity_headers,
            // The responses client gates on the session id alone.
            Api::OpenaiResponses => true,
        };
        if gated {
            match session_affinity_format {
                SessionAffinityFormat::Openrouter => {
                    headers.push(("x-session-id".to_owned(), session.to_owned()));
                }
                SessionAffinityFormat::Openai => {
                    headers.push(("session_id".to_owned(), session.to_owned()));
                    headers.push(("x-client-request-id".to_owned(), session.to_owned()));
                    // Only the completions client also claims affinity.
                    if api == Api::OpenaiCompletions {
                        headers.push(("x-session-affinity".to_owned(), session.to_owned()));
                    }
                }
            }
        }
    }
    if let KeyFace::Bearer(key) = key_face {
        headers.push(("authorization".to_owned(), format!("Bearer {key}")));
    }
    headers.push(("accept".to_owned(), "application/json".to_owned()));
    headers.push(("content-type".to_owned(), "application/json".to_owned()));
    for (name, value) in route_headers {
        if !name.eq_ignore_ascii_case("user-agent") {
            headers.push((name.clone(), value.clone()));
        }
    }
    headers
}

/// The SDK's URL join (`client.js` `buildURL`): a trailing base slash
/// plus a leading path slash collapse to one. The SDK default base
/// (`https://api.openai.com/v1`) is dead here — the strict profile face
/// requires a non-empty `base-url`.
fn sdk_url(base: &str, path: &str) -> String {
    if base.ends_with('/') && path.starts_with('/') {
        format!("{base}{}", &path[1..])
    } else {
        format!("{base}{path}")
    }
}

/// The ambient retention resolution (dsh `resolveCacheRetention`):
/// explicit route preference, then the ambient long override, then the
/// default.
fn resolve_cache_retention(
    profile: Option<CacheRetention>,
    ambient: Option<&str>,
) -> CacheRetention {
    if let Some(explicit) = profile {
        return explicit;
    }
    if ambient == Some("long") {
        return CacheRetention::Long;
    }
    CacheRetention::Short
}

/// The completions wire messages: the folded transcript through the
/// history transform and conversion, with the folded system prompt
/// prepended as the instruction-role message (the pure layers carry only
/// the conversation turns).
fn completions_wire_messages(
    plan: &RequestPlan,
    projected: &GenerateOptions,
    versions: &HashMap<AttachmentId, fr_attachment::request_image::RequestImageAttachment>,
    compat: &crate::compat::ResolvedCompletionsCompat,
) -> (Vec<serde_json::Value>, Vec<Turn>) {
    let folded = fold_transcript(projected);
    let route = RouteIdentity {
        provider: plan.model.provider.as_str(),
        model: plan.model.id.as_str(),
    };
    let supports_images = plan.model.input.contains(&Modality::Image);
    let turns = transform_history(
        &folded.messages,
        &route,
        supports_images,
        IdNormalization::Completions,
    );
    let images = image_resolver(versions);
    let convert_ctx = ConvertContext {
        route,
        compat,
        reasoning: plan.model.reasoning,
        images: &images,
    };
    let mut messages = convert_messages(&turns, &convert_ctx);
    if !folded.system.is_empty() {
        let role = if plan.model.reasoning && compat.supports_developer_role {
            "developer"
        } else {
            "system"
        };
        messages.insert(
            0,
            serde_json::json!({ "role": role, "content": folded.system }),
        );
    }
    (messages, turns)
}

/// One completions request (dsh `openai-completions.ts` `stream`): the
/// folded conversion, the params, the POST, and the chunk translation.
#[expect(
    clippy::too_many_arguments,
    reason = "the dsh stream closure environment (options, request, plan, projected history, versions, credential, watchdog, sender) passes as explicit parameters"
)]
fn run_completions(
    options: &OpenAiAdapterOptions,
    request: &GenerateOptions,
    plan: &RequestPlan,
    projected: &GenerateOptions,
    versions: &HashMap<AttachmentId, fr_attachment::request_image::RequestImageAttachment>,
    key_face: &KeyFace<'_>,
    watchdog: &IdleWatchdog,
    sender: &Sender<StreamChunk>,
) -> Result<(), RequestFailure> {
    let ResolvedCompat::Completions(compat) = &plan.model.compat else {
        // The plan's model carries the protocol it was declared with.
        return Err(RequestFailure::Direct(transport_wrapped()));
    };
    let (messages, turns) = completions_wire_messages(plan, projected, versions, compat);
    let retention = resolve_cache_retention(
        plan.profile.cache_retention,
        std::env::var(PI_CACHE_RETENTION_ENV).ok().as_deref(),
    );
    let max_tokens = clamp_max_tokens_to_context(
        plan.model.context_window.cast_signed(),
        request.max_tokens,
        plan.model.max_tokens,
        request,
    );
    let params = build_params(
        &plan.model,
        compat,
        retention,
        messages,
        request.tools.as_deref(),
        has_tool_history(&turns),
        plan.profile.thinking_budgets.as_ref(),
        &BuildParamsOptions {
            effort: plan.effort,
            temperature: request.temperature,
            max_tokens: Some(max_tokens),
            session_id: request.session_id.as_ref().map(fr_wire::SessionId::as_str),
            tool_choice: None,
        },
    );
    let cache_session_id = (retention != CacheRetention::None)
        .then(|| request.session_id.as_ref().map(fr_wire::SessionId::as_str))
        .flatten();
    let headers = build_headers(
        plan.model.api,
        compat.send_session_affinity_headers,
        compat.session_affinity_format,
        cache_session_id,
        key_face,
        &plan.profile.headers,
    );
    // The request deadline starts when the runner takes the request,
    // ahead of the POST (the SDK timeout covers the whole fetch).
    let deadline_ms = request_deadline(options, plan);
    let response = post(
        &options.fetch,
        sdk_url(&plan.model.base_url, CHAT_COMPLETIONS_PATH),
        headers,
        &params.to_string(),
    )?;
    if !(200..300).contains(&response.status) {
        return Err(error_event_of_response(
            response.status,
            response.body,
            plan.model.api,
            plan.model.provider.as_str(),
        ));
    }
    let cycle = BodyLoop {
        request,
        model: &plan.model,
        watchdog,
        deadline_ms,
        now_ms: &options.now_ms,
        sender,
    };
    consume_response(
        response.body,
        ProtocolTranslator::Completions(Translator::new(
            plan.model.id.clone(),
            compat.supports_finish_reason,
        )),
        &cycle,
    )
}

/// One responses request (dsh `openai-responses.ts` `stream`).
#[expect(
    clippy::too_many_arguments,
    reason = "the dsh stream closure environment (options, request, plan, projected history, versions, credential, watchdog, sender) passes as explicit parameters"
)]
fn run_responses(
    options: &OpenAiAdapterOptions,
    request: &GenerateOptions,
    plan: &RequestPlan,
    projected: &GenerateOptions,
    versions: &HashMap<AttachmentId, fr_attachment::request_image::RequestImageAttachment>,
    key_face: &KeyFace<'_>,
    watchdog: &IdleWatchdog,
    sender: &Sender<StreamChunk>,
) -> Result<(), RequestFailure> {
    let ResolvedCompat::Responses(compat) = &plan.model.compat else {
        return Err(RequestFailure::Direct(transport_wrapped()));
    };
    let folded = fold_transcript(projected);
    let route = RouteIdentity {
        provider: plan.model.provider.as_str(),
        model: plan.model.id.as_str(),
    };
    let supports_images = plan.model.input.contains(&Modality::Image);
    let turns = transform_history(
        &folded.messages,
        &route,
        supports_images,
        IdNormalization::Responses,
    );
    let images = image_resolver(versions);
    let convert_ctx = ResponsesConvertContext {
        route,
        reasoning: plan.model.reasoning,
        supports_developer_role: compat.supports_developer_role,
        supports_images,
        images: &images,
    };
    let retention = resolve_cache_retention(
        plan.profile.cache_retention,
        std::env::var(PI_CACHE_RETENTION_ENV).ok().as_deref(),
    );
    let max_tokens = clamp_max_tokens_to_context(
        plan.model.context_window.cast_signed(),
        request.max_tokens,
        plan.model.max_tokens,
        request,
    );
    let omit_unsupported_fields = is_chat_gpt_sign_in(
        plan.model.provider.as_str(),
        plan.model.base_url.as_str(),
        plan.api_key.as_deref(),
    );
    let params = build_params_responses(
        &plan.model,
        compat,
        retention,
        Some(folded.system.as_str()),
        &turns,
        &convert_ctx,
        request.tools.as_deref(),
        &ResponsesParamsOptions {
            effort: plan.effort,
            temperature: request.temperature,
            max_tokens: Some(max_tokens),
            session_id: request.session_id.as_ref().map(fr_wire::SessionId::as_str),
            tool_choice: None,
            omit_unsupported_fields,
        },
    );
    let cache_session_id = (retention != CacheRetention::None)
        .then(|| request.session_id.as_ref().map(fr_wire::SessionId::as_str))
        .flatten();
    // The responses client gates affinity on the session id alone (no
    // `sendSessionAffinityHeaders` compat switch exists there).
    let headers = build_headers(
        plan.model.api,
        true,
        compat.session_affinity_format,
        cache_session_id,
        key_face,
        &plan.profile.headers,
    );
    // The request deadline starts when the runner takes the request,
    // ahead of the POST (the SDK timeout covers the whole fetch).
    let deadline_ms = request_deadline(options, plan);
    let response = post(
        &options.fetch,
        sdk_url(&plan.model.base_url, RESPONSES_PATH),
        headers,
        &params.to_string(),
    )?;
    if !(200..300).contains(&response.status) {
        return Err(error_event_of_response(
            response.status,
            response.body,
            plan.model.api,
            plan.model.provider.as_str(),
        ));
    }
    let cycle = BodyLoop {
        request,
        model: &plan.model,
        watchdog,
        deadline_ms,
        now_ms: &options.now_ms,
        sender,
    };
    consume_response(
        response.body,
        ProtocolTranslator::Responses(ResponsesTranslator::new(plan.model.id.clone())),
        &cycle,
    )
}

/// The wire-carrying value of one media type (the serde rename face).
fn media_type_str(media_type: fr_wire::content::ImageMediaType) -> String {
    serde_json::to_value(media_type)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The adapter-side image bytes resolution: the pre-resolved version of
/// one reference (a miss drops the part — the degenerate
/// attachment-service face the conversion layer documents).
fn image_resolver(
    versions: &HashMap<AttachmentId, fr_attachment::request_image::RequestImageAttachment>,
) -> impl Fn(&ImageAttachmentRef) -> Option<(String, Vec<u8>)> + '_ {
    move |reference: &ImageAttachmentRef| {
        versions
            .get(&reference.attachment_id)
            .map(|version| (media_type_str(version.media_type), version.data.clone()))
    }
}

/// Whether the converted history carries tool turns (dsh `hasToolHistory`
/// over the pi-ai messages: a tool result, or an assistant tool call).
fn has_tool_history(turns: &[Turn]) -> bool {
    turns.iter().any(|turn| match turn {
        Turn::Tool { .. } => true,
        Turn::Assistant { content, .. } => content.iter().any(|block| {
            matches!(
                block,
                ContentBlock::Known(KnownContentBlock::ToolCall { .. })
            )
        }),
        Turn::User { .. } => false,
    })
}

/// The one-protocol translator face the body loop drives.
enum ProtocolTranslator {
    /// The Chat Completions chunk state machine.
    Completions(Translator),
    /// The Responses event state machine.
    Responses(ResponsesTranslator),
}

impl ProtocolTranslator {
    /// Translate one decoded SSE frame's data object.
    fn translate_frame(
        &mut self,
        frame: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<StreamChunk>, LlmError> {
        match self {
            Self::Completions(translator) => translator.translate_chunk(frame),
            Self::Responses(translator) => translator.translate_event(frame),
        }
    }

    /// Close the stream and produce the terminal chunks.
    fn end_of_stream(&mut self) -> Result<Vec<StreamChunk>, LlmError> {
        match self {
            Self::Completions(translator) => translator.end_of_stream(),
            Self::Responses(translator) => translator.end_of_stream(),
        }
    }

    /// The accumulated usage (the error-event settlement face).
    const fn usage(&self) -> TokenUsage {
        match self {
            Self::Completions(translator) => translator.usage(),
            Self::Responses(translator) => translator.usage(),
        }
    }

    /// The content blocks produced so far.
    const fn blocks_len(&self) -> usize {
        match self {
            Self::Completions(translator) => translator.blocks_len(),
            Self::Responses(translator) => translator.blocks_len(),
        }
    }
}

/// Send one streaming POST and return the response (dsh
/// `retryProviderRequest` at `maxRetries: 0`: one SDK attempt; transport
/// failures wear the SDK connection message verbatim).
fn post(
    fetch: &Arc<dyn Fetch>,
    url: String,
    headers: Vec<(String, String)>,
    body: &str,
) -> Result<fr_net::http::HttpResponse, RequestFailure> {
    fetch
        .fetch(HttpRequest {
            method: HttpMethod::Post,
            url,
            headers,
            body: Some(body.as_bytes().to_vec()),
        })
        .map_err(|_transport| RequestFailure::event(CONNECTION_ERROR_MESSAGE))
}

/// The non-2xx face (dsh catch: `normalizeProviderError` over the read
/// body, `formatProviderError`, and the protocol arms — the responses
/// prefix and the ChatGPT-usage hint; completions carries no prefix and
/// its `OpenRouter` raw-metadata append is dead here, no SDK error object
/// exists to carry `error.metadata.raw`). A redirect a custom `Fetch`
/// still hands back wears the SDK's refused-redirect connection face.
fn error_event_of_response(
    status: u16,
    body: Box<dyn Read + Send>,
    api: Api,
    provider: &str,
) -> RequestFailure {
    let mut body = body;
    let mut bytes = Vec::new();
    let text = body
        .read_to_end(&mut bytes)
        .map(|_| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    if fr_net::http::is_refused_redirect(status) {
        return RequestFailure::event(CONNECTION_ERROR_MESSAGE);
    }
    let normalized = normalize_error_body(Some(status), &text);
    let prefix = match api {
        Api::OpenaiResponses => Some(format!(
            "{} API error",
            if provider == "openai" {
                "OpenAI"
            } else {
                provider
            }
        )),
        Api::OpenaiCompletions => None,
    };
    let mut message = format_provider_error(&normalized, prefix.as_deref());
    if api == Api::OpenaiResponses && message.contains("subscription_sharing_usage_limit_exceeded")
    {
        message.push_str(CHATGPT_USAGE_HINT);
    }
    RequestFailure::event(message)
}

/// The shared facts of one response-body loop.
struct BodyLoop<'a> {
    request: &'a GenerateOptions,
    model: &'a ModelDescriptor,
    watchdog: &'a IdleWatchdog,
    /// The whole-request deadline of `timeout-ms` (dsh SDK request
    /// timeout; the launch wall clock starts it).
    deadline_ms: Option<u64>,
    now_ms: &'a NowMsCallback,
    sender: &'a Sender<StreamChunk>,
}

/// The event-face failure carrying the translator's snapshot.
fn event_failure(translator: &ProtocolTranslator, message: impl Into<String>) -> RequestFailure {
    RequestFailure::Event(EventFailure {
        message: message.into(),
        usage: translator.usage(),
        blocks_len: translator.blocks_len(),
    })
}

/// Consume one streamed response body: read-window boundary checks
/// (watchdog, caller abort, request deadline), SDK-semantics SSE framing
/// with the `[DONE]` sentinel, chunk translation, and the terminal gate
/// (dsh `stream`'s event loop plus `finishBlock` close).
fn consume_response(
    mut body: Box<dyn Read + Send>,
    mut translator: ProtocolTranslator,
    cycle: &BodyLoop<'_>,
) -> Result<(), RequestFailure> {
    let mut parser = SseParser::new();
    let mut window = [0_u8; READ_WINDOW_BYTES];
    let mut done_sentinel = false;
    loop {
        if cycle.watchdog.check() || caller_aborted(cycle.request) {
            // The watchdog abort surfaces through the outer timeout
            // mapping; a caller abort settles `aborted` (the pi-ai
            // terminal-gate throw, whose wording both faces share).
            return Err(event_failure(&translator, REQUEST_ABORTED_MESSAGE));
        }
        if let Some(deadline) = cycle.deadline_ms
            && (cycle.now_ms)() >= deadline
        {
            return Err(event_failure(&translator, REQUEST_TIMED_OUT_MESSAGE));
        }
        let read = body
            .read(&mut window)
            .map_err(|error| event_failure(&translator, error.to_string()))?;
        let frames = if read == 0 {
            parser.flush()
        } else {
            cycle.watchdog.pulse();
            parser.feed(&window[..read])
        }
        .map_err(|error| {
            event_failure(
                &translator,
                Into::<LlmError>::into(error).message().to_owned(),
            )
        })?;
        for frame in frames {
            match frame {
                SseFrame::Done => {
                    done_sentinel = true;
                }
                SseFrame::Message { data, .. } => {
                    // A non-object data payload reads as no choices and
                    // no usage in the JS face; it translates to nothing.
                    if let serde_json::Value::Object(object) = data {
                        for chunk in translator.translate_frame(&object).map_err(|error| {
                            event_failure(&translator, error.message().to_owned())
                        })? {
                            cycle.sender.send(chunk).map_err(|_consumer_gone| {
                                RequestFailure::Direct(transport_wrapped())
                            })?;
                        }
                    }
                }
            }
            if done_sentinel {
                break;
            }
        }
        if read == 0 || done_sentinel {
            break;
        }
    }
    // The terminal gate (dsh `finishBlock` loop and the stop checks; the
    // pi-ai `Request was aborted` throw is superseded by the boundary
    // checks above).
    let mut chunks = translator
        .end_of_stream()
        .map_err(|error| event_failure(&translator, error.message().to_owned()))?;
    apply_done_settlement(&mut chunks, cycle, &translator);
    for chunk in chunks {
        cycle
            .sender
            .send(chunk)
            .map_err(|_consumer_gone| RequestFailure::Direct(transport_wrapped()))?;
    }
    Ok(())
}

/// The terminal gate's settlement (dsh `toStreamChunks` done arm through
/// `mapStopReason`): a clean stop with no content blocks is the
/// degenerate empty completion, and the usage-driven overflow cases
/// re-settle the finish; the replay state rides unchanged.
fn apply_done_settlement(
    chunks: &mut [StreamChunk],
    cycle: &BodyLoop<'_>,
    translator: &ProtocolTranslator,
) {
    let usage = translator.usage();
    for chunk in chunks {
        let StreamChunk::Known(KnownStreamChunk::Finish { reason, .. }) = chunk else {
            continue;
        };
        let stop: &'static str = match reason {
            FinishReason::Known(FinishReasonKind::Stop) => "stop",
            FinishReason::Known(FinishReasonKind::MaxTokens) => "length",
            FinishReason::Known(FinishReasonKind::ToolCalls) => "toolUse",
            // The translators' success faces never carry these two; the
            // mapping keeps the settlement total.
            FinishReason::Known(
                FinishReasonKind::Error { .. } | FinishReasonKind::Aborted { .. },
            )
            | FinishReason::Other(_) => "error",
        };
        let settlement = Settlement {
            stop,
            error_message: None,
            model: cycle.model.id.clone(),
            provider: cycle.model.provider.clone(),
            blocks_len: translator.blocks_len(),
            input_tokens: usage_input_with_cache_read(&usage),
            output_tokens: usage.output_tokens,
            context_window: Some(cycle.model.context_window),
        };
        *reason = settle_failure(&settlement);
    }
}

/// The whole-request deadline of `timeout-ms` (dsh SDK request timeout
/// face: the fetch-scoped deadline, started when the protocol runner
/// takes the request).
fn request_deadline(options: &OpenAiAdapterOptions, plan: &RequestPlan) -> Option<u64> {
    plan.profile
        .timeout_ms
        .map(|milliseconds| (options.now_ms)().saturating_add(milliseconds))
}

fn caller_aborted(options: &GenerateOptions) -> bool {
    options.signal.as_ref().is_some_and(AbortSignal::is_aborted)
}

fn static_error(message: &str, code: &str) -> LlmError {
    LlmError::try_new(message, code, None, None, None, None)
        .expect("static message and code satisfy the LlmError invariants")
}

/// The non-LLM pipeline-failure wrap (deepseek-crate judgment: the fr
/// error has no cause chain for the raw attachment or thread-start
/// failure).
fn transport_wrapped() -> LlmError {
    static_error("fr-llm-openai transport failed", TRANSPORT_CODE)
}

impl std::fmt::Debug for OpenAiAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiAdapter")
            .field("options", &self.options)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::resolve_route_profile;
    use fr_llm::error::{AUTH_CODE, CONTEXT_WINDOW_EXCEEDED_CODE, EMPTY_RESPONSE_CODE};
    use fr_net::http::HttpResponse;
    use fr_net::http::TransportError;
    use std::io::Cursor;
    use std::time::Duration;

    /// One recorded completions run: a streamed tool call with usage and
    /// the `[DONE]` sentinel.
    const RECORDED_TOOL_RUN: &str = concat!(
        "data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"role\":\"assistant\",\"tool_calls\":",
        "[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"sh\",\"arguments\":\"{\\\"cmd\\\":\\\"ls -la\\\"}\"}}]}}]}\n\n",
        "data: {\"id\":\"c1\",\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: {\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15,",
        "\"prompt_tokens_details\":{\"cached_tokens\":3}}}\n\n",
        "data: [DONE]\n\n",
    );

    fn route_profile(api: &str, base_url: &str) -> ResolvedRouteProfile {
        let raw = serde_json::json!({
            "api": api,
            "base-url": base_url,
            "models": [{ "id": "acme-large" }],
        });
        resolve_route_profile("acme", &raw).expect("route resolves")
    }

    fn with_key(mut profile: ResolvedRouteProfile, reference: &str) -> ResolvedRouteProfile {
        profile.api_key_env = Some(reference.to_owned());
        profile
    }

    fn reasoning_route() -> ResolvedRouteProfile {
        let raw = serde_json::json!({
            "api": "openai-completions",
            "base-url": "https://gw.example/v1",
            "models": [{
                "id": "acme-think",
                "reasoning-efforts": {
                    "off": null,
                    "high": "high",
                    "xhigh": "xhigh",
                },
            }],
        });
        resolve_route_profile("acme", &raw).expect("route resolves")
    }

    /// One recorded HTTP response; every request is captured in arrival
    /// order for the header and URL pins.
    struct RecordedFetch {
        status: u16,
        body: Vec<u8>,
        delay: Option<Duration>,
        recorded: Mutex<Vec<HttpRequest>>,
    }

    impl RecordedFetch {
        fn new(status: u16, body: &str) -> Arc<Self> {
            Arc::new(Self {
                status,
                body: body.as_bytes().to_vec(),
                delay: None,
                recorded: Mutex::new(Vec::new()),
            })
        }

        fn slow(status: u16, body: &str, delay: Duration) -> Arc<Self> {
            Arc::new(Self {
                status,
                body: body.as_bytes().to_vec(),
                delay: Some(delay),
                recorded: Mutex::new(Vec::new()),
            })
        }

        fn requests(&self) -> Vec<HttpRequest> {
            self.recorded.lock().expect("recorded lock").clone()
        }
    }

    impl Fetch for RecordedFetch {
        fn fetch(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
            self.recorded.lock().expect("recorded lock").push(request);
            if let Some(delay) = self.delay {
                std::thread::sleep(delay);
            }
            Ok(HttpResponse {
                status: self.status,
                headers: Vec::new(),
                body: Box::new(Cursor::new(self.body.clone())),
            })
        }
    }

    /// A response body that streams one prefix and then fails like a
    /// dropped connection.
    struct FailingAfterPrefix {
        prefix: Vec<u8>,
        position: usize,
    }

    impl Read for FailingAfterPrefix {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.position < self.prefix.len() {
                let take = (self.prefix.len() - self.position).min(buf.len());
                buf[..take].copy_from_slice(&self.prefix[self.position..self.position + take]);
                self.position += take;
                Ok(take)
            } else {
                Err(std::io::Error::other("connection reset"))
            }
        }
    }

    /// A failing-body response factory (the transport drop face).
    struct FailingBodyFetch {
        prefix: &'static str,
    }

    impl Fetch for FailingBodyFetch {
        fn fetch(&self, _request: HttpRequest) -> Result<HttpResponse, TransportError> {
            Ok(HttpResponse {
                status: 200,
                headers: Vec::new(),
                body: Box::new(FailingAfterPrefix {
                    prefix: self.prefix.as_bytes().to_vec(),
                    position: 0,
                }),
            })
        }
    }

    fn options_with(
        fetch: Arc<dyn Fetch>,
        profiles: Vec<ResolvedRouteProfile>,
    ) -> OpenAiAdapterOptions {
        let routes: BTreeMap<String, ResolvedRouteProfile> = profiles
            .into_iter()
            .map(|profile| (profile.provider.clone(), profile))
            .collect();
        let fetch_for_key = Arc::clone(&fetch);
        let resolve: ResolveApiKeyCallback = Arc::new(move |provider, profile| {
            resolve_env_api_key(
                &|_name: &str| {
                    let _ = &fetch_for_key;
                    None
                },
                provider,
                profile,
            )
        });
        OpenAiAdapterOptions {
            profiles: Arc::new(move || routes.clone()),
            resolve_api_key: resolve,
            fetch,
            attachments: None,
            image_access: None,
            now_ms: Arc::new(|| 1_000_000),
        }
    }

    fn keyed_options(
        fetch: Arc<dyn Fetch>,
        profiles: Vec<ResolvedRouteProfile>,
        env: BTreeMap<String, String>,
    ) -> OpenAiAdapterOptions {
        let routes: BTreeMap<String, ResolvedRouteProfile> = profiles
            .into_iter()
            .map(|profile| (profile.provider.clone(), profile))
            .collect();
        let resolve: ResolveApiKeyCallback = Arc::new(move |provider, profile| {
            resolve_env_api_key(&|name: &str| env.get(name).cloned(), provider, profile)
        });
        OpenAiAdapterOptions {
            profiles: Arc::new(move || routes.clone()),
            resolve_api_key: resolve,
            fetch,
            attachments: None,
            image_access: None,
            now_ms: Arc::new(|| 1_000_000),
        }
    }

    fn bare_options(provider: &str, model: &str) -> GenerateOptions {
        GenerateOptions {
            provider: provider.to_owned(),
            model: model.to_owned(),
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

    fn collect(stream: &mut Pin<Box<dyn Stream<Item = StreamChunk> + Send>>) -> Vec<StreamChunk> {
        let mut cx = Context::from_waker(Waker::noop());
        let mut chunks = Vec::new();
        loop {
            match stream.as_mut().poll_next(&mut cx) {
                Poll::Ready(Some(chunk)) => chunks.push(chunk),
                Poll::Ready(None) => return chunks,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let mut cx = Context::from_waker(Waker::noop());
        let mut future = Box::pin(future);
        loop {
            match future.as_mut().poll(&mut cx) {
                Poll::Ready(output) => return output,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    fn record_of(chunk: &StreamChunk) -> &serde_json::Map<String, serde_json::Value> {
        match chunk {
            StreamChunk::Other(record) => record,
            StreamChunk::Known(_known) => panic!("expected a raw record chunk"),
        }
    }

    fn finish_of(chunk: &StreamChunk) -> &FinishReason {
        let StreamChunk::Known(KnownStreamChunk::Finish { reason, .. }) = chunk else {
            panic!("expected a finish chunk");
        };
        reason
    }

    fn finish_code(chunk: &StreamChunk) -> String {
        match finish_of(chunk) {
            FinishReason::Known(
                FinishReasonKind::Error { failure } | FinishReasonKind::Aborted { failure },
            ) => failure.code.clone(),
            _ => String::new(),
        }
    }

    /// The base-url override scenario: a custom endpoint receives the
    /// request at the joined completions resource path — never a
    /// hard-coded provider address.
    #[test]
    fn openai_base_url_overrides_endpoint() {
        let fetch = RecordedFetch::new(200, RECORDED_TOOL_RUN);
        let adapter = OpenAiAdapter::new(keyed_options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            vec![with_key(
                route_profile("openai-completions", "https://gw.internal/acme"),
                "ACME_KEY",
            )],
            BTreeMap::from([("ACME_KEY".to_owned(), "sk-test".to_owned())]),
        ));
        let mut stream = adapter.stream(bare_options("acme", "acme-large"));
        collect(&mut stream);
        let requests = fetch.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url, "https://gw.internal/acme/chat/completions");
        assert_eq!(requests[0].method, HttpMethod::Post);
        assert!(!requests[0].url.contains("api.openai.com"));
    }

    /// A distinct responses route targets `/responses` under its own
    /// base-url with the SDK join (trailing slash collapses).
    #[test]
    fn responses_route_targets_responses_path() {
        let fetch = RecordedFetch::new(200, "");
        let responses_profile = {
            let raw = serde_json::json!({
                "api": "openai-responses",
                "base-url": "https://gw.internal/acme/",
                "models": [{ "id": "acme-r" }],
            });
            resolve_route_profile("acme-r", &raw).expect("route resolves")
        };
        let adapter = OpenAiAdapter::new(keyed_options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            vec![with_key(responses_profile, "ACME_KEY")],
            BTreeMap::from([("ACME_KEY".to_owned(), "sk-test".to_owned())]),
        ));
        let mut stream = adapter.stream(bare_options("acme-r", "acme-r"));
        collect(&mut stream);
        let requests = fetch.requests();
        assert_eq!(requests[0].url, "https://gw.internal/acme/responses");
    }

    /// The undeclared-model scenario: the request refuses with the
    /// verbatim message naming the model, no network I/O happens, and no
    /// catalog fallback exists to save it.
    #[test]
    fn openai_undeclared_model_refuses_without_catalog() {
        let fetch = RecordedFetch::new(200, RECORDED_TOOL_RUN);
        let adapter = OpenAiAdapter::new(options_with(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            vec![route_profile("openai-completions", "https://gw.example/v1")],
        ));
        let mut stream = adapter.stream(bare_options("acme", "gpt-imagination"));
        let chunks = collect(&mut stream);
        assert_eq!(chunks.len(), 1);
        let record = record_of(&chunks[0]);
        assert_eq!(
            record.get("message").and_then(serde_json::Value::as_str),
            Some("fr-llm-openai provider \"acme\" has no configured model \"gpt-imagination\"")
        );
        assert_eq!(
            record.get("code").and_then(serde_json::Value::as_str),
            Some("UNKNOWN_MODEL")
        );
        assert!(fetch.requests().is_empty());
    }

    /// The fail-settle scenario: a transport drop mid-body settles as one
    /// usage plus one error finish — no terminal fr-error record — and a
    /// completed tool call's durable arguments stay byte-identical to the
    /// provider's raw string.
    #[test]
    fn openai_fail_settle_emits_usage_and_error_finish() {
        let fetch = RecordedFetch::new(200, RECORDED_TOOL_RUN);
        let adapter = OpenAiAdapter::new(keyed_options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            vec![with_key(
                route_profile("openai-completions", "https://gw.example/v1"),
                "ACME_KEY",
            )],
            BTreeMap::from([("ACME_KEY".to_owned(), "sk-test".to_owned())]),
        ));
        let mut clean = adapter.stream(bare_options("acme", "acme-large"));
        let chunks = collect(&mut clean);
        let tags: Vec<String> = chunks
            .iter()
            .map(|chunk| match chunk {
                StreamChunk::Known(known) => {
                    let value = serde_json::to_value(known).expect("known chunks serialize");
                    value
                        .get("type")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                }
                StreamChunk::Other(record) => record
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            })
            .collect();
        let tags: Vec<&str> = tags.iter().map(String::as_str).collect();
        assert_eq!(
            tags,
            [
                "block-start",
                "tool-call-delta",
                "block-end",
                "usage",
                "finish"
            ]
        );
        // The durable tool-call arguments re-stringify to exactly the
        // provider's raw bytes (the Q75-C contract face).
        let StreamChunk::Known(KnownStreamChunk::BlockEnd { block, .. }) = &chunks[2] else {
            panic!("expected the tool-call block end");
        };
        let ContentBlock::Known(KnownContentBlock::ToolCall { arguments, .. }) = block else {
            panic!("expected a tool-call block");
        };
        assert_eq!(arguments, r#"{"cmd":"ls -la"}"#);
        assert!(matches!(
            finish_of(&chunks[4]),
            FinishReason::Known(FinishReasonKind::ToolCalls)
        ));

        // The drop face: chunks before the drop stay, then usage plus an
        // error finish classified from the transport wording.
        let failing: Arc<dyn Fetch> = Arc::new(FailingBodyFetch {
            prefix: "data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n",
        });
        let adapter = OpenAiAdapter::new(keyed_options(
            failing,
            vec![with_key(
                route_profile("openai-completions", "https://gw.example/v1"),
                "ACME_KEY",
            )],
            BTreeMap::from([("ACME_KEY".to_owned(), "sk-test".to_owned())]),
        ));
        let mut dropped = adapter.stream(bare_options("acme", "acme-large"));
        let chunks = collect(&mut dropped);
        assert_eq!(chunks.len(), 4);
        let StreamChunk::Known(KnownStreamChunk::Usage { usage }) = &chunks[2] else {
            panic!("expected the settle usage");
        };
        assert_eq!(usage.input_tokens, 0);
        let reason = finish_of(&chunks[3]);
        let FinishReason::Known(FinishReasonKind::Error { failure }) = reason else {
            panic!("expected an error finish");
        };
        assert_eq!(failure.code, TRANSPORT_CODE);
        assert_eq!(failure.message, "connection reset");
    }

    /// The admission chain refuses before any network I/O: the `stop`
    /// face, the unowned route, the unsupported reasoning effort, and
    /// the named-credential miss.
    #[test]
    fn admission_refuses_before_network() {
        let fetch = RecordedFetch::new(200, RECORDED_TOOL_RUN);
        let adapter = OpenAiAdapter::new(keyed_options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            vec![with_key(reasoning_route(), "ACME_KEY")],
            BTreeMap::from([("ACME_KEY".to_owned(), "sk-test".to_owned())]),
        ));
        let mut options = bare_options("acme", "acme-think");
        options.stop = Some(vec!["END".to_owned()]);
        let chunks = collect(&mut adapter.stream(options));
        assert_eq!(
            record_of(&chunks[0])
                .get("message")
                .and_then(serde_json::Value::as_str),
            Some("fr-llm-openai does not support GenerateOptions.stop")
        );
        assert_eq!(
            record_of(&chunks[0])
                .get("code")
                .and_then(serde_json::Value::as_str),
            Some("UNSUPPORTED_OPTION")
        );

        let chunks = collect(&mut adapter.stream(bare_options("other", "acme-think")));
        assert_eq!(
            record_of(&chunks[0])
                .get("message")
                .and_then(serde_json::Value::as_str),
            Some("fr-llm-openai adapter does not own provider \"other\"")
        );

        let mut options = bare_options("acme", "acme-think");
        options.reasoning_effort = Some(ReasoningEffortId::new("minimal"));
        let chunks = collect(&mut adapter.stream(options));
        assert_eq!(
            record_of(&chunks[0])
                .get("message")
                .and_then(serde_json::Value::as_str),
            Some(
                "fr-llm-openai provider \"acme\" model \"acme-think\" does not support \
                 reasoning effort \"minimal\""
            )
        );

        // A declared-valueless off stays selectable (the SendNothing
        // face): the request streams instead of refusing.
        let mut options = bare_options("acme", "acme-think");
        options.reasoning_effort = Some(ReasoningEffortId::new("off"));
        let chunks = collect(&mut adapter.stream(options));
        assert!(
            !chunks
                .iter()
                .any(|chunk| matches!(chunk, StreamChunk::Other(_))),
            "off is selectable and the request proceeds"
        );
        // Only the off request reached the network; the three refusals
        // above never did.
        assert_eq!(fetch.requests().len(), 1);
    }

    /// The header pin: attribution first, the bearer credential, the JSON
    /// content type, and the route headers merged last (they override the
    /// credential and every protocol default but attribution); the
    /// session-affinity trio rides only with a session id on an endpoint
    /// that asks for it.
    #[test]
    fn headers_pin_attribution_bearer_and_route_merge() {
        let fetch = RecordedFetch::new(200, RECORDED_TOOL_RUN);
        let mut profile = with_key(
            route_profile("openai-completions", "https://gw.example/v1"),
            "ACME_KEY",
        );
        profile.headers = vec![("x-route".to_owned(), "1".to_owned())];
        let adapter = OpenAiAdapter::new(keyed_options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            vec![profile],
            BTreeMap::from([("ACME_KEY".to_owned(), "sk-test".to_owned())]),
        ));
        let mut stream = adapter.stream(bare_options("acme", "acme-large"));
        collect(&mut stream);
        let request = &fetch.requests()[0];
        let names: Vec<&str> = request
            .headers
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "user-agent",
                "authorization",
                "accept",
                "content-type",
                "x-route"
            ]
        );
        assert!(request.headers[0].1.starts_with("fly-rafter/"));
        assert_eq!(request.headers[1].1, "Bearer sk-test");
        assert_eq!(request.headers[2].1, "application/json");
        assert_eq!(request.headers[3].1, "application/json");
        assert!(
            !request
                .headers
                .iter()
                .any(|(name, _)| name.starts_with("x-stainless-"))
        );

        // The affinity trio: a session id rides the default openai format
        // only when the compat switch sends it (the default openai
        // endpoint detection does not).
        let mut options = bare_options("acme", "acme-large");
        options.session_id = Some(fr_wire::SessionId::new("sess-1"));
        let mut stream = adapter.stream(options);
        collect(&mut stream);
        let request = &fetch.requests()[1];
        assert!(
            !request
                .headers
                .iter()
                .any(|(name, _)| name == "x-client-request-id"),
            "the default openai endpoint sends no affinity headers"
        );
    }

    /// The affinity faces: the responses client gates on the session id
    /// alone and sends the openai-format pair without the completions
    /// affinity claim; an openrouter-detected completions route sends
    /// `x-session-id` alone (the openai-format completions trio is
    /// structurally unreachable here: its compat switch is withheld and
    /// the detection couples sending with the openrouter format).
    #[test]
    fn affinity_headers_pin_both_protocol_gates() {
        let fetch = RecordedFetch::new(200, "");
        let responses_profile = {
            let raw = serde_json::json!({
                "api": "openai-responses",
                "base-url": "https://gw.example/v1",
                "models": [{ "id": "acme-r" }],
            });
            resolve_route_profile("acme-r", &raw).expect("route resolves")
        };
        let adapter = OpenAiAdapter::new(keyed_options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            vec![with_key(responses_profile, "ACME_KEY")],
            BTreeMap::from([("ACME_KEY".to_owned(), "sk-test".to_owned())]),
        ));
        let mut options = bare_options("acme-r", "acme-r");
        options.session_id = Some(fr_wire::SessionId::new("sess-1"));
        let mut stream = adapter.stream(options);
        collect(&mut stream);
        let request = &fetch.requests()[0];
        let header = |name: &str| {
            request
                .headers
                .iter()
                .find(|(candidate, _)| candidate == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(header("session_id"), Some("sess-1"));
        assert_eq!(header("x-client-request-id"), Some("sess-1"));
        assert_eq!(header("x-session-affinity"), None);

        let openrouter_profile = {
            let raw = serde_json::json!({
                "api": "openai-completions",
                "base-url": "https://openrouter.ai/api/v1",
                "models": [{ "id": "acme-or" }],
            });
            resolve_route_profile("openrouter", &raw).expect("route resolves")
        };
        let adapter = OpenAiAdapter::new(keyed_options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            vec![with_key(openrouter_profile, "ACME_KEY")],
            BTreeMap::from([("ACME_KEY".to_owned(), "sk-test".to_owned())]),
        ));
        let mut options = bare_options("openrouter", "acme-or");
        options.session_id = Some(fr_wire::SessionId::new("sess-2"));
        let mut stream = adapter.stream(options);
        collect(&mut stream);
        let request = &fetch.requests()[1];
        let header = |name: &str| {
            request
                .headers
                .iter()
                .find(|(candidate, _)| candidate == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(header("x-session-id"), Some("sess-2"));
        assert_eq!(header("session_id"), None);
        assert_eq!(header("x-client-request-id"), None);
    }

    /// The unnamed-route credential face: no key and no authorization
    /// header settles as the error event with the verbatim SDK message.
    #[test]
    fn missing_key_settles_as_event_with_sdk_text() {
        let fetch = RecordedFetch::new(200, RECORDED_TOOL_RUN);
        let adapter = OpenAiAdapter::new(options_with(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            vec![route_profile("openai-completions", "https://gw.example/v1")],
        ));
        let mut stream = adapter.stream(bare_options("acme", "acme-large"));
        let chunks = collect(&mut stream);
        assert_eq!(chunks.len(), 2);
        let StreamChunk::Known(KnownStreamChunk::Usage { .. }) = &chunks[0] else {
            panic!("expected the settle usage");
        };
        let FinishReason::Known(FinishReasonKind::Error { failure }) = finish_of(&chunks[1]) else {
            panic!("expected an error finish");
        };
        assert_eq!(failure.message, "No API key for provider: acme");
        assert_eq!(failure.code, "PI_AI_ERROR");
        assert!(fetch.requests().is_empty());
    }

    /// The named-credential texts: the miss, the blank key, and the
    /// header-unsafe key.
    #[test]
    fn credential_texts_pin_the_q66b_adaptation() {
        let profile = with_key(
            route_profile("openai-completions", "https://gw.example/v1"),
            "ACME_KEY",
        );
        let miss = resolve_env_api_key(&|_| None, "acme", &profile).expect_err("miss");
        assert_eq!(miss.code(), MISSING_CREDENTIAL_CODE);
        assert_eq!(
            miss.message(),
            "fr-llm-openai: no credential for provider route \"acme\"; its profile resolves \
             ACME_KEY, which is not set — set ACME_KEY in the fly-rafter configuration, or \
             export it in the launching environment, and remove api-key-env only if this \
             provider should authenticate from the ambient environment"
        );
        let blank =
            resolve_env_api_key(&|_| Some("   ".to_owned()), "acme", &profile).expect_err("blank");
        assert_eq!(blank.code(), INVALID_CREDENTIAL_CODE);
        assert!(
            blank
                .message()
                .contains("is blank; set ACME_KEY to the raw key")
        );
        let unsafe_key = resolve_env_api_key(&|_| Some("sk-\u{00e9}".to_owned()), "acme", &profile)
            .expect_err("unsafe");
        assert_eq!(unsafe_key.code(), INVALID_CREDENTIAL_CODE);
        assert!(
            unsafe_key
                .message()
                .contains("characters no HTTP header can carry")
        );
        // A named miss surfaces as the terminal fr-error record.
        let fetch = RecordedFetch::new(200, RECORDED_TOOL_RUN);
        let adapter = OpenAiAdapter::new(options_with(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            vec![profile],
        ));
        let chunks = collect(&mut adapter.stream(bare_options("acme", "acme-large")));
        assert_eq!(
            record_of(&chunks[0])
                .get("code")
                .and_then(serde_json::Value::as_str),
            Some("MISSING_CREDENTIAL")
        );
        assert!(fetch.requests().is_empty());
    }

    /// The non-2xx faces: completions formats the status and body without
    /// a prefix and classifies; responses adds the provider prefix and
    /// the ChatGPT-usage hint on the subscription-sharing quota failure.
    #[test]
    fn non_2xx_settles_with_protocol_arms() {
        let fetch = RecordedFetch::new(401, r#"{"error":{"message":"bad key"}}"#);
        let adapter = OpenAiAdapter::new(keyed_options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            vec![with_key(
                route_profile("openai-completions", "https://gw.example/v1"),
                "ACME_KEY",
            )],
            BTreeMap::from([("ACME_KEY".to_owned(), "sk-test".to_owned())]),
        ));
        let chunks = collect(&mut adapter.stream(bare_options("acme", "acme-large")));
        assert_eq!(chunks.len(), 2);
        let FinishReason::Known(FinishReasonKind::Error { failure }) = finish_of(&chunks[1]) else {
            panic!("expected an error finish");
        };
        assert_eq!(failure.code, AUTH_CODE);
        assert_eq!(failure.message, r#"401: {"error":{"message":"bad key"}}"#);

        let responses_profile = {
            let raw = serde_json::json!({
                "api": "openai-responses",
                "base-url": "https://gw.example/v1",
                "models": [{ "id": "acme-r" }],
            });
            resolve_route_profile("acme-r", &raw).expect("route resolves")
        };
        let fetch = RecordedFetch::new(
            429,
            r#"{"error":{"code":"subscription_sharing_usage_limit_exceeded","message":"quota"}}"#,
        );
        let adapter = OpenAiAdapter::new(keyed_options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            vec![with_key(responses_profile, "ACME_KEY")],
            BTreeMap::from([("ACME_KEY".to_owned(), "sk-test".to_owned())]),
        ));
        let chunks = collect(&mut adapter.stream(bare_options("acme-r", "acme-r")));
        let FinishReason::Known(FinishReasonKind::Error { failure }) = finish_of(&chunks[1]) else {
            panic!("expected an error finish");
        };
        assert_eq!(
            failure.message,
            "acme-r API error (429): {\"error\":{\"code\":\
             \"subscription_sharing_usage_limit_exceeded\",\"message\":\"quota\"}}\n\
             Check your ChatGPT usage: https://chatgpt.com/settings/usage"
        );
        assert_eq!(failure.code, "RATE_LIMIT");
    }

    /// The done-path settlement: a clean stop with no content blocks is
    /// the degenerate empty completion, and the usage-driven overflow
    /// case re-settles a stopped stream.
    #[test]
    fn done_settlement_pins_empty_and_overflow_faces() {
        let empty_run = concat!(
            "data: {\"id\":\"c1\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let fetch = RecordedFetch::new(200, empty_run);
        let adapter = OpenAiAdapter::new(keyed_options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            vec![with_key(
                route_profile("openai-completions", "https://gw.example/v1"),
                "ACME_KEY",
            )],
            BTreeMap::from([("ACME_KEY".to_owned(), "sk-test".to_owned())]),
        ));
        let chunks = collect(&mut adapter.stream(bare_options("acme", "acme-large")));
        assert_eq!(finish_code(&chunks[chunks.len() - 1]), EMPTY_RESPONSE_CODE);

        let overflow_run = concat!(
            "data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n",
            "data: {\"id\":\"c1\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"usage\":{\"prompt_tokens\":900,\"completion_tokens\":1,",
            "\"total_tokens\":901}}\n\n",
            "data: [DONE]\n\n",
        );
        let mut tiny_profile = with_key(
            route_profile("openai-completions", "https://gw.example/v1"),
            "ACME_KEY",
        );
        tiny_profile.catalog.models[0].context_window = 800;
        let fetch = RecordedFetch::new(200, overflow_run);
        let adapter = OpenAiAdapter::new(keyed_options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            vec![tiny_profile],
            BTreeMap::from([("ACME_KEY".to_owned(), "sk-test".to_owned())]),
        ));
        let chunks = collect(&mut adapter.stream(bare_options("acme", "acme-large")));
        assert_eq!(
            finish_code(&chunks[chunks.len() - 1]),
            CONTEXT_WINDOW_EXCEEDED_CODE
        );
    }

    /// The watchdog face: an idle deadline that fires while the response
    /// waits maps to the adapted verbatim timeout record.
    #[test]
    fn idle_timeout_maps_to_adapted_record() {
        let fetch = RecordedFetch::slow(200, RECORDED_TOOL_RUN, Duration::from_millis(80));
        let mut profile = route_profile("openai-completions", "https://gw.example/v1");
        profile.stream_idle_timeout_ms = 10.0;
        let adapter = OpenAiAdapter::new(keyed_options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            vec![with_key(profile, "ACME_KEY")],
            BTreeMap::from([("ACME_KEY".to_owned(), "sk-test".to_owned())]),
        ));
        let chunks = collect(&mut adapter.stream(bare_options("acme", "acme-large")));
        assert_eq!(chunks.len(), 1);
        let record = record_of(&chunks[0]);
        assert_eq!(
            record.get("message").and_then(serde_json::Value::as_str),
            Some("fr-llm-openai stream idle timeout after 10ms")
        );
        assert_eq!(
            record.get("code").and_then(serde_json::Value::as_str),
            Some(TIMEOUT_CODE)
        );
    }

    /// The caller-cancellation face: an already-aborted signal settles as
    /// the aborted finish with the pi-ai terminal-gate wording.
    #[test]
    fn caller_abort_settles_aborted_finish() {
        let fetch = RecordedFetch::new(200, RECORDED_TOOL_RUN);
        let adapter = OpenAiAdapter::new(keyed_options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            vec![with_key(
                route_profile("openai-completions", "https://gw.example/v1"),
                "ACME_KEY",
            )],
            BTreeMap::from([("ACME_KEY".to_owned(), "sk-test".to_owned())]),
        ));
        let signal = AbortSignal::new();
        signal.abort();
        let mut options = bare_options("acme", "acme-large");
        options.signal = Some(signal);
        let chunks = collect(&mut adapter.stream(options));
        assert_eq!(chunks.len(), 2);
        assert!(
            matches!(
                finish_of(&chunks[1]),
                FinishReason::Known(FinishReasonKind::Aborted { failure }) if failure.message == "Request was aborted"
            ),
            "the aborted finish carries the pi-ai wording"
        );
    }

    /// The whole-request deadline of `timeout-ms` settles with the SDK
    /// timeout wording.
    #[test]
    fn request_deadline_settles_sdk_timeout_text() {
        let fetch = RecordedFetch::slow(200, RECORDED_TOOL_RUN, Duration::from_millis(120));
        let mut profile = route_profile("openai-completions", "https://gw.example/v1");
        profile.timeout_ms = Some(20);
        let started = std::time::Instant::now();
        let mut options = keyed_options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            vec![with_key(profile, "ACME_KEY")],
            BTreeMap::from([("ACME_KEY".to_owned(), "sk-test".to_owned())]),
        );
        options.now_ms =
            Arc::new(move || u64::try_from(started.elapsed().as_millis()).unwrap_or(0));
        let adapter = OpenAiAdapter::new(options);
        let chunks = collect(&mut adapter.stream(bare_options("acme", "acme-large")));
        let FinishReason::Known(FinishReasonKind::Error { failure }) =
            finish_of(&chunks[chunks.len() - 1])
        else {
            panic!("expected an error finish");
        };
        assert_eq!(failure.message, "Request timed out.");
        assert_eq!(failure.code, TIMEOUT_CODE);
    }

    /// The image gates refuse before the network: a non-vision model, and
    /// a vision model without the attachment service.
    #[test]
    fn image_gates_refuse_without_attachments() {
        let fetch = RecordedFetch::new(200, RECORDED_TOOL_RUN);
        let adapter = OpenAiAdapter::new(keyed_options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            vec![with_key(
                route_profile("openai-completions", "https://gw.example/v1"),
                "ACME_KEY",
            )],
            BTreeMap::from([("ACME_KEY".to_owned(), "sk-test".to_owned())]),
        ));
        let mut options = bare_options("acme", "acme-large");
        options.messages = vec![image_user_message()];
        let chunks = collect(&mut adapter.stream(options));
        assert_eq!(
            record_of(&chunks[0])
                .get("message")
                .and_then(serde_json::Value::as_str),
            Some("fr-llm-openai model \"acme-large\" does not support image input")
        );
        assert_eq!(
            record_of(&chunks[0])
                .get("code")
                .and_then(serde_json::Value::as_str),
            Some("UNSUPPORTED_CONTENT")
        );

        let vision_profile = {
            let raw = serde_json::json!({
                "api": "openai-completions",
                "base-url": "https://gw.example/v1",
                "models": [{ "id": "acme-vision", "input": ["text", "image"] }],
            });
            resolve_route_profile("acme", &raw).expect("route resolves")
        };
        let adapter = OpenAiAdapter::new(keyed_options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            vec![with_key(vision_profile, "ACME_KEY")],
            BTreeMap::from([("ACME_KEY".to_owned(), "sk-test".to_owned())]),
        ));
        let mut options = bare_options("acme", "acme-vision");
        options.messages = vec![image_user_message()];
        let chunks = collect(&mut adapter.stream(options));
        assert_eq!(
            record_of(&chunks[0])
                .get("message")
                .and_then(serde_json::Value::as_str),
            Some("fr-llm-openai image input requires the durable attachment service")
        );
        assert!(fetch.requests().is_empty());
    }

    /// An image in a non-user, non-tool message refuses with the adapted
    /// history-shape text.
    #[test]
    fn image_in_assistant_history_refuses() {
        let vision_profile = {
            let raw = serde_json::json!({
                "api": "openai-completions",
                "base-url": "https://gw.example/v1",
                "models": [{ "id": "acme-vision", "input": ["text", "image"] }],
            });
            resolve_route_profile("acme", &raw).expect("route resolves")
        };
        let fetch = RecordedFetch::new(200, RECORDED_TOOL_RUN);
        let mut options = keyed_options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            vec![with_key(vision_profile, "ACME_KEY")],
            BTreeMap::from([("ACME_KEY".to_owned(), "sk-test".to_owned())]),
        );
        // The attachment service present, so the history-shape check is
        // the refusal that fires (it never reads the store).
        options.attachments = Some(Arc::new(EmptyAttachments));
        let adapter = OpenAiAdapter::new(options);
        let assistant_image =
            RequestMessage::Durable(fr_wire::Message::Known(Box::new(assistant_image_message())));
        let mut options = bare_options("acme", "acme-vision");
        options.messages = vec![assistant_image];
        let chunks = collect(&mut adapter.stream(options));
        assert_eq!(
            record_of(&chunks[0])
                .get("message")
                .and_then(serde_json::Value::as_str),
            Some("fr-llm-openai cannot represent an image in an in-history assistant message")
        );
        assert!(fetch.requests().is_empty());
    }

    /// An attachment store the history-shape refusal never reaches.
    struct EmptyAttachments;

    impl AttachmentRead for EmptyAttachments {
        fn read_image_request(
            &self,
            _reference: &ImageAttachmentRef,
            _target: fr_llm::adapter::ImageRequestTarget,
            _signal: Option<&AbortSignal>,
        ) -> Result<
            fr_attachment::request_image::RequestImageAttachment,
            fr_attachment::AttachmentError,
        > {
            unreachable!("the history-shape refusal fires before any read")
        }
    }

    /// The prepared call binds one frozen profiles generation: dispatch
    /// still serves the prepared route after the live profiles change,
    /// and the metadata comes from the same snapshot.
    #[test]
    fn prepare_call_dispatches_the_frozen_generation() {
        let fetch = RecordedFetch::new(200, RECORDED_TOOL_RUN);
        let live: Arc<Mutex<Vec<ResolvedRouteProfile>>> = Arc::new(Mutex::new(vec![with_key(
            route_profile("openai-completions", "https://gw.example/v1"),
            "ACME_KEY",
        )]));
        let resolve: ResolveApiKeyCallback = Arc::new(|provider, profile| {
            resolve_env_api_key(
                &|name: &str| (name == "ACME_KEY").then(|| "sk-test".to_owned()),
                provider,
                profile,
            )
        });
        let options = OpenAiAdapterOptions {
            profiles: {
                let live = Arc::clone(&live);
                Arc::new(move || {
                    live.lock()
                        .expect("live lock")
                        .iter()
                        .map(|profile| (profile.provider.clone(), profile.clone()))
                        .collect()
                })
            },
            resolve_api_key: resolve,
            fetch: Arc::clone(&fetch) as Arc<dyn Fetch>,
            attachments: None,
            image_access: None,
            now_ms: Arc::new(|| 1_000_000),
        };
        let adapter = Arc::new(OpenAiAdapter::new(options));
        let prepared = block_on(adapter.prepare_call("acme", "acme-large"));
        assert_eq!(prepared.model().name, "acme-large");
        // The live route moves away; the frozen generation still serves.
        live.lock().expect("live lock").push(route_profile(
            "openai-responses",
            "https://other.example/v1",
        ));
        let mut stream = prepared
            .dispatch(bare_options("acme", "acme-large"))
            .expect("frozen generation dispatches");
        let chunks = collect(&mut stream);
        assert!(
            !chunks
                .iter()
                .any(|chunk| matches!(chunk, StreamChunk::Other(_)))
        );
        assert_eq!(fetch.requests().len(), 1);
        assert_eq!(
            fetch.requests()[0].url,
            "https://gw.example/v1/chat/completions"
        );
    }

    fn image_user_message() -> RequestMessage {
        RequestMessage::UserInput {
            content: vec![ContentBlock::Known(KnownContentBlock::Image {
                attachment: ImageAttachmentRef {
                    attachment_id: AttachmentId::new("att-1"),
                    media_type: fr_wire::content::ImageMediaType::Png,
                    bytes: 64,
                    width: 10,
                    height: 10,
                    name: None,
                    original_dimensions: None,
                },
                offloaded: None,
            })],
        }
    }

    fn assistant_image_message() -> fr_wire::message::KnownMessage {
        fr_wire::message::KnownMessage::Assistant {
            id: fr_wire::ids::MessageId::new("m-1"),
            content: vec![ContentBlock::Known(KnownContentBlock::Image {
                attachment: ImageAttachmentRef {
                    attachment_id: AttachmentId::new("att-2"),
                    media_type: fr_wire::content::ImageMediaType::Png,
                    bytes: 64,
                    width: 10,
                    height: 10,
                    name: None,
                    original_dimensions: None,
                },
                offloaded: None,
            })],
            source: fr_wire::message::MessageSource::Known(
                fr_wire::message::KnownMessageSource::Model {
                    provider: "acme".to_owned(),
                    model: "acme-vision".to_owned(),
                    replay_state: None,
                    extra: fr_wire::RawObject::new(),
                },
            ),
            extra: fr_wire::RawObject::new(),
        }
    }

    // Rust guideline compliant 2026-10-27
}
