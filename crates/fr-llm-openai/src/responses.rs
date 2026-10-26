//! Responses wire conversion and request params (pi-ai
//! `api/openai-responses.ts` buildParams + `api/openai-responses-shared.ts`
//! `convertResponsesMessages`/`convertResponsesTools`).
//!
//! The leading instruction item rides [`build_params_responses`] (dsh
//! emits it from `convertResponsesMessages`; fr folds the system text
//! upstream in [`crate::transcript`]). The non-leading system arms are
//! dead: the Q74-B baseline fold collapses every system message into the
//! one leading prompt and `Turn` has no system variant, so
//! `appendSystemToolAdditions`/`renderSystemMessageUpdate` have no input.
//! Grammar-tool faces (`custom_tool_call`, the `ctc_` prefix,
//! `namespace`, `defer_loading`, the `additional_tools` /
//! `tool_search_call` / `tool_search_output` items) are dead: D8 withholds
//! grammar support and the Q74-B withheld keys pin
//! `supports-additional-tools`/`supports-tool-search` to `false`.
//! `service_tier` and the sampling-params merge are dead too: fr has no
//! `service-tier` config face, and the D10b route-profile key set carries
//! no `sampling-params`.

use fr_wire::call_config::ToolSchema;
use fr_wire::message::{KnownMessageSource, MessageSource};
use fr_wire::{ContentBlock, KnownContentBlock};
use serde_json::{Map, Value};

use crate::compat::ResolvedResponsesCompat;
use crate::config::CacheRetention;
use crate::convert::{ImageResolver, image_data_url, sanitize_surrogates, tool_arguments};
use crate::history_transform::{RouteIdentity, Turn, is_same_model};
use crate::models::{LevelMapping, ModelDescriptor, ThinkingLevel};
use crate::params::level_spelling;
use crate::prompt_cache::clamp_openai_prompt_cache_key;

/// `OpenAI` Responses rejects `max_output_tokens` below this floor
/// (openai-responses.ts:32-33).
pub(crate) const OPENAI_RESPONSES_MIN_OUTPUT_TOKENS: u64 = 16;

/// The default `ChatGPT` sign-in endpoint [`is_chat_gpt_sign_in`] pins.
const OPENAI_DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// One parsed text replay signature (`parseTextSignature`,
/// responses-shared.ts:59-77): the v1 JSON envelope or a legacy plain id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextSignature {
    /// The provider message id.
    pub id: String,
    /// The v1 phase tag, present only for the two known spellings.
    pub phase: Option<String>,
}

/// Encode a v1 text signature (`encodeTextSignatureV1`): key order
/// `v`, `id`, `phase?`.
#[must_use]
pub(crate) fn encode_text_signature_v1(id: &str, phase: Option<&str>) -> String {
    let mut payload = Map::new();
    payload.insert("v".to_owned(), Value::from(1));
    payload.insert("id".to_owned(), Value::from(id));
    if let Some(phase) = phase {
        payload.insert("phase".to_owned(), Value::from(phase));
    }
    Value::Object(payload).to_string()
}

/// Parse a text replay signature: a `{`-leading string parses as the v1
/// JSON (`v == 1`, string `id`, phase filtered to the two known
/// spellings); anything else — unparsable, non-v1, or a plain string —
/// degrades to the legacy `{id: <raw>}` form.
#[must_use]
pub(crate) fn parse_text_signature(signature: Option<&str>) -> Option<TextSignature> {
    let signature = signature?;
    if signature.starts_with('{')
        && let Ok(parsed) = serde_json::from_str::<Value>(signature)
    {
        let version = parsed.get("v").and_then(Value::as_f64);
        let id = parsed.get("id").and_then(Value::as_str);
        if version == Some(1.0)
            && let Some(id) = id
        {
            let phase = parsed
                .get("phase")
                .and_then(Value::as_str)
                .filter(|phase| matches!(*phase, "commentary" | "final_answer"))
                .map(ToOwned::to_owned);
            return Some(TextSignature {
                id: id.to_owned(),
                phase,
            });
        }
    }
    Some(TextSignature {
        id: signature.to_owned(),
        phase: None,
    })
}

/// Convert the request tool set (`convertResponsesTools` reduced to its
/// live face): the strict field is the batch-2 dead-input finding (fr has
/// no constrained-sampling config, so `resolveJsonSchemaStrictSampling` is
/// permanently `undefined` and the withheld `supports-strict-mode` default
/// `false` omits the field), and grammar tools are D8-dead.
pub(crate) fn convert_responses_tools(tools: &[ToolSchema]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": Value::Object(tool.parameters.clone()),
            })
        })
        .collect()
}

/// The conversion seams for one request.
pub struct ResponsesConvertContext<'a> {
    /// The route identity.
    pub route: RouteIdentity<'a>,
    /// Whether the model reasons (`model.reasoning` gate).
    pub reasoning: bool,
    /// The resolved `supports-developer-role` switch (`instructionRole`).
    pub supports_developer_role: bool,
    /// Whether the model accepts image input; the transform layer already
    /// downgraded the first gate, this mirrors the provider-side recheck in
    /// `convertToolResultOutput`.
    pub supports_images: bool,
    /// Attachment bytes resolution; a `None` drop skips the image part
    /// (a degenerate attachment-service failure, not a normal path).
    pub images: ImageResolver<'a>,
}

impl std::fmt::Debug for ResponsesConvertContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponsesConvertContext")
            .field("route", &self.route)
            .field("reasoning", &self.reasoning)
            .field("supports_developer_role", &self.supports_developer_role)
            .field("supports_images", &self.supports_images)
            .field("images", &"<closure>")
            .finish()
    }
}

/// The replay signature of one block, read positionally from the D12
/// envelope (kind `fr-openai`, model gate, `blocks[index]` with the
/// expected type). Same-model turns keep their block order across the
/// transform, so the envelope position is the content position; cross-model
/// turns never carry signatures to read.
fn envelope_block_signature<'a>(
    source: &'a MessageSource,
    route_model: &str,
    index: usize,
    block_type: &str,
) -> Option<&'a str> {
    let MessageSource::Known(KnownMessageSource::Model {
        replay_state: Some(replay),
        ..
    }) = source
    else {
        return None;
    };
    let response = replay.get("response")?;
    if response.get("kind")?.as_str()? != "fr-openai" {
        return None;
    }
    if response.get("model")?.as_str()? != route_model {
        return None;
    }
    let block = replay.get("blocks")?.as_array()?.get(index)?;
    if block.get("type")?.as_str()? != block_type {
        return None;
    }
    block.get("signature")?.as_str()
}

/// The user content parts (`input_text` / `input_image`).
fn user_parts(content: &[ContentBlock], images: ImageResolver<'_>) -> Vec<Value> {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Known(KnownContentBlock::Text { text }) => {
                Some(serde_json::json!({ "type": "input_text", "text": sanitize_surrogates(text) }))
            }
            ContentBlock::Known(KnownContentBlock::Image { attachment, .. }) => images(attachment)
                .map(|(media_type, data)| {
                    serde_json::json!({
                        "type": "input_image",
                        "detail": "auto",
                        "image_url": image_data_url(&media_type, &data),
                    })
                }),
            // dsh user content is text|image only; fr file references
            // project to handle text before request assembly.
            _ => None,
        })
        .collect()
}

/// One tool result's output (`convertToolResultOutput`): without images
/// (or on a non-vision model) a joined text string with the two
/// placeholder spellings; with images an `input_text` / `input_image`
/// array.
fn tool_result_output(content: &[ContentBlock], ctx: &ResponsesConvertContext<'_>) -> Value {
    let text = content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Known(KnownContentBlock::Text { text }) => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let images: Vec<_> = content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Known(KnownContentBlock::Image { attachment, .. }) => Some(attachment),
            _ => None,
        })
        .collect();
    let has_text = !text.is_empty();
    if images.is_empty() || !ctx.supports_images {
        let chosen = if has_text {
            text
        } else if images.is_empty() {
            "(no tool output)".to_owned()
        } else {
            "(see attached image)".to_owned()
        };
        return Value::String(sanitize_surrogates(&chosen).to_owned());
    }
    let mut parts = Vec::new();
    if has_text {
        parts.push(serde_json::json!({
            "type": "input_text",
            "text": sanitize_surrogates(&text),
        }));
    }
    for attachment in images {
        if let Some((media_type, data)) = (ctx.images)(attachment) {
            parts.push(serde_json::json!({
                "type": "input_image",
                "detail": "auto",
                "image_url": image_data_url(&media_type, &data),
            }));
        }
    }
    Value::Array(parts)
}

/// Convert the transformed history to Responses input items
/// (`convertResponsesMessages` minus the leading-system arm; see the
/// module doc). `msgIndex` counts every emitted message for the
/// `msg_pi_*` fallback ids, and a skipped empty user turn does not
/// consume a number (the dsh `continue` precedes the loop-end increment).
pub(crate) fn convert_responses_messages(
    turns: &[Turn],
    ctx: &ResponsesConvertContext<'_>,
) -> Vec<Value> {
    let mut messages: Vec<Value> = Vec::new();
    let mut msg_index = 0usize;
    for turn in turns {
        match turn {
            Turn::User { content } => {
                let parts = user_parts(content, ctx.images);
                if parts.is_empty() {
                    continue;
                }
                messages.push(serde_json::json!({
                    "role": "user",
                    "content": Value::Array(parts),
                }));
                msg_index += 1;
            }
            Turn::Assistant { source, content } => {
                if let Some(items) = assistant_items(source, content, msg_index, ctx) {
                    messages.extend(items);
                    msg_index += 1;
                }
            }
            Turn::Tool {
                call_id, content, ..
            } => {
                // JS destructuring takes the first pipe segment.
                let call = call_id.as_str().split('|').next().unwrap_or_default();
                messages.push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": call,
                    "output": tool_result_output(content, ctx),
                }));
                msg_index += 1;
            }
        }
    }
    messages
}

/// One assistant turn's input items (`ResponseInput` entries).
fn assistant_items(
    source: &MessageSource,
    content: &[ContentBlock],
    msg_index: usize,
    ctx: &ResponsesConvertContext<'_>,
) -> Option<Vec<Value>> {
    let same_provider = matches!(
        source,
        MessageSource::Known(KnownMessageSource::Model { provider, .. })
            if provider == ctx.route.provider
    );
    let same_model = is_same_model(source, &ctx.route);
    let different_model = same_provider && !same_model;
    let mut output: Vec<Value> = Vec::new();
    let mut text_block_index = 0usize;
    for (position, block) in content.iter().enumerate() {
        match block {
            ContentBlock::Known(KnownContentBlock::Reasoning { .. }) => {
                // dsh pushes the parsed `thinkingSignature` item verbatim
                // and skips the block without one. fr signatures are
                // fr-serialized envelope JSON, so the unguarded dsh
                // `JSON.parse` throw is unreachable; a mismatch degrades
                // to the skip.
                if let Some(item) =
                    envelope_block_signature(source, ctx.route.model, position, "reasoning")
                    && let Ok(item) = serde_json::from_str::<Value>(item)
                {
                    output.push(item);
                }
            }
            ContentBlock::Known(KnownContentBlock::Text { text }) => {
                let parsed = parse_text_signature(envelope_block_signature(
                    source,
                    ctx.route.model,
                    position,
                    "text",
                ));
                let msg_id = match &parsed {
                    Some(signature) if !signature.id.is_empty() => {
                        // OpenAI requires ids of at most 64 characters.
                        if signature.id.chars().count() > 64 {
                            format!("msg_{}", crate::hash::short_hash(&signature.id))
                        } else {
                            signature.id.clone()
                        }
                    }
                    _ => {
                        if text_block_index == 0 {
                            format!("msg_pi_{msg_index}")
                        } else {
                            format!("msg_pi_{msg_index}_{text_block_index}")
                        }
                    }
                };
                text_block_index += 1;
                let mut item = Map::new();
                item.insert("type".to_owned(), Value::from("message"));
                item.insert("role".to_owned(), Value::from("assistant"));
                item.insert(
                    "content".to_owned(),
                    Value::Array(vec![serde_json::json!({
                        "type": "output_text",
                        "text": sanitize_surrogates(text),
                        "annotations": [],
                    })]),
                );
                item.insert("status".to_owned(), Value::from("completed"));
                item.insert("id".to_owned(), Value::from(msg_id));
                if let Some(phase) = parsed
                    .as_ref()
                    .and_then(|signature| signature.phase.as_deref())
                {
                    item.insert("phase".to_owned(), Value::from(phase));
                }
                output.push(Value::Object(item));
            }
            ContentBlock::Known(KnownContentBlock::ToolCall {
                id,
                name,
                arguments,
            }) => {
                let mut parts = id.as_str().split('|');
                let call_id = parts.next().unwrap_or_default().to_owned();
                let item_raw = parts.next();
                // The grammar `ctc_` prefix is D8-dead; the prefix gate is
                // always `fc_`.
                let mut item_id = item_raw;
                if different_model || !item_id.is_some_and(|value| value.starts_with("fc_")) {
                    item_id = None;
                }
                let mut item = Map::new();
                item.insert("type".to_owned(), Value::from("function_call"));
                if let Some(value) = item_id {
                    item.insert("id".to_owned(), Value::from(value));
                }
                item.insert("call_id".to_owned(), Value::from(call_id));
                item.insert("name".to_owned(), Value::from(name.as_str()));
                item.insert(
                    "arguments".to_owned(),
                    // dsh stringifies the parsed arguments object; the
                    // durable fr raw string parses to that object (the
                    // same spelling as the Completions converter).
                    Value::String(tool_arguments(arguments.as_str()).to_string()),
                );
                output.push(Value::Object(item));
            }
            // fr assistant content carries no image or file blocks.
            _ => {}
        }
    }
    (!output.is_empty()).then_some(output)
}

/// The `ChatGPT` sign-in predicate (`isChatGPTSignIn`).
///
/// An `openai`-route key on the default endpoint whose credential is not
/// an `sk-` API key is a Sign in with `ChatGPT` access token; those
/// requests reject the optional request fields.
#[must_use]
pub fn is_chat_gpt_sign_in(provider: &str, base_url: &str, api_key: Option<&str>) -> bool {
    provider == "openai"
        && base_url == OPENAI_DEFAULT_BASE_URL
        && api_key.is_some_and(|key| !key.starts_with("sk-"))
}

/// The request options [`build_params_responses`] consumes.
#[derive(Debug)]
pub struct ResponsesParamsOptions<'a> {
    /// The reasoning effort for this request; `None` covers `off` and
    /// non-reasoning callers (dsh `streamSimple` maps `off` to
    /// `undefined`).
    pub effort: Option<ThinkingLevel>,
    /// Sampling temperature.
    pub temperature: Option<f64>,
    /// The output cap (D14: the entry chain always supplies it).
    pub max_tokens: Option<u64>,
    /// The session id feeding the prompt-cache key.
    pub session_id: Option<&'a str>,
    /// The caller tool-choice face.
    pub tool_choice: Option<Value>,
    /// Whether `ChatGPT` sign-in credentials forbid the optional fields;
    /// the caller evaluates [`is_chat_gpt_sign_in`] over the request key
    /// (D10: credentials are adapter face).
    pub omit_unsupported_fields: bool,
}

/// The dsh `model.thinkingLevelMap?.off ?? "none"` fallback over the fr
/// level mapping: a declared `off` spelling passes through, an undeclared
/// one falls back to `"none"`, and the `null` mapping (`SendNothing`)
/// suppresses the whole arm.
fn off_wire_effort(model: &ModelDescriptor) -> Option<String> {
    match model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.get(&ThinkingLevel::Off))
    {
        Some(LevelMapping::Wire(spelling)) => Some((*spelling).clone()),
        Some(LevelMapping::Unsupported) | None => Some("none".to_owned()),
        Some(LevelMapping::SendNothing) => None,
    }
}

/// Build the Responses create-params object (`buildParams`,
/// openai-responses.ts:302-389) in dsh insertion order.
///
/// Differences from the Completions face are load-bearing: the
/// prompt-cache key has no `api.openai.com` gate, `store: false` is
/// unconditional (Completions gates it on `supports-store`), the output
/// cap is `max_output_tokens` with a floor of 16, and there is no
/// empty-tools arm (the key only appears with a non-empty set).
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "the dsh buildParams closure environment (model, compat, retention, system, turns, context, tools, options) passes as explicit parameters"
)]
pub fn build_params_responses(
    model: &ModelDescriptor,
    compat: &ResolvedResponsesCompat,
    retention: CacheRetention,
    system: Option<&str>,
    turns: &[Turn],
    ctx: &ResponsesConvertContext<'_>,
    tools: Option<&[ToolSchema]>,
    options: &ResponsesParamsOptions<'_>,
) -> Value {
    let omit = options.omit_unsupported_fields;
    let mut items = convert_responses_messages(turns, ctx);
    if let Some(text) = system.filter(|text| !text.is_empty()) {
        let role = if ctx.reasoning && ctx.supports_developer_role {
            "developer"
        } else {
            "system"
        };
        items.insert(
            0,
            serde_json::json!({ "role": role, "content": sanitize_surrogates(text) }),
        );
    }

    let mut params = Map::new();
    params.insert("model".to_owned(), Value::from(model.id.as_str()));
    params.insert("input".to_owned(), Value::Array(items));
    params.insert("stream".to_owned(), Value::Bool(true));
    // Unlike Completions, the key rides every endpoint (no
    // `api.openai.com` gate); only `retention: none` suppresses it.
    if retention != CacheRetention::None
        && let Some(key) = clamp_openai_prompt_cache_key(options.session_id)
    {
        params.insert("prompt_cache_key".to_owned(), Value::from(key));
    }
    if !omit
        && retention == CacheRetention::Long
        && compat.supports_long_cache_retention
        && !compat.supports_explicit_prompt_cache_mode
    {
        params.insert("prompt_cache_retention".to_owned(), Value::from("24h"));
    }
    // `prompt_cache_options` is dead: `supports-explicit-prompt-cache-mode`
    // is withheld (always `false`), so neither `{mode: "explicit"}` nor
    // `{ttl: "30m"}` is ever emitted.
    params.insert("store".to_owned(), Value::Bool(false));
    if let Some(max_tokens) = options
        .max_tokens
        .filter(|tokens| *tokens != 0)
        .filter(|_| compat.supports_max_output_tokens)
        .filter(|_| !omit)
    {
        params.insert(
            "max_output_tokens".to_owned(),
            Value::from(max_tokens.max(OPENAI_RESPONSES_MIN_OUTPUT_TOKENS)),
        );
    }
    if !omit && let Some(temperature) = options.temperature {
        params.insert("temperature".to_owned(), Value::from(temperature));
    }
    // `service_tier` is dead: fr has no config face for it.
    if let Some(tools) = tools.filter(|tools| !tools.is_empty()) {
        params.insert(
            "tools".to_owned(),
            Value::Array(convert_responses_tools(tools)),
        );
    }
    if let Some(choice) = &options.tool_choice {
        params.insert("tool_choice".to_owned(), choice.clone());
    }
    if model.reasoning {
        match options.effort {
            Some(level) => {
                let effort = model
                    .thinking_level_map
                    .as_ref()
                    .and_then(|map| map.get(&level))
                    .map_or_else(
                        || level_spelling(level).to_owned(),
                        |mapping| match mapping {
                            LevelMapping::Wire(spelling) => (*spelling).clone(),
                            // dsh `thinkingLevelMap?.[effort] ?? effort`:
                            // only `off` can map to a non-string, and the
                            // effort arm never receives `off`.
                            LevelMapping::Unsupported | LevelMapping::SendNothing => {
                                level_spelling(level).to_owned()
                            }
                        },
                    );
                let mut reasoning = Map::new();
                reasoning.insert("effort".to_owned(), Value::from(effort));
                // dsh `options?.reasoningSummary || "auto"`; fr has no
                // reasoning-summary face, so this is always `"auto"`.
                reasoning.insert("summary".to_owned(), Value::from("auto"));
                params.insert("reasoning".to_owned(), Value::Object(reasoning));
                params.insert(
                    "include".to_owned(),
                    serde_json::json!(["reasoning.encrypted_content"]),
                );
            }
            // dsh `model.provider !== "github-copilot"`: the exclusion
            // arm stays (the fr provider dict is caller-named, so the
            // condition is live), while the copilot dynamic-header
            // builder itself is D9-dead.
            None if model.provider != "github-copilot" => {
                if let Some(effort) = off_wire_effort(model) {
                    let mut reasoning = Map::new();
                    reasoning.insert("effort".to_owned(), Value::from(effort));
                    params.insert("reasoning".to_owned(), Value::Object(reasoning));
                }
            }
            None => {}
        }
        if model.provider == "xai" {
            params.insert(
                "include".to_owned(),
                serde_json::json!(["reasoning.encrypted_content"]),
            );
        }
    }
    // The sampling-params merge is dead: the D10b route-profile key set
    // carries no `sampling-params`, so `resolveSamplingParams` has no
    // input and the params object is final.
    Value::Object(params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history_transform::normalize_tool_call_id_responses;
    use crate::models::Modality;
    use fr_wire::ids::ToolCallId;
    use fr_wire::{AttachmentId, ImageAttachmentRef, ImageMediaType, RawObject};

    fn responses_compat() -> ResolvedResponsesCompat {
        ResolvedResponsesCompat {
            supports_developer_role: true,
            supports_mid_convo_system_messages: false,
            session_affinity_format: crate::compat::SessionAffinityFormat::Openai,
            supports_long_cache_retention: true,
            supports_strict_mode: false,
            supports_openai_grammar_tools: false,
            supports_additional_tools: false,
            supports_tool_search: false,
            supports_explicit_prompt_cache_mode: false,
            supports_max_output_tokens: true,
        }
    }

    fn model(provider: &str, map: Option<crate::models::ThinkingLevelMap>) -> ModelDescriptor {
        ModelDescriptor {
            id: "m".to_owned(),
            name: "M".to_owned(),
            api: crate::compat::Api::OpenaiResponses,
            provider: provider.to_owned(),
            base_url: "https://relay.example/v1".to_owned(),
            input: vec![Modality::Text],
            context_window: 128_000,
            max_tokens: 8_192,
            reasoning: true,
            thinking_level_map: map,
            compat: crate::compat::ResolvedCompat::Responses(responses_compat()),
        }
    }

    fn pinned_map() -> crate::models::ThinkingLevelMap {
        let mut map = crate::models::ThinkingLevelMap::new();
        map.insert(ThinkingLevel::Off, LevelMapping::SendNothing);
        map.insert(
            ThinkingLevel::Minimal,
            LevelMapping::Wire("minimal".to_owned()),
        );
        map.insert(ThinkingLevel::Low, LevelMapping::Wire("low".to_owned()));
        map.insert(
            ThinkingLevel::Medium,
            LevelMapping::Wire("medium".to_owned()),
        );
        map.insert(ThinkingLevel::High, LevelMapping::Wire("high".to_owned()));
        map.insert(ThinkingLevel::Xhigh, LevelMapping::Unsupported);
        map.insert(ThinkingLevel::Max, LevelMapping::Unsupported);
        map
    }

    fn ctx(model: &ModelDescriptor) -> ResponsesConvertContext<'_> {
        ResponsesConvertContext {
            route: RouteIdentity {
                provider: &model.provider,
                model: &model.id,
            },
            reasoning: model.reasoning,
            supports_developer_role: true,
            supports_images: false,
            images: &|_| None,
        }
    }

    fn options(
        effort: Option<ThinkingLevel>,
        session_id: Option<&str>,
        omit: bool,
    ) -> ResponsesParamsOptions<'_> {
        ResponsesParamsOptions {
            effort,
            temperature: Some(0.4),
            max_tokens: Some(4_096),
            session_id,
            tool_choice: None,
            omit_unsupported_fields: omit,
        }
    }

    fn model_source(provider: &str, model: &str, replay: Option<Value>) -> MessageSource {
        MessageSource::Known(KnownMessageSource::Model {
            provider: provider.to_owned(),
            model: model.to_owned(),
            replay_state: replay,
            extra: RawObject::new(),
        })
    }

    fn text_block(text: &str) -> ContentBlock {
        ContentBlock::Known(KnownContentBlock::Text {
            text: text.to_owned(),
        })
    }

    fn call(id: &str) -> ContentBlock {
        ContentBlock::Known(KnownContentBlock::ToolCall {
            id: ToolCallId::new(id),
            name: "grep".to_owned(),
            arguments: r#"{"q":"x"}"#.to_owned(),
        })
    }

    fn image_reference() -> ImageAttachmentRef {
        ImageAttachmentRef {
            attachment_id: AttachmentId::new("att"),
            media_type: ImageMediaType::Png,
            bytes: 3,
            width: 1,
            height: 1,
            name: None,
            original_dimensions: None,
        }
    }

    fn empty_turns() -> Vec<Turn> {
        Vec::new()
    }

    /// The dsh insertion order is load-bearing: model, input, stream,
    /// prompt cache faces, store, the output cap, temperature, tools, and
    /// the reasoning block. The cache key has no `api.openai.com` gate and
    /// the cap floors at 16.
    #[test]
    fn build_params_pins_key_order_and_floors_output_tokens() {
        let descriptor = model("p", Some(pinned_map()));
        let context = ctx(&descriptor);
        let value = build_params_responses(
            &descriptor,
            &responses_compat(),
            CacheRetention::Long,
            Some("be helpful"),
            &empty_turns(),
            &context,
            None,
            &options(Some(ThinkingLevel::Medium), Some("session-1"), false),
        );
        let object = value.as_object().expect("object");
        let keys: Vec<&String> = object.keys().collect();
        assert_eq!(
            keys,
            [
                "model",
                "input",
                "stream",
                "prompt_cache_key",
                "prompt_cache_retention",
                "store",
                "max_output_tokens",
                "temperature",
                "reasoning",
                "include",
            ]
        );
        assert_eq!(object["prompt_cache_key"], "session-1");
        assert_eq!(object["prompt_cache_retention"], "24h");
        assert_eq!(object["store"], false);
        // A cap below the provider floor clamps up to 16.
        let small = ResponsesParamsOptions {
            max_tokens: Some(4),
            ..options(Some(ThinkingLevel::Medium), None, false)
        };
        let small_value = build_params_responses(
            &descriptor,
            &responses_compat(),
            CacheRetention::None,
            None,
            &empty_turns(),
            &context,
            None,
            &small,
        );
        assert_eq!(small_value["max_output_tokens"], 16);
        assert!(small_value.get("prompt_cache_key").is_none());
        assert_eq!(
            value["reasoning"],
            serde_json::json!({"effort": "medium", "summary": "auto"})
        );
        assert_eq!(
            value["include"],
            serde_json::json!(["reasoning.encrypted_content"])
        );
        // The leading instruction item rides first with the developer
        // role (reasoning model, developer accepted).
        assert_eq!(
            value["input"][0],
            serde_json::json!({"role": "developer", "content": "be helpful"})
        );
    }

    /// `ChatGPT` sign-in credentials drop the optional fields while `store`
    /// and the cache key stay; the predicate pins the four arms.
    #[test]
    fn chat_gpt_sign_in_omits_optional_fields() {
        assert!(is_chat_gpt_sign_in(
            "openai",
            "https://api.openai.com/v1",
            Some("token")
        ));
        assert!(!is_chat_gpt_sign_in(
            "openai",
            "https://api.openai.com/v1",
            Some("sk-key")
        ));
        assert!(!is_chat_gpt_sign_in(
            "openai",
            "https://relay.example/v1",
            Some("token")
        ));
        assert!(!is_chat_gpt_sign_in(
            "openai",
            "https://api.openai.com/v1",
            None
        ));

        let descriptor = model("openai", None);
        let context = ctx(&descriptor);
        let value = build_params_responses(
            &descriptor,
            &responses_compat(),
            CacheRetention::Long,
            None,
            &empty_turns(),
            &context,
            None,
            &options(None, Some("s"), true),
        );
        let object = value.as_object().expect("object");
        assert_eq!(object["store"], false);
        assert_eq!(object["prompt_cache_key"], "s");
        assert!(object.get("prompt_cache_retention").is_none());
        assert!(object.get("max_output_tokens").is_none());
        assert!(object.get("temperature").is_none());
    }

    /// The reasoning arms: an effort passes the mapped spelling with the
    /// `auto` summary and the encrypted-content include, an unmapped
    /// effort falls back to the raw spelling, the off arm emits `none` for
    /// an undeclared off spelling and skips for `SendNothing`, `xai`
    /// always includes encrypted content, and a non-reasoning model sends
    /// no reasoning key at all.
    #[test]
    fn reasoning_arms_map_levels_and_gates() {
        let descriptor = model("p", Some(pinned_map()));
        let context = ctx(&descriptor);
        let value = build_params_responses(
            &descriptor,
            &responses_compat(),
            CacheRetention::None,
            None,
            &empty_turns(),
            &context,
            None,
            &options(Some(ThinkingLevel::Xhigh), None, false),
        );
        assert_eq!(value["reasoning"]["effort"], "xhigh");

        let mut map = pinned_map();
        map.remove(&ThinkingLevel::Off);
        let descriptor = model("p", Some(map));
        let value = build_params_responses(
            &descriptor,
            &responses_compat(),
            CacheRetention::None,
            None,
            &empty_turns(),
            &context,
            None,
            &options(None, None, false),
        );
        assert_eq!(value["reasoning"], serde_json::json!({"effort": "none"}));

        let mut map = pinned_map();
        map.insert(ThinkingLevel::Off, LevelMapping::SendNothing);
        let descriptor = model("p", Some(map));
        let value = build_params_responses(
            &descriptor,
            &responses_compat(),
            CacheRetention::None,
            None,
            &empty_turns(),
            &context,
            None,
            &options(None, None, false),
        );
        assert!(value.get("reasoning").is_none());

        let mut map = pinned_map();
        map.insert(ThinkingLevel::Off, LevelMapping::Wire("low".to_owned()));
        let descriptor = model("p", Some(map));
        let value = build_params_responses(
            &descriptor,
            &responses_compat(),
            CacheRetention::None,
            None,
            &empty_turns(),
            &context,
            None,
            &options(None, None, false),
        );
        assert_eq!(value["reasoning"], serde_json::json!({"effort": "low"}));

        let descriptor = model("xai", Some(pinned_map()));
        let value = build_params_responses(
            &descriptor,
            &responses_compat(),
            CacheRetention::None,
            None,
            &empty_turns(),
            &ctx(&descriptor),
            None,
            &options(None, None, false),
        );
        assert!(value.get("reasoning").is_none());
        assert_eq!(
            value["include"],
            serde_json::json!(["reasoning.encrypted_content"])
        );

        let mut descriptor = model("p", Some(pinned_map()));
        descriptor.reasoning = false;
        let value = build_params_responses(
            &descriptor,
            &responses_compat(),
            CacheRetention::None,
            None,
            &empty_turns(),
            &ctx(&descriptor),
            None,
            &options(Some(ThinkingLevel::High), None, false),
        );
        assert!(value.get("reasoning").is_none());
        assert!(value.get("include").is_none());

        let descriptor = model("github-copilot", Some(pinned_map()));
        let value = build_params_responses(
            &descriptor,
            &responses_compat(),
            CacheRetention::None,
            None,
            &empty_turns(),
            &ctx(&descriptor),
            None,
            &options(None, None, false),
        );
        assert!(value.get("reasoning").is_none());
    }

    /// History conversion pins the item shapes: the fallback message id,
    /// the replayed reasoning item, the pipe-id tool call, and the tool
    /// output string form.
    #[test]
    fn converts_history_to_input_items() {
        let descriptor = model("p", None);
        let replay = serde_json::json!({
            "response": {"kind": "fr-openai", "model": "m"},
            "blocks": [
                {"type": "reasoning", "signature": r#"{"id":"rs_1","summary":[]}"#},
                {"type": "text", "signature": r#"{"v":1,"id":"msg_9","phase":"final_answer"}"#},
            ],
        });
        let source = model_source("p", "m", Some(replay));
        let turns = vec![
            Turn::User {
                content: vec![text_block("hi")],
            },
            Turn::Assistant {
                source,
                content: vec![
                    ContentBlock::Known(KnownContentBlock::Reasoning {
                        text: "think".to_owned(),
                    }),
                    text_block("answer"),
                    call("fc_item1|fc_item1"),
                ],
            },
            Turn::Tool {
                call_id: ToolCallId::new("fc_item1|fc_item1"),
                is_error: None,
                content: vec![text_block("done")],
            },
        ];
        let items = convert_responses_messages(&turns, &ctx(&descriptor));
        assert_eq!(
            items[0],
            serde_json::json!({"role": "user", "content": [{"type": "input_text", "text": "hi"}]})
        );
        let reasoning_item: Value =
            serde_json::from_str(r#"{"id":"rs_1","summary":[]}"#).expect("item");
        assert_eq!(items[1], reasoning_item);
        assert_eq!(
            items[2]["id"], "msg_9",
            "the v1 signature id wins over the fallback"
        );
        assert_eq!(items[2]["phase"], "final_answer");
        assert_eq!(items[2]["status"], "completed");
        assert_eq!(
            items[2]["content"],
            serde_json::json!([{"type": "output_text", "text": "answer", "annotations": []}])
        );
        assert_eq!(items[3]["type"], "function_call");
        assert_eq!(items[3]["id"], "fc_item1");
        assert_eq!(items[3]["call_id"], "fc_item1");
        assert_eq!(items[3]["arguments"], r#"{"q":"x"}"#);
        assert_eq!(items[4]["type"], "function_call_output");
        assert_eq!(items[4]["call_id"], "fc_item1");
        assert_eq!(items[4]["output"], "done");
    }

    /// Cross-model tool calls drop the item id, ids outside the `fc_`
    /// shape drop it too, signature-less text falls back to `msg_pi_*`,
    /// and an over-long signature id hash-shortens.
    #[test]
    fn cross_model_and_foreign_ids_drop_or_repair() {
        let descriptor = model("p", None);
        let long_id = "msg_".to_owned() + &"x".repeat(70);
        let replay = serde_json::json!({
            "response": {"kind": "fr-openai", "model": "m"},
            "blocks": [
                {"type": "text", "signature": long_id},
                {"type": "tool-call"},
            ],
        });
        let source = model_source("p", "m", Some(replay));
        let turns = vec![Turn::Assistant {
            source,
            content: vec![text_block("a"), call("call1|item1")],
        }];
        let items = convert_responses_messages(&turns, &ctx(&descriptor));
        let expected_hash = format!("msg_{}", crate::hash::short_hash(&long_id));
        assert_eq!(items[0]["id"], expected_hash);
        assert!(items[0].get("phase").is_none());
        assert_eq!(items[1]["type"], "function_call");
        assert!(
            items[1].get("id").is_none(),
            "a non-fc_ item id drops on the same model too"
        );

        let empty_replay = serde_json::json!({
            "response": {"kind": "fr-openai", "model": "m"},
            "blocks": [{"type": "text"}],
        });
        let turns = vec![Turn::Assistant {
            source: model_source("p", "m", Some(empty_replay)),
            content: vec![text_block("a")],
        }];
        let items = convert_responses_messages(&turns, &ctx(&descriptor));
        assert_eq!(items[0]["id"], "msg_pi_0");
    }

    /// Tool results join text with newlines, degrade to the two
    /// placeholder spellings, and carry images only on vision models.
    #[test]
    fn tool_results_pin_text_and_image_faces() {
        let descriptor = model("p", None);
        let context = ctx(&descriptor);
        let items = convert_responses_messages(
            &[Turn::Tool {
                call_id: ToolCallId::new("c1"),
                is_error: None,
                content: vec![],
            }],
            &context,
        );
        assert_eq!(items[0]["output"], "(no tool output)");

        let items = convert_responses_messages(
            &[Turn::Tool {
                call_id: ToolCallId::new("c1"),
                is_error: None,
                content: vec![ContentBlock::Known(KnownContentBlock::Image {
                    attachment: image_reference(),
                    offloaded: None,
                })],
            }],
            &context,
        );
        assert_eq!(items[0]["output"], "(see attached image)");

        let mut vision = model("p", None);
        vision.input = vec![Modality::Text, Modality::Image];
        let context = ResponsesConvertContext {
            supports_images: true,
            images: &|reference: &ImageAttachmentRef| {
                let media_type = match reference.media_type {
                    ImageMediaType::Png => "image/png",
                    ImageMediaType::Jpeg => "image/jpeg",
                    ImageMediaType::Webp => "image/webp",
                    ImageMediaType::Gif => "image/gif",
                };
                Some((media_type.to_owned(), vec![1, 2, 3]))
            },
            ..ctx(&vision)
        };
        let items = convert_responses_messages(
            &[Turn::Tool {
                call_id: ToolCallId::new("c1"),
                is_error: None,
                content: vec![
                    text_block("look"),
                    ContentBlock::Known(KnownContentBlock::Image {
                        attachment: image_reference(),
                        offloaded: None,
                    }),
                ],
            }],
            &context,
        );
        assert_eq!(
            items[0]["output"],
            serde_json::json!([
                {"type": "input_text", "text": "look"},
                {"type": "input_image", "detail": "auto", "image_url": "data:image/png;base64,AQID"},
            ])
        );
    }

    /// The tools face is the plain function shape: no strict field (the
    /// batch-2 dead-input finding), no grammar variant, and an empty set
    /// sends no key.
    #[test]
    fn tools_pin_plain_function_shape() {
        let descriptor = model("p", None);
        let tools = vec![ToolSchema {
            defer_loading: None,
            name: "grep".to_owned(),
            description: "search".to_owned(),
            parameters: serde_json::from_str(r#"{"type":"object"}"#).expect("schema"),
        }];
        let value = build_params_responses(
            &descriptor,
            &responses_compat(),
            CacheRetention::None,
            None,
            &empty_turns(),
            &ctx(&descriptor),
            Some(&tools),
            &options(None, None, false),
        );
        assert_eq!(
            value["tools"],
            serde_json::json!([{
                "type": "function",
                "name": "grep",
                "description": "search",
                "parameters": {"type": "object"},
            }])
        );
        let value = build_params_responses(
            &descriptor,
            &responses_compat(),
            CacheRetention::None,
            None,
            &empty_turns(),
            &ctx(&descriptor),
            Some(&[]),
            &options(None, None, false),
        );
        assert!(value.get("tools").is_none());
    }

    /// The text signature codec: a v1 payload round-trips with the phase
    /// filter, and legacy or non-v1 strings degrade to plain ids.
    #[test]
    fn text_signature_codec_round_trips() {
        let encoded = encode_text_signature_v1("msg_1", Some("final_answer"));
        assert_eq!(
            parse_text_signature(Some(&encoded)),
            Some(TextSignature {
                id: "msg_1".to_owned(),
                phase: Some("final_answer".to_owned()),
            })
        );
        assert_eq!(
            parse_text_signature(Some(&encode_text_signature_v1("msg_1", None))),
            Some(TextSignature {
                id: "msg_1".to_owned(),
                phase: None,
            })
        );
        assert_eq!(
            parse_text_signature(Some("plain_id")),
            Some(TextSignature {
                id: "plain_id".to_owned(),
                phase: None,
            })
        );
        assert_eq!(
            parse_text_signature(Some(r#"{"v":2,"id":"x"}"#)),
            Some(TextSignature {
                id: r#"{"v":2,"id":"x"}"#.to_owned(),
                phase: None,
            })
        );
        assert_eq!(
            parse_text_signature(Some(r"{broken")).map(|signature| signature.id),
            Some("{broken".to_owned())
        );
        assert_eq!(parse_text_signature(None), None);
    }

    /// The Responses id normalizer: non-member routes and pipe-less ids
    /// sanitize to one part, member routes keep the pipe pair with the
    /// `fc_` repair, and foreign sources hash the item id.
    #[test]
    fn normalize_ids_pin_responses_shape() {
        assert_eq!(
            normalize_tool_call_id_responses("call|item", "deepseek", "deepseek"),
            "call_item",
            "outside the member set the id is one sanitized part (pipe included)"
        );
        assert_eq!(
            normalize_tool_call_id_responses("plain id!", "openai", "openai"),
            "plain_id",
            "sanitized, then trailing underscores strip"
        );
        assert_eq!(
            normalize_tool_call_id_responses("call1|item1", "openai", "openai"),
            "call1|fc_item1",
            "member ids keep the pipe and repair the fc_ prefix"
        );
        assert_eq!(
            normalize_tool_call_id_responses("call_|item__", "openai", "openai"),
            "call|fc_item",
            "both parts trim trailing underscores before the repair"
        );
        let foreign = normalize_tool_call_id_responses("call1|item1", "openai", "deepseek");
        let expected = format!("call1|fc_{}", crate::hash::short_hash("item1"));
        assert_eq!(foreign, expected, "foreign item ids hash under fc_");
        let long_call = "c".repeat(70);
        let normalized =
            normalize_tool_call_id_responses(&format!("{long_call}|fc_i"), "openai", "openai");
        let (call_part, item_part) = normalized.split_once('|').expect("pipe kept");
        assert_eq!(call_part.chars().count(), 64, "parts cap at 64");
        assert_eq!(call_part.trim_end_matches('_'), call_part);
        assert_eq!(item_part, "fc_i");
    }
}
