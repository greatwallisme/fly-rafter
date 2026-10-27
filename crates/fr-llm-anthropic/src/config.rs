//! Provider-route profile resolution (dsh `llm-pi-ai` `config.ts`
//! `resolveProfiles`, strict face): the `providers` dict entry parse, the
//! scalar defaults, and the validation diagnostics.
//!
//! Programmatic construction may bypass schema normalization, so every
//! default and bound is re-judged here (deepseek-crate precedent). dsh
//! schema-level member errors (schemastery texts) are owned by the fr
//! config layer; this surface keeps the dsh `resolveProfiles` semantic
//! texts verbatim in their English wording, with the owning-plugin prefix
//! rendered as `fr-llm-anthropic` and config keys rendered kebab-case.
//! Unknown profile keys are ignored, mirroring the dsh schemastery strip;
//! the three keys dsh itself refuses (`provider`, `max-retries`,
//! `max-retry-delay-ms`) keep their refusal texts.

use crate::compat::{Api, CompatProfile, parse_compat_profile};
use crate::models::{
    Modality, ModelProfile, ReasoningEfforts, RouteCatalog, RouteCatalogRequest, ThinkingLevel,
    parse_route_api, resolve_route_models,
};
use fr_llm::retry_policy::{RetryPolicy, RetryPolicyInput, resolve_retry_policy};
use fr_wire::ids::SAFE_INTEGER_MAX;
use std::collections::BTreeMap;

/// Default maximum idle interval while an adapter stream read is
/// outstanding (dsh `DEFAULT_STREAM_IDLE_TIMEOUT_MS`, config.ts:47).
pub const DEFAULT_STREAM_IDLE_TIMEOUT_MS: u64 = 300_000;

/// Default request-level bound on base64-encoded image payload (dsh
/// `DEFAULT_MAX_REQUEST_IMAGE_BYTES`, config.ts:58).
pub const DEFAULT_MAX_REQUEST_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// Default total-pixel budget (dsh `DEFAULT_REQUEST_IMAGE_PIXEL_BUDGET`,
/// config.ts:60).
pub const DEFAULT_REQUEST_IMAGE_PIXEL_BUDGET: u64 = 2048 * 2048;

/// Default raw encoded-byte target (dsh `DEFAULT_REQUEST_IMAGE_MAX_BYTES`,
/// config.ts:62).
pub const DEFAULT_REQUEST_IMAGE_MAX_BYTES: u64 = 1024 * 1024;

/// Context capacity assumed for a model neither the entry nor the route
/// sizes (dsh `DEFAULT_CONTEXT_WINDOW`, config.ts:65).
pub const DEFAULT_CONTEXT_WINDOW: u64 = 262_144;

/// Output capability assumed for a model neither the entry nor the route
/// sizes (dsh `DEFAULT_MAX_TOKENS`, config.ts:68).
pub const DEFAULT_MAX_TOKENS: u64 = 32_768;

/// Positive finite timer ceiling (dsh `MAX_TIMER_DELAY_MS` from
/// dsh-timeout; fr-timeout carries the same constant).
const MAX_TIMER_DELAY_MS: f64 = 2_147_483_647.0;
/// The ceiling in integer form, for diagnostics.
const MAX_TIMER_DELAY_MS_TEXT: u64 = 2_147_483_647;

/// The prompt-cache retention preference (pi-ai `CacheRetention`,
/// config.ts:340).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheRetention {
    /// Send no cache key or retention field.
    None,
    /// The default retention.
    Short,
    /// Long retention where the endpoint takes it.
    Long,
}

impl CacheRetention {
    /// The config literal (`cache-retention:` value).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Short => "short",
            Self::Long => "long",
        }
    }
}

/// Token budgets used by reasoning providers that take them (pi-ai
/// `ThinkingBudgets`, dsh `thinkingBudgets` schema, config.ts:234-239).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThinkingBudgets {
    /// The minimal-level budget.
    pub minimal: u64,
    /// The low-level budget.
    pub low: u64,
    /// The medium-level budget.
    pub medium: u64,
    /// The high-level budget.
    pub high: u64,
}

/// One resolved provider route: the validated profile with every
/// adapter-owned default filled (dsh `ResolvedPiAiProviderProfile`
/// restricted to the fr face) plus the materialized model catalog.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedRouteProfile {
    /// Provider route key (the dict key).
    pub provider: String,
    /// Resolved display name (defaults to the route key).
    pub display_name: String,
    /// The credential reference (env-var name), when configured.
    pub api_key_env: Option<String>,
    /// The route's wire protocol.
    pub api: Api,
    /// The route endpoint.
    pub base_url: String,
    /// The materialized models plus configured per-request output caps.
    pub catalog: RouteCatalog,
    /// Provider request headers in declaration order.
    pub headers: Vec<(String, String)>,
    /// Provider-neutral reasoning level.
    pub reasoning: Option<ThinkingLevel>,
    /// Token budgets for reasoning providers that take them.
    pub thinking_budgets: Option<ThinkingBudgets>,
    /// Prompt-cache retention preference.
    pub cache_retention: Option<CacheRetention>,
    /// HTTP request timeout in milliseconds.
    pub timeout_ms: Option<u64>,
    /// Maximum provider idle time while one stream read is outstanding
    /// (f64: a fractional millisecond is a legal JS Number and u64 would
    /// break the `> 0` invariant; deepseek-crate slice-3 precedent).
    pub stream_idle_timeout_ms: f64,
    /// Request-level base64 image payload bound.
    pub max_request_image_bytes: u64,
    /// Total-pixel budget for each request image version.
    pub request_image_pixel_budget: u64,
    /// Raw encoded-byte target for each request image version.
    pub request_image_max_bytes: u64,
    /// The frozen retry policy (fr-llm shared vocabulary).
    pub retry_policy: RetryPolicy,
}

/// Formats one route diagnostic with the fr artifact prefix.
fn invalid(provider: &str, detail: &str) -> String {
    format!("fr-llm-anthropic: provider \"{provider}\" {detail}")
}

/// Parses one thinking level literal (`reasoning:` value and
/// `reasoning-efforts` keys).
fn parse_thinking_level(value: &str) -> Option<ThinkingLevel> {
    ThinkingLevel::ALL
        .iter()
        .copied()
        .find(|level| level.as_str() == value)
}

/// Parses one modality literal (`default-input`/`input` entries).
fn parse_modality(value: &str) -> Option<Modality> {
    match value {
        "text" => Some(Modality::Text),
        "image" => Some(Modality::Image),
        _ => None,
    }
}

/// Parses one model entry (dsh `PiAiModelProfile` members; the id is
/// required).
fn parse_model_profile(provider: &str, entry: &serde_json::Value) -> Result<ModelProfile, String> {
    let map = entry
        .as_object()
        .ok_or_else(|| "model entry must be an object".to_owned())?;
    let id = map
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "model entry must carry an \"id\" string".to_owned())?
        .to_owned();
    let mut profile = ModelProfile {
        id,
        ..ModelProfile::default()
    };
    for (key, member) in map {
        match key.as_str() {
            "name" => {
                let name = member
                    .as_str()
                    .ok_or_else(|| "model \"name\" must be a string".to_owned())?;
                profile.name = Some(name.to_owned());
            }
            "context-window" => {
                profile.context_window = Some(member.as_u64().ok_or_else(|| {
                    "model \"context-window\" must be a positive integer".to_owned()
                })?);
            }
            "max-tokens" => {
                profile.max_tokens =
                    Some(member.as_u64().ok_or_else(|| {
                        "model \"max-tokens\" must be a positive integer".to_owned()
                    })?);
            }
            "input" => {
                let list = member
                    .as_array()
                    .ok_or_else(|| "model \"input\" must be an array of modalities".to_owned())?;
                let mut modalities = Vec::with_capacity(list.len());
                for item in list {
                    let literal = item.as_str().ok_or_else(|| {
                        "model \"input\" entries must be \"text\" or \"image\"".to_owned()
                    })?;
                    let modality = parse_modality(literal).ok_or_else(|| {
                        format!("model \"input\" names unknown modality \"{literal}\"")
                    })?;
                    modalities.push(modality);
                }
                profile.input = Some(modalities);
            }
            "reasoning-efforts" => {
                profile.reasoning_efforts = Some(parse_reasoning_efforts(member)?);
            }
            "compat" => {
                let site = format!("model \"{}\"", profile.id);
                profile.compat = Some(parse_compat_profile(provider, &site, member)?);
            }
            _ => {}
        }
    }
    Ok(profile)
}

/// Parses the reasoning-efforts declaration (dsh `reasoningEfforts`
/// union: `false` disables, a dict declares offered levels; only `off`
/// may carry a null wire value).
fn parse_reasoning_efforts(value: &serde_json::Value) -> Result<ReasoningEfforts, String> {
    if value.as_bool() == Some(false) {
        return Ok(ReasoningEfforts::Disable);
    }
    let serde_json::Value::Object(map) = value else {
        return Err("reasoning-efforts must be false or a level dict".to_owned());
    };
    let mut efforts = BTreeMap::new();
    for (key, member) in map {
        let level = parse_thinking_level(key)
            .ok_or_else(|| format!("reasoning-efforts names unknown level \"{key}\""))?;
        let wire = match member {
            serde_json::Value::Null => None,
            serde_json::Value::String(wire) => Some(wire.clone()),
            _ => return Err("reasoning-efforts wire values must be strings or null".to_owned()),
        };
        efforts.insert(level, wire);
    }
    Ok(ReasoningEfforts::Map(efforts))
}

/// Parses the retry-policy input (dsh `RetryPolicySchema`: `mode`
/// normal|always, `max-retries`, `retryable-codes`, and the backoff
/// knobs; fr-llm carries the shared vocabulary).
fn parse_retry_policy_input(value: &serde_json::Value) -> Result<RetryPolicyInput, String> {
    let map = value
        .as_object()
        .ok_or_else(|| "retry-policy must be an object".to_owned())?;
    let mut input = RetryPolicyInput::default();
    for (key, member) in map {
        match key.as_str() {
            "mode" => {
                let mode = member.as_str().ok_or_else(|| {
                    "retry-policy \"mode\" must be \"normal\" or \"always\"".to_owned()
                })?;
                match mode {
                    "normal" => input.always = false,
                    "always" => input.always = true,
                    other => {
                        return Err(format!(
                            "retry-policy \"mode\" must be \"normal\" or \"always\"; got \"{other}\""
                        ));
                    }
                }
            }
            "max-retries" => {
                input.max_retries = Some(member.as_u64().ok_or_else(|| {
                    "retry-policy \"max-retries\" must be a non-negative integer".to_owned()
                })?);
            }
            "retryable-codes" => {
                let list = member.as_array().ok_or_else(|| {
                    "retry-policy \"retryable-codes\" must be an array of strings".to_owned()
                })?;
                let mut codes = Vec::with_capacity(list.len());
                for code in list {
                    codes.push(
                        code.as_str()
                            .ok_or_else(|| {
                                "retry-policy \"retryable-codes\" entries must be strings"
                                    .to_owned()
                            })?
                            .to_owned(),
                    );
                }
                input.retryable_codes = Some(codes);
            }
            "backoff" => {
                let backoff = member
                    .as_object()
                    .ok_or_else(|| "retry-policy \"backoff\" must be an object".to_owned())?;
                if let Some(delay) = backoff.get("initial-delay-ms") {
                    input
                        .backoff
                        .get_or_insert_with(Default::default)
                        .initial_delay_ms = Some(delay.as_u64().ok_or_else(|| {
                        "retry-policy backoff \"initial-delay-ms\" must be a non-negative \
                             integer"
                            .to_owned()
                    })?);
                }
                if let Some(delay) = backoff.get("max-delay-ms") {
                    input
                        .backoff
                        .get_or_insert_with(Default::default)
                        .max_delay_ms = Some(delay.as_u64().ok_or_else(|| {
                        "retry-policy backoff \"max-delay-ms\" must be a non-negative integer"
                            .to_owned()
                    })?);
                }
                if let Some(jitter) = backoff.get("jitter-ratio") {
                    input
                        .backoff
                        .get_or_insert_with(Default::default)
                        .jitter_ratio = Some(jitter.as_f64().ok_or_else(|| {
                        "retry-policy backoff \"jitter-ratio\" must be a number".to_owned()
                    })?);
                }
            }
            _ => {}
        }
    }
    Ok(input)
}

/// Parses the `headers` member into an ordered list (dsh `headers` dict
/// with `assertValidHeaders`, config.ts:389-400).
fn parse_headers(
    provider: &str,
    value: &serde_json::Value,
) -> Result<Vec<(String, String)>, String> {
    let header_map = value
        .as_object()
        .ok_or_else(|| invalid(provider, "profile key \"headers\" must be an object"))?;
    let mut headers = Vec::with_capacity(header_map.len());
    for (name, header_value) in header_map {
        let header_value = header_value
            .as_str()
            .ok_or_else(|| invalid(provider, "profile key \"headers\" values must be strings"))?;
        assert_valid_header(provider, name, header_value)?;
        headers.push((name.clone(), header_value.to_owned()));
    }
    Ok(headers)
}

/// Parses the `default-input` member (dsh `defaultInput` schema with the
/// non-empty judgment at resolve time, config.ts:449-457).
fn parse_modality_list(provider: &str, value: &serde_json::Value) -> Result<Vec<Modality>, String> {
    let list = value
        .as_array()
        .ok_or_else(|| invalid(provider, "profile key \"default-input\" must be an array"))?;
    let mut modalities = Vec::with_capacity(list.len());
    for item in list {
        let literal = item.as_str().ok_or_else(|| {
            invalid(
                provider,
                "profile key \"default-input\" entries must be \"text\" or \"image\"",
            )
        })?;
        let modality = parse_modality(literal).ok_or_else(|| {
            invalid(
                provider,
                &format!("profile key \"default-input\" names unknown modality \"{literal}\""),
            )
        })?;
        modalities.push(modality);
    }
    Ok(modalities)
}

/// Parses the `thinking-budgets` member (dsh `thinkingBudgets` schema,
/// config.ts:234-239).
fn parse_thinking_budgets(
    provider: &str,
    value: &serde_json::Value,
) -> Result<ThinkingBudgets, String> {
    let budgets = value.as_object().ok_or_else(|| {
        invalid(
            provider,
            "profile key \"thinking-budgets\" must be an object",
        )
    })?;
    let mut parsed = ThinkingBudgets {
        minimal: 0,
        low: 0,
        medium: 0,
        high: 0,
    };
    for (field, entry) in budgets {
        let number = entry.as_u64().ok_or_else(|| {
            invalid(
                provider,
                "profile key \"thinking-budgets\" values must be non-negative integers",
            )
        })?;
        match field.as_str() {
            "minimal" => parsed.minimal = number,
            "low" => parsed.low = number,
            "medium" => parsed.medium = number,
            "high" => parsed.high = number,
            _ => {}
        }
    }
    Ok(parsed)
}

/// Parses the `cache-retention` member (dsh `cacheRetention` union,
/// config.ts:340).
fn parse_cache_retention(
    provider: &str,
    value: &serde_json::Value,
) -> Result<CacheRetention, String> {
    let literal = value
        .as_str()
        .ok_or_else(|| invalid(provider, "profile key \"cache-retention\" must be a string"))?;
    Ok(match literal {
        "none" => CacheRetention::None,
        "short" => CacheRetention::Short,
        "long" => CacheRetention::Long,
        other => {
            return Err(invalid(
                provider,
                &format!(
                    "profile key \"cache-retention\" must be \"none\", \"short\", or \"long\"; got \"{other}\""
                ),
            ));
        }
    })
}

/// Refuses the three profile keys dsh itself rejects before any member is
/// read (dsh `rejectRemovedFields`, config.ts:371-386, plus the
/// catalog-less `model-overrides` refusal, catalog.ts:872-893).
fn reject_removed_keys(
    provider: &str,
    map: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    for (key, member) in map {
        match key.as_str() {
            "provider" => {
                return Err(invalid(
                    provider,
                    "sets \"provider\", which moved to the providers dict key",
                ));
            }
            "max-retries" | "max-retry-delay-ms" => {
                return Err(invalid(
                    provider,
                    "sets max-retries or max-retry-delay-ms, which were removed; compose agent \
                     recovery with the provider retry-policy executor",
                ));
            }
            "model-overrides" => {
                // A catalog-less route spells every model out in its list.
                let first = member
                    .as_object()
                    .and_then(|overrides| overrides.keys().next())
                    .cloned()
                    .unwrap_or_default();
                return Err(invalid(
                    provider,
                    &format!(
                        "sets model-overrides for \"{first}\", but the installed catalog does \
                         not describe this route; a declared route spells every model out in \
                         its models list"
                    ),
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Resolves one `providers` dict entry from raw configuration into a
/// validated route profile (dsh `resolveProfiles` strict face).
///
/// # Errors
///
/// Carries the dsh diagnostic for the first failing fact (fr prefix,
/// kebab keys): removed keys, empty names, malformed scalars, unserviceable
/// models, or a retry policy that fails its own validation.
#[expect(
    clippy::too_many_lines,
    reason = "one match arm per dsh profile key (config.ts:326-349); splitting the dispatch \
              would scatter the key-to-field mapping the dsh shape keeps together"
)]
pub fn resolve_route_profile(
    provider: &str,
    raw: &serde_json::Value,
) -> Result<ResolvedRouteProfile, String> {
    if provider.is_empty() {
        return Err("fr-llm-anthropic: provider names must be non-empty".to_owned());
    }
    let map = raw
        .as_object()
        .ok_or_else(|| invalid(provider, "profile must be an object"))?;
    reject_removed_keys(provider, map)?;
    let mut display_name = provider.to_owned();
    let mut api_key_env: Option<String> = None;
    let mut api: Option<Api> = None;
    let mut base_url: Option<String> = None;
    let mut models: Vec<ModelProfile> = Vec::new();
    let mut compat: Option<CompatProfile> = None;
    let mut default_context_window = DEFAULT_CONTEXT_WINDOW;
    let mut default_max_tokens = DEFAULT_MAX_TOKENS;
    let mut default_input: Option<Vec<Modality>> = None;
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut reasoning: Option<ThinkingLevel> = None;
    let mut thinking_budgets: Option<ThinkingBudgets> = None;
    let mut cache_retention: Option<CacheRetention> = None;
    let mut timeout_ms: Option<u64> = None;
    let mut stream_idle_timeout_ms: Option<f64> = None;
    let mut max_request_image_bytes: Option<u64> = None;
    let mut request_image_pixel_budget: Option<u64> = None;
    let mut request_image_max_bytes: Option<u64> = None;
    let mut retry_policy_raw: Option<serde_json::Value> = None;
    for (key, member) in map {
        match key.as_str() {
            "api-key-env" => {
                api_key_env = Some(
                    member
                        .as_str()
                        .ok_or_else(|| {
                            invalid(provider, "profile key \"api-key-env\" must be a string")
                        })?
                        .to_owned(),
                );
            }
            "display-name" => {
                let name = member.as_str().ok_or_else(|| {
                    invalid(provider, "profile key \"display-name\" must be a string")
                })?;
                name.clone_into(&mut display_name);
            }
            "api" => {
                let literal = member
                    .as_str()
                    .ok_or_else(|| invalid(provider, "profile key \"api\" must be a string"))?;
                api = Some(parse_route_api(provider, literal)?);
            }
            "base-url" => {
                base_url = Some(
                    member
                        .as_str()
                        .ok_or_else(|| {
                            invalid(provider, "profile key \"base-url\" must be a string")
                        })?
                        .to_owned(),
                );
            }
            "models" => {
                let list = member
                    .as_array()
                    .ok_or_else(|| invalid(provider, "profile key \"models\" must be an array"))?;
                for entry in list {
                    models.push(parse_model_profile(provider, entry)?);
                }
            }
            "compat" => compat = Some(parse_compat_profile(provider, "route", member)?),
            "default-context-window" => {
                default_context_window = member.as_u64().ok_or_else(|| {
                    invalid(
                        provider,
                        "profile key \"default-context-window\" must be a positive integer",
                    )
                })?;
            }
            "default-max-tokens" => {
                default_max_tokens = member.as_u64().ok_or_else(|| {
                    invalid(
                        provider,
                        "profile key \"default-max-tokens\" must be a positive integer",
                    )
                })?;
            }
            "default-input" => default_input = Some(parse_modality_list(provider, member)?),
            "headers" => headers.extend(parse_headers(provider, member)?),
            "reasoning" => {
                let literal = member.as_str().ok_or_else(|| {
                    invalid(provider, "profile key \"reasoning\" must be a string")
                })?;
                reasoning = Some(parse_thinking_level(literal).ok_or_else(|| {
                    invalid(provider, "profile key \"reasoning\" names unknown level")
                })?);
            }
            "thinking-budgets" => {
                thinking_budgets = Some(parse_thinking_budgets(provider, member)?);
            }
            "cache-retention" => cache_retention = Some(parse_cache_retention(provider, member)?),
            "timeout-ms" => {
                timeout_ms = Some(member.as_u64().ok_or_else(|| {
                    invalid(
                        provider,
                        "profile key \"timeout-ms\" must be a non-negative integer",
                    )
                })?);
            }
            "stream-idle-timeout-ms" => {
                stream_idle_timeout_ms = Some(member.as_f64().ok_or_else(|| {
                    invalid(
                        provider,
                        "profile key \"stream-idle-timeout-ms\" must be a number",
                    )
                })?);
            }
            "max-request-image-bytes" => {
                max_request_image_bytes = Some(member.as_u64().ok_or_else(|| {
                    invalid(
                        provider,
                        "profile key \"max-request-image-bytes\" must be a positive integer",
                    )
                })?);
            }
            "request-image-pixel-budget" => {
                request_image_pixel_budget = Some(member.as_u64().ok_or_else(|| {
                    invalid(
                        provider,
                        "profile key \"request-image-pixel-budget\" must be a positive integer",
                    )
                })?);
            }
            "request-image-max-bytes" => {
                request_image_max_bytes = Some(member.as_u64().ok_or_else(|| {
                    invalid(
                        provider,
                        "profile key \"request-image-max-bytes\" must be a positive integer",
                    )
                })?);
            }
            "retry-policy" => {
                retry_policy_raw = Some(member.clone());
            }
            _ => {}
        }
    }
    // Scalar validation chain (dsh resolveProfiles body, config.ts:421-457),
    // in the dsh judgment order.
    if base_url.as_deref().is_some_and(str::is_empty) {
        return Err(invalid(provider, "has an empty base-url"));
    }
    if display_name.is_empty() {
        return Err(invalid(provider, "has an empty display-name"));
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "the default is 300_000, exact in f64; the profile value was parsed as f64"
    )]
    let stream_idle_timeout_ms =
        stream_idle_timeout_ms.unwrap_or(DEFAULT_STREAM_IDLE_TIMEOUT_MS as f64);
    if !(stream_idle_timeout_ms.is_finite()
        && stream_idle_timeout_ms > 0.0
        && stream_idle_timeout_ms <= MAX_TIMER_DELAY_MS)
    {
        return Err(invalid(
            provider,
            &format!(
                "stream-idle-timeout-ms must be a positive finite number no greater than {MAX_TIMER_DELAY_MS_TEXT}"
            ),
        ));
    }
    let max_request_image_bytes =
        max_request_image_bytes.unwrap_or(DEFAULT_MAX_REQUEST_IMAGE_BYTES);
    if max_request_image_bytes == 0 {
        return Err(invalid(
            provider,
            "max-request-image-bytes must be a positive integer",
        ));
    }
    let request_image_pixel_budget =
        request_image_pixel_budget.unwrap_or(DEFAULT_REQUEST_IMAGE_PIXEL_BUDGET);
    if request_image_pixel_budget == 0 || request_image_pixel_budget > SAFE_INTEGER_MAX {
        return Err(invalid(
            provider,
            "request-image-pixel-budget must be a positive safe integer",
        ));
    }
    let request_image_max_bytes =
        request_image_max_bytes.unwrap_or(DEFAULT_REQUEST_IMAGE_MAX_BYTES);
    if request_image_max_bytes == 0 || request_image_max_bytes > SAFE_INTEGER_MAX {
        return Err(invalid(
            provider,
            "request-image-max-bytes must be a positive safe integer",
        ));
    }
    let default_input = default_input.unwrap_or_else(|| vec![Modality::Text]);
    if default_input.is_empty() {
        return Err(invalid(
            provider,
            "default-input must name at least one modality",
        ));
    }
    let retry_policy = match retry_policy_raw {
        Some(raw) => resolve_retry_policy(
            parse_retry_policy_input(&raw)?,
            &format!("fr-llm-anthropic: provider \"{provider}\" retry-policy"),
        )?,
        None => resolve_retry_policy(
            RetryPolicyInput::default(),
            &format!("fr-llm-anthropic: provider \"{provider}\" retry-policy"),
        )?,
    };
    let catalog = resolve_route_models(&RouteCatalogRequest {
        provider: provider.to_owned(),
        api,
        base_url,
        models,
        compat,
        default_context_window,
        default_max_tokens,
        default_input,
    })?;
    let api = catalog
        .models
        .first()
        .map_or(Api::AnthropicMessages, |model| model.api);
    Ok(ResolvedRouteProfile {
        provider: provider.to_owned(),
        display_name,
        api_key_env,
        api,
        base_url: catalog
            .models
            .first()
            .map_or(String::new(), |m| m.base_url.clone()),
        catalog,
        headers,
        reasoning,
        thinking_budgets,
        cache_retention,
        timeout_ms,
        stream_idle_timeout_ms,
        max_request_image_bytes,
        request_image_pixel_budget,
        request_image_max_bytes,
        retry_policy,
    })
}

/// Validates one profile header against the Fetch surface (dsh
/// `assertValidHeaders`, config.ts:389-400): a valid HTTP field name and a
/// single-line value.
fn assert_valid_header(provider: &str, name: &str, value: &str) -> Result<(), String> {
    let valid_name = !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        });
    let valid_value = !value.bytes().any(|byte| byte == b'\r' || byte == b'\n');
    if !valid_name || !valid_value {
        return Err(invalid(
            provider,
            &format!(
                "header \"{name}\" is not valid for Fetch; use a valid HTTP field name and a \
                 single-line value representable as bytes"
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn resolve(
        provider: &str,
        profile: &serde_json::Value,
    ) -> Result<ResolvedRouteProfile, String> {
        resolve_route_profile(provider, profile)
    }

    fn minimal() -> serde_json::Value {
        json!({
            "api": "anthropic-messages",
            "base-url": "https://gw.example/v1",
            "models": [{ "id": "acme-large" }],
        })
    }

    #[test]
    fn resolves_a_route_with_defaults() {
        let route = resolve("acme", &minimal()).expect("resolves");
        assert_eq!(route.display_name, "acme");
        assert_eq!(route.api, Api::AnthropicMessages);
        assert_eq!(route.base_url, "https://gw.example/v1");
        assert_eq!(route.stream_idle_timeout_ms, 300_000.0);
        assert_eq!(route.max_request_image_bytes, 20 * 1024 * 1024);
        assert_eq!(route.catalog.models.len(), 1);
        assert_eq!(route.catalog.models[0].context_window, 262_144);
        assert_eq!(route.catalog.models[0].max_tokens, 32_768);
        assert_eq!(route.catalog.models[0].input, vec![Modality::Text]);
        match route.retry_policy {
            RetryPolicy::Normal { max_retries, .. } => assert_eq!(max_retries, 5),
            RetryPolicy::Always { .. } => panic!("normal by default"),
        }
    }

    #[test]
    fn removed_keys_keep_their_refusal_texts() {
        let err = resolve("acme", &json!({"provider": "acme"})).expect_err("moved");
        assert!(
            err.contains("sets \"provider\", which moved to the providers dict key"),
            "got: {err}"
        );
        let err = resolve(
            "acme",
            &json!({"max-retries": 2, "api": "anthropic-messages"}),
        )
        .expect_err("removed");
        assert!(
            err.contains(
                "sets max-retries or max-retry-delay-ms, which were removed; compose agent \
                 recovery with the provider retry-policy executor"
            ),
            "got: {err}"
        );
    }

    #[test]
    fn stream_idle_timeout_bounds_are_enforced() {
        let mut profile = minimal();
        profile["stream-idle-timeout-ms"] = json!(0);
        let err = resolve("acme", &profile).expect_err("zero idle");
        assert!(err.contains("stream-idle-timeout-ms must be a positive finite number"));
        let mut profile = minimal();
        profile["stream-idle-timeout-ms"] = json!(4_000_000_000.0);
        let err = resolve("acme", &profile).expect_err("too large");
        assert!(err.contains("no greater than 2147483647"));
    }

    #[test]
    fn model_overrides_are_refused_on_the_catalog_less_surface() {
        let mut profile = minimal();
        profile["model-overrides"] = json!({ "acme-large": { "max-tokens": 1 } });
        let err = resolve("acme", &profile).expect_err("overrides");
        assert!(
            err.contains(
                "sets model-overrides for \"acme-large\", but the installed catalog does not \
                 describe this route"
            ),
            "got: {err}"
        );
    }

    #[test]
    fn compat_vocabulary_refusals_name_the_offered_set() {
        let mut profile = minimal();
        profile["compat"] = json!({ "supports-nonsense": true });
        let err = resolve("acme", &profile).expect_err("undeclared key");
        assert!(
            err.contains(
                "route sets compat \"supports-nonsense\", which no wire protocol declares; the \
                 configurable switches are supports-eager-tool-input-streaming, \
                 supports-long-cache-retention, supports-cache-control-on-tools, \
                 supports-temperature, force-adaptive-thinking, allow-empty-signature, \
                 supports-strict-tools"
            ),
            "got: {err}"
        );
        let mut profile = minimal();
        profile["compat"] = json!({ "supports-mid-convo-system-messages": true });
        let err = resolve("acme", &profile).expect_err("withheld key");
        assert!(
            err.contains(
                "route sets compat \"supports-mid-convo-system-messages\", which is not \
                 configurable here"
            ),
            "got: {err}"
        );
    }

    #[test]
    fn retry_policy_modes_resolve() {
        let mut profile = minimal();
        profile["retry-policy"] = json!({ "mode": "always" });
        let route = resolve("acme", &profile).expect("resolves");
        assert!(matches!(route.retry_policy, RetryPolicy::Always { .. }));
        let mut profile = minimal();
        profile["retry-policy"] =
            json!({ "mode": "normal", "max-retries": 2, "retryable-codes": ["TRANSPORT"] });
        let route = resolve("acme", &profile).expect("resolves");
        match route.retry_policy {
            RetryPolicy::Normal {
                max_retries,
                retryable_codes,
                ..
            } => {
                assert_eq!(max_retries, 2);
                assert_eq!(retryable_codes, vec!["TRANSPORT".to_owned()]);
            }
            RetryPolicy::Always { .. } => panic!("normal requested"),
        }
    }

    #[test]
    fn compat_switches_flow_into_the_catalog() {
        let mut profile = minimal();
        profile["compat"] = json!({ "supports-temperature": false, "supports-strict-tools": true });
        let route = resolve("acme", &profile).expect("resolves");
        let compat = &route.catalog.models[0].compat;
        assert!(!compat.supports_temperature);
        assert!(compat.supports_strict_tools);
    }
}
// Rust guideline compliant 2026-10-26
