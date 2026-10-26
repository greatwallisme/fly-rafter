//! Translate decoded Messages events into the harness stream protocol while
//! preserving block order and cumulative usage (dsh `translate.ts`).
//!
//! dsh's generator state machine maps to [`Translator`]: feed framed events
//! in arrival order, collect the chunks each event yields, and call
//! [`Translator::end_of_stream`] once the body ends. The dsh generator
//! terminates at `message_stop`; the driver stops feeding, and any event
//! fed after settlement is dropped the way a completed generator would
//! never observe it.

use std::collections::HashMap;

use fr_llm::error::LlmError;
use fr_wire::call_config::{FinishReason, FinishReasonKind, TokenUsage};
use fr_wire::ids::SAFE_INTEGER_MAX;
use fr_wire::stream::{KnownStreamChunk, StreamChunk};
use fr_wire::{ContentBlock, KnownContentBlock, RawObject, ToolCallId};
use serde_json::Value;

use crate::codes::{MALFORMED_RESPONSE_CODE, STREAM_CLOSED_CODE, UNSUPPORTED_CONTENT_CODE};
use crate::replay::{
    ReplayBlock, ReplayBlockKind, expect_object, js_string, replay_state, static_error,
};

/// Upper bound of every provider-supplied count (`Number.isSafeInteger`
/// with a non-negative check); exactly representable in `f64`.
#[expect(
    clippy::cast_precision_loss,
    reason = "2^53-1 is exactly representable in f64"
)]
const SAFE_INTEGER_MAX_F64: f64 = SAFE_INTEGER_MAX as f64;

/// One in-flight response block, keyed by its wire index but renumbered
/// locally by arrival order (dsh `Block`).
#[derive(Debug)]
struct OpenBlock {
    local: u64,
    content: KnownContentBlock,
    replay: ReplayBlock,
    closed: bool,
    json: String,
}

/// The wire type tag of one in-flight block; `start_block` only stores
/// text, reasoning, and tool-call blocks.
const fn local_type_name(content: &KnownContentBlock) -> &'static str {
    match content {
        KnownContentBlock::Text { .. } => "text",
        KnownContentBlock::Reasoning { .. } => "reasoning",
        KnownContentBlock::Image { .. } => "image",
        KnownContentBlock::File { .. } => "file",
        KnownContentBlock::ToolCall { .. } => "tool-call",
        KnownContentBlock::ToolAddition { .. } => "tool-addition",
        KnownContentBlock::ToolRemoval { .. } => "tool-removal",
    }
}

/// A required provider string; anything else is malformed (dsh `string()`).
fn require_string(value: Option<&Value>) -> Result<String, LlmError> {
    match value {
        Some(Value::String(text)) => Ok(text.clone()),
        _ => Err(static_error(
            "DeepSeek Messages expected a string field",
            MALFORMED_RESPONSE_CODE,
        )),
    }
}

fn malformed(detail: &str) -> LlmError {
    static_error(
        &format!("DeepSeek Messages stream: {detail}"),
        MALFORMED_RESPONSE_CODE,
    )
}

/// A non-negative safe-integer count (dsh `Number.isSafeInteger(v) && v >= 0`);
/// integer-valued floats pass, matching the JS check.
fn safe_count(value: &Value) -> Option<u64> {
    let number = value.as_f64()?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 || number > SAFE_INTEGER_MAX_F64
    {
        return None;
    }
    #[expect(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "the bounds above pin the value to 0..=2^53-1, exactly representable"
    )]
    Some(number as u64)
}

fn index_of(event: &RawObject) -> Result<u64, LlmError> {
    event
        .get("index")
        .and_then(safe_count)
        .ok_or_else(|| malformed("invalid block index"))
}

/// Apply one provider usage object (dsh `updateUsage`): the four wire keys
/// overwrite local counts; a present key that is not a non-negative safe
/// integer is malformed.
fn update_usage(usage: &mut TokenUsage, raw: Option<&Value>) -> Result<(), LlmError> {
    let fields = expect_object(raw, MALFORMED_RESPONSE_CODE)?;
    let keys = [
        ("input_tokens", 0usize),
        ("output_tokens", 1),
        ("cache_read_input_tokens", 2),
        ("cache_creation_input_tokens", 3),
    ];
    for (wire, slot) in keys {
        let Some(value) = fields.get(wire) else {
            continue;
        };
        let Some(count) = safe_count(value) else {
            return Err(malformed(&format!("invalid {wire}")));
        };
        match slot {
            0 => usage.input_tokens = count,
            1 => usage.output_tokens = count,
            2 => usage.cache_read_tokens = Some(count),
            _ => usage.cache_write_tokens = Some(count),
        }
    }
    Ok(())
}

/// Translate one decoded `content_block_start` body (dsh `startBlock`).
fn start_block(event: &RawObject, local: u64) -> Result<OpenBlock, LlmError> {
    let native = expect_object(event.get("content_block"), MALFORMED_RESPONSE_CODE)?;
    let (content, replay) = match native.get("type").and_then(Value::as_str) {
        Some("text") => (
            KnownContentBlock::Text {
                text: require_string(native.get("text"))?,
            },
            ReplayBlock {
                kind: ReplayBlockKind::Text,
                signature: None,
            },
        ),
        Some("thinking") => {
            let signature = match native.get("signature") {
                None => None,
                Some(signature) => Some(require_string(Some(signature))?),
            };
            (
                KnownContentBlock::Reasoning {
                    text: require_string(native.get("thinking"))?,
                },
                ReplayBlock {
                    kind: ReplayBlockKind::Reasoning,
                    signature,
                },
            )
        }
        Some("tool_use") => {
            let id = require_string(native.get("id"))?;
            let name = require_string(native.get("name"))?;
            if id.is_empty() || name.is_empty() {
                return Err(malformed("empty tool identity"));
            }
            let input = expect_object(native.get("input"), MALFORMED_RESPONSE_CODE)?;
            (
                KnownContentBlock::ToolCall {
                    id: ToolCallId::new(id),
                    name,
                    arguments: serde_json::to_string(&Value::Object(input.clone()))
                        .expect("serializing a parsed JSON value is infallible"),
                },
                ReplayBlock {
                    kind: ReplayBlockKind::ToolCall,
                    signature: None,
                },
            )
        }
        _ => {
            return Err(static_error(
                &format!(
                    "DeepSeek Messages does not support response block {}",
                    js_string(native.get("type"))
                ),
                UNSUPPORTED_CONTENT_CODE,
            ));
        }
    };
    Ok(OpenBlock {
        local,
        content,
        replay,
        closed: false,
        json: String::new(),
    })
}

/// Translate one decoded `content_block_delta` body (dsh `deltaChunk`):
/// text, thinking, and argument deltas accumulate into the open block and
/// surface as stream chunks; signature deltas only grow the replay
/// metadata; a delta shape that does not match the open block is malformed.
fn delta_chunk(
    block: &mut OpenBlock,
    raw: Option<&Value>,
) -> Result<Option<StreamChunk>, LlmError> {
    let delta = expect_object(raw, MALFORMED_RESPONSE_CODE)?;
    let delta_type = delta.get("type").and_then(Value::as_str);
    match delta_type {
        Some("text_delta") => {
            let KnownContentBlock::Text { text } = &mut block.content else {
                return Err(mismatched_delta(block, delta));
            };
            let chunk_text = require_string(delta.get("text"))?;
            text.push_str(&chunk_text);
            Ok(Some(StreamChunk::Known(KnownStreamChunk::TextDelta {
                index: block.local,
                text: chunk_text,
            })))
        }
        Some("thinking_delta") => {
            let KnownContentBlock::Reasoning { text } = &mut block.content else {
                return Err(mismatched_delta(block, delta));
            };
            let chunk_text = require_string(delta.get("thinking"))?;
            text.push_str(&chunk_text);
            Ok(Some(StreamChunk::Known(KnownStreamChunk::ReasoningDelta {
                index: block.local,
                text: chunk_text,
            })))
        }
        Some("signature_delta") => {
            if !matches!(block.content, KnownContentBlock::Reasoning { .. }) {
                return Err(mismatched_delta(block, delta));
            }
            let signature = require_string(delta.get("signature"))?;
            let replay = &mut block.replay;
            let grown = format!("{}{signature}", replay.signature.as_deref().unwrap_or(""));
            replay.signature = Some(grown);
            Ok(None)
        }
        Some("input_json_delta") => {
            let KnownContentBlock::ToolCall { id, .. } = &block.content else {
                return Err(mismatched_delta(block, delta));
            };
            let arguments_delta = require_string(delta.get("partial_json"))?;
            block.json.push_str(&arguments_delta);
            Ok(Some(StreamChunk::Known(KnownStreamChunk::ToolCallDelta {
                index: block.local,
                id: id.clone(),
                name: None,
                arguments_delta,
            })))
        }
        other => Err(mismatched_delta_text(block, other)),
    }
}

/// The dsh mismatch message for a delta that does not fit the open block.
fn mismatched_delta(block: &OpenBlock, delta: &RawObject) -> LlmError {
    mismatched_delta_text(block, delta.get("type").and_then(Value::as_str))
}

fn mismatched_delta_text(block: &OpenBlock, delta_type: Option<&str>) -> LlmError {
    malformed(&format!(
        "unsupported delta {} for {}",
        js_string(delta_type.map(Value::from).as_ref()),
        local_type_name(&block.content),
    ))
}

/// Translate one decoded `stop_reason` (dsh `stopReason`).
fn stop_reason(raw: &Value) -> Result<FinishReason, LlmError> {
    match raw.as_str() {
        Some("end_turn" | "stop_sequence") => Ok(FinishReason::Known(FinishReasonKind::Stop)),
        Some("tool_use") => Ok(FinishReason::Known(FinishReasonKind::ToolCalls)),
        Some("max_tokens") => Ok(FinishReason::Known(FinishReasonKind::MaxTokens)),
        _ => Err(malformed(&format!(
            "unsupported stop reason {}",
            js_string(Some(raw))
        ))),
    }
}

/// The dsh `translate` generator as a state machine: feed framed events in
/// arrival order and collect the stream chunks each one yields.
#[derive(Debug)]
pub struct Translator {
    model: String,
    blocks: Vec<OpenBlock>,
    wire: HashMap<u64, usize>,
    usage: TokenUsage,
    started: bool,
    settled: bool,
    reason: Option<FinishReason>,
}

impl Translator {
    /// A translator for one request of `model`; the model id is stored in
    /// the durable replay state of the terminal finish.
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            blocks: Vec::new(),
            wire: HashMap::new(),
            usage: TokenUsage::default(),
            started: false,
            settled: false,
            reason: None,
        }
    }

    /// Translate one framed, decoded provider event.
    ///
    /// # Errors
    /// [`LlmError`] with `MALFORMED_RESPONSE`, `UNSUPPORTED_CONTENT`, or
    /// `EMPTY_RESPONSE` for every stream-shape violation dsh rejects;
    /// events after settlement yield nothing (the dsh generator has
    /// returned and never observes them).
    pub fn translate_event(&mut self, event: &RawObject) -> Result<Vec<StreamChunk>, LlmError> {
        if self.settled {
            return Ok(Vec::new());
        }
        let Some(kind) = event.get("type").and_then(Value::as_str) else {
            return Ok(Vec::new());
        };
        match kind {
            "message_start" => {
                if self.started {
                    return Err(malformed("duplicate message_start"));
                }
                let message = expect_object(event.get("message"), MALFORMED_RESPONSE_CODE)?;
                self.update_usage_from(message.get("usage"))?;
                self.started = true;
                Ok(Vec::new())
            }
            "content_block_start"
            | "content_block_delta"
            | "content_block_stop"
            | "message_delta"
            | "message_stop" => self.translate_content_event(event, kind),
            // Anthropic permits additional event types; dsh skips them.
            _ => Ok(Vec::new()),
        }
    }

    fn translate_content_event(
        &mut self,
        event: &RawObject,
        kind: &str,
    ) -> Result<Vec<StreamChunk>, LlmError> {
        if !self.started {
            return Err(malformed("event precedes message_start"));
        }
        match kind {
            "content_block_start" => self.open_block(event),
            "content_block_delta" => self.append_delta(event),
            "content_block_stop" => self.close_block(event),
            "message_delta" => self.apply_message_delta(event),
            _ => self.settle(event),
        }
    }

    fn open_block(&mut self, event: &RawObject) -> Result<Vec<StreamChunk>, LlmError> {
        let wire_index = index_of(event)?;
        if self.wire.contains_key(&wire_index) || self.reason.is_some() {
            return Err(malformed(
                "block starts after settlement or repeats an index",
            ));
        }
        let local = u64::try_from(self.blocks.len()).expect("a live block count never exceeds u64");
        let block = start_block(event, local)?;
        let mut chunks = vec![StreamChunk::Known(KnownStreamChunk::BlockStart {
            index: block.local,
            block_type: local_type_name(&block.content).to_owned(),
        })];
        match &block.content {
            KnownContentBlock::Text { text } | KnownContentBlock::Reasoning { text } => {
                if !text.is_empty() {
                    let initial = match &block.content {
                        KnownContentBlock::Text { .. } => KnownStreamChunk::TextDelta {
                            index: block.local,
                            text: text.clone(),
                        },
                        _ => KnownStreamChunk::ReasoningDelta {
                            index: block.local,
                            text: text.clone(),
                        },
                    };
                    chunks.push(StreamChunk::Known(initial));
                }
            }
            KnownContentBlock::ToolCall { id, name, .. } => {
                chunks.push(StreamChunk::Known(KnownStreamChunk::ToolCallDelta {
                    index: block.local,
                    id: id.clone(),
                    name: Some(name.clone()),
                    arguments_delta: String::new(),
                }));
            }
            KnownContentBlock::Image { .. }
            | KnownContentBlock::File { .. }
            | KnownContentBlock::ToolAddition { .. }
            | KnownContentBlock::ToolRemoval { .. } => {}
        }
        self.wire.insert(wire_index, self.blocks.len());
        self.blocks.push(block);
        Ok(chunks)
    }

    fn append_delta(&mut self, event: &RawObject) -> Result<Vec<StreamChunk>, LlmError> {
        let wire_index = index_of(event)?;
        let block = self
            .wire
            .get(&wire_index)
            .copied()
            .and_then(|local| self.blocks.get_mut(local));
        let Some(block) = block.filter(|block| !block.closed) else {
            return Err(malformed("delta/stop without an open block"));
        };
        Ok(delta_chunk(block, event.get("delta"))?
            .into_iter()
            .collect())
    }

    fn close_block(&mut self, event: &RawObject) -> Result<Vec<StreamChunk>, LlmError> {
        let wire_index = index_of(event)?;
        let block = self
            .wire
            .get(&wire_index)
            .copied()
            .and_then(|local| self.blocks.get_mut(local));
        let Some(block) = block.filter(|block| !block.closed) else {
            return Err(malformed("delta/stop without an open block"));
        };
        block.closed = true;
        if let KnownContentBlock::ToolCall { arguments, .. } = &mut block.content
            && !block.json.is_empty()
        {
            arguments.clone_from(&block.json);
        }
        Ok(vec![StreamChunk::Known(KnownStreamChunk::BlockEnd {
            index: block.local,
            block: ContentBlock::Known(block.content.clone()),
        })])
    }

    fn apply_message_delta(&mut self, event: &RawObject) -> Result<Vec<StreamChunk>, LlmError> {
        let delta = expect_object(event.get("delta"), MALFORMED_RESPONSE_CODE)?;
        let raw_stop = delta.get("stop_reason").filter(|value| !value.is_null());
        if let Some(raw_stop) = raw_stop {
            self.reason = Some(stop_reason(raw_stop)?);
        }
        if let Some(usage) = event.get("usage") {
            self.update_usage_from(Some(usage))?;
        }
        Ok(Vec::new())
    }

    fn update_usage_from(&mut self, raw: Option<&Value>) -> Result<(), LlmError> {
        update_usage(&mut self.usage, raw)
    }

    /// Truncated tool JSON is retained in the stream, then pruned by the
    /// shared assembler; any other run must carry parseable object input
    /// (dsh `message_stop` validation).
    fn validate_tool_json(&self) -> Result<(), LlmError> {
        for block in &self.blocks {
            let KnownContentBlock::ToolCall { arguments, .. } = &block.content else {
                continue;
            };
            let parsed: Value = serde_json::from_str(arguments)
                .map_err(|_invalid_provider_tool_json| malformed("tool input is invalid JSON"))?;
            expect_object(Some(&parsed), MALFORMED_RESPONSE_CODE)?;
        }
        Ok(())
    }

    /// The dsh `message_stop` arm: settled blocks and a stop reason are
    /// mandatory, an empty stop run is an `EMPTY_RESPONSE`, tool JSON is
    /// validated unless the run hit the token ceiling, and the usage total
    /// plus the replay envelope ride the terminal chunks.
    fn settle(&mut self, _event: &RawObject) -> Result<Vec<StreamChunk>, LlmError> {
        if self.reason.is_none() || self.blocks.iter().any(|block| !block.closed) {
            return Err(malformed(
                "message_stop without settled blocks and stop reason",
            ));
        }
        let Some(reason) = self.reason.clone() else {
            return Err(malformed(
                "message_stop without settled blocks and stop reason",
            ));
        };
        if self.blocks.is_empty() && matches!(reason, FinishReason::Known(FinishReasonKind::Stop)) {
            return Err(static_error(
                "DeepSeek Messages returned no content",
                fr_llm::error::EMPTY_RESPONSE_CODE,
            ));
        }
        if !matches!(reason, FinishReason::Known(FinishReasonKind::MaxTokens)) {
            self.validate_tool_json()?;
        }
        let total = self
            .usage
            .input_tokens
            .saturating_add(self.usage.output_tokens)
            .saturating_add(self.usage.cache_read_tokens.unwrap_or(0))
            .saturating_add(self.usage.cache_write_tokens.unwrap_or(0));
        self.usage.total_tokens = Some(total);
        let replay = replay_state(
            &self.model,
            self.blocks
                .iter()
                .map(|block| block.replay.clone())
                .collect(),
        );
        self.settled = true;
        Ok(vec![
            StreamChunk::Known(KnownStreamChunk::Usage { usage: self.usage }),
            StreamChunk::Known(KnownStreamChunk::Finish {
                reason,
                replay_state: Some(replay),
            }),
        ])
    }

    /// The stream body ended: dsh throws unless `message_stop` already
    /// settled the run (dsh throws `STREAM_CLOSED` at generator end).
    ///
    /// # Errors
    /// [`STREAM_CLOSED_CODE`] when no `message_stop` settled the run.
    pub fn end_of_stream(&self) -> Result<(), LlmError> {
        if !self.settled {
            return Err(static_error(
                "DeepSeek Messages stream ended before message_stop",
                STREAM_CLOSED_CODE,
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(json: &serde_json::Value) -> RawObject {
        json.as_object().expect("object fixture").clone()
    }

    fn feed(translator: &mut Translator, events: &[serde_json::Value]) -> Vec<StreamChunk> {
        let mut chunks = Vec::new();
        for value in events {
            chunks.extend(
                translator
                    .translate_event(&event(value))
                    .expect("translate"),
            );
        }
        chunks
    }

    /// The recorded happy path: three blocks (text, thinking with a
    /// signature delta, tool call split across two JSON deltas), a wire
    /// index that differs from the local renumbering, usage started at
    /// `message_start` and overridden by `message_delta`.
    fn full_run_events() -> Vec<serde_json::Value> {
        vec![
            json!({"type": "message_start", "message": {"usage": {
                "input_tokens": 10, "output_tokens": 2,
                "cache_read_input_tokens": 3, "cache_creation_input_tokens": 4}}}),
            json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "text", "text": "Hel"}}),
            json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "text_delta", "text": "lo"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "content_block_start", "index": 1,
                "content_block": {"type": "thinking", "thinking": "th", "signature": "s0"}}),
            json!({"type": "content_block_delta", "index": 1,
                "delta": {"type": "thinking_delta", "thinking": "ink"}}),
            json!({"type": "content_block_delta", "index": 1,
                "delta": {"type": "signature_delta", "signature": "1"}}),
            json!({"type": "content_block_stop", "index": 1}),
            json!({"type": "content_block_start", "index": 5,
                "content_block": {"type": "tool_use", "id": "t1", "name": "sh", "input": {}}}),
            json!({"type": "content_block_delta", "index": 5,
                "delta": {"type": "input_json_delta", "partial_json": "{\"a\":"}}),
            json!({"type": "content_block_delta", "index": 5,
                "delta": {"type": "input_json_delta", "partial_json": "1}"}}),
            json!({"type": "content_block_stop", "index": 5}),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"},
                "usage": {"output_tokens": 7}}),
            json!({"type": "message_stop"}),
        ]
    }

    #[test]
    fn translates_full_stream_to_chunks() {
        let mut translator = Translator::new("m");
        let chunks = feed(&mut translator, &full_run_events());
        translator.end_of_stream().expect("settled");
        assert_eq!(chunks.len(), 15);
        assert_eq!(
            chunks[0],
            StreamChunk::Known(KnownStreamChunk::BlockStart {
                index: 0,
                block_type: "text".to_owned(),
            })
        );
        assert_eq!(
            chunks[1],
            StreamChunk::Known(KnownStreamChunk::TextDelta {
                index: 0,
                text: "Hel".to_owned(),
            })
        );
        assert_eq!(
            chunks[3],
            StreamChunk::Known(KnownStreamChunk::BlockEnd {
                index: 0,
                block: ContentBlock::Known(KnownContentBlock::Text {
                    text: "Hello".to_owned(),
                }),
            })
        );
        assert_eq!(
            chunks[6],
            StreamChunk::Known(KnownStreamChunk::ReasoningDelta {
                index: 1,
                text: "ink".to_owned(),
            })
        );
        assert_eq!(
            chunks[7],
            StreamChunk::Known(KnownStreamChunk::BlockEnd {
                index: 1,
                block: ContentBlock::Known(KnownContentBlock::Reasoning {
                    text: "think".to_owned(),
                }),
            })
        );
        assert_eq!(
            chunks[9],
            StreamChunk::Known(KnownStreamChunk::ToolCallDelta {
                index: 2,
                id: ToolCallId::new("t1"),
                name: Some("sh".to_owned()),
                arguments_delta: String::new(),
            })
        );
        assert_eq!(
            chunks[12],
            StreamChunk::Known(KnownStreamChunk::BlockEnd {
                index: 2,
                block: ContentBlock::Known(KnownContentBlock::ToolCall {
                    id: ToolCallId::new("t1"),
                    name: "sh".to_owned(),
                    arguments: "{\"a\":1}".to_owned(),
                }),
            })
        );
        assert_eq!(
            chunks[13],
            StreamChunk::Known(KnownStreamChunk::Usage {
                usage: TokenUsage {
                    input_tokens: 10,
                    output_tokens: 7,
                    total_tokens: Some(24),
                    cache_read_tokens: Some(3),
                    cache_write_tokens: Some(4),
                    reasoning_tokens: None,
                },
            })
        );
        let StreamChunk::Known(KnownStreamChunk::Finish {
            reason,
            replay_state,
        }) = &chunks[14]
        else {
            panic!("finish chunk");
        };
        assert_eq!(*reason, FinishReason::Known(FinishReasonKind::ToolCalls));
        let replay = replay_state.as_ref().expect("replay envelope");
        assert_eq!(
            replay.response,
            json!({"kind": "deepseek-messages", "version": 1, "model": "m"})
        );
        assert_eq!(
            replay.blocks.as_deref().unwrap_or_default(),
            &[
                json!({"type": "text"}),
                json!({"type": "reasoning", "signature": "s01"}),
                json!({"type": "tool-call"}),
            ]
        );
    }

    #[test]
    fn rejects_broken_stream_order() {
        // Unknown event kinds pass through silently (Anthropic permits them).
        let mut translator = Translator::new("m");
        assert_eq!(
            feed(&mut translator, &[json!({"type": "ping"})]),
            [] as [fr_wire::StreamChunk; 0]
        );

        // Content events before message_start are malformed.
        let mut translator = Translator::new("m");
        let error = translator
            .translate_event(&event(&json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "text_delta", "text": "x"}})))
            .expect_err("precedes message_start");
        assert_eq!(
            error.message(),
            "DeepSeek Messages stream: event precedes message_start"
        );

        // A second message_start is malformed.
        let mut translator = Translator::new("m");
        feed(
            &mut translator,
            &[json!({"type": "message_start", "message": {"usage": {}}})],
        );
        let error = translator
            .translate_event(&event(&json!({
                "type": "message_start",
                "message": {"usage": {}},
            })))
            .expect_err("duplicate message_start");
        assert_eq!(
            error.message(),
            "DeepSeek Messages stream: duplicate message_start"
        );

        // Delta for a wire index that never opened.
        let mut translator = Translator::new("m");
        feed(
            &mut translator,
            &[json!({"type": "message_start", "message": {"usage": {}}})],
        );
        let error = translator
            .translate_event(&event(&json!({"type": "content_block_delta", "index": 3,
                "delta": {"type": "text_delta", "text": "x"}})))
            .expect_err("unknown index");
        assert_eq!(
            error.message(),
            "DeepSeek Messages stream: delta/stop without an open block"
        );

        // A repeated wire index, and a block after settlement.
        let mut translator = Translator::new("m");
        feed(
            &mut translator,
            &[
                json!({"type": "message_start", "message": {"usage": {}}}),
                json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "text", "text": ""}}),
                json!({"type": "content_block_stop", "index": 0}),
            ],
        );
        let error = translator
            .translate_event(&event(&json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "text", "text": ""}})))
            .expect_err("repeated index");
        assert_eq!(
            error.message(),
            "DeepSeek Messages stream: block starts after settlement or repeats an index"
        );

        // After a stop reason settles the run, no new block may open.
        let mut translator = Translator::new("m");
        feed(
            &mut translator,
            &[
                json!({"type": "message_start", "message": {"usage": {}}}),
                json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}),
            ],
        );
        let error = translator
            .translate_event(&event(&json!({"type": "content_block_start", "index": 9,
                "content_block": {"type": "text", "text": ""}})))
            .expect_err("settled blocks refuse new ones");
        assert_eq!(
            error.message(),
            "DeepSeek Messages stream: block starts after settlement or repeats an index"
        );
    }

    #[test]
    fn message_stop_requires_settlement() {
        // message_stop with an open block is refused.
        let mut translator = Translator::new("m");
        feed(
            &mut translator,
            &[
                json!({"type": "message_start", "message": {"usage": {}}}),
                json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "text", "text": ""}}),
                json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}),
            ],
        );
        let error = translator
            .translate_event(&event(&json!({"type": "message_stop"})))
            .expect_err("open block at stop");
        assert_eq!(
            error.message(),
            "DeepSeek Messages stream: message_stop without settled blocks and stop reason"
        );

        // A body that ends without message_stop is a closed stream.
        let mut translator = Translator::new("m");
        feed(
            &mut translator,
            &[json!({"type": "message_start", "message": {"usage": {}}})],
        );
        let error = translator.end_of_stream().expect_err("unsettled body");
        assert_eq!(error.code(), STREAM_CLOSED_CODE);
        assert_eq!(
            error.message(),
            "DeepSeek Messages stream ended before message_stop"
        );
    }
    #[test]
    fn rejects_invalid_fields() {
        let started = |translator: &mut Translator| {
            feed(
                translator,
                &[json!({"type": "message_start", "message": {"usage": {}}})],
            );
        };

        let mut translator = Translator::new("m");
        started(&mut translator);
        let error = translator
            .translate_event(&event(&json!({"type": "content_block_start", "index": -1,
                "content_block": {"type": "text", "text": ""}})))
            .expect_err("negative index");
        assert_eq!(
            error.message(),
            "DeepSeek Messages stream: invalid block index"
        );

        let mut translator = Translator::new("m");
        started(&mut translator);
        let error = translator
            .translate_event(&event(&json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "redaction"}})))
            .expect_err("unknown block kind");
        assert_eq!(error.code(), UNSUPPORTED_CONTENT_CODE);
        assert_eq!(
            error.message(),
            "DeepSeek Messages does not support response block redaction"
        );

        let mut translator = Translator::new("m");
        started(&mut translator);
        let error = translator
            .translate_event(&event(&json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "tool_use", "id": "", "name": "sh", "input": {}}})))
            .expect_err("empty tool id");
        assert_eq!(
            error.message(),
            "DeepSeek Messages stream: empty tool identity"
        );

        let mut translator = Translator::new("m");
        started(&mut translator);
        let error = translator
            .translate_event(&event(&json!({"type": "message_delta",
                "delta": {"stop_reason": "bogus"}})))
            .expect_err("unknown stop reason");
        assert_eq!(
            error.message(),
            "DeepSeek Messages stream: unsupported stop reason bogus"
        );

        let mut translator = Translator::new("m");
        let error = translator
            .translate_event(&event(&json!({"type": "message_start", "message": {
                "usage": {"input_tokens": -1}}})))
            .expect_err("negative usage");
        assert_eq!(
            error.message(),
            "DeepSeek Messages stream: invalid input_tokens"
        );

        // A delta kind that does not fit the open block is malformed.
        let mut translator = Translator::new("m");
        started(&mut translator);
        feed(
            &mut translator,
            &[json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "text", "text": ""}})],
        );
        let error = translator
            .translate_event(&event(&json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "input_json_delta", "partial_json": "{}"}})))
            .expect_err("mismatched delta");
        assert_eq!(
            error.message(),
            "DeepSeek Messages stream: unsupported delta input_json_delta for text"
        );
    }

    #[test]
    fn empty_stop_run_is_empty_response() {
        // Zero blocks with a plain stop is EMPTY_RESPONSE.
        let mut translator = Translator::new("m");
        feed(
            &mut translator,
            &[
                json!({"type": "message_start", "message": {"usage": {}}}),
                json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}),
            ],
        );
        let error = translator
            .translate_event(&event(&json!({"type": "message_stop"})))
            .expect_err("empty run");
        assert_eq!(error.code(), fr_llm::error::EMPTY_RESPONSE_CODE);
        assert_eq!(error.message(), "DeepSeek Messages returned no content");
    }
    /// The `message_stop` settlement arms dsh rejects, and the stop-reason
    /// mapping with the max-tokens truncation carve-out.
    #[test]
    fn message_stop_settlement_arms() {
        // Invalid tool JSON at stop rejects (the block must be closed
        // first — dsh checks settlement before the JSON validation).
        let mut translator = Translator::new("m");
        feed(
            &mut translator,
            &[
                json!({"type": "message_start", "message": {"usage": {}}}),
                json!({"type": "content_block_start", "index": 0,
                    "content_block": {"type": "tool_use", "id": "t1", "name": "sh",
                        "input": {}}}),
                json!({"type": "content_block_delta", "index": 0,
                    "delta": {"type": "input_json_delta", "partial_json": "{\"a\":"}}),
                json!({"type": "content_block_stop", "index": 0}),
                json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}}),
            ],
        );
        let error = translator
            .translate_event(&event(&json!({"type": "message_stop"})))
            .expect_err("invalid tool json");
        assert_eq!(
            error.message(),
            "DeepSeek Messages stream: tool input is invalid JSON"
        );

        // An array tool input fails the object check at stop.
        let mut translator = Translator::new("m");
        feed(
            &mut translator,
            &[
                json!({"type": "message_start", "message": {"usage": {}}}),
                json!({"type": "content_block_start", "index": 0,
                    "content_block": {"type": "tool_use", "id": "t1", "name": "sh",
                        "input": {}}}),
                json!({"type": "content_block_delta", "index": 0,
                    "delta": {"type": "input_json_delta", "partial_json": "[1]"}}),
                json!({"type": "content_block_stop", "index": 0}),
                json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}}),
            ],
        );
        let error = translator
            .translate_event(&event(&json!({"type": "message_stop"})))
            .expect_err("array tool json");
        assert_eq!(error.message(), "DeepSeek Messages expected a JSON object");

        // The max-tokens ceiling keeps truncated tool JSON in the stream;
        // the shared assembler prunes it later.
        let mut translator = Translator::new("m");
        feed(
            &mut translator,
            &[
                json!({"type": "message_start", "message": {"usage": {}}}),
                json!({"type": "content_block_start", "index": 0,
                    "content_block": {"type": "tool_use", "id": "t1", "name": "sh",
                        "input": {}}}),
                json!({"type": "content_block_delta", "index": 0,
                    "delta": {"type": "input_json_delta", "partial_json": "{\"a\":"}}),
                json!({"type": "content_block_stop", "index": 0}),
                json!({"type": "message_delta", "delta": {"stop_reason": "max_tokens"}}),
            ],
        );
        let chunks = translator
            .translate_event(&event(&json!({"type": "message_stop"})))
            .expect("max-tokens keeps truncated json");
        let Some(StreamChunk::Known(KnownStreamChunk::Finish { reason, .. })) = chunks.last()
        else {
            panic!("finish chunk");
        };
        assert_eq!(*reason, FinishReason::Known(FinishReasonKind::MaxTokens));

        // The stop-reason mapping on a settled text run.
        for (wire, kind) in [
            ("end_turn", FinishReasonKind::Stop),
            ("stop_sequence", FinishReasonKind::Stop),
            ("max_tokens", FinishReasonKind::MaxTokens),
        ] {
            let mut translator = Translator::new("m");
            feed(
                &mut translator,
                &[
                    json!({"type": "message_start", "message": {"usage": {}}}),
                    json!({"type": "content_block_start", "index": 0,
                        "content_block": {"type": "text", "text": ""}}),
                    json!({"type": "content_block_stop", "index": 0}),
                    json!({"type": "message_delta", "delta": {"stop_reason": wire}}),
                ],
            );
            let chunks = translator
                .translate_event(&event(&json!({"type": "message_stop"})))
                .expect("settled run");
            let Some(StreamChunk::Known(KnownStreamChunk::Finish { reason, .. })) = chunks.last()
            else {
                panic!("finish chunk for {wire}");
            };
            assert_eq!(*reason, FinishReason::Known(kind.clone()));
            translator.end_of_stream().expect("settled");
        }
    }
    /// A recorded dsh response body: heartbeat comments between frames,
    /// the full event vocabulary of one tool-using run, replayed through
    /// the real SSE parser and translator.
    #[test]
    fn dsh_recorded_sse_replays_chunks() {
        let body = r#": ping

event: message_start
data: {"type":"message_start","message":{"usage":{"input_tokens":5,"output_tokens":1,"cache_read_input_tokens":2}}}

: ping

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"sh","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"cmd\":\"ls\"}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}

event: message_stop
data: {"type":"message_stop"}

"#;
        let events = crate::sse::parse_sse(body, &mut || {}).expect("recorded body parses");
        let mut translator = Translator::new("deepseek-flash");
        let mut chunks = Vec::new();
        for value in &events {
            let object = value.as_object().expect("validated objects");
            chunks.extend(translator.translate_event(object).expect("translate"));
        }
        translator.end_of_stream().expect("settled");
        // Two heartbeats pulse before the frames; the chunk list carries
        // only the translated stream.
        assert_eq!(chunks.len(), 9);
        assert_eq!(
            chunks[0],
            StreamChunk::Known(KnownStreamChunk::BlockStart {
                index: 0,
                block_type: "text".to_owned(),
            })
        );
        assert_eq!(
            chunks[3],
            StreamChunk::Known(KnownStreamChunk::BlockStart {
                index: 1,
                block_type: "tool-call".to_owned(),
            })
        );
        assert_eq!(
            chunks[4],
            StreamChunk::Known(KnownStreamChunk::ToolCallDelta {
                index: 1,
                id: ToolCallId::new("toolu_1"),
                name: Some("sh".to_owned()),
                arguments_delta: String::new(),
            })
        );
        assert_eq!(
            chunks[6],
            StreamChunk::Known(KnownStreamChunk::BlockEnd {
                index: 1,
                block: ContentBlock::Known(KnownContentBlock::ToolCall {
                    id: ToolCallId::new("toolu_1"),
                    name: "sh".to_owned(),
                    arguments: "{\"cmd\":\"ls\"}".to_owned(),
                }),
            })
        );
        let StreamChunk::Known(KnownStreamChunk::Usage { usage }) = &chunks[7] else {
            panic!("usage chunk");
        };
        assert_eq!(usage.input_tokens, 5);
        assert_eq!(usage.output_tokens, 9);
        assert_eq!(usage.cache_read_tokens, Some(2));
        assert_eq!(usage.total_tokens, Some(16));
        let StreamChunk::Known(KnownStreamChunk::Finish { replay_state, .. }) = &chunks[8] else {
            panic!("finish chunk");
        };
        let envelope = replay_state.as_ref().expect("replay envelope");
        assert_eq!(
            envelope.response,
            serde_json::json!({"kind": "deepseek-messages", "version": 1, "model": "deepseek-flash"})
        );
    }
}
