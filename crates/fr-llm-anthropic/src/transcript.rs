//! Transcript folding (pi-ai `utils/transcript.ts` under the Q74-B
//! baseline).
//!
//! `supportsMidConvoSystemMessages` is pinned `false` for every
//! fr-llm-openai route, so `resolveTranscript` always collapses: the caller
//! prompt and every system message's text fold into one leading prompt, and
//! developer updates fold in the same way (their tool-change blocks are
//! carried by the request tool set — "工具全集每请求重发", contract
//! baseline-folding clause citing transcript.ts:108-120). The resolved tool
//! set itself is `GenerateOptions::tools`, which the harness already folds
//! ("complete active tool declarations"), so no tool-state walk happens
//! here; `resolveTranscriptTools` reduces to "send the full set".

use fr_llm::adapter::{GenerateOptions, RequestMessage};
use fr_wire::message::{KnownMessage, Message};

use crate::text::content_text;

/// The folded transcript handed to message conversion.
#[derive(Debug)]
pub struct FoldedTranscript<'a> {
    /// The leading system prompt: caller prompt, then every system and
    /// developer message's text, joined with blank lines (dsh
    /// `getCurrentSystemMessage` content join; fr has no `sections` face).
    pub system: String,
    /// Every non-system, non-developer message, in order.
    pub messages: Vec<&'a RequestMessage>,
}

/// Fold the request into the collapsed transcript (dsh
/// `collapseSystemMessages` composed with `normalizeContext`).
#[must_use]
pub fn fold_transcript(options: &GenerateOptions) -> FoldedTranscript<'_> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(caller) = options.system.as_deref().filter(|text| !text.is_empty()) {
        parts.push(caller.to_owned());
    }
    let mut messages = Vec::with_capacity(options.messages.len());
    for message in &options.messages {
        let known = match message {
            RequestMessage::Durable(Message::Known(known)) => known.as_ref(),
            // Untyped request input and raw user turns pass through
            // untouched.
            RequestMessage::UserInput { .. } | RequestMessage::Durable(Message::Other(_)) => {
                messages.push(message);
                continue;
            }
        };
        match known {
            KnownMessage::System { content, .. } | KnownMessage::Developer { content, .. } => {
                let text = content_text(content, "\n");
                if !text.is_empty() {
                    parts.push(text);
                }
            }
            KnownMessage::User { .. }
            | KnownMessage::Assistant { .. }
            | KnownMessage::Tool { .. } => messages.push(message),
        }
    }
    FoldedTranscript {
        system: parts.join("\n\n"),
        messages,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fr_wire::ids::MessageId;
    use fr_wire::message::{KnownMessageSource, MessageSource};
    use fr_wire::{ContentBlock, KnownContentBlock, RawObject, ToolCallId};

    fn durable(known: KnownMessage) -> RequestMessage {
        RequestMessage::Durable(Message::Known(Box::new(known)))
    }

    fn text_block(text: &str) -> ContentBlock {
        ContentBlock::Known(KnownContentBlock::Text {
            text: text.to_owned(),
        })
    }

    fn system_msg(text: &str) -> KnownMessage {
        KnownMessage::System {
            id: MessageId::new("m1"),
            content: vec![text_block(text)],
            source: MessageSource::Known(KnownMessageSource::SystemPrompt {
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        }
    }

    fn user_msg(text: &str) -> KnownMessage {
        KnownMessage::User {
            id: MessageId::new("m2"),
            content: vec![text_block(text)],
            source: MessageSource::Known(KnownMessageSource::User {
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        }
    }

    /// The caller prompt and every system/developer text fold into one
    /// leading prompt; conversation turns keep their order; tool-change
    /// blocks contribute nothing (the request set carries them).
    #[test]
    fn collapses_system_and_developer_text_into_the_head() {
        let developer = KnownMessage::Developer {
            id: MessageId::new("m3"),
            content: vec![
                text_block("use the shell"),
                ContentBlock::Known(KnownContentBlock::ToolAddition {
                    tool_name: "sh".to_owned(),
                }),
            ],
            source: MessageSource::Known(KnownMessageSource::User {
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        };
        let tool = KnownMessage::Tool {
            id: MessageId::new("m4"),
            content: vec![text_block("ok")],
            source: MessageSource::Known(KnownMessageSource::Tool {
                call_id: ToolCallId::new("c1"),
                extra: RawObject::new(),
            }),
            tool_call_id: ToolCallId::new("c1"),
            is_error: None,
            extra: RawObject::new(),
        };
        let options = GenerateOptions {
            provider: "p".to_owned(),
            model: "m".to_owned(),
            reasoning_effort: None,
            messages: vec![
                durable(system_msg("be nice")),
                durable(user_msg("run")),
                durable(developer),
                durable(tool),
                durable(system_msg("")),
            ],
            system: Some("caller prompt".to_owned()),
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
        assert_eq!(folded.system, "caller prompt\n\nbe nice\n\nuse the shell");
        assert_eq!(folded.messages.len(), 2);
        let RequestMessage::Durable(Message::Known(first)) = folded.messages[0] else {
            panic!("durable message expected");
        };
        assert!(matches!(first.as_ref(), KnownMessage::User { .. }));
        let RequestMessage::Durable(Message::Known(second)) = folded.messages[1] else {
            panic!("durable message expected");
        };
        assert!(matches!(second.as_ref(), KnownMessage::Tool { .. }));
    }
}
