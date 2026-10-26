//! Connection and request-default shapes (dsh `types.ts`).
//!
//! The resolved connection facts are the output of the plugin's
//! `resolveAdapterOptions` (the one explicit resolve step) and the input of
//! [`crate::serialize`], which re-reads them per operation so a
//! configuration change reaches the next request. The network callback
//! seams of the dsh file (`DeepSeekAdapterOptions`, `DeepSeekRequestAuth`)
//! belong to the network slice and are not ported here.

use fr_llm::retry_policy::RetryPolicy;

use crate::models::CatalogModel;

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

// Rust guideline compliant 2026-10-26
