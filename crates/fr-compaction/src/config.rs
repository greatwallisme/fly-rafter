//! Load-time validation and routed-model policy resolution for the basic
//! compaction backend (dsh compaction-basic/src/config.ts 1:1).
//!
//! fr config keys are kebab-case (the dsh camelCase keys converted per the
//! naming rule), and the user-facing validation messages name the same keys
//! in kebab-case. The `resolveCompactSpec` diagnostics keep the dsh wording
//! verbatim where the words name computed or adapter quantities, not config
//! keys (`contextWindow`, `reservedCompletionTokens`, `retainTokens`).
//!
//! The dsh numeric discipline is mirrored exactly: config integers accept
//! any JSON number with a zero fractional part within the JS safe-integer
//! range, and the pressure math multiplies in `f64` and floors, so both
//! sides derive identical budgets from identical inputs.

use std::collections::HashSet;

use fr_wire::ids::SAFE_INTEGER_MAX;
use serde_json::Value;

/// Default request-pressure fraction for every routed model.
const DEFAULT_THRESHOLD_RATIO: f64 = 0.8;

/// Default verbatim-tail fraction for every routed model.
const DEFAULT_RETAIN_RATIO: f64 = 0.16;

/// Default pressure headroom beyond the routed output reservation.
const DEFAULT_HEADROOM_TOKENS: i64 = 65_536;

/// Default extra attempts after the first compaction under pressure.
const DEFAULT_COMPACTION_RETRIES: i64 = 1;

/// Default retries after canonical context overflow (`0` disables).
const DEFAULT_MAX_OVERFLOW_RETRIES: i64 = 1;

/// Complete public top-level configuration key set (the dsh
/// `POLICY_CONFIG_KEYS` fields plus the table-level keys; the exact-target
/// key set reuses the same policy fields).
const BASIC_COMPACT_CONFIG_KEYS: [&str; 11] = [
    "threshold-ratio",
    "headroom-tokens",
    "retain-ratio",
    "retain-tokens",
    "summarization-provider",
    "summarization-model",
    "max-tokens",
    "compaction-retries",
    "max-overflow-retries",
    "model-policies",
    "auto",
];

/// Complete exact-target override key set.
const MODEL_POLICY_KEYS: [&str; 11] = [
    "provider",
    "model",
    "threshold-ratio",
    "headroom-tokens",
    "retain-ratio",
    "retain-tokens",
    "summarization-provider",
    "summarization-model",
    "max-tokens",
    "compaction-retries",
    "max-overflow-retries",
];

/// Target-specific pressure configuration failure eligible for warning
/// suppression (the engine deduplicates one warning per `target_key`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetPressureConfigError {
    /// The exact provider/model route used as the warning key.
    pub target_key: String,
    message: String,
}

impl TargetPressureConfigError {
    /// Create one classified pressure-config failure.
    pub fn new(target_key: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            target_key: target_key.into(),
            message: message.into(),
        }
    }

    /// The actionable configuration failure detail.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for TargetPressureConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TargetPressureConfigError {}

/// Exactly one validated retention form (dsh `ResolvedRetention`; the dsh
/// `retainRatio | retainTokens` discriminated union).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ResolvedRetention {
    /// Recent context retained as a fraction of the message budget.
    Ratio(f64),
    /// Absolute recent-context budget in tokens.
    Tokens(i64),
}

/// The exact routed provider/model pair a policy targets (dsh
/// `Pick<LlmCallConfig, 'provider' | 'model'>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteTarget {
    /// Registered provider route.
    pub provider: String,
    /// Exact routed model id.
    pub model: String,
}

/// One exact provider/model override, validated (dsh
/// `ModelCompactPolicyConfig`; the retention alternatives collapse into
/// [`ResolvedRetention`]).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelCompactPolicyConfig {
    /// Registered provider route to match.
    pub provider: String,
    /// Exact routed model id to match within `provider`.
    pub model: String,
    /// Window fraction override.
    pub threshold_ratio: Option<f64>,
    /// Pressure headroom override.
    pub headroom_tokens: Option<i64>,
    /// Retention override, in whichever form the config declared.
    pub retention: Option<ResolvedRetention>,
    /// Summary provider override; set together with the model.
    pub summarization_provider: Option<String>,
    /// Summary model override; set together with the provider.
    pub summarization_model: Option<String>,
    /// Summary generation cap override.
    pub max_tokens: Option<i64>,
    /// Pressure compaction retries override.
    pub compaction_retries: Option<i64>,
    /// Overflow recovery retries override.
    pub max_overflow_retries: Option<i64>,
}

/// Validated immutable config whose target-specific defaults remain
/// unresolved (dsh `ResolvedConfig`).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedConfig {
    /// Window fraction for pressure.
    pub threshold_ratio: f64,
    /// Pressure headroom beyond the routed output reservation.
    pub headroom_tokens: i64,
    /// The service-wide retention form.
    pub retention: ResolvedRetention,
    /// Default summary provider; empty inherits the conversation target.
    pub summarization_provider: String,
    /// Default summary model; empty inherits the conversation target.
    pub summarization_model: String,
    /// Summary generation cap (defaults to the resolved headroom).
    pub max_tokens: i64,
    /// Extra attempts after the first compaction under pressure.
    pub compaction_retries: i64,
    /// Maximum retries after canonical context overflow.
    pub max_overflow_retries: i64,
    /// Validated exact-target overrides, in declaration order.
    pub model_policies: Vec<ModelCompactPolicyConfig>,
    /// Whether automatic step-boundary listeners are enabled.
    pub auto: bool,
}

/// Fully merged policy for one routed conversation target, before capacity
/// scaling (dsh `ResolvedTargetPolicy`).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTargetPolicy {
    /// The matched route.
    pub target: RouteTarget,
    /// Merged window fraction for pressure.
    pub threshold_ratio: f64,
    /// Merged pressure headroom.
    pub headroom_tokens: i64,
    /// Merged retention form.
    pub retention: ResolvedRetention,
    /// Merged summary provider.
    pub summarization_provider: String,
    /// Merged summary model.
    pub summarization_model: String,
    /// Merged summary generation cap.
    pub max_tokens: i64,
    /// Merged pressure compaction retries.
    pub compaction_retries: i64,
    /// Merged overflow recovery retries.
    pub max_overflow_retries: i64,
}

/// One routed model's concrete pressure and retention budgets (dsh
/// `ResolvedCompactSpec`).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedCompactSpec {
    /// The matched route.
    pub target: RouteTarget,
    /// Adapter-declared full window; the budgets below exclude reserved
    /// output tokens.
    pub context_window: i64,
    /// Merged window fraction for pressure.
    pub threshold_ratio: f64,
    /// Pressure trigger budget.
    pub threshold_tokens: i64,
    /// Verbatim-tail retention budget.
    pub retain_tokens: i64,
    /// Merged summary provider.
    pub summarization_provider: String,
    /// Merged summary model.
    pub summarization_model: String,
    /// Merged summary generation cap.
    pub max_tokens: i64,
    /// Merged pressure compaction retries.
    pub compaction_retries: i64,
    /// Merged overflow recovery retries.
    pub max_overflow_retries: i64,
}

/// Resolve and validate service defaults plus exact-target partial
/// overrides.
///
/// A `Value::Null` config maps to the dsh `config = {}` default parameter
/// (the loader face never produces other non-object shapes).
///
/// # Errors
///
/// Returns the dsh verbatim failure text (kebab-case keys) for unknown
/// keys, malformed values, invariant violations, and duplicate policies.
pub fn resolve_config(config: &Value) -> Result<ResolvedConfig, String> {
    let empty = serde_json::Map::new();
    let map = match config {
        Value::Object(map) => map,
        _ => &empty,
    };
    validate_keys(map, &BASIC_COMPACT_CONFIG_KEYS, "BasicCompactionConfig")?;
    let defaults = parse_policy(map, "BasicCompactionConfig")?;
    if let Some(auto) = map.get("auto")
        && !auto.is_boolean()
    {
        return Err("BasicCompactionConfig: auto must be a boolean".to_owned());
    }

    let headroom_tokens = defaults.headroom_tokens.unwrap_or(DEFAULT_HEADROOM_TOKENS);
    let max_tokens = match map.get("max-tokens") {
        Some(value) => assert_positive_integer("BasicCompactionConfig.max-tokens", value)?,
        None => headroom_tokens,
    };
    if max_tokens <= 0 {
        return Err(format!(
            "BasicCompactionConfig.max-tokens (explicit or from headroom-tokens) \
             ({max_tokens}) must be a positive integer"
        ));
    }
    let threshold_ratio = defaults.threshold_ratio.unwrap_or(DEFAULT_THRESHOLD_RATIO);
    let retention = defaults
        .retention
        .unwrap_or(ResolvedRetention::Ratio(DEFAULT_RETAIN_RATIO));
    validate_ratio_retention(threshold_ratio, retention, "BasicCompactionConfig")?;

    let model_policies = resolve_model_policies(map.get("model-policies"))?;
    let mut resolved_policies = Vec::with_capacity(model_policies.len());
    for (index, policy) in model_policies.into_iter().enumerate() {
        let name = format!("BasicCompactionConfig: model-policies[{index}]");
        let mut policy = policy;
        if policy.max_tokens.is_none()
            && map.get("max-tokens").is_none()
            && policy.headroom_tokens.is_some()
        {
            policy.max_tokens = policy.headroom_tokens;
        }
        let policy_max_tokens = policy.max_tokens.unwrap_or(max_tokens);
        if policy_max_tokens <= 0 {
            return Err(format!(
                "BasicCompactionConfig: model-policies[{index}].max-tokens \
                 (explicit or from headroom-tokens) ({policy_max_tokens}) must be \
                 a positive integer"
            ));
        }
        validate_ratio_retention(
            policy.threshold_ratio.unwrap_or(threshold_ratio),
            policy.retention.unwrap_or(retention),
            &name,
        )?;
        resolved_policies.push(policy);
    }

    Ok(ResolvedConfig {
        threshold_ratio,
        headroom_tokens,
        retention,
        summarization_provider: defaults.summarization_provider.unwrap_or_default(),
        summarization_model: defaults.summarization_model.unwrap_or_default(),
        max_tokens,
        compaction_retries: defaults
            .compaction_retries
            .unwrap_or(DEFAULT_COMPACTION_RETRIES),
        max_overflow_retries: defaults
            .max_overflow_retries
            .unwrap_or(DEFAULT_MAX_OVERFLOW_RETRIES),
        model_policies: resolved_policies,
        auto: map.get("auto").and_then(Value::as_bool).unwrap_or(true),
    })
}

/// Merge the exact provider/model override over the validated default
/// policy (detached result before model-capacity scaling).
#[must_use]
pub fn resolve_target_policy(
    config: &ResolvedConfig,
    provider: &str,
    model: &str,
) -> ResolvedTargetPolicy {
    let override_policy = config
        .model_policies
        .iter()
        .find(|policy| policy.provider == provider && policy.model == model);
    ResolvedTargetPolicy {
        target: RouteTarget {
            provider: provider.to_owned(),
            model: model.to_owned(),
        },
        threshold_ratio: override_policy
            .and_then(|policy| policy.threshold_ratio)
            .unwrap_or(config.threshold_ratio),
        headroom_tokens: override_policy
            .and_then(|policy| policy.headroom_tokens)
            .unwrap_or(config.headroom_tokens),
        retention: override_policy
            .and_then(|policy| policy.retention)
            .unwrap_or(config.retention),
        summarization_provider: override_policy
            .and_then(|policy| policy.summarization_provider.clone())
            .unwrap_or_else(|| config.summarization_provider.clone()),
        summarization_model: override_policy
            .and_then(|policy| policy.summarization_model.clone())
            .unwrap_or_else(|| config.summarization_model.clone()),
        max_tokens: override_policy
            .and_then(|policy| policy.max_tokens)
            .unwrap_or(config.max_tokens),
        compaction_retries: override_policy
            .and_then(|policy| policy.compaction_retries)
            .unwrap_or(config.compaction_retries),
        max_overflow_retries: override_policy
            .and_then(|policy| policy.max_overflow_retries)
            .unwrap_or(config.max_overflow_retries),
    }
}

/// Scale one routed policy into concrete token budgets for its model
/// capacity.
///
/// Pressure is capped by both the window fraction and the capacity remaining
/// after the routed output reservation plus compaction headroom. Retention
/// scales the message budget before headroom is deducted. The dsh
/// non-integer input arms are type-level unreachable here: both budgets
/// arrive as integers by construction.
///
/// # Errors
///
/// Returns the dsh verbatim [`TargetPressureConfigError`] when the window
/// or reservation exhausts the message or pressure budget, or when the
/// resolved retention reaches the pressure threshold.
pub fn resolve_compact_spec(
    policy: &ResolvedTargetPolicy,
    context_window: i64,
    reserved_completion_tokens: i64,
) -> Result<ResolvedCompactSpec, TargetPressureConfigError> {
    let target_key = format!("{}/{}", policy.target.provider, policy.target.model);
    let fail = |message: String| TargetPressureConfigError::new(target_key.clone(), message);
    if context_window <= 0 {
        return Err(fail(format!(
            "BasicCompactionConfig: contextWindow ({context_window}) must be a \
             positive integer"
        )));
    }
    if reserved_completion_tokens < 0 {
        return Err(fail(format!(
            "BasicCompactionConfig: reservedCompletionTokens \
             ({reserved_completion_tokens}) must be a non-negative integer"
        )));
    }
    let message_budget_tokens = context_window - reserved_completion_tokens;
    if message_budget_tokens <= 0 {
        return Err(fail(format!(
            "compaction-basic: {target_key} reserves {reserved_completion_tokens} \
             completion tokens of its {context_window}-token context window, \
             leaving no message budget; configure the adapter model's \
             contextWindow above the effective request maxTokens"
        )));
    }
    let pressure_budget_tokens = message_budget_tokens - policy.headroom_tokens;
    if pressure_budget_tokens <= 0 {
        return Err(fail(format!(
            "compaction-basic: {target_key} reserves {reserved_completion_tokens} \
             completion tokens and {} headroom tokens of its {context_window} \
             -token context window, leaving no pressure budget; reduce the \
             effective request maxTokens or compaction headroomTokens, or \
             configure a larger adapter model contextWindow",
            policy.headroom_tokens
        )));
    }
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "dsh does this math in JS Numbers (f64) and floors it; the budgets stay far below 2^53 and the floor is the specified truncation"
    )]
    let (threshold_tokens, retain_tokens) = {
        let threshold_tokens = ((context_window as f64) * policy.threshold_ratio)
            .min(pressure_budget_tokens as f64)
            .floor() as i64;
        let retain_tokens = match policy.retention {
            ResolvedRetention::Ratio(ratio) => {
                (message_budget_tokens as f64 * ratio).floor() as i64
            }
            ResolvedRetention::Tokens(tokens) => tokens,
        };
        (threshold_tokens, retain_tokens)
    };
    if retain_tokens >= threshold_tokens {
        return Err(fail(format!(
            "BasicCompactionConfig: {}/{} retainTokens ({retain_tokens}) must be \
             less than threshold tokens {threshold_tokens}",
            policy.target.provider, policy.target.model
        )));
    }
    Ok(ResolvedCompactSpec {
        target: policy.target.clone(),
        context_window,
        threshold_ratio: policy.threshold_ratio,
        threshold_tokens,
        retain_tokens,
        summarization_provider: policy.summarization_provider.clone(),
        summarization_model: policy.summarization_model.clone(),
        max_tokens: policy.max_tokens,
        compaction_retries: policy.compaction_retries,
        max_overflow_retries: policy.max_overflow_retries,
    })
}

/// The validated-and-extracted common policy fields of one scope.
struct ParsedPolicy {
    threshold_ratio: Option<f64>,
    headroom_tokens: Option<i64>,
    retention: Option<ResolvedRetention>,
    summarization_provider: Option<String>,
    summarization_model: Option<String>,
    max_tokens: Option<i64>,
    compaction_retries: Option<i64>,
    max_overflow_retries: Option<i64>,
}

/// Validate the fields common to defaults and exact-target partial
/// overrides, extracting the values in the same pass (dsh `validatePolicy`
/// plus the extraction its callers repeat).
fn parse_policy(
    config: &serde_json::Map<String, Value>,
    name: &str,
) -> Result<ParsedPolicy, String> {
    let threshold_ratio = match config.get("threshold-ratio") {
        Some(value) => Some(assert_ratio(&format!("{name}.threshold-ratio"), value)?),
        None => None,
    };
    let headroom_tokens = match config.get("headroom-tokens") {
        Some(value) => Some(assert_non_negative_integer(
            &format!("{name}.headroom-tokens"),
            value,
        )?),
        None => None,
    };
    let retain_ratio = match config.get("retain-ratio") {
        Some(value) => Some(assert_ratio(&format!("{name}.retain-ratio"), value)?),
        None => None,
    };
    let retain_tokens = match config.get("retain-tokens") {
        Some(value) => Some(assert_non_negative_integer(
            &format!("{name}.retain-tokens"),
            value,
        )?),
        None => None,
    };
    if retain_ratio.is_some() && retain_tokens.is_some() {
        return Err(format!(
            "{name}: retain-ratio and retain-tokens are mutually exclusive"
        ));
    }
    let max_tokens = match config.get("max-tokens") {
        Some(value) => Some(assert_positive_integer(
            &format!("{name}.max-tokens"),
            value,
        )?),
        None => None,
    };
    let compaction_retries = match config.get("compaction-retries") {
        Some(value) => Some(assert_non_negative_integer(
            &format!("{name}.compaction-retries"),
            value,
        )?),
        None => None,
    };
    let max_overflow_retries = match config.get("max-overflow-retries") {
        Some(value) => Some(assert_non_negative_integer(
            &format!("{name}.max-overflow-retries"),
            value,
        )?),
        None => None,
    };
    let summarization_provider = parse_optional_string(
        &format!("{name}.summarization-provider"),
        config.get("summarization-provider"),
    )?;
    let summarization_model = parse_optional_string(
        &format!("{name}.summarization-model"),
        config.get("summarization-model"),
    )?;
    validate_summarization_pair(
        name,
        summarization_provider.as_deref(),
        summarization_model.as_deref(),
    )?;
    let retention = match (retain_tokens, retain_ratio) {
        (Some(tokens), _) => Some(ResolvedRetention::Tokens(tokens)),
        (None, Some(ratio)) => Some(ResolvedRetention::Ratio(ratio)),
        (None, None) => None,
    };
    Ok(ParsedPolicy {
        threshold_ratio,
        headroom_tokens,
        retention,
        summarization_provider,
        summarization_model,
        max_tokens,
        compaction_retries,
        max_overflow_retries,
    })
}

/// Require one scope to omit, clear, or replace the summarization target as
/// a pair.
fn validate_summarization_pair(
    name: &str,
    provider: Option<&str>,
    model: Option<&str>,
) -> Result<(), String> {
    let (Some(provider), Some(model)) = (provider, model) else {
        if provider.is_none() && model.is_none() {
            return Ok(());
        }
        return Err(format!(
            "{name}: summarization-provider and summarization-model must be set \
             together as an empty or non-empty pair"
        ));
    };
    if provider.is_empty() != model.is_empty() {
        return Err(format!(
            "{name}: summarization-provider and summarization-model must be set \
             together as an empty or non-empty pair"
        ));
    }
    Ok(())
}

/// Validate, detach, and reject duplicate exact-target policies (dsh
/// `resolveModelPolicies`).
fn resolve_model_policies(
    configured: Option<&Value>,
) -> Result<Vec<ModelCompactPolicyConfig>, String> {
    let Some(configured) = configured else {
        return Ok(Vec::new());
    };
    let Some(list) = configured.as_array() else {
        return Err("BasicCompactionConfig: model-policies must be an array".to_owned());
    };
    let mut seen = HashSet::new();
    let mut policies = Vec::with_capacity(list.len());
    for (index, source) in list.iter().enumerate() {
        let name = format!("BasicCompactionConfig: model-policies[{index}]");
        let policy = parse_model_policy(source, &name)?;
        let key = format!("{}\u{0}{}", policy.provider, policy.model);
        if !seen.insert(key) {
            return Err(format!(
                "BasicCompactionConfig: duplicate model policy for {}/{}",
                policy.provider, policy.model
            ));
        }
        policies.push(policy);
    }
    Ok(policies)
}

/// Validate one untrusted exact-target override and narrow its type (dsh
/// `assertModelPolicy`).
fn parse_model_policy(source: &Value, name: &str) -> Result<ModelCompactPolicyConfig, String> {
    let Some(map) = source.as_object() else {
        return Err(format!("{name} must be an object"));
    };
    validate_keys(map, &MODEL_POLICY_KEYS, name)?;
    let provider = assert_non_empty_string(&format!("{name}.provider"), map.get("provider"))?;
    let model = assert_non_empty_string(&format!("{name}.model"), map.get("model"))?;
    let parsed = parse_policy(map, name)?;
    Ok(ModelCompactPolicyConfig {
        provider,
        model,
        threshold_ratio: parsed.threshold_ratio,
        headroom_tokens: parsed.headroom_tokens,
        retention: parsed.retention,
        summarization_provider: parsed.summarization_provider,
        summarization_model: parsed.summarization_model,
        max_tokens: parsed.max_tokens,
        compaction_retries: parsed.compaction_retries,
        max_overflow_retries: parsed.max_overflow_retries,
    })
}

/// Reject a capacity-independent retention conflict at plugin load.
fn validate_ratio_retention(
    threshold_ratio: f64,
    retention: ResolvedRetention,
    name: &str,
) -> Result<(), String> {
    if let ResolvedRetention::Ratio(ratio) = retention
        && ratio >= threshold_ratio
    {
        return Err(format!(
            "{name}: retain-ratio ({ratio}) must be less than the resolved \
             threshold-ratio ({threshold_ratio})"
        ));
    }
    Ok(())
}

/// Reject stale or misspelled keys before defaults can hide them.
fn validate_keys(
    config: &serde_json::Map<String, Value>,
    keys: &[&str],
    name: &str,
) -> Result<(), String> {
    for key in config.keys() {
        if !keys.contains(&key.as_str()) {
            return Err(format!("{name}: unknown key \"{key}\""));
        }
    }
    Ok(())
}

/// Parse an optional string field (dsh `typeof provider !== 'string'`
/// checks).
fn parse_optional_string(name: &str, value: Option<&Value>) -> Result<Option<String>, String> {
    value.map_or_else(
        || Ok(None),
        |value| {
            value.as_str().map_or_else(
                || Err(format!("{name} must be a string")),
                |text| Ok(Some(text.to_owned())),
            )
        },
    )
}

/// The JS `String(value)` face for error text: raw string content, JSON
/// rendering for the other scalars (the assert faces only ever fire on
/// scalars).
fn js_display(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The integer face of a JSON number under the JS safe-integer discipline.
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "dsh checks these values as JS Numbers; the admitted face is a safe integer, so both casts are exact on it"
)]
fn as_safe_integer(value: &Value) -> Option<i64> {
    let number = value.as_f64()?;
    if number.fract() != 0.0 || number.abs() > (SAFE_INTEGER_MAX as f64) {
        return None;
    }
    Some(number as i64)
}

fn assert_ratio(name: &str, value: &Value) -> Result<f64, String> {
    let Some(number) = value.as_f64() else {
        return Err(format!(
            "{name} ({}) must be a number in (0, 1]",
            js_display(value)
        ));
    };
    if number <= 0.0 || number > 1.0 {
        return Err(format!("{name} ({number}) must be a number in (0, 1]"));
    }
    Ok(number)
}

fn assert_non_negative_integer(name: &str, value: &Value) -> Result<i64, String> {
    match as_safe_integer(value) {
        Some(number) if number >= 0 => Ok(number),
        _ => Err(format!(
            "{name} ({}) must be a non-negative integer",
            js_display(value)
        )),
    }
}

fn assert_positive_integer(name: &str, value: &Value) -> Result<i64, String> {
    match as_safe_integer(value) {
        Some(number) if number > 0 => Ok(number),
        _ => Err(format!(
            "{name} ({}) must be a positive integer",
            js_display(value)
        )),
    }
}

fn assert_non_empty_string(name: &str, value: Option<&Value>) -> Result<String, String> {
    match value.and_then(Value::as_str) {
        Some(text) if !text.is_empty() => Ok(text.to_owned()),
        _ => Err(format!("{name} must be a non-empty string")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PROVIDER: &str = "mock-provider";
    const MODEL: &str = "mock-model";

    fn expect_error(config: &serde_json::Value, fragment: &str) {
        let message = resolve_config(config).expect_err(fragment);
        assert!(
            message.contains(fragment),
            "expected {fragment:?} in {message:?}"
        );
    }

    #[test]
    fn uses_low_friction_service_wide_defaults() {
        let resolved = resolve_config(&json!({})).expect("defaults");
        assert_eq!(
            resolved,
            ResolvedConfig {
                threshold_ratio: 0.8,
                headroom_tokens: 65_536,
                retention: ResolvedRetention::Ratio(0.16),
                summarization_provider: String::new(),
                summarization_model: String::new(),
                max_tokens: 65_536,
                compaction_retries: 1,
                max_overflow_retries: 1,
                model_policies: Vec::new(),
                auto: true,
            }
        );
        // A null config maps to the dsh default-parameter face.
        assert_eq!(resolve_config(&Value::Null).expect("null"), resolved);
    }

    #[test]
    fn defaults_summary_generation_to_headroom_and_keeps_explicit_caps() {
        assert_eq!(
            resolve_config(&json!({"headroom-tokens": 16_384}))
                .expect("headroom")
                .max_tokens,
            16_384
        );
        assert_eq!(
            resolve_config(&json!({"headroom-tokens": 0, "max-tokens": 32}))
                .expect("explicit")
                .max_tokens,
            32
        );
        let policy = |value: serde_json::Value| {
            let config = resolve_config(&value).expect("config");
            resolve_target_policy(&config, PROVIDER, MODEL).max_tokens
        };
        assert_eq!(
            policy(json!({
                "model-policies": [
                    {"provider": PROVIDER, "model": MODEL, "headroom-tokens": 16_384}
                ]
            })),
            16_384
        );
        assert_eq!(
            policy(json!({
                "max-tokens": 512,
                "model-policies": [
                    {"provider": PROVIDER, "model": MODEL, "headroom-tokens": 0}
                ]
            })),
            512
        );
        assert_eq!(
            policy(json!({
                "max-tokens": 512,
                "model-policies": [
                    {"provider": PROVIDER, "model": MODEL, "headroom-tokens": 0, "max-tokens": 32}
                ]
            })),
            32
        );
    }

    #[test]
    fn rejects_a_zero_summary_cap_inherited_from_headroom() {
        expect_error(
            &json!({"headroom-tokens": 0}),
            "BasicCompactionConfig.max-tokens (explicit or from headroom-tokens) (0) \
             must be a positive integer",
        );
        expect_error(
            &json!({
                "model-policies": [
                    {"provider": PROVIDER, "model": MODEL, "headroom-tokens": 0}
                ]
            }),
            "model-policies[0].max-tokens (explicit or from headroom-tokens) (0) \
             must be a positive integer",
        );
    }

    #[test]
    fn resolves_threshold_and_retention_overrides_independently() {
        let threshold_only = resolve_config(&json!({"threshold-ratio": 0.5})).expect("threshold");
        assert_eq!(threshold_only.threshold_ratio, 0.5);
        assert_eq!(threshold_only.retention, ResolvedRetention::Ratio(0.16));

        let retention_only = resolve_config(&json!({"retain-tokens": 70})).expect("retention");
        assert_eq!(retention_only.threshold_ratio, 0.8);
        assert_eq!(retention_only.retention, ResolvedRetention::Tokens(70));
    }

    #[test]
    fn merges_exact_policy_overrides_and_scales_ratios_per_model() {
        let config = resolve_config(&json!({
            "headroom-tokens": 0,
            "max-tokens": 8192,
            "threshold-ratio": 0.8,
            "retain-ratio": 0.1,
            "model-policies": [{
                "provider": "small-provider",
                "model": "shared-id",
                "threshold-ratio": 0.5,
                "headroom-tokens": 600,
                "retain-tokens": 120,
            }],
        }))
        .expect("config");
        let small = resolve_target_policy(&config, "small-provider", "shared-id");
        let other = resolve_target_policy(&config, "large-provider", "shared-id");

        let small_spec = resolve_compact_spec(&small, 1_000, 0).expect("small");
        assert_eq!(small_spec.threshold_tokens, 400);
        assert_eq!(small_spec.retain_tokens, 120);
        let other_spec = resolve_compact_spec(&other, 2_000, 0).expect("other");
        assert_eq!(other_spec.threshold_tokens, 1_600);
        assert_eq!(other_spec.retain_tokens, 200);

        let ratio_config = resolve_config(&json!({
            "headroom-tokens": 0,
            "max-tokens": 8192,
            "retain-tokens": 200,
            "model-policies": [{
                "provider": "ratio-provider",
                "model": "ratio-model",
                "threshold-ratio": 0.6,
                "retain-ratio": 0.2,
                "summarization-provider": "summary-provider",
                "summarization-model": "summary-model",
                "max-tokens": 512,
                "compaction-retries": 2,
                "max-overflow-retries": 3,
            }],
        }))
        .expect("ratio config");
        let ratio = resolve_target_policy(&ratio_config, "ratio-provider", "ratio-model");
        let spec = resolve_compact_spec(&ratio, 2_000, 0).expect("ratio spec");
        assert_eq!(spec.threshold_tokens, 1_200);
        assert_eq!(spec.retain_tokens, 400);
        assert_eq!(spec.summarization_provider, "summary-provider");
        assert_eq!(spec.summarization_model, "summary-model");
        assert_eq!(spec.max_tokens, 512);
        assert_eq!(spec.compaction_retries, 2);
        assert_eq!(spec.max_overflow_retries, 3);
    }

    #[test]
    fn reserves_64k_headroom_across_window_capacities() {
        let policy = resolve_target_policy(
            &resolve_config(&json!({})).expect("defaults"),
            PROVIDER,
            MODEL,
        );
        let rows = [
            (1_048_576, 256_000, 727_040, 126_812),
            (1_000_000, 0, 800_000, 160_000),
            (1_000_000, 100_000, 800_000, 144_000),
            (1_000_000, 134_464, 800_000, 138_485),
            (1_000_000, 256_000, 678_464, 119_040),
        ];
        for (window, output, threshold, retained) in rows {
            let spec = resolve_compact_spec(&policy, window, output).expect("spec");
            assert_eq!(spec.context_window, window);
            assert_eq!(spec.threshold_tokens, threshold, "window {window}");
            assert_eq!(spec.retain_tokens, retained, "window {window}");
        }
    }

    #[test]
    fn rejects_headroom_that_exhausts_the_remaining_capacity() {
        for headroom in [500, 501] {
            let policy = resolve_target_policy(
                &resolve_config(&json!({"headroom-tokens": headroom})).expect("config"),
                PROVIDER,
                MODEL,
            );
            let error = resolve_compact_spec(&policy, 1_000, 500).expect_err("exhausted");
            assert!(
                error.message().contains("leaving no pressure budget"),
                "headroom {headroom}: {}",
                error.message()
            );
        }
    }

    #[test]
    fn rejects_ratio_retention_that_reaches_the_limited_threshold() {
        let policy = resolve_target_policy(
            &resolve_config(&json!({"headroom-tokens": 420})).expect("config"),
            PROVIDER,
            MODEL,
        );
        let error = resolve_compact_spec(&policy, 1_000, 500).expect_err("threshold");
        assert_eq!(
            error.message(),
            "BasicCompactionConfig: mock-provider/mock-model retainTokens (80) \
             must be less than threshold tokens 80"
        );
    }

    #[test]
    fn rejects_a_reserve_that_leaves_no_message_budget() {
        let policy = resolve_target_policy(
            &resolve_config(&json!({})).expect("defaults"),
            PROVIDER,
            MODEL,
        );
        let equal = resolve_compact_spec(&policy, 1_000, 1_000).expect_err("equal");
        assert!(
            equal.message().contains(
                "reserves 1000 completion tokens of its 1000-token \
                           context window, leaving no message budget"
            ),
            "{}",
            equal.message()
        );
        let over = resolve_compact_spec(&policy, 1_000, 1_500).expect_err("over");
        assert!(over.message().contains("leaving no message budget"));
        let negative = resolve_compact_spec(&policy, 1_000, -1).expect_err("negative");
        assert_eq!(
            negative.message(),
            "BasicCompactionConfig: reservedCompletionTokens (-1) must be a \
             non-negative integer"
        );
        let zero_window = resolve_compact_spec(&policy, 0, 0).expect_err("zero");
        assert_eq!(
            zero_window.message(),
            "BasicCompactionConfig: contextWindow (0) must be a positive integer"
        );
    }

    #[test]
    fn inherits_clears_and_replaces_the_summarization_pair() {
        let config = resolve_config(&json!({
            "summarization-provider": "default-provider",
            "summarization-model": "default-model",
            "model-policies": [
                {"provider": "inherit-provider", "model": MODEL},
                {
                    "provider": "clear-provider",
                    "model": MODEL,
                    "summarization-provider": "",
                    "summarization-model": "",
                },
                {
                    "provider": "replace-provider",
                    "model": MODEL,
                    "summarization-provider": "replacement-provider",
                    "summarization-model": "replacement-model",
                },
            ],
        }))
        .expect("config");
        let inherit = resolve_target_policy(&config, "inherit-provider", MODEL);
        assert_eq!(inherit.summarization_provider, "default-provider");
        assert_eq!(inherit.summarization_model, "default-model");
        let clear = resolve_target_policy(&config, "clear-provider", MODEL);
        assert_eq!(clear.summarization_provider, "");
        assert_eq!(clear.summarization_model, "");
        let replace = resolve_target_policy(&config, "replace-provider", MODEL);
        assert_eq!(replace.summarization_provider, "replacement-provider");
        assert_eq!(replace.summarization_model, "replacement-model");
    }

    #[test]
    fn rejects_invalid_top_level_values() {
        let bad: Vec<(serde_json::Value, &str)> = vec![
            (json!({"max-tokens": 0}), "max-tokens"),
            (json!({"max-tokens": 0}), "must be a positive integer"),
            (
                json!({"headroom-tokens": -1}),
                "headroom-tokens (-1) must be a non-negative integer",
            ),
            (
                json!({"headroom-tokens": 0.5}),
                "headroom-tokens (0.5) must be a non-negative integer",
            ),
            (
                json!({"headroom-tokens": "65536"}),
                "headroom-tokens (65536) must be a non-negative integer",
            ),
            (
                json!({"model-policies": [
                    {"provider": PROVIDER, "model": MODEL, "headroom-tokens": -1}
                ]}),
                "model-policies[0].headroom-tokens (-1) must be a non-negative \
                 integer",
            ),
            (
                json!({"compaction-retries": -1}),
                "compaction-retries (-1) must be a non-negative integer",
            ),
            (
                json!({"max-overflow-retries": -1}),
                "max-overflow-retries (-1) must be a non-negative integer",
            ),
            (json!({"auto": "yes"}), "auto must be a boolean"),
            (
                json!({"summarization-provider": 1}),
                "summarization-provider must be a string",
            ),
            (
                json!({"summarization-model": 1}),
                "summarization-model must be a string",
            ),
            (
                json!({"summarization-provider": MODEL}),
                "must be set together",
            ),
            (
                json!({"summarization-model": MODEL}),
                "must be set together",
            ),
            (
                json!({"summarization-provider": ""}),
                "must be set together",
            ),
            (json!({"summarization-model": ""}), "must be set together"),
            (
                json!({"threshold-ratio": 0}),
                "threshold-ratio (0) must be a number in (0, 1]",
            ),
            (
                json!({"threshold-ratio": 1.1}),
                "threshold-ratio (1.1) must be a number in (0, 1]",
            ),
            (
                json!({"retain-ratio": 0.9}),
                "retain-ratio (0.9) must be less than the resolved \
                 threshold-ratio (0.8)",
            ),
            (
                json!({"threshold-ratio": 0.1}),
                "retain-ratio (0.16) must be less than the resolved \
                 threshold-ratio (0.1)",
            ),
            (
                json!({"retain-tokens": -1}),
                "retain-tokens (-1) must be a non-negative integer",
            ),
            (
                json!({"retain-ratio": 0.2, "retain-tokens": 100}),
                "retain-ratio and retain-tokens are mutually exclusive",
            ),
            (
                json!({"models": {}}),
                "BasicCompactionConfig: unknown key \"models\"",
            ),
            (
                json!({"thresholdRato": 0.5}),
                "BasicCompactionConfig: unknown key \"thresholdRato\"",
            ),
        ];
        for (config, fragment) in bad {
            expect_error(&config, fragment);
        }
    }

    #[test]
    fn rejects_invalid_model_policy_entries() {
        let bad: Vec<(serde_json::Value, &str)> = vec![
            (
                json!({"model-policies": [
                    {"provider": PROVIDER, "model": MODEL, "headroom-tokens": -1}
                ]}),
                "model-policies[0].headroom-tokens (-1) must be a non-negative \
                 integer",
            ),
            (
                json!({"model-policies": {}}),
                "model-policies must be an array",
            ),
            (
                json!({"model-policies": [1]}),
                "model-policies[0] must be an object",
            ),
            (
                json!({"model-policies": [null]}),
                "model-policies[0] must be an object",
            ),
            (
                json!({"model-policies": [[]]}),
                "model-policies[0] must be an object",
            ),
            (
                json!({"model-policies": [{"provider": 1, "model": MODEL}]}),
                "provider must be a non-empty string",
            ),
            (
                json!({"model-policies": [{"provider": "", "model": MODEL}]}),
                "provider must be a non-empty string",
            ),
            (
                json!({"model-policies": [{"provider": PROVIDER, "model": 1}]}),
                "model must be a non-empty string",
            ),
            (
                json!({"model-policies": [{"provider": PROVIDER, "model": ""}]}),
                "model must be a non-empty string",
            ),
            (
                json!({"model-policies": [
                    {"provider": PROVIDER, "model": MODEL, "summarization-provider": 1}
                ]}),
                "model-policies[0].summarization-provider must be a string",
            ),
            (
                json!({
                    "summarization-provider": "default-provider",
                    "summarization-model": "default-model",
                    "model-policies": [
                        {"provider": PROVIDER, "model": MODEL, "summarization-model": ""}
                    ],
                }),
                "model-policies[0]: summarization-provider and summarization-model \
                 must be set together",
            ),
            (
                json!({
                    "summarization-provider": "default-provider",
                    "summarization-model": "default-model",
                    "model-policies": [
                        {"provider": PROVIDER, "model": MODEL, "summarization-provider": ""}
                    ],
                }),
                "model-policies[0]: summarization-provider and summarization-model \
                 must be set together",
            ),
            (
                json!({"model-policies": [
                    {"provider": PROVIDER, "model": MODEL, "retain-ratio": 0.2, "retain-tokens": 100}
                ]}),
                "model-policies[0]: retain-ratio and retain-tokens are mutually \
                 exclusive",
            ),
            (
                json!({"model-policies": [
                    {"provider": PROVIDER, "model": MODEL, "threshold-ratio": 0.1}
                ]}),
                "model-policies[0]: retain-ratio (0.16) must be less than the \
                 resolved threshold-ratio (0.1)",
            ),
            (
                json!({"model-policies": [
                    {"provider": PROVIDER, "model": MODEL, "retain-ratio": 0.9}
                ]}),
                "model-policies[0]: retain-ratio (0.9) must be less than the \
                 resolved threshold-ratio (0.8)",
            ),
            (
                json!({"model-policies": [
                    {"provider": PROVIDER, "model": MODEL},
                    {"provider": PROVIDER, "model": MODEL},
                ]}),
                "duplicate model policy for mock-provider/mock-model",
            ),
        ];
        for (config, fragment) in bad {
            expect_error(&config, fragment);
        }
    }

    #[test]
    fn pressure_invariants_reject_at_spec_time() {
        let invalid = resolve_target_policy(
            &resolve_config(&json!({
                "headroom-tokens": 0,
                "max-tokens": 8192,
                "threshold-ratio": 0.5,
                "retain-tokens": 500,
            }))
            .expect("config"),
            PROVIDER,
            MODEL,
        );
        let error = resolve_compact_spec(&invalid, 1_000, 0).expect_err("invalid");
        assert!(
            error
                .message()
                .contains("retainTokens (500) must be less than threshold tokens 500"),
            "{}",
            error.message()
        );
        assert_eq!(error.target_key, "mock-provider/mock-model");
    }
}
