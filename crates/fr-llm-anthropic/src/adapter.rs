//! The anthropic provider adapter (dsh `llm-pi-ai` `adapter.ts` serving
//! pi-ai `api/anthropic-messages.ts`): the one wire protocol's execution
//! shell over the hand-declared route profiles.
//!
//! One operation captures the profiles once, refuses what the route
//! cannot serve (the `stop` face, route ownership, the exact model, the
//! reasoning effort, the credential), and runs the full pipeline —
//! history-shape admission, request-image preparation with the base64
//! offload bound, transcript folding, the anthropic conversion, the
//! streaming POST to `/v1/messages?beta=true`, and SSE translation — on
//! a plugin-owned worker thread (Q69-B) exactly as the dsh generator's
//! async executor does.
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
//! `fr-llm-anthropic` prefix while every code value stays verbatim. The
//! OAuth stealth face follows the credential's own value (`sk-ant-oat`,
//! D15-a): the x-app header, the identity system block, the Claude Code
//! name map, and the two OAuth betas — while the user-agent stays the
//! attribution (dsh `requestHeaders` wins the collision, D13 face).
//!
//! The replay-degrade observation hook of the dsh options is not carried:
//! the fr conversion layers degrade unusable replay state silently (their
//! documented face), so a callback seam here would observe nothing.

use crate::codes::{
    NO_ADAPTER_CODE, UNKNOWN_MODEL_CODE, UNSUPPORTED_CONTENT_CODE, UNSUPPORTED_OPTION_CODE,
    UNSUPPORTED_REASONING_EFFORT_CODE,
};
use crate::config::{CacheRetention, ResolvedRouteProfile};
use crate::convert::{ConvertContext, convert_messages, convert_tools};
use crate::history_transform::{IdNormalization, RouteIdentity, transform_history};
use crate::models::{LevelMapping, Modality, ModelDescriptor, ThinkingLevel};
use crate::params::{build_params, build_request_options, cache_control_value, resolve_betas};
use crate::settle::{Settlement, settle_failure};
use crate::sse::{SseDecoder, SseFrame};
use crate::transcript::fold_transcript;
use crate::translate::AnthropicTranslator;
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
use fr_wire::call_config::TokenUsage;
use fr_wire::ids::ReasoningEffortId;
use fr_wire::{
    AttachmentId, ContentBlock, ImageAttachmentRef, KnownContentBlock, KnownStreamChunk,
    StreamChunk,
};
use futures_core::Stream;
use serde_json::Value;
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
/// The messages resource path with its beta query (npm SDK
/// `resources/beta/messages`, messages.ts:120 verbatim).
const MESSAGES_PATH: &str = "/v1/messages?beta=true";
/// The SDK connection-failure message the settle face classifies
/// (`APIConnectionError`, verbatim; fr-net's own texts never surface).
const CONNECTION_ERROR_MESSAGE: &str = "Connection error.";
/// The SDK request-timeout message (`APIConnectionTimeoutError`,
/// verbatim), the `timeout-ms` deadline's settle face.
const REQUEST_TIMED_OUT_MESSAGE: &str = "Request timed out.";
/// The pi-ai terminal-gate abort throw (anthropic-messages.ts:862
/// verbatim).
const REQUEST_ABORTED_MESSAGE: &str = "Request was aborted";
/// The ambient cache-retention override knob (pi-ai `PI_CACHE_RETENTION`,
/// a provider-side env name kept verbatim by the D10 env-only face).
const PI_CACHE_RETENTION_ENV: &str = "PI_CACHE_RETENTION";
/// The SDK's own request timeout default (`BaseAnthropic.DEFAULT_TIMEOUT`
/// = 10 minutes, client.ts:1676): the `x-stainless-timeout` header sends
/// even without a configured `timeout-ms` (D19 — the anthropic SDK pins
/// it unconditionally, unlike the openai SDK's conditional face).
const SDK_DEFAULT_TIMEOUT_MS: u64 = 600_000;
/// The protocol version header (npm SDK client.ts:1599 verbatim).
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// The OAuth token marker (pi-ai `isOAuthToken`,
/// anthropic-messages.ts:978-980 verbatim; D15-a).
const OAUTH_TOKEN_MARKER: &str = "sk-ant-oat";

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

/// Constructor options for [`AnthropicAdapter`] (dsh
/// `PiAiAdapterOptions` restricted to the fr M1 face: the credential
/// store and auth context are the env-only seam, D16).
#[derive(Clone)]
pub struct AnthropicAdapterOptions {
    /// Current validated profiles by provider route; called once per
    /// operation and frozen for that operation.
    pub profiles: ProfilesCallback,
    /// The credential resolution, once per stream call.
    pub resolve_api_key: ResolveApiKeyCallback,
    /// The transport seam every provider HTTP call goes through.
    pub fetch: Arc<dyn Fetch>,
    /// The optional durable attachment service, resolved at request time.
    pub attachments: Option<Arc<dyn AttachmentRead + Send + Sync>>,
    /// The offloaded-image access facts for the stand-in text.
    pub image_access: Option<ImageAccessCallback>,
    /// The adapter's wall clock.
    pub now_ms: NowMsCallback,
}

impl std::fmt::Debug for AnthropicAdapterOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicAdapterOptions")
            .field("profiles", &"<callback>")
            .field("resolve_api_key", &"<callback>")
            .field("fetch", &"<seam>")
            .field("attachments", &self.attachments.is_some())
            .field("image_access", &self.image_access.is_some())
            .field("now_ms", &"<callback>")
            .finish()
    }
}

/// The env-only credential arm over an injectable environment (D16; dsh
/// `resolveApiKey`, environment face; `ANTHROPIC_API_KEY` the pi-ai
/// provider's own name).
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
    // face (hasRequestAuth decides below).
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
                "fr-llm-anthropic: the API key resolved from {reference} is blank; set \
                 {reference} to the raw key in the fly-rafter configuration, or export it in \
                 the launching environment"
            ),
            ApiKeyRejection::IllegalCharacters => format!(
                "fr-llm-anthropic: the API key resolved from {reference} contains characters no \
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
            "fr-llm-anthropic: no credential for provider route \"{provider}\"; its profile \
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
                "fr-llm-anthropic provider \"{}\" model \"{}\" does not support reasoning effort \
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
/// alone is a request default, and the update modes are absent (A2
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

/// The anthropic provider adapter over the hand-declared route profiles
/// (dsh `PiAiAdapter`).
#[derive(Debug)]
pub struct AnthropicAdapter {
    options: AnthropicAdapterOptions,
}

impl AnthropicAdapter {
    /// Bind the adapter's operation seams.
    #[must_use]
    pub const fn new(options: AnthropicAdapterOptions) -> Self {
        Self { options }
    }

    /// One operation's plan from a frozen profiles view (dsh
    /// `profileOf`/`modelOf`/`resolveReasoningLevel`/`resolveApiKey`, in
    /// the dsh order).
    fn plan_of(
        options: &AnthropicAdapterOptions,
        profiles: &BTreeMap<String, ResolvedRouteProfile>,
        request: &GenerateOptions,
    ) -> Result<RequestPlan, LlmError> {
        if request.stop.is_some() {
            return Err(static_error(
                "fr-llm-anthropic does not support GenerateOptions.stop",
                UNSUPPORTED_OPTION_CODE,
            ));
        }
        let profile = profiles.get(&request.provider).ok_or_else(|| {
            static_error(
                &format!(
                    "fr-llm-anthropic adapter does not own provider \"{}\"",
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
                        "fr-llm-anthropic provider \"{}\" has no configured model \"{}\"",
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
                &format!("fr-llm-anthropic adapter does not own provider \"{provider}\""),
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
                    "fr-llm-anthropic provider \"{provider}\" has no configured model \"{model}\""
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

impl LlmAdapter for AnthropicAdapter {
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
            &format!("fr-llm-anthropic adapter does not own provider \"{provider}\""),
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
                    "fr-llm-anthropic provider \"{provider}\" has no configured model \"{model}\""
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
    options: AnthropicAdapterOptions,
    request: GenerateOptions,
    plan: RequestPlan,
) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
    let (sender, receiver) = channel::<StreamChunk>();
    let signal: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));
    let wake_signal = Arc::clone(&signal);
    let worker_sender = sender.clone();
    let spawned = std::thread::Builder::new()
        .name("fr-llm-anthropic".to_owned())
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
    /// The composed error message (the SDK `APIError` face).
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
    options: &AnthropicAdapterOptions,
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
                        "fr-llm-anthropic stream idle timeout after {}ms",
                        plan.profile.stream_idle_timeout_ms
                    ),
                    TIMEOUT_CODE,
                ))
            } else {
                match failure {
                    RequestFailure::Direct(error) => {
                        if caller_aborted(request) {
                            Err(static_error(
                                "fr-llm-anthropic request aborted by caller",
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
        input_tokens: event
            .usage
            .cache_read_tokens
            .unwrap_or_default()
            .saturating_add(event.usage.input_tokens),
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

/// The history-preparation face (dsh `streamWithSnapshot`'s image gates
/// over `toPiContext`): the two model/service gates, the three
/// history-shape refusals, the deterministic request-image versions, the
/// base64 offload bound, and the offloaded-occurrence projection. The
/// returned request carries the projected messages for the transcript
/// fold; the versions feed the conversion seam.
fn prepare_history(
    options: &AnthropicAdapterOptions,
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
                "fr-llm-anthropic model \"{}\" does not support image input",
                plan.model.id
            ),
            UNSUPPORTED_CONTENT_CODE,
        )));
    }
    let attachments: Option<&dyn AttachmentRead> = if contains_image {
        let Some(store) = options.attachments.as_deref() else {
            return Err(RequestFailure::Direct(static_error(
                "fr-llm-anthropic image input requires the durable attachment service",
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
                    "fr-llm-anthropic request images exceed the {}-byte base64 bound; {offload} \
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
    options: &AnthropicAdapterOptions,
    request: &GenerateOptions,
    plan: &RequestPlan,
    watchdog: &IdleWatchdog,
    sender: &Sender<StreamChunk>,
) -> Result<(), RequestFailure> {
    let (projected_request, versions) = prepare_history(options, request, plan)?;
    // The effective credential (pi-ai `assertRequestAuth` over
    // `hasRequestAuth`): the resolved key (either face), an auth header
    // the route supplies, and nothing else serves the request.
    let key_face = match plan.api_key.as_deref().filter(|key| !key.is_empty()) {
        Some(key) if key.contains(OAUTH_TOKEN_MARKER) => KeyFace::Bearer(key),
        Some(key) => KeyFace::XApiKey(key),
        None if has_auth_header(&plan.profile.headers) => KeyFace::HeaderAuth,
        None => {
            return Err(RequestFailure::event(format!(
                "No API key for provider: {}",
                plan.model.provider
            )));
        }
    };
    run_anthropic(
        options,
        request,
        plan,
        &projected_request,
        &versions,
        &key_face,
        watchdog,
        sender,
    )
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
                "fr-llm-anthropic cannot represent an image in an in-history {role} message"
            )));
        }
        if role == "system"
            && blocks
                .iter()
                .any(|block| !matches!(block, ContentBlock::Known(KnownContentBlock::Text { .. })))
        {
            return Err(shape_rejection(
                "fr-llm-anthropic cannot represent non-text system messages".to_owned(),
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

/// Which authorization face the request carries (pi-ai `createClient`'s
/// three reachable branches, D15-a: the OAuth value detection is the
/// credential's own fact, not a configuration switch).
enum KeyFace<'a> {
    /// `X-Api-Key: <key>` (the SDK's api-key arm; the pi-ai provider
    /// default).
    XApiKey(&'a str),
    /// `Authorization: Bearer <key>` (the OAuth token face: the key's
    /// value carries the `sk-ant-oat` marker).
    Bearer(&'a str),
    /// A route-supplied authorization header stands on its own (pi-ai
    /// `hasRequestAuth`: `authorization`, `x-api-key`, or
    /// `cf-aig-authorization`, case-insensitive, trimmed non-empty).
    HeaderAuth,
}

/// Whether the route headers carry their own authorization (pi-ai
/// `hasHeader` over the three recognized names).
fn has_auth_header(headers: &[(String, String)]) -> bool {
    headers.iter().any(|(name, value)| {
        (name.eq_ignore_ascii_case("authorization")
            || name.eq_ignore_ascii_case("x-api-key")
            || name.eq_ignore_ascii_case("cf-aig-authorization"))
            && !value.trim().is_empty()
    })
}

/// The `x-stainless-*` telemetry family the anthropic SDK sends on
/// every request (client.ts:1593-1595 + detect-platform.ts, D19): the
/// same header names with fr's own client facts rather than a
/// fabricated js/node environment (the decision-37-3 attribution
/// principle; the value-domain deviation is registered in PRD 19.2 item
/// 23). Unlike the openai SDK, the anthropic SDK pins the timeout
/// unconditionally — a route without `timeout-ms` reports the SDK's own
/// 10-minute default — and the single network attempt of D18 keeps the
/// retry count at zero.
fn stainless_headers(timeout_ms: Option<u64>) -> Vec<(String, String)> {
    let mut headers = vec![
        ("x-stainless-retry-count".to_owned(), "0".to_owned()),
        (
            "x-stainless-timeout".to_owned(),
            (timeout_ms.unwrap_or(SDK_DEFAULT_TIMEOUT_MS) / 1000).to_string(),
        ),
    ];
    headers.push(("x-stainless-lang".to_owned(), "rust".to_owned()));
    headers.push((
        "x-stainless-package-version".to_owned(),
        env!("CARGO_PKG_VERSION").to_owned(),
    ));
    headers.push(("x-stainless-os".to_owned(), stainless_os()));
    headers.push(("x-stainless-arch".to_owned(), stainless_arch()));
    headers.push(("x-stainless-runtime".to_owned(), "fly-rafter".to_owned()));
    headers.push((
        "x-stainless-runtime-version".to_owned(),
        env!("CARGO_PKG_VERSION").to_owned(),
    ));
    headers
}

/// The host OS on the SDK's `PlatformName` vocabulary
/// (`MacOS`/`Linux`/`Windows`/..., else `Other:<raw>`).
fn stainless_os() -> String {
    match std::env::consts::OS {
        "macos" => "MacOS".to_owned(),
        "linux" => "Linux".to_owned(),
        "windows" => "Windows".to_owned(),
        "freebsd" => "FreeBSD".to_owned(),
        "openbsd" => "OpenBSD".to_owned(),
        raw => format!("Other:{raw}"),
    }
}

/// The host CPU on the SDK's `Arch` vocabulary (`x32`/`x64`/`arm`/
/// `arm64`, else `Other:<raw>`).
fn stainless_arch() -> String {
    match std::env::consts::ARCH {
        "x86" => "x32".to_owned(),
        "x86_64" => "x64".to_owned(),
        "aarch64" => "arm64".to_owned(),
        "arm" => "arm".to_owned(),
        raw => format!("Other:{raw}"),
    }
}

/// The request headers (the SDK's `buildHeaders` order over the pi-ai
/// client merge, reconstructed in wire order): the SDK's own accept and
/// telemetry first, then the protocol version, the auth face, the OAuth
/// identity header, the route headers (attribution-collision-immune),
/// the JSON content type, and the beta list the resource layer hoists
/// into its `anthropic-beta` header. The attribution user-agent wins the
/// pi-ai client's own name (D13 face — including the OAuth branch, whose
/// `claude-cli` spelling the dsh header merge overrides the same way),
/// and the session-affinity pair is a withheld face that never sends
/// (D20).
///
/// # Panics
///
/// Never: the attribution family always carries the user-agent.
fn build_headers(
    key_face: &KeyFace<'_>,
    is_oauth: bool,
    timeout_ms: Option<u64>,
    betas: &[String],
    route_headers: &[(String, String)],
) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = vec![
        ("accept".to_owned(), "application/json".to_owned()),
        (
            "user-agent".to_owned(),
            attribution_headers()
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
                .map(|(_, value)| value.clone())
                .unwrap_or_default(),
        ),
    ];
    headers.extend(stainless_headers(timeout_ms));
    headers.push((
        "anthropic-dangerous-direct-browser-access".to_owned(),
        "true".to_owned(),
    ));
    headers.push(("anthropic-version".to_owned(), ANTHROPIC_VERSION.to_owned()));
    match key_face {
        KeyFace::XApiKey(key) => headers.push(("x-api-key".to_owned(), (*key).to_owned())),
        KeyFace::Bearer(key) => {
            headers.push(("authorization".to_owned(), format!("Bearer {key}")));
        }
        KeyFace::HeaderAuth => {}
    }
    if is_oauth {
        headers.push(("x-app".to_owned(), "cli".to_owned()));
    }
    for (name, value) in route_headers {
        if !name.eq_ignore_ascii_case("user-agent") {
            headers.push((name.clone(), value.clone()));
        }
    }
    headers.push(("content-type".to_owned(), "application/json".to_owned()));
    if !betas.is_empty() {
        headers.push(("anthropic-beta".to_owned(), betas.join(",")));
    }
    headers
}

/// The SDK's URL join (`client.ts` `buildURL`): a trailing base slash
/// plus a leading path slash collapse to one. The SDK default base
/// (`https://api.anthropic.com`) is dead here — the strict profile face
/// requires a non-empty `base-url`.
fn sdk_url(base: &str, path: &str) -> String {
    if base.ends_with('/') && path.starts_with('/') {
        format!("{base}{}", &path[1..])
    } else {
        format!("{base}{path}")
    }
}

/// The ambient retention resolution (pi-ai `resolveCacheRetention`):
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

/// The wire-carrying value of one media type (the serde rename face).
fn media_type_str(media_type: fr_wire::content::ImageMediaType) -> String {
    serde_json::to_value(media_type)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The adapter-side image bytes resolution: the pre-resolved version of
/// one reference (a miss drops the part — the degenerate
/// attachment-service failure the transform layer already downgraded).
fn image_resolver(
    versions: &HashMap<AttachmentId, fr_attachment::request_image::RequestImageAttachment>,
) -> impl Fn(&ImageAttachmentRef) -> Option<(String, Vec<u8>)> + '_ {
    move |reference: &ImageAttachmentRef| {
        versions
            .get(&reference.attachment_id)
            .map(|version| (media_type_str(version.media_type), version.data.clone()))
    }
}

/// One anthropic request (pi-ai `stream` over `buildParams`).
#[expect(
    clippy::too_many_arguments,
    reason = "the dsh stream closure environment (options, request, plan, projected history, versions, credential, watchdog, sender) passes as explicit parameters"
)]
fn run_anthropic(
    options: &AnthropicAdapterOptions,
    request: &GenerateOptions,
    plan: &RequestPlan,
    projected: &GenerateOptions,
    versions: &HashMap<AttachmentId, fr_attachment::request_image::RequestImageAttachment>,
    key_face: &KeyFace<'_>,
    watchdog: &IdleWatchdog,
    sender: &Sender<StreamChunk>,
) -> Result<(), RequestFailure> {
    let compat = &plan.model.compat;
    let is_oauth = matches!(key_face, KeyFace::Bearer(_));
    let retention = resolve_cache_retention(
        plan.profile.cache_retention,
        std::env::var(PI_CACHE_RETENTION_ENV).ok().as_deref(),
    );
    let wire = assemble_wire(
        plan, request, projected, versions, *compat, is_oauth, retention,
    );
    let headers = build_headers(
        key_face,
        is_oauth,
        plan.profile.timeout_ms,
        &wire.betas,
        &plan.profile.headers,
    );
    // The request deadline starts when the runner takes the request,
    // ahead of the POST (the SDK timeout covers the whole fetch).
    let deadline_ms = request_deadline(options, plan);
    let response = post(
        &options.fetch,
        sdk_url(&plan.model.base_url, MESSAGES_PATH),
        headers,
        &wire.body.to_string(),
    )?;
    if !(200..300).contains(&response.status) {
        return Err(error_event_of_response(response.status, response.body));
    }
    let translator = AnthropicTranslator::new(
        plan.model.id.as_str(),
        plan.model.provider.as_str(),
        Some(plan.model.context_window),
        request.tools.clone().unwrap_or_default(),
        is_oauth,
    );
    consume_response(
        response.body,
        translator,
        request,
        watchdog,
        deadline_ms,
        &options.now_ms,
        sender,
    )
}

/// The assembled wire facts of one request: the body value and the beta
/// header parts (pi-ai `buildParams` plus `getBetaFeatures`).
struct Wire {
    /// The request body.
    body: Value,
    /// The `anthropic-beta` header parts.
    betas: Vec<String>,
}

/// The wire assembly (pi-ai `buildParams` over the converted history):
/// the folded transcript through the anthropic transform and conversion,
/// the request options, the cache stamps, the tools, and the betas.
fn assemble_wire(
    plan: &RequestPlan,
    request: &GenerateOptions,
    projected: &GenerateOptions,
    versions: &HashMap<AttachmentId, fr_attachment::request_image::RequestImageAttachment>,
    compat: crate::compat::AnthropicCompat,
    is_oauth: bool,
    retention: CacheRetention,
) -> Wire {
    let route = RouteIdentity {
        provider: plan.model.provider.as_str(),
        model: plan.model.id.as_str(),
    };
    let supports_images = plan.model.input.contains(&Modality::Image);
    let folded = fold_transcript(projected);
    let turns = transform_history(
        &folded.messages,
        &route,
        supports_images,
        IdNormalization::Anthropic,
    );
    let images = image_resolver(versions);
    let convert_ctx = ConvertContext {
        route,
        compat: &compat,
        is_oauth,
        images: &images,
    };
    let mut messages = convert_messages(&turns, &convert_ctx);
    let request_options = build_request_options(
        &plan.model,
        &compat,
        request,
        plan.effort,
        plan.profile.thinking_budgets.as_ref(),
        retention,
    );
    let system_cache = cache_control_value(retention, &compat);
    // The trailing cache stamp lands on the wire array before the body
    // assembly reads it (the pi-ai `convertMessages` tail).
    if let Some(cache_control) = &system_cache {
        crate::convert::stamp_trailing_cache_control(&mut messages, cache_control);
    }
    let tools = request
        .tools
        .as_deref()
        .map(|tools| convert_tools(tools, &convert_ctx, system_cache.as_ref()));
    let body = build_params(
        &plan.model,
        &request_options,
        &compat,
        is_oauth,
        messages,
        folded.system.as_str(),
        tools,
        system_cache.as_ref(),
    );
    let explicit_beta = plan
        .profile
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("anthropic-beta"))
        .map(|(_, value)| value.as_str());
    let betas = resolve_betas(
        &compat,
        plan.model.reasoning,
        &request_options,
        request
            .tools
            .as_ref()
            .is_some_and(|tools| !tools.is_empty()),
        is_oauth,
        explicit_beta,
    );
    Wire { body, betas }
}

/// Send one streaming POST and return the response (pi-ai
/// `retryProviderRequest` at `maxRetries: 0`: one SDK attempt; transport
/// failures wear the SDK connection message verbatim, D18).
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

/// The non-2xx face (the SDK fetch-layer throw over
/// `APIError.makeMessage`, client.ts:1277-1292): the message composes as
/// `${status} ${body}` — the parsed body carries no top-level `message`
/// key, so the whole JSON stringifies; a non-JSON body wears its raw
/// text; an empty body wears the no-body wording. A redirect a custom
/// `Fetch` still hands back wears the SDK's refused-redirect connection
/// face.
fn error_event_of_response(status: u16, body: Box<dyn Read + Send>) -> RequestFailure {
    let mut body = body;
    let mut bytes = Vec::new();
    let text = body
        .read_to_end(&mut bytes)
        .map(|_| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    if fr_net::http::is_refused_redirect(status) {
        return RequestFailure::event(CONNECTION_ERROR_MESSAGE);
    }
    let message = if text.trim().is_empty() {
        format!("{status} status code (no body)")
    } else {
        match serde_json::from_str::<Value>(&text) {
            Ok(parsed @ Value::Object(_)) => format!("{status} {parsed}"),
            _ => format!("{status} {text}"),
        }
    };
    RequestFailure::event(message)
}

/// Consume one streamed response body: read-window boundary checks
/// (watchdog, caller abort, request deadline), pi-ai-semantics SSE
/// framing, frame translation, and the terminal gate (pi-ai `stream`'s
/// event loop and post-loop checks).
fn consume_response(
    mut body: Box<dyn Read + Send>,
    mut translator: AnthropicTranslator,
    request: &GenerateOptions,
    watchdog: &IdleWatchdog,
    deadline_ms: Option<u64>,
    now_ms: &crate::adapter::NowMsCallback,
    sender: &Sender<StreamChunk>,
) -> Result<(), RequestFailure> {
    let mut parser = SseDecoder::new();
    let mut window = [0_u8; READ_WINDOW_BYTES];
    loop {
        if watchdog.check() || caller_aborted(request) {
            // The watchdog abort surfaces through the outer timeout
            // mapping; a caller abort settles `aborted` (the pi-ai
            // terminal-gate throw, whose wording both faces share).
            return Err(event_failure(&translator, REQUEST_ABORTED_MESSAGE));
        }
        if let Some(deadline) = deadline_ms
            && now_ms() >= deadline
        {
            return Err(event_failure(&translator, REQUEST_TIMED_OUT_MESSAGE));
        }
        let read = body
            .read(&mut window)
            .map_err(|error| event_failure(&translator, error.to_string()))?;
        let frames: Vec<SseFrame> = if read == 0 {
            parser.finish()
        } else {
            watchdog.pulse();
            parser.feed(&window[..read])
        };
        for frame in &frames {
            for chunk in translator
                .feed_frame(frame)
                .map_err(|message| event_failure(&translator, message))?
            {
                sender
                    .send(chunk)
                    .map_err(|_consumer_gone| RequestFailure::Direct(transport_wrapped()))?;
            }
        }
        if read == 0 {
            break;
        }
    }
    // The terminal gate (pi-ai post-loop checks and the done push; the
    // settlement rides the translator's own finish).
    translator
        .end_of_stream()
        .map_err(|message| event_failure(&translator, message))?;
    let chunks = translator.settle_chunks(caller_aborted(request).then_some("aborted"), None);
    for chunk in chunks {
        sender
            .send(chunk)
            .map_err(|_consumer_gone| RequestFailure::Direct(transport_wrapped()))?;
    }
    Ok(())
}

/// The event-face failure carrying the translator's snapshot.
fn event_failure(translator: &AnthropicTranslator, message: impl Into<String>) -> RequestFailure {
    RequestFailure::Event(EventFailure {
        message: message.into(),
        usage: translator.usage_snapshot(),
        blocks_len: translator.blocks_len(),
    })
}

/// The whole-request deadline of `timeout-ms` (dsh SDK request timeout
/// face: the fetch-scoped deadline, started when the protocol runner
/// takes the request).
fn request_deadline(options: &AnthropicAdapterOptions, plan: &RequestPlan) -> Option<u64> {
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
    static_error("fr-llm-anthropic transport failed", TRANSPORT_CODE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::resolve_route_profile;
    use fr_net::http::{HttpResponse, TransportError};
    use serde_json::json;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    /// Records every request and answers from a queue of preset
    /// responses, after an optional pre-response delay.
    struct FakeFetch {
        requests: StdMutex<Vec<HttpRequest>>,
        responses: StdMutex<Vec<HttpResponse>>,
        delay: Option<Duration>,
    }

    impl FakeFetch {
        fn with(status: u16, body: Vec<u8>) -> Arc<Self> {
            Self::delayed(status, body, None)
        }

        fn delayed(status: u16, body: Vec<u8>, delay: Option<Duration>) -> Arc<Self> {
            Arc::new(Self {
                requests: StdMutex::new(Vec::new()),
                responses: StdMutex::new(vec![HttpResponse {
                    status,
                    headers: Vec::new(),
                    body: Box::new(std::io::Cursor::new(body)),
                }]),
                delay,
            })
        }

        fn pair(first: Vec<u8>, second: Vec<u8>) -> Arc<Self> {
            Arc::new(Self {
                requests: StdMutex::new(Vec::new()),
                responses: StdMutex::new(vec![
                    HttpResponse {
                        status: 200,
                        headers: Vec::new(),
                        body: Box::new(std::io::Cursor::new(first)),
                    },
                    HttpResponse {
                        status: 200,
                        headers: Vec::new(),
                        body: Box::new(std::io::Cursor::new(second)),
                    },
                ]),
                delay: None,
            })
        }

        fn requests(&self) -> Vec<HttpRequest> {
            self.requests.lock().expect("test lock").clone()
        }

        fn single_request(&self) -> HttpRequest {
            self.requests
                .lock()
                .expect("test lock")
                .first()
                .cloned()
                .expect("one request")
        }
    }

    impl Fetch for FakeFetch {
        fn fetch(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
            self.requests.lock().expect("test lock").push(request);
            if let Some(delay) = self.delay {
                std::thread::sleep(delay);
            }
            let mut queue = self.responses.lock().expect("test lock");
            if queue.is_empty() {
                return Ok(HttpResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: Box::new(std::io::Cursor::new(Vec::new())),
                });
            }
            Ok(queue.remove(0))
        }
    }

    /// An attachment store whose reads are unreachable (the shape gate
    /// refuses the history before any read runs).
    struct NoopAttachments;

    impl AttachmentRead for NoopAttachments {
        fn read_image_request(
            &self,
            _reference: &ImageAttachmentRef,
            _target: fr_llm::adapter::ImageRequestTarget,
            _signal: Option<&AbortSignal>,
        ) -> Result<
            fr_attachment::request_image::RequestImageAttachment,
            fr_attachment::AttachmentError,
        > {
            unreachable!("the shape gate refuses the history before any attachment read runs")
        }
    }

    fn collect(stream: Pin<Box<dyn Stream<Item = StreamChunk> + Send>>) -> Vec<StreamChunk> {
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        let mut stream = stream;
        let mut chunks = Vec::new();
        loop {
            match stream.as_mut().poll_next(&mut context) {
                Poll::Ready(Some(chunk)) => chunks.push(chunk),
                Poll::Ready(None) => break,
                // The worker thread owns the progress; yield to it until
                // the channel carries the next chunk (or closes).
                Poll::Pending => std::thread::yield_now(),
            }
        }
        chunks
    }

    fn chunk_json(chunk: &StreamChunk) -> Value {
        serde_json::to_value(chunk).expect("chunk serializes")
    }

    fn profile_json(base_url: &str) -> Value {
        json!({
            "api": "anthropic-messages",
            "base-url": base_url,
            "models": [{ "id": "claude-test", "max-tokens": 4096 }],
        })
    }

    fn profiles(base_url: &str) -> BTreeMap<String, ResolvedRouteProfile> {
        let route =
            resolve_route_profile("anthropic", &profile_json(base_url)).expect("profile resolves");
        BTreeMap::from([("anthropic".to_owned(), route)])
    }

    fn options(
        fetch: Arc<dyn Fetch>,
        base_url: &str,
        key: Option<&str>,
    ) -> AnthropicAdapterOptions {
        let key = key.map(str::to_owned);
        let base_url = base_url.to_owned();
        AnthropicAdapterOptions {
            profiles: Arc::new(move || profiles(&base_url)),
            resolve_api_key: Arc::new(move |_provider, _profile| Ok(key.clone())),
            fetch,
            attachments: None,
            image_access: None,
            now_ms: Arc::new(|| {
                u64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .expect("post-epoch clock")
                        .as_millis(),
                )
                .expect("ms fits u64")
            }),
        }
    }

    fn request(model: &str) -> GenerateOptions {
        GenerateOptions {
            provider: "anthropic".to_owned(),
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

    fn adapter_options_profiles(adapter: &AnthropicAdapter) -> crate::adapter::ProfilesCallback {
        Arc::clone(&adapter.options.profiles)
    }

    fn sse_body(frames: &[(&str, &str)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (event, data) in frames {
            body.extend_from_slice(format!("event: {event}\ndata: {data}\n\n").as_bytes());
        }
        body
    }

    const START: (&str, &str) = (
        "message_start",
        r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-test","usage":{"input_tokens":10,"output_tokens":1}}}"#,
    );
    const TEXT_BLOCK: (&str, &str) = (
        "content_block_start",
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
    );
    const TEXT_DELTA: (&str, &str) = (
        "content_block_delta",
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
    );
    const TEXT_STOP: (&str, &str) = (
        "content_block_stop",
        r#"{"type":"content_block_stop","index":0}"#,
    );
    const END_TURN: (&str, &str) = (
        "message_delta",
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#,
    );
    const MESSAGE_STOP: (&str, &str) = ("message_stop", r#"{"type":"message_stop"}"#);

    /// The official endpoint: the beta-qualified path and the full
    /// header order (SDK `buildHeaders` wire order, attribution UA,
    /// unconditional stainless timeout at the SDK default).
    #[test]
    fn official_endpoint_pins_path_and_header_order() {
        let fetch = FakeFetch::with(
            200,
            sse_body(&[
                START,
                TEXT_BLOCK,
                TEXT_DELTA,
                TEXT_STOP,
                END_TURN,
                MESSAGE_STOP,
            ]),
        );
        let adapter = AnthropicAdapter::new(options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            "https://api.anthropic.com",
            Some("sk-test"),
        ));
        collect(adapter.stream(request("claude-test")));
        let sent = fetch.single_request();
        assert_eq!(sent.url, "https://api.anthropic.com/v1/messages?beta=true");
        let names: Vec<&str> = sent.headers.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "accept",
                "user-agent",
                "x-stainless-retry-count",
                "x-stainless-timeout",
                "x-stainless-lang",
                "x-stainless-package-version",
                "x-stainless-os",
                "x-stainless-arch",
                "x-stainless-runtime",
                "x-stainless-runtime-version",
                "anthropic-dangerous-direct-browser-access",
                "anthropic-version",
                "x-api-key",
                "content-type",
            ]
        );
        let value = |name: &str| {
            sent.headers
                .iter()
                .find(|(candidate, _)| candidate == name)
                .map(|(_, value)| value.clone())
                .unwrap_or_default()
        };
        assert!(value("user-agent").starts_with("fly-rafter/"));
        assert_eq!(value("anthropic-version"), "2023-06-01");
        assert_eq!(value("x-api-key"), "sk-test");
        assert_eq!(value("x-stainless-retry-count"), "0");
        assert_eq!(value("x-stainless-timeout"), "600");
        assert_eq!(value("x-stainless-runtime"), "fly-rafter");
    }

    /// A custom base-url redirects the endpoint (the beta path rides
    /// along).
    #[test]
    fn base_url_overrides_endpoint() {
        let fetch = FakeFetch::with(
            200,
            sse_body(&[START, TEXT_BLOCK, TEXT_STOP, END_TURN, MESSAGE_STOP]),
        );
        let adapter = AnthropicAdapter::new(options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            "https://gw.example",
            Some("sk-test"),
        ));
        collect(adapter.stream(request("claude-test")));
        assert_eq!(
            fetch.single_request().url,
            "https://gw.example/v1/messages?beta=true"
        );
    }

    /// An undeclared model refuses without any network.
    #[test]
    fn undeclared_model_refuses_without_catalog() {
        let fetch = FakeFetch::with(200, Vec::new());
        let adapter = AnthropicAdapter::new(options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            "https://api.anthropic.com",
            Some("sk-test"),
        ));
        let chunks = collect(adapter.stream(request("claude-other")));
        assert_eq!(chunks.len(), 1);
        let record = chunk_json(&chunks[0]);
        assert_eq!(record["type"], "fr-error");
        assert_eq!(record["code"], "UNKNOWN_MODEL");
        assert_eq!(
            record["message"],
            "fr-llm-anthropic provider \"anthropic\" has no configured model \"claude-other\""
        );
        assert!(fetch.requests.lock().expect("test lock").is_empty());
    }

    /// `GenerateOptions.stop` refuses on the pi-ai-face adapters.
    #[test]
    fn stop_refuses() {
        let fetch = FakeFetch::with(200, Vec::new());
        let adapter = AnthropicAdapter::new(options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            "https://api.anthropic.com",
            Some("sk-test"),
        ));
        let mut stream_request = request("claude-test");
        stream_request.stop = Some(vec!["END".to_owned()]);
        let chunks = collect(adapter.stream(stream_request));
        let record = chunk_json(&chunks[0]);
        assert_eq!(record["code"], "UNSUPPORTED_OPTION");
        assert_eq!(
            record["message"],
            "fr-llm-anthropic does not support GenerateOptions.stop"
        );
    }

    /// An unsupported reasoning effort refuses with the verbatim text.
    #[test]
    fn unsupported_effort_refuses() {
        let fetch = FakeFetch::with(200, Vec::new());
        let adapter = AnthropicAdapter::new(options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            "https://api.anthropic.com",
            Some("sk-test"),
        ));
        let mut stream_request = request("claude-test");
        stream_request.reasoning_effort = Some(ReasoningEffortId::new("high"));
        let chunks = collect(adapter.stream(stream_request));
        let record = chunk_json(&chunks[0]);
        assert_eq!(record["code"], "UNSUPPORTED_REASONING_EFFORT");
        assert_eq!(
            record["message"],
            "fr-llm-anthropic provider \"anthropic\" model \"claude-test\" does not support \
             reasoning effort \"high\""
        );
    }

    /// No credential settles as the event face, not a fr-error record.
    #[test]
    fn no_key_settles_as_event() {
        let fetch = FakeFetch::with(200, Vec::new());
        let adapter = AnthropicAdapter::new(options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            "https://api.anthropic.com",
            None,
        ));
        let chunks = collect(adapter.stream(request("claude-test")));
        assert_eq!(chunks.len(), 2);
        assert!(matches!(
            chunks[0],
            StreamChunk::Known(KnownStreamChunk::Usage { .. })
        ));
        let finish = chunk_json(&chunks[1]);
        assert_eq!(finish["type"], "finish");
        assert_eq!(finish["reason"]["kind"], "error");
        assert_eq!(finish["reason"]["failure"]["code"], "PI_AI_ERROR");
        assert_eq!(
            finish["reason"]["failure"]["message"],
            "No API key for provider: anthropic"
        );
    }

    /// A full stream settles clean: block chunks, usage before finish,
    /// and the `fr-anthropic` envelope; the body pins the wire keys.
    #[test]
    fn full_stream_settles() {
        let fetch = FakeFetch::with(
            200,
            sse_body(&[
                START,
                TEXT_BLOCK,
                TEXT_DELTA,
                TEXT_STOP,
                END_TURN,
                MESSAGE_STOP,
            ]),
        );
        let adapter = AnthropicAdapter::new(options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            "https://api.anthropic.com",
            Some("sk-test"),
        ));
        let mut stream_request = request("claude-test");
        stream_request.system = Some("You are helpful.".to_owned());
        stream_request.temperature = Some(0.5);
        let chunks = collect(adapter.stream(stream_request));
        let kinds: Vec<String> = chunks
            .iter()
            .map(|chunk| {
                chunk_json(chunk)["type"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "block-start".to_owned(),
                "text-delta".to_owned(),
                "block-end".to_owned(),
                "usage".to_owned(),
                "finish".to_owned(),
            ]
        );
        let finish = chunk_json(chunks.last().expect("finish"));
        assert_eq!(finish["reason"]["kind"], "stop");
        assert_eq!(finish["replayState"]["response"]["kind"], "fr-anthropic");
        let body: Value =
            serde_json::from_slice(fetch.single_request().body.as_deref().unwrap_or_default())
                .expect("body parses");
        assert_eq!(body["model"], "claude-test");
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["temperature"], 0.5);
        assert_eq!(body["system"][0]["text"], "You are helpful.");
        assert_eq!(
            body["system"][0]["cache_control"],
            json!({ "type": "ephemeral" })
        );
    }

    /// The OAuth stealth face: bearer authorization, the cli identity
    /// header, the two OAuth betas, the identity system block first, and
    /// the send-side tool-name map.
    #[test]
    fn oauth_stealth_face() {
        let fetch = FakeFetch::with(
            200,
            sse_body(&[START, TEXT_BLOCK, TEXT_STOP, END_TURN, MESSAGE_STOP]),
        );
        let adapter = AnthropicAdapter::new(options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            "https://api.anthropic.com",
            Some("sk-ant-oat-TOKEN"),
        ));
        let mut stream_request = request("claude-test");
        stream_request.system = Some("sys".to_owned());
        stream_request.tools = Some(vec![fr_wire::call_config::ToolSchema {
            defer_loading: None,
            name: "bash".to_owned(),
            description: "shell".to_owned(),
            parameters: fr_wire::RawObject::new(),
        }]);
        collect(adapter.stream(stream_request));
        let sent = fetch.single_request();
        let value = |name: &str| {
            sent.headers
                .iter()
                .find(|(candidate, _)| candidate == name)
                .map(|(_, value)| value.clone())
                .unwrap_or_default()
        };
        assert_eq!(value("authorization"), "Bearer sk-ant-oat-TOKEN");
        assert!(sent.headers.iter().all(|(name, _)| name != "x-api-key"));
        assert_eq!(value("x-app"), "cli");
        assert_eq!(
            value("anthropic-beta"),
            "claude-code-20250219,oauth-2025-04-20"
        );
        let body: Value =
            serde_json::from_slice(sent.body.as_deref().unwrap_or_default()).expect("body parses");
        assert_eq!(
            body["system"][0]["text"],
            "You are Claude Code, Anthropic's official CLI for Claude."
        );
        assert_eq!(body["system"][1]["text"], "sys");
        assert_eq!(body["tools"][0]["name"], "Bash");
    }

    /// The non-2xx face composes the SDK error message and settles it.
    #[test]
    fn non_2xx_composes_the_api_error_face() {
        let fetch = FakeFetch::with(
            401,
            br#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#.to_vec(),
        );
        let adapter = AnthropicAdapter::new(options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            "https://api.anthropic.com",
            Some("sk-bad"),
        ));
        let chunks = collect(adapter.stream(request("claude-test")));
        assert_eq!(chunks.len(), 2);
        let finish = chunk_json(&chunks[1]);
        assert_eq!(finish["reason"]["kind"], "error");
        assert_eq!(finish["reason"]["failure"]["code"], "AUTH");
        assert_eq!(
            finish["reason"]["failure"]["message"],
            "401 {\"type\":\"error\",\"error\":{\"type\":\"authentication_error\",\"message\":\"invalid x-api-key\"}}"
        );
    }

    /// An in-stream SSE `error` frame settles as the event face.
    #[test]
    fn sse_error_frame_settles() {
        let fetch = FakeFetch::with(200, sse_body(&[START, TEXT_BLOCK, ("error", "overloaded")]));
        let adapter = AnthropicAdapter::new(options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            "https://api.anthropic.com",
            Some("sk-test"),
        ));
        let chunks = collect(adapter.stream(request("claude-test")));
        let finish = chunk_json(chunks.last().expect("finish"));
        assert_eq!(finish["reason"]["kind"], "error");
        assert_eq!(finish["reason"]["failure"]["code"], "PI_AI_ERROR");
        assert_eq!(finish["reason"]["failure"]["message"], "overloaded");
    }

    /// The idle watchdog times a stalled stream out.
    #[test]
    fn idle_watchdog_times_out() {
        let fetch = FakeFetch::delayed(
            200,
            sse_body(&[START, TEXT_BLOCK, TEXT_STOP, END_TURN, MESSAGE_STOP]),
            Some(Duration::from_millis(400)),
        );
        let mut base = profile_json("https://api.anthropic.com");
        base["stream-idle-timeout-ms"] = json!(50);
        let route = resolve_route_profile("anthropic", &base).expect("profile resolves");
        let map = BTreeMap::from([("anthropic".to_owned(), route)]);
        let adapter = AnthropicAdapter::new(AnthropicAdapterOptions {
            profiles: Arc::new(move || map.clone()),
            resolve_api_key: Arc::new(|_p, _r| Ok(Some("sk-test".to_owned()))),
            fetch,
            attachments: None,
            image_access: None,
            now_ms: Arc::new(|| 0),
        });
        let chunks = collect(adapter.stream(request("claude-test")));
        let record = chunk_json(&chunks[0]);
        assert_eq!(record["type"], "fr-error");
        assert_eq!(record["code"], "TIMEOUT");
        assert_eq!(
            record["message"],
            "fr-llm-anthropic stream idle timeout after 50ms"
        );
    }

    /// The `timeout-ms` deadline settles as the SDK timeout face.
    #[test]
    fn timeout_ms_deadline_settles() {
        let fetch = FakeFetch::delayed(
            200,
            sse_body(&[START, TEXT_BLOCK, TEXT_STOP, END_TURN, MESSAGE_STOP]),
            Some(Duration::from_millis(400)),
        );
        let mut base = profile_json("https://api.anthropic.com");
        base["timeout-ms"] = json!(50);
        let route = resolve_route_profile("anthropic", &base).expect("profile resolves");
        let map = BTreeMap::from([("anthropic".to_owned(), route)]);
        let adapter = AnthropicAdapter::new(AnthropicAdapterOptions {
            profiles: Arc::new(move || map.clone()),
            resolve_api_key: Arc::new(|_p, _r| Ok(Some("sk-test".to_owned()))),
            fetch,
            attachments: None,
            image_access: None,
            now_ms: Arc::new(|| {
                u64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .expect("post-epoch clock")
                        .as_millis(),
                )
                .expect("ms fits u64")
            }),
        });
        let chunks = collect(adapter.stream(request("claude-test")));
        let finish = chunk_json(chunks.last().expect("finish"));
        assert_eq!(finish["reason"]["kind"], "error");
        assert_eq!(finish["reason"]["failure"]["code"], "TIMEOUT");
        assert_eq!(finish["reason"]["failure"]["message"], "Request timed out.");
    }

    /// A pre-aborted caller signal settles `aborted`.
    #[test]
    fn caller_abort_settles_aborted() {
        let fetch = FakeFetch::with(200, sse_body(&[START, TEXT_BLOCK]));
        let adapter = AnthropicAdapter::new(options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            "https://api.anthropic.com",
            Some("sk-test"),
        ));
        let mut stream_request = request("claude-test");
        let signal = AbortSignal::new();
        signal.abort();
        stream_request.signal = Some(signal);
        let chunks = collect(adapter.stream(stream_request));
        let finish = chunk_json(chunks.last().expect("finish"));
        assert_eq!(finish["reason"]["kind"], "aborted");
    }

    /// The toolUpdate face (A2): the adapter declares no mid-conversation
    /// tool capability, and every request carries the full declared tool
    /// set — the baseline folding the seam's projection performs — with
    /// no in-history tool-change blocks on the wire.
    #[test]
    fn toolupdate_absent_baseline_folding() {
        let stream = sse_body(&[START, TEXT_BLOCK, TEXT_STOP, END_TURN, MESSAGE_STOP]);
        let fetch = FakeFetch::pair(stream.clone(), stream);
        let adapter = AnthropicAdapter::new(options(
            Arc::clone(&fetch) as Arc<dyn Fetch>,
            "https://api.anthropic.com",
            Some("sk-test"),
        ));
        // No capability in the resolved metadata.
        let profiles = (adapter_options_profiles(&adapter))();
        let (profile, model) = snapshot_of(&profiles, "anthropic", "claude-test").expect("route");
        let resolved = model_info(profile, model);
        assert!(resolved.tool_update.is_none());
        // Two requests with two different full sets: each body carries
        // exactly the set it was given, and no wire system block appears.
        for names in [["alpha", "beta"], ["beta", "gamma"]] {
            let mut stream_request = request("claude-test");
            stream_request.tools = Some(
                names
                    .iter()
                    .map(|name| fr_wire::call_config::ToolSchema {
                        defer_loading: None,
                        name: (*name).to_owned(),
                        description: String::new(),
                        parameters: fr_wire::RawObject::new(),
                    })
                    .collect(),
            );
            collect(adapter.stream(stream_request));
        }
        let requests = fetch.requests();
        assert_eq!(requests.len(), 2);
        for (index, expected) in [(0usize, ["alpha", "beta"]), (1, ["beta", "gamma"])] {
            let body: Value =
                serde_json::from_slice(requests[index].body.as_deref().unwrap_or_default())
                    .expect("body parses");
            let wire_names: Vec<&str> = body["tools"]
                .as_array()
                .expect("tools")
                .iter()
                .map(|tool| tool["name"].as_str().expect("name"))
                .collect();
            assert_eq!(wire_names, expected);
            assert!(!body["messages"].to_string().contains("\"tool_addition\""));
            assert!(!body["messages"].to_string().contains("\"tool_removal\""));
        }
    }

    /// The assistant-image history gate.
    #[test]
    fn assistant_image_history_refuses() {
        let fetch = FakeFetch::with(200, Vec::new());
        let mut base = profile_json("https://api.anthropic.com");
        base["models"][0]["input"] = json!(["text", "image"]);
        let vision_route = resolve_route_profile("anthropic", &base).expect("profile resolves");
        let vision_map = BTreeMap::from([("anthropic".to_owned(), vision_route)]);
        let adapter = AnthropicAdapter::new(AnthropicAdapterOptions {
            profiles: Arc::new(move || vision_map.clone()),
            resolve_api_key: Arc::new(|_p, _r| Ok(Some("sk-test".to_owned()))),
            fetch: fetch as Arc<dyn Fetch>,
            attachments: Some(Arc::new(NoopAttachments)),
            image_access: None,
            now_ms: Arc::new(|| 0),
        });
        let mut stream_request = request("claude-test");
        stream_request.messages = vec![RequestMessage::Durable(fr_wire::Message::Known(Box::new(
            fr_wire::message::KnownMessage::Assistant {
                id: fr_wire::ids::MessageId::new("a"),
                content: vec![ContentBlock::Known(KnownContentBlock::Image {
                    attachment: ImageAttachmentRef {
                        attachment_id: AttachmentId::new("att"),
                        media_type: fr_wire::ImageMediaType::Png,
                        bytes: 4,
                        width: 1,
                        height: 1,
                        name: None,
                        original_dimensions: None,
                    },
                    offloaded: None,
                })],
                source: fr_wire::message::MessageSource::Known(
                    fr_wire::message::KnownMessageSource::Model {
                        provider: "anthropic".to_owned(),
                        model: "claude-test".to_owned(),
                        replay_state: None,
                        extra: fr_wire::RawObject::new(),
                    },
                ),
                extra: fr_wire::RawObject::new(),
            },
        )))];
        let chunks = collect(adapter.stream(stream_request));
        let record = chunk_json(&chunks[0]);
        assert_eq!(record["type"], "fr-error");
        assert_eq!(record["code"], "UNSUPPORTED_CONTENT");
        assert_eq!(
            record["message"],
            "fr-llm-anthropic cannot represent an image in an in-history assistant message"
        );
    }
}
