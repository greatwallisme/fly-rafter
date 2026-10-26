//! Plugin configuration resolution: the one explicit resolve step from raw
//! config to validated protocol settings (dsh `config.ts`
//! `resolveAdapterOptions`).
//!
//! Programmatic construction may bypass schema normalization, so every
//! default and bound is re-judged here. The dsh `protocol`-key guard is
//! unrepresentable in this typed struct (the key cannot exist), and the
//! `imageDetail` migration guard belongs to the config layer above; both
//! stay with fr-config.

use fr_llm::adapter::ModelModality;
use fr_llm::retry_policy::{RetryPolicyInput, resolve_retry_policy};
use fr_wire::call_config::{SystemPromptUpdate, ToolUpdate};
use fr_wire::ids::SAFE_INTEGER_MAX;

use crate::defaults::{
    DEFAULT_CONTEXT_WINDOW, DEFAULT_FILE_EXPIRY_SECONDS, DEFAULT_FILE_QUOTA_CLEANUP_BATCH,
    DEFAULT_FILE_REFRESH_MARGIN_SECONDS, DEFAULT_FILES_API_TIMEOUT_MS,
    DEFAULT_IMAGE_OFFLOAD_BYTE_QUANTUM, DEFAULT_IMAGE_OFFLOAD_COUNT_QUANTUM,
    DEFAULT_INLINE_IMAGE_OFFLOAD_BYTE_QUANTUM, DEFAULT_MAX_IMAGES_PER_REQUEST,
    DEFAULT_MAX_INLINE_REQUEST_IMAGE_BYTES, DEFAULT_MAX_REQUEST_FILES_BYTES, DEFAULT_MAX_TOKENS,
    DEFAULT_REQUEST_IMAGE_MAX_BYTES, DEFAULT_STREAM_IDLE_TIMEOUT_MS,
};
use crate::models::{CatalogModel, ImagePixelBudget, default_models};
use crate::types::{ConnectionOptions, EffortLevel, FilePolicy, RequestDefaults, ThinkingMode};

/// Public API default; the internal endpoint comes from `$DEEPSEEK_BASE_URL`.
pub const PUBLIC_BASE_URL: &str = "https://api.deepseek.com/anthropic";

/// Environment variable naming this provider's endpoint, honored only from
/// trusted layers.
pub const BASE_URL_ENV: &str = "DEEPSEEK_BASE_URL";

/// Raw deployment inputs for the resolver (dsh `Options`): every field
/// optional, every default re-judged at resolve time.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfigOptions {
    /// Endpoint base; falls back to the env layer, then the public API.
    pub base_url: Option<String>,
    /// Deployment thinking policy.
    pub thinking: Option<ThinkingMode>,
    /// Default thinking effort.
    pub reasoning_effort: Option<EffortLevel>,
    /// Default per-request output cap.
    pub max_tokens: Option<u64>,
    /// Positive context capacity used when the selected model has no exact
    /// value.
    pub default_context_window: Option<u64>,
    /// Advisory models; omission resolves the built-in catalog.
    pub models: Option<Vec<CatalogModel>>,
    /// Maximum provider idle time while one stream read is outstanding
    /// (positive finite, capped at `MAX_TIMER_DELAY_MS`).
    pub stream_idle_timeout_ms: Option<f64>,
    /// Maximum accumulated file-referenced image bytes per chat request.
    pub max_request_files_bytes: Option<u64>,
    /// Maximum accumulated base64 image payload after Files API fallback.
    pub max_inline_request_image_bytes: Option<u64>,
    /// Maximum number of represented images per chat request.
    /// Maximum number of represented images per chat request.
    pub max_images_per_request: Option<u64>,
    /// Raw-byte removal step after the request exceeds its file bound.
    pub image_offload_byte_quantum: Option<u64>,
    /// Base64-byte removal step after inline fallback exceeds its bound.
    pub inline_image_offload_byte_quantum: Option<u64>,
    /// Image-count removal step after the request exceeds its count bound.
    pub image_offload_count_quantum: Option<u64>,
    /// Maximum duration of one request-image Files API resolution (positive
    /// finite, capped at `MAX_TIMER_DELAY_MS`).
    pub files_api_timeout_ms: Option<f64>,
    /// Explicit lifetime assigned to each uploaded image.
    pub file_expires_after_seconds: Option<u64>,
    /// Remaining lifetime below which an indexed file is replaced.
    pub file_refresh_margin_seconds: Option<u64>,
    /// Oldest harness-owned files deleted before one quota-recovery retry.
    pub file_quota_cleanup_batch: Option<u64>,
    /// Provider-owned model-request retry policy input.
    pub retry_policy: Option<RetryPolicyInput>,
}

/// Positive finite timer bound (dsh `MAX_TIMER_DELAY_MS` from dsh-timeout);
/// shared with the retry policy's own ceiling.
const MAX_TIMER_DELAY_MS: f64 = 2_147_483_647.0;

/// Positive safe-integer gate shared by the byte and count bounds.
const fn positive_safe_int(value: u64) -> bool {
    value > 0 && value <= SAFE_INTEGER_MAX
}

/// Positive finite timer gate (dsh `Number.isFinite(v) && v > 0 && v <=
/// MAX_TIMER_DELAY_MS`).
fn positive_finite_timer(value: f64) -> bool {
    value.is_finite() && value > 0.0 && value <= MAX_TIMER_DELAY_MS
}

/// Validate one endpoint root: HTTP(S), no userinfo, query, or fragment.
///
/// dsh parses with the WHATWG URL constructor and lets its raw `Invalid
/// URL` throw escape; a JS-engine artifact fr folds into the same
/// validation message.
fn validate_base_url(base_url: &str) -> Result<(), String> {
    let reject = || {
        Err(
            "llm-deepseek: Messages baseURL must be an HTTP(S) root without credentials, \
             query, or fragment"
                .to_owned(),
        )
    };
    let Some((scheme, rest)) = base_url.split_once("://") else {
        return reject();
    };
    if !matches!(scheme, "http" | "https") || rest.is_empty() {
        return reject();
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') || rest.contains('?') || rest.contains('#') {
        return reject();
    }
    Ok(())
}

/// Validate one catalog entry's shape (dsh `resolveModels` checks, in
/// order): identity, caps, modalities, image limits, and route updates.
fn validate_model_entry(model: &CatalogModel) -> Result<(), String> {
    if model.id.is_empty() {
        return Err("llm-deepseek: catalog model ids must be non-empty".to_owned());
    }
    if model.name.as_deref().is_some_and(str::is_empty) {
        return Err(format!(
            "llm-deepseek: catalog model \"{}\" has an empty name",
            model.id
        ));
    }
    if let Some(window) = model.context_window
        && window == 0
    {
        return Err(format!(
            "llm-deepseek: catalog model \"{}\" contextWindow must be a positive integer",
            model.id
        ));
    }
    if let Some(cap) = model.max_tokens
        && !positive_safe_int(cap)
    {
        return Err(format!(
            "llm-deepseek: catalog model \"{}\" maxTokens must be a positive integer",
            model.id
        ));
    }
    let input_modalities = model
        .input_modalities
        .clone()
        .unwrap_or_else(|| vec![ModelModality::Text]);
    if input_modalities.is_empty() {
        return Err(format!(
            "llm-deepseek: catalog model \"{}\" inputModalities must not be empty",
            model.id
        ));
    }
    if input_modalities
        .iter()
        .any(|modality| !matches!(modality, ModelModality::Text | ModelModality::Image))
    {
        return Err(format!(
            "llm-deepseek: catalog model \"{}\" inputModalities must contain only \"text\" and \"image\"",
            model.id
        ));
    }
    let spellings = input_modalities
        .iter()
        .map(|modality| match modality {
            ModelModality::Text => "text",
            ModelModality::Image => "image",
            ModelModality::Other(_) => "other",
        })
        .collect::<std::collections::HashSet<_>>();
    if spellings.len() != input_modalities.len() {
        return Err(format!(
            "llm-deepseek: catalog model \"{}\" inputModalities must not contain duplicates",
            model.id
        ));
    }
    let has_image = input_modalities.contains(&ModelModality::Image);
    if !has_image && (model.image_pixel_budget.is_some() || model.image_max_bytes.is_some()) {
        return Err(format!(
            "llm-deepseek: text-only catalog model \"{}\" cannot declare image request limits",
            model.id
        ));
    }
    if let Some(ImagePixelBudget::Pixels(budget)) = model.image_pixel_budget
        && !positive_safe_int(budget)
    {
        return Err(format!(
            "llm-deepseek: catalog model \"{}\" imagePixelBudget must be \"low\" or a positive safe integer",
            model.id
        ));
    }
    if let Some(budget) = model.image_max_bytes
        && !positive_safe_int(budget)
    {
        return Err(format!(
            "llm-deepseek: catalog model \"{}\" imageMaxBytes must be a positive safe integer",
            model.id
        ));
    }
    if let Some(update) = model.system_prompt_update
        && update != SystemPromptUpdate::InHistory
    {
        return Err(format!(
            "llm-deepseek: catalog model \"{}\" systemPromptUpdate must be \"in-history\" when present",
            model.id
        ));
    }
    if let Some(update) = model.tool_update
        && update != ToolUpdate::InHistory
        && update != ToolUpdate::AdditionOnly
    {
        return Err(format!(
            "llm-deepseek: catalog model \"{}\" toolUpdate must be \"in-history\" or \"addition-only\" when present",
            model.id
        ));
    }
    Ok(())
}

/// Resolve, validate, and detach the advisory model catalog (dsh
/// `resolveModels`).
///
/// Defaults fill per entry and every bound is re-judged.
fn resolve_models(models: Option<&[CatalogModel]>) -> Result<Vec<CatalogModel>, String> {
    // Omission resolves the built-in catalog (dsh `models ?? DEFAULT_MODELS`).
    let source: Vec<CatalogModel> =
        models.map_or_else(|| default_models().to_vec(), <[CatalogModel]>::to_vec);
    let mut seen = std::collections::HashSet::new();
    let mut resolved = Vec::new();
    for model in &source {
        validate_model_entry(model)?;
        let input_modalities = model
            .input_modalities
            .clone()
            .unwrap_or_else(|| vec![ModelModality::Text]);
        let has_image = input_modalities.contains(&ModelModality::Image);
        if !seen.insert(model.id.clone()) {
            return Err(format!(
                "llm-deepseek: duplicate catalog model \"{}\"",
                model.id
            ));
        }
        resolved.push(CatalogModel {
            id: model.id.clone(),
            name: model.name.clone(),
            description: model.description.clone(),
            context_window: model.context_window,
            max_tokens: model.max_tokens,
            input_modalities: Some(input_modalities),
            image_pixel_budget: model.image_pixel_budget.filter(|_| has_image),
            image_max_bytes: has_image.then(|| {
                model
                    .image_max_bytes
                    .unwrap_or(DEFAULT_REQUEST_IMAGE_MAX_BYTES)
            }),
            system_prompt_update: model.system_prompt_update,
            tool_update: model.tool_update,
        });
    }
    Ok(resolved)
}

/// The request-image byte/count bounds with their removal quanta (dsh
/// checks, in order): every bound is a positive safe integer and each
/// quantum never exceeds its own bound.
struct ImageBounds {
    max_request_files_bytes: u64,
    max_inline_request_image_bytes: u64,
    max_images_per_request: u64,
    image_offload_byte_quantum: u64,
    inline_image_offload_byte_quantum: u64,
    image_offload_count_quantum: u64,
}

fn resolve_image_bounds(options: &ConfigOptions) -> Result<ImageBounds, String> {
    let max_request_files_bytes = options
        .max_request_files_bytes
        .unwrap_or(DEFAULT_MAX_REQUEST_FILES_BYTES);
    if !positive_safe_int(max_request_files_bytes) {
        return Err(
            "llm-deepseek: maxRequestFilesBytes must be a positive safe integer".to_owned(),
        );
    }
    let max_inline_request_image_bytes = options
        .max_inline_request_image_bytes
        .unwrap_or(DEFAULT_MAX_INLINE_REQUEST_IMAGE_BYTES);
    if !positive_safe_int(max_inline_request_image_bytes) {
        return Err(
            "llm-deepseek: maxInlineRequestImageBytes must be a positive safe integer".to_owned(),
        );
    }
    let max_images_per_request = options
        .max_images_per_request
        .unwrap_or(DEFAULT_MAX_IMAGES_PER_REQUEST);
    if !positive_safe_int(max_images_per_request) {
        return Err("llm-deepseek: maxImagesPerRequest must be a positive safe integer".to_owned());
    }
    let image_offload_byte_quantum = options
        .image_offload_byte_quantum
        .unwrap_or(DEFAULT_IMAGE_OFFLOAD_BYTE_QUANTUM);
    if !positive_safe_int(image_offload_byte_quantum) {
        return Err(
            "llm-deepseek: imageOffloadByteQuantum must be a positive safe integer".to_owned(),
        );
    }
    if image_offload_byte_quantum > max_request_files_bytes {
        return Err(
            "llm-deepseek: imageOffloadByteQuantum must not exceed maxRequestFilesBytes".to_owned(),
        );
    }
    let inline_image_offload_byte_quantum = options
        .inline_image_offload_byte_quantum
        .unwrap_or(DEFAULT_INLINE_IMAGE_OFFLOAD_BYTE_QUANTUM);
    if !positive_safe_int(inline_image_offload_byte_quantum) {
        return Err(
            "llm-deepseek: inlineImageOffloadByteQuantum must be a positive safe integer"
                .to_owned(),
        );
    }
    if inline_image_offload_byte_quantum > max_inline_request_image_bytes {
        return Err(
        "llm-deepseek: inlineImageOffloadByteQuantum must not exceed maxInlineRequestImageBytes"
            .to_owned(),
    );
    }
    let image_offload_count_quantum = options
        .image_offload_count_quantum
        .unwrap_or(DEFAULT_IMAGE_OFFLOAD_COUNT_QUANTUM);
    if !positive_safe_int(image_offload_count_quantum) {
        return Err(
            "llm-deepseek: imageOffloadCountQuantum must be a positive safe integer".to_owned(),
        );
    }
    if image_offload_count_quantum > max_images_per_request {
        return Err(
            "llm-deepseek: imageOffloadCountQuantum must not exceed maxImagesPerRequest".to_owned(),
        );
    }
    Ok(ImageBounds {
        max_request_files_bytes,
        max_inline_request_image_bytes,
        max_images_per_request,
        image_offload_byte_quantum,
        inline_image_offload_byte_quantum,
        image_offload_count_quantum,
    })
}

/// The upload expiry, refresh, and quota-recovery policy (dsh checks, in
/// order).
fn resolve_file_policy(options: &ConfigOptions) -> Result<FilePolicy, String> {
    let expires_after_seconds = options
        .file_expires_after_seconds
        .unwrap_or(DEFAULT_FILE_EXPIRY_SECONDS);
    if !(3_600..=2_592_000).contains(&expires_after_seconds) {
        return Err(
            "llm-deepseek: fileExpiresAfterSeconds must be an integer from 3600 through 2592000"
                .to_owned(),
        );
    }
    let refresh_margin_seconds = options
        .file_refresh_margin_seconds
        .unwrap_or(DEFAULT_FILE_REFRESH_MARGIN_SECONDS);
    if refresh_margin_seconds >= expires_after_seconds {
        return Err(
            "llm-deepseek: fileRefreshMarginSeconds must be a non-negative integer below fileExpiresAfterSeconds"
                .to_owned(),
        );
    }
    let quota_cleanup_batch = options
        .file_quota_cleanup_batch
        .unwrap_or(DEFAULT_FILE_QUOTA_CLEANUP_BATCH);
    if !(1..=1_000).contains(&quota_cleanup_batch) {
        return Err(
            "llm-deepseek: fileQuotaCleanupBatch must be an integer from 1 through 1000".to_owned(),
        );
    }
    Ok(FilePolicy {
        expires_after_seconds,
        refresh_margin_seconds,
        quota_cleanup_batch,
    })
}

/// The one explicit resolve step from raw config to validated protocol
/// settings (dsh `resolveAdapterOptions`).
///
/// This runs for the composition entry at load, and for each settings
/// snapshot at its first use.
///
/// `base_url_env` carries the trusted-layer `$DEEPSEEK_BASE_URL` value; the
/// real environment read belongs to the composition above this crate.
///
/// # Errors
/// A plain diagnostic `String` (dsh throws plain `Error` here, not
/// `LlmError`) for every bound and shape violation, in dsh's check order.
pub fn resolve_adapter_options(
    options: &ConfigOptions,
    base_url_env: Option<&str>,
) -> Result<ConnectionOptions, String> {
    if options.thinking == Some(ThinkingMode::Disabled)
        && options
            .reasoning_effort
            .is_some_and(|effort| effort != EffortLevel::Off)
    {
        return Err(
            "llm-deepseek: only reasoningEffort \"off\" can be configured when thinking is disabled"
                .to_owned(),
        );
    }
    if let Some(window) = options.default_context_window
        && window == 0
    {
        return Err("llm-deepseek: defaultContextWindow must be a positive integer".to_owned());
    }
    if let Some(cap) = options.max_tokens
        && !positive_safe_int(cap)
    {
        return Err("llm-deepseek: maxTokens must be a positive safe integer".to_owned());
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "the five-minute default is exactly representable in f64"
    )]
    let stream_idle_timeout_ms = options
        .stream_idle_timeout_ms
        .unwrap_or(DEFAULT_STREAM_IDLE_TIMEOUT_MS as f64);
    if !positive_finite_timer(stream_idle_timeout_ms) {
        return Err(format!(
            "llm-deepseek: streamIdleTimeoutMs must be a positive finite number no greater than {MAX_TIMER_DELAY_MS}"
        ));
    }

    let bounds = resolve_image_bounds(options)?;
    #[expect(
        clippy::cast_precision_loss,
        reason = "the one-minute default is exactly representable in f64"
    )]
    let files_api_timeout_ms = options
        .files_api_timeout_ms
        .unwrap_or(DEFAULT_FILES_API_TIMEOUT_MS as f64);
    if !positive_finite_timer(files_api_timeout_ms) {
        return Err(format!(
            "llm-deepseek: filesApiTimeoutMs must be a positive finite number no greater than {MAX_TIMER_DELAY_MS}"
        ));
    }
    let file_policy = resolve_file_policy(options)?;
    let base_url = options
        .base_url
        .clone()
        .or_else(|| base_url_env.map(str::to_owned))
        .unwrap_or_else(|| PUBLIC_BASE_URL.to_owned());
    validate_base_url(&base_url)?;
    let retry_policy = resolve_retry_policy(
        options.retry_policy.clone().unwrap_or_default(),
        "llm-deepseek: retryPolicy",
    )?;
    Ok(ConnectionOptions {
        base_url,
        defaults: RequestDefaults {
            thinking: options.thinking,
            reasoning_effort: options.reasoning_effort,
        },
        max_tokens: options.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        default_context_window: options
            .default_context_window
            .unwrap_or(DEFAULT_CONTEXT_WINDOW),
        models: resolve_models(options.models.as_deref())?,
        stream_idle_timeout_ms,
        max_request_files_bytes: bounds.max_request_files_bytes,
        max_inline_request_image_bytes: bounds.max_inline_request_image_bytes,
        max_images_per_request: bounds.max_images_per_request,
        image_offload_byte_quantum: bounds.image_offload_byte_quantum,
        inline_image_offload_byte_quantum: bounds.inline_image_offload_byte_quantum,
        image_offload_count_quantum: bounds.image_offload_count_quantum,
        files_api_timeout_ms,
        file_policy,
        retry_policy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fr_llm::retry_policy::{Backoff, RetryPolicy};

    #[test]
    fn resolve_defaults_pin_dsh() {
        let resolved = resolve_adapter_options(&ConfigOptions::default(), None).expect("defaults");
        assert_eq!(resolved.base_url, PUBLIC_BASE_URL);
        assert_eq!(resolved.max_tokens, DEFAULT_MAX_TOKENS);
        assert_eq!(resolved.default_context_window, DEFAULT_CONTEXT_WINDOW);
        assert_eq!(resolved.stream_idle_timeout_ms, 300_000.0);
        assert_eq!(resolved.max_request_files_bytes, 128 * 1024 * 1024);
        assert_eq!(resolved.max_inline_request_image_bytes, 20 * 1024 * 1024);
        assert_eq!(resolved.max_images_per_request, 600);
        assert_eq!(resolved.image_offload_byte_quantum, 64 * 1024 * 1024);
        assert_eq!(resolved.inline_image_offload_byte_quantum, 10 * 1024 * 1024);
        assert_eq!(resolved.image_offload_count_quantum, 20);
        assert_eq!(resolved.files_api_timeout_ms, 60_000.0);
        assert_eq!(
            resolved.file_policy,
            FilePolicy {
                expires_after_seconds: 7 * 24 * 60 * 60,
                refresh_margin_seconds: 60 * 60,
                quota_cleanup_batch: 100,
            }
        );
        // The built-in catalog resolves with its modalities filled and the
        // image entry's byte default applied.
        let models = &resolved.models;
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "deepseek-flash");
        assert_eq!(
            models[0].input_modalities.as_deref(),
            Some(&[ModelModality::Text, ModelModality::Image][..])
        );
        assert_eq!(
            models[0].image_max_bytes,
            Some(DEFAULT_REQUEST_IMAGE_MAX_BYTES)
        );
        assert_eq!(models[1].id, "deepseek-v4-pro");
        assert_eq!(
            models[1].input_modalities.as_deref(),
            Some(&[ModelModality::Text][..])
        );
        assert_eq!(models[1].image_max_bytes, None);
        // Omission uses normal mode with the shared defaults.
        assert_eq!(
            resolved.retry_policy,
            RetryPolicy::Normal {
                max_retries: 5,
                retryable_codes: [
                    "EMPTY_RESPONSE",
                    "RATE_LIMIT",
                    "SERVER",
                    "TIMEOUT",
                    "TRANSPORT",
                ]
                .iter()
                .map(|code| (*code).to_owned())
                .collect(),
                backoff: Backoff {
                    initial_delay_ms: 500,
                    max_delay_ms: 10_000,
                    jitter_ratio: 0.1,
                },
            }
        );
    }

    #[test]
    fn resolve_base_url_layers() {
        // The trusted env layer beats the public default.
        let resolved = resolve_adapter_options(
            &ConfigOptions::default(),
            Some("https://gateway.internal/anthropic"),
        )
        .expect("env layer");
        assert_eq!(resolved.base_url, "https://gateway.internal/anthropic");
        // Explicit config beats the env layer.
        let options = ConfigOptions {
            base_url: Some("https://explicit.example/api".to_owned()),
            ..ConfigOptions::default()
        };
        let resolved =
            resolve_adapter_options(&options, Some("https://gateway.internal")).expect("explicit");
        assert_eq!(resolved.base_url, "https://explicit.example/api");
    }

    #[test]
    fn resolve_rejects_out_of_bounds() {
        let reject = |options: &ConfigOptions| {
            resolve_adapter_options(options, None).expect_err("bounds reject")
        };
        let options = ConfigOptions {
            thinking: Some(ThinkingMode::Disabled),
            reasoning_effort: Some(EffortLevel::Low),
            ..ConfigOptions::default()
        };
        assert_eq!(
            reject(&options),
            "llm-deepseek: only reasoningEffort \"off\" can be configured when thinking is disabled"
        );

        let options = ConfigOptions {
            max_tokens: Some(0),
            ..ConfigOptions::default()
        };
        assert_eq!(
            reject(&options),
            "llm-deepseek: maxTokens must be a positive safe integer"
        );

        let options = ConfigOptions {
            image_offload_byte_quantum: Some(129 * 1024 * 1024),
            ..ConfigOptions::default()
        };
        assert_eq!(
            reject(&options),
            "llm-deepseek: imageOffloadByteQuantum must not exceed maxRequestFilesBytes"
        );

        let options = ConfigOptions {
            file_expires_after_seconds: Some(3_599),
            ..ConfigOptions::default()
        };
        assert_eq!(
            reject(&options),
            "llm-deepseek: fileExpiresAfterSeconds must be an integer from 3600 through 2592000"
        );

        let options = ConfigOptions {
            file_refresh_margin_seconds: Some(7 * 24 * 60 * 60),
            ..ConfigOptions::default()
        };
        assert_eq!(
            reject(&options),
            "llm-deepseek: fileRefreshMarginSeconds must be a non-negative integer below fileExpiresAfterSeconds"
        );

        let options = ConfigOptions {
            file_quota_cleanup_batch: Some(1_001),
            ..ConfigOptions::default()
        };
        assert_eq!(
            reject(&options),
            "llm-deepseek: fileQuotaCleanupBatch must be an integer from 1 through 1000"
        );

        let options = ConfigOptions {
            stream_idle_timeout_ms: Some(0.0),
            ..ConfigOptions::default()
        };
        assert_eq!(
            reject(&options),
            format!(
                "llm-deepseek: streamIdleTimeoutMs must be a positive finite number no greater than {}",
                2_147_483_647.0
            )
        );
    }

    #[test]
    fn resolve_rejects_bad_base_urls() {
        for base_url in [
            "ftp://api.example.com",
            "https://user:pass@api.example.com",
            "https://api.example.com/?x=1",
            "https://api.example.com/#frag",
            "not-a-url",
            "https://",
        ] {
            let options = ConfigOptions {
                base_url: Some(base_url.to_owned()),
                ..ConfigOptions::default()
            };
            assert_eq!(
                resolve_adapter_options(&options, None).expect_err(base_url),
                "llm-deepseek: Messages baseURL must be an HTTP(S) root without credentials, query, or fragment",
                "{base_url}"
            );
        }
    }

    #[test]
    fn resolve_models_arms() {
        let entry = |id: &str| CatalogModel {
            id: id.to_owned(),
            name: None,
            description: None,
            context_window: None,
            max_tokens: None,
            input_modalities: None,
            image_pixel_budget: None,
            image_max_bytes: None,
            system_prompt_update: None,
            tool_update: None,
        };
        // Duplicate ids reject.
        let options = ConfigOptions {
            models: Some(vec![entry("m"), entry("m")]),
            ..ConfigOptions::default()
        };
        assert_eq!(
            resolve_adapter_options(&options, None).expect_err("dup"),
            "llm-deepseek: duplicate catalog model \"m\""
        );
        // An empty id rejects.
        let options = ConfigOptions {
            models: Some(vec![entry("")]),
            ..ConfigOptions::default()
        };
        assert_eq!(
            resolve_adapter_options(&options, None).expect_err("empty id"),
            "llm-deepseek: catalog model ids must be non-empty"
        );
        // A text-only entry cannot declare image request limits.
        let mut text_only = entry("m");
        text_only.image_max_bytes = Some(1_024);
        let options = ConfigOptions {
            models: Some(vec![text_only]),
            ..ConfigOptions::default()
        };
        assert_eq!(
            resolve_adapter_options(&options, None).expect_err("text-only limits"),
            "llm-deepseek: text-only catalog model \"m\" cannot declare image request limits"
        );
        // An empty modality list rejects.
        let mut no_input = entry("m");
        no_input.input_modalities = Some(Vec::new());
        let options = ConfigOptions {
            models: Some(vec![no_input]),
            ..ConfigOptions::default()
        };
        assert_eq!(
            resolve_adapter_options(&options, None).expect_err("empty modalities"),
            "llm-deepseek: catalog model \"m\" inputModalities must not be empty"
        );
    }
}
