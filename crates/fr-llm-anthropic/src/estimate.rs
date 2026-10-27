//! Context token estimation for the max-tokens clamp (pi-ai
//! `utils/estimate.ts`, D14: the 3.5 chars-per-token estimate with the
//! 4096-token safety margin).
//!
//! fr vocabulary adaptations (each load-bearing): assistant messages carry
//! no durable `usage`/`timestamp` (settlement metadata lives outside the
//! message), so the usage-anchored branch of `estimateContextTokens` is
//! unreachable and the plain per-message estimate always runs; tool
//! declarations ride the request tool set instead of system messages, so
//! their chars count from [`GenerateOptions::tools`]; file blocks count
//! their handle text via the shared projection.

use fr_llm::adapter::{GenerateOptions, RequestMessage};
use fr_llm::projection::file_handle_text;
use fr_wire::message::{KnownMessage, Message};
use fr_wire::{ContentBlock, KnownContentBlock};

/// `CHARS_PER_TOKEN` (estimate.ts:16).
const CHARS_PER_TOKEN: f64 = 3.5;
/// `ESTIMATED_IMAGE_CHARS` (estimate.ts:17).
const ESTIMATED_IMAGE_CHARS: f64 = 4800.0;
/// `CONTEXT_SAFETY_TOKENS` (simple-options.ts:15).
pub const CONTEXT_SAFETY_TOKENS: u64 = 4096;
/// `MIN_MAX_TOKENS` (simple-options.ts:16).
pub const MIN_MAX_TOKENS: u64 = 1;

/// JS `String.prototype.length` (UTF-16 code units).
fn utf16_len(text: &str) -> f64 {
    text.chars()
        .map(|c| if c as u32 > 0xffff { 2.0 } else { 1.0 })
        .sum()
}

/// `estimateTextTokens`.
#[must_use]
pub fn estimate_text_tokens(text: &str) -> u64 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "ceil of a non-negative quotient is a non-negative integer; magnitudes stay far below u64"
    )]
    let tokens = (utf16_len(text) / CHARS_PER_TOKEN).ceil() as u64;
    tokens
}

fn content_chars(content: &[ContentBlock]) -> f64 {
    content
        .iter()
        .map(|block| match block {
            ContentBlock::Known(
                KnownContentBlock::Text { text } | KnownContentBlock::Reasoning { text },
            ) => utf16_len(text),
            ContentBlock::Known(KnownContentBlock::Image { .. }) => ESTIMATED_IMAGE_CHARS,
            ContentBlock::Known(KnownContentBlock::File { attachment }) => {
                utf16_len(&file_handle_text(attachment, None))
            }
            // Tool-change, unknown, and other blocks contribute nothing to
            // the dsh estimate faces.
            _ => 0.0,
        })
        .sum()
}

fn ceil_tokens(chars: f64) -> u64 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "ceil of a non-negative quotient is a non-negative integer; magnitudes stay far below u64"
    )]
    let tokens = (chars / CHARS_PER_TOKEN).ceil() as u64;
    tokens
}

/// `estimateMessageTokens` over the fr message vocabulary.
///
/// System messages count only their text (fr carries no
/// `sections`/`toolsAdded` there), user and tool results count text and
/// image blocks, and assistants count text, reasoning, and tool-call name
/// plus serialized arguments.
#[must_use]
pub fn estimate_message_tokens(message: &RequestMessage) -> u64 {
    let durable = match message {
        RequestMessage::Durable(Message::Known(known)) => known.as_ref(),
        // Untyped request input estimates like its user content; unknown
        // durable roles estimate to zero (they are skipped downstream too).
        RequestMessage::UserInput { content } => {
            return ceil_tokens(content_chars(content));
        }
        RequestMessage::Durable(_) => return 0,
    };
    match durable {
        KnownMessage::System { content, .. }
        | KnownMessage::Developer { content, .. }
        | KnownMessage::User { content, .. }
        | KnownMessage::Tool { content, .. } => ceil_tokens(content_chars(content)),
        KnownMessage::Assistant { content, .. } => {
            let chars: f64 = content
                .iter()
                .map(|block| match block {
                    ContentBlock::Known(
                        KnownContentBlock::Text { text } | KnownContentBlock::Reasoning { text },
                    ) => utf16_len(text),
                    ContentBlock::Known(KnownContentBlock::ToolCall {
                        name, arguments, ..
                    }) => {
                        // dsh stringifies the parsed arguments object; fr
                        // history stores the raw provider JSON, whose length
                        // matches the compact stringify in practice.
                        utf16_len(name) + utf16_len(arguments)
                    }
                    _ => 0.0,
                })
                .sum();
            ceil_tokens(chars)
        }
    }
}

/// `estimateContextTokens` without the unreachable usage anchor (see the
/// module docs): the plain sum over every message.
#[must_use]
pub fn estimate_context_tokens(options: &GenerateOptions) -> u64 {
    let mut chars = 0.0;
    let message_tokens: u64 = options.messages.iter().map(estimate_message_tokens).sum();
    let _ = &mut chars;
    // Tool declarations ride the request (dsh counted them via the leading
    // system message's `toolsAdded`; the fr request set is the same payload).
    let tools_tokens = options.tools.as_ref().map_or(0, |tools| {
        let serialized = serde_json::to_string(
            &tools
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    })
                })
                .collect::<Vec<_>>(),
        )
        .unwrap_or_default();
        estimate_text_tokens(&serialized)
    });
    message_tokens.saturating_add(tools_tokens)
}

/// `clampMaxTokensToContext`: the request cap, else the model cap, reduced
/// by the estimate and the safety margin, never below `MIN_MAX_TOKENS`.
#[must_use]
pub fn clamp_max_tokens_to_context(
    context_window: i64,
    request_max_tokens: Option<u64>,
    model_max_tokens: u64,
    options: &GenerateOptions,
) -> u64 {
    let requested = request_max_tokens.unwrap_or(model_max_tokens);
    if context_window <= 0 {
        return requested.max(MIN_MAX_TOKENS);
    }
    #[expect(
        clippy::cast_sign_loss,
        reason = "the branch condition pins context_window to the positive range"
    )]
    let window = context_window as u64;
    let available = window
        .saturating_sub(estimate_context_tokens(options))
        .saturating_sub(CONTEXT_SAFETY_TOKENS);
    requested
        .min(available.max(MIN_MAX_TOKENS))
        .max(MIN_MAX_TOKENS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fr_wire::call_config::ToolSchema;
    use fr_wire::ids::MessageId;
    use fr_wire::message::{KnownMessageSource, MessageSource};
    use fr_wire::{RawObject, ToolCallId};

    fn durable(known: KnownMessage) -> RequestMessage {
        RequestMessage::Durable(Message::Known(Box::new(known)))
    }

    fn user_text_message(text: &str) -> KnownMessage {
        KnownMessage::User {
            id: MessageId::new("m1"),
            content: vec![ContentBlock::Known(KnownContentBlock::Text {
                text: text.to_owned(),
            })],
            source: MessageSource::Known(KnownMessageSource::User {
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        }
    }

    /// 3.5 chars/token with ceil: 7 chars = 2 tokens, 8 chars = 3; the
    /// clamp subtracts the estimate and the 4096 margin from the window and
    /// never drops below one token.
    #[test]
    fn clamps_max_tokens_to_context_window() {
        let mut options = GenerateOptions {
            provider: "p".to_owned(),
            model: "m".to_owned(),
            reasoning_effort: None,
            messages: vec![durable(user_text_message(&"x".repeat(35_000)))],
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
        // 35 000 chars -> 10 000 estimated tokens; window 32 768 leaves
        // 32 768 - 10 000 - 4096 = 18 672.
        assert_eq!(
            clamp_max_tokens_to_context(32_768, None, 100_000, &options),
            18_672
        );
        // The floor is one token even with an overflowing estimate.
        options.messages = vec![durable(user_text_message(&"y".repeat(200_000)))];
        assert_eq!(
            clamp_max_tokens_to_context(4_096, None, 100_000, &options),
            1
        );
        // A non-positive window disables the clamp entirely.
        assert_eq!(
            clamp_max_tokens_to_context(0, None, 100_000, &options),
            100_000
        );
        // The request cap wins over the available room when smaller.
        options.messages = vec![durable(user_text_message(&"x".repeat(35_000)))];
        assert_eq!(
            clamp_max_tokens_to_context(32_768, Some(500), 100_000, &options),
            500
        );
    }

    /// Tool declarations count toward the estimate (dsh counted them via
    /// the leading system message).
    #[test]
    fn counts_request_tool_declarations() {
        let mut options = GenerateOptions {
            provider: "p".to_owned(),
            model: "m".to_owned(),
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
        };
        // No messages and no tools estimate to zero.
        assert_eq!(estimate_context_tokens(&options), 0);
        options.tools = None;
        options.messages = vec![durable(user_text_message("1234567"))];
        assert_eq!(estimate_context_tokens(&options), 2);
        // The serialized declarations add their own estimate.
        options.tools = Some(vec![ToolSchema {
            defer_loading: None,
            name: "sh".to_owned(),
            description: "runs a shell".to_owned(),
            parameters: RawObject::from_iter([(
                "type".to_owned(),
                serde_json::Value::String("object".to_owned()),
            )]),
        }]);
        let with_tools = estimate_context_tokens(&options);
        assert!(with_tools > 2, "tools add to the estimate: {with_tools}");
    }

    /// Assistant tool calls count name plus arguments characters; an
    /// unparseable arguments string is skipped harmlessly.
    #[test]
    fn estimates_assistant_tool_call_lengths() {
        let assistant = KnownMessage::Assistant {
            id: MessageId::new("m2"),
            content: vec![ContentBlock::Known(KnownContentBlock::ToolCall {
                id: ToolCallId::new("c1"),
                name: "abc".to_owned(),
                arguments: r#"{"k":"v"}"#.to_owned(),
            })],
            source: MessageSource::Known(KnownMessageSource::Model {
                provider: "p".to_owned(),
                model: "m".to_owned(),
                replay_state: None,
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        };
        let options = GenerateOptions {
            provider: "p".to_owned(),
            model: "m".to_owned(),
            reasoning_effort: None,
            messages: vec![durable(assistant)],
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
        // 3 + 9 = 12 chars -> ceil(12 / 3.5) = 4 tokens.
        assert_eq!(estimate_context_tokens(&options), 4);
    }
}
