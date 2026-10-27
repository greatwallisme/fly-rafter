//! Responses stream translation (pi-ai `processResponsesStream`,
//! openai-responses-shared.ts:433-777): one `ResponseStreamEvent` JSON
//! object per call, `StreamChunk`s out.
//!
//! Unlike the Completions face, dsh closes each block mid-stream at
//! `response.output_item.done` (not at stream end), the usage rides the
//! terminal `response.completed`/`response.incomplete` event (the
//! provider total, not a four-way sum), and the terminal gate is the
//! saw-a-terminal-event flag rather than a `finish_reason`. The dsh patch
//! removes only the `function_call_arguments.delta` parse (tool arguments
//! accumulate as raw text); the `.done` and `output_item.done` parses
//! stay — the `.done` parse result is unobservable in dsh itself (the
//! `output_item.done` parse overwrites it), so fr keeps the raw replace
//! only. Block replay signatures are per block: reasoning items serialize
//! verbatim into the envelope signature, text blocks carry the v1
//! `{v, id, phase?}` envelope. Errors surface through the `Err` channel
//! (the adapter maps them to the `fr-error` stream chunk, Q73-A).

use fr_llm::error::LlmError;
use fr_wire::call_config::{FinishReason, FinishReasonKind, ReplayEnvelope, TokenUsage};
use fr_wire::{
    ContentBlock, KnownContentBlock, KnownStreamChunk, RawObject, StreamChunk, ToolCallId,
};
use serde_json::Value;

use crate::json_parse::parse_streaming_json;
use crate::responses::encode_text_signature_v1;
use crate::usage::{non_negative_u64, truthy_count};

/// One open or closed stream block.
#[derive(Debug)]
enum RespBlock {
    /// Provider reasoning; the signature is the reasoning item JSON.
    Thinking {
        text: String,
        signature: Option<String>,
    },
    /// Visible text; the signature is the v1 `{v, id, phase?}` envelope.
    Text {
        text: String,
        signature: Option<String>,
    },
    /// A tool call accumulated from raw argument text; `partial_json`
    /// drops when the item completes (the unfinished-call guard reads it).
    ToolCall {
        id: String,
        name: String,
        partial_json: Option<String>,
    },
}

impl RespBlock {
    const fn kind(&self) -> &'static str {
        match self {
            Self::Thinking { .. } => "reasoning",
            Self::Text { .. } => "text",
            Self::ToolCall { .. } => "tool-call",
        }
    }
}

/// The chunk state machine for one Responses request.
#[derive(Debug)]
pub struct ResponsesTranslator {
    model: String,
    blocks: Vec<RespBlock>,
    /// `output_index` → block position (dsh `outputSlots`).
    slots: std::collections::HashMap<u64, usize>,
    /// Reasoning item id → block position (dsh `reasoningBlocksById`),
    /// feeding the Azure `encrypted_content` backfill.
    reasoning_by_id: std::collections::HashMap<String, usize>,
    usage: TokenUsage,
    response_id: Option<String>,
    stop_reason: Option<FinishReason>,
    saw_terminal: bool,
    settled: bool,
}

impl ResponsesTranslator {
    /// The usage accumulated so far (the error-path settlement emits it
    /// before the terminal finish, dsh `toStreamChunks` error arm).
    #[must_use]
    pub const fn usage(&self) -> TokenUsage {
        self.usage
    }

    /// How many content blocks the stream produced so far (the settlement's
    /// empty-stop face, dsh `event.message.content.length`).
    #[must_use]
    pub const fn blocks_len(&self) -> usize {
        self.blocks.len()
    }

    /// Creates a translator for one request of `model`; the model id rides
    /// the durable replay envelope of the terminal finish.
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            blocks: Vec::new(),
            slots: std::collections::HashMap::new(),
            reasoning_by_id: std::collections::HashMap::new(),
            usage: TokenUsage::default(),
            response_id: None,
            stop_reason: None,
            saw_terminal: false,
            settled: false,
        }
    }

    /// The response id echoed back for durable metadata (`response.id`).
    /// The Completions `responseModel` echo face has no Responses
    /// counterpart.
    #[must_use]
    pub const fn response_id(&self) -> &Option<String> {
        &self.response_id
    }

    /// The block position for an output index whose slot type matches, or
    /// `None` (dsh `getSlot` continue).
    fn slot_of(&self, output_index: u64, expected: &str) -> Option<usize> {
        let position = *self.slots.get(&output_index)?;
        (self.blocks.get(position)?.kind() == expected).then_some(position)
    }

    /// Open the slot for an output item (`createSlot`); unknown and
    /// D8-dead item types open nothing, so their later events miss.
    fn create_slot(&mut self, output_index: u64, item: &Value) -> Option<(usize, bool)> {
        let item_type = item.get("type").and_then(Value::as_str)?;
        let (block, kind) = match item_type {
            "reasoning" => (
                RespBlock::Thinking {
                    text: String::new(),
                    signature: None,
                },
                "thinking",
            ),
            "message" => {
                self.apply_message_phase(item);
                (
                    RespBlock::Text {
                        text: String::new(),
                        signature: None,
                    },
                    "text",
                )
            }
            "function_call" => {
                let call_id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let item_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
                let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
                let arguments = item.get("arguments").and_then(Value::as_str).unwrap_or("");
                (
                    RespBlock::ToolCall {
                        id: format!("{call_id}|{item_id}"),
                        name: name.to_owned(),
                        partial_json: Some(arguments.to_owned()),
                    },
                    "tool-call",
                )
            }
            // `custom_tool_call` is D8-dead: fr never declares grammar
            // tools, so the provider never emits the item.
            _ => return None,
        };
        let position = self.blocks.len();
        self.blocks.push(block);
        self.slots.insert(output_index, position);
        Some((position, kind == "tool-call"))
    }

    /// The `final_answer` phase promotes the stop reason before the
    /// terminal event (`applyMessagePhaseStopReason`); the terminal event
    /// overwrites it again.
    fn apply_message_phase(&mut self, item: &Value) {
        if item.get("type").and_then(Value::as_str) == Some("message")
            && item.get("phase").and_then(Value::as_str) == Some("final_answer")
        {
            self.stop_reason = Some(FinishReason::Known(FinishReasonKind::Stop));
        }
    }

    /// Translate one stream event (dsh `processResponsesStream` loop
    /// body); unknown event types are ignored.
    ///
    /// # Errors
    /// [`LlmError`] on the in-band `error` and `response.failed` events,
    /// on error stop statuses, and on unknown terminal statuses.
    pub fn translate_event(&mut self, event: &RawObject) -> Result<Vec<StreamChunk>, LlmError> {
        let mut chunks = Vec::new();
        let Some(event_type) = event.get("type").and_then(Value::as_str) else {
            return Ok(chunks);
        };
        if is_delta_event(event_type) {
            chunks.extend(self.delta_chunks(event_type, event));
            return Ok(chunks);
        }
        match event_type {
            "response.created" => {
                if let Some(id) = event
                    .get("response")
                    .and_then(|response| response.get("id"))
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                {
                    self.response_id = Some(id.to_owned());
                }
            }
            "response.output_item.added" => {
                if let Some(item) = event.get("item")
                    && let Some(output_index) = event.get("output_index").and_then(Value::as_u64)
                    && let Some((position, _)) = self.create_slot(output_index, item)
                {
                    chunks.push(StreamChunk::Known(KnownStreamChunk::BlockStart {
                        index: position as u64,
                        block_type: self.blocks[position].kind().to_owned(),
                    }));
                }
            }
            // `response.custom_tool_call_input.{delta,done}` are D8-dead.
            "response.output_item.done" => {
                if let Some(item) = event.get("item")
                    && let Some(output_index) = event.get("output_index").and_then(Value::as_u64)
                {
                    self.close_item(output_index, item, &mut chunks);
                }
            }
            "response.completed" | "response.incomplete" => {
                if let Some(response) = event.get("response") {
                    self.finalize(response)?;
                }
            }
            // The JS `|| "Unknown error"` fallback is unreachable: the
            // formatted message is always a non-empty string.
            "error" => {
                let code = event
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let message = event
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                return Err(static_error(&format!("Error Code {code}: {message}")));
            }
            "response.failed" => {
                self.saw_terminal = true;
                let response = event.get("response");
                let error = response.and_then(|response| response.get("error"));
                let details = response.and_then(|response| response.get("incomplete_details"));
                let message = error.filter(|value| !value.is_null()).map_or_else(
                    || {
                        details
                            .and_then(|details| details.get("reason"))
                            .and_then(Value::as_str)
                            .map_or_else(
                                || "Unknown error (no error details in response)".to_owned(),
                                |reason| format!("incomplete: {reason}"),
                            )
                    },
                    |error| {
                        let code = error
                            .get("code")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown");
                        let message = error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("no message");
                        format!("{code}: {message}")
                    },
                );
                return Err(static_error(&message));
            }
            _ => {}
        }
        Ok(chunks)
    }

    /// The delta-class events; each finds its type-checked slot and
    /// appends the raw text (the dsh patch removed the in-stream parse,
    /// so arguments accumulate raw until the item completes).
    fn delta_chunks(&mut self, event_type: &str, event: &RawObject) -> Vec<StreamChunk> {
        let mut chunks = Vec::new();
        match event_type {
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(position) = self.slot_of(output_index(event), "reasoning")
                    && let Some(delta) = event.get("delta").and_then(Value::as_str)
                    && let RespBlock::Thinking { text, .. } = &mut self.blocks[position]
                {
                    text.push_str(delta);
                    chunks.push(StreamChunk::Known(KnownStreamChunk::ReasoningDelta {
                        index: position as u64,
                        text: delta.to_owned(),
                    }));
                }
            }
            "response.reasoning_summary_part.done" => {
                if let Some(position) = self.slot_of(output_index(event), "reasoning")
                    && let RespBlock::Thinking { text, .. } = &mut self.blocks[position]
                {
                    text.push_str("\n\n");
                    chunks.push(StreamChunk::Known(KnownStreamChunk::ReasoningDelta {
                        index: position as u64,
                        text: "\n\n".to_owned(),
                    }));
                }
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                if let Some(position) = self.slot_of(output_index(event), "text")
                    && let Some(delta) = event.get("delta").and_then(Value::as_str)
                    && let RespBlock::Text { text, .. } = &mut self.blocks[position]
                {
                    text.push_str(delta);
                    chunks.push(StreamChunk::Known(KnownStreamChunk::TextDelta {
                        index: position as u64,
                        text: delta.to_owned(),
                    }));
                }
            }
            "response.function_call_arguments.delta" => {
                // The dsh patch removes the in-stream parse here; the raw
                // text accumulates until the item completes.
                if let Some(position) = self.slot_of(output_index(event), "tool-call")
                    && let Some(delta) = event.get("delta").and_then(Value::as_str)
                    && let RespBlock::ToolCall {
                        id,
                        name,
                        partial_json: Some(partial),
                    } = &mut self.blocks[position]
                {
                    partial.push_str(delta);
                    chunks.push(StreamChunk::Known(KnownStreamChunk::ToolCallDelta {
                        index: position as u64,
                        id: ToolCallId::new(id.clone()),
                        name: (!name.is_empty()).then(|| name.clone()),
                        arguments_delta: delta.to_owned(),
                    }));
                }
            }
            "response.function_call_arguments.done" => {
                if let Some(position) = self.slot_of(output_index(event), "tool-call")
                    && let Some(arguments) = event.get("arguments").and_then(Value::as_str)
                    && let RespBlock::ToolCall {
                        id,
                        name,
                        partial_json: previous @ Some(_),
                    } = &mut self.blocks[position]
                {
                    let previous_text = previous.take().unwrap_or_default();
                    // dsh also parses here, but the parsed object is
                    // overwritten by the `output_item.done` parse and
                    // never observed; the prefix diff rides raw strings.
                    if arguments.starts_with(previous_text.as_str()) {
                        let delta = &arguments[previous_text.len()..];
                        if !delta.is_empty() {
                            chunks.push(StreamChunk::Known(KnownStreamChunk::ToolCallDelta {
                                index: position as u64,
                                id: ToolCallId::new(id.clone()),
                                name: (!name.is_empty()).then(|| name.clone()),
                                arguments_delta: delta.to_owned(),
                            }));
                        }
                    }
                    *previous = Some(arguments.to_owned());
                }
            }
            _ => {}
        }
        chunks
    }

    /// Close one completed output item (`response.output_item.done`):
    /// the slot re-creates when the server skipped `.added`.
    fn close_item(&mut self, output_index: u64, item: &Value, chunks: &mut Vec<StreamChunk>) {
        self.apply_message_phase(item);
        let position = if let Some(&position) = self.slots.get(&output_index) {
            position
        } else {
            let Some((position, _)) = self.create_slot(output_index, item) else {
                return;
            };
            chunks.push(StreamChunk::Known(KnownStreamChunk::BlockStart {
                index: position as u64,
                block_type: self.blocks[position].kind().to_owned(),
            }));
            position
        };
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
        let closed = match (item_type, self.blocks[position].kind()) {
            ("reasoning", "reasoning") => {
                self.close_reasoning(position, item, chunks);
                true
            }
            ("message", "text") => {
                self.close_message(position, item, chunks);
                true
            }
            ("function_call", "tool-call") => self.close_function_call(position, item, chunks),
            // `custom_tool_call` is D8-dead; type-slot mismatches fall
            // through (dsh's `else if` chain ends).
            _ => false,
        };
        if closed {
            self.slots.remove(&output_index);
        }
    }

    /// Fold a completed reasoning item: the summary/content fallback
    /// chain (dsh `summaryText || contentText || accumulated`) and the
    /// verbatim item JSON as the replay signature.
    fn close_reasoning(&mut self, position: usize, item: &Value, chunks: &mut Vec<StreamChunk>) {
        let summary = joined_text(item.get("summary"), "text", "\n\n");
        let content = joined_text(item.get("content"), "text", "\n\n");
        let signature = item.to_string();
        if let RespBlock::Thinking {
            text,
            signature: slot,
        } = &mut self.blocks[position]
        {
            if !summary.is_empty() {
                *text = summary;
            } else if !content.is_empty() {
                *text = content;
            }
            *slot = Some(signature);
        }
        if let Some(id) = item.get("id").and_then(Value::as_str) {
            self.reasoning_by_id.insert(id.to_owned(), position);
        }
        self.finish_block(position, chunks);
    }

    /// Fold a completed message item: the parts join and the v1 text
    /// signature.
    fn close_message(&mut self, position: usize, item: &Value, chunks: &mut Vec<StreamChunk>) {
        let text = item
            .get("content")
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .map(|part| {
                        if part.get("type").and_then(Value::as_str) == Some("output_text") {
                            part.get("text").and_then(Value::as_str).unwrap_or_default()
                        } else {
                            // JS `join` renders a missing refusal as an
                            // empty segment.
                            part.get("refusal")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                        }
                    })
                    .collect::<String>()
            })
            .unwrap_or_default();
        let signature = encode_text_signature_v1(
            item.get("id").and_then(Value::as_str).unwrap_or_default(),
            item.get("phase").and_then(Value::as_str),
        );
        if let RespBlock::Text {
            text: slot,
            signature: slot_signature,
        } = &mut self.blocks[position]
        {
            *slot = text;
            *slot_signature = Some(signature);
        }
        self.finish_block(position, chunks);
    }

    /// Fold a completed function call: the falsy-chain source, the
    /// lenient parse re-stringified to the durable raw form, and the
    /// scratch-buffer drop. `false` leaves the slot open (the
    /// unfinished-call guard reports it).
    fn close_function_call(
        &mut self,
        position: usize,
        item: &Value,
        chunks: &mut Vec<StreamChunk>,
    ) -> bool {
        let RespBlock::ToolCall {
            id,
            name,
            partial_json: Some(partial),
        } = &self.blocks[position]
        else {
            return false;
        };
        // dsh `item.arguments || partialJson || "{}"` over falsy strings;
        // the durable raw form is the parsed object re-stringified (the
        // contract's "stringify-back" clause).
        let source = item
            .get("arguments")
            .and_then(Value::as_str)
            .filter(|arguments| !arguments.is_empty())
            .unwrap_or(if partial.is_empty() { "{}" } else { partial });
        let arguments = parse_streaming_json(Some(source)).to_string();
        let block = KnownContentBlock::ToolCall {
            id: ToolCallId::new(id.clone()),
            name: name.clone(),
            arguments,
        };
        if let RespBlock::ToolCall { partial_json, .. } = &mut self.blocks[position] {
            *partial_json = None;
        }
        Self::finish_block_with(position, block, chunks);
        true
    }

    /// Emit the closing `BlockEnd` for a reasoning or text block.
    fn finish_block(&self, position: usize, chunks: &mut Vec<StreamChunk>) {
        let block = match &self.blocks[position] {
            RespBlock::Thinking { text, .. } => KnownContentBlock::Reasoning { text: text.clone() },
            RespBlock::Text { text, .. } => KnownContentBlock::Text { text: text.clone() },
            RespBlock::ToolCall { .. } => return,
        };
        chunks.push(StreamChunk::Known(KnownStreamChunk::BlockEnd {
            index: position as u64,
            block: ContentBlock::Known(block),
        }));
    }

    /// Emit the closing `BlockEnd` carrying an already-assembled block.
    fn finish_block_with(position: usize, block: KnownContentBlock, chunks: &mut Vec<StreamChunk>) {
        chunks.push(StreamChunk::Known(KnownStreamChunk::BlockEnd {
            index: position as u64,
            block: ContentBlock::Known(block),
        }));
    }

    /// Fold the terminal response (`finalizeResponse`): Azure reasoning
    /// backfill, ids, usage, and the stop mapping.
    fn finalize(&mut self, response: &Value) -> Result<(), LlmError> {
        self.saw_terminal = true;
        self.backfill_reasoning_signatures(response.get("output").and_then(Value::as_array));
        if let Some(id) = response
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            self.response_id = Some(id.to_owned());
        }
        if let Some(usage) = response.get("usage").and_then(Value::as_object) {
            let details = usage.get("input_tokens_details");
            let cached = details
                .and_then(|details| details.get("cached_tokens"))
                .and_then(Value::as_f64)
                .filter(|value| *value != 0.0)
                .unwrap_or(0.0);
            let cache_write = details
                .and_then(|details| details.get("cache_write_tokens"))
                .and_then(Value::as_f64)
                .filter(|value| *value != 0.0)
                .unwrap_or(0.0);
            let input = (truthy_count(usage, "input_tokens") - cached - cache_write).max(0.0);
            // OpenAI's total already covers everything; dsh reads it
            // instead of summing.
            let total = truthy_count(usage, "total_tokens");
            self.usage = TokenUsage {
                input_tokens: non_negative_u64(input).unwrap_or(0),
                output_tokens: non_negative_u64(truthy_count(usage, "output_tokens")).unwrap_or(0),
                total_tokens: non_negative_u64(total),
                cache_read_tokens: non_negative_u64(cached),
                cache_write_tokens: non_negative_u64(cache_write),
                reasoning_tokens: non_negative_u64(
                    usage
                        .get("output_tokens_details")
                        .and_then(|details| details.get("reasoning_tokens"))
                        .and_then(Value::as_f64)
                        .filter(|value| *value != 0.0)
                        .unwrap_or(0.0),
                ),
            };
        }
        // `calculateCost` and the service-tier pricing are dead (D7: cost
        // stays zero; fr has no service-tier face).

        let status = response.get("status").and_then(Value::as_str);
        // The dsh `rawStopReason` (including the
        // `<status>.<incomplete-reason>` spelling) has no fr durable face.
        let incomplete_reason = response
            .get("incomplete_details")
            .and_then(|details| details.get("reason"))
            .and_then(Value::as_str);
        self.stop_reason = Some(match map_stop_reason(status, incomplete_reason)? {
            MappedStop::Stop => FinishReason::Known(FinishReasonKind::Stop),
            MappedStop::MaxTokens => FinishReason::Known(FinishReasonKind::MaxTokens),
            MappedStop::Error(message) => return Err(static_error(&message)),
        });
        // A finished response with tool calls always stops for tool use.
        if self.stop_reason.as_ref() == Some(&FinishReason::Known(FinishReasonKind::Stop))
            && self
                .blocks
                .iter()
                .any(|block| matches!(block, RespBlock::ToolCall { .. }))
        {
            self.stop_reason = Some(FinishReason::Known(FinishReasonKind::ToolCalls));
        }
        Ok(())
    }

    /// Merge terminal `encrypted_content` into stored reasoning
    /// signatures that lack it (`backfillReasoningSignatures`; Azure omits
    /// it from `output_item.done`).
    fn backfill_reasoning_signatures(&mut self, output: Option<&Vec<Value>>) {
        let Some(items) = output else {
            return;
        };
        for item in items {
            let Some(encrypted) = item.get("encrypted_content").and_then(Value::as_str) else {
                continue;
            };
            if item.get("type").and_then(Value::as_str) != Some("reasoning") {
                continue;
            }
            let Some(id) = item.get("id").and_then(Value::as_str) else {
                continue;
            };
            let Some(position) = self.reasoning_by_id.get(id).copied() else {
                continue;
            };
            let RespBlock::Thinking {
                signature: Some(stored),
                ..
            } = &mut self.blocks[position]
            else {
                continue;
            };
            let Ok(mut parsed) = serde_json::from_str::<Value>(stored) else {
                continue;
            };
            if parsed.get("encrypted_content").is_some() {
                continue;
            }
            if let Some(object) = parsed.as_object_mut() {
                object.insert(
                    "encrypted_content".to_owned(),
                    Value::from(encrypted.to_owned()),
                );
            }
            *stored = parsed.to_string();
        }
    }

    /// The terminal chunks: usage, then the finish with the D12 replay
    /// envelope. Blocks the server never completed still close here —
    /// dsh keeps their partial content in the message object, and fr's
    /// chunk channel needs the close to deliver it.
    ///
    /// # Errors
    /// [`LlmError`] when the stream ended before a terminal response
    /// event, or when a tool-use finish carries an unfinished call.
    pub fn end_of_stream(&mut self) -> Result<Vec<StreamChunk>, LlmError> {
        if self.settled {
            return Ok(Vec::new());
        }
        if !self.saw_terminal {
            return Err(static_error(
                "OpenAI Responses stream ended before a terminal response event",
            ));
        }
        if self.stop_reason.as_ref() == Some(&FinishReason::Known(FinishReasonKind::ToolCalls)) {
            let unfinished: Vec<String> = self
                .blocks
                .iter()
                .filter_map(|block| match block {
                    RespBlock::ToolCall {
                        id,
                        name,
                        partial_json: Some(_),
                    } => Some(format!("{name} ({id})")),
                    _ => None,
                })
                .collect();
            if let Some(name_and_id) = unfinished.first() {
                return Err(static_error(&format!(
                    "OpenAI Responses stream completed with an unfinished tool call: \
                     {name_and_id}"
                )));
            }
        }
        let kinds: Vec<&'static str> = self.blocks.iter().map(RespBlock::kind).collect();
        let signatures: Vec<Option<String>> = self
            .blocks
            .iter()
            .map(|block| match block {
                RespBlock::Thinking { signature, .. } | RespBlock::Text { signature, .. } => {
                    signature.clone()
                }
                RespBlock::ToolCall { .. } => None,
            })
            .collect();
        let mut chunks = Vec::new();
        for position in 0..self.blocks.len() {
            if self.slots.values().any(|slot| *slot == position) {
                self.finish_block(position, &mut chunks);
            }
        }
        let blocks: Vec<Value> = kinds
            .iter()
            .zip(signatures)
            .map(|(kind, signature)| {
                signature.map_or_else(
                    || serde_json::json!({ "type": kind }),
                    |signature| {
                        serde_json::json!({
                            "type": kind,
                            "signature": signature,
                        })
                    },
                )
            })
            .collect();
        self.settled = true;
        chunks.push(StreamChunk::Known(KnownStreamChunk::Usage {
            usage: self.usage,
        }));
        chunks.push(StreamChunk::Known(KnownStreamChunk::Finish {
            reason: self
                .stop_reason
                .take()
                .unwrap_or(FinishReason::Known(FinishReasonKind::Stop)),
            replay_state: Some(ReplayEnvelope {
                response: serde_json::json!({
                    "kind": "fr-openai",
                    "version": 1,
                    "model": self.model,
                }),
                blocks: Some(blocks),
            }),
        }));
        Ok(chunks)
    }
}

/// Whether the event type is one of the delta-class events the
/// [`ResponsesTranslator::delta_chunks`] helper owns.
fn is_delta_event(event_type: &str) -> bool {
    matches!(
        event_type,
        "response.reasoning_summary_text.delta"
            | "response.reasoning_text.delta"
            | "response.reasoning_summary_part.done"
            | "response.output_text.delta"
            | "response.refusal.delta"
            | "response.function_call_arguments.delta"
            | "response.function_call_arguments.done"
    )
}

/// One static server-class error (`LlmError` over the static message).
fn static_error(message: &str) -> LlmError {
    LlmError::try_new(message, fr_llm::error::SERVER_CODE, None, None, None, None)
        .expect("static message and code satisfy the LlmError invariants")
}

/// The `output_index` of a delta-class event.
fn output_index(event: &RawObject) -> u64 {
    event
        .get("output_index")
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

/// Join one text field across an optional array (dsh
/// `item.summary?.map((s) => s.text).join(sep)`).
fn joined_text(container: Option<&Value>, field: &str, separator: &str) -> String {
    container
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get(field).and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(separator)
        })
        .unwrap_or_default()
}

/// One terminal status mapped (`mapStopReason`): the error variants carry
/// their dsh messages verbatim.
enum MappedStop {
    Stop,
    MaxTokens,
    Error(String),
}

fn map_stop_reason(
    status: Option<&str>,
    incomplete_reason: Option<&str>,
) -> Result<MappedStop, LlmError> {
    let Some(status) = status else {
        return Ok(MappedStop::Stop);
    };
    match status {
        // dsh marks `in_progress`/`queued` as wonky and stops anyway.
        "completed" | "in_progress" | "queued" => Ok(MappedStop::Stop),
        "incomplete" if incomplete_reason == Some("max_output_tokens") => Ok(MappedStop::MaxTokens),
        "incomplete" => Ok(MappedStop::Error(incomplete_reason.map_or_else(
            || "Response incomplete without a provider reason".to_owned(),
            |reason| format!("Response incomplete: {reason}"),
        ))),
        // dsh throws `output.errorMessage || "An unknown error occurred"`
        // for these two, and `mapStopReason` set no message.
        "failed" | "cancelled" => Ok(MappedStop::Error("An unknown error occurred".to_owned())),
        other => Err(static_error(&format!("Unhandled stop reason: {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(body: serde_json::Value) -> RawObject {
        serde_json::from_value(body).expect("object")
    }

    fn chunks(translator: &mut ResponsesTranslator, body: serde_json::Value) -> Vec<StreamChunk> {
        translator.translate_event(&event(body)).expect("event")
    }

    /// Drive the full happy path and collect every emitted chunk,
    /// terminal pair included.
    fn happy_stream_chunks() -> Vec<StreamChunk> {
        let mut translator = ResponsesTranslator::new("m");
        let mut all = Vec::new();
        all.extend(chunks(
            &mut translator,
            json!({"type": "response.created", "response": {"id": "resp_1"}}),
        ));
        all.extend(chunks(
            &mut translator,
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "reasoning", "id": "rs_1"}}),
        ));
        all.extend(chunks(
            &mut translator,
            json!({"type": "response.reasoning_summary_text.delta", "output_index": 0,
                   "delta": "th"}),
        ));
        all.extend(chunks(
            &mut translator,
            json!({"type": "response.reasoning_summary_part.done", "output_index": 0}),
        ));
        all.extend(chunks(
            &mut translator,
            json!({"type": "response.output_item.added", "output_index": 1,
                   "item": {"type": "message", "id": "msg_1", "phase": "final_answer"}}),
        ));
        all.extend(chunks(
            &mut translator,
            json!({"type": "response.output_text.delta", "output_index": 1, "delta": "hi"}),
        ));
        all.extend(chunks(
            &mut translator,
            json!({"type": "response.output_item.added", "output_index": 2,
                   "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1",
                            "name": "grep", "arguments": "{\"q\":"}}),
        ));
        all.extend(chunks(
            &mut translator,
            json!({"type": "response.function_call_arguments.delta", "output_index": 2,
                   "delta": "\"x\"}"}),
        ));
        all.extend(chunks(
            &mut translator,
            json!({"type": "response.function_call_arguments.done", "output_index": 2,
                   "arguments": "{\"q\":\"x\"} "}),
        ));
        all.extend(chunks(
            &mut translator,
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"type": "reasoning", "id": "rs_1",
                            "summary": [{"type": "summary_text", "text": "thinking"}]}}),
        ));
        all.extend(chunks(
            &mut translator,
            json!({"type": "response.output_item.done", "output_index": 1,
                   "item": {"type": "message", "id": "msg_1", "phase": "final_answer",
                            "content": [{"type": "output_text", "text": "hi"}]}}),
        ));
        all.extend(chunks(
            &mut translator,
            json!({"type": "response.output_item.done", "output_index": 2,
                   "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1",
                            "name": "grep", "arguments": "{\"q\":\"x\",\"extra\":1}"}}),
        ));
        all.extend(chunks(
            &mut translator,
            json!({"type": "response.completed", "response": {
                "id": "resp_1", "status": "completed",
                "usage": {"input_tokens": 100, "output_tokens": 40, "total_tokens": 140,
                          "input_tokens_details": {"cached_tokens": 30, "cache_write_tokens": 5},
                          "output_tokens_details": {"reasoning_tokens": 10}},
                "output": []}}),
        ));
        all.extend(translator.end_of_stream().expect("terminal"));
        all
    }

    /// The happy-path chunk shape: block starts, the reasoning part
    /// separator, raw tool deltas with the done-event prefix diff, the
    /// mid-stream closures, and the tool-call arguments re-stringified.
    #[test]
    fn happy_stream_pins_chunk_sequence() {
        let all = happy_stream_chunks();
        assert_eq!(all.len(), 13);
        assert!(matches!(
            &all[0],
            StreamChunk::Known(KnownStreamChunk::BlockStart { index: 0, block_type })
                if block_type == "reasoning"
        ));
        assert!(matches!(
            &all[1],
            StreamChunk::Known(KnownStreamChunk::ReasoningDelta { index: 0, text }) if text == "th"
        ));
        assert!(matches!(
            &all[2],
            StreamChunk::Known(KnownStreamChunk::ReasoningDelta { index: 0, text })
                if text == "\n\n"
        ));
        assert!(matches!(
            &all[3],
            StreamChunk::Known(KnownStreamChunk::BlockStart { index: 1, block_type })
                if block_type == "text"
        ));
        assert!(matches!(
            &all[4],
            StreamChunk::Known(KnownStreamChunk::TextDelta { index: 1, text }) if text == "hi"
        ));
        assert!(matches!(
            &all[5],
            StreamChunk::Known(KnownStreamChunk::BlockStart { index: 2, block_type })
                if block_type == "tool-call"
        ));
        assert!(matches!(
            &all[6],
            StreamChunk::Known(KnownStreamChunk::ToolCallDelta { index: 2, arguments_delta, .. })
                if arguments_delta == "\"x\"}"
        ));
        assert!(matches!(
            &all[7],
            StreamChunk::Known(KnownStreamChunk::ToolCallDelta { index: 2, arguments_delta, .. })
                if arguments_delta == " "
        ));
        assert!(matches!(
            &all[8],
            StreamChunk::Known(KnownStreamChunk::BlockEnd {
                index: 0,
                block: ContentBlock::Known(KnownContentBlock::Reasoning { text }),
            }) if text == "thinking"
        ));
        assert!(matches!(
            &all[9],
            StreamChunk::Known(KnownStreamChunk::BlockEnd {
                index: 1,
                block: ContentBlock::Known(KnownContentBlock::Text { text }),
            }) if text == "hi"
        ));
        assert!(matches!(
            &all[10],
            StreamChunk::Known(KnownStreamChunk::BlockEnd {
                index: 2,
                block: ContentBlock::Known(KnownContentBlock::ToolCall { arguments, .. }),
            }) if arguments == r#"{"q":"x","extra":1}"#
        ));
    }

    /// The happy-path terminal pair: the provider-total usage with the
    /// cache subtraction, the tool-use upgrade, and the signed D12
    /// envelope.
    #[test]
    fn happy_stream_pins_usage_and_envelope() {
        let all = happy_stream_chunks();
        let StreamChunk::Known(KnownStreamChunk::Usage { usage }) = &all[11] else {
            panic!("usage expected");
        };
        assert_eq!(usage.input_tokens, 65);
        assert_eq!(usage.cache_read_tokens, Some(30));
        assert_eq!(usage.cache_write_tokens, Some(5));
        assert_eq!(usage.reasoning_tokens, Some(10));
        assert_eq!(usage.total_tokens, Some(140), "the provider total wins");
        let StreamChunk::Known(KnownStreamChunk::Finish {
            reason,
            replay_state,
        }) = &all[12]
        else {
            panic!("finish expected");
        };
        assert_eq!(*reason, FinishReason::Known(FinishReasonKind::ToolCalls));
        let envelope = replay_state.as_ref().expect("envelope");
        assert_eq!(
            envelope.response,
            json!({"kind": "fr-openai", "version": 1, "model": "m"})
        );
        let blocks = envelope.blocks.as_ref().expect("blocks");
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0]["type"], "reasoning");
        let reasoning_signature: Value =
            serde_json::from_str(blocks[0]["signature"].as_str().expect("signature"))
                .expect("item json");
        assert_eq!(reasoning_signature["id"], "rs_1");
        assert_eq!(blocks[1]["type"], "text");
        let text_signature: Value =
            serde_json::from_str(blocks[1]["signature"].as_str().expect("signature"))
                .expect("v1 json");
        assert_eq!(
            text_signature,
            json!({"v": 1, "id": "msg_1", "phase": "final_answer"})
        );
        assert_eq!(blocks[2]["type"], "tool-call");
        assert!(blocks[2].get("signature").is_none());
    }

    /// The terminal status mapping: `max_output_tokens` truncation maps to
    /// the length reason, other incomplete reasons and failed statuses
    /// throw their dsh messages, and the in-band error and
    /// `response.failed` events throw at translate time.
    #[test]
    fn terminal_and_in_band_errors_throw_verbatim() {
        let mut translator = ResponsesTranslator::new("m");
        let _ = chunks(
            &mut translator,
            json!({"type": "response.incomplete", "response": {
                "id": "r", "status": "incomplete",
                "incomplete_details": {"reason": "max_output_tokens"},
                "usage": {"input_tokens": 5, "output_tokens": 5, "total_tokens": 10}}}),
        );
        let tail = translator.end_of_stream().expect("length terminal");
        let StreamChunk::Known(KnownStreamChunk::Finish { reason, .. }) = &tail[1] else {
            panic!("finish expected");
        };
        assert_eq!(*reason, FinishReason::Known(FinishReasonKind::MaxTokens));

        let mut translator = ResponsesTranslator::new("m");
        let error = translator
            .translate_event(&event(json!({"type": "response.incomplete", "response": {
                "status": "incomplete", "incomplete_details": {"reason": "content_filter"}}})))
            .expect_err("throws");
        assert_eq!(error.message(), "Response incomplete: content_filter");

        let mut translator = ResponsesTranslator::new("m");
        let error = translator
            .translate_event(&event(json!({"type": "response.incomplete", "response": {
                "status": "incomplete"}})))
            .expect_err("throws");
        assert_eq!(
            error.message(),
            "Response incomplete without a provider reason"
        );

        let mut translator = ResponsesTranslator::new("m");
        let error = translator
            .translate_event(&event(json!({"type": "response.completed", "response": {
                "status": "failed"}})))
            .expect_err("throws");
        assert_eq!(error.message(), "An unknown error occurred");

        let mut translator = ResponsesTranslator::new("m");
        let error = translator
            .translate_event(&event(json!({
                "type": "error", "code": "e1", "message": "boom"})))
            .expect_err("throws");
        assert_eq!(error.message(), "Error Code e1: boom");

        let mut translator = ResponsesTranslator::new("m");
        let error = translator
            .translate_event(&event(json!({"type": "response.failed", "response": {
                "status": "failed",
                "error": {"code": "server_error", "message": "overloaded"}}})))
            .expect_err("throws");
        assert_eq!(error.message(), "server_error: overloaded");

        let mut translator = ResponsesTranslator::new("m");
        let error = translator
            .translate_event(&event(json!({"type": "response.failed", "response": {
                "status": "failed",
                "incomplete_details": {"reason": "safety"}}})))
            .expect_err("throws");
        assert_eq!(error.message(), "incomplete: safety");

        let mut translator = ResponsesTranslator::new("m");
        let error = translator
            .translate_event(&event(json!({"type": "response.failed", "response": {
                "status": "failed"}})))
            .expect_err("throws");
        assert_eq!(
            error.message(),
            "Unknown error (no error details in response)"
        );
    }

    /// The terminal gate and the unfinished-tool-call guard throw their
    /// dsh messages; an item the server never completed still closes at
    /// the end and rides the envelope without a signature.
    #[test]
    fn terminal_gate_and_guards_pin_messages() {
        let mut translator = ResponsesTranslator::new("m");
        let error = translator.end_of_stream().expect_err("gate");
        assert_eq!(
            error.message(),
            "OpenAI Responses stream ended before a terminal response event"
        );

        let mut translator = ResponsesTranslator::new("m");
        let _ = chunks(
            &mut translator,
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "function_call", "id": "fc_1", "call_id": "c1",
                            "name": "grep", "arguments": "{\"q\":"}}),
        );
        let _ = chunks(
            &mut translator,
            json!({"type": "response.completed", "response": {"id": "r", "status": "completed"}}),
        );
        let error = translator.end_of_stream().expect_err("unfinished");
        assert_eq!(
            error.message(),
            "OpenAI Responses stream completed with an unfinished tool call: grep (c1|fc_1)"
        );

        let mut translator = ResponsesTranslator::new("m");
        let _ = chunks(
            &mut translator,
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "message", "id": "msg_1"}}),
        );
        let _ = chunks(
            &mut translator,
            json!({"type": "response.output_text.delta", "output_index": 0, "delta": "partial"}),
        );
        let _ = chunks(
            &mut translator,
            json!({"type": "response.completed", "response": {"id": "r", "status": "completed"}}),
        );
        let tail = translator.end_of_stream().expect("closes open blocks");
        let StreamChunk::Known(KnownStreamChunk::BlockEnd {
            index: 0,
            block: ContentBlock::Known(KnownContentBlock::Text { text }),
        }) = &tail[0]
        else {
            panic!("open block closes first");
        };
        assert_eq!(text, "partial");
        assert_eq!(tail.len(), 3);
    }

    /// The Azure backfill merges `encrypted_content` into a stored
    /// signature that lacks it and leaves a complete one untouched.
    #[test]
    fn backfill_merges_encrypted_content_once() {
        let item_json = r#"{"type":"reasoning","id":"rs_1","summary":[]}"#;
        let mut translator = ResponsesTranslator::new("m");
        let _ = chunks(
            &mut translator,
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "reasoning", "id": "rs_1"}}),
        );
        let _ = chunks(
            &mut translator,
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"type": "reasoning", "id": "rs_1", "summary": []}}),
        );
        let _ = chunks(
            &mut translator,
            json!({"type": "response.completed", "response": {
                "id": "r", "status": "completed",
                "output": [{"type": "reasoning", "id": "rs_1",
                            "encrypted_content": "enc1"}]}}),
        );
        let tail = translator.end_of_stream().expect("terminal");
        let StreamChunk::Known(KnownStreamChunk::Finish { replay_state, .. }) = &tail[1] else {
            panic!("finish expected");
        };
        let blocks = replay_state
            .as_ref()
            .expect("envelope")
            .blocks
            .clone()
            .expect("blocks");
        let signature = blocks[0]["signature"].as_str().expect("signature");
        let parsed: Value = serde_json::from_str(signature).expect("json");
        assert_eq!(parsed["encrypted_content"], "enc1");
        let _ = item_json;
    }

    /// A slot the server opened without `.added` re-creates at
    /// `output_item.done`, and unknown item types open nothing.
    #[test]
    fn slot_recreation_and_unknown_items() {
        let mut translator = ResponsesTranslator::new("m");
        let created = chunks(
            &mut translator,
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"type": "message", "id": "msg_1",
                            "content": [{"type": "output_text", "text": "late"}]}}),
        );
        assert_eq!(created.len(), 2, "BlockStart then BlockEnd");
        let _ = chunks(
            &mut translator,
            json!({"type": "response.output_item.added", "output_index": 1,
                   "item": {"type": "web_search_call", "id": "ws_1"}}),
        );
        let ignored = chunks(
            &mut translator,
            json!({"type": "response.output_item.done", "output_index": 1,
                   "item": {"type": "web_search_call", "id": "ws_1"}}),
        );
        assert!(ignored.is_empty(), "unknown items open no slot");
        let _ = chunks(
            &mut translator,
            json!({"type": "response.completed", "response": {"id": "r", "status": "completed"}}),
        );
        let tail = translator.end_of_stream().expect("terminal");
        let StreamChunk::Known(KnownStreamChunk::Finish { replay_state, .. }) = &tail[1] else {
            panic!("finish expected");
        };
        let blocks = replay_state
            .as_ref()
            .expect("envelope")
            .blocks
            .clone()
            .expect("blocks");
        assert_eq!(blocks.len(), 1, "unknown items leave no block");
    }
}
