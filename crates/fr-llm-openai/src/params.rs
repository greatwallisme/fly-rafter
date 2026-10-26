//! Request parameter assembly (pi-ai `buildParams`,
//! openai-completions.ts:802-1010).
//!
//! Key order is the dsh insertion order (`serde_json` `preserve_order`):
//! model/messages/stream, prompt-cache faces, stream options, store, the
//! output cap, temperature, tools, `Anthropic` cache control, tool choice,
//! vLLM priority, the thinking-dispatch dialect, the top-level thinking
//! budget, and (last) any sampling overrides. The `samplingParamsByThinkingLevel`
//! and request-level sampling faces are fr-unreachable (fr route/model
//! vocabulary carries no such tables), so the last-wins merge is dead here;
//! the `openRouterRouting`/`vercelGatewayRouting` faces are withheld keys
//! (catalog.ts:268-275) and `zaiToolStream` is withheld the same way.

use serde_json::{Map, Value};

use fr_wire::call_config::ToolSchema;

use crate::compat::{
    CacheControlFormat, ChatTemplateKwargValue,
    ChatTemplateKwargValue::{Dispatch, Scalar},
    ChatTemplateVar, MaxTokensField, ResolvedCompletionsCompat, ThinkingFormat,
    ThinkingTokenBudgetField,
};
use crate::convert::convert_tools;
use crate::models::{LevelMapping, ModelDescriptor, ThinkingLevel, ThinkingLevelMap};
use crate::prompt_cache::clamp_openai_prompt_cache_key;

/// `DEFAULT_THINKING_BUDGETS` (simple-options.ts:70-75).
const DEFAULT_THINKING_BUDGETS: (u64, u64, u64, u64) = (1024, 2048, 8192, 16384);
/// `MIN_ANSWER_TOKENS` (simple-options.ts:68).
const MIN_ANSWER_TOKENS: u64 = 1024;

/// The per-request controls `buildParams` reads; the effort is already the
/// composition of the route default and the request override.
#[derive(Debug)]
pub struct BuildParamsOptions<'a> {
    /// The reasoning effort for this request (`off` = reasoning off).
    pub effort: Option<ThinkingLevel>,
    /// Sampling temperature.
    pub temperature: Option<f64>,
    /// The output cap (D14: always sent — the entry chain clamped it).
    pub max_tokens: Option<u64>,
    /// The session id feeding the prompt-cache key.
    pub session_id: Option<&'a str>,
    /// The caller tool-choice face.
    pub tool_choice: Option<Value>,
}

/// The wire spelling of a level (dsh `reasoningEffort` literals).
#[must_use]
pub const fn level_spelling(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Off => "off",
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::Xhigh => "xhigh",
        ThinkingLevel::Max => "max",
    }
}

/// The mapped effort for a level: `Wire` spells the provider string;
/// `Unsupported`/`SendNothing` are dsh `null` (never sent).
fn mapped(map: &ThinkingLevelMap, level: ThinkingLevel) -> Option<&str> {
    match map.get(&level) {
        Some(LevelMapping::Wire(spelling)) => Some(spelling.as_str()),
        _ => None,
    }
}

/// `clampReasoning`: `xhigh`/`max` clamp to `high` (simple-options.ts:77-79).
const fn clamp_reasoning(level: ThinkingLevel) -> ThinkingLevel {
    match level {
        ThinkingLevel::Xhigh | ThinkingLevel::Max => ThinkingLevel::High,
        other => other,
    }
}

/// `thinkingBudgetForLevel` with the route budget overrides.
#[must_use]
pub fn thinking_budget_for_level(
    level: ThinkingLevel,
    budgets: Option<&crate::config::ThinkingBudgets>,
) -> u64 {
    let (minimal, low, medium, high) = budgets.map_or(DEFAULT_THINKING_BUDGETS, |custom| {
        (custom.minimal, custom.low, custom.medium, custom.high)
    });
    match clamp_reasoning(level) {
        ThinkingLevel::Minimal => minimal,
        ThinkingLevel::Low => low,
        ThinkingLevel::Medium => medium,
        _ => high,
    }
}

/// `clampThinkingBudgetToAnswerRoom`: at least `MIN_ANSWER_TOKENS` of the
/// shared ceiling stays for the answer.
#[must_use]
pub fn clamp_thinking_budget_to_answer_room(budget: u64, ceiling: u64) -> u64 {
    budget.min(ceiling.saturating_sub(MIN_ANSWER_TOKENS))
}

/// `resolveThinkingTokenBudgetField` (openai-completions.ts:1012-1018).
fn thinking_token_budget_field(
    compat: &ResolvedCompletionsCompat,
) -> Option<ThinkingTokenBudgetField> {
    compat.thinking_token_budget_field.or_else(|| {
        compat
            .supports_thinking_token_budget
            .then_some(ThinkingTokenBudgetField::ThinkingTokenBudget)
    })
}

/// `resolveChatTemplateKwargValue` (openai-completions.ts:1052-1075): a
/// scalar rides as-is; a `$var` dispatch resolves per request. The
/// effort-spelling dispatch falls back to the raw spelling only when the
/// map has no entry, and a non-string mapping drops the entry.
fn chat_template_value(
    value: &ChatTemplateKwargValue,
    map: &ThinkingLevelMap,
    effort: Option<ThinkingLevel>,
    budget: Option<u64>,
) -> Option<Value> {
    match value {
        Scalar(scalar) => Some(match scalar {
            crate::compat::ScalarValue::Str(text) => Value::String(text.clone()),
            crate::compat::ScalarValue::Num(number) => {
                serde_json::Number::from_f64(*number).map_or(Value::Null, Value::Number)
            }
            crate::compat::ScalarValue::Bool(flag) => Value::Bool(*flag),
            crate::compat::ScalarValue::Null => Value::Null,
        }),
        Dispatch { var, omit_when_off } => {
            if effort.is_none() && *omit_when_off {
                return None;
            }
            match var {
                ChatTemplateVar::ThinkingEnabled => Some(Value::Bool(
                    effort.is_some_and(|level| level != ThinkingLevel::Off),
                )),
                ChatTemplateVar::ThinkingBudget => {
                    budget.map(|amount| Value::Number(serde_json::Number::from(amount)))
                }
                ChatTemplateVar::ThinkingEffort => effort
                    .and_then(|level| mapped(map, level))
                    .map(|spelling| Value::String(spelling.to_owned()))
                    .or_else(|| {
                        effort.map(|level| Value::String(level_spelling(level).to_owned()))
                    }),
            }
        }
    }
}

/// `buildChatTemplateValues` (openai-completions.ts:1034-1050): resolve
/// every entry, drop the skipped ones, and only send a non-empty object.
fn build_chat_template_values(
    values: &std::collections::BTreeMap<String, ChatTemplateKwargValue>,
    map: &ThinkingLevelMap,
    effort: Option<ThinkingLevel>,
    budget: Option<u64>,
) -> Option<Map<String, Value>> {
    let mut resolved = Map::new();
    for (key, value) in values {
        if let Some(resolved_value) = chat_template_value(value, map, effort, budget) {
            resolved.insert(key.clone(), resolved_value);
        }
    }
    (!resolved.is_empty()).then_some(resolved)
}

/// `resolveClampedThinkingBudget` (openai-completions.ts:1020-1032).
fn clamped_thinking_budget(
    model: &ModelDescriptor,
    compat: &ResolvedCompletionsCompat,
    effort: Option<ThinkingLevel>,
    max_tokens_sent: Option<u64>,
    budgets: Option<&crate::config::ThinkingBudgets>,
) -> Option<u64> {
    let effort = effort.filter(|level| *level != ThinkingLevel::Off)?;
    if !model.reasoning {
        return None;
    }
    let ceiling = max_tokens_sent
        .or(Some(model.max_tokens))
        .unwrap_or_else(|| compat_max_ceiling(compat, model));
    let raw = thinking_budget_for_level(effort, budgets);
    let clamped = clamp_thinking_budget_to_answer_room(raw, ceiling);
    (clamped > 0).then_some(clamped)
}

const fn compat_max_ceiling(_compat: &ResolvedCompletionsCompat, model: &ModelDescriptor) -> u64 {
    model.max_tokens
}

/// The cache-control marker (dsh `getCompatCacheControl`): anthropic
/// format, retention not `none`, `ttl: "1h"` only for long retention on a
/// long-capable endpoint.
fn cache_control_marker(
    compat: &ResolvedCompletionsCompat,
    retention: crate::config::CacheRetention,
) -> Option<Map<String, Value>> {
    if compat.cache_control_format != Some(CacheControlFormat::Anthropic)
        || retention == crate::config::CacheRetention::None
    {
        return None;
    }
    let mut marker = Map::new();
    marker.insert("type".to_owned(), Value::String("ephemeral".to_owned()));
    if retention == crate::config::CacheRetention::Long && compat.supports_long_cache_retention {
        marker.insert("ttl".to_owned(), Value::String("1h".to_owned()));
    }
    Some(marker)
}

/// `applyAnthropicCacheControl` (openai-completions.ts:1089-1191): annotate
/// the first system/developer message's text, the last tool, and the last
/// conversation message's text part. String content upgrades to a one-part
/// array only when non-empty (dsh `addCacheControlToTextContent`).
fn apply_cache_control(
    messages: &mut [Value],
    tools: Option<&mut Vec<Value>>,
    marker: &Map<String, Value>,
) {
    // First system or developer message.
    for message in messages.iter_mut() {
        if message["role"]
            .as_str()
            .is_some_and(|role| role == "system" || role == "developer")
        {
            annotate_text(message, marker);
            break;
        }
    }
    // Last tool.
    if let Some(last) = tools.and_then(|tool_list| tool_list.last_mut()) {
        last["cache_control"] = Value::Object(marker.clone());
    }
    // Last conversation message (user, assistant, or tool) with text.
    for message in messages.iter_mut().rev() {
        let conversational = message["role"]
            .as_str()
            .is_some_and(|role| matches!(role, "user" | "assistant" | "tool"));
        if conversational && annotate_text(message, marker) {
            break;
        }
    }
}

/// `addCacheControlToTextContent`: a string upgrades to a non-empty
/// one-part array; an array annotates its last text part. Returns whether
/// the marker landed.
fn annotate_text(message: &mut Value, marker: &Map<String, Value>) -> bool {
    let Some(content) = message.get_mut("content") else {
        return false;
    };
    match content {
        Value::String(text) => {
            if text.is_empty() {
                false
            } else {
                *content = serde_json::json!([{
                    "type": "text",
                    "text": text,
                    "cache_control": marker,
                }]);
                true
            }
        }
        Value::Array(parts) => {
            for part in parts.iter_mut().rev() {
                if part["type"] == "text" {
                    part["cache_control"] = Value::Object(marker.clone());
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

/// `buildParams` in dsh field order; see the module docs for the key order
/// rationale and the dead fr faces.
///
/// # Panics
///
/// Panics when a reasoning model carries no pinned thinking-level map
/// (models.rs guarantees one; the dsh counterpart would read
/// `undefined`, so the contract is an internal invariant).
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "the dsh closure environment (model, compat, route budgets, request options) passes as explicit parameters"
)]
pub fn build_params(
    model: &ModelDescriptor,
    compat: &ResolvedCompletionsCompat,
    retention: crate::config::CacheRetention,
    messages: Vec<Value>,
    tools: Option<&[ToolSchema]>,
    has_tool_history: bool,
    route_budgets: Option<&crate::config::ThinkingBudgets>,
    options: &BuildParamsOptions<'_>,
) -> Value {
    let mut params = Map::new();
    params.insert("model".to_owned(), Value::String(model.id.clone()));
    params.insert("messages".to_owned(), Value::Array(messages));
    params.insert("stream".to_owned(), Value::Bool(true));

    // Prompt-cache key: the OpenAI endpoint keeps the default short
    // retention, and long retention rides any long-capable endpoint.
    let cache_key_wanted = (model.base_url.contains("api.openai.com")
        && retention != crate::config::CacheRetention::None)
        || (retention == crate::config::CacheRetention::Long
            && compat.supports_long_cache_retention);
    if cache_key_wanted && let Some(key) = clamp_openai_prompt_cache_key(options.session_id) {
        params.insert("prompt_cache_key".to_owned(), Value::String(key));
    }
    if retention == crate::config::CacheRetention::Long && compat.supports_long_cache_retention {
        params.insert(
            "prompt_cache_retention".to_owned(),
            Value::String("24h".to_owned()),
        );
    }

    if compat.supports_usage_in_streaming {
        params.insert(
            "stream_options".to_owned(),
            serde_json::json!({ "include_usage": true }),
        );
    }
    if compat.supports_store {
        params.insert("store".to_owned(), Value::Bool(false));
    }

    // D14: the output cap is always sent (the entry chain supplies it).
    if let Some(max_tokens) = options.max_tokens {
        let field = match compat.max_tokens_field {
            MaxTokensField::MaxTokens => "max_tokens",
            MaxTokensField::MaxCompletionTokens => "max_completion_tokens",
        };
        params.insert(
            field.to_owned(),
            Value::Number(serde_json::Number::from(max_tokens)),
        );
    }
    if let Some(temperature) = options.temperature {
        params.insert("temperature".to_owned(), Value::from(temperature));
    }

    // Tools: the request set, or an explicit empty array when history has
    // tool turns (the Anthropic-via-proxy requirement).
    let tool_list: Vec<Value> = match tools {
        Some(declarations) if !declarations.is_empty() => convert_tools(declarations, compat),
        _ => has_tool_history.then(Vec::new).unwrap_or_default(),
    };
    if !tool_list.is_empty() {
        params.insert("tools".to_owned(), Value::Array(tool_list));
    }

    // Cache-control annotation mutates the placed messages and tools in
    // place (dsh applyAnthropicCacheControl over the built params).
    if let Some(marker) = cache_control_marker(compat, retention) {
        let mut message_list = params
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let tools_mut = params.get_mut("tools").and_then(Value::as_array_mut);
        apply_cache_control(&mut message_list, tools_mut, &marker);
        params.insert("messages".to_owned(), Value::Array(message_list));
    }

    if let Some(choice) = &options.tool_choice {
        params.insert("tool_choice".to_owned(), choice.clone());
    }
    if let Some(priority) = compat.vllm_priority {
        params.insert(
            "priority".to_owned(),
            Value::Number(serde_json::Number::from(priority)),
        );
    }

    // The thinking-dispatch dialect; every arm gates on `model.reasoning`
    // and sends nothing for `off` unless the map pins a string spelling.
    let budget = clamped_thinking_budget(
        model,
        compat,
        options.effort,
        options.max_tokens,
        route_budgets,
    );
    apply_thinking_dialect(&mut params, model, compat, options.effort, budget);

    // The top-level thinking budget rides its compat field, independent of
    // the dialect (reasoning and the answer share the output cap).
    if let Some(field) = thinking_token_budget_field(compat)
        && let Some(budget) = budget
    {
        let field = match field {
            ThinkingTokenBudgetField::ThinkingTokenBudget => "thinking_token_budget",
            ThinkingTokenBudgetField::ThinkingBudget => "thinking_budget",
            ThinkingTokenBudgetField::ThinkingBudgetTokens => "thinking_budget_tokens",
        };
        params.insert(
            field.to_owned(),
            Value::Number(serde_json::Number::from(budget)),
        );
    }

    Value::Object(params)
}

/// The mapped-or-raw effort insert (dsh `map?.[effort] ?? effort`).
fn insert_effort(
    params: &mut Map<String, Value>,
    map: Option<&ThinkingLevelMap>,
    effort: Option<ThinkingLevel>,
) {
    let spelling = effort
        .and_then(|level| map.and_then(|map| mapped(map, level)))
        .unwrap_or_else(|| level_spelling(effort.expect("effort checked")));
    params.insert(
        "reasoning_effort".to_owned(),
        Value::String(spelling.to_owned()),
    );
}

/// dsh `model.thinkingLevelMap?.off !== null`: a pinned-null `off` (or the
/// off-only `SendNothing`) suppresses the disabled/no-effort arm.
fn off_not_null(map: Option<&ThinkingLevelMap>) -> bool {
    map.is_none_or(|map| {
        !matches!(
            map.get(&ThinkingLevel::Off),
            Some(LevelMapping::Unsupported | LevelMapping::SendNothing)
        )
    })
}

/// The thinking-dispatch dialect (openai-completions.ts:880-977): twelve
/// arms keyed by `thinkingFormat`, all gated on `model.reasoning`.
///
/// The single match is the dsh dispatch-table shape; splitting arms into
/// helpers would scatter the parity mapping across files.
#[expect(
    clippy::too_many_lines,
    reason = "the twelve-arm dsh dispatch table reads best as one match; each arm is a verbatim openai-completions.ts fragment"
)]
fn apply_thinking_dialect(
    params: &mut Map<String, Value>,
    model: &ModelDescriptor,
    compat: &ResolvedCompletionsCompat,
    effort: Option<ThinkingLevel>,
    budget: Option<u64>,
) {
    if !model.reasoning {
        return;
    }
    let map = model.thinking_level_map.as_ref();
    let effort_on = effort.is_some_and(|level| level != ThinkingLevel::Off);
    let pinned = || map.expect("reasoning models carry a pinned map");
    match compat.thinking_format {
        ThinkingFormat::Zai => {
            let kind = if effort_on { "enabled" } else { "disabled" };
            params.insert(
                "thinking".to_owned(),
                serde_json::json!({ "type": kind, "clear_thinking": false }),
            );
            if effort_on && compat.supports_reasoning_effort {
                insert_effort(params, map, effort);
            }
        }
        ThinkingFormat::Qwen => {
            params.insert("enable_thinking".to_owned(), Value::Bool(effort_on));
            if effort_on && compat.supports_reasoning_effort {
                insert_effort(params, map, effort);
            }
        }
        ThinkingFormat::QwenChatTemplate => {
            params.insert(
                "chat_template_kwargs".to_owned(),
                serde_json::json!({
                    "enable_thinking": effort_on,
                    "preserve_thinking": true,
                }),
            );
        }
        ThinkingFormat::ChatTemplate => {
            if let Some(values) =
                build_chat_template_values(&compat.chat_template_kwargs, pinned(), effort, budget)
            {
                params.insert("chat_template_kwargs".to_owned(), Value::Object(values));
            }
        }
        ThinkingFormat::Baseten => {
            let pinned_map = pinned();
            if let Some(values) =
                build_chat_template_values(&compat.chat_template_args, pinned_map, effort, budget)
            {
                params.insert("chat_template_args".to_owned(), Value::Object(values));
            }
            if compat.supports_reasoning_effort {
                // dsh baseten: a requested effort maps directly; the
                // no-effort arm sends the mapped `off` spelling.
                let spelling = effort.map_or_else(
                    || mapped(pinned_map, ThinkingLevel::Off),
                    |level| mapped(pinned_map, level),
                );
                if let Some(spelling) = spelling {
                    params.insert(
                        "reasoning_effort".to_owned(),
                        Value::String(spelling.to_owned()),
                    );
                }
            }
        }
        ThinkingFormat::Deepseek => {
            if effort_on {
                params.insert(
                    "thinking".to_owned(),
                    serde_json::json!({ "type": "enabled" }),
                );
            } else if off_not_null(map) {
                params.insert(
                    "thinking".to_owned(),
                    serde_json::json!({ "type": "disabled" }),
                );
            }
            if effort_on && compat.supports_reasoning_effort {
                insert_effort(params, map, effort);
            }
        }
        ThinkingFormat::Openrouter => {
            let pinned_map = pinned();
            let off_or_raw = |level: ThinkingLevel| -> &str {
                mapped(pinned_map, level).unwrap_or_else(|| level_spelling(level))
            };
            if effort_on {
                params.insert(
                    "reasoning".to_owned(),
                    serde_json::json!({ "effort": off_or_raw(effort.expect("effort_on")) }),
                );
            } else if off_not_null(Some(pinned_map)) {
                let off_value = mapped(pinned_map, ThinkingLevel::Off).unwrap_or("none");
                params.insert(
                    "reasoning".to_owned(),
                    serde_json::json!({ "effort": off_value }),
                );
            }
        }
        ThinkingFormat::AntLing => {
            if effort_on {
                let spelling = effort.and_then(|level| mapped(pinned(), level));
                if let Some(spelling) = spelling {
                    params.insert(
                        "reasoning".to_owned(),
                        serde_json::json!({ "effort": spelling }),
                    );
                }
            }
        }
        ThinkingFormat::Together => {
            params.insert(
                "reasoning".to_owned(),
                serde_json::json!({ "enabled": effort_on }),
            );
            if effort_on && compat.supports_reasoning_effort {
                insert_effort(params, map, effort);
            }
        }
        ThinkingFormat::StringThinking => {
            let pinned_map = pinned();
            let off_or_raw = |level: ThinkingLevel| -> &str {
                mapped(pinned_map, level).unwrap_or_else(|| level_spelling(level))
            };
            if effort_on {
                params.insert(
                    "thinking".to_owned(),
                    Value::String(off_or_raw(effort.expect("effort_on")).to_owned()),
                );
            } else if off_not_null(Some(pinned_map)) {
                let off_value = mapped(pinned_map, ThinkingLevel::Off).unwrap_or("none");
                params.insert("thinking".to_owned(), Value::String(off_value.to_owned()));
            }
        }
        ThinkingFormat::Openai => {
            if effort_on {
                if compat.supports_reasoning_effort {
                    insert_effort(params, map, effort);
                }
            } else if compat.supports_reasoning_effort
                && let Some(pinned_map) = map
                && let Some(off_value) = mapped(pinned_map, ThinkingLevel::Off)
            {
                // The off arm sends only a string spelling from the map.
                params.insert(
                    "reasoning_effort".to_owned(),
                    Value::String(off_value.to_owned()),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::{ScalarValue, SessionAffinityFormat};
    use crate::config::ThinkingBudgets;
    use fr_wire::RawObject;

    fn compat() -> ResolvedCompletionsCompat {
        ResolvedCompletionsCompat {
            supports_store: true,
            supports_developer_role: true,
            supports_reasoning_effort: true,
            supports_usage_in_streaming: true,
            supports_finish_reason: true,
            max_tokens_field: MaxTokensField::MaxCompletionTokens,
            requires_tool_result_name: false,
            requires_assistant_after_tool_result: false,
            requires_thinking_as_text: false,
            requires_reasoning_content_on_assistant_messages: false,
            thinking_format: ThinkingFormat::Openai,
            chat_template_kwargs: std::collections::BTreeMap::new(),
            chat_template_args: std::collections::BTreeMap::new(),
            supports_thinking_token_budget: false,
            thinking_token_budget_field: None,
            vllm_priority: None,
            supports_strict_mode: false,
            cache_control_format: None,
            supports_long_cache_retention: true,
            send_session_affinity_headers: false,
            session_affinity_format: SessionAffinityFormat::Openai,
            supports_mid_convo_system_messages: false,
            supports_mid_convo_tool_additions: false,
        }
    }

    fn pinned_map() -> ThinkingLevelMap {
        [
            (ThinkingLevel::Off, LevelMapping::SendNothing),
            (ThinkingLevel::Minimal, LevelMapping::Unsupported),
            (ThinkingLevel::Low, LevelMapping::Wire("low".to_owned())),
            (
                ThinkingLevel::Medium,
                LevelMapping::Wire("medium".to_owned()),
            ),
            (ThinkingLevel::High, LevelMapping::Wire("high".to_owned())),
            (ThinkingLevel::Xhigh, LevelMapping::Wire("high".to_owned())),
            (ThinkingLevel::Max, LevelMapping::Wire("high".to_owned())),
        ]
        .into_iter()
        .collect()
    }

    fn model(thinking_format: ThinkingFormat) -> ModelDescriptor {
        let mut profile = compat();
        profile.thinking_format = thinking_format;
        ModelDescriptor {
            id: "m".to_owned(),
            name: "M".to_owned(),
            api: crate::compat::Api::OpenaiCompletions,
            provider: "p".to_owned(),
            base_url: "https://api.openai.com/v1".to_owned(),
            input: vec![crate::models::Modality::Text],
            context_window: 128_000,
            max_tokens: 8_192,
            reasoning: true,
            thinking_level_map: Some(pinned_map()),
            compat: crate::compat::ResolvedCompat::Completions(profile),
        }
    }

    fn options(effort: Option<ThinkingLevel>) -> BuildParamsOptions<'static> {
        BuildParamsOptions {
            effort,
            temperature: None,
            max_tokens: Some(4_096),
            session_id: None,
            tool_choice: None,
        }
    }

    fn model_compat(descriptor: &ModelDescriptor) -> &ResolvedCompletionsCompat {
        let crate::compat::ResolvedCompat::Completions(profile) = &descriptor.compat else {
            panic!("completions compat expected");
        };
        profile
    }

    /// The dsh key order pin on a non-reasoning request with the `OpenAI`
    /// endpoint faces and tools; without a session id the cache-key face
    /// is skipped even when the endpoint wants it.
    #[test]
    fn pins_dsh_key_order() {
        let tools = vec![ToolSchema {
            defer_loading: None,
            name: "sh".to_owned(),
            description: "runs".to_owned(),
            parameters: RawObject::from_iter([(
                "type".to_owned(),
                Value::String("object".to_owned()),
            )]),
        }];
        let profile = compat();
        let descriptor = ModelDescriptor {
            id: "m".to_owned(),
            name: "M".to_owned(),
            api: crate::compat::Api::OpenaiCompletions,
            provider: "p".to_owned(),
            base_url: "https://api.openai.com/v1".to_owned(),
            input: vec![crate::models::Modality::Text],
            context_window: 128_000,
            max_tokens: 8_192,
            reasoning: false,
            thinking_level_map: None,
            compat: crate::compat::ResolvedCompat::Completions(profile.clone()),
        };
        let params = build_params(
            &descriptor,
            &profile,
            crate::config::CacheRetention::Short,
            vec![serde_json::json!({"role": "user", "content": "hi"})],
            Some(&tools),
            false,
            None,
            &options(None),
        );
        let keys: Vec<&str> = params
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "model",
                "messages",
                "stream",
                "stream_options",
                "store",
                "max_completion_tokens",
                "tools",
            ]
        );
        assert_eq!(params["stream"], true);
        assert_eq!(params["max_completion_tokens"], 4_096);
        assert_eq!(params["tools"][0]["type"], "function");
    }

    /// The `OpenAI` effort chain: on sends the mapped spelling, `off`
    /// sends nothing (`SendNothing` pins null), and the budget field only
    /// rides its compat gate.
    #[test]
    fn openai_effort_and_budget() {
        let descriptor = model(ThinkingFormat::Openai);
        let profile = compat();
        let on = build_params(
            &descriptor,
            &profile,
            crate::config::CacheRetention::None,
            Vec::new(),
            None,
            false,
            None,
            &options(Some(ThinkingLevel::Xhigh)),
        );
        assert_eq!(on["reasoning_effort"], "high");
        assert!(on.get("thinking_token_budget").is_none(), "field gate off");

        let budgets = ThinkingBudgets {
            minimal: 1_024,
            low: 2_048,
            medium: 8_192,
            high: 16_384,
        };
        let mut gated = profile.clone();
        gated.thinking_token_budget_field = Some(ThinkingTokenBudgetField::ThinkingTokenBudget);
        let on_budget = build_params(
            &descriptor,
            &gated,
            crate::config::CacheRetention::None,
            Vec::new(),
            None,
            false,
            Some(&budgets),
            &options(Some(ThinkingLevel::Xhigh)),
        );
        // xhigh clamps to high (16 384), then to the answer room 4 096-1 024.
        assert_eq!(on_budget["thinking_token_budget"], 3_072);
        let off = build_params(
            &descriptor,
            &profile,
            crate::config::CacheRetention::None,
            Vec::new(),
            None,
            false,
            None,
            &options(Some(ThinkingLevel::Off)),
        );
        assert!(off.get("reasoning_effort").is_none());
        assert!(off.get("thinking").is_none());
    }

    /// The dialect arms: deepseek thinking type and effort, together
    /// enabled flag, qwen-chat-template fixed pair, string-thinking on;
    /// ant-ling maps only when on; deepseek/string-thinking off with a
    /// pinned-null `off` suppress the arm, and an absent `off` falls back
    /// to "none".
    #[test]
    fn thinking_dialect_arms() {
        let cases: &[(ThinkingFormat, ThinkingLevel, &str, Value)] = &[
            (
                ThinkingFormat::StringThinking,
                ThinkingLevel::High,
                "thinking",
                serde_json::json!("high"),
            ),
            (
                ThinkingFormat::Deepseek,
                ThinkingLevel::High,
                "thinking",
                serde_json::json!({"type": "enabled"}),
            ),
            (
                ThinkingFormat::Together,
                ThinkingLevel::Off,
                "reasoning",
                serde_json::json!({"enabled": false}),
            ),
            (
                ThinkingFormat::QwenChatTemplate,
                ThinkingLevel::High,
                "chat_template_kwargs",
                serde_json::json!({"enable_thinking": true, "preserve_thinking": true}),
            ),
        ];
        for &(format, effort, key, ref expected) in cases {
            let descriptor = model(format);
            let params = build_params(
                &descriptor,
                model_compat(&descriptor),
                crate::config::CacheRetention::None,
                Vec::new(),
                None,
                false,
                None,
                &options(Some(effort)),
            );
            assert_eq!(
                params.get(key),
                Some(expected),
                "format {format:?} effort {effort:?}"
            );
        }
        // deepseek off with a pinned-null `off` (SendNothing) suppresses
        // the disabled arm entirely.
        let deepseek = model(ThinkingFormat::Deepseek);
        let off = build_params(
            &deepseek,
            model_compat(&deepseek),
            crate::config::CacheRetention::None,
            Vec::new(),
            None,
            false,
            None,
            &options(Some(ThinkingLevel::Off)),
        );
        assert!(off.get("thinking").is_none());
        // ant-ling: mapped string only when on.
        let ant = model(ThinkingFormat::AntLing);
        let on = build_params(
            &ant,
            model_compat(&ant),
            crate::config::CacheRetention::None,
            Vec::new(),
            None,
            false,
            None,
            &options(Some(ThinkingLevel::High)),
        );
        assert_eq!(on["reasoning"]["effort"], "high");
        let off = build_params(
            &ant,
            model_compat(&ant),
            crate::config::CacheRetention::None,
            Vec::new(),
            None,
            false,
            None,
            &options(Some(ThinkingLevel::Off)),
        );
        assert!(off.get("reasoning").is_none());
        // string-thinking off: a pinned-null `off` suppresses the arm.
        let st = model(ThinkingFormat::StringThinking);
        let off_none = build_params(
            &st,
            model_compat(&st),
            crate::config::CacheRetention::None,
            Vec::new(),
            None,
            false,
            None,
            &options(Some(ThinkingLevel::Off)),
        );
        assert!(off_none.get("thinking").is_none(), "SendNothing suppresses");
    }

    /// An `off` ABSENT from the map keeps the string-thinking disabled arm
    /// alive (dsh `undefined !== null`), falling back to "none".
    #[test]
    fn string_thinking_absent_off_falls_back_to_none() {
        let mut absent_off = model(ThinkingFormat::StringThinking);
        let without_off: ThinkingLevelMap = pinned_map()
            .into_iter()
            .filter(|(level, _)| *level != ThinkingLevel::Off)
            .collect();
        absent_off.thinking_level_map = Some(without_off);
        let off_fallback = build_params(
            &absent_off,
            model_compat(&absent_off),
            crate::config::CacheRetention::None,
            Vec::new(),
            None,
            false,
            None,
            &options(Some(ThinkingLevel::Off)),
        );
        assert_eq!(off_fallback["thinking"], "none");
    }

    /// The chat-template dispatch: scalars ride, enable is a bool, the
    /// effort dispatch maps, and omit-when-off drops entries only when no
    /// effort is selected at all.
    #[test]
    fn chat_template_dispatch() {
        let mut kwargs = std::collections::BTreeMap::new();
        kwargs.insert(
            "flag".to_owned(),
            ChatTemplateKwargValue::Dispatch {
                var: ChatTemplateVar::ThinkingEnabled,
                omit_when_off: true,
            },
        );
        kwargs.insert(
            "effort".to_owned(),
            ChatTemplateKwargValue::Dispatch {
                var: ChatTemplateVar::ThinkingEffort,
                omit_when_off: false,
            },
        );
        kwargs.insert(
            "lit".to_owned(),
            ChatTemplateKwargValue::Scalar(ScalarValue::Num(7.0)),
        );
        let map = pinned_map();
        let on = build_chat_template_values(&kwargs, &map, Some(ThinkingLevel::High), None)
            .expect("entries");
        assert_eq!(on["flag"], Value::Bool(true));
        assert_eq!(on["effort"], "high");
        assert_eq!(on["lit"], serde_json::json!(7.0));
        // `Off` is a selected effort, so omit-when-off keeps the entry as
        // false; only a missing effort drops it.
        let off = build_chat_template_values(&kwargs, &map, Some(ThinkingLevel::Off), None)
            .expect("entries");
        assert_eq!(off["flag"], Value::Bool(false));
        assert_eq!(off["effort"], "off");
        let no_effort = build_chat_template_values(&kwargs, &map, None, None).expect("entries");
        assert!(no_effort.get("flag").is_none());
        assert!(no_effort.get("effort").is_none());
    }

    /// The `Anthropic` cache-control annotation: first system text, last
    /// tool, and the last conversation message (string content upgrades to
    /// the one-part annotated array), with the long-retention ttl.
    #[test]
    fn anthropic_cache_control_annotates() {
        let mut profile = compat();
        profile.cache_control_format = Some(CacheControlFormat::Anthropic);
        let descriptor = ModelDescriptor {
            id: "m".to_owned(),
            name: "M".to_owned(),
            api: crate::compat::Api::OpenaiCompletions,
            provider: "p".to_owned(),
            base_url: "https://relay.example/v1".to_owned(),
            input: vec![crate::models::Modality::Text],
            context_window: 128_000,
            max_tokens: 8_192,
            reasoning: false,
            thinking_level_map: None,
            compat: crate::compat::ResolvedCompat::Completions(profile.clone()),
        };
        let tools = vec![ToolSchema {
            defer_loading: None,
            name: "sh".to_owned(),
            description: "runs".to_owned(),
            parameters: RawObject::from_iter([(
                "type".to_owned(),
                Value::String("object".to_owned()),
            )]),
        }];
        let messages = vec![
            serde_json::json!({"role": "system", "content": "be nice"}),
            serde_json::json!({"role": "user", "content": "run"}),
        ];
        let params = build_params(
            &descriptor,
            &profile,
            crate::config::CacheRetention::Long,
            messages,
            Some(&tools),
            false,
            None,
            &options(None),
        );
        let system = &params["messages"][0]["content"][0];
        assert_eq!(system["cache_control"]["type"], "ephemeral");
        assert_eq!(system["cache_control"]["ttl"], "1h");
        assert_eq!(params["tools"][0]["cache_control"]["type"], "ephemeral");
        let last = &params["messages"][1];
        assert_eq!(last["content"][0]["text"], "run");
        assert_eq!(last["content"][0]["cache_control"]["type"], "ephemeral");
    }

    /// The budget clamp: at least `MIN_ANSWER_TOKENS` stays for the answer
    /// under a shared ceiling.
    #[test]
    fn budget_clamps_to_answer_room() {
        assert_eq!(clamp_thinking_budget_to_answer_room(16_384, 4_096), 3_072);
        assert_eq!(clamp_thinking_budget_to_answer_room(500, 4_096), 500);
        assert_eq!(clamp_thinking_budget_to_answer_room(16_384, 1_024), 0);
    }
}
