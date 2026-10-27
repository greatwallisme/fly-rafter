//! Request construction (pi-ai `api/anthropic-messages.ts`
//! `buildParams`/`streamSimple`, anthropic-messages.ts:928-976 and
//! 1124-1287, over `api/simple-options.ts`).
//!
//! The dead arms of the dsh shape stay documented here: `tool_choice`,
//! `metadata.user_id`, and `fallbacks` never arrive from the harness
//! seam; the managed-effort face (`supportsMidConvoEffort`), the
//! server-side-fallback face (`allowedFallbackModels`), and the
//! in-history tool-change face are withheld compat switches only an
//! installed catalog sets (decision 45 A2/D16-D24).
//!
//! `betas` ride the `anthropic-beta` header, not the body (the SDK
//! destructures them out at resources/beta/messages/messages.ts:91/:126);
//! [`resolve_betas`] returns the header value's comma-joined parts.

use serde_json::{Map, Value};

use crate::compat::AnthropicCompat;
use crate::config::{CacheRetention, ThinkingBudgets};
use crate::estimate::clamp_max_tokens_to_context;
use crate::models::{LevelMapping, ModelDescriptor, ThinkingLevel};
use fr_llm::adapter::GenerateOptions;

/// The OAuth identity beta (pi-ai `getBetaFeatures`,
/// anthropic-messages.ts:1106).
pub const CLAUDE_CODE_BETA: &str = "claude-code-20250219";

/// The OAuth transport beta.
pub const OAUTH_BETA: &str = "oauth-2025-04-20";

/// The fine-grained tool streaming beta (only when the endpoint lacks
/// eager input streaming).
pub const FINE_GRAINED_TOOL_STREAMING_BETA: &str = "fine-grained-tool-streaming-2025-05-14";

/// The interleaved thinking beta (non-adaptive thinking models).
pub const INTERLEAVED_THINKING_BETA: &str = "interleaved-thinking-2025-05-14";

/// Tokens always left for the answer when a thinking budget shares the
/// response ceiling (pi-ai `MIN_ANSWER_TOKENS`, simple-options.ts:68).
pub const MIN_ANSWER_TOKENS: u64 = 1024;

/// The default per-level thinking budgets (pi-ai
/// `DEFAULT_THINKING_BUDGETS`, simple-options.ts:70-75).
pub const DEFAULT_THINKING_BUDGETS: ThinkingBudgets = ThinkingBudgets {
    minimal: 1024,
    low: 2048,
    medium: 8192,
    high: 16384,
};

/// The resolved per-request options the adapter hands to
/// [`build_params`] (the pi-ai `streamSimple` product restricted to the
/// reachable faces).
#[derive(Debug, Clone, PartialEq)]
pub struct RequestOptions {
    /// Whether thinking was requested (an effort or a budget path).
    pub thinking_enabled: bool,
    /// The adaptive effort literal, on the `force-adaptive-thinking`
    /// path (pi-ai `AnthropicEffort`).
    pub effort: Option<String>,
    /// The thinking budget, on the budget path.
    pub thinking_budget_tokens: Option<u64>,
    /// The response ceiling (already context-clamped).
    pub max_tokens: u64,
    /// The caller temperature, when set.
    pub temperature: Option<f64>,
    /// The prompt-cache retention preference.
    pub cache_retention: CacheRetention,
}

/// `clampReasoning`: the budget path sizes `xhigh`/`max` as `high`
/// (simple-options.ts:77-79).
const fn clamp_reasoning(level: ThinkingLevel) -> ThinkingLevel {
    match level {
        ThinkingLevel::Xhigh | ThinkingLevel::Max => ThinkingLevel::High,
        other => other,
    }
}

/// The budget one level maps to, custom budgets winning per field
/// (pi-ai `thinkingBudgetForLevel`, simple-options.ts:81-85).
fn thinking_budget_for_level(level: ThinkingLevel, custom: Option<&ThinkingBudgets>) -> u64 {
    let budgets = custom.unwrap_or(&DEFAULT_THINKING_BUDGETS);
    match clamp_reasoning(level) {
        ThinkingLevel::Minimal => budgets.minimal,
        ThinkingLevel::Low => budgets.low,
        ThinkingLevel::Medium => budgets.medium,
        _ => budgets.high,
    }
}

/// The effort literal one level dispatches to (pi-ai
/// `mapThinkingLevelToEffort`, anthropic-messages.ts:908-926): an
/// explicit wire spelling wins; `minimal`/`low` map to `low`, `medium`
/// to `medium`, everything else to `high`.
fn effort_for_level(model: &ModelDescriptor, level: ThinkingLevel) -> String {
    if let Some(map) = &model.thinking_level_map
        && let Some(LevelMapping::Wire(wire)) = map.get(&level)
    {
        return wire.clone();
    }
    match level {
        ThinkingLevel::Minimal | ThinkingLevel::Low => "low".to_owned(),
        ThinkingLevel::Medium => "medium".to_owned(),
        _ => "high".to_owned(),
    }
}

/// Resolves the per-request options (pi-ai `streamSimple`,
/// anthropic-messages.ts:928-976).
///
/// The entry cap context-clamps once; the budget path may lift it by the
/// thinking budget under the model cap and clamps again, and the budget
/// never spends the answer room.
#[must_use]
pub fn build_request_options(
    model: &ModelDescriptor,
    compat: &AnthropicCompat,
    options: &GenerateOptions,
    reasoning: Option<ThinkingLevel>,
    thinking_budgets: Option<&ThinkingBudgets>,
    cache_retention: CacheRetention,
) -> RequestOptions {
    // context_window is a positive integer by the config validation, so
    // the i64 conversion cannot overflow the checked domain.
    let context_window = i64::try_from(model.context_window).unwrap_or(i64::MAX);
    let base = clamp_max_tokens_to_context(
        context_window,
        options.max_tokens,
        model.max_tokens,
        options,
    );
    match reasoning.filter(|level| *level != ThinkingLevel::Off) {
        None => RequestOptions {
            thinking_enabled: false,
            effort: None,
            thinking_budget_tokens: None,
            max_tokens: base,
            temperature: options.temperature,
            cache_retention,
        },
        Some(level) if compat.force_adaptive_thinking => RequestOptions {
            thinking_enabled: true,
            effort: Some(effort_for_level(model, level)),
            thinking_budget_tokens: None,
            max_tokens: base,
            temperature: options.temperature,
            cache_retention,
        },
        Some(level) => {
            // adjustMaxTokensForThinking (simple-options.ts:92-108): an
            // explicit entry cap may grow by the budget under the model
            // cap; an entry that cannot hold the budget spends only the
            // answer room.
            let budget = thinking_budget_for_level(level, thinking_budgets);
            let mut thinking_budget = budget;
            let adjusted_max = options.max_tokens.map_or(model.max_tokens, |entry| {
                entry.saturating_add(budget).min(model.max_tokens)
            });
            if adjusted_max <= thinking_budget {
                thinking_budget =
                    thinking_budget.min(adjusted_max.saturating_sub(MIN_ANSWER_TOKENS));
            }
            let max_tokens = clamp_max_tokens_to_context(
                context_window,
                Some(adjusted_max),
                model.max_tokens,
                options,
            );
            RequestOptions {
                thinking_enabled: true,
                effort: None,
                thinking_budget_tokens: Some(
                    thinking_budget.min(max_tokens.saturating_sub(MIN_ANSWER_TOKENS)),
                ),
                max_tokens,
                temperature: options.temperature,
                cache_retention,
            }
        }
    }
}

/// The `cache_control` value one retention resolves to (pi-ai
/// `getCacheControl`, anthropic-messages.ts:79-93): `none` sends none;
/// `long` upgrades to the one-hour TTL where the endpoint takes it.
#[must_use]
pub fn cache_control_value(retention: CacheRetention, compat: &AnthropicCompat) -> Option<Value> {
    if retention == CacheRetention::None {
        return None;
    }
    let ttl = retention == CacheRetention::Long && compat.supports_long_cache_retention;
    Some(if ttl {
        serde_json::json!({ "type": "ephemeral", "ttl": "1h" })
    } else {
        serde_json::json!({ "type": "ephemeral" })
    })
}

/// The beta list one request assembles (pi-ai `getBetaFeatures`,
/// anthropic-messages.ts:1080-1122).
///
/// An explicit `anthropic-beta` header value (model or profile headers,
/// D24) replaces the assembly with its comma-separated parts; the dsh
/// `null` sentinel (empty list) is unreachable through the string-valued
/// headers face. The server-side-fallback, managed-effort, and
/// inline-tools betas belong to withheld compat faces and never
/// assemble.
#[must_use]
pub fn resolve_betas(
    compat: &AnthropicCompat,
    model_reasoning: bool,
    options: &RequestOptions,
    has_tools: bool,
    is_oauth: bool,
    explicit_beta: Option<&str>,
) -> Vec<String> {
    let push = |betas: &mut Vec<String>, beta: &str| {
        if !betas.iter().any(|seen| seen == beta) {
            betas.push(beta.to_owned());
        }
    };
    if let Some(explicit) = explicit_beta {
        let mut betas = Vec::new();
        for part in explicit.split(',') {
            let part = part.trim();
            if !part.is_empty() {
                push(&mut betas, part);
            }
        }
        return betas;
    }
    let mut betas = Vec::new();
    if is_oauth {
        push(&mut betas, CLAUDE_CODE_BETA);
        push(&mut betas, OAUTH_BETA);
    }
    if has_tools && !compat.supports_eager_tool_input_streaming {
        push(&mut betas, FINE_GRAINED_TOOL_STREAMING_BETA);
    }
    if model_reasoning && options.thinking_enabled && !compat.force_adaptive_thinking {
        push(&mut betas, INTERLEAVED_THINKING_BETA);
    }
    betas
}

/// Builds the request body (pi-ai `buildParams`,
/// anthropic-messages.ts:1124-1287, reachable faces in dsh key order:
/// `model`, `messages`, `max_tokens`, `stream`, `system`,
/// `temperature`, `tools`, `thinking`).
///
/// The OAuth face prefixes the identity system block
/// (`You are Claude Code, Anthropic's official CLI for Claude.`, D15-a).
#[expect(
    clippy::too_many_arguments,
    reason = "one parameter per dsh buildParams input (anthropic-messages.ts:1124-1155); a               struct would restate the dsh call shape the port keeps flat"
)]
#[must_use]
pub fn build_params(
    model: &ModelDescriptor,
    options: &RequestOptions,
    compat: &AnthropicCompat,
    is_oauth: bool,
    messages: Vec<Value>,
    system_text: &str,
    tools: Option<Vec<Value>>,
    system_cache_control: Option<&Value>,
) -> Value {
    let model_id = model.id.as_str();
    let mut body = Map::new();
    body.insert("model".to_owned(), Value::String(model_id.to_owned()));
    body.insert("messages".to_owned(), Value::Array(messages));
    body.insert(
        "max_tokens".to_owned(),
        Value::Number(serde_json::Number::from(options.max_tokens)),
    );
    body.insert("stream".to_owned(), Value::Bool(true));

    let system_block = |text: &str| {
        let mut block = Map::new();
        block.insert("type".to_owned(), Value::String("text".to_owned()));
        block.insert("text".to_owned(), Value::String(text.to_owned()));
        if let Some(cache_control) = system_cache_control {
            block.insert("cache_control".to_owned(), cache_control.clone());
        }
        Value::Object(block)
    };
    if is_oauth {
        let mut system = vec![system_block(
            "You are Claude Code, Anthropic's official CLI for Claude.",
        )];
        if !system_text.is_empty() {
            system.push(system_block(system_text));
        }
        body.insert("system".to_owned(), Value::Array(system));
    } else if !system_text.is_empty() {
        let system = vec![system_block(system_text)];
        body.insert("system".to_owned(), Value::Array(system));
    }

    // Temperature is incompatible with thinking and unsupported on some
    // models; the managed-effort exclusion is a withheld face.
    if let Some(temperature) = options.temperature
        && !options.thinking_enabled
        && compat.supports_temperature
    {
        body.insert(
            "temperature".to_owned(),
            serde_json::Number::from_f64(temperature).map_or_else(|| Value::Null, Value::Number),
        );
    }

    if let Some(tools) = tools.filter(|tools| !tools.is_empty()) {
        body.insert("tools".to_owned(), Value::Array(tools));
    }

    // The thinking face (anthropic-messages.ts:1233-1264, reachable
    // arms): a reasoning model sends adaptive thinking under
    // `force-adaptive-thinking` (with its effort), budget thinking
    // otherwise, and `disabled` only when the model's map keeps `off`
    // non-null. The managed-effort adaptive arm (`drop_block` binding
    // plus a hardcoded request effort) is a withheld face.
    if model.reasoning {
        if options.thinking_enabled {
            if compat.force_adaptive_thinking {
                let mut thinking = Map::new();
                thinking.insert("type".to_owned(), Value::String("adaptive".to_owned()));
                thinking.insert("display".to_owned(), Value::String("summarized".to_owned()));
                body.insert("thinking".to_owned(), Value::Object(thinking));
                if let Some(effort) = &options.effort {
                    body.insert(
                        "output_config".to_owned(),
                        serde_json::json!({ "effort": effort }),
                    );
                }
            } else {
                body.insert(
                    "thinking".to_owned(),
                    serde_json::json!({
                        "type": "enabled",
                        "budget_tokens": options.thinking_budget_tokens.unwrap_or(1024),
                        "display": "summarized",
                    }),
                );
            }
        } else if model
            .thinking_level_map
            .as_ref()
            .and_then(|map| map.get(&ThinkingLevel::Off))
            .is_some_and(|mapping| *mapping != LevelMapping::Unsupported)
        {
            body.insert(
                "thinking".to_owned(),
                serde_json::json!({ "type": "disabled" }),
            );
        }
    }
    Value::Object(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::Api;
    use crate::models::Modality;
    use std::collections::BTreeMap;

    fn descriptor(reasoning: bool, map: Option<ThinkingLevelMap2>) -> ModelDescriptor {
        ModelDescriptor {
            id: "claude-test".to_owned(),
            name: "Claude Test".to_owned(),
            api: Api::AnthropicMessages,
            provider: "anthropic".to_owned(),
            base_url: "https://api.anthropic.com".to_owned(),
            input: vec![Modality::Text],
            context_window: 100_000,
            max_tokens: 8_192,
            reasoning,
            thinking_level_map: map,
            compat: crate::compat::AnthropicCompat {
                supports_eager_tool_input_streaming: true,
                supports_long_cache_retention: true,
                supports_cache_control_on_tools: true,
                supports_temperature: true,
                force_adaptive_thinking: false,
                allow_empty_signature: false,
                supports_strict_tools: false,
            },
        }
    }

    type ThinkingLevelMap2 = BTreeMap<ThinkingLevel, LevelMapping>;

    fn options() -> GenerateOptions {
        GenerateOptions {
            provider: "anthropic".to_owned(),
            model: "claude-test".to_owned(),
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

    /// The budget chain: an unclamped entry lets the budget grow to the
    /// model cap, and a cap that cannot hold the budget spends only the
    /// answer room.
    #[test]
    fn budget_chain_pins_the_answer_room() {
        let model = descriptor(true, None);
        let compat = model.compat;
        let resolved = build_request_options(
            &model,
            &compat,
            &options(),
            Some(ThinkingLevel::High),
            None,
            CacheRetention::Short,
        );
        assert!(resolved.thinking_enabled);
        assert_eq!(resolved.effort, None);
        // Entry absent: the ceiling stays the model cap and the default
        // high budget (16384) exceeds it, so the budget clamps to
        // 8192 - 1024.
        assert_eq!(resolved.max_tokens, 8_192);
        assert_eq!(resolved.thinking_budget_tokens, Some(8_192 - 1_024));
    }

    /// The adaptive arm: effort dispatch, no budget, adaptive thinking
    /// body with its `output_config` effort.
    #[test]
    fn adaptive_arm_dispatches_effort() {
        let mut map = BTreeMap::new();
        map.insert(ThinkingLevel::Off, LevelMapping::SendNothing);
        map.insert(ThinkingLevel::Xhigh, LevelMapping::Wire("xhigh".to_owned()));
        let model = descriptor(true, Some(map));
        let mut compat = model.compat;
        compat.force_adaptive_thinking = true;
        let resolved = build_request_options(
            &model,
            &compat,
            &options(),
            Some(ThinkingLevel::Xhigh),
            None,
            CacheRetention::Short,
        );
        assert_eq!(resolved.effort.as_deref(), Some("xhigh"));
        let body = build_params(
            &model,
            &resolved,
            &compat,
            false,
            Vec::new(),
            "sys",
            None,
            None,
        );
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["thinking"]["display"], "summarized");
        assert_eq!(body["output_config"]["effort"], "xhigh");
        assert!(body.get("temperature").is_none());
    }

    /// Body assembly: key order, the budget thinking body, the disabled
    /// arm, temperature gating, system blocks (plain and OAuth), and the
    /// retained `off` face.
    #[test]
    fn body_assembly_arms() {
        let mut map = BTreeMap::new();
        map.insert(ThinkingLevel::Off, LevelMapping::SendNothing);
        let model = descriptor(true, Some(map));
        let compat = model.compat;
        let mut opts = options();
        opts.temperature = Some(0.7);
        let resolved = build_request_options(
            &model,
            &compat,
            &opts,
            Some(ThinkingLevel::High),
            None,
            CacheRetention::Short,
        );
        let body = build_params(
            &model,
            &resolved,
            &compat,
            false,
            Vec::new(),
            "sys",
            None,
            None,
        );
        let keys: Vec<&str> = body
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        // Temperature is incompatible with thinking, so the budget body
        // omits it even though the caller set one.
        assert_eq!(
            keys,
            [
                "model",
                "messages",
                "max_tokens",
                "stream",
                "system",
                "thinking"
            ]
        );
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 8_192 - 1_024);

        // Off: disabled thinking, temperature rides, no system block for
        // an empty prompt.
        let resolved = build_request_options(
            &model,
            &compat,
            &opts,
            Some(ThinkingLevel::Off),
            None,
            CacheRetention::Short,
        );
        let body = build_params(
            &model,
            &resolved,
            &compat,
            false,
            Vec::new(),
            "",
            None,
            None,
        );
        assert_eq!(body["thinking"]["type"], "disabled");
        assert_eq!(body["temperature"], 0.7);
        assert!(body.get("system").is_none());

        // OAuth: the identity block leads the system array.
        let body = build_params(
            &model,
            &resolved,
            &compat,
            true,
            Vec::new(),
            "sys",
            None,
            Some(&serde_json::json!({ "type": "ephemeral" })),
        );
        let system = body["system"].as_array().expect("system");
        assert_eq!(system.len(), 2);
        assert_eq!(
            system[0]["text"],
            "You are Claude Code, Anthropic's official CLI for Claude."
        );
        assert_eq!(system[1]["text"], "sys");
        assert_eq!(
            system[0]["cache_control"],
            serde_json::json!({ "type": "ephemeral" })
        );
    }

    /// Betas: interleaved on the budget path only, fine-grained without
    /// eager streaming, the OAuth pair, and the explicit override.
    #[test]
    fn betas_assemble_the_reachable_set() {
        let mut map = BTreeMap::new();
        map.insert(ThinkingLevel::Off, LevelMapping::SendNothing);
        let model = descriptor(true, Some(map));
        let compat = model.compat;
        let budget = build_request_options(
            &model,
            &compat,
            &options(),
            Some(ThinkingLevel::High),
            None,
            CacheRetention::Short,
        );
        assert_eq!(
            resolve_betas(&compat, true, &budget, true, false, None),
            [INTERLEAVED_THINKING_BETA.to_owned()]
        );
        let off = build_request_options(
            &model,
            &compat,
            &options(),
            Some(ThinkingLevel::Off),
            None,
            CacheRetention::Short,
        );
        assert_eq!(
            resolve_betas(&compat, true, &off, true, false, None),
            [] as [String; 0]
        );
        let mut no_eager = compat;
        no_eager.supports_eager_tool_input_streaming = false;
        assert_eq!(
            resolve_betas(&no_eager, true, &off, true, false, None),
            [FINE_GRAINED_TOOL_STREAMING_BETA.to_owned()]
        );
        assert_eq!(
            resolve_betas(&compat, false, &off, false, true, None),
            [CLAUDE_CODE_BETA.to_owned(), OAUTH_BETA.to_owned()]
        );
        assert_eq!(
            resolve_betas(
                &compat,
                true,
                &budget,
                true,
                false,
                Some(" b ,oauth-2025-04-20, ")
            ),
            ["b".to_owned(), OAUTH_BETA.to_owned()]
        );
    }
}
// Rust guideline compliant 2026-10-27
