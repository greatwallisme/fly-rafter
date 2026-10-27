//! History conversion to anthropic-messages wire messages (pi-ai
//! `api/anthropic-messages.ts` `convertMessages`/`convertContentBlocks`/
//! `convertTools`, anthropic-messages.ts:1294-1611).
//!
//! Input is the folded, transformed history ([`crate::transcript`] then
//! [`crate::history_transform`]); output is the `messages` array in dsh
//! key order. The mid-conversation system blocks (`tool_addition`/
//! `tool_removal` held back until the next assistant turn) are an
//! in-history face that needs withheld compat switches only an installed
//! catalog sets (decision 45 A2): the fold collapses system messages
//! upstream, so no system turn reaches this conversion and the pending
//! buffer is a dead arm. The pi-ai deferred-tool anchor constant belongs
//! to the same unreachable face.
//!
//! Thinking replay: signatures and the redacted marker ride the D12
//! envelope (kind `fr-anthropic`, one block entry per emitted block in
//! stream order); `sanitizeSurrogates` is the identity in fr (the
//! openai-crate note).

use std::borrow::Cow;

use fr_wire::call_config::ToolSchema;
use fr_wire::message::{KnownMessageSource, MessageSource};
use fr_wire::{ContentBlock, KnownContentBlock};
use serde_json::{Map, Value};

use crate::compat::AnthropicCompat;
use crate::history_transform::{RouteIdentity, Turn};

/// The Claude Code client version the OAuth stealth face names (pi-ai
/// `claudeCodeVersion`, anthropic-messages.ts:96).
pub const CLAUDE_CODE_VERSION: &str = "2.1.280";

/// The Claude Code tool-name vocabulary (pi-ai `claudeCodeTools`,
/// anthropic-messages.ts:101-119, canonical casing).
const CLAUDE_CODE_TOOLS: [&str; 17] = [
    "Read",
    "Write",
    "Edit",
    "Bash",
    "Grep",
    "Glob",
    "AskUserQuestion",
    "EnterPlanMode",
    "ExitPlanMode",
    "KillShell",
    "NotebookEdit",
    "Skill",
    "Task",
    "TaskOutput",
    "TodoWrite",
    "WebFetch",
    "WebSearch",
];

/// Converts a tool name to Claude Code canonical casing when it matches
/// the vocabulary case-insensitively (pi-ai `toClaudeCodeName`; the
/// send-side half of the D15-a stealth face).
#[must_use]
pub fn to_claude_code_name(name: &str) -> Cow<'_, str> {
    CLAUDE_CODE_TOOLS
        .iter()
        .find(|canonical| canonical.eq_ignore_ascii_case(name))
        .map_or_else(
            || Cow::Borrowed(name),
            |canonical| Cow::Borrowed(*canonical),
        )
}

/// Maps a Claude Code casing back to the declared tool name (pi-ai
/// `fromClaudeCodeName`; the receive-side half — the model echoed the
/// canonical casing, the harness knows the original).
#[must_use]
pub fn from_claude_code_name<'a>(name: &'a str, tools: &[ToolSchema]) -> Cow<'a, str> {
    tools
        .iter()
        .find(|tool| tool.name.eq_ignore_ascii_case(name))
        .map_or_else(|| Cow::Borrowed(name), |tool| Cow::Owned(tool.name.clone()))
}

/// RFC 4648 base64 with padding (dsh `Buffer.toString('base64')`; the
/// fr-llm-deepseek `serialize.rs` encoder, openai-crate copy).
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

/// The image-bytes resolution seam: attachment reference to wire media
/// type and bytes (the openai-crate contract).
pub type ImageResolver<'a> = &'a dyn Fn(&fr_wire::ImageAttachmentRef) -> Option<(String, Vec<u8>)>;

/// The conversion seams.
pub struct ConvertContext<'a> {
    /// The route identity (replay envelope model check).
    pub route: RouteIdentity<'a>,
    /// The resolved anthropic compat.
    pub compat: &'a AnthropicCompat,
    /// Whether the resolved credential is an OAuth token (`sk-ant-oat`,
    /// the D15-a stealth face: send-side tool names map to Claude Code
    /// casing).
    pub is_oauth: bool,
    /// Attachment bytes resolution; `None` results skip the image block
    /// (the transform layer already downgraded non-vision images).
    pub images: ImageResolver<'a>,
}

impl std::fmt::Debug for ConvertContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConvertContext")
            .field("route", &self.route)
            .field("compat", &self.compat)
            .field("is_oauth", &self.is_oauth)
            .field("images", &"<closure>")
            .finish()
    }
}

/// Tool-call arguments history: parse the durable raw string to the
/// object the wire wants; unparseable history degrades to the empty
/// object (the uniform lenient-parse rule, Q75-C / verdict 1A).
pub(crate) fn tool_arguments(raw: &str) -> Value {
    match serde_json::from_str::<Value>(raw) {
        Ok(value @ Value::Object(_)) => value,
        _ => Value::Object(Map::new()),
    }
}

fn text_block(text: &str) -> Value {
    serde_json::json!({ "type": "text", "text": text })
}

fn image_block(media_type: &str, data: &[u8]) -> Value {
    serde_json::json!({
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": media_type,
            "data": base64_encode(data),
        },
    })
}

/// One tool-result content body (pi-ai `convertContentBlocks`,
/// anthropic-messages.ts:137-184): text-only joins on newlines into a
/// plain string; any image switches to the block array, and an
/// images-only array leads with the placeholder text.
fn convert_content_blocks(content: &[ContentBlock], ctx: &ConvertContext<'_>) -> Value {
    let has_images = content
        .iter()
        .any(|block| matches!(block, ContentBlock::Known(KnownContentBlock::Image { .. })));
    if !has_images {
        let texts: Vec<&str> = content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Known(KnownContentBlock::Text { text }) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        return Value::String(texts.join("\n"));
    }
    let mut blocks: Vec<Value> = content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Known(KnownContentBlock::Text { text }) => Some(text_block(text)),
            ContentBlock::Known(KnownContentBlock::Image { attachment, .. }) => {
                (ctx.images)(attachment).map(|(media_type, data)| image_block(&media_type, &data))
            }
            _ => None,
        })
        .collect();
    if blocks.iter().all(|block| block["type"] == "image") {
        blocks.insert(0, text_block("(see attached image)"));
    }
    Value::Array(blocks)
}

/// The thinking-replay entries of one assistant message's D12 envelope
/// (kind `fr-anthropic`), in block order: `(signature, redacted)` per
/// thinking block. Degrades to nothing on any envelope mismatch.
fn thinking_replay(
    source: &MessageSource,
    route: &RouteIdentity<'_>,
) -> Vec<(Option<String>, bool)> {
    let MessageSource::Known(KnownMessageSource::Model {
        provider,
        model,
        replay_state: Some(replay),
        ..
    }) = source
    else {
        return Vec::new();
    };
    if provider != route.provider || model != route.model {
        return Vec::new();
    }
    let Some(blocks) = replay.get("blocks").and_then(Value::as_array) else {
        return Vec::new();
    };
    if replay
        .get("response")
        .and_then(|r| r.get("kind"))
        .and_then(Value::as_str)
        != Some("fr-anthropic")
    {
        return Vec::new();
    }
    blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("thinking"))
        .map(|block| {
            let signature = block
                .get("signature")
                .and_then(Value::as_str)
                .filter(|sig| !sig.is_empty())
                .map(str::to_owned);
            let redacted = block.get("redacted") == Some(&Value::Bool(true));
            (signature, redacted)
        })
        .collect()
}

/// Convert the transformed history to wire messages (pi-ai
/// `convertMessages`, anthropic-messages.ts:1308-1512).
///
/// Consecutive tool results read as one `user` message of `tool_result`
/// blocks (Anthropic requires them to immediately follow their
/// `tool_use`); the caller stamps the trailing `cache_control`.
#[must_use]
pub fn convert_messages(turns: &[Turn], ctx: &ConvertContext<'_>) -> Vec<Value> {
    let mut params: Vec<Value> = Vec::new();
    let mut index = 0;
    while index < turns.len() {
        match &turns[index] {
            Turn::User { content } => {
                convert_user(content, ctx, &mut params);
                index += 1;
            }
            Turn::Assistant { source, content } => {
                convert_assistant(source, content, ctx, &mut params);
                index += 1;
            }
            Turn::Tool { .. } => {
                let mut end = index;
                while end < turns.len() && matches!(turns[end], Turn::Tool { .. }) {
                    end += 1;
                }
                let mut results: Vec<Value> = Vec::new();
                for turn in &turns[index..end] {
                    if let Turn::Tool {
                        call_id,
                        is_error,
                        content,
                    } = turn
                    {
                        let mut result = Map::new();
                        result.insert("type".to_owned(), Value::String("tool_result".to_owned()));
                        result.insert(
                            "tool_use_id".to_owned(),
                            Value::String(call_id.as_str().to_owned()),
                        );
                        result.insert("content".to_owned(), convert_content_blocks(content, ctx));
                        if is_error.is_some() {
                            result.insert("is_error".to_owned(), Value::Bool(true));
                        }
                        results.push(Value::Object(result));
                    }
                }
                params.push(serde_json::json!({
                    "role": "user",
                    "content": results,
                }));
                index = end;
            }
        }
    }
    params
}

/// The user arm: block arrays drop blank text parts and render images;
/// an all-blank turn skips (pi-ai's string-content case never arrives:
/// harness history always carries blocks).
fn convert_user(content: &[ContentBlock], ctx: &ConvertContext<'_>, params: &mut Vec<Value>) {
    let parts: Vec<Value> = content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Known(KnownContentBlock::Text { text }) if !text.trim().is_empty() => {
                Some(text_block(text))
            }
            ContentBlock::Known(KnownContentBlock::Image { attachment, .. }) => {
                (ctx.images)(attachment).map(|(media_type, data)| image_block(&media_type, &data))
            }
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        return;
    }
    params.push(serde_json::json!({ "role": "user", "content": parts }));
}

/// The assistant arm (anthropic-messages.ts:1393-1461): blank text drops,
/// thinking replays its signature (redacted markers restore
/// `redacted_thinking`), a missing signature degrades to plain text
/// unless the model takes empty signatures, and tool calls re-parse their
/// arguments.
fn convert_assistant(
    source: &MessageSource,
    content: &[ContentBlock],
    ctx: &ConvertContext<'_>,
    params: &mut Vec<Value>,
) {
    let replay = thinking_replay(source, &ctx.route);
    let mut thinking_index = 0;
    let mut blocks: Vec<Value> = Vec::new();
    for block in content {
        match block {
            ContentBlock::Known(KnownContentBlock::Text { text }) if !text.trim().is_empty() => {
                blocks.push(text_block(text));
            }
            ContentBlock::Known(KnownContentBlock::Reasoning { text }) => {
                let (signature, redacted) =
                    replay.get(thinking_index).cloned().unwrap_or((None, false));
                thinking_index += 1;
                if redacted {
                    // The opaque payload rides the signature slot.
                    blocks.push(serde_json::json!({
                        "type": "redacted_thinking",
                        "data": signature.unwrap_or_default(),
                    }));
                    continue;
                }
                let has_signature = signature.is_some();
                if text.trim().is_empty() && !has_signature {
                    continue;
                }
                match signature {
                    Some(signature) => blocks.push(serde_json::json!({
                        "type": "thinking",
                        "thinking": text,
                        "signature": signature,
                    })),
                    None => {
                        if ctx.compat.allow_empty_signature {
                            blocks.push(serde_json::json!({
                                "type": "thinking",
                                "thinking": text,
                                "signature": "",
                            }));
                        } else {
                            blocks.push(text_block(text));
                        }
                    }
                }
            }
            ContentBlock::Known(KnownContentBlock::ToolCall {
                id,
                name,
                arguments,
            }) => {
                let wire_name = if ctx.is_oauth {
                    to_claude_code_name(name)
                } else {
                    Cow::Borrowed(name.as_str())
                };
                blocks.push(serde_json::json!({
                    "type": "tool_use",
                    "id": id.as_str(),
                    "name": wire_name.as_ref(),
                    "input": tool_arguments(arguments),
                }));
            }
            _ => {}
        }
    }
    if blocks.is_empty() {
        return;
    }
    params.push(serde_json::json!({ "role": "assistant", "content": blocks }));
}

/// Stamps the trailing `cache_control` on the last wire message's last
/// block (pi-ai `convertMessages` tail, anthropic-messages.ts:1483-1509).
///
/// The stamp lands on the last `user` message (a system tail and the
/// in-history block types are unreachable faces); a string content wraps
/// into one text block.
///
/// # Panics
///
/// Never on well-formed output: the "typed block" arm expects an object,
/// and every block this module emits with a `type` field is an object.
pub fn stamp_trailing_cache_control(messages: &mut [Value], cache_control: &Value) {
    let Some(last) = messages.last_mut() else {
        return;
    };
    if last.get("role").and_then(Value::as_str) != Some("user") {
        return;
    }
    match last.get_mut("content") {
        Some(Value::Array(blocks)) => {
            let Some(block) = blocks.last_mut() else {
                return;
            };
            let stampable = block
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| {
                    matches!(
                        kind,
                        "text" | "image" | "tool_result" | "tool_addition" | "tool_removal"
                    )
                });
            if stampable {
                block
                    .as_object_mut()
                    .expect("a typed block is an object")
                    .insert("cache_control".to_owned(), cache_control.clone());
            }
        }
        Some(content @ Value::String(_)) => {
            let text = content.as_str().unwrap_or_default().to_owned();
            *content = serde_json::json!({
                "type": "text",
                "text": text,
                "cache_control": cache_control,
            });
        }
        _ => {}
    }
}

/// The tools request field (pi-ai `convertTools`,
/// anthropic-messages.ts:1576-1611).
///
/// Legacy `input_schema` normalization (`properties`/`required` default
/// to empty), `eager_input_streaming` under its compat switch, the
/// send-side stealth name map, and `cache_control` on the last tool.
/// `strict: true` never appears: fr tool declarations carry no
/// `constrainedSampling`, so `resolveJsonSchemaStrictSampling` stays
/// `undefined` (D23 dead input). The deferred-tool anchor belongs to the
/// unreachable in-history face (A2).
#[must_use]
pub fn convert_tools(
    tools: &[ToolSchema],
    ctx: &ConvertContext<'_>,
    cache_control: Option<&Value>,
) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(tools.len());
    for (index, tool) in tools.iter().enumerate() {
        let parameters = &tool.parameters;
        let mut schema = Map::new();
        schema.insert("type".to_owned(), Value::String("object".to_owned()));
        schema.insert(
            "properties".to_owned(),
            parameters
                .get("properties")
                .cloned()
                .unwrap_or_else(|| Value::Object(Map::new())),
        );
        schema.insert(
            "required".to_owned(),
            parameters
                .get("required")
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new())),
        );
        let name = if ctx.is_oauth {
            to_claude_code_name(&tool.name)
        } else {
            Cow::Borrowed(tool.name.as_str())
        };
        let mut entry = Map::new();
        entry.insert("name".to_owned(), Value::String(name.into_owned()));
        entry.insert(
            "description".to_owned(),
            Value::String(tool.description.clone()),
        );
        if ctx.compat.supports_eager_tool_input_streaming {
            entry.insert("eager_input_streaming".to_owned(), Value::Bool(true));
        }
        entry.insert("input_schema".to_owned(), Value::Object(schema));
        if let Some(cache_control) = cache_control
            && ctx.compat.supports_cache_control_on_tools
            && index == tools.len() - 1
        {
            entry.insert("cache_control".to_owned(), cache_control.clone());
        }
        out.push(Value::Object(entry));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history_transform::{IdNormalization, transform_history};
    use crate::transcript::fold_transcript;
    use fr_llm::adapter::{GenerateOptions, RequestMessage};
    use fr_wire::RawObject;
    use fr_wire::ids::{MessageId, ToolCallId};
    use fr_wire::message::KnownMessage;

    fn route() -> RouteIdentity<'static> {
        RouteIdentity {
            provider: "anthropic",
            model: "claude-test",
        }
    }

    fn no_images(_reference: &fr_wire::ImageAttachmentRef) -> Option<(String, Vec<u8>)> {
        None
    }

    fn ctx(compat: &AnthropicCompat) -> ConvertContext<'_> {
        ConvertContext {
            route: route(),
            compat,
            is_oauth: false,
            images: &no_images,
        }
    }

    fn compat() -> AnthropicCompat {
        AnthropicCompat {
            supports_eager_tool_input_streaming: true,
            supports_long_cache_retention: true,
            supports_cache_control_on_tools: true,
            supports_temperature: true,
            force_adaptive_thinking: false,
            allow_empty_signature: false,
            supports_strict_tools: false,
        }
    }

    fn replay_envelope(blocks: &Value) -> serde_json::Value {
        serde_json::json!({
            "response": { "kind": "fr-anthropic", "version": 1, "model": "claude-test" },
            "blocks": blocks.clone(),
        })
    }

    fn user(text: &str) -> KnownMessage {
        KnownMessage::User {
            id: MessageId::new("u"),
            content: vec![ContentBlock::Known(KnownContentBlock::Text {
                text: text.to_owned(),
            })],
            source: MessageSource::Known(KnownMessageSource::User {
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        }
    }

    fn assistant(content: Vec<ContentBlock>, replay: Option<Value>) -> KnownMessage {
        KnownMessage::Assistant {
            id: MessageId::new("a"),
            content,
            source: MessageSource::Known(KnownMessageSource::Model {
                provider: "anthropic".to_owned(),
                model: "claude-test".to_owned(),
                replay_state: replay,
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        }
    }

    fn tool_result(call_id: &str, text: &str, is_error: bool) -> KnownMessage {
        KnownMessage::Tool {
            id: MessageId::new("t"),
            content: vec![ContentBlock::Known(KnownContentBlock::Text {
                text: text.to_owned(),
            })],
            source: MessageSource::Known(KnownMessageSource::Tool {
                call_id: ToolCallId::new(call_id),
                extra: RawObject::new(),
            }),
            tool_call_id: ToolCallId::new(call_id),
            is_error: is_error.then_some(fr_wire::TrueOnly),
            extra: RawObject::new(),
        }
    }

    fn turns(messages: Vec<RequestMessage>) -> Vec<Turn> {
        let options = GenerateOptions {
            provider: "anthropic".to_owned(),
            model: "claude-test".to_owned(),
            reasoning_effort: None,
            messages,
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
        transform_history(&folded.messages, &route(), true, IdNormalization::Anthropic)
    }

    fn durable(known: KnownMessage) -> RequestMessage {
        RequestMessage::Durable(fr_wire::Message::Known(Box::new(known)))
    }

    /// Full history: thinking replays its signature, tool arguments
    /// re-parse, consecutive tool results group into one user message,
    /// and the trailing cache stamp lands on the last `tool_result`.
    #[test]
    fn converts_history_with_thinking_and_tool_results() {
        let replay = replay_envelope(&serde_json::json!([
            { "type": "thinking", "signature": "sig1" },
        ]));
        let history = vec![
            durable(user("run")),
            durable(assistant(
                vec![
                    ContentBlock::Known(KnownContentBlock::Reasoning {
                        text: "ponder".to_owned(),
                    }),
                    ContentBlock::Known(KnownContentBlock::ToolCall {
                        id: ToolCallId::new("call_1"),
                        name: "sh".to_owned(),
                        arguments: r#"{"cmd":"ls"}"#.to_owned(),
                    }),
                ],
                Some(replay),
            )),
            durable(tool_result("call_1", "done", false)),
            durable(user("next")),
        ];
        let profile = compat();
        let context = ctx(&profile);
        let mut messages = convert_messages(&turns(history), &context);
        stamp_trailing_cache_control(&mut messages, &serde_json::json!({ "type": "ephemeral" }));
        let roles: Vec<&str> = messages
            .iter()
            .map(|message| message["role"].as_str().expect("role"))
            .collect();
        assert_eq!(roles, ["user", "assistant", "user", "user"]);
        let assistant = &messages[1];
        assert_eq!(assistant["content"][0]["type"], "thinking");
        assert_eq!(assistant["content"][0]["signature"], "sig1");
        assert_eq!(assistant["content"][1]["type"], "tool_use");
        assert_eq!(assistant["content"][1]["input"]["cmd"], "ls");
        let results = &messages[2];
        assert_eq!(results["content"][0]["type"], "tool_result");
        assert_eq!(results["content"][0]["tool_use_id"], "call_1");
        assert_eq!(results["content"][0]["content"], "done");
        assert!(results["content"][0].get("is_error").is_none());
        let last = &messages[3];
        assert_eq!(
            last["content"].as_array().expect("array")[0]["cache_control"],
            serde_json::json!({ "type": "ephemeral" })
        );
    }

    /// Signature degradation: an unsignatured thinking block becomes a
    /// plain text block unless the model takes empty signatures; a
    /// redacted replay entry restores `redacted_thinking` with its
    /// payload.
    #[test]
    fn signature_degradation_arms() {
        let no_replay = assistant(
            vec![ContentBlock::Known(KnownContentBlock::Reasoning {
                text: "hmm".to_owned(),
            })],
            None,
        );
        let profile = compat();
        let context = ctx(&profile);
        let converted = convert_messages(&turns(vec![durable(no_replay)]), &context);
        assert_eq!(converted[0]["content"][0]["type"], "text");
        let mut allow_empty = profile;
        allow_empty.allow_empty_signature = true;
        let context = ctx(&allow_empty);
        let converted = convert_messages(
            &turns(vec![durable(assistant(
                vec![ContentBlock::Known(KnownContentBlock::Reasoning {
                    text: "hmm".to_owned(),
                })],
                None,
            ))]),
            &context,
        );
        assert_eq!(converted[0]["content"][0]["type"], "thinking");
        assert_eq!(converted[0]["content"][0]["signature"], "");

        let redacted = replay_envelope(&serde_json::json!([
            { "type": "thinking", "signature": "opaque", "redacted": true },
        ]));
        let context = ctx(&profile);
        let converted = convert_messages(
            &turns(vec![durable(assistant(
                vec![ContentBlock::Known(KnownContentBlock::Reasoning {
                    text: "[Reasoning redacted]".to_owned(),
                })],
                Some(redacted),
            ))]),
            &context,
        );
        assert_eq!(converted[0]["content"][0]["type"], "redacted_thinking");
        assert_eq!(converted[0]["content"][0]["data"], "opaque");
    }

    /// The stealth name map: send-side casing up, receive-side casing
    /// back down to the declared name.
    #[test]
    fn claude_code_name_map_round_trips() {
        assert_eq!(to_claude_code_name("bash").as_ref(), "Bash");
        assert_eq!(to_claude_code_name("todoWrite").as_ref(), "TodoWrite");
        assert_eq!(to_claude_code_name("custom").as_ref(), "custom");
        let tools = vec![ToolSchema {
            defer_loading: None,
            name: "bash".to_owned(),
            description: String::new(),
            parameters: RawObject::new(),
        }];
        assert_eq!(from_claude_code_name("Bash", &tools).as_ref(), "bash");
        assert_eq!(from_claude_code_name("Other", &tools).as_ref(), "Other");
    }

    /// `convert_tools`: legacy schema normalization, the eager flag, and
    /// the last-tool cache stamp under its compat switch.
    #[test]
    fn converts_tools_with_legacy_schema() {
        let tools = vec![
            ToolSchema {
                defer_loading: None,
                name: "sh".to_owned(),
                description: "runs".to_owned(),
                parameters: RawObject::from_iter([(
                    "type".to_owned(),
                    Value::String("object".to_owned()),
                )]),
            },
            ToolSchema {
                defer_loading: None,
                name: "bash".to_owned(),
                description: "shell".to_owned(),
                parameters: RawObject::new(),
            },
        ];
        let profile = compat();
        let context = ctx(&profile);
        let cache = serde_json::json!({ "type": "ephemeral", "ttl": "1h" });
        let converted = convert_tools(&tools, &context, Some(&cache));
        assert_eq!(
            converted[0]["input_schema"]["properties"],
            serde_json::json!({})
        );
        assert_eq!(
            converted[0]["input_schema"]["required"],
            serde_json::json!([])
        );
        assert_eq!(converted[0]["eager_input_streaming"], true);
        assert!(converted[0].get("cache_control").is_none());
        assert_eq!(converted[1]["cache_control"], cache);
        let mut no_eager = profile;
        no_eager.supports_eager_tool_input_streaming = false;
        no_eager.supports_cache_control_on_tools = false;
        let context = ctx(&no_eager);
        let converted = convert_tools(&tools, &context, Some(&cache));
        assert!(converted[0].get("eager_input_streaming").is_none());
        assert!(converted[1].get("cache_control").is_none());
    }

    /// Blank turns drop: an all-blank user turn skips and an assistant
    /// with no surviving blocks skips.
    #[test]
    fn blank_turns_drop() {
        let blank_user = KnownMessage::User {
            id: MessageId::new("u"),
            content: vec![ContentBlock::Known(KnownContentBlock::Text {
                text: "  ".to_owned(),
            })],
            source: MessageSource::Known(KnownMessageSource::User {
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        };
        let profile = compat();
        let context = ctx(&profile);
        let converted = convert_messages(
            &turns(vec![durable(blank_user), durable(user("real"))]),
            &context,
        );
        assert_eq!(converted.len(), 1);
        let converted = convert_messages(
            &turns(vec![durable(assistant(
                vec![ContentBlock::Known(KnownContentBlock::Text {
                    text: String::new(),
                })],
                None,
            ))]),
            &context,
        );
        assert_eq!(converted, [] as [Value; 0]);
    }
}
// Rust guideline compliant 2026-10-27
