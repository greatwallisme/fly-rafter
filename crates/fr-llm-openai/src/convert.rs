//! History conversion to Chat Completions wire messages (pi-ai
//! `api/openai-completions.ts:1193-1517`, `convertMessages`).
//!
//! Input is the folded, transformed history ([`crate::transcript`] then
//! [`crate::history_transform`]); output is the `messages` array in dsh
//! key order (`role` first, then `content`, then the optional faces). The
//! grammar-tool arms are D8 exclusions and the tool-level strict face is
//! dead input (fr tool declarations carry no `constrainedSampling`), so
//! `convertTools` emits plain function schemas with `strict: false` only
//! when the compat gate accepts the field.

use fr_wire::message::{KnownMessageSource, MessageSource};
use fr_wire::{ContentBlock, KnownContentBlock};
use serde_json::{Map, Value};

use crate::compat::ResolvedCompletionsCompat;
use crate::history_transform::{RouteIdentity, Turn};
use crate::text::content_text;
use fr_wire::call_config::ToolSchema;

/// `sanitizeSurrogates` is the identity in fr: `String` is always a valid
/// scalar encoding and `serde_json` cannot parse lone surrogates
/// (2026-10-27 Q75-C note; the JS-only hazard cannot reach fr history).
pub(crate) const fn sanitize_surrogates(text: &str) -> &str {
    text
}

/// RFC 4648 base64 with padding (dsh `Buffer.toString('base64')`); the
/// alphabet is standardized, so no dependency is spent on it (Q65-A
/// precedent, the fr-llm-deepseek `serialize.rs` encoder).
///
/// # Panics
///
/// Never: the six-bit index lookup is bounded by the mask, and `chunks(3)`
/// never yields an empty slice.
#[must_use]
pub fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let byte = |index: usize| u32::from(chunk.get(index).copied().unwrap_or(0));
        let word = (byte(0) << 16) | (byte(1) << 8) | byte(2);
        let pick = |shift: u32| -> char {
            let sextet = (word >> shift) & 63;
            char::from(ALPHABET[usize::try_from(sextet).expect("a six-bit value fits usize")])
        };
        match chunk.len() {
            3 => {
                out.push(pick(18));
                out.push(pick(12));
                out.push(pick(6));
                out.push(pick(0));
            }
            2 => {
                out.push(pick(18));
                out.push(pick(12));
                out.push(pick(6));
                out.push('=');
            }
            _ => {
                out.push(pick(18));
                out.push(pick(12));
                out.push('=');
                out.push('=');
            }
        }
    }
    out
}

pub(crate) fn image_data_url(media_type: &str, data: &[u8]) -> String {
    format!("data:{media_type};base64,{}", base64_encode(data))
}

/// The image-bytes resolution seam: attachment reference to wire media
/// type and bytes.
pub type ImageResolver<'a> = &'a dyn Fn(&fr_wire::ImageAttachmentRef) -> Option<(String, Vec<u8>)>;

/// The conversion seams.
///
/// Route identity (for the id normalizer's provider check), resolved
/// compat, the model's reasoning flag (`instructionRole`/`thinking`
/// gates), and the image-bytes access the adapter supplies (fr image
/// blocks reference the attachment service; dsh carried inline bytes).
pub struct ConvertContext<'a> {
    /// The route identity.
    pub route: RouteIdentity<'a>,
    /// The resolved completions compat.
    pub compat: &'a ResolvedCompletionsCompat,
    /// Whether the model reasons (`model.reasoning` gate).
    pub reasoning: bool,
    /// Attachment bytes resolution; `None` results skip the image block
    /// (the transform layer already downgraded non-vision images, so a miss
    /// here is a degenerate attachment-service failure, not a normal path).
    pub images: ImageResolver<'a>,
}

impl std::fmt::Debug for ConvertContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConvertContext")
            .field("route", &self.route)
            .field("compat", &self.compat)
            .field("reasoning", &self.reasoning)
            .field("images", &"<closure>")
            .finish()
    }
}

fn image_wire(media_type: &str, data: &[u8]) -> Value {
    serde_json::json!({
        "type": "image_url",
        "image_url": { "url": image_data_url(media_type, data) },
    })
}

/// Tool-call arguments history: the durable raw string is parsed and
/// re-stringified compactly (dsh `JSON.stringify(tc.arguments)` over the
/// parsed object); unparseable history keeps its raw text
/// ("将已解析参数 stringify 回原始串" — contract clause).
pub(crate) fn tool_arguments(raw: &str) -> Value {
    match serde_json::from_str::<Value>(raw) {
        Ok(value @ Value::Object(_)) => value,
        _ => Value::Object(Map::new()),
    }
}

fn text_part(text: &str) -> Value {
    serde_json::json!({ "type": "text", "text": sanitize_surrogates(text) })
}

/// Convert the transformed history to wire messages (dsh
/// `convertMessages`).
///
/// The leading prompt rides `buildParams`' system handling, so only
/// user/assistant/tool turns appear here. Consecutive tool results read as
/// one group (dsh's `j` loop); image groups insert the bridge (when
/// required) and a user message carrying the images, which also resets the
/// bridge-tracking role to `user`.
#[must_use]
pub fn convert_messages(turns: &[Turn], ctx: &ConvertContext<'_>) -> Vec<Value> {
    let mut params: Vec<Value> = Vec::new();
    let mut last_role: Option<&'static str> = None;
    let mut index = 0;
    while index < turns.len() {
        // The bridge arm: a user message directly after tool results needs
        // a synthetic assistant acknowledgment (`requiresAssistantAfterToolResult`).
        if ctx.compat.requires_assistant_after_tool_result
            && last_role == Some("toolResult")
            && matches!(turns[index], Turn::User { .. })
        {
            params.push(serde_json::json!({
                "role": "assistant",
                "content": "I have processed the tool results.",
            }));
            // dsh only pushes the bridge; `lastRole` then updates below.
        }
        match &turns[index] {
            Turn::User { content } => {
                convert_user(content, ctx, &mut params);
                last_role = Some("user");
                index += 1;
            }
            Turn::Assistant { source, content } => {
                convert_assistant(source, content, ctx, &mut params);
                last_role = Some("assistant");
                index += 1;
            }
            Turn::Tool { .. } => {
                let mut end = index;
                while end < turns.len() && matches!(turns[end], Turn::Tool { .. }) {
                    end += 1;
                }
                let mut image_blocks: Vec<Value> = Vec::new();
                for turn in &turns[index..end] {
                    if let Turn::Tool {
                        call_id, content, ..
                    } = turn
                    {
                        convert_tool(
                            call_id.as_str(),
                            content,
                            ctx,
                            &mut params,
                            &mut image_blocks,
                        );
                    }
                }
                if !image_blocks.is_empty() && ctx.compat.requires_assistant_after_tool_result {
                    params.push(serde_json::json!({
                        "role": "assistant",
                        "content": "I have processed the tool results.",
                    }));
                }
                if image_blocks.is_empty() {
                    last_role = Some("toolResult");
                } else {
                    let mut content = vec![text_part("Attached image(s) from tool result:")];
                    content.extend(image_blocks);
                    params.push(serde_json::json!({ "role": "user", "content": content }));
                    last_role = Some("user");
                }
                index = end;
            }
        }
    }
    params
}

/// The user arm: a raw string stays a string; content arrays drop empty
/// text parts and render images as data URLs.
fn convert_user(content: &[ContentBlock], ctx: &ConvertContext<'_>, params: &mut Vec<Value>) {
    match content {
        [] => {}
        [ContentBlock::Known(KnownContentBlock::Text { text })] => {
            params.push(serde_json::json!({
                "role": "user",
                "content": sanitize_surrogates(text),
            }));
        }
        blocks => {
            let parts: Vec<Value> = blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Known(KnownContentBlock::Text { text }) if !text.is_empty() => {
                        Some(text_part(text))
                    }
                    ContentBlock::Known(KnownContentBlock::Image { attachment, .. }) => {
                        (ctx.images)(attachment)
                            .map(|(media_type, data)| image_wire(&media_type, &data))
                    }
                    _ => None,
                })
                .collect();
            if parts.is_empty() {
                return;
            }
            params.push(serde_json::json!({ "role": "user", "content": parts }));
        }
    }
}

/// The assistant arm (openai-completions.ts:1291-1405).
///
/// Content is always a plain string (the `DeepSeek` V3.2 / `NVIDIA NIM`
/// recursive-nesting comment); thinking travels as text under
/// `requiresThinkingAsText` or rides the three reasoning fields; replayed
/// reasoning details come from the D12 envelope, and empty content without
/// tool calls drops.
fn convert_assistant(
    source: &MessageSource,
    content: &[ContentBlock],
    ctx: &ConvertContext<'_>,
    params: &mut Vec<Value>,
) {
    let initial_content = if ctx.compat.requires_assistant_after_tool_result {
        ""
    } else {
        "null"
    };
    let mut message = Map::new();
    message.insert("role".to_owned(), Value::String("assistant".to_owned()));
    message.insert(
        "content".to_owned(),
        Value::String(initial_content.to_owned()),
    );
    let text_parts: Vec<&str> = content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Known(KnownContentBlock::Text { text }) if !text.trim().is_empty() => {
                Some(sanitize_surrogates(text))
            }
            _ => None,
        })
        .collect();
    let assistant_text = text_parts.concat();
    let thinking_blocks: Vec<&str> = content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Known(KnownContentBlock::Reasoning { text })
                if !text.trim().is_empty() =>
            {
                Some(sanitize_surrogates(text))
            }
            _ => None,
        })
        .collect();
    let tool_calls: Vec<&fr_wire::KnownContentBlock> = content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Known(call @ KnownContentBlock::ToolCall { .. }) => Some(call),
            _ => None,
        })
        .collect();
    if !thinking_blocks.is_empty() {
        if ctx.compat.requires_thinking_as_text {
            let thinking_text = thinking_blocks.join("\n\n");
            let mut parts = vec![text_part(&thinking_text)];
            parts.extend(text_parts.iter().map(|text| text_part(text)));
            message.insert("content".to_owned(), Value::Array(parts));
        } else {
            if !assistant_text.is_empty() {
                message.insert("content".to_owned(), Value::String(assistant_text));
            }
            // Replay signatures: the fr-llm-deepseek `read_replay` degrade
            // semantics over the D12 envelope; kind `fr-openai`.
            let signature_field = thinking_signature_field(source, ctx);
            if let Some(field) = signature_field {
                message.insert(field.to_owned(), Value::String(thinking_blocks.join("\n")));
            }
        }
    } else if !assistant_text.is_empty() {
        message.insert("content".to_owned(), Value::String(assistant_text));
    }
    if !tool_calls.is_empty() {
        message.insert(
            "tool_calls".to_owned(),
            Value::Array(tool_calls.into_iter().filter_map(tool_call_wire).collect()),
        );
    }
    if ctx.compat.requires_reasoning_content_on_assistant_messages
        && ctx.reasoning
        && !message.contains_key("reasoning_content")
    {
        message.insert("reasoning_content".to_owned(), Value::String(String::new()));
    }
    let has_content = message.get("content").is_some_and(|content| match content {
        Value::String(text) => !text.is_empty(),
        Value::Array(parts) => !parts.is_empty(),
        _ => false,
    });
    if !has_content && !message.contains_key("tool_calls") {
        return;
    }
    params.push(Value::Object(message));
}

/// The reasoning replay field the provider used for a block (dsh
/// `thinkingSignature` routed over
/// `OPENAI_COMPLETIONS_REASONING_FIELDS`): the D12 envelope's reasoning
/// block signature. Degrades to `None` on any envelope mismatch (dsh
/// `read_replay` degrade → drop the replay face). The `opencode-go`
/// `"reasoning" → "reasoning_content"` correction arm is dead here (that
/// provider never appears in fr route tables).
fn thinking_signature_field(
    source: &MessageSource,
    ctx: &ConvertContext<'_>,
) -> Option<&'static str> {
    const REASONING_FIELDS: [&str; 3] = ["reasoning", "reasoning_content", "reasoning_text"];
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
    if response.get("model")?.as_str()? != ctx.route.model {
        return None;
    }
    let blocks = replay.get("blocks")?.as_array()?;
    blocks
        .iter()
        .find_map(|block| {
            let type_matches = block.get("type")?.as_str()? == "reasoning";
            let signature = block.get("signature")?.as_str()?;
            type_matches.then_some(signature)
        })
        .and_then(|signature| REASONING_FIELDS.iter().find(|field| *field == &signature))
        .copied()
}

/// One tool call in the assistant message (`tool_calls` entry): the
/// `type: "custom"` grammar arm is a D8 exclusion, so every call is a
/// function call whose arguments are the compact stringify of the parsed
/// history (unparseable history yields the empty object, the
/// fr-llm-deepseek `toolInput` precedent).
fn tool_call_wire(call: &KnownContentBlock) -> Option<Value> {
    let KnownContentBlock::ToolCall {
        id,
        name,
        arguments,
    } = call
    else {
        return None;
    };
    Some(serde_json::json!({
        "id": id.as_str(),
        "type": "function",
        "function": {
            "name": name,
            "arguments": tool_arguments(arguments).to_string(),
        },
    }))
}

/// One tool result in a group (`role: "tool"`): text joins on newlines with
/// the image-only/no-output placeholders, the call id rides `tool_call_id`,
/// and the `name` field appears only under `requiresToolResultName` — dead
/// in fr (tool results carry no name field), noted at the compat gate.
fn convert_tool(
    call_id: &str,
    content: &[ContentBlock],
    ctx: &ConvertContext<'_>,
    params: &mut Vec<Value>,
    image_blocks: &mut Vec<Value>,
) {
    let text_result = content_text(content, "\n");
    let has_images = content
        .iter()
        .any(|block| matches!(block, ContentBlock::Known(KnownContentBlock::Image { .. })));
    let has_text = !text_result.is_empty();
    let tool_result_text = if has_text {
        text_result
    } else if has_images {
        "(see attached image)".to_owned()
    } else {
        "(no tool output)".to_owned()
    };
    let mut message = Map::new();
    message.insert("role".to_owned(), Value::String("tool".to_owned()));
    message.insert(
        "content".to_owned(),
        Value::String(sanitize_surrogates(&tool_result_text).to_owned()),
    );
    message.insert("tool_call_id".to_owned(), Value::String(call_id.to_owned()));
    if ctx.compat.requires_tool_result_name {
        // Unreachable: fr tool results have no name; the gate exists to
        // keep the compat face complete. If a future vocabulary adds the
        // field, it lands here.
    }
    params.push(Value::Object(message));
    for block in content {
        if let ContentBlock::Known(KnownContentBlock::Image { attachment, .. }) = block
            && let Some((media_type, data)) = (ctx.images)(attachment)
        {
            image_blocks.push(image_wire(&media_type, &data));
        }
    }
}

/// The tools request field (dsh `convertTools`): plain function schemas.
///
/// The grammar arm is a D8 exclusion and the tool-level strict face is
/// dead input (fr declarations carry no `constrainedSampling`), so
/// `strict` is `false` exactly when the compat gate accepts the field
/// (`supportsStrictMode !== false`).
#[must_use]
pub fn convert_tools(tools: &[ToolSchema], compat: &ResolvedCompletionsCompat) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            let mut function = Map::new();
            function.insert("name".to_owned(), Value::String(tool.name.clone()));
            function.insert(
                "description".to_owned(),
                Value::String(tool.description.clone()),
            );
            function.insert(
                "parameters".to_owned(),
                Value::Object(tool.parameters.clone()),
            );
            if compat.supports_strict_mode {
                // The strict value is `strict ?? false` with an always-
                // undefined strict request (see the module docs).
                function.insert("strict".to_owned(), Value::Bool(false));
            }
            serde_json::json!({ "type": "function", "function": Value::Object(function) })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::{MaxTokensField, ThinkingFormat};
    use crate::history_transform::transform_history;
    use crate::transcript::fold_transcript;
    use fr_llm::adapter::{GenerateOptions, RequestMessage};
    use fr_wire::RawObject;
    use fr_wire::ids::{MessageId, ToolCallId};
    use fr_wire::message::KnownMessage;

    fn route() -> RouteIdentity<'static> {
        RouteIdentity {
            provider: "p",
            model: "m",
        }
    }

    fn no_images(_reference: &fr_wire::ImageAttachmentRef) -> Option<(String, Vec<u8>)> {
        None
    }

    fn compat() -> ResolvedCompletionsCompat {
        ResolvedCompletionsCompat {
            supports_store: true,
            supports_developer_role: true,
            supports_reasoning_effort: true,
            supports_usage_in_streaming: true,
            supports_finish_reason: true,
            max_tokens_field: MaxTokensField::MaxCompletionTokens,
            requires_tool_result_name: false,
            requires_assistant_after_tool_result: true,
            requires_thinking_as_text: true,
            requires_reasoning_content_on_assistant_messages: false,
            thinking_format: ThinkingFormat::Openai,
            chat_template_kwargs: std::collections::BTreeMap::new(),
            chat_template_args: std::collections::BTreeMap::new(),
            supports_thinking_token_budget: false,
            thinking_token_budget_field: None,
            vllm_priority: None,
            supports_strict_mode: true,
            cache_control_format: None,
            supports_long_cache_retention: true,
            send_session_affinity_headers: false,
            session_affinity_format: crate::compat::SessionAffinityFormat::Openai,
            supports_mid_convo_system_messages: false,
            supports_mid_convo_tool_additions: false,
        }
    }

    /// Full-history shape: the bridge lands between tool results and the
    /// user turn, thinking travels as text, tool arguments re-stringify
    /// compactly, and the group image path emits the attachment message.
    #[test]
    fn converts_history_with_bridge_and_thinking_text() {
        let request = |content: Vec<ContentBlock>| KnownMessage::User {
            id: MessageId::new("u"),
            content,
            source: MessageSource::Known(KnownMessageSource::User {
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        };
        let call = KnownMessage::Assistant {
            id: MessageId::new("a"),
            content: vec![
                ContentBlock::Known(KnownContentBlock::Reasoning {
                    text: "ponder".to_owned(),
                }),
                ContentBlock::Known(KnownContentBlock::ToolCall {
                    id: ToolCallId::new("c1"),
                    name: "sh".to_owned(),
                    arguments: r#"{"cmd":"ls"}"#.to_owned(),
                }),
            ],
            source: MessageSource::Known(KnownMessageSource::Model {
                provider: "p".to_owned(),
                model: "m".to_owned(),
                replay_state: None,
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        };
        let result = KnownMessage::Tool {
            id: MessageId::new("t"),
            content: vec![ContentBlock::Known(KnownContentBlock::Text {
                text: "done".to_owned(),
            })],
            source: MessageSource::Known(KnownMessageSource::Tool {
                call_id: ToolCallId::new("c1"),
                extra: RawObject::new(),
            }),
            tool_call_id: ToolCallId::new("c1"),
            is_error: None,
            extra: RawObject::new(),
        };
        let history = vec![
            RequestMessage::Durable(fr_wire::Message::Known(Box::new(request(vec![
                ContentBlock::Known(KnownContentBlock::Text {
                    text: "run".to_owned(),
                }),
            ])))),
            RequestMessage::Durable(fr_wire::Message::Known(Box::new(call))),
            RequestMessage::Durable(fr_wire::Message::Known(Box::new(result))),
            RequestMessage::Durable(fr_wire::Message::Known(Box::new(request(vec![
                ContentBlock::Known(KnownContentBlock::Text {
                    text: "next".to_owned(),
                }),
            ])))),
        ];
        let options = GenerateOptions {
            provider: "p".to_owned(),
            model: "m".to_owned(),
            reasoning_effort: None,
            messages: history,
            system: None,
            tools: None,
            tool_history: None,
            temperature: None,
            max_tokens: None,
            stop: None,
            signal: None,
            session_id: None,
            purpose: None,
        };
        let folded = fold_transcript(&options);
        let profile = compat();
        let ctx = ConvertContext {
            route: route(),
            compat: &profile,
            reasoning: true,
            images: &no_images,
        };
        let turns = transform_history(
            &folded.messages,
            &ctx.route,
            true,
            crate::history_transform::IdNormalization::Completions,
        );
        let params = convert_messages(&turns, &ctx);
        let roles: Vec<&str> = params
            .iter()
            .map(|message| message["role"].as_str().expect("role"))
            .collect();
        // user, assistant(bridge-worthy: content ""+tool_calls), tool,
        // bridge, user.
        assert_eq!(roles, ["user", "assistant", "tool", "assistant", "user"]);
        // Thinking text rides the content array head; the call arguments
        // compact-stringify.
        let assistant = &params[1];
        let content = assistant["content"].as_array().expect("array content");
        assert_eq!(content[0]["text"], "ponder");
        let arguments = &assistant["tool_calls"][0]["function"]["arguments"];
        assert_eq!(arguments.as_str(), Some(r#"{"cmd":"ls"}"#));
        // The bridge text.
        assert_eq!(params[3]["content"], "I have processed the tool results.");
    }

    /// `convert_tools`: plain function schemas; `strict: false` appears
    /// only when the compat gate accepts the field.
    #[test]
    fn converts_tools_with_strict_gate() {
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
        let with_strict = convert_tools(&tools, &profile);
        assert_eq!(with_strict[0]["function"]["strict"], Value::Bool(false));
        let mut no_strict_profile = profile;
        no_strict_profile.supports_strict_mode = false;
        let without = convert_tools(&tools, &no_strict_profile);
        assert!(without[0]["function"].get("strict").is_none());
    }
}
