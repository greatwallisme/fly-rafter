//! History transformation before protocol conversion (pi-ai
//! `api/transform-messages.ts`).
//!
//! fr vocabulary adaptations: assistant `stopReason` (the errored/aborted
//! skip arm) and `thoughtSignature` have no durable face here — the
//! settlement layer already keeps failed runs out of the replayed history,
//! so those arms are unreachable (fr-llm-deepseek `serialize.rs` same
//! ruling); system/developer messages never reach this pass because the
//! Q74-B fold removed them, so the held-system transparency logic is dead;
//! `ToolResultMessage.toolName` has no fr field, so the
//! `requiresToolResultName` arm reads nothing and stays dead.

use std::collections::HashMap;

use fr_llm::adapter::RequestMessage;
use fr_wire::ids::ToolCallId;
use fr_wire::message::{KnownMessage, KnownMessageSource, Message, MessageSource};
use fr_wire::{ContentBlock, KnownContentBlock, TrueOnly};

use crate::hash::short_hash;

/// The non-vision placeholders (transform-messages.ts:12-13).
const NON_VISION_USER_IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";
const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str =
    "(tool image omitted: model does not support images)";

/// One transformed conversation message (owned; block-level edits below).
#[derive(Debug)]
pub enum Turn {
    /// A user turn (images possibly downgraded to placeholders).
    User {
        /// The transformed content.
        content: Vec<ContentBlock>,
    },
    /// An assistant turn (thinking downgraded cross-model, ids normalized).
    Assistant {
        /// The producer envelope, replay signature source.
        source: MessageSource,
        /// The transformed content.
        content: Vec<ContentBlock>,
    },
    /// A tool result (call id possibly normalized via the assistant map).
    Tool {
        /// The (possibly normalized) call id.
        call_id: ToolCallId,
        /// The error flag, preserved.
        is_error: Option<TrueOnly>,
        /// The transformed content.
        content: Vec<ContentBlock>,
    },
}

/// The route identity for the same-model checks.
#[derive(Debug, Clone, Copy)]
pub struct RouteIdentity<'a> {
    /// The provider route key.
    pub provider: &'a str,
    /// The model id.
    pub model: &'a str,
}

fn replace_images_with_placeholder(
    content: &[ContentBlock],
    placeholder: &str,
) -> Vec<ContentBlock> {
    let mut result = Vec::with_capacity(content.len());
    let mut previous_was_placeholder = false;
    for block in content {
        if matches!(block, ContentBlock::Known(KnownContentBlock::Image { .. })) {
            if !previous_was_placeholder {
                result.push(ContentBlock::Known(KnownContentBlock::Text {
                    text: placeholder.to_owned(),
                }));
            }
            previous_was_placeholder = true;
            continue;
        }
        previous_was_placeholder = matches!(
            block,
            ContentBlock::Known(KnownContentBlock::Text { text })
                if text == placeholder
        );
        result.push(block.clone());
    }
    result
}

/// The tool-call id normalizer for Responses-pipeline ids
/// (`normalizeToolCallId`, openai-completions.ts:1202-1226).
///
/// Pipe-separated `{call_id}|{item_id}` ids are sanitized, joined, and
/// hash-shortened over the 40-character `OpenAI` limit; plain ids truncate
/// only for the `openai` provider.
#[must_use]
pub fn normalize_tool_call_id(id: &str, provider: &str) -> String {
    if let Some(separator) = id.find('|') {
        let sanitize = |text: &str| -> String {
            text.chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect()
        };
        let call_id = sanitize(&id[..separator]);
        let item_id = sanitize(&id[separator + 1..]);
        let combined = if item_id.is_empty() {
            call_id.clone()
        } else {
            format!("{call_id}_{item_id}")
        };
        if combined.chars().count() <= 40 {
            return combined;
        }
        let hash = short_hash(id)[..8].to_owned();
        let keep = 40usize
            .saturating_sub(hash.chars().count())
            .saturating_sub(1);
        let prefix_len = keep.max(1);
        let prefix: String = call_id.chars().take(prefix_len).collect();
        return format!("{prefix}_{hash}");
    }
    if provider == "openai" {
        return id.chars().take(40).collect();
    }
    id.to_owned()
}

/// The tool-call id normalization policy (dsh `transformMessages`
/// receives the calling pipeline's `normalizeToolCallId`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdNormalization {
    /// The Chat Completions pipeline variant (40-character `OpenAI` limit,
    /// `shortHash` suffix over the combined id).
    Completions,
    /// The Responses pipeline variant (64-character limit, `fc_` item-id
    /// prefix, hashed ids for foreign tool calls).
    Responses,
}

/// The providers whose Responses tool-call ids carry the
/// `{call_id}|{item_id}` pipe pair (pi-ai `OPENAI_TOOL_CALL_PROVIDERS`,
/// openai-responses.ts:31, verbatim).
const RESPONSES_TOOL_CALL_PROVIDERS: [&str; 3] = ["openai", "openai-codex", "opencode"];

/// One Responses id part (`normalizeIdPart`, responses-shared.ts:154-158):
/// every character outside `[a-zA-Z0-9_-]` becomes `_`, the part caps at
/// 64 characters, and trailing underscores strip.
#[must_use]
fn normalize_id_part(part: &str) -> String {
    let sanitized: String = part
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let capped: String = sanitized.chars().take(64).collect();
    capped.trim_end_matches('_').to_owned()
}

/// The synthetic item id for a tool call produced by another provider
/// (`buildForeignResponsesItemId`): `fc_<shortHash>`, capped at 64
/// characters without the trailing-underscore strip.
#[must_use]
fn build_foreign_responses_item_id(item_id: &str) -> String {
    let normalized = format!("fc_{}", short_hash(item_id));
    normalized.chars().take(64).collect()
}

/// The Responses pipeline's tool-call id normalizer
/// (`normalizeToolCallId`, responses-shared.ts:165-177).
///
/// Outside the Responses tool-call provider set, or for ids without the
/// `{call_id}|{item_id}` pipe pair, the id is a single sanitized part. A
/// piped pair normalizes both parts; an item id from a foreign source
/// (the producing provider differs from the route — fr route keys pin one
/// api each, so the dsh `source.api !== model.api` disjunct is subsumed)
/// becomes a hashed `fc_` id, and every item id must keep that prefix.
#[must_use]
pub fn normalize_tool_call_id_responses(
    id: &str,
    route_provider: &str,
    source_provider: &str,
) -> String {
    if !RESPONSES_TOOL_CALL_PROVIDERS.contains(&route_provider) {
        return normalize_id_part(id);
    }
    // dsh checks `!id.includes("|")` before splitting; a pipe-less id is
    // one part even on a member route.
    if !id.contains('|') {
        return normalize_id_part(id);
    }
    let mut parts = id.split('|');
    let (Some(call_raw), item_raw) = (parts.next(), parts.next()) else {
        return normalize_id_part(id);
    };
    let call_id = normalize_id_part(call_raw);
    let is_foreign = source_provider != route_provider;
    let mut item_id = match item_raw {
        Some(item) if is_foreign => build_foreign_responses_item_id(item),
        Some(item) => normalize_id_part(item),
        // JS destructuring of a one-element `split` leaves `itemId`
        // undefined; the plain-part arm normalizes nothing further, but
        // the `fc_` prefix repair still applies to the empty item id.
        None => String::new(),
    };
    if !item_id.starts_with("fc_") {
        item_id = normalize_id_part(&format!("fc_{item_id}"));
    }
    format!("{call_id}|{item_id}")
}

/// The assistant same-model check (transform-messages.ts:95-98): provider
/// and model must equal the route (the `api` component is single-valued per
/// route, so the route's api matches by construction).
pub(crate) fn is_same_model(source: &MessageSource, route: &RouteIdentity<'_>) -> bool {
    match source {
        MessageSource::Known(KnownMessageSource::Model {
            provider, model, ..
        }) => provider == route.provider && model == route.model,
        MessageSource::Known(_) | MessageSource::Other(_) => false,
    }
}

/// Downgrade image blocks for non-vision models
/// (`downgradeUnsupportedImages`): user and tool-result image blocks become
/// placeholder text; other roles pass through.
fn downgrade_unsupported_images(message: &KnownMessage, supports_images: bool) -> KnownMessage {
    if supports_images {
        return message.clone();
    }
    match message {
        KnownMessage::User {
            id,
            content,
            source,
            extra,
        } => KnownMessage::User {
            id: id.clone(),
            content: replace_images_with_placeholder(content, NON_VISION_USER_IMAGE_PLACEHOLDER),
            source: source.clone(),
            extra: extra.clone(),
        },
        KnownMessage::Tool {
            id,
            content,
            source,
            tool_call_id,
            is_error,
            extra,
        } => KnownMessage::Tool {
            id: id.clone(),
            content: replace_images_with_placeholder(content, NON_VISION_TOOL_IMAGE_PLACEHOLDER),
            source: source.clone(),
            tool_call_id: tool_call_id.clone(),
            is_error: *is_error,
            extra: extra.clone(),
        },
        _ => message.clone(),
    }
}

/// Transform one assistant message's content
/// (transform-messages.ts:100-148): thinking blocks survive only for the
/// same model (cross-model they become plain text, empty ones drop), and
/// cross-model tool-call ids normalize through [`normalize_tool_call_id`],
/// feeding the result map that rewrites the matching tool results.
fn transform_assistant_content(
    content: &[ContentBlock],
    route: &RouteIdentity<'_>,
    same_model: bool,
    source_provider: &str,
    normalization: IdNormalization,
    id_map: &mut HashMap<String, String>,
) -> Vec<ContentBlock> {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Known(KnownContentBlock::Reasoning { text }) => {
                if same_model {
                    Some(block.clone())
                } else if text.trim().is_empty() {
                    None
                } else {
                    Some(ContentBlock::Known(KnownContentBlock::Text {
                        text: text.clone(),
                    }))
                }
            }
            ContentBlock::Known(KnownContentBlock::ToolCall {
                id,
                name,
                arguments,
            }) => {
                let mut call_id = id.as_str().to_owned();
                if !same_model {
                    let normalized = match normalization {
                        IdNormalization::Completions => {
                            normalize_tool_call_id(&call_id, route.provider)
                        }
                        IdNormalization::Responses => normalize_tool_call_id_responses(
                            &call_id,
                            route.provider,
                            source_provider,
                        ),
                    };
                    if normalized != call_id {
                        id_map.insert(call_id.clone(), normalized.clone());
                        call_id = normalized;
                    }
                }
                Some(ContentBlock::Known(KnownContentBlock::ToolCall {
                    id: ToolCallId::new(call_id),
                    name: name.clone(),
                    arguments: arguments.clone(),
                }))
            }
            other => Some(other.clone()),
        })
        .collect()
}

/// Transform the folded conversation (dsh `transformMessages` first pass
/// plus the orphan-synthesis second pass).
///
/// Unanswered tool calls synthesize `No result provided` results (error
/// flagged) when an assistant turn, a user turn, or the end of history
/// closes the window; unmatched tool results keep their call ids (dsh
/// synthesizes by id even when the call itself vanished).
#[must_use]
pub fn transform_history(
    messages: &[&RequestMessage],
    route: &RouteIdentity<'_>,
    supports_images: bool,
    normalization: IdNormalization,
) -> Vec<Turn> {
    let mut id_map: HashMap<String, String> = HashMap::new();
    // First pass: image downgrade and assistant normalization.
    let normalized: Vec<KnownMessage> = messages
        .iter()
        .filter_map(|message| match message {
            RequestMessage::Durable(Message::Known(known)) => {
                Some(downgrade_unsupported_images(known, supports_images))
            }
            _ => None,
        })
        .collect();
    // Second pass: orphan synthesis with pending tracking.
    let mut turns: Vec<Turn> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    let mut answered: Vec<String> = Vec::new();
    let close_pending =
        |turns: &mut Vec<Turn>, pending: &mut Vec<String>, answered: &mut Vec<String>| {
            for call_id in pending.drain(..) {
                if !answered.iter().any(|seen| seen == &call_id) {
                    turns.push(Turn::Tool {
                        call_id: ToolCallId::new(call_id),
                        is_error: Some(TrueOnly),
                        content: vec![ContentBlock::Known(KnownContentBlock::Text {
                            text: "No result provided".to_owned(),
                        })],
                    });
                }
            }
            answered.clear();
        };
    for message in &normalized {
        match message {
            KnownMessage::User { content, .. } => {
                close_pending(&mut turns, &mut pending, &mut answered);
                turns.push(Turn::User {
                    content: content.clone(),
                });
            }
            KnownMessage::Tool {
                tool_call_id,
                is_error,
                content,
                ..
            } => {
                let mapped = id_map
                    .get(tool_call_id.as_str())
                    .cloned()
                    .unwrap_or_else(|| tool_call_id.as_str().to_owned());
                answered.push(mapped.clone());
                turns.push(Turn::Tool {
                    call_id: ToolCallId::new(mapped),
                    is_error: *is_error,
                    content: content.clone(),
                });
            }
            KnownMessage::Assistant {
                source, content, ..
            } => {
                close_pending(&mut turns, &mut pending, &mut answered);
                let same_model = is_same_model(source, route);
                let source_provider = match source {
                    MessageSource::Known(KnownMessageSource::Model { provider, .. }) => {
                        provider.as_str()
                    }
                    _ => "",
                };
                let transformed = transform_assistant_content(
                    content,
                    route,
                    same_model,
                    source_provider,
                    normalization,
                    &mut id_map,
                );
                for block in &transformed {
                    if let ContentBlock::Known(KnownContentBlock::ToolCall { id, .. }) = block {
                        pending.push(id.as_str().to_owned());
                    }
                }
                turns.push(Turn::Assistant {
                    source: source.clone(),
                    content: transformed,
                });
            }
            // System and developer messages were folded away upstream.
            KnownMessage::System { .. } | KnownMessage::Developer { .. } => {}
        }
    }
    close_pending(&mut turns, &mut pending, &mut answered);
    turns
}

#[cfg(test)]
mod tests {
    use super::*;
    use fr_wire::RawObject;
    use fr_wire::ids::MessageId;
    fn route() -> RouteIdentity<'static> {
        RouteIdentity {
            provider: "p",
            model: "m",
        }
    }

    fn assistant(source: KnownMessageSource, content: Vec<ContentBlock>) -> KnownMessage {
        KnownMessage::Assistant {
            id: MessageId::new("a"),
            content,
            source: MessageSource::Known(source),
            extra: RawObject::new(),
        }
    }

    fn model_source(provider: &str, model: &str) -> KnownMessageSource {
        KnownMessageSource::Model {
            provider: provider.to_owned(),
            model: model.to_owned(),
            replay_state: None,
            extra: RawObject::new(),
        }
    }

    fn call(id: &str) -> ContentBlock {
        ContentBlock::Known(KnownContentBlock::ToolCall {
            id: ToolCallId::new(id),
            name: "sh".to_owned(),
            arguments: "{}".to_owned(),
        })
    }

    fn user(content: Vec<ContentBlock>) -> KnownMessage {
        KnownMessage::User {
            id: MessageId::new("u"),
            content,
            source: MessageSource::Known(KnownMessageSource::User {
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        }
    }

    fn tool(call_id: &str) -> KnownMessage {
        KnownMessage::Tool {
            id: MessageId::new("t"),
            content: vec![ContentBlock::Known(KnownContentBlock::Text {
                text: "ok".to_owned(),
            })],
            source: MessageSource::Known(KnownMessageSource::Tool {
                call_id: ToolCallId::new(call_id),
                extra: RawObject::new(),
            }),
            tool_call_id: ToolCallId::new(call_id),
            is_error: None,
            extra: RawObject::new(),
        }
    }

    /// The pipe-id pipeline: sanitize both halves, join, keep it under the
    /// `OpenAI` limit, and hash-shorten over-long ids; plain ids pass
    /// through unless the route is `openai`.
    #[test]
    fn normalizes_pipe_separated_call_ids() {
        assert_eq!(
            normalize_tool_call_id("resp_1|a+b/c", "github-copilot"),
            "resp_1_a_b_c"
        );
        let long_call = "c".repeat(50);
        let long = format!("{long_call}|item");
        let normalized = normalize_tool_call_id(&long, "github-copilot");
        assert!(normalized.chars().count() <= 40);
        // `prefix_hash` shape: prefix = 40 - 8 - 1 chars, then the hash.
        let hash = short_hash(&long);
        let sliced = &hash[..8];
        let separator = normalized.rfind('_').expect("prefix_hash shape");
        assert_eq!(separator, 40 - sliced.len() - 1);
        assert_eq!(&normalized[separator + 1..], sliced);
        assert_eq!(normalize_tool_call_id("plain_id", "openrouter"), "plain_id");
        assert_eq!(
            normalize_tool_call_id(&"x".repeat(45), "openai")
                .chars()
                .count(),
            40
        );
        assert_eq!(
            normalize_tool_call_id(&"x".repeat(45), "deepseek")
                .chars()
                .count(),
            45
        );
    }

    /// Cross-model assistants turn reasoning into text, normalize tool-call
    /// ids, and the matching tool results follow the map; same-model
    /// history keeps its reasoning blocks and ids.
    #[test]
    fn downgrades_cross_model_assistants_and_rewrites_ids() {
        let foreign = assistant(
            model_source("other", "m2"),
            vec![
                ContentBlock::Known(KnownContentBlock::Reasoning {
                    text: "think".to_owned(),
                }),
                call("resp_1|a"),
            ],
        );
        let result = tool("resp_1|a");
        let foreign_msg = durable_owned(foreign);
        let result_msg = durable_owned(result);
        let turns = transform_history(
            &[&foreign_msg, &result_msg],
            &route(),
            true,
            IdNormalization::Completions,
        );
        assert_eq!(turns.len(), 2);
        let Turn::Assistant { content, .. } = &turns[0] else {
            panic!("assistant first");
        };
        assert!(matches!(
            &content[0],
            ContentBlock::Known(KnownContentBlock::Text { text }) if text == "think"
        ));
        let ContentBlock::Known(KnownContentBlock::ToolCall { id, .. }) = &content[1] else {
            panic!("tool call expected");
        };
        let normalized_id = id.as_str().to_owned();
        let Turn::Tool { call_id, .. } = &turns[1] else {
            panic!("tool second");
        };
        assert_eq!(call_id.as_str(), normalized_id);

        let same = assistant(
            model_source("p", "m"),
            vec![ContentBlock::Known(KnownContentBlock::Reasoning {
                text: "kept".to_owned(),
            })],
        );
        let same_msg = durable_owned(same);
        let turns = transform_history(&[&same_msg], &route(), true, IdNormalization::Completions);
        let Turn::Assistant { content, .. } = &turns[0] else {
            panic!("assistant expected");
        };
        assert!(matches!(
            &content[0],
            ContentBlock::Known(KnownContentBlock::Reasoning { .. })
        ));
    }

    /// Unanswered calls synthesize error results at the closing boundary;
    /// non-vision models get placeholder text for images, deduplicated.
    #[test]
    fn synthesizes_orphans_and_downgrades_images() {
        let image = ContentBlock::Known(KnownContentBlock::Image {
            attachment: fr_wire::ImageAttachmentRef {
                attachment_id: fr_wire::AttachmentId::new("att"),
                media_type: fr_wire::ImageMediaType::Png,
                bytes: 4,
                width: 1,
                height: 1,
                name: None,
                original_dimensions: None,
            },
            offloaded: None,
        });
        let asks = user(vec![image.clone(), image]);
        let calls = assistant(model_source("p", "m"), vec![call("c1")]);
        let asks_msg = durable_owned(asks);
        let calls_msg = durable_owned(calls);
        let turns = transform_history(
            &[&asks_msg, &calls_msg],
            &route(),
            false,
            IdNormalization::Completions,
        );
        // The user turn, the assistant turn, then the synthesized result at
        // the end-of-history boundary.
        assert_eq!(turns.len(), 3);
        let Turn::User { content } = &turns[0] else {
            panic!("user first");
        };
        // Two adjacent images collapse into one placeholder.
        assert!(matches!(
            &content[0],
            ContentBlock::Known(KnownContentBlock::Text { text })
                if text == NON_VISION_USER_IMAGE_PLACEHOLDER
        ));
        let Turn::Tool {
            is_error, content, ..
        } = &turns[2]
        else {
            panic!("synthetic result last");
        };
        assert!(is_error.is_some());
        assert!(matches!(
            &content[0],
            ContentBlock::Known(KnownContentBlock::Text { text }) if text == "No result provided"
        ));
    }

    fn durable_owned(known: KnownMessage) -> RequestMessage {
        RequestMessage::Durable(Message::Known(Box::new(known)))
    }
}
