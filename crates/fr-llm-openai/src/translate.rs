//! Stream translation for Chat Completions chunks (pi-ai `stream()`,
//! openai-completions.ts:304-734): one `ChatCompletionChunk` JSON object
//! per call, `StreamChunk`s out.
//!
//! Block model: one text block, one reasoning block, and N tool-call
//! blocks keyed by stream index then id — the dsh singleton/lazily-opened
//! structure. All blocks close at stream end (dsh `finishBlock` loop);
//! tool arguments parse leniently at close (`parseStreamingJson` on the
//! accumulated raw deltas — the dsh patch keeps that single call site,
//! Q75-C) and stringify back to the durable raw form. The reasoning
//! replay signature is the `reasoning_details` buffer's JSON (D12
//! envelope, kind `fr-openai`); the `opencode-go` signature-correction
//! arm is dead (that provider never appears in fr route tables).

use fr_llm::error::LlmError;
use fr_wire::call_config::{FinishReason, FinishReasonKind, ReplayEnvelope, TokenUsage};
use fr_wire::{
    ContentBlock, KnownContentBlock, KnownStreamChunk, RawObject, StreamChunk, ToolCallId,
};
use serde_json::Value;

use crate::json_parse::parse_streaming_json;
use crate::usage::{map_stop_reason, parse_chunk_usage};

/// The reasoning delta fields dsh probes in order; the first non-empty
/// string wins (some endpoints duplicate content across two fields).
const REASONING_FIELDS: [&str; 3] = ["reasoning_content", "reasoning", "reasoning_text"];

/// One open stream block: lazily opened, closed at stream end.
#[derive(Debug)]
enum OpenBlock {
    /// Visible text.
    Text { text: String },
    /// Reasoning; the signature rides the last field that fed it.
    Reasoning {
        text: String,
        signature: Option<&'static str>,
    },
    /// A tool call accumulated from raw argument deltas.
    ToolCall {
        id: String,
        name: String,
        partial_arguments: String,
    },
}

impl OpenBlock {
    const fn kind(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::Reasoning { .. } => "reasoning",
            Self::ToolCall { .. } => "tool-call",
        }
    }
}

/// The chunk state machine for one request.
#[derive(Debug)]
pub struct Translator {
    model: String,
    blocks: Vec<OpenBlock>,
    /// Stream-index → block position (dsh `toolCallBlocksByIndex`).
    by_index: std::collections::HashMap<u64, usize>,
    /// Call id → block position (dsh `toolCallBlocksById`).
    by_id: std::collections::HashMap<String, usize>,
    usage: TokenUsage,
    response_id: Option<String>,
    response_model: Option<String>,
    reason: Option<FinishReason>,
    has_finish_reason: bool,
    reasoning_details: Vec<Value>,
    settled: bool,
    /// Whether streams carry `finish_reason` (compat gate for the
    /// terminal-inference arm).
    supports_finish_reason: bool,
}

impl Translator {
    /// A translator for one request of `model`; the model id is stored in
    /// The usage accumulated so far (the error-path settlement emits it
    /// before the terminal finish, dsh `toStreamChunks` error arm).
    #[must_use]
    pub const fn usage(&self) -> TokenUsage {
        self.usage
    }

    /// Creates a translator for one request against the route model; the
    /// response id and model ride the durable replay state of the terminal
    /// finish, and the compat flag drives the terminal-inference arm
    /// (`supports-finish-reason`) when the provider never sends it.
    #[must_use]
    pub fn new(model: impl Into<String>, supports_finish_reason: bool) -> Self {
        Self {
            model: model.into(),
            blocks: Vec::new(),
            by_index: std::collections::HashMap::new(),
            by_id: std::collections::HashMap::new(),
            usage: TokenUsage::default(),
            response_id: None,
            response_model: None,
            reason: None,
            has_finish_reason: false,
            reasoning_details: Vec::new(),
            settled: false,
            supports_finish_reason,
        }
    }

    /// The local position of a tool-call block, opening one when neither
    /// the stream index nor the id matches (dsh `ensureToolCallBlock`).
    fn ensure_tool_block(
        &mut self,
        index: Option<u64>,
        id: Option<&str>,
        name: Option<&str>,
    ) -> (usize, bool) {
        if let Some(stream_index) = index
            && let Some(local) = self.by_index.get(&stream_index)
        {
            return (*local, false);
        }
        if let Some(id_text) = id
            && let Some(local) = self.by_id.get(id_text)
        {
            return (*local, false);
        }
        let local = self.blocks.len();
        self.blocks.push(OpenBlock::ToolCall {
            id: id.unwrap_or_default().to_owned(),
            name: name.unwrap_or_default().to_owned(),
            partial_arguments: String::new(),
        });
        if let Some(stream_index) = index {
            self.by_index.insert(stream_index, local);
        }
        if let Some(id_text) = id {
            self.by_id.insert(id_text.to_owned(), local);
        }
        (local, true)
    }

    /// The envelope faces: `responseId ||= chunk.id` (first non-empty
    /// wins), `responseModel ||= chunk.model` when it differs from the
    /// route model, and the chunk-level usage.
    fn absorb_envelope(&mut self, chunk: &RawObject) {
        if self.response_id.is_none()
            && let Some(id) = chunk.get("id").and_then(Value::as_str)
            && !id.is_empty()
        {
            self.response_id = Some(id.to_owned());
        }
        if self.response_model.is_none()
            && let Some(response_model) = chunk
                .get("model")
                .and_then(Value::as_str)
                .filter(|candidate| !candidate.is_empty() && *candidate != self.model)
        {
            self.response_model = Some(response_model.to_owned());
        }
        if let Some(usage) = chunk.get("usage").filter(|value| !value.is_null()) {
            self.usage = parse_chunk_usage(usage.as_object().expect("usage object"));
        }
    }

    /// Append text to the singleton text block, opening it on first use.
    fn append_text(&mut self, delta: &str) -> (usize, Vec<StreamChunk>) {
        let existing = self
            .blocks
            .iter()
            .position(|block| matches!(block, OpenBlock::Text { .. }));
        if let Some(local) = existing {
            if let OpenBlock::Text { text } = &mut self.blocks[local] {
                text.push_str(delta);
            }
            return (
                local,
                vec![StreamChunk::Known(KnownStreamChunk::TextDelta {
                    index: local as u64,
                    text: delta.to_owned(),
                })],
            );
        }
        let local = self.blocks.len();
        self.blocks.push(OpenBlock::Text {
            text: delta.to_owned(),
        });
        (
            local,
            vec![
                StreamChunk::Known(KnownStreamChunk::BlockStart {
                    index: local as u64,
                    block_type: "text".to_owned(),
                }),
                StreamChunk::Known(KnownStreamChunk::TextDelta {
                    index: local as u64,
                    text: delta.to_owned(),
                }),
            ],
        )
    }

    /// Append reasoning to the singleton reasoning block, opening it with
    /// the feeding field as its signature.
    fn append_reasoning(&mut self, delta: &str, field: &'static str) -> Vec<StreamChunk> {
        let existing = self
            .blocks
            .iter()
            .position(|block| matches!(block, OpenBlock::Reasoning { .. }));
        if let Some(local) = existing
            && let OpenBlock::Reasoning { text, signature } = &mut self.blocks[local]
        {
            text.push_str(delta);
            if signature.is_none() {
                *signature = Some(field);
            }
            return vec![StreamChunk::Known(KnownStreamChunk::ReasoningDelta {
                index: local as u64,
                text: delta.to_owned(),
            })];
        }
        let local = self.blocks.len();
        self.blocks.push(OpenBlock::Reasoning {
            text: delta.to_owned(),
            signature: Some(field),
        });
        vec![
            StreamChunk::Known(KnownStreamChunk::BlockStart {
                index: local as u64,
                block_type: "reasoning".to_owned(),
            }),
            StreamChunk::Known(KnownStreamChunk::ReasoningDelta {
                index: local as u64,
                text: delta.to_owned(),
            }),
        ]
    }

    /// Translate one parsed `ChatCompletionChunk` (dsh `for await` body).
    ///
    /// # Errors
    /// [`LlmError`] when a terminal error stop reason arrives
    /// (`Provider finish_reason: …`); the adapter maps it to the fr error
    /// channel (finish kind `error`). Chunks after settlement yield
    /// nothing (the dsh generator has returned).
    ///
    /// # Panics
    ///
    /// Never in practice: the usage-object expects guard on `usage` being
    /// a JSON object, which the provider grammar pins.
    pub fn translate_chunk(&mut self, chunk: &RawObject) -> Result<Vec<StreamChunk>, LlmError> {
        if self.settled {
            return Ok(Vec::new());
        }
        self.absorb_envelope(chunk);
        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        else {
            return Ok(Vec::new());
        };
        // Fallback: some providers (e.g. Moonshot) return usage on the
        // choice instead of the chunk.
        if chunk.get("usage").is_none()
            && let Some(usage) = choice.get("usage").filter(|value| !value.is_null())
        {
            self.usage = parse_chunk_usage(usage.as_object().expect("usage object"));
        }
        if let Some(raw_reason) = choice.get("finish_reason").filter(|value| !value.is_null()) {
            let reason_text = raw_reason.as_str().unwrap_or_default();
            self.reason = Some(map_stop_reason(Some(reason_text)));
            self.has_finish_reason = true;
        }
        let chunks = self.apply_delta(choice.get("delta"));
        if let Some(FinishReason::Known(FinishReasonKind::Error { failure })) = &self.reason {
            return Err(
                LlmError::try_new(&failure.message, &failure.code, None, None, None, None).expect(
                    "mapped stop-reason messages and codes satisfy the LlmError invariants",
                ),
            );
        }
        Ok(chunks)
    }

    /// The delta arm (dsh `choice.delta` body): visible content, the first
    /// non-empty reasoning field, tool-call accumulation, and the
    /// `reasoning_details` replay buffer.
    fn apply_delta(&mut self, delta: Option<&Value>) -> Vec<StreamChunk> {
        let Some(delta) = delta else {
            return Vec::new();
        };
        let mut chunks = Vec::new();
        if let Some(content) = delta
            .get("content")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            let (_, text_chunks) = self.append_text(content);
            chunks.extend(text_chunks);
        }
        // First non-empty reasoning field wins (dsh avoids duplicating
        // endpoints that stream two fields with the same content).
        let reasoning_field = REASONING_FIELDS.iter().find_map(|field| {
            delta
                .get(field)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(|text| (*field, text))
        });
        if let Some((field, text)) = reasoning_field {
            let signature = REASONING_FIELDS
                .iter()
                .position(|candidate| *candidate == field);
            let signature: &'static str = signature
                .map(|position| REASONING_FIELDS[position])
                .expect("field from the list");
            chunks.extend(self.append_reasoning(text, signature));
        }
        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in tool_calls {
                let index = call.get("index").and_then(Value::as_u64);
                let id = call.get("id").and_then(Value::as_str);
                let function = call.get("function");
                let name = function
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str);
                let (local, opened) = self.ensure_tool_block(index, id, name);
                if opened {
                    chunks.push(StreamChunk::Known(KnownStreamChunk::BlockStart {
                        index: local as u64,
                        block_type: "tool-call".to_owned(),
                    }));
                }
                let arguments_delta = function
                    .and_then(|function| function.get("arguments"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !arguments_delta.is_empty()
                    && let OpenBlock::ToolCall {
                        partial_arguments, ..
                    } = &mut self.blocks[local]
                {
                    partial_arguments.push_str(arguments_delta);
                }
                let OpenBlock::ToolCall {
                    id: call_id,
                    name: call_name,
                    ..
                } = &self.blocks[local]
                else {
                    continue;
                };
                chunks.push(StreamChunk::Known(KnownStreamChunk::ToolCallDelta {
                    index: local as u64,
                    id: ToolCallId::new(call_id.clone()),
                    name: (!call_name.is_empty()).then(|| call_name.clone()),
                    arguments_delta: arguments_delta.to_owned(),
                }));
            }
        }
        // `reasoning_details` are replay metadata, not user-visible deltas;
        // merged per dsh `appendOpenAIReasoningDetail` (text/summary merge
        // into the previous entry, encrypted entries stay discrete).
        if let Some(details) = delta.get("reasoning_details").and_then(Value::as_array)
            && !details.is_empty()
        {
            // dsh `ensureThinkingBlock("")`: the details open a reasoning
            // block even without any reasoning-text delta.
            let has_reasoning = self
                .blocks
                .iter()
                .any(|block| matches!(block, OpenBlock::Reasoning { .. }));
            if !has_reasoning {
                chunks.extend(self.append_reasoning("", "reasoning_content"));
            }
            for detail in details {
                merge_reasoning_detail(&mut self.reasoning_details, detail);
            }
        }
        chunks
    }

    /// Close every open block and settle the run (dsh `finishBlock` loop
    /// plus the post-loop terminal checks and `done` push).
    ///
    /// # Errors
    /// [`LlmError`] when a `supports-finish-reason` endpoint ended without
    /// a `finish_reason` (dsh throws `Stream ended without finish_reason`).
    pub fn end_of_stream(&mut self) -> Result<Vec<StreamChunk>, LlmError> {
        if self.settled {
            return Ok(Vec::new());
        }
        let mut chunks = Vec::new();
        let saw_tool_calls = self
            .blocks
            .iter()
            .any(|block| matches!(block, OpenBlock::ToolCall { .. }));
        // Kinds captured before the close loop replaces the blocks (the
        // replay envelope needs them after).
        let kinds: Vec<&'static str> = self.blocks.iter().map(OpenBlock::kind).collect();
        for local in 0..self.blocks.len() {
            let block = std::mem::replace(
                &mut self.blocks[local],
                OpenBlock::Text {
                    text: String::new(),
                },
            );
            let content = match block {
                OpenBlock::Text { text } => KnownContentBlock::Text { text },
                OpenBlock::Reasoning { text, .. } => KnownContentBlock::Reasoning { text },
                OpenBlock::ToolCall {
                    id,
                    name,
                    partial_arguments,
                } => {
                    // The single toolcall_end lenient parse (Q75-C): parsed
                    // then stringified back to the durable raw form.
                    let parsed = parse_streaming_json(Some(&partial_arguments));
                    KnownContentBlock::ToolCall {
                        id: ToolCallId::new(id),
                        name,
                        arguments: parsed.to_string(),
                    }
                }
            };
            chunks.push(StreamChunk::Known(KnownStreamChunk::BlockEnd {
                index: local as u64,
                block: ContentBlock::Known(content),
            }));
        }

        // dsh post-loop: without a finish reason on a non-supporting
        // endpoint the content decides (tool calls → toolUse, else stop);
        // a supporting endpoint without one is a thrown error.
        let reason = match self.reason.take() {
            Some(reason) => reason,
            None if !self.has_finish_reason && !self.supports_finish_reason => {
                if saw_tool_calls {
                    FinishReason::Known(FinishReasonKind::ToolCalls)
                } else {
                    FinishReason::Known(FinishReasonKind::Stop)
                }
            }
            None => {
                return Err(static_error(
                    "Stream ended without finish_reason",
                    fr_llm::error::SERVER_CODE,
                ));
            }
        };
        self.settled = true;
        chunks.push(StreamChunk::Known(KnownStreamChunk::Usage {
            usage: self.usage,
        }));
        chunks.push(StreamChunk::Known(KnownStreamChunk::Finish {
            reason,
            replay_state: Some(self.replay_envelope(&kinds)),
        }));
        Ok(chunks)
    }

    /// The D12 replay envelope: kind `fr-openai`, the route model, and one
    /// block entry per emitted block — reasoning blocks carry their merged
    /// `reasoning_details` JSON as the signature (the dsh
    /// `thinkingSignature`), text and tool-call blocks carry their type
    /// only.
    fn replay_envelope(&self, kinds: &[&'static str]) -> ReplayEnvelope {
        let blocks: Vec<Value> = kinds
            .iter()
            .map(|kind| {
                if *kind == "reasoning" {
                    serde_json::json!({
                        "type": "reasoning",
                        "signature": serde_json::to_string(&self.reasoning_details)
                            .unwrap_or_else(|_| "[]".to_owned()),
                    })
                } else {
                    serde_json::json!({ "type": kind })
                }
            })
            .collect();
        ReplayEnvelope {
            response: serde_json::json!({
                "kind": "fr-openai",
                "version": 1,
                "model": self.model,
            }),
            blocks: Some(blocks),
        }
    }

    /// The response ids echoed back for durable metadata
    /// (`response.id` / `response.model`).
    #[must_use]
    pub const fn response_ids(&self) -> (&Option<String>, &Option<String>) {
        (&self.response_id, &self.response_model)
    }
}

/// `appendOpenAIReasoningDetail` (constrained-sampling siblings at
/// openai-completions.ts:257-271): consecutive text/summary deltas merge
/// into the previous entry; anything else appends discrete.
fn merge_reasoning_detail(buffer: &mut Vec<Value>, detail: &Value) {
    let Some(kind) = detail.get("type").and_then(Value::as_str) else {
        buffer.push(detail.clone());
        return;
    };
    let merge_field = match kind {
        "reasoning.text" => Some("text"),
        "reasoning.summary" => Some("summary"),
        _ => None,
    };
    let Some(field) = merge_field else {
        buffer.push(detail.clone());
        return;
    };
    if let Some(last) = buffer.last_mut()
        && last.get("type").and_then(Value::as_str) == Some(kind)
    {
        let merged = last
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let appended = detail
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let Some(slot) = last.get_mut(field) {
            *slot = Value::String(format!("{merged}{appended}"));
        }
        return;
    }
    buffer.push(detail.clone());
}

fn static_error(message: &str, code: &str) -> LlmError {
    LlmError::try_new(message, code, None, None, None, None)
        .expect("static message and code satisfy the LlmError invariants")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chunk(body: serde_json::Value) -> RawObject {
        serde_json::from_value::<RawObject>(body).expect("object")
    }

    /// The full happy path: text delta, reasoning delta (with the field
    /// name as the replay signature), a tool call accumulated across two
    /// deltas, usage, and the finish mapping — with the replay envelope
    /// carrying the merged `reasoning_details` JSON.
    fn kind_labels(chunks: &[StreamChunk]) -> Vec<String> {
        chunks
            .iter()
            .map(|chunk| match chunk {
                StreamChunk::Known(known) => match known {
                    KnownStreamChunk::BlockStart { block_type, .. } => {
                        format!("start:{block_type}")
                    }
                    KnownStreamChunk::TextDelta { .. } => "text-delta".to_owned(),
                    KnownStreamChunk::ReasoningDelta { .. } => "reasoning-delta".to_owned(),
                    KnownStreamChunk::ToolCallDelta { .. } => "tool-call-delta".to_owned(),
                    KnownStreamChunk::BlockEnd { .. } => "block-end".to_owned(),
                    KnownStreamChunk::Usage { .. } => "usage".to_owned(),
                    KnownStreamChunk::Finish { .. } => "finish".to_owned(),
                },
                StreamChunk::Other(_) => "other".to_owned(),
            })
            .collect()
    }

    #[test]
    fn translates_text_reasoning_and_tool_calls() {
        let mut translator = Translator::new("m", true);
        let mut out = Vec::new();
        out.extend(
            translator
                .translate_chunk(&chunk(json!({"id": "resp-1", "model": "m", "choices": []})))
                .expect("empty choices chunk"),
        );
        out.extend(
            translator
                .translate_chunk(&chunk(json!({
                    "choices": [{"delta": {"content": "hello"}}],
                })))
                .expect("text delta"),
        );
        out.extend(
            translator
                .translate_chunk(&chunk(json!({
                    "choices": [{"delta": {"reasoning_content": "think"}}],
                })))
                .expect("reasoning delta"),
        );
        out.extend(
            translator
                .translate_chunk(&chunk(json!({
                    "choices": [{"delta": {"tool_calls": [
                        {"index": 0, "id": "c1", "function": {"name": "sh", "arguments": "{\"cm"}},
                        {"index": 0, "function": {"arguments": "d\":\"ls\"}}"}},
                    ]}}],
                })))
                .expect("tool deltas"),
        );
        out.extend(
            translator
                .translate_chunk(&chunk(json!({
                    "choices": [{"finish_reason": "tool_calls", "delta": {}}],
                    "usage": {"prompt_tokens": 10, "completion_tokens": 5},
                })))
                .expect("finish chunk"),
        );
        let tail = translator.end_of_stream().expect("settles");
        out.extend(tail);

        // Order: text start+delta, reasoning start+delta, tool start+2
        // deltas, block ends ×3, usage, finish.
        assert_eq!(
            kind_labels(&out),
            [
                "start:text",
                "text-delta",
                "start:reasoning",
                "reasoning-delta",
                "start:tool-call",
                "tool-call-delta",
                "tool-call-delta",
                "block-end",
                "block-end",
                "block-end",
                "usage",
                "finish",
            ]
        );
        // The assembled tool call carries the lenient-parsed arguments
        // stringified back.
        let Some(StreamChunk::Known(KnownStreamChunk::BlockEnd { index, block })) =
            out.iter().find(|chunk| {
                matches!(
                    chunk,
                    StreamChunk::Known(KnownStreamChunk::BlockEnd {
                        block: ContentBlock::Known(KnownContentBlock::ToolCall { .. }),
                        ..
                    })
                )
            })
        else {
            panic!("tool block end expected");
        };
        assert_eq!(*index, 2);
        let ContentBlock::Known(KnownContentBlock::ToolCall {
            id,
            name,
            arguments,
        }) = block
        else {
            unreachable!("narrowed above");
        };
        assert_eq!(id.as_str(), "c1");
        assert_eq!(name, "sh");
        assert_eq!(arguments, r#"{"cmd":"ls"}"#);
        // The finish reason maps tool_calls → tool-calls; usage rides first.
        let Some(StreamChunk::Known(KnownStreamChunk::Usage { usage })) = out
            .iter()
            .rev()
            .find(|chunk| matches!(chunk, StreamChunk::Known(KnownStreamChunk::Usage { .. })))
        else {
            panic!("usage chunk expected");
        };
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 5);
        let Some(StreamChunk::Known(KnownStreamChunk::Finish { reason, .. })) = out.last() else {
            panic!("finish expected last");
        };
        assert_eq!(*reason, FinishReason::Known(FinishReasonKind::ToolCalls));
    }

    /// The replay envelope: kind `fr-openai`, the route model, and the
    /// merged `reasoning_details` JSON as the reasoning block signature.
    #[test]
    fn carries_fr_openai_replay_envelope() {
        let mut translator = Translator::new("m", true);
        translator
            .translate_chunk(&chunk(json!({
                "choices": [{"delta": {
                    "reasoning_details": [
                        {"type": "reasoning.text", "text": "a"},
                        {"type": "reasoning.text", "text": "b"},
                    ],
                }}],
            })))
            .expect("reasoning details chunk");
        translator
            .translate_chunk(&chunk(json!({
                "choices": [{"finish_reason": "stop", "delta": {}}],
            })))
            .expect("finish chunk");
        let tail = translator.end_of_stream().expect("settles");
        let Some(StreamChunk::Known(KnownStreamChunk::Finish {
            replay_state: Some(envelope),
            ..
        })) = tail.last()
        else {
            panic!("finish with envelope expected");
        };
        assert_eq!(envelope.response["kind"], "fr-openai");
        assert_eq!(envelope.response["model"], "m");
        let blocks = envelope.blocks.as_ref().expect("blocks");
        assert_eq!(blocks[0]["type"], "reasoning");
        // Two consecutive text entries merged into one signature JSON.
        let signature = blocks[0]["signature"].as_str().expect("signature");
        let details: Vec<Value> = serde_json::from_str(signature).expect("details array");
        assert_eq!(details.len(), 1);
        assert_eq!(details[0]["text"], "ab");
    }

    /// A supporting endpoint that ends without `finish_reason` throws; a
    /// non-supporting one infers the reason from the content (tool calls →
    /// tool-calls, else stop).
    #[test]
    fn finish_reason_gates() {
        let mut strict = Translator::new("m", true);
        strict
            .translate_chunk(&chunk(json!({
                "choices": [{"delta": {"content": "hi"}}],
            })))
            .expect("text chunk");
        let error = strict.end_of_stream().expect_err("strict throws");
        assert_eq!(error.message(), "Stream ended without finish_reason");

        let mut lenient = Translator::new("m", false);
        lenient
            .translate_chunk(&chunk(json!({
                "choices": [{"delta": {"content": "hi"}}],
            })))
            .expect("text chunk");
        let tail = lenient.end_of_stream().expect("infers stop");
        let Some(StreamChunk::Known(KnownStreamChunk::Finish { reason, .. })) = tail.last() else {
            panic!("finish expected");
        };
        assert_eq!(*reason, FinishReason::Known(FinishReasonKind::Stop));
    }

    /// An error finish reason throws with the verbatim mapped message (the
    /// adapter turns it into the fr error channel).
    #[test]
    fn error_finish_reason_throws() {
        let mut translator = Translator::new("m", true);
        let error = translator
            .translate_chunk(&chunk(json!({
                "choices": [{"finish_reason": "content_filter", "delta": {}}],
            })))
            .expect_err("content_filter throws");
        assert_eq!(error.message(), "Provider finish_reason: content_filter");
    }
}
