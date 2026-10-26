//! The `DeepSeek` provider adapter (dsh `adapter.ts`) plus the host
//! first-occurrence warning coordinators of `host.ts` that stay on the
//! library face.
//!
//! The public [`DeepSeekAdapter`] implements
//! [`fr_llm::adapter::LlmAdapter`] over the [`crate::types::Dependencies`]
//! callback bag: one operation reads the connection snapshot once, admits
//! the model, then runs the full request pipeline — image preparation,
//! credential resolution, the Files inline-fallback loop, serialization,
//! request extensions, the beta opt-ins, and the streaming POST — as the
//! dsh generator does.
//!
//! Error channel (Q73-A, PRD decision 41 tail): dsh throws `LlmError`
//! out of the generator for the runtime to catch, while the fr trait
//! face's stream carries chunks only. The internal execution therefore
//! keeps a full `Result` channel, and the public stream emits one final
//! `StreamChunk::Other` record `{"type":"fr-error","message":...,
//! "code":...}` before closing on every failure.
//!
//! Blocking face (Q69-B): the execution runs on a plugin-owned worker
//! thread feeding an `std::sync::mpsc` channel; the stream polls the
//! receiver and shares its waker so any executor sleeps correctly. The
//! idle watchdog is re-checked at every read window, so an abort observed
//! mid-read settles at window granularity — the same nature dsh's undici
//! body reads have.

use crate::codes::{MALFORMED_RESPONSE_CODE, UNKNOWN_MODEL_CODE};
use crate::file_store::{DeepSeekFileStore, FileConnection, FileStoreError, FileStoreOptions};
use crate::http::{HttpMethod, HttpRequest, is_refused_redirect};
use crate::images::{PrepareImagesError, inline_images, prepare_file_ids, prepare_images};
use crate::messages_api::{ANTHROPIC_VERSION, messages_api_root, request_betas};
use crate::model_info::model_info;
use crate::request_extensions::{
    ExtensionRequest, ExtensionsOutcome, OmittedReporter, PreparedExtensionSeam,
    UnacceptedReporter, prepare_request_extensions,
};
use crate::request_files::{FileResolutionFailure, RequestFiles};
use crate::request_pricing::{deep_seek_image_request_pricing, resolve_request_image_target};
use crate::serialize::serialize;
use crate::sse::{SseFrame, SseOutput, SseParser};
use crate::translate::Translator;
use crate::transport::{provider_error, provider_error_detail};
use crate::types::{
    ConnectionOptions, Dependencies, ImageAccessCallback, RequestAuth, RequestErrorFacts,
};
use fr_llm::adapter::{
    AbortSignal, GenerateOptions, ImageRequestPrice, ImageRequestPricing, ImageRequestTarget,
    LlmAdapter, LlmCallConfig, LlmModelInfo, PreparedAdapterCall, ProviderInfo, ResolvedModelInfo,
};
use fr_llm::attribution::attribution_headers;
use fr_llm::error::{ABORTED_CODE, EMPTY_RESPONSE_CODE, LlmError, TIMEOUT_CODE, TRANSPORT_CODE};
use fr_llm::retry_policy::RetryPolicy;
use fr_timeout::{IdleWatchdog, idle_watchdog, timeout_of};
use fr_wire::call_config::CallControls;
use fr_wire::{ContentBlock, ImageAttachmentRef, StreamChunk};
use futures_core::Stream;
use std::collections::HashSet;
use std::io::Read;
use std::pin::Pin;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

/// The Messages transport's idle-watchdog code (dsh `'MESSAGES_IDLE'`).
const MESSAGES_IDLE: &str = "MESSAGES_IDLE";
/// The terminal error record's type tag (Q73-A; adapter-private value
/// domain of the merge-extensible `StreamChunk::Other` face).
const FR_ERROR_TYPE: &str = "fr-error";
/// The protocol family label used when no provider name is set (dsh
/// `'DeepSeek'`).
const PROVIDER_FAMILY_NAME: &str = "DeepSeek";
/// The read window of the response body loop: deadline checks and
/// activity pulses happen at most one window apart.
const READ_WINDOW_BYTES: usize = 8192;

/// `DeepSeek` provider adapter using Messages content and native thinking
/// replay (dsh `DeepSeekAdapter`).
pub struct DeepSeekAdapter {
    dependencies: Dependencies,
}

impl DeepSeekAdapter {
    /// Bind the adapter's operation-local resolution hooks.
    #[must_use]
    pub const fn new(dependencies: Dependencies) -> Self {
        Self { dependencies }
    }

    /// Resolve the connection for one exact-model lookup or prepared call
    /// (dsh `connection(signal)`); `connect` wins over the options
    /// snapshot.
    fn connection(&self) -> Result<ConnectionOptions, LlmError> {
        self.dependencies
            .connect
            .as_ref()
            .map_or_else(|| Ok((self.dependencies.options)()), |connect| connect())
    }

    /// Refuse a model before any network I/O (dsh `admit`).
    fn admit(&self, connection: &ConnectionOptions, model: &str) -> Result<(), LlmError> {
        self.dependencies
            .admit
            .as_ref()
            .map_or(Ok(()), |admit| admit(connection, model))
    }

    /// The image dimensions and byte target one `DeepSeek` model route
    /// applies to every request image (dsh `imageRequestTarget`). The
    /// undeclared-model rejection carries the dsh message and the
    /// `UNKNOWN_MODEL` code verbatim; the trait face has no error channel,
    /// so the registry slice dispatches through this fallible method.
    ///
    /// # Errors
    /// [`UNKNOWN_MODEL_CODE`] when the connection's advisory catalog does
    /// not declare the model.
    pub fn request_image_target(
        &self,
        _provider: &str,
        model: &str,
        image: &ImageAttachmentRef,
    ) -> Result<ImageRequestTarget, LlmError> {
        let connection = (self.dependencies.options)();
        let Some(entry) = connection.model(model) else {
            return Err(static_error(
                &format!("DeepSeek model \"{model}\" has no declared image projection"),
                UNKNOWN_MODEL_CODE,
            ));
        };
        Ok(resolve_request_image_target(entry, image))
    }
}

impl LlmAdapter for DeepSeekAdapter {
    /// Stream one model call: the connection snapshot is read once, the
    /// model is admitted, and the request runs to completion on a
    /// plugin-owned worker thread (dsh `stream`).
    fn stream(&self, options: GenerateOptions) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        let connection = (self.dependencies.options)();
        if let Err(error) = self.admit(&connection, &options.model) {
            return terminal_error_stream(&error);
        }
        spawn_generation(self.dependencies.clone(), options, connection)
    }

    /// The route description with the configured provider label (dsh
    /// `providerInfo`).
    fn provider_info(&self, provider: &str) -> ProviderInfo {
        ProviderInfo {
            id: provider.to_owned(),
            name: self
                .dependencies
                .provider_name
                .clone()
                .unwrap_or_else(|| PROVIDER_FAMILY_NAME.to_owned()),
        }
    }

    /// The live provider-owned retry policy read from the current
    /// connection snapshot (dsh `providerRetryPolicy`).
    fn provider_retry_policy(&self, _provider: &str) -> Option<RetryPolicy> {
        Some((self.dependencies.options)().retry_policy)
    }

    /// The request-image pricing of one route built from the current
    /// snapshot (dsh `imageRequestPricing`).
    fn image_request_pricing(
        &self,
        _provider: &str,
        model: &str,
    ) -> Option<Arc<dyn ImageRequestPricing>> {
        let connection = (self.dependencies.options)();
        Some(Arc::new(RoutePricing {
            connection,
            model: model.to_owned(),
            access: self.dependencies.image_access.clone(),
        }))
    }

    /// Provider-owned catalog availability (dsh `listModels`).
    fn list_models(&self, provider: &str) -> impl Future<Output = Vec<LlmModelInfo>> {
        std::future::ready(
            self.dependencies
                .discover_models
                .as_ref()
                .map_or_else(Vec::new, |discover| discover(provider)),
        )
    }

    /// Resolve all metadata available for one exact model (dsh
    /// `resolveModel`). A `connect` rejection cannot cross the fixed trait
    /// face; it degrades to the options snapshot there, while the
    /// registry slice's dispatch carries the error through the fallible
    /// private `connection` resolution.
    fn resolve_model(
        &self,
        provider: &str,
        model: &str,
    ) -> impl Future<Output = ResolvedModelInfo> {
        let connection = self
            .connection()
            .unwrap_or_else(|_| (self.dependencies.options)());
        std::future::ready(model_info(&connection, provider, model))
    }

    /// Bind exact model metadata and the eventual dispatch to one adapter
    /// generation (dsh `prepareCall`). An admission refusal cannot reject
    /// the trait face, so it surfaces at dispatch time as the terminal
    /// fr-error record of the bound generation's stream.
    fn prepare_call(
        self: &Arc<Self>,
        provider: &str,
        model: &str,
    ) -> impl Future<Output = PreparedAdapterCall> {
        let connection = self
            .connection()
            .unwrap_or_else(|_| (self.dependencies.options)());
        let admission = self.admit(&connection, model);
        let resolved = model_info(&connection, provider, model);
        let config = LlmCallConfig {
            provider: provider.to_owned(),
            model: model.to_owned(),
            controls: CallControls::default(),
            agent_loop: false,
        };
        let dependencies = self.dependencies.clone();
        std::future::ready(PreparedAdapterCall::new(
            resolved,
            config,
            Arc::new(move |options| match &admission {
                Ok(()) => spawn_generation(dependencies.clone(), options, connection.clone()),
                Err(error) => terminal_error_stream(error),
            }),
        ))
    }
}

/// Run one generation's request on a plugin-owned worker thread (Q69-B)
/// and stream its chunks from the receiver. The worker owns the full
/// `Result` channel of the execution and, on failure, sends the terminal
/// fr-error record before closing the channel (Q73-A).
fn spawn_generation(
    dependencies: Dependencies,
    options: GenerateOptions,
    connection: ConnectionOptions,
) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
    let (sender, receiver) = channel::<StreamChunk>();
    let signal: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));
    let wake_signal = Arc::clone(&signal);
    let worker_sender = sender.clone();
    let spawned = std::thread::Builder::new()
        .name("fr-llm-deepseek".to_owned())
        .spawn(move || {
            let outcome = run_stream(&dependencies, &options, &connection, &worker_sender);
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

/// A stream that carries one refusal or transport-start failure record.
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

/// The adapter-private terminal error record (Q73-A): the field names are
/// verbatim and the value domain is owned by this adapter.
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

/// The request-image pricing of one route: the snapshot-frozen catalog
/// facts of `deep_seek_image_request_pricing` behind a `'static` trait
/// object that rebuilds the pricing per call with the live access
/// resolution (dsh constructs the pricing over the same snapshot once).
struct RoutePricing {
    connection: ConnectionOptions,
    model: String,
    access: Option<ImageAccessCallback>,
}

impl ImageRequestPricing for RoutePricing {
    fn price_images(&self, images: &[ContentBlock]) -> Vec<ImageRequestPrice> {
        let access = self.access.clone();
        let resolve = move |reference: &ImageAttachmentRef| {
            access.as_ref().and_then(|resolve| resolve(reference))
        };
        let pricing =
            deep_seek_image_request_pricing(&self.connection, &self.model, Some(&resolve));
        pricing.price_images(images)
    }
}

/// Why one request attempt failed (dsh's thrown-error domain inside the
/// request generator: `LlmError` values pass the generate-level mapping
/// untouched, anything else wraps as `TRANSPORT`).
enum RequestFailure {
    /// A provider-neutral failure.
    Llm(LlmError),
    /// A failure outside the LLM taxonomy (attachment pipeline, local
    /// index, transport seam); carries its detail for the credential
    /// classifier and the transport wrap.
    Other(String),
}

impl RequestFailure {
    /// The message/code view the credential classifier maps (dsh hands
    /// the raw error).
    fn facts(&self) -> RequestErrorFacts {
        match self {
            Self::Llm(error) => RequestErrorFacts {
                message: error.message().to_owned(),
                code: error.code().to_owned(),
            },
            Self::Other(detail) => RequestErrorFacts {
                message: detail.clone(),
                code: String::new(),
            },
        }
    }
}

/// The dsh `generate` mapping over the request outcome: a watchdog
/// timeout wins first, then caller cancellation, then LLM passthrough,
/// and anything else becomes the fixed transport failure (adapter.ts
/// :76-80; fr carries no cause chain because `LlmError` has none).
fn run_stream(
    dependencies: &Dependencies,
    options: &GenerateOptions,
    connection: &ConnectionOptions,
    sender: &Sender<StreamChunk>,
) -> Result<(), LlmError> {
    let watchdog = idle_watchdog(None, connection.stream_idle_timeout_ms, MESSAGES_IDLE)
        .map_err(|_rejected| transport_wrapped())?;
    match run_request(dependencies, options, connection, &watchdog, sender) {
        Ok(()) => Ok(()),
        Err(failure) => {
            if timeout_of(watchdog.signal().reason(), Some(MESSAGES_IDLE)).is_some() {
                Err(static_error(
                    "DeepSeek Messages stream idle timeout",
                    TIMEOUT_CODE,
                ))
            } else if caller_aborted(options) {
                Err(static_error(
                    "DeepSeek Messages request aborted",
                    ABORTED_CODE,
                ))
            } else {
                match failure {
                    RequestFailure::Llm(error) => Err(error),
                    RequestFailure::Other(_detail) => Err(transport_wrapped()),
                }
            }
        }
    }
}

/// One model request (dsh `request`): the credential classifier wraps the
/// whole pipeline, and a mapper failure preserves the original error.
fn run_request(
    dependencies: &Dependencies,
    options: &GenerateOptions,
    connection: &ConnectionOptions,
    watchdog: &IdleWatchdog,
    sender: &Sender<StreamChunk>,
) -> Result<(), RequestFailure> {
    if caller_aborted(options) {
        return Err(RequestFailure::Other("request aborted".to_owned()));
    }
    let auth = (dependencies.resolve_auth)(connection).map_err(RequestFailure::Llm)?;
    match request_attempt(dependencies, options, connection, watchdog, &auth, sender) {
        Ok(()) => Ok(()),
        Err(failure) => {
            let Some(map) = &auth.on_request_error else {
                return Err(failure);
            };
            match map(&failure.facts()) {
                Ok(mapped) => Err(RequestFailure::Llm(mapped)),
                Err(_mapper_failed) => Err(failure),
            }
        }
    }
}

/// The request pipeline's retry loop (dsh `request` body): image
/// preparation, at most one inline fallback, at most one stale-file-id
/// retry, then the streaming POST.
fn request_attempt(
    dependencies: &Dependencies,
    options: &GenerateOptions,
    connection: &ConnectionOptions,
    watchdog: &IdleWatchdog,
    auth: &RequestAuth,
    sender: &Sender<StreamChunk>,
) -> Result<(), RequestFailure> {
    let access = image_access_view(dependencies);
    let attachments: Option<&dyn crate::images::AttachmentRead> =
        dependencies.attachments.as_ref().map(|store| {
            let read: &dyn crate::images::AttachmentRead = store.as_ref();
            read
        });
    let prepared = prepare_images(
        &options.messages,
        connection,
        &options.model,
        attachments,
        &access,
        options.signal.as_ref(),
    )
    .map_err(failure_of_prepare)?;
    let files_seam = dependencies.resolve_files.clone().unwrap_or_default();
    let store_clock = Arc::clone(&files_seam.now_ms);
    let store_view: &(dyn Fn() -> u64 + Sync) = &*store_clock;
    let store = DeepSeekFileStore::new(FileStoreOptions {
        index: files_seam.index,
        now: store_view,
        fetch: &*dependencies.fetch,
    });
    let activity = || watchdog.pulse();
    let file_connection = FileConnection {
        base_url: connection.base_url.clone(),
        headers: auth.headers.clone(),
    };
    let mut files = RequestFiles::new(
        &store,
        &file_connection,
        &connection.file_policy,
        connection.files_api_timeout_ms,
        options.signal.as_ref(),
        &activity,
    );
    let ctx = AttemptContext {
        dependencies,
        options,
        connection,
        watchdog,
        auth,
        sender,
        access: &access,
        messages: &prepared.messages,
        versions: &prepared.versions,
    };
    attempt_loop(&ctx, &mut files)
}

/// The loop-invariant context of one request pipeline (the dsh `request`
/// locals every attempt shares).
struct AttemptContext<'a> {
    dependencies: &'a Dependencies,
    options: &'a GenerateOptions,
    connection: &'a ConnectionOptions,
    watchdog: &'a IdleWatchdog,
    auth: &'a RequestAuth,
    sender: &'a Sender<StreamChunk>,
    access: &'a dyn Fn(&ImageAttachmentRef) -> Option<fr_llm::projection::ImageAttachmentAccess>,
    messages: &'a [fr_llm::adapter::RequestMessage],
    versions: &'a std::collections::HashMap<
        fr_wire::ids::AttachmentId,
        crate::request_image::RequestImageAttachment,
    >,
}

/// One attempt of the retry loop (dsh `request` loop body): the file-id
/// resolution with the inline fallback, serialization, request extensions,
/// the beta opt-ins, and the streaming POST.
fn attempt_loop(
    ctx: &AttemptContext<'_>,
    files: &mut RequestFiles<'_>,
) -> Result<(), RequestFailure> {
    let mut inline = false;
    loop {
        if caller_aborted(ctx.options) {
            return Err(RequestFailure::Other("request aborted".to_owned()));
        }
        files.begin_attempt();
        let file_ids = if inline {
            None
        } else {
            match prepare_file_ids(ctx.messages, ctx.versions, files) {
                Ok(ids) => Some(ids),
                // The raw abort cause propagates unwrapped (dsh rethrows).
                Err(FileResolutionFailure::Abort(cause)) => {
                    return Err(failure_of_store(cause));
                }
                Err(FileResolutionFailure::Other(_unresolved)) => {
                    inline = true;
                    continue;
                }
            }
        };
        if inline {
            inline_images(ctx.messages, ctx.versions, ctx.connection)
                .map_err(RequestFailure::Llm)?;
        }
        let provider = ctx.options.provider.clone();
        let model = ctx.options.model.clone();
        let mut on_degrade = |reason: &str| {
            if let Some(report) = &ctx.dependencies.on_replay_degrade {
                report(&provider, &model, reason);
            }
        };
        let wire = serialize(
            ctx.options,
            ctx.connection,
            ctx.messages,
            ctx.versions,
            ctx.access,
            Some(&mut on_degrade),
            file_ids.as_ref(),
        )
        .map_err(RequestFailure::Llm)?;
        let body = serde_json::to_value(&wire)
            .map_err(|error| RequestFailure::Other(error.to_string()))?;
        let mut extensions = prepare_extensions(ctx.dependencies, ctx.options, &body)?;
        if caller_aborted(ctx.options) {
            return Err(RequestFailure::Other("request aborted".to_owned()));
        }
        let betas = request_betas(
            file_ids.as_ref().is_some_and(|ids| !ids.is_empty()),
            &wire.messages,
        );
        // Take the payload out in place: the outcome carries drop glue, so
        // a partial move out would poison the accept() call.
        let payload = std::mem::take(&mut extensions.payload);
        let response = post_messages(
            ctx.dependencies,
            ctx.auth,
            &betas,
            &ctx.connection.base_url,
            payload.into_bytes(),
        )?;
        if is_refused_redirect(response.status) {
            return Err(RequestFailure::Other("HTTP redirect refused".to_owned()));
        }
        if !(200..300).contains(&response.status) {
            let headers = response.headers.clone();
            handle_error_response(
                ctx.dependencies,
                files,
                response.status,
                response.body,
                headers,
            )?;
            continue;
        }
        extensions.accept();
        consume_response(
            ctx.dependencies,
            response.body,
            ctx.options,
            ctx.watchdog,
            ctx.sender,
        )?;
        return Ok(());
    }
}

/// Build one attempt's request extensions: the two dsh reporter closures,
/// the extension request view, and the contributed preparation seam.
fn prepare_extensions(
    dependencies: &Dependencies,
    options: &GenerateOptions,
    body: &serde_json::Value,
) -> Result<ExtensionsOutcome, RequestFailure> {
    let on_omitted: OmittedReporter = {
        let dependencies = dependencies.clone();
        let provider = options.provider.clone();
        let model = options.model.clone();
        Arc::new(move |fields: &[String], error: &str| {
            if let Some(report) = &dependencies.on_extensions_omitted {
                report(&provider, &model, fields, error);
            }
        })
    };
    let on_unaccepted: UnacceptedReporter = {
        let dependencies = dependencies.clone();
        let provider = options.provider.clone();
        let model = options.model.clone();
        Arc::new(move |error: &str| {
            if let Some(report) = &dependencies.on_extensions_unaccepted {
                report(&provider, &model, error);
            }
        })
    };
    let request = ExtensionRequest {
        body,
        session_id: options.session_id.as_ref().map(fr_wire::SessionId::as_str),
        purpose: options.purpose,
        signal: options.signal.as_ref(),
    };
    let prepare: Option<&PreparedExtensionSeam<'_>> =
        dependencies.prepare_extensions.as_ref().map(|callback| {
            let seam: &PreparedExtensionSeam<'_> = callback.as_ref();
            seam
        });
    prepare_request_extensions(body, &request, prepare, &on_omitted, on_unaccepted)
        .map_err(RequestFailure::Llm)
}

/// Send one attempt's streaming POST with the dsh header order
/// (attribution, content-type, accept, auth, `anthropic-version`,
/// optional beta opt-ins) over the messages root.
fn post_messages(
    dependencies: &Dependencies,
    auth: &RequestAuth,
    betas: &[&str],
    base_url: &str,
    body: Vec<u8>,
) -> Result<crate::http::HttpResponse, RequestFailure> {
    let mut headers: Vec<(String, String)> = Vec::new();
    for (name, value) in attribution_headers() {
        headers.push((name.to_owned(), value));
    }
    headers.push(("content-type".to_owned(), "application/json".to_owned()));
    headers.push(("accept".to_owned(), "text/event-stream".to_owned()));
    headers.extend(auth.headers.iter().cloned());
    headers.push(("anthropic-version".to_owned(), ANTHROPIC_VERSION.to_owned()));
    if !betas.is_empty() {
        headers.push(("anthropic-beta".to_owned(), betas.join(",")));
    }
    let url = format!("{}/messages", messages_api_root(base_url));
    dependencies
        .fetch
        .fetch(HttpRequest {
            method: HttpMethod::Post,
            url,
            headers,
            body: Some(body),
        })
        .map_err(|error| RequestFailure::Other(error.to_string()))
}

/// Classify one non-2xx Messages response (dsh `request` catch): try the
/// bounded stale-file retry first, then rebuild the provider failure with
/// its status, retry-after, request-id, and offload facts. `Ok(())` asks
/// the caller to retry the attempt loop.
fn handle_error_response(
    dependencies: &Dependencies,
    files: &mut RequestFiles<'_>,
    status: u16,
    body: Box<dyn Read + Send>,
    headers: Vec<(String, String)>,
) -> Result<(), RequestFailure> {
    let text = read_response_text(body)?;
    let raw: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    let detail = provider_error_detail(&raw);
    match files.retry(&detail) {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(cause) => return Err(failure_of_store(cause)),
    }
    let lookup = move |name: &str| {
        headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    };
    let failure = provider_error(&raw, Some(status), Some(&lookup), (dependencies.now_ms)());
    let message = files.error_message(status, failure.message(), &detail);
    let facts = failure.failure();
    let rebuilt = LlmError::try_new(
        &message,
        &facts.code,
        facts.status,
        facts.provider_retry_after_ms,
        facts
            .request_id
            .as_ref()
            .map(fr_wire::ProviderRequestId::as_str),
        facts.offload_images,
    )
    .expect("provider failure facts satisfy the LlmError invariants");
    Err(RequestFailure::Llm(rebuilt))
}

/// Consume one streamed response body (dsh `yield* translate(parseSse(
/// response.body, activity))`): incremental UTF-8 decoding at chunk
/// boundaries, SSE framing with per-output activity pulses, per-frame
/// validation identical to [`crate::sse::parse_sse`], and the
/// `message_stop` settlement check at end of body.
fn consume_response(
    dependencies: &Dependencies,
    body: Box<dyn Read + Send>,
    options: &GenerateOptions,
    watchdog: &IdleWatchdog,
    sender: &Sender<StreamChunk>,
) -> Result<(), RequestFailure> {
    let mut reader = body;
    let mut translator = Translator::new(options.model.clone());
    let mut parser = SseParser::default();
    // A UTF-8 code point may straddle read windows; the decoder holds the
    // trailing partial bytes until the point completes (dsh's
    // `TextDecoderStream`).
    let mut pending: Vec<u8> = Vec::new();
    let mut window = [0_u8; READ_WINDOW_BYTES];
    let mut received = false;
    loop {
        if watchdog.check() {
            return Err(RequestFailure::Other("idle deadline elapsed".to_owned()));
        }
        if caller_aborted(options) {
            return Err(RequestFailure::Other("request aborted".to_owned()));
        }
        let read = reader
            .read(&mut window)
            .map_err(|error| RequestFailure::Other(error.to_string()))?;
        if read == 0 {
            if !pending.is_empty() {
                // A truncated trailing code point flushes as replacement
                // text (TextDecoderStream semantics) and carries no frame.
                pending.clear();
                feed_text(
                    "\u{FFFD}",
                    &mut parser,
                    &mut translator,
                    watchdog,
                    sender,
                    dependencies,
                )?;
            }
            if !received {
                return Err(RequestFailure::Llm(static_error(
                    "DeepSeek Messages returned no response body",
                    EMPTY_RESPONSE_CODE,
                )));
            }
            translator.end_of_stream().map_err(RequestFailure::Llm)?;
            return Ok(());
        }
        received = true;
        watchdog.pulse();
        pending.extend_from_slice(&window[..read]);
        while let Err(error) = std::str::from_utf8(&pending) {
            let valid = error.valid_up_to();
            if valid > 0 {
                let text = std::str::from_utf8(&pending[..valid])
                    .expect("valid_up_to bounds a valid UTF-8 prefix");
                feed_text(
                    text,
                    &mut parser,
                    &mut translator,
                    watchdog,
                    sender,
                    dependencies,
                )?;
            }
            if let Some(bad) = error.error_len() {
                // A malformed sequence decodes as one replacement character.
                feed_text(
                    "\u{FFFD}",
                    &mut parser,
                    &mut translator,
                    watchdog,
                    sender,
                    dependencies,
                )?;
                pending.drain(..valid + bad);
            } else {
                pending.drain(..valid);
                break;
            }
        }
        if let Ok(text) = std::str::from_utf8(&pending) {
            feed_text(
                text,
                &mut parser,
                &mut translator,
                watchdog,
                sender,
                dependencies,
            )?;
            pending.clear();
        }
    }
}

/// Feed one decoded text piece through the parser, pulsing on every
/// output: heartbeat comments and dispatched frames are both transport
/// activity for the idle watchdog.
fn feed_text(
    text: &str,
    parser: &mut SseParser,
    translator: &mut Translator,
    watchdog: &IdleWatchdog,
    sender: &Sender<StreamChunk>,
    dependencies: &Dependencies,
) -> Result<(), RequestFailure> {
    for output in parser.feed(text) {
        watchdog.pulse();
        match output {
            SseOutput::Comment => {}
            SseOutput::Frame(frame) => {
                dispatch_frame(&frame, translator, sender, dependencies)?;
            }
        }
    }
    Ok(())
}

/// Validate and translate one dispatched frame with the checks of
/// [`crate::sse::parse_sse`] (JSON shape, object shape, event-type
/// cross-check, in-band error classification).
fn dispatch_frame(
    frame: &SseFrame,
    translator: &mut Translator,
    sender: &Sender<StreamChunk>,
    dependencies: &Dependencies,
) -> Result<(), RequestFailure> {
    let raw: serde_json::Value = serde_json::from_str(&frame.data).map_err(|_invalid_json| {
        RequestFailure::Llm(sse_failure("DeepSeek Messages SSE contains invalid JSON"))
    })?;
    let Some(object) = raw.as_object() else {
        return Err(RequestFailure::Llm(sse_failure(
            "DeepSeek Messages expected a JSON object",
        )));
    };
    let Some(kind) = object.get("type").and_then(serde_json::Value::as_str) else {
        return Err(RequestFailure::Llm(sse_failure(
            "DeepSeek Messages SSE event type mismatch",
        )));
    };
    if frame.event.as_deref().is_some_and(|name| name != kind) {
        return Err(RequestFailure::Llm(sse_failure(
            "DeepSeek Messages SSE event type mismatch",
        )));
    }
    if kind == "error" {
        return Err(RequestFailure::Llm(provider_error(
            &raw,
            None,
            None,
            (dependencies.now_ms)(),
        )));
    }
    for chunk in translator
        .translate_event(object)
        .map_err(RequestFailure::Llm)?
    {
        sender.send(chunk).map_err(|_consumer_gone| {
            RequestFailure::Other("stream consumer dropped".to_owned())
        })?;
    }
    Ok(())
}

/// The execution-world access resolution as the plain closure the
/// serializer and the image preparation take (dsh `this.imageAccess`).
fn image_access_view(
    dependencies: &Dependencies,
) -> impl Fn(&ImageAttachmentRef) -> Option<fr_llm::projection::ImageAttachmentAccess> {
    let resolve = dependencies.image_access.clone();
    move |reference: &ImageAttachmentRef| resolve.as_ref().and_then(|resolve| resolve(reference))
}

fn failure_of_prepare(error: PrepareImagesError) -> RequestFailure {
    match error {
        PrepareImagesError::Llm(error) => RequestFailure::Llm(error),
        PrepareImagesError::Attachment(error) => RequestFailure::Other(error.message().to_owned()),
    }
}

fn failure_of_store(error: FileStoreError) -> RequestFailure {
    match error {
        FileStoreError::Llm(error) => RequestFailure::Llm(error),
        FileStoreError::Attachment(error) => RequestFailure::Other(error.message().to_owned()),
        FileStoreError::Index { message } => RequestFailure::Other(message),
    }
}

fn caller_aborted(options: &GenerateOptions) -> bool {
    options.signal.as_ref().is_some_and(AbortSignal::is_aborted)
}

fn read_response_text(body: Box<dyn Read + Send>) -> Result<String, RequestFailure> {
    let mut body = body;
    let mut bytes = Vec::new();
    body.read_to_end(&mut bytes)
        .map_err(|error| RequestFailure::Other(error.to_string()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn static_error(message: &str, code: &str) -> LlmError {
    LlmError::try_new(message, code, None, None, None, None)
        .expect("static message and code satisfy the LlmError invariants")
}

fn sse_failure(message: &str) -> LlmError {
    static_error(message, MALFORMED_RESPONSE_CODE)
}

fn transport_wrapped() -> LlmError {
    static_error("DeepSeek Messages transport failed", TRANSPORT_CODE)
}

impl std::fmt::Debug for DeepSeekAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeepSeekAdapter")
            .field("dependencies", &self.dependencies)
            .finish()
    }
}

/// The three first-occurrence warning coordinators of the dsh host wiring
/// (dsh `host.ts` `registerDeepSeekProvider`).
///
/// The same condition repeats on every request of a route, so each route
/// logs each condition class only once.
///
/// The host assembly logs the returned text; `None` is the deduplicated
/// repeat.
///
/// The dedupe keys follow the dsh sets: a degraded replay keys on
/// `provider/model\0reason`, the two extension conditions key on
/// `provider/model` alone.
#[derive(Debug, Default)]
pub struct HostWarnings {
    degraded_replays: HashSet<String>,
    omitted_routes: HashSet<String>,
    unaccepted_routes: HashSet<String>,
}

impl HostWarnings {
    /// Record one unusable native Messages replay report (dsh
    /// `onReplayDegrade`); `Some` carries the dsh warn text on the first
    /// occurrence of the provider/model/reason triple.
    pub fn replay_degrade(&mut self, provider: &str, model: &str, reason: &str) -> Option<String> {
        let key = format!("{provider}/{model}\0{reason}");
        self.degraded_replays.insert(key).then(|| {
            format!(
                "llm-deepseek: unusable Messages replay state on assistant history for route \
                 \"{provider}/{model}\"; sending provider-neutral content ({reason})"
            )
        })
    }

    /// Record one request sent without extension fields (dsh
    /// `onExtensionsOmitted`); `Some` carries the dsh warn text on the
    /// first occurrence for the route.
    pub fn extensions_omitted(
        &mut self,
        provider: &str,
        model: &str,
        fields: &[String],
        error: &str,
    ) -> Option<String> {
        let route = format!("{provider}/{model}");
        self.omitted_routes.insert(route.clone()).then(|| {
            format!(
                "llm-deepseek: sending route \"{route}\" without request extension fields {} \
                 because they failed to serialize: {error}",
                fields.join(", ")
            )
        })
    }

    /// Record one post-2xx extension acceptance failure (dsh
    /// `onExtensionsUnaccepted`); `Some` carries the dsh warn text on the
    /// first occurrence for the route.
    pub fn extensions_unaccepted(
        &mut self,
        provider: &str,
        model: &str,
        error: &str,
    ) -> Option<String> {
        let route = format!("{provider}/{model}");
        self.unaccepted_routes.insert(route.clone()).then(|| {
            format!(
                "llm-deepseek: route \"{route}\" request extension acceptance failed; \
                 contributors resend on a later request: {error}"
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codes::INVALID_REQUEST_CODE;
    use crate::http::{Fetch, HttpResponse, TransportError};
    use crate::messages_api::{MESSAGES_FILES_BETA, MESSAGES_TOOL_CHANGES_BETA};
    use crate::types::{FilePolicy, OptionsCallback, RequestDefaults, ResolveAuthCallback};
    use fr_llm::retry_policy::Backoff;
    use fr_wire::RawObject;
    use fr_wire::content::ImageMediaType;
    use fr_wire::ids::{AttachmentId, MessageId};
    use fr_wire::message::{
        KnownMessage, KnownMessageSource, Message as WireMessage, MessageSource,
    };
    use fr_wire::{KnownContentBlock, KnownStreamChunk};
    use std::io::Cursor;
    use std::time::Duration;

    /// A recorded dsh response body: heartbeat comments between frames and
    /// the full event vocabulary of one tool-using run.
    const RECORDED_RUN_BODY: &str = r#": ping

event: message_start
data: {"type":"message_start","message":{"usage":{"input_tokens":5,"output_tokens":1,"cache_read_input_tokens":2}}}

: ping

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"sh","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"cmd\":\"ls\"}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}

event: message_stop
data: {"type":"message_stop"}

"#;

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

    fn test_connection() -> ConnectionOptions {
        ConnectionOptions {
            base_url: "https://api.deepseek.com".to_owned(),
            defaults: RequestDefaults::default(),
            max_tokens: 8_000,
            default_context_window: 128_000,
            models: Vec::new(),
            stream_idle_timeout_ms: 300_000.0,
            max_request_files_bytes: 100 * 1024 * 1024,
            max_inline_request_image_bytes: 20 * 1024 * 1024,
            max_images_per_request: 5,
            image_offload_byte_quantum: 64 * 1024 * 1024,
            inline_image_offload_byte_quantum: 10 * 1024 * 1024,
            image_offload_count_quantum: 20,
            files_api_timeout_ms: 60_000.0,
            file_policy: FilePolicy {
                expires_after_seconds: 7 * 24 * 60 * 60,
                refresh_margin_seconds: 60 * 60,
                quota_cleanup_batch: 100,
            },
            retry_policy: RetryPolicy::Always {
                backoff: Backoff {
                    initial_delay_ms: 500,
                    max_delay_ms: 10_000,
                    jitter_ratio: 0.1,
                },
            },
        }
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

    fn dependencies_with(fetch: Arc<RecordedFetch>) -> Dependencies {
        let transport: Arc<dyn Fetch> = fetch;
        let options: OptionsCallback = Arc::new(test_connection);
        let resolve_auth: ResolveAuthCallback = Arc::new(|_connection| {
            Ok(RequestAuth {
                headers: vec![("x-api-key".to_owned(), "sk-test".to_owned())],
                on_request_error: None,
            })
        });
        let mut dependencies = Dependencies::new(options, resolve_auth, transport);
        dependencies.now_ms = Arc::new(|| 1_000_000);
        dependencies
    }

    fn adapter_with(fetch: Arc<RecordedFetch>) -> DeepSeekAdapter {
        DeepSeekAdapter::new(dependencies_with(fetch))
    }

    /// Minimal spin executor: the adapter's async methods and the test
    /// streams never genuinely suspend (a Pending poll is retried after a
    /// yield), matching the seam tests' driver.
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

    fn record_of(chunk: &StreamChunk) -> &serde_json::Map<String, serde_json::Value> {
        match chunk {
            StreamChunk::Other(record) => record,
            StreamChunk::Known(_known) => panic!("expected a raw record chunk"),
        }
    }

    /// The bound scenario: one recorded SSE body replays through
    /// serialize, POST, SSE framing, and translation as the recorded
    /// chunk sequence, ending in the settled finish.
    #[test]
    fn stream_replays_recorded_sse() {
        let fetch = RecordedFetch::new(200, RECORDED_RUN_BODY);
        let adapter = adapter_with(Arc::clone(&fetch));
        let mut stream = adapter.stream(bare_options("deepseek-official", "deepseek-flash"));
        let chunks = collect(&mut stream);
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
        assert_eq!(
            tags,
            [
                "block-start",
                "text-delta",
                "block-end",
                "block-start",
                "tool-call-delta",
                "tool-call-delta",
                "block-end",
                "usage",
                "finish"
            ]
        );
        let StreamChunk::Known(KnownStreamChunk::Usage { usage }) = &chunks[7] else {
            panic!("usage chunk");
        };
        assert_eq!(usage.input_tokens, 5);
        assert_eq!(usage.output_tokens, 9);
    }

    /// A response factory whose body streams one prefix and then fails
    /// like a dropped connection.
    struct FailingBodyFetch {
        status: u16,
        prefix: &'static [u8],
    }

    impl Fetch for FailingBodyFetch {
        fn fetch(&self, _request: HttpRequest) -> Result<HttpResponse, TransportError> {
            Ok(HttpResponse {
                status: self.status,
                headers: Vec::new(),
                body: Box::new(FailingAfterPrefix {
                    prefix: self.prefix.to_vec(),
                    position: 0,
                }),
            })
        }
    }

    /// The bound scenario: a transport failure mid-stream ends the stream
    /// with exactly one terminal fr-error record carrying the verbatim
    /// transport wrap and its field insertion order.
    #[test]
    fn transport_failure_emits_fr_error_record() {
        let transport: Arc<dyn Fetch> = Arc::new(FailingBodyFetch {
            status: 200,
            prefix: br#"data: {"type":"message_start","message":{"usage":{}}}

"#,
        });
        let mut dependencies = Dependencies::new(
            Arc::new(test_connection),
            Arc::new(|_connection| {
                Ok(RequestAuth {
                    headers: Vec::new(),
                    on_request_error: None,
                })
            }),
            transport,
        );
        dependencies.now_ms = Arc::new(|| 1_000_000);
        let adapter = DeepSeekAdapter::new(dependencies);
        let mut stream = adapter.stream(bare_options("deepseek-official", "m"));
        let chunks = collect(&mut stream);
        assert_eq!(chunks.len(), 1);
        let record = record_of(&chunks[0]);
        assert_eq!(
            record.get("type").and_then(serde_json::Value::as_str),
            Some(FR_ERROR_TYPE)
        );
        assert_eq!(
            record.get("message").and_then(serde_json::Value::as_str),
            Some("DeepSeek Messages transport failed")
        );
        assert_eq!(
            record.get("code").and_then(serde_json::Value::as_str),
            Some(TRANSPORT_CODE)
        );
        // The record's field insertion order is type, message, code.
        let rendered = serde_json::to_string(&chunks[0]).expect("chunk serializes");
        let expected = r#"{"type":"fr-error","message":"DeepSeek Messages transport failed","code":"TRANSPORT"}"#;
        assert!(
            rendered.starts_with(expected),
            "unexpected rendering: {rendered}"
        );
    }

    /// The bound scenario: the POST carries dsh's exact header order —
    /// attribution, content negotiation, credential, protocol version —
    /// with no harness-branded header, and lands on the Messages root.
    #[test]
    fn headers_pin_dsh_order_without_private_headers() {
        let fetch = RecordedFetch::new(200, RECORDED_RUN_BODY);
        let adapter = adapter_with(Arc::clone(&fetch));
        let mut stream = adapter.stream(bare_options("deepseek-official", "deepseek-flash"));
        collect(&mut stream);
        let requests = fetch.requests();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.method, HttpMethod::Post);
        assert_eq!(request.url, "https://api.deepseek.com/v1/messages");
        let names: Vec<&str> = request
            .headers
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "user-agent",
                "content-type",
                "accept",
                "x-api-key",
                "anthropic-version"
            ]
        );
        assert!(request.headers[0].1.starts_with("fly-rafter/"));
        assert_eq!(request.headers[1].1, "application/json");
        assert_eq!(request.headers[2].1, "text/event-stream");
        assert_eq!(request.headers[3].1, "sk-test");
        assert_eq!(request.headers[4].1, "2023-06-01");
        assert!(
            !request
                .headers
                .iter()
                .any(|(name, _)| name.starts_with("x-deepseek-harness-"))
        );
    }

    /// The bound scenario: the beta opt-ins ride exactly when the
    /// serialized body carries file ids or tool-change blocks — a plain
    /// request sends no beta header, a developer tool addition sends the
    /// tool-changes beta alone.
    #[test]
    fn betas_rule_pins_files_and_tool_changes() {
        let fetch = RecordedFetch::new(200, RECORDED_RUN_BODY);
        let adapter = adapter_with(Arc::clone(&fetch));
        let mut stream = adapter.stream(bare_options("deepseek-official", "m"));
        collect(&mut stream);
        let mut options = bare_options("deepseek-official", "m");
        options.messages = vec![durable(user_msg(1, "run")), durable(developer_msg(2, "sh"))];
        let mut stream = adapter.stream(options);
        collect(&mut stream);
        let requests = fetch.requests();
        assert_eq!(requests.len(), 2);
        assert!(
            !requests[0]
                .headers
                .iter()
                .any(|(name, _)| name == "anthropic-beta")
        );
        let beta = requests[1]
            .headers
            .iter()
            .find(|(name, _)| name == "anthropic-beta")
            .map(|(_, value)| value.as_str());
        assert_eq!(beta, Some(MESSAGES_TOOL_CHANGES_BETA));
        assert_ne!(beta, Some(MESSAGES_FILES_BETA));
    }

    /// The bound scenario: a watchdog deadline that fires while the
    /// response waits maps to the verbatim idle-timeout failure.
    #[test]
    fn idle_timeout_maps_to_verbatim_message() {
        let fetch = RecordedFetch::slow(200, RECORDED_RUN_BODY, Duration::from_millis(80));
        let transport: Arc<dyn Fetch> = fetch;
        let mut connection = test_connection();
        connection.stream_idle_timeout_ms = 10.0;
        let mut dependencies = Dependencies::new(
            Arc::new(move || connection.clone()),
            Arc::new(|_connection| {
                Ok(RequestAuth {
                    headers: Vec::new(),
                    on_request_error: None,
                })
            }),
            transport,
        );
        dependencies.now_ms = Arc::new(|| 1_000_000);
        let adapter = DeepSeekAdapter::new(dependencies);
        let mut stream = adapter.stream(bare_options("deepseek-official", "m"));
        let chunks = collect(&mut stream);
        assert_eq!(chunks.len(), 1);
        let record = record_of(&chunks[0]);
        assert_eq!(
            record.get("message").and_then(serde_json::Value::as_str),
            Some("DeepSeek Messages stream idle timeout")
        );
        assert_eq!(
            record.get("code").and_then(serde_json::Value::as_str),
            Some(TIMEOUT_CODE)
        );
    }

    /// The bound scenario: an admission refusal surfaces as the terminal
    /// fr-error record before any network I/O, both on the direct stream
    /// and on a prepared call's dispatch.
    #[test]
    fn prepare_call_refused_admission_streams_fr_error() {
        let fetch = RecordedFetch::new(200, RECORDED_RUN_BODY);
        let mut dependencies = dependencies_with(Arc::clone(&fetch));
        dependencies.admit = Some(Arc::new(|_connection, _model| {
            Err(LlmError::try_new(
                "route refuses this model",
                INVALID_REQUEST_CODE,
                None,
                None,
                None,
                None,
            )
            .expect("static fields"))
        }));
        let adapter = Arc::new(DeepSeekAdapter::new(dependencies));
        let mut stream = adapter.stream(bare_options("deepseek-official", "m"));
        let chunks = collect(&mut stream);
        assert_eq!(chunks.len(), 1);
        let record = record_of(&chunks[0]);
        assert_eq!(
            record.get("message").and_then(serde_json::Value::as_str),
            Some("route refuses this model")
        );
        assert_eq!(
            record.get("code").and_then(serde_json::Value::as_str),
            Some(INVALID_REQUEST_CODE)
        );
        let prepared = block_on(adapter.prepare_call("deepseek-official", "m"));
        let mut stream = prepared
            .dispatch(bare_options("deepseek-official", "m"))
            .expect("matching config dispatches");
        let chunks = collect(&mut stream);
        assert_eq!(chunks.len(), 1);
        assert_eq!(
            record_of(&chunks[0])
                .get("code")
                .and_then(serde_json::Value::as_str),
            Some(INVALID_REQUEST_CODE)
        );
        assert!(fetch.requests().is_empty());
    }

    /// The bound scenario: an undeclared model carries the dsh rejection
    /// message and the `UNKNOWN_MODEL` code.
    #[test]
    fn image_request_target_undeclared_model_message() {
        let adapter = adapter_with(RecordedFetch::new(200, RECORDED_RUN_BODY));
        let image = ImageAttachmentRef {
            attachment_id: AttachmentId::new("att-1"),
            media_type: ImageMediaType::Png,
            bytes: 64,
            width: 10,
            height: 10,
            name: None,
            original_dimensions: None,
        };
        let error = adapter
            .request_image_target("deepseek-official", "m", &image)
            .expect_err("uncatalogued model refuses");
        assert_eq!(
            error.message(),
            "DeepSeek model \"m\" has no declared image projection"
        );
        assert_eq!(error.code(), UNKNOWN_MODEL_CODE);
    }

    /// The bound scenario: the three host warning coordinators emit their
    /// dsh warn text on the first occurrence of their dedupe key and
    /// nothing on repeats.
    #[test]
    fn host_warnings_report_first_occurrence() {
        let mut warnings = HostWarnings::default();
        let first = warnings
            .replay_degrade("p", "m", "no-signature")
            .expect("first occurrence");
        assert_eq!(
            first,
            "llm-deepseek: unusable Messages replay state on assistant history for route \"p/m\"; sending provider-neutral content (no-signature)"
        );
        assert!(warnings.replay_degrade("p", "m", "no-signature").is_none());
        assert!(warnings.replay_degrade("p", "m", "other-reason").is_some());
        assert!(
            warnings
                .replay_degrade("other", "m", "no-signature")
                .is_some()
        );

        let omitted = warnings
            .extensions_omitted("p", "m", &["x-field".to_owned()], "boom")
            .expect("first occurrence");
        assert_eq!(
            omitted,
            "llm-deepseek: sending route \"p/m\" without request extension fields x-field because they failed to serialize: boom"
        );
        assert!(
            warnings
                .extensions_omitted("p", "m", &["y-field".to_owned()], "other")
                .is_none()
        );

        let unaccepted = warnings
            .extensions_unaccepted("p", "m", "late")
            .expect("first occurrence");
        assert_eq!(
            unaccepted,
            "llm-deepseek: route \"p/m\" request extension acceptance failed; contributors resend on a later request: late"
        );
        assert!(warnings.extensions_unaccepted("p", "m", "late").is_none());
    }

    fn durable(known: KnownMessage) -> fr_llm::adapter::RequestMessage {
        fr_llm::adapter::RequestMessage::Durable(WireMessage::Known(Box::new(known)))
    }

    fn user_msg(number: usize, text: &str) -> KnownMessage {
        KnownMessage::User {
            id: MessageId::new(format!("msg-{number}")),
            content: vec![ContentBlock::Known(KnownContentBlock::Text {
                text: text.to_owned(),
            })],
            source: MessageSource::Known(KnownMessageSource::User {
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        }
    }

    fn developer_msg(number: usize, tool: &str) -> KnownMessage {
        KnownMessage::Developer {
            id: MessageId::new(format!("msg-{number}")),
            content: vec![ContentBlock::Known(KnownContentBlock::ToolAddition {
                tool_name: tool.to_owned(),
            })],
            source: MessageSource::Known(KnownMessageSource::User {
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        }
    }

    // Rust guideline compliant 2026-10-26
}
