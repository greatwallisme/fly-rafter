//! Minimal native thinking metadata for replay; durable blocks own all
//! response text (dsh `replay.ts`).
//!
//! The replay envelope is adapter-private lossless state carried inside the
//! durable stream, so its wire spellings (`deepseek-messages`, the
//! `tool-call` tag) are protocol-exempt and stay verbatim.

use fr_llm::error::LlmError;
use fr_wire::{
    ContentBlock, KnownContentBlock, KnownMessageSource, MessageSource, RawObject, ReplayEnvelope,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::codes::INVALID_REPLAY_STATE_CODE;

/// Index-aligned metadata retained alongside each emitted block (dsh
/// `ReplayBlock`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayBlock {
    /// The durable block kind this metadata aligns with.
    #[serde(rename = "type")]
    pub kind: ReplayBlockKind,
    /// Native thinking signature; only reasoning blocks carry one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// The only replay metadata kinds; the struct field carries the wire key
/// `type` (dsh `{ type: 'text' | 'reasoning' | 'tool-call' }`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReplayBlockKind {
    /// Plain text block.
    Text,
    /// Reasoning block; the only kind that may carry a signature.
    Reasoning,
    /// Model tool call block.
    ToolCall,
}

/// One static failure whose message and code satisfy the [`LlmError`]
/// invariants by construction (dsh throws these directly).
pub(crate) fn static_error(message: &str, code: &str) -> LlmError {
    LlmError::try_new(message, code, None, None, None, None)
        .expect("static message and code satisfy the LlmError invariants")
}

/// Reject malformed JSON objects at provider and durable-data reads (dsh
/// `object()`); arrays are rejected like any non-object.
pub(crate) fn expect_object<'a>(
    value: Option<&'a Value>,
    code: &str,
) -> Result<&'a RawObject, LlmError> {
    value
        .and_then(Value::as_object)
        .ok_or_else(|| static_error("DeepSeek Messages expected a JSON object", code))
}

/// The `String(value)` coercion dsh's template literals apply to provider
/// JSON fields in error text; a missing key coerces like `undefined`.
pub(crate) fn js_string(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_owned(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) => "null".to_owned(),
        Some(Value::Bool(true)) => "true".to_owned(),
        Some(Value::Bool(false)) => "false".to_owned(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| js_string(Some(item)))
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => "[object Object]".to_owned(),
    }
}

/// The wire `type` tag of a content block; unknown blocks report their
/// verbatim tag, and a missing one coerces like `undefined` (dsh reads
/// `block.type` directly).
pub(crate) fn content_type_name(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Known(KnownContentBlock::Text { .. }) => "text".to_owned(),
        ContentBlock::Known(KnownContentBlock::Reasoning { .. }) => "reasoning".to_owned(),
        ContentBlock::Known(KnownContentBlock::Image { .. }) => "image".to_owned(),
        ContentBlock::Known(KnownContentBlock::File { .. }) => "file".to_owned(),
        ContentBlock::Known(KnownContentBlock::ToolCall { .. }) => "tool-call".to_owned(),
        ContentBlock::Known(KnownContentBlock::ToolAddition { .. }) => "tool-addition".to_owned(),
        ContentBlock::Known(KnownContentBlock::ToolRemoval { .. }) => "tool-removal".to_owned(),
        ContentBlock::Other(map) => js_string(map.get("type")),
    }
}

/// Construct response metadata without duplicating the assistant text (dsh
/// `replayState`): a versioned envelope persisted by the existing assembler.
///
/// # Panics
/// Never: the `expect` covers an infallible derived `Serialize`.
#[must_use]
pub fn replay_state(model: &str, blocks: Vec<ReplayBlock>) -> ReplayEnvelope {
    ReplayEnvelope {
        response: serde_json::json!({
            "kind": "deepseek-messages",
            "version": 1,
            "model": model,
        }),
        blocks: Some(
            blocks
                .into_iter()
                .map(|block| {
                    serde_json::to_value(&block).expect("ReplayBlock derives infallible Serialize")
                })
                .collect(),
        ),
    }
}

/// Validate native replay, discarding unusable metadata before serializing
/// durable content (dsh `readReplay`).
///
/// A replay-state failure degrades through `on_degrade` and yields `None`;
/// cross-model history (the source names another model than the target) is
/// foreign metadata and silently yields `None` too.
///
/// # Errors
/// Anything other than [`INVALID_REPLAY_STATE_CODE`] propagates.
pub fn read_replay<'p, 'd>(
    source: &MessageSource,
    content: &[ContentBlock],
    model: &str,
    on_degrade: Option<&'p mut (dyn FnMut(&str) + 'd)>,
) -> Result<Option<Vec<ReplayBlock>>, LlmError> {
    match validate_replay(source, content, model) {
        Ok(blocks) => Ok(blocks),
        Err(error) if error.code() == INVALID_REPLAY_STATE_CODE => {
            if let Some(on_degrade) = on_degrade {
                on_degrade(error.message());
            }
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn validate_replay(
    source: &MessageSource,
    content: &[ContentBlock],
    model: &str,
) -> Result<Option<Vec<ReplayBlock>>, LlmError> {
    let MessageSource::Known(KnownMessageSource::Model {
        model: source_model,
        replay_state: Some(state),
        ..
    }) = source
    else {
        return Ok(None);
    };
    let fail = |detail: &str| {
        static_error(
            &format!("DeepSeek Messages replay: {detail}"),
            INVALID_REPLAY_STATE_CODE,
        )
    };
    let envelope = expect_object(Some(state), INVALID_REPLAY_STATE_CODE)?;
    let response = expect_object(envelope.get("response"), INVALID_REPLAY_STATE_CODE)?;
    if response.get("kind").and_then(Value::as_str) != Some("deepseek-messages")
        || response.get("version").and_then(Value::as_f64) != Some(1.0)
    {
        return Err(fail("unsupported kind or version"));
    }
    let response_model = response.get("model").and_then(Value::as_str);
    if response_model != Some(source_model.as_str()) {
        return Err(fail("model does not match assistant source model"));
    }
    let raw_blocks = envelope
        .get("blocks")
        .and_then(Value::as_array)
        .ok_or_else(|| fail("block count mismatch"))?;
    if raw_blocks.len() != content.len() {
        return Err(fail("block count mismatch"));
    }
    let mut blocks = Vec::with_capacity(raw_blocks.len());
    for (index, value) in raw_blocks.iter().enumerate() {
        let block = expect_object(Some(value), INVALID_REPLAY_STATE_CODE)?;
        let wire_type = block.get("type").and_then(Value::as_str);
        let content_type = content.get(index).map(content_type_name);
        if content_type.as_deref() != wire_type
            || !matches!(wire_type, Some("text" | "reasoning" | "tool-call"))
        {
            return Err(fail("block type mismatch"));
        }
        let kind = match wire_type {
            Some("text") => ReplayBlockKind::Text,
            Some("reasoning") => ReplayBlockKind::Reasoning,
            _ => ReplayBlockKind::ToolCall,
        };
        let signature = match block.get("signature") {
            None => None,
            Some(value) => match (wire_type, value.as_str()) {
                (Some("reasoning"), Some(text)) => Some(text.to_owned()),
                _ => return Err(fail("invalid signature")),
            },
        };
        blocks.push(ReplayBlock { kind, signature });
    }
    Ok((response_model == Some(model)).then_some(blocks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model_source(model: &str, replay: Option<serde_json::Value>) -> MessageSource {
        MessageSource::Known(KnownMessageSource::Model {
            provider: "deepseek-official".to_owned(),
            model: model.to_owned(),
            replay_state: replay,
            extra: RawObject::new(),
        })
    }

    fn durable_content() -> Vec<ContentBlock> {
        vec![
            ContentBlock::Known(KnownContentBlock::Text {
                text: "hi".to_owned(),
            }),
            ContentBlock::Known(KnownContentBlock::Reasoning {
                text: "th".to_owned(),
            }),
            ContentBlock::Known(KnownContentBlock::ToolCall {
                id: fr_wire::ToolCallId::new("c1"),
                name: "sh".to_owned(),
                arguments: "{}".to_owned(),
            }),
        ]
    }

    fn good_replay() -> serde_json::Value {
        json!({
            "response": {"kind": "deepseek-messages", "version": 1, "model": "m"},
            "blocks": [
                {"type": "text"},
                {"type": "reasoning", "signature": "s"},
                {"type": "tool-call"},
            ],
        })
    }

    #[test]
    fn replay_envelope_pins_dsh_shape() {
        let envelope = replay_state(
            "m",
            vec![
                ReplayBlock {
                    kind: ReplayBlockKind::Text,
                    signature: None,
                },
                ReplayBlock {
                    kind: ReplayBlockKind::Reasoning,
                    signature: Some("s".to_owned()),
                },
                ReplayBlock {
                    kind: ReplayBlockKind::ToolCall,
                    signature: None,
                },
            ],
        );
        assert_eq!(
            envelope.response,
            json!({"kind": "deepseek-messages", "version": 1, "model": "m"})
        );
        let blocks = envelope.blocks.as_deref().unwrap_or_default();
        assert_eq!(
            blocks,
            &[
                json!({"type": "text"}),
                json!({"type": "reasoning", "signature": "s"}),
                json!({"type": "tool-call"}),
            ]
        );
    }

    #[test]
    fn read_replay_validates_blocks() {
        let good = read_replay(
            &model_source("m", Some(good_replay())),
            &durable_content(),
            "m",
            None,
        )
        .expect("valid replay");
        let blocks = good.expect("same-model replay");
        assert_eq!(
            blocks,
            vec![
                ReplayBlock {
                    kind: ReplayBlockKind::Text,
                    signature: None,
                },
                ReplayBlock {
                    kind: ReplayBlockKind::Reasoning,
                    signature: Some("s".to_owned()),
                },
                ReplayBlock {
                    kind: ReplayBlockKind::ToolCall,
                    signature: None,
                },
            ]
        );

        let cases: [(&str, serde_json::Value); 5] = [
            (
                "unsupported kind or version",
                json!({
                    "response": {"kind": "other", "version": 1, "model": "m"},
                    "blocks": [],
                }),
            ),
            (
                "model does not match assistant source model",
                json!({
                    "response": {"kind": "deepseek-messages", "version": 1, "model": "other"},
                    "blocks": [],
                }),
            ),
            (
                "block count mismatch",
                json!({
                    "response": {"kind": "deepseek-messages", "version": 1, "model": "m"},
                    "blocks": [{"type": "text"}],
                }),
            ),
            (
                "block type mismatch",
                json!({
                    "response": {"kind": "deepseek-messages", "version": 1, "model": "m"},
                    "blocks": [
                        {"type": "reasoning"},
                        {"type": "reasoning", "signature": "s"},
                        {"type": "tool-call"},
                    ],
                }),
            ),
            (
                "invalid signature",
                json!({
                    "response": {"kind": "deepseek-messages", "version": 1, "model": "m"},
                    "blocks": [
                        {"type": "text", "signature": "s"},
                        {"type": "reasoning", "signature": "s"},
                        {"type": "tool-call"},
                    ],
                }),
            ),
        ];
        for (detail, replay) in cases {
            // dsh readReplay degrades replay-state failures to `undefined`
            // through the diagnostic callback; the validator message is
            // only observable there.
            let mut degraded = Vec::new();
            let outcome = read_replay(
                &model_source("m", Some(replay)),
                &durable_content(),
                "m",
                Some(&mut |reason: &str| degraded.push(reason.to_owned())),
            )
            .expect("replay failures degrade");
            assert!(outcome.is_none(), "{detail}");
            assert_eq!(
                degraded,
                vec![format!("DeepSeek Messages replay: {detail}")]
            );
        }
    }

    #[test]
    fn read_replay_degrades_and_cross_models_silently() {
        // Foreign source kinds carry no replay at all.
        let user = MessageSource::Known(KnownMessageSource::User {
            extra: RawObject::new(),
        });
        assert!(
            read_replay(&user, &durable_content(), "m", None)
                .expect("foreign source")
                .is_none()
        );
        // Same validator, different target model: silent degrade.
        let cross = read_replay(
            &model_source("m", Some(good_replay())),
            &durable_content(),
            "other",
            None,
        )
        .expect("cross-model replay");
        assert!(cross.is_none());
        // Corrupted metadata degrades with the validator message.
        let mut degraded = Vec::new();
        let corrupt = json!({
            "response": {"kind": "deepseek-messages", "version": 2, "model": "m"},
            "blocks": [],
        });
        let outcome = read_replay(
            &model_source("m", Some(corrupt)),
            &durable_content(),
            "m",
            Some(&mut |reason: &str| degraded.push(reason.to_owned())),
        )
        .expect("degraded replay");
        assert!(outcome.is_none());
        assert_eq!(
            degraded,
            vec!["DeepSeek Messages replay: unsupported kind or version"]
        );
    }
}

// Rust guideline compliant 2026-10-26
