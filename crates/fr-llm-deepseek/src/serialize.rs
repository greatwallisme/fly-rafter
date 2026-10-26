//! Map system snapshots, tool changes, and conversation turns to Messages
//! using the configured route capability (dsh `serialize.ts`).

use std::collections::{HashMap, HashSet};

use fr_llm::adapter::{GenerateOptions, RequestMessage};
use fr_llm::error::LlmError;
use fr_llm::projection::{ImageAttachmentAccess, request_image_handle_text};
use fr_wire::call_config::SystemPromptUpdate;
use fr_wire::message::{KnownMessage, Message, MessageSource};
use fr_wire::{
    AttachmentId, ContentBlock, ImageAttachmentRef, ImageMediaType, KnownContentBlock, RawObject,
    ToolCallId,
};
use serde_json::Value;

use crate::codes::{
    INVALID_REQUEST_CODE, UNSUPPORTED_CONTENT_CODE, UNSUPPORTED_REASONING_EFFORT_CODE,
};
use crate::file_id::DeepSeekFileId;
use crate::replay::{content_type_name, read_replay, static_error};
use crate::request_image::RequestImageAttachment;
use crate::types::{ConnectionOptions, EffortLevel, ThinkingMode};
use crate::wire_types::{
    ImageSource, OutputConfig, OutputEffort, ThinkingBlock, ThinkingConfig, ThinkingKind,
    ToolReference, ToolReferenceKind, ToolResultBlock, ToolUseBlock, WireBlock, WireInput,
    WireMessage, WireRequest, WireRole, WireTool,
};

/// One durable message classified by the role serialize dispatches on; the
/// unknown-role forward-compat face classifies to `None` and is skipped
/// (dsh has no such role to mirror).
#[derive(Clone, Copy)]
enum Incoming<'a> {
    Developer(&'a [ContentBlock]),
    System(&'a [ContentBlock]),
    Assistant {
        source: &'a MessageSource,
        content: &'a [ContentBlock],
    },
    Tool {
        call: &'a ToolCallId,
        is_error: Option<bool>,
        content: &'a [ContentBlock],
    },
    User(&'a [ContentBlock]),
}

/// The four non-developer turns; the `Developer` classification carries its
/// own pending-update flow, so it never reaches this enum.
#[derive(Clone, Copy)]
enum Turn<'a> {
    System(&'a [ContentBlock]),
    Assistant {
        source: &'a MessageSource,
        content: &'a [ContentBlock],
    },
    Tool {
        call: &'a ToolCallId,
        is_error: Option<bool>,
        content: &'a [ContentBlock],
    },
    User(&'a [ContentBlock]),
}

impl Turn<'_> {
    const fn content(&self) -> &[ContentBlock] {
        match self {
            Self::System(content)
            | Self::User(content)
            | Self::Tool { content, .. }
            | Self::Assistant { content, .. } => content,
        }
    }
}

fn classify(message: &RequestMessage) -> Option<Incoming<'_>> {
    match message {
        RequestMessage::Durable(Message::Known(known)) => match known.as_ref() {
            KnownMessage::Developer { content, .. } => Some(Incoming::Developer(content)),
            KnownMessage::System { content, .. } => Some(Incoming::System(content)),
            KnownMessage::Assistant {
                source, content, ..
            } => Some(Incoming::Assistant { source, content }),
            KnownMessage::Tool {
                tool_call_id,
                is_error,
                content,
                ..
            } => Some(Incoming::Tool {
                call: tool_call_id,
                is_error: is_error.map(|_| true),
                content,
            }),
            KnownMessage::User { content, .. } => Some(Incoming::User(content)),
        },
        RequestMessage::UserInput { content } => Some(Incoming::User(content)),
        RequestMessage::Durable(Message::Other(_)) => None,
    }
}

fn unsupported(kind: &str) -> LlmError {
    static_error(
        &format!("DeepSeek Messages cannot represent {kind}"),
        UNSUPPORTED_CONTENT_CODE,
    )
}

/// Historical arguments that Messages cannot represent use empty input;
/// durable content stays unchanged (dsh `toolInput`).
fn tool_input(raw: &str) -> Value {
    match serde_json::from_str::<Value>(raw) {
        Ok(value @ Value::Object(_)) => value,
        _ => Value::Object(RawObject::new()),
    }
}

/// The wire media-type spelling of a normalized attachment.
const fn media_type_wire(media_type: ImageMediaType) -> &'static str {
    match media_type {
        ImageMediaType::Png => "image/png",
        ImageMediaType::Jpeg => "image/jpeg",
        ImageMediaType::Webp => "image/webp",
        ImageMediaType::Gif => "image/gif",
    }
}

/// RFC 4648 base64 with padding (dsh `Buffer.toString('base64')`); the
/// alphabet is fully standardized, so no dependency is spent on it (Q65-A).
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        // `chunks(3)` never yields an empty slice, so byte 0 is real; the
        // missing tail bytes are zero, exactly like the padded encoding.
        let byte = |index: usize| u32::from(chunk.get(index).copied().unwrap_or(0));
        let word = (byte(0) << 16) | (byte(1) << 8) | byte(2);
        let pick = |shift: u32| -> char {
            let sextet = (word >> shift) & 63;
            char::from(ALPHABET[usize::try_from(sextet).expect("a six-bit value fits every usize")])
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

/// The assistant projection (dsh `assistant()`): empty text drops, reasoning
/// reattaches the replay signature of its source block, tool calls map to
/// `tool_use` with an empty input when history JSON is unrepresentable.
fn assistant_blocks<'p, 'd>(
    source: &MessageSource,
    content: &[ContentBlock],
    model: &str,
    on_replay_degrade: Option<&'p mut (dyn FnMut(&str) + 'd)>,
) -> Result<Vec<WireBlock>, LlmError> {
    let replay = read_replay(source, content, model, on_replay_degrade)?;
    let mut blocks = Vec::new();
    for (index, block) in content.iter().enumerate() {
        match block {
            ContentBlock::Known(KnownContentBlock::Text { text }) => {
                if !text.is_empty() {
                    blocks.push(WireBlock::Text { text: text.clone() });
                }
            }
            ContentBlock::Known(KnownContentBlock::Reasoning { text }) => {
                let signature = replay
                    .as_ref()
                    .and_then(|replay| replay.get(index))
                    .and_then(|block| block.signature.clone());
                blocks.push(WireBlock::Thinking(ThinkingBlock {
                    thinking: text.clone(),
                    signature,
                }));
            }
            ContentBlock::Known(KnownContentBlock::ToolCall {
                id,
                name,
                arguments,
            }) => {
                blocks.push(WireBlock::ToolUse(ToolUseBlock {
                    id: id.as_str().to_owned(),
                    name: name.clone(),
                    input: tool_input(arguments),
                }));
            }
            other => {
                return Err(unsupported(&format!(
                    "assistant content {}",
                    content_type_name(other)
                )));
            }
        }
    }
    Ok(blocks)
}

/// The user/tool-result projection (dsh `input()`): text survives, history
/// reasoning and tool calls drop, one image becomes its handle text plus a
/// base64 or Files-referenced image block.
fn input_block<S: ::std::hash::BuildHasher>(
    block: &ContentBlock,
    images: &HashMap<AttachmentId, RequestImageAttachment, S>,
    file_ids: Option<&HashMap<AttachmentId, DeepSeekFileId, S>>,
    access: &dyn Fn(&ImageAttachmentRef) -> Option<ImageAttachmentAccess>,
) -> Result<Vec<WireInput>, LlmError> {
    match block {
        ContentBlock::Known(KnownContentBlock::Text { text }) => {
            if text.is_empty() {
                Ok(Vec::new())
            } else {
                Ok(vec![WireInput::Text { text: text.clone() }])
            }
        }
        ContentBlock::Known(
            KnownContentBlock::Reasoning { .. } | KnownContentBlock::ToolCall { .. },
        ) => Ok(Vec::new()),
        ContentBlock::Known(KnownContentBlock::Image { attachment, .. }) => {
            let Some(version) = images.get(&attachment.attachment_id) else {
                return Err(static_error(
                    "DeepSeek Messages request image is missing",
                    INVALID_REQUEST_CODE,
                ));
            };
            let file_id = file_ids.and_then(|ids| ids.get(&attachment.attachment_id));
            if file_ids.is_some() && file_id.is_none() {
                return Err(static_error(
                    "DeepSeek Messages request file id is missing",
                    INVALID_REQUEST_CODE,
                ));
            }
            let text = request_image_handle_text(
                attachment,
                (version.width, version.height),
                access(attachment).as_ref(),
            );
            let source = file_id.map_or_else(
                || ImageSource::Base64 {
                    media_type: media_type_wire(version.media_type).to_owned(),
                    data: base64_encode(&version.data),
                },
                |file_id| ImageSource::File {
                    file_id: file_id.as_str().to_owned(),
                },
            );
            Ok(vec![WireInput::Text { text }, WireInput::Image { source }])
        }
        other => Err(unsupported(&format!(
            "user/tool-result content {}",
            content_type_name(other)
        ))),
    }
}

/// Serialize one complete request using already prepared image bytes (dsh
/// `serialize`).
///
/// User and tool-result content omits reasoning and tool-call blocks; empty
/// assistant text blocks and empty user/assistant messages are skipped;
/// empty tool results retain their call ids; developer messages already
/// projected for this route become system-role updates after the preceding
/// user turn.
///
/// # Errors
/// [`LlmError`] with `UNSUPPORTED_CONTENT` for unrepresentable blocks and
/// system updates without a user turn, and `INVALID_REQUEST` for a missing
/// request image, a broken tool pairing, or an unsupported effort chain.
pub fn serialize<S: ::std::hash::BuildHasher>(
    options: &GenerateOptions,
    connection: &ConnectionOptions,
    history: &[RequestMessage],
    images: &HashMap<AttachmentId, RequestImageAttachment, S>,
    access: &dyn Fn(&ImageAttachmentRef) -> Option<ImageAttachmentAccess>,
    mut on_replay_degrade: Option<&mut dyn FnMut(&str)>,
    file_ids: Option<&HashMap<AttachmentId, DeepSeekFileId, S>>,
) -> Result<WireRequest, LlmError> {
    let model = connection.model(&options.model);
    let in_history = model
        .is_some_and(|entry| entry.system_prompt_update == Some(SystemPromptUpdate::InHistory));
    let mut messages: Vec<WireMessage> = Vec::new();
    let mut history_system: Option<String> = None;
    // Harness admits system updates before user input. Messages places the
    // same update after that user/tool-result turn and before the next
    // assistant.
    let mut system_updates: Vec<WireMessage> = Vec::new();
    for message in history {
        let Some(incoming) = classify(message) else {
            continue;
        };
        serialize_turn(
            incoming,
            options,
            &mut messages,
            &mut history_system,
            &mut system_updates,
            in_history,
            images,
            access,
            &mut on_replay_degrade,
            file_ids,
        )?;
    }
    flush_system_updates(&mut messages, &mut system_updates)?;
    validate_pairing(&mut messages)?;
    let effort = resolve_effort(options, connection)?;
    let snapshot = history_system.take();
    Ok(assemble_request(
        options,
        connection,
        model,
        messages,
        effort,
        snapshot.as_deref(),
    ))
}

/// Serialize one non-developer turn into `messages` (dsh loop body): the
/// tool-change guard, the in-history system update arm, the three content
/// projections, empty-message skips, the flush before assistants, and the
/// same-role merge.
#[expect(
    clippy::too_many_arguments,
    reason = "the dsh closure environment (options, images, access, file_ids, degrade hook) passes as explicit parameters"
)]
fn serialize_turn<'d, S: ::std::hash::BuildHasher>(
    incoming: Incoming<'_>,
    options: &GenerateOptions,
    messages: &mut Vec<WireMessage>,
    history_system: &mut Option<String>,
    system_updates: &mut Vec<WireMessage>,
    in_history: bool,
    images: &HashMap<AttachmentId, RequestImageAttachment, S>,
    access: &dyn Fn(&ImageAttachmentRef) -> Option<ImageAttachmentAccess>,
    on_replay_degrade: &mut Option<&'d mut dyn FnMut(&str)>,
    file_ids: Option<&HashMap<AttachmentId, DeepSeekFileId, S>>,
) -> Result<(), LlmError> {
    // Developer messages become pending system-role updates, appended when
    // the next assistant turn lands (dsh developer arm).
    let turn = match incoming {
        Incoming::Developer(content) => {
            append_developer_update(content, system_updates)?;
            return Ok(());
        }
        Incoming::System(content) => Turn::System(content),
        Incoming::Assistant { source, content } => Turn::Assistant { source, content },
        Incoming::Tool {
            call,
            is_error,
            content,
        } => Turn::Tool {
            call,
            is_error,
            content,
        },
        Incoming::User(content) => Turn::User(content),
    };
    let content = turn.content();
    if content.iter().any(|block| {
        matches!(
            block,
            ContentBlock::Known(
                KnownContentBlock::ToolAddition { .. } | KnownContentBlock::ToolRemoval { .. }
            )
        )
    }) {
        return Err(unsupported("tool-change blocks outside developer messages"));
    }
    let (wire_content, wire_role, skippable) = match turn {
        Turn::System(blocks) => {
            let mut text = String::new();
            for block in blocks {
                let ContentBlock::Known(KnownContentBlock::Text { text: piece }) = block else {
                    return Err(unsupported("non-text system message"));
                };
                text.push_str(piece);
            }
            if in_history && !messages.is_empty() {
                if text.is_empty() {
                    return Err(unsupported("empty in-history system update"));
                }
                system_updates.push(WireMessage {
                    role: WireRole::System,
                    content: vec![WireBlock::Text { text }],
                });
            } else {
                *history_system = Some(text);
            }
            return Ok(());
        }
        Turn::Assistant { source, content } => {
            let projected = assistant_blocks(
                source,
                content,
                &options.model,
                on_replay_degrade.as_deref_mut(),
            )?;
            (projected, WireRole::Assistant, true)
        }
        Turn::Tool {
            call,
            is_error,
            content,
        } => {
            let projected = vec![WireBlock::ToolResult(ToolResultBlock {
                tool_use_id: call.as_str().to_owned(),
                content: project_input(content, images, access, file_ids)?,
                is_error,
            })];
            (projected, WireRole::User, false)
        }
        Turn::User(content) => {
            let projected: Vec<WireBlock> = project_input(content, images, access, file_ids)?
                .into_iter()
                .map(input_into_block)
                .collect();
            (projected, WireRole::User, true)
        }
    };
    if skippable && wire_content.is_empty() {
        return Ok(());
    }
    if wire_role == WireRole::Assistant {
        flush_system_updates(messages, system_updates)?;
    }
    match messages
        .last_mut()
        .filter(|previous| previous.role == wire_role)
    {
        Some(previous) => previous.content.extend(wire_content),
        None => messages.push(WireMessage {
            role: wire_role,
            content: wire_content,
        }),
    }
    Ok(())
}

/// Lifts one projected user input into a conversation block (dsh's
/// `WireInput` is structurally a `WireBlock` subset; fr separates them).
fn input_into_block(input: WireInput) -> WireBlock {
    match input {
        WireInput::Text { text } => WireBlock::Text { text },
        WireInput::Image { source } => WireBlock::Image { source },
    }
}

/// Project one developer message into a pending system-role update (dsh
/// developer arm): empty text drops, tool changes become tool references.
fn append_developer_update(
    content: &[ContentBlock],
    system_updates: &mut Vec<WireMessage>,
) -> Result<(), LlmError> {
    let mut update = Vec::new();
    for block in content {
        match block {
            ContentBlock::Known(KnownContentBlock::Text { text }) => {
                if !text.is_empty() {
                    update.push(WireBlock::Text { text: text.clone() });
                }
            }
            ContentBlock::Known(KnownContentBlock::ToolAddition { tool_name }) => {
                update.push(WireBlock::ToolAddition {
                    tool: ToolReference {
                        kind: ToolReferenceKind::ToolReference,
                        name: tool_name.clone(),
                    },
                });
            }
            ContentBlock::Known(KnownContentBlock::ToolRemoval { tool_name }) => {
                update.push(WireBlock::ToolRemoval {
                    tool: ToolReference {
                        kind: ToolReferenceKind::ToolReference,
                        name: tool_name.clone(),
                    },
                });
            }
            other => {
                return Err(unsupported(&format!(
                    "developer content {}",
                    content_type_name(other)
                )));
            }
        }
    }
    if !update.is_empty() {
        system_updates.push(WireMessage {
            role: WireRole::System,
            content: update,
        });
    }
    Ok(())
}

fn project_input<S: ::std::hash::BuildHasher>(
    content: &[ContentBlock],
    images: &HashMap<AttachmentId, RequestImageAttachment, S>,
    access: &dyn Fn(&ImageAttachmentRef) -> Option<ImageAttachmentAccess>,
    file_ids: Option<&HashMap<AttachmentId, DeepSeekFileId, S>>,
) -> Result<Vec<WireInput>, LlmError> {
    let mut projected = Vec::new();
    for block in content {
        projected.extend(input_block(block, images, file_ids, access)?);
    }
    Ok(projected)
}

/// Flush pending system updates: they may only land after a user turn (dsh
/// `flushSystemUpdates`).
fn flush_system_updates(
    messages: &mut Vec<WireMessage>,
    system_updates: &mut Vec<WireMessage>,
) -> Result<(), LlmError> {
    if system_updates.is_empty() {
        return Ok(());
    }
    if !messages
        .last()
        .is_some_and(|previous| previous.role == WireRole::User)
    {
        return Err(unsupported(
            "system update without a preceding user or tool-result turn",
        ));
    }
    messages.append(system_updates);
    Ok(())
}

/// The `tool_use`/`tool_result` pairing pass (dsh post-loop): every
/// assistant resets the pending set and rejects duplicate call ids, every
/// user turn must answer exactly the pending calls and is reordered
/// results-first, and the history must not end with unresolved calls.
fn validate_pairing(messages: &mut [WireMessage]) -> Result<(), LlmError> {
    let reject = |message: &'static str| static_error(message, INVALID_REQUEST_CODE);
    let mut pending: HashSet<String> = HashSet::new();
    for message in messages {
        match message.role {
            WireRole::Assistant => {
                let calls: Vec<&str> = message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        WireBlock::ToolUse(tool) => Some(tool.id.as_str()),
                        _ => None,
                    })
                    .collect();
                let unique: HashSet<&str> = calls.iter().copied().collect();
                if unique.len() != calls.len() {
                    return Err(reject("DeepSeek Messages duplicate tool call id"));
                }
                pending = unique.into_iter().map(str::to_owned).collect();
            }
            WireRole::User => {
                let mut results = Vec::new();
                let mut rest = Vec::new();
                for block in std::mem::take(&mut message.content) {
                    match block {
                        WireBlock::ToolResult(result) => {
                            if !pending.remove(&result.tool_use_id) {
                                return Err(reject(
                                    "DeepSeek Messages tool result has no matching call",
                                ));
                            }
                            results.push(WireBlock::ToolResult(result));
                        }
                        other => rest.push(other),
                    }
                }
                if !pending.is_empty() {
                    return Err(reject(
                        "DeepSeek Messages tool calls need immediate results",
                    ));
                }
                results.extend(rest);
                message.content = results;
            }
            WireRole::System => {}
        }
    }
    if !pending.is_empty() {
        return Err(reject(
            "DeepSeek Messages history ends with unresolved tools",
        ));
    }
    Ok(())
}

/// The effort chain (dsh): the request effort, else the connection default,
/// else the thinking-mode default; a value outside the protocol set, or any
/// non-`off` effort while thinking is disabled, is rejected.
fn resolve_effort(
    options: &GenerateOptions,
    connection: &ConnectionOptions,
) -> Result<Option<EffortLevel>, LlmError> {
    let effort = options
        .reasoning_effort
        .as_ref()
        .map(|id| id.as_str().to_owned())
        .or_else(|| {
            connection
                .defaults
                .reasoning_effort
                .map(|level| level.as_str().to_owned())
        })
        .unwrap_or_else(|| {
            match connection.defaults.thinking {
                Some(ThinkingMode::Disabled) => "off",
                _ => "high",
            }
            .to_owned()
        });
    let parsed = match effort.as_str() {
        "off" => Some(EffortLevel::Off),
        "low" => Some(EffortLevel::Low),
        "high" => Some(EffortLevel::High),
        "max" => Some(EffortLevel::Max),
        _ => None,
    };
    if parsed.is_none()
        || (connection.defaults.thinking == Some(ThinkingMode::Disabled)
            && parsed != Some(EffortLevel::Off))
    {
        return Err(static_error(
            &format!("DeepSeek Messages does not support reasoning effort {effort}"),
            UNSUPPORTED_REASONING_EFFORT_CODE,
        ));
    }
    Ok(parsed)
}

/// Assemble the wire body (dsh return object): explicit request values win
/// over the model entry, which wins over the connection cap; the effort
/// toggle and `output_config` mirror the resolved level; the system prompt
/// joins the one-shot caller prompt and the leading snapshot.
fn assemble_request(
    options: &GenerateOptions,
    connection: &ConnectionOptions,
    model: Option<&crate::models::CatalogModel>,
    messages: Vec<WireMessage>,
    effort: Option<EffortLevel>,
    history_system: Option<&str>,
) -> WireRequest {
    let system = [options.system.as_deref(), history_system]
        .into_iter()
        .flatten()
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let off = effort == Some(EffortLevel::Off);
    let output_config = effort.and_then(|level| match level {
        EffortLevel::Off => None,
        EffortLevel::Low => Some(OutputEffort::Low),
        EffortLevel::High => Some(OutputEffort::High),
        EffortLevel::Max => Some(OutputEffort::Max),
    });
    WireRequest {
        model: options.model.clone(),
        stream: true,
        max_tokens: options
            .max_tokens
            .or_else(|| model.and_then(|entry| entry.max_tokens))
            .unwrap_or(connection.max_tokens),
        messages,
        system: if system.is_empty() {
            None
        } else {
            Some(system)
        },
        thinking: ThinkingConfig {
            kind: if off {
                ThinkingKind::Disabled
            } else {
                ThinkingKind::Enabled
            },
        },
        output_config: output_config.map(|effort| OutputConfig { effort }),
        temperature: options.temperature,
        stop_sequences: options.stop.clone(),
        tools: options.tools.as_ref().map(|tools| {
            tools
                .iter()
                .map(|tool| WireTool {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    input_schema: Value::Object(tool.parameters.clone()),
                    defer_loading: tool.defer_loading.map(|_| true),
                })
                .collect()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::CatalogModel;
    use crate::types::{FilePolicy, RequestDefaults};
    use fr_llm::retry_policy::{Backoff, RetryPolicy};
    use fr_wire::TrueOnly;
    use fr_wire::call_config::ToolSchema;
    use fr_wire::ids::{MessageId, ReasoningEffortId};
    use fr_wire::message::KnownMessageSource;
    use serde_json::json;

    fn options(model: &str, messages: Vec<RequestMessage>) -> GenerateOptions {
        GenerateOptions {
            provider: "deepseek-official".to_owned(),
            model: model.to_owned(),
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
        }
    }

    fn model_entry(id: &str, in_history: bool) -> CatalogModel {
        CatalogModel {
            id: id.to_owned(),
            name: None,
            description: None,
            context_window: None,
            max_tokens: Some(4_000),
            input_modalities: None,
            image_pixel_budget: None,
            image_max_bytes: None,
            system_prompt_update: in_history.then_some(SystemPromptUpdate::InHistory),
            tool_update: None,
        }
    }

    fn connection(models: Vec<CatalogModel>) -> ConnectionOptions {
        ConnectionOptions {
            base_url: "https://api.deepseek.com/anthropic".to_owned(),
            defaults: RequestDefaults::default(),
            max_tokens: 8_000,
            default_context_window: 128_000,
            models,
            stream_idle_timeout_ms: 300_000.0,
            max_request_files_bytes: 100 * 1024 * 1024,
            max_inline_request_image_bytes: 20 * 1024 * 1024,
            max_images_per_request: 5,
            image_offload_byte_quantum: 64 * 1024 * 1024,
            inline_image_offload_byte_quantum: 10 * 1024 * 1024,
            image_offload_count_quantum: 20,
            files_api_timeout_ms: 60_000.0,
            file_policy: FilePolicy {
                expires_after_seconds: 7 * 24 * 60 * 60,
                refresh_margin_seconds: 60 * 60,
                quota_cleanup_batch: 100,
            },
            retry_policy: RetryPolicy::Always {
                backoff: Backoff {
                    initial_delay_ms: 500,
                    max_delay_ms: 10_000,
                    jitter_ratio: 0.1,
                },
            },
        }
    }

    fn durable(known: KnownMessage) -> RequestMessage {
        RequestMessage::Durable(Message::Known(Box::new(known)))
    }

    fn id(number: usize) -> MessageId {
        MessageId::new(format!("msg-{number}"))
    }

    fn source_of(kind: KnownMessageSource) -> fr_wire::message::MessageSource {
        fr_wire::message::MessageSource::Known(kind)
    }

    fn text_block(text: &str) -> ContentBlock {
        ContentBlock::Known(KnownContentBlock::Text {
            text: text.to_owned(),
        })
    }

    fn system_msg(number: usize, text: &str) -> KnownMessage {
        KnownMessage::System {
            id: id(number),
            content: vec![text_block(text)],
            source: source_of(KnownMessageSource::SystemPrompt {
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        }
    }

    fn user_msg(number: usize, blocks: Vec<ContentBlock>) -> KnownMessage {
        KnownMessage::User {
            id: id(number),
            content: blocks,
            source: source_of(KnownMessageSource::User {
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        }
    }

    fn assistant_msg(
        number: usize,
        blocks: Vec<ContentBlock>,
        replay: Option<serde_json::Value>,
    ) -> KnownMessage {
        KnownMessage::Assistant {
            id: id(number),
            content: blocks,
            source: source_of(KnownMessageSource::Model {
                provider: "deepseek-official".to_owned(),
                model: "m".to_owned(),
                replay_state: replay,
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        }
    }

    fn tool_msg(number: usize, call: &str, text: &str) -> KnownMessage {
        KnownMessage::Tool {
            id: id(number),
            content: vec![text_block(text)],
            source: source_of(KnownMessageSource::Tool {
                call_id: ToolCallId::new(call),
                extra: RawObject::new(),
            }),
            tool_call_id: ToolCallId::new(call),
            is_error: None,
            extra: RawObject::new(),
        }
    }

    fn developer_msg(number: usize, tool: &str) -> KnownMessage {
        KnownMessage::Developer {
            id: id(number),
            content: vec![ContentBlock::Known(KnownContentBlock::ToolAddition {
                tool_name: tool.to_owned(),
            })],
            source: source_of(KnownMessageSource::User {
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        }
    }

    fn role_names(request: &WireRequest) -> Vec<&'static str> {
        request
            .messages
            .iter()
            .map(|message| match message.role {
                WireRole::User => "user",
                WireRole::Assistant => "assistant",
                WireRole::System => "system",
            })
            .collect()
    }

    fn no_access(_reference: &ImageAttachmentRef) -> Option<ImageAttachmentAccess> {
        None
    }

    /// Full conversation: a leading snapshot, a developer tool addition
    /// flushed before the assistant that follows, same-role merging of a
    /// user turn with a following tool result, the results-first reorder,
    /// and the replay signature reattached to reasoning history.
    #[test]
    fn serializes_history_with_updates_and_pairing() {
        let replay = json!({
            "response": {"kind": "deepseek-messages", "version": 1, "model": "m"},
            "blocks": [
                {"type": "reasoning", "signature": "s"},
                {"type": "tool-call"},
            ],
        });
        let history = vec![
            durable(system_msg(1, "be nice")),
            durable(user_msg(2, vec![text_block("run")])),
            durable(developer_msg(8, "sh")),
            durable(assistant_msg(
                3,
                vec![
                    ContentBlock::Known(KnownContentBlock::Reasoning {
                        text: "think".to_owned(),
                    }),
                    ContentBlock::Known(KnownContentBlock::ToolCall {
                        id: ToolCallId::new("c1"),
                        name: "sh".to_owned(),
                        arguments: "{}".to_owned(),
                    }),
                ],
                Some(replay),
            )),
            durable(user_msg(4, vec![text_block("hmm")])),
            durable(tool_msg(5, "c1", "ok")),
            durable(assistant_msg(6, vec![text_block("done")], None)),
            durable(user_msg(7, vec![text_block("thanks")])),
        ];
        let request = serialize(
            &options("m", Vec::new()),
            &connection(vec![model_entry("m", true)]),
            &history,
            &HashMap::new(),
            &no_access,
            None,
            None,
        )
        .expect("serializes");
        assert_eq!(
            role_names(&request),
            ["user", "system", "assistant", "user", "assistant", "user"]
        );
        // The leading snapshot rides the top-level system field.
        assert_eq!(request.system.as_deref(), Some("be nice"));
        // The developer update flushes after the first user turn.
        assert_eq!(
            request.messages[1].content,
            vec![WireBlock::ToolAddition {
                tool: ToolReference {
                    kind: ToolReferenceKind::ToolReference,
                    name: "sh".to_owned(),
                },
            }]
        );
        // Reasoning history carries its replay signature; the tool call
        // projects to tool_use with its parsed input.
        assert_eq!(
            request.messages[2].content[0],
            WireBlock::Thinking(ThinkingBlock {
                thinking: "think".to_owned(),
                signature: Some("s".to_owned()),
            })
        );
        assert_eq!(
            request.messages[2].content[1],
            WireBlock::ToolUse(ToolUseBlock {
                id: "c1".to_owned(),
                name: "sh".to_owned(),
                input: json!({}),
            })
        );
        // The merged user turn is reordered results-first.
        assert_eq!(
            request.messages[3].content,
            vec![
                WireBlock::ToolResult(ToolResultBlock {
                    tool_use_id: "c1".to_owned(),
                    content: vec![WireInput::Text {
                        text: "ok".to_owned()
                    }],
                    is_error: None,
                }),
                WireBlock::Text {
                    text: "hmm".to_owned()
                },
            ]
        );
        // Defaults: effort high, thinking enabled, model cap wins over the
        // connection cap.
        assert_eq!(request.max_tokens, 4_000);
        assert_eq!(request.thinking.kind, ThinkingKind::Enabled);
        assert_eq!(
            request.output_config,
            Some(OutputConfig {
                effort: OutputEffort::High
            })
        );
    }

    #[test]
    fn rejects_broken_tool_pairing() {
        let run = |history: Vec<RequestMessage>| {
            serialize(
                &options("m", Vec::new()),
                &connection(vec![model_entry("m", false)]),
                &history,
                &HashMap::new(),
                &no_access,
                None,
                None,
            )
            .expect_err("pairing rejects")
            .message()
            .to_owned()
        };
        let call = |call_id: &str| {
            ContentBlock::Known(KnownContentBlock::ToolCall {
                id: ToolCallId::new(call_id),
                name: "sh".to_owned(),
                arguments: "{}".to_owned(),
            })
        };
        // Two calls share one id.
        let message = assistant_msg(1, vec![call("c1"), call("c1")], None);
        assert_eq!(
            run(vec![durable(message)]),
            "DeepSeek Messages duplicate tool call id"
        );
        // The result answers a call that was never made.
        let history = vec![
            durable(assistant_msg(1, vec![call("c1")], None)),
            durable(tool_msg(2, "c2", "ok")),
        ];
        assert_eq!(
            run(history),
            "DeepSeek Messages tool result has no matching call"
        );
        // A user turn between the call and its result clears the pending set.
        let history = vec![
            durable(assistant_msg(1, vec![call("c1")], None)),
            durable(user_msg(2, vec![text_block("still waiting")])),
        ];
        assert_eq!(
            run(history),
            "DeepSeek Messages tool calls need immediate results"
        );
        // The history ends with the call unanswered.
        let history = vec![durable(assistant_msg(1, vec![call("c1")], None))];
        assert_eq!(
            run(history),
            "DeepSeek Messages history ends with unresolved tools"
        );
    }

    /// The degrade hook observes unusable replay metadata without any
    /// signature content, and the request controls (caller prompt, tools
    /// with deferred loading, sampling, effort) ride the wire body.
    #[test]
    fn projects_replay_degrade_and_request_controls() {
        let mut degraded = Vec::new();
        let corrupt = json!({
            "response": {"kind": "deepseek-messages", "version": 7, "model": "m"},
            "blocks": [],
        });
        let history = vec![
            durable(system_msg(1, "be nice")),
            durable(assistant_msg(
                2,
                vec![ContentBlock::Known(KnownContentBlock::Reasoning {
                    text: "think".to_owned(),
                })],
                Some(corrupt),
            )),
        ];
        let mut request_options = options("m", history);
        request_options.system = Some("caller".to_owned());
        request_options.reasoning_effort = Some(ReasoningEffortId::new("low"));
        request_options.temperature = Some(0.5);
        request_options.max_tokens = Some(111);
        request_options.stop = Some(vec!["END".to_owned()]);
        request_options.tools = Some(vec![ToolSchema {
            defer_loading: Some(TrueOnly),
            name: "sh".to_owned(),
            description: "runs".to_owned(),
            parameters: RawObject::from_iter([(
                "type".to_owned(),
                Value::String("object".to_owned()),
            )]),
        }]);
        let request = serialize(
            &request_options,
            &connection(vec![model_entry("m", true)]),
            &request_options.messages,
            &HashMap::new(),
            &no_access,
            Some(&mut |reason: &str| degraded.push(reason.to_owned())),
            None,
        )
        .expect("serializes despite degraded replay");
        assert_eq!(
            degraded,
            vec!["DeepSeek Messages replay: unsupported kind or version"]
        );
        assert_eq!(request.system.as_deref(), Some("caller\n\nbe nice"));
        assert_eq!(request.max_tokens, 111);
        assert_eq!(request.temperature, Some(0.5));
        assert_eq!(
            request.stop_sequences.as_deref(),
            Some(&["END".to_owned()][..])
        );
        assert_eq!(
            request.output_config,
            Some(OutputConfig {
                effort: OutputEffort::Low
            })
        );
        let tools = request.tools.as_deref().unwrap_or_default();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].defer_loading, Some(true));
        assert_eq!(tools[0].input_schema, json!({"type": "object"}));
        // Degraded history drops the signature but keeps the reasoning.
        assert_eq!(
            request.messages[0].content[0],
            WireBlock::Thinking(ThinkingBlock {
                thinking: "think".to_owned(),
                signature: None,
            })
        );
    }

    #[test]
    fn rejects_unsupported_effort_and_content() {
        let run = |request_options: &GenerateOptions, models: Vec<CatalogModel>| {
            serialize(
                request_options,
                &connection(models),
                request_options.messages.as_slice(),
                &HashMap::new(),
                &no_access,
                None,
                None,
            )
            .expect_err("unsupported rejects")
        };
        // An effort outside the protocol set, from the request.
        let mut request_options = options("m", Vec::new());
        request_options.reasoning_effort = Some(ReasoningEffortId::new("bogus"));
        let error = run(&request_options, vec![model_entry("m", false)]);
        assert_eq!(error.code(), UNSUPPORTED_REASONING_EFFORT_CODE);
        assert_eq!(
            error.message(),
            "DeepSeek Messages does not support reasoning effort bogus"
        );
        // Thinking disabled permits effort off only.
        let mut disabled = connection(vec![model_entry("m", false)]);
        disabled.defaults.thinking = Some(ThinkingMode::Disabled);
        disabled.defaults.reasoning_effort = Some(EffortLevel::Low);
        let error = serialize(
            &options("m", Vec::new()),
            &disabled,
            &[],
            &HashMap::new(),
            &no_access,
            None,
            None,
        )
        .expect_err("disabled thinking");
        assert_eq!(
            error.message(),
            "DeepSeek Messages does not support reasoning effort low"
        );

        // Tool-change blocks outside developer messages.
        let history = vec![durable(user_msg(
            1,
            vec![ContentBlock::Known(KnownContentBlock::ToolAddition {
                tool_name: "sh".to_owned(),
            })],
        ))];
        let error = serialize(
            &options("m", Vec::new()),
            &connection(vec![model_entry("m", false)]),
            &history,
            &HashMap::new(),
            &no_access,
            None,
            None,
        )
        .expect_err("tool change outside developer");
        assert_eq!(
            error.message(),
            "DeepSeek Messages cannot represent tool-change blocks outside developer messages"
        );

        // A non-text system message.
        let history = vec![durable(KnownMessage::System {
            id: id(1),
            content: vec![ContentBlock::Other(
                json!({"type": "odd"}).as_object().expect("object").clone(),
            )],
            source: source_of(KnownMessageSource::SystemPrompt {
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        })];
        let error = serialize(
            &options("m", Vec::new()),
            &connection(vec![model_entry("m", false)]),
            &history,
            &HashMap::new(),
            &no_access,
            None,
            None,
        )
        .expect_err("non-text system");
        assert_eq!(
            error.message(),
            "DeepSeek Messages cannot represent non-text system message"
        );

        // An empty in-history system update after a user turn.
        let history = vec![
            durable(user_msg(1, vec![text_block("run")])),
            durable(system_msg(2, "")),
        ];
        let error = serialize(
            &options("m", Vec::new()),
            &connection(vec![model_entry("m", true)]),
            &history,
            &HashMap::new(),
            &no_access,
            None,
            None,
        )
        .expect_err("empty in-history update");
        assert_eq!(
            error.message(),
            "DeepSeek Messages cannot represent empty in-history system update"
        );
    }

    fn image_fixtures() -> (
        Vec<RequestMessage>,
        HashMap<AttachmentId, RequestImageAttachment>,
    ) {
        let attachment = ImageAttachmentRef {
            attachment_id: AttachmentId::new("att"),
            media_type: ImageMediaType::Png,
            bytes: 4,
            width: 2,
            height: 2,
            name: None,
            original_dimensions: None,
        };
        let version = RequestImageAttachment {
            variant_id: "v1".to_owned(),
            attachment: attachment.clone(),
            data: vec![1, 2, 3, 4],
            media_type: ImageMediaType::Png,
            bytes: 4,
            width: 2,
            height: 2,
            depth: crate::request_image::ImageSampleDepth::Uchar,
            space: crate::request_image::ImageColorSpace::Srgb,
            has_alpha: false,
        };
        let mut images = HashMap::new();
        images.insert(AttachmentId::new("att"), version);
        let history = vec![durable(user_msg(
            1,
            vec![ContentBlock::Known(KnownContentBlock::Image {
                attachment,
                offloaded: None,
            })],
        ))];
        (history, images)
    }

    #[test]
    fn serializes_image_inputs() {
        let (history, images) = image_fixtures();
        // Inline bytes: handle text plus a base64 image block.
        let request = serialize(
            &options("m", Vec::new()),
            &connection(vec![model_entry("m", false)]),
            &history,
            &images,
            &no_access,
            None,
            None,
        )
        .expect("inline image");
        assert_eq!(
            request.messages[0].content,
            vec![
                WireBlock::Text {
                    text: "Image att; request preview 2x2px. It may be resized or re-encoded; \
                           source dimensions, format, and byte size may differ."
                        .to_owned(),
                },
                WireBlock::Image {
                    source: ImageSource::Base64 {
                        media_type: "image/png".to_owned(),
                        data: "AQIDBA==".to_owned(),
                    },
                },
            ]
        );
        // Files references swap the inline bytes for the file id.
        let mut file_ids = HashMap::new();
        file_ids.insert(AttachmentId::new("att"), DeepSeekFileId::new("file-1"));
        let request = serialize(
            &options("m", Vec::new()),
            &connection(vec![model_entry("m", false)]),
            &history,
            &images,
            &no_access,
            None,
            Some(&file_ids),
        )
        .expect("file image");
        assert_eq!(
            request.messages[0].content[1],
            WireBlock::Image {
                source: ImageSource::File {
                    file_id: "file-1".to_owned(),
                },
            }
        );
        // The map is provided but the image is not in it.
        let error = serialize(
            &options("m", Vec::new()),
            &connection(vec![model_entry("m", false)]),
            &history,
            &images,
            &no_access,
            None,
            Some(&HashMap::new()),
        )
        .expect_err("file id missing");
        assert_eq!(
            error.message(),
            "DeepSeek Messages request file id is missing"
        );
        // No prepared version at all.
        let error = serialize(
            &options("m", Vec::new()),
            &connection(vec![model_entry("m", false)]),
            &history,
            &HashMap::new(),
            &no_access,
            None,
            None,
        )
        .expect_err("version missing");
        assert_eq!(
            error.message(),
            "DeepSeek Messages request image is missing"
        );
    }

    /// RFC 4648 test vectors for the dependency-free encoder.
    #[test]
    fn base64_encode_pins_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }
}

// Rust guideline compliant 2026-10-26
