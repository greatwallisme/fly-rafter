//! Connection and request-default shapes (dsh `types.ts`).
//!
//! The resolved connection facts are the output of the plugin's
//! `resolveAdapterOptions` (the one explicit resolve step) and the input of
//! [`crate::serialize`], which re-reads them per operation so a
//! configuration change reaches the next request. The network callback
//! seams of the dsh file (`DeepSeekAdapterOptions`, `DeepSeekRequestAuth`)
//! belong to the network slice and are not ported here.

use fr_llm::adapter::LlmModelInfo;
use fr_llm::error::LlmError;
use fr_llm::projection::ImageAttachmentAccess;
use fr_llm::retry_policy::RetryPolicy;
use fr_wire::ImageAttachmentRef;
use std::sync::Arc;

use crate::models::CatalogModel;
use crate::request_extensions::{ExtensionRequest, PreparedExtensions};
use crate::upload_index::DeepSeekUploadIndex;
use fr_attachment::AttachmentRead;
use fr_net::http::Fetch;

/// Thinking toggle for request defaults (dsh `'enabled' | 'disabled'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingMode {
    /// Extended reasoning enabled.
    Enabled,
    /// Extended reasoning disabled.
    Disabled,
}

/// Reasoning effort level (dsh `'off' | 'low' | 'high' | 'max'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortLevel {
    /// Reasoning off.
    Off,
    /// Low effort.
    Low,
    /// High effort.
    High,
    /// Maximum effort.
    Max,
}

impl EffortLevel {
    /// The wire spelling (dsh's string literals).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Low => "low",
            Self::High => "high",
            Self::Max => "max",
        }
    }
}

/// Adapter-level request defaults (dsh `RequestDefaults`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RequestDefaults {
    /// Thinking mode; `None` leaves the endpoint default in force.
    pub thinking: Option<ThinkingMode>,
    /// Default effort for every request.
    pub reasoning_effort: Option<EffortLevel>,
}

/// Upload expiry, refresh, and quota-recovery policy (dsh
/// `DeepSeekFilePolicy` from `file-store.ts`; carried on the connection
/// because the resolve step validates it with the rest).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilePolicy {
    /// Upload expiry in seconds.
    pub expires_after_seconds: u64,
    /// Refresh margin ahead of expiry in seconds.
    pub refresh_margin_seconds: u64,
    /// Quota-cleanup batch size.
    pub quota_cleanup_batch: u64,
}

/// Validated connection facts for one operation (dsh
/// `DeepSeekConnectionOptions`).
#[derive(Debug, Clone)]
pub struct ConnectionOptions {
    /// Messages API root; custom paths remain unchanged.
    pub base_url: String,
    /// Request defaults applied to every call.
    pub defaults: RequestDefaults,
    /// Default per-request output cap; explicit request values win.
    pub max_tokens: u64,
    /// Positive context capacity used when the selected model has no exact
    /// value.
    pub default_context_window: u64,
    /// Advisory models exposed to discovery consumers; requests remain
    /// unrestricted.
    pub models: Vec<CatalogModel>,
    /// Maximum provider idle time while one stream read is outstanding; a
    /// positive finite float like dsh's `Number` (sub-millisecond values
    /// are representable).
    pub stream_idle_timeout_ms: f64,
    /// Maximum accumulated file-referenced image bytes in one request.
    pub max_request_files_bytes: u64,
    /// Maximum accumulated base64 image payload after Files API fallback.
    pub max_inline_request_image_bytes: u64,
    /// Maximum number of represented images in one request.
    pub max_images_per_request: u64,
    /// Raw-byte removal step after the file-reference bound is exceeded.
    pub image_offload_byte_quantum: u64,
    /// Base64-byte removal step after the inline fallback bound is exceeded.
    pub inline_image_offload_byte_quantum: u64,
    /// Image-count removal step after the count bound is exceeded.
    pub image_offload_count_quantum: u64,
    /// Maximum duration of one request-image Files API resolution; a
    /// positive finite float like dsh's `Number`.
    pub files_api_timeout_ms: f64,
    /// Upload expiry, refresh, and quota-recovery policy.
    pub file_policy: FilePolicy,
    /// Provider-owned model-request retry policy, already resolved.
    pub retry_policy: RetryPolicy,
}

impl ConnectionOptions {
    /// The catalog entry for one model id, if the advisory catalog names it.
    #[must_use]
    pub fn model(&self, id: &str) -> Option<&CatalogModel> {
        self.models.iter().find(|entry| entry.id == id)
    }
}

/// The error facts of one failed model request, as the credential
/// classifier sees them (dsh `onRequestError` receives the raw thrown
/// error).
///
/// The fr mapper classifies this message/code view because the
/// internal failure shapes differ per stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestErrorFacts {
    /// The failure text.
    pub message: String,
    /// The stable machine-routing code; empty when the failure carries
    /// none (dsh errors without a stable code).
    pub code: String,
}

/// Classify a request failure with the captured credential (dsh
/// `onRequestError`).
///
/// `Ok` replaces the original failure with the mapped
/// error; `Err` means the mapper itself failed, which preserves the
/// original error.
pub type AuthErrorMapper =
    Arc<dyn Fn(&RequestErrorFacts) -> Result<LlmError, LlmError> + Send + Sync>;

/// Per-request credential facts (dsh `DeepSeekRequestAuth`).
#[derive(Clone)]
pub struct RequestAuth {
    /// Credential headers sent unchanged to the resolved endpoint.
    pub headers: Vec<(String, String)>,
    /// Classify a request failure using the captured credential.
    pub on_request_error: Option<AuthErrorMapper>,
}

impl std::fmt::Debug for RequestAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestAuth")
            .field("headers", &self.headers)
            .field("on_request_error", &self.on_request_error.is_some())
            .finish()
    }
}

/// The shared upload-reuse store's durable ingredients (dsh
/// `resolveFiles` hands the adapter a `DeepSeekFileStore` instance).
///
/// The fr
/// store borrows the transport and clock seams, so the adapter binds a
/// request-local store to these shared ingredients — the durable upload
/// index stays the shared state, which is what file-id reuse needs.
#[derive(Clone)]
pub struct FilesSeam {
    /// Durable upload index of the shared store.
    pub index: DeepSeekUploadIndex,
    /// Current Unix time in milliseconds (dsh `options.now`).
    pub now_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl Default for FilesSeam {
    /// The default shared store: the fr-home index path and the system
    /// clock (dsh `new DeepSeekFileStore()` resolves the harness home; the
    /// fr-config home resolution is not a dependency of this crate, so the
    /// same `FR_HOME`, else `~/.fly-rafter`, order is applied here).
    fn default() -> Self {
        let home = match std::env::var("FR_HOME") {
            Ok(home) if !home.trim().is_empty() => std::path::PathBuf::from(home),
            _ => std::env::home_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join(".fly-rafter"),
        };
        Self {
            index: DeepSeekUploadIndex::new(crate::upload_index::index_path(&home)),
            now_ms: Arc::new(|| {
                u64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis(),
                )
                .unwrap_or(0)
            }),
        }
    }
}

impl std::fmt::Debug for FilesSeam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilesSeam")
            .field("index", &self.index)
            .finish_non_exhaustive()
    }
}

/// Reports unusable native Messages replay metadata (dsh
/// `onReplayDegrade`): provider, model, then the degradation reason.
pub type ReplayDegradeCallback = Arc<dyn Fn(&str, &str, &str) + Send + Sync>;
/// Reports extension fields omitted after a merge-serialization failure
/// (dsh `onExtensionsOmitted`): provider, model, the omitted field names,
/// then the serialization error.
pub type ExtensionsOmittedCallback = Arc<dyn Fn(&str, &str, &[String], &str) + Send + Sync>;
/// Reports an extension acceptance failure after HTTP 2xx (dsh
/// `onExtensionsUnaccepted`): provider, model, then the error.
pub type ExtensionsUnacceptedCallback = Arc<dyn Fn(&str, &str, &str) + Send + Sync>;
/// Provider-owned catalog availability (dsh `discoverModels`).
pub type DiscoverModelsCallback = Arc<dyn Fn(&str) -> Vec<LlmModelInfo> + Send + Sync>;
/// Current validated connection facts, read once per operation (dsh
/// `options`).
pub type OptionsCallback = Arc<dyn Fn() -> ConnectionOptions + Send + Sync>;
/// Provider-owned connection resolution for exact-model lookups (dsh
/// `connect`; the abort signal of the dsh signature has no trait-face
/// carrier, so prompt settlement belongs to the implementation).
pub type ConnectCallback = Arc<dyn Fn() -> Result<ConnectionOptions, LlmError> + Send + Sync>;
/// Refuse a model before any network I/O (dsh `admit`).
pub type AdmitCallback =
    Arc<dyn Fn(&ConnectionOptions, &str) -> Result<(), LlmError> + Send + Sync>;
/// Resolve authentication from one request's connection snapshot (dsh
/// `resolveAuth`; must not re-read the endpoint).
pub type ResolveAuthCallback =
    Arc<dyn Fn(&ConnectionOptions) -> Result<RequestAuth, LlmError> + Send + Sync>;
/// Execution-world access resolution shared with serialization (dsh
/// `resolveImageAccess`).
pub type ImageAccessCallback =
    Arc<dyn Fn(&ImageAttachmentRef) -> Option<ImageAttachmentAccess> + Send + Sync>;
/// Plugin-contributed request-field preparation (dsh `prepareExtensions`;
/// the no-op default applies when `None`).
pub type PrepareExtensionsCallback =
    Arc<dyn Fn(&ExtensionRequest<'_>) -> Result<PreparedExtensions, String> + Send + Sync>;

/// The adapter's operation-local resolution hooks (dsh
/// `DeepSeekAdapterOptions`).
///
/// The dsh callbacks the M1 protocol posture
/// excludes do not appear: `resolveUserId` fed the `x-deepseek-harness-*`
/// headers, which fr never sends (PRD 19.3), and
/// `resolveAttachments`/`resolveImageAccess` fold into the
/// [`AttachmentRead`] seam beside [`Self::image_access`].
#[derive(Clone)]
pub struct Dependencies {
    /// Report unusable native Messages replay metadata.
    pub on_replay_degrade: Option<ReplayDegradeCallback>,
    /// Report extension fields omitted from one request.
    pub on_extensions_omitted: Option<ExtensionsOmittedCallback>,
    /// Report an extension acceptance failure after HTTP 2xx.
    pub on_extensions_unaccepted: Option<ExtensionsUnacceptedCallback>,
    /// Provider label for selectors; `None` uses the protocol family name.
    pub provider_name: Option<String>,
    /// Provider-owned catalog availability; `None` exposes no entries.
    pub discover_models: Option<DiscoverModelsCallback>,
    /// Current validated connection facts; called once per operation.
    pub options: OptionsCallback,
    /// Resolve the connection for one exact-model lookup or prepared call;
    /// `None` uses [`Self::options`].
    pub connect: Option<ConnectCallback>,
    /// Refuse a model before any network I/O; `None` admits every model.
    pub admit: Option<AdmitCallback>,
    /// Resolve authentication from this request's connection snapshot.
    pub resolve_auth: ResolveAuthCallback,
    /// Shared upload-reuse store ingredients; `None` binds the default
    /// store under the fr home.
    pub resolve_files: Option<FilesSeam>,
    /// The durable attachment seam: reads one route-chosen request image
    /// (dsh `resolveAttachments`; absence rejects image input).
    pub attachments: Option<Arc<dyn AttachmentRead + Send + Sync>>,
    /// Execution-world access resolution shared with serialization (dsh
    /// `resolveImageAccess`; `None` projects offload text without paths).
    pub image_access: Option<ImageAccessCallback>,
    /// Prepare the official API's plugin-contributed top-level fields;
    /// `None` is the no-op production default.
    pub prepare_extensions: Option<PrepareExtensionsCallback>,
    /// The transport seam behind the Messages POST and the file store
    /// (dsh's ambient `fetch`).
    pub fetch: Arc<dyn Fetch>,
    /// Wall clock in Unix milliseconds for retry-after arithmetic (dsh
    /// reads `Date.now()` inside the transport classifier).
    pub now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl Dependencies {
    /// Bind the required seams and the transport; every optional seam
    /// starts omitted and both clocks use the system time.
    pub fn new(
        options: OptionsCallback,
        resolve_auth: ResolveAuthCallback,
        fetch: Arc<dyn Fetch>,
    ) -> Self {
        Self {
            on_replay_degrade: None,
            on_extensions_omitted: None,
            on_extensions_unaccepted: None,
            provider_name: None,
            discover_models: None,
            options,
            connect: None,
            admit: None,
            resolve_auth,
            resolve_files: None,
            attachments: None,
            image_access: None,
            prepare_extensions: None,
            fetch,
            now_ms: Arc::new(|| {
                i64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis(),
                )
                .unwrap_or(i64::MAX)
            }),
        }
    }
}

impl std::fmt::Debug for Dependencies {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dependencies")
            .field("provider_name", &self.provider_name)
            .field("on_replay_degrade", &self.on_replay_degrade.is_some())
            .field(
                "on_extensions_omitted",
                &self.on_extensions_omitted.is_some(),
            )
            .field(
                "on_extensions_unaccepted",
                &self.on_extensions_unaccepted.is_some(),
            )
            .field("discover_models", &self.discover_models.is_some())
            .field("connect", &self.connect.is_some())
            .field("admit", &self.admit.is_some())
            .field("resolve_files", &self.resolve_files.is_some())
            .field("attachments", &self.attachments.is_some())
            .field("image_access", &self.image_access.is_some())
            .field("prepare_extensions", &self.prepare_extensions.is_some())
            .finish_non_exhaustive()
    }
}

// Rust guideline compliant 2026-10-26
