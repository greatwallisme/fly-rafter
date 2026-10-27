//! Stream translation for anthropic-messages events (pi-ai
//! `iterateAnthropicEvents` + the `stream()` event loop,
//! anthropic-messages.ts:530-901, merged into one pass over SSE frames).
//!
//! Six wire events are consumed (`message_start`, `content_block_start`,
//! `content_block_delta`, `content_block_stop`, `message_delta`,
//! `message_stop`); an SSE `error` frame and any unparseable event fail
//! the stream with the pi-ai texts. Tool argument deltas stay the raw
//! provider string (the dsh patch removed the per-delta
//! `parseStreamingJson`); the single close-time parse stringifies back
//! to the durable raw form (Q75-C). Thinking signatures and the redacted
//! marker ride the D12 envelope (kind `fr-anthropic`, one entry per
//! emitted block in stream order). The `input_transformations`
//! diagnostic attaches to the pi-ai `AssistantMessage` only — the
//! harness `StreamChunk` face has no channel, so it drops here exactly
//! as it drops in the dsh shell translator.

use std::collections::HashMap;

/// Local block counts to the chunk-index domain (`u64`); a stream cannot
/// open enough blocks to overflow.
fn chunk_index(count: usize) -> u64 {
    u64::try_from(count).expect("a block count fits the chunk index")
}

use fr_wire::call_config::{ReplayEnvelope, TokenUsage, ToolSchema};
use fr_wire::{ContentBlock, KnownContentBlock, KnownStreamChunk, StreamChunk, ToolCallId};
use serde_json::Value;

use crate::json_parse::{parse_streaming_json, repair_json};
use crate::settle::{Settlement, settle_failure};
use crate::sse::SseFrame;

/// The wire events this protocol consumes (pi-ai
/// `ANTHROPIC_MESSAGE_EVENTS`, anthropic-messages.ts:391-398); every
/// other event name skips.
const MESSAGE_EVENTS: [&str; 6] = [
    "message_start",
    "message_delta",
    "message_stop",
    "content_block_start",
    "content_block_delta",
    "content_block_stop",
];

/// One open wire block, keyed by the wire index it opened with.
#[derive(Debug)]
enum OpenBlock {
    /// Visible text.
    Text { text: String },
    /// Thinking with its accumulating signature.
    Thinking {
        text: String,
        signature: String,
        redacted: bool,
    },
    /// A tool call accumulated from raw argument deltas.
    ToolCall {
        id: String,
        name: String,
        partial_json: String,
    },
}

impl OpenBlock {
    /// The `StreamChunk` block-type literal and the envelope kind.
    const fn kind(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::Thinking { .. } => "reasoning",
            Self::ToolCall { .. } => "tool-call",
        }
    }
}

/// The merged usage state (pi-ai `message_start` baseline, `message_delta`
/// null-guarded overwrite — proxies omit fields, so only present values
/// replace).
#[derive(Debug, Default)]
struct UsageState {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    /// The one-hour cache-write split; the harness `TokenUsage` has no
    /// slot, so it folds into the total only (the pi-ai total formula
    /// sums the four base fields, not the 1h split — kept verbatim).
    cache_write_1h: u64,
    reasoning: Option<u64>,
}

impl UsageState {
    /// pi-ai computes the total from the components (the wire carries
    /// none).
    const fn total(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }

    const fn token_usage(&self) -> TokenUsage {
        TokenUsage {
            input_tokens: self.input,
            output_tokens: self.output,
            total_tokens: Some(self.total()),
            cache_read_tokens: Some(self.cache_read),
            cache_write_tokens: Some(self.cache_write),
            reasoning_tokens: self.reasoning,
        }
    }
}

/// Maps one wire stop reason (pi-ai `mapStopReason`,
/// anthropic-messages.ts:1613-1639): the refusal explanation, the
/// `sensitive` safety stop, and unknown values become terminal errors.
fn map_stop_reason(event: &Value) -> Result<(&'static str, Option<String>), String> {
    let reason = event["delta"]["stop_reason"].as_str().unwrap_or_default();
    match reason {
        "end_turn" | "pause_turn" | "stop_sequence" => Ok(("stop", None)),
        "max_tokens" => Ok(("length", None)),
        "tool_use" => Ok(("toolUse", None)),
        "refusal" => {
            let explanation = event["delta"]["stop_details"]["explanation"]
                .as_str()
                .filter(|text| !text.is_empty())
                .map(str::to_owned);
            Ok((
                "error",
                Some(
                    explanation
                        .unwrap_or_else(|| "The model refused to complete the request".to_owned()),
                ),
            ))
        }
        "sensitive" => Ok(("error", Some("Provider stopped with: sensitive".to_owned()))),
        other => Err(format!("Unhandled stop reason: {other}")),
    }
}

/// The incremental anthropic stream translator.
#[derive(Debug)]
pub struct AnthropicTranslator {
    /// Route model id (settlement texts, envelope).
    model: String,
    /// Provider route key.
    provider: String,
    /// Declared context window for silent-overflow detection.
    context_window: Option<u64>,
    /// Open blocks in wire order; the local index is the emission index.
    blocks: Vec<Option<OpenBlock>>,
    /// Envelope entries parallel to `blocks`, filled at close (and at
    /// settle for blocks the stream left open, in block order).
    envelope: Vec<Option<Value>>,
    /// How many real blocks opened (settlement's block count; a skipped
    /// fallback block does not count).
    emitted_blocks: usize,
    /// Wire block index → local index.
    wire_index: HashMap<u64, usize>,
    /// The declared tools (receive-side stealth name map).
    tools: Vec<ToolSchema>,
    /// Whether the credential is an OAuth token (D15-a).
    is_oauth: bool,
    usage: UsageState,
    /// The mapped pi-ai stop reason, once `message_delta` carried one.
    stop: Option<&'static str>,
    /// The terminal error message a mapped stop produced.
    error_message: Option<String>,
    saw_start: bool,
    saw_stop: bool,
    /// Response identity echoed for durable metadata.
    response_id: Option<String>,
    response_model: Option<String>,
}

impl AnthropicTranslator {
    /// Creates a translator for one stream.
    #[must_use]
    pub fn new(
        model: &str,
        provider: &str,
        context_window: Option<u64>,
        tools: Vec<ToolSchema>,
        is_oauth: bool,
    ) -> Self {
        Self {
            model: model.to_owned(),
            provider: provider.to_owned(),
            context_window,
            blocks: Vec::new(),
            envelope: Vec::new(),
            emitted_blocks: 0,
            wire_index: HashMap::new(),
            tools,
            is_oauth,
            usage: UsageState::default(),
            stop: None,
            error_message: None,
            saw_start: false,
            saw_stop: false,
            response_id: None,
            response_model: None,
        }
    }

    /// The response ids echoed back for durable metadata.
    #[must_use]
    pub const fn response_ids(&self) -> (&Option<String>, &Option<String>) {
        (&self.response_id, &self.response_model)
    }

    /// Feeds one SSE frame (pi-ai `iterateAnthropicEvents` fused with
    /// the event loop): an `error` frame or an unparseable event fails
    /// the stream; unknown event names skip.
    ///
    /// # Errors
    ///
    /// The pi-ai throw texts: SSE `error` frames carry their data, parse
    /// failures name the event, an unsupported mid-output fallback is
    /// refused, and an unknown stop reason is unhandled.
    pub fn feed_frame(&mut self, frame: &SseFrame) -> Result<Vec<StreamChunk>, String> {
        if frame.event.as_deref() == Some("error") {
            return Err(frame.data.clone());
        }
        if !frame
            .event
            .as_deref()
            .is_some_and(|name| MESSAGE_EVENTS.contains(&name))
        {
            return Ok(Vec::new());
        }
        let event: Value = serde_json::from_str(&repair_json(&frame.data)).map_err(|error| {
            let raw = frame.raw.join("\\n");
            format!(
                "Could not parse Anthropic SSE event {}: {}; data={}; raw={}",
                frame.event.as_deref().unwrap_or_default(),
                error,
                frame.data,
                raw
            )
        })?;
        self.handle_event(&event)
    }

    /// Dispatches one parsed wire event (the pi-ai loop arms).
    fn handle_event(&mut self, event: &Value) -> Result<Vec<StreamChunk>, String> {
        match event["type"].as_str().unwrap_or_default() {
            "message_start" => {
                self.saw_start = true;
                let message = &event["message"];
                self.response_id = message["id"].as_str().map(str::to_owned);
                self.response_model = message["model"].as_str().map(str::to_owned);
                self.usage.input = message["usage"]["input_tokens"].as_u64().unwrap_or(0);
                self.usage.output = message["usage"]["output_tokens"].as_u64().unwrap_or(0);
                self.usage.cache_read = message["usage"]["cache_read_input_tokens"]
                    .as_u64()
                    .unwrap_or(0);
                self.usage.cache_write = message["usage"]["cache_creation_input_tokens"]
                    .as_u64()
                    .unwrap_or(0);
                self.usage.cache_write_1h =
                    message["usage"]["cache_creation"]["ephemeral_1h_input_tokens"]
                        .as_u64()
                        .unwrap_or(0);
                // The fallback content-block face (server-side-fallback
                // beta) and the input-transformations diagnostic are
                // withheld faces; see the module docs.
            }
            "content_block_start" => {
                if let Some(chunk) = self.open_block(event)? {
                    return Ok(vec![chunk]);
                }
            }
            "content_block_delta" => {
                if let Some(chunk) = self.apply_delta(event) {
                    return Ok(vec![chunk]);
                }
            }
            "content_block_stop" => {
                if let Some(chunk) = self.close_block(event) {
                    return Ok(vec![chunk]);
                }
            }
            "message_delta" => {
                if event["delta"]["stop_reason"].as_str().is_some() {
                    let (stop, error_message) = map_stop_reason(event)?;
                    self.stop = Some(stop);
                    self.error_message = error_message;
                }
                let usage = &event["usage"];
                if let Some(value) = usage["input_tokens"].as_u64() {
                    self.usage.input = value;
                }
                if let Some(value) = usage["output_tokens"].as_u64() {
                    self.usage.output = value;
                }
                if let Some(value) = usage["cache_read_input_tokens"].as_u64() {
                    self.usage.cache_read = value;
                }
                if let Some(value) = usage["cache_creation_input_tokens"].as_u64() {
                    self.usage.cache_write = value;
                }
                if let Some(value) = usage["cache_creation"]["ephemeral_1h_input_tokens"].as_u64() {
                    self.usage.cache_write_1h = value;
                }
                if let Some(value) = usage["output_tokens_details"]["thinking_tokens"].as_u64() {
                    self.usage.reasoning = Some(value);
                }
            }
            "message_stop" => {
                self.saw_stop = true;
            }
            _ => {}
        }
        Ok(Vec::new())
    }

    /// `content_block_start` (anthropic-messages.ts:689-736): text,
    /// thinking (redacted markers restore the opaque payload), `tool_use`
    /// (receive-side stealth name map); a `fallback` block refuses
    /// mid-output and skips otherwise.
    fn open_block(&mut self, event: &Value) -> Result<Option<StreamChunk>, String> {
        let wire = event["index"].as_u64().unwrap_or_default();
        let block_type = event["content_block"]["type"].as_str().unwrap_or_default();
        let block = match block_type {
            "text" => OpenBlock::Text {
                text: event["content_block"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            },
            "thinking" => OpenBlock::Thinking {
                text: event["content_block"]["thinking"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                signature: event["content_block"]["signature"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                redacted: false,
            },
            "redacted_thinking" => OpenBlock::Thinking {
                text: "[Reasoning redacted]".to_owned(),
                signature: event["content_block"]["data"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                redacted: true,
            },
            "tool_use" => {
                let wire_name = event["content_block"]["name"].as_str().unwrap_or_default();
                let name = if self.is_oauth {
                    crate::convert::from_claude_code_name(wire_name, &self.tools).into_owned()
                } else {
                    wire_name.to_owned()
                };
                OpenBlock::ToolCall {
                    id: event["content_block"]["id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    name,
                    partial_json: String::new(),
                }
            }
            "fallback" => {
                if self.blocks.iter().any(Option::is_some) {
                    return Err(
                        "Anthropic performed an unsupported mid-output model fallback".to_owned(),
                    );
                }
                // An empty-content fallback switch skips the block; the
                // response model rides the identity echo.
                self.wire_index.insert(wire, self.blocks.len());
                self.blocks.push(None);
                self.envelope.push(None);
                return Ok(None);
            }
            // pi-ai's start arm only names the five known types; an
            // unknown block opens nothing and its deltas find no block.
            _ => return Ok(None),
        };
        let chunk = StreamChunk::Known(KnownStreamChunk::BlockStart {
            index: chunk_index(self.blocks.len()),
            block_type: block.kind().to_owned(),
        });
        self.wire_index.insert(wire, self.blocks.len());
        self.blocks.push(Some(block));
        self.envelope.push(None);
        self.emitted_blocks += 1;
        Ok(Some(chunk))
    }

    /// `content_block_delta` (anthropic-messages.ts:737-782): text and
    /// thinking deltas emit; `input_json_delta` accumulates the raw
    /// string and emits it verbatim (the dsh patch removed the per-delta
    /// parse); signature deltas accumulate silently; a delta for a
    /// missing or closed block skips (pi-ai `if (block && …)`).
    fn apply_delta(&mut self, event: &Value) -> Option<StreamChunk> {
        let wire = event["index"].as_u64()?;
        let delta = &event["delta"];
        let local = *self.wire_index.get(&wire)?;
        let index = chunk_index(local);
        // Capture the tool identity before the mutable block borrow.
        let tool_identity = match self.blocks.get(local)? {
            Some(OpenBlock::ToolCall { id, name, .. }) => Some((id.clone(), name.clone())),
            _ => None,
        };
        let block = self.blocks.get_mut(local)?.as_mut()?;
        match (block, delta["type"].as_str().unwrap_or_default()) {
            (OpenBlock::Text { text }, "text_delta") => {
                let appended = delta["text"].as_str().unwrap_or_default();
                text.push_str(appended);
                Some(StreamChunk::Known(KnownStreamChunk::TextDelta {
                    index,
                    text: appended.to_owned(),
                }))
            }
            (OpenBlock::Thinking { text, .. }, "thinking_delta") => {
                let appended = delta["thinking"].as_str().unwrap_or_default();
                text.push_str(appended);
                Some(StreamChunk::Known(KnownStreamChunk::ReasoningDelta {
                    index,
                    text: appended.to_owned(),
                }))
            }
            (OpenBlock::ToolCall { partial_json, .. }, "input_json_delta") => {
                let (id, name) = tool_identity?;
                let appended = delta["partial_json"].as_str().unwrap_or_default();
                partial_json.push_str(appended);
                Some(StreamChunk::Known(KnownStreamChunk::ToolCallDelta {
                    index,
                    id: ToolCallId::new(&id),
                    name: Some(name),
                    arguments_delta: appended.to_owned(),
                }))
            }
            (OpenBlock::Thinking { signature, .. }, "signature_delta") => {
                signature.push_str(delta["signature"].as_str().unwrap_or_default());
                None
            }
            _ => None,
        }
    }

    /// `content_block_stop` (anthropic-messages.ts:783-814): the closed
    /// block emits its `block-end` with the durable content — tool
    /// arguments are the single close-time lenient parse stringified
    /// back to the raw form.
    fn close_block(&mut self, event: &Value) -> Option<StreamChunk> {
        let wire = event["index"].as_u64()?;
        let local = *self.wire_index.get(&wire)?;
        let block = self.blocks.get_mut(local)?.take()?;
        if let Some(slot) = self.envelope.get_mut(local) {
            *slot = Some(Self::envelope_entry(&block));
        }
        let content = match block {
            OpenBlock::Text { text } => ContentBlock::Known(KnownContentBlock::Text { text }),
            OpenBlock::Thinking { text, .. } => {
                ContentBlock::Known(KnownContentBlock::Reasoning { text })
            }
            OpenBlock::ToolCall {
                id,
                name,
                partial_json,
            } => ContentBlock::Known(KnownContentBlock::ToolCall {
                id: ToolCallId::new(&id),
                name,
                arguments: parse_streaming_json(Some(&partial_json)).to_string(),
            }),
        };
        Some(StreamChunk::Known(KnownStreamChunk::BlockEnd {
            index: chunk_index(local),
            block: content,
        }))
    }

    /// The end-of-stream guard (pi-ai post-loop checks,
    /// anthropic-messages.ts:566-569 and 861-870): a stream that started
    /// but never saw `message_stop`, or one that settled no stop reason,
    /// fails with the pi-ai texts.
    ///
    /// # Errors
    ///
    /// `Anthropic stream ended before message_stop` /
    /// `Anthropic stream ended without a stop reason` / a mapped error
    /// stop's message (`Request was aborted` belongs to the adapter's
    /// signal face).
    pub fn end_of_stream(&mut self) -> Result<(), String> {
        if self.saw_start && !self.saw_stop {
            return Err("Anthropic stream ended before message_stop".to_owned());
        }
        if self.stop.is_none() {
            return Err("Anthropic stream ended without a stop reason".to_owned());
        }
        if self.stop == Some("error") {
            return Err(self
                .error_message
                .clone()
                .unwrap_or_else(|| "An unknown error occurred".to_owned()));
        }
        Ok(())
    }

    /// The terminal chunk pair: usage before the settled finish (the
    /// seam's ordering contract) with the D12 envelope (kind
    /// `fr-anthropic`).
    ///
    /// A failed or aborted stream settles through the same face: the
    /// error message rides the settlement, `stop` names the pi-ai stop
    /// (`error` for every terminal failure, `aborted` for a caller
    /// abort).
    #[must_use]
    pub fn settle_chunks(
        &self,
        stop_override: Option<&'static str>,
        error: Option<&str>,
    ) -> Vec<StreamChunk> {
        let blocks_len = self.emitted_blocks;
        let stop = stop_override.or(self.stop).unwrap_or("pending");
        let settlement = Settlement {
            stop,
            error_message: error
                .map(str::to_owned)
                .or_else(|| self.error_message.clone()),
            model: self.model.clone(),
            provider: self.provider.clone(),
            blocks_len,
            input_tokens: self.usage.input.saturating_add(self.usage.cache_read),
            output_tokens: self.usage.output,
            context_window: self.context_window,
        };
        let reason = settle_failure(&settlement);
        vec![
            StreamChunk::Known(KnownStreamChunk::Usage {
                usage: self.usage.token_usage(),
            }),
            StreamChunk::Known(KnownStreamChunk::Finish {
                reason,
                replay_state: Some(self.replay_envelope()),
            }),
        ]
    }

    /// One block's envelope entry: thinking blocks carry their signature
    /// (the redacted marker for redacted thinking), text and tool-call
    /// blocks carry their type only.
    fn envelope_entry(block: &OpenBlock) -> Value {
        match block {
            OpenBlock::Thinking {
                signature,
                redacted,
                ..
            } => {
                let mut entry = serde_json::Map::new();
                entry.insert("type".to_owned(), Value::String("thinking".to_owned()));
                if !signature.is_empty() {
                    entry.insert("signature".to_owned(), Value::String(signature.clone()));
                }
                if *redacted {
                    entry.insert("redacted".to_owned(), Value::Bool(true));
                }
                Value::Object(entry)
            }
            other => serde_json::json!({ "type": other.kind() }),
        }
    }

    /// The D12 replay envelope: kind `fr-anthropic`, the route model,
    /// and one entry per emitted block — thinking blocks carry their
    /// signature (the redacted marker for redacted thinking), text and
    /// tool-call blocks carry their type only.
    fn replay_envelope(&self) -> ReplayEnvelope {
        let blocks: Vec<Value> = self
            .envelope
            .iter()
            .enumerate()
            .filter_map(|(local, slot)| match (slot, self.blocks.get(local)) {
                (Some(entry), _) => Some(entry.clone()),
                // A block the stream left open still contributes its
                // partial state (pi-ai keeps it in the message content).
                (None, Some(Some(block))) => Some(Self::envelope_entry(block)),
                _ => None,
            })
            .collect();
        ReplayEnvelope {
            response: serde_json::json!({
                "kind": "fr-anthropic",
                "version": 1,
                "model": self.model,
            }),
            blocks: Some(blocks),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn frame(event: &str, data: &Value) -> SseFrame {
        SseFrame {
            event: Some(event.to_owned()),
            data: data.to_string(),
            raw: Vec::new(),
        }
    }

    fn translator() -> AnthropicTranslator {
        AnthropicTranslator::new("claude-test", "anthropic", None, Vec::new(), false)
    }

    fn drain(
        translator: &mut AnthropicTranslator,
        frames: Vec<SseFrame>,
    ) -> Result<Vec<StreamChunk>, String> {
        let mut chunks = Vec::new();
        for frame in frames {
            chunks.extend(translator.feed_frame(&frame)?);
        }
        translator.end_of_stream()?;
        chunks.extend(translator.settle_chunks(None, None));
        Ok(chunks)
    }

    fn start(usage: &Value) -> SseFrame {
        frame(
            "message_start",
            &json!({ "type": "message_start", "message": { "id": "msg_1", "model": "claude-test", "usage": usage } }),
        )
    }

    fn base_usage() -> Value {
        serde_json::json!({
            "input_tokens": 10, "output_tokens": 2,
            "cache_read_input_tokens": 3, "cache_creation_input_tokens": 4,
        })
    }

    /// A full stream: text and tool-call blocks in order, raw argument
    /// deltas, the close-time parse, usage before the settled finish,
    /// and the envelope kinds.
    #[test]
    fn translates_a_full_stream() {
        let mut translator = translator();
        let chunks = drain(
            &mut translator,
            vec![
                start(&base_usage()),
                frame("content_block_start", &json!({
                    "type": "content_block_start", "index": 0,
                    "content_block": { "type": "text", "text": "" }
                })),
                frame("content_block_delta", &json!({
                    "type": "content_block_delta", "index": 0,
                    "delta": { "type": "text_delta", "text": "hi" }
                })),
                frame("content_block_stop", &json!({ "type": "content_block_stop", "index": 0 })),
                frame("content_block_start", &json!({
                    "type": "content_block_start", "index": 1,
                    "content_block": { "type": "tool_use", "id": "toolu_1", "name": "sh", "input": {} }
                })),
                frame("content_block_delta", &json!({
                    "type": "content_block_delta", "index": 1,
                    "delta": { "type": "input_json_delta", "partial_json": "{\"cmd\":" }
                })),
                frame("content_block_delta", &json!({
                    "type": "content_block_delta", "index": 1,
                    "delta": { "type": "input_json_delta", "partial_json": "\"ls\"}" }
                })),
                frame("content_block_stop", &json!({ "type": "content_block_stop", "index": 1 })),
                frame("message_delta", &json!({
                    "type": "message_delta",
                    "delta": { "stop_reason": "end_turn", "stop_details": null },
                    "usage": { "output_tokens": 5 }
                })),
                frame("message_stop", &json!({ "type": "message_stop" })),
            ],
        )
        .expect("translates");
        let kinds: Vec<&str> = chunks
            .iter()
            .map(|chunk| match chunk {
                StreamChunk::Known(KnownStreamChunk::BlockStart { block_type, .. }) => {
                    block_type.as_str()
                }
                _ => "",
            })
            .filter(|kind| !kind.is_empty())
            .collect();
        assert_eq!(kinds, ["text", "tool-call"]);
        // The raw delta strings survive verbatim.
        let deltas: Vec<String> = chunks
            .iter()
            .filter_map(|chunk| match chunk {
                StreamChunk::Known(KnownStreamChunk::ToolCallDelta {
                    arguments_delta, ..
                }) => Some(arguments_delta.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, ["{\"cmd\":".to_owned(), "\"ls\"}".to_owned()]);
        // The close-time parse stringifies back compactly.
        let end = chunks.iter().find_map(|chunk| match chunk {
            StreamChunk::Known(KnownStreamChunk::BlockEnd {
                block: ContentBlock::Known(KnownContentBlock::ToolCall { arguments, .. }),
                ..
            }) => Some(arguments.clone()),
            _ => None,
        });
        assert_eq!(end, Some(r#"{"cmd":"ls"}"#.to_owned()));
        // Usage precedes the finish; the delta output overwrite lands.
        let usage_index = chunks
            .iter()
            .position(|chunk| matches!(chunk, StreamChunk::Known(KnownStreamChunk::Usage { .. })))
            .expect("usage");
        let finish_index = chunks
            .iter()
            .position(|chunk| matches!(chunk, StreamChunk::Known(KnownStreamChunk::Finish { .. })))
            .expect("finish");
        assert!(usage_index < finish_index);
        let usage = chunks.iter().find_map(|chunk| match chunk {
            StreamChunk::Known(KnownStreamChunk::Usage { usage }) => Some(*usage),
            _ => None,
        });
        let usage = usage.expect("usage chunk");
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.total_tokens, Some(10 + 5 + 3 + 4));
        let finish = chunks.iter().find_map(|chunk| match chunk {
            StreamChunk::Known(KnownStreamChunk::Finish { replay_state, .. }) => {
                replay_state.clone()
            }
            _ => None,
        });
        let envelope = finish.expect("envelope");
        assert_eq!(envelope.response["kind"], "fr-anthropic");
        assert_eq!(envelope.response["model"], "claude-test");
        assert_eq!(
            envelope.blocks,
            Some(vec![
                json!({ "type": "text" }),
                json!({ "type": "tool-call" }),
            ])
        );
    }

    /// Thinking blocks: signature deltas accumulate into the envelope,
    /// redacted markers restore the opaque payload, and block-end
    /// contents carry the visible text.
    #[test]
    fn thinking_signature_and_redacted_ride_the_envelope() {
        let mut translator = translator();
        let chunks = drain(
            &mut translator,
            vec![
                start(&base_usage()),
                frame(
                    "content_block_start",
                    &json!({
                        "type": "content_block_start", "index": 0,
                        "content_block": { "type": "thinking", "thinking": "", "signature": "" }
                    }),
                ),
                frame(
                    "content_block_delta",
                    &json!({
                        "type": "content_block_delta", "index": 0,
                        "delta": { "type": "thinking_delta", "thinking": "ponder" }
                    }),
                ),
                frame(
                    "content_block_delta",
                    &json!({
                        "type": "content_block_delta", "index": 0,
                        "delta": { "type": "signature_delta", "signature": "sig1" }
                    }),
                ),
                frame(
                    "content_block_stop",
                    &json!({ "type": "content_block_stop", "index": 0 }),
                ),
                frame(
                    "content_block_start",
                    &json!({
                        "type": "content_block_start", "index": 1,
                        "content_block": { "type": "redacted_thinking", "data": "opaque" }
                    }),
                ),
                frame(
                    "content_block_stop",
                    &json!({ "type": "content_block_stop", "index": 1 }),
                ),
                frame(
                    "message_delta",
                    &json!({
                        "type": "message_delta", "delta": { "stop_reason": "end_turn" },
                        "usage": null
                    }),
                ),
                frame("message_stop", &json!({ "type": "message_stop" })),
            ],
        )
        .expect("translates");
        let finish = chunks.iter().find_map(|chunk| match chunk {
            StreamChunk::Known(KnownStreamChunk::Finish { replay_state, .. }) => {
                replay_state.clone()
            }
            _ => None,
        });
        let envelope = finish.expect("envelope");
        assert_eq!(
            envelope.blocks,
            Some(vec![
                json!({ "type": "thinking", "signature": "sig1" }),
                json!({ "type": "thinking", "signature": "opaque", "redacted": true }),
            ])
        );
        let reasoning = chunks.iter().find_map(|chunk| match chunk {
            StreamChunk::Known(KnownStreamChunk::BlockEnd {
                block: ContentBlock::Known(KnownContentBlock::Reasoning { text }),
                ..
            }) => Some(text.clone()),
            _ => None,
        });
        assert_eq!(reasoning, Some("ponder".to_owned()));
    }

    /// Stop-reason arms: the clean mappings settle through, refusal and
    /// sensitive carry their messages, unknown values are unhandled.
    #[test]
    fn stop_reason_arms() {
        for (reason, expected) in [
            ("end_turn", "stop"),
            ("max_tokens", "length"),
            ("tool_use", "toolUse"),
            ("pause_turn", "stop"),
            ("stop_sequence", "stop"),
        ] {
            let mut stream = translator();
            let chunks = drain(
                &mut stream,
                vec![
                    start(&base_usage()),
                    frame(
                        "content_block_start",
                        &json!({
                            "type": "content_block_start", "index": 0,
                            "content_block": { "type": "text", "text": "x" }
                        }),
                    ),
                    frame(
                        "content_block_stop",
                        &json!({ "type": "content_block_stop", "index": 0 }),
                    ),
                    frame(
                        "message_delta",
                        &json!({
                            "type": "message_delta", "delta": { "stop_reason": reason }
                        }),
                    ),
                    frame("message_stop", &json!({ "type": "message_stop" })),
                ],
            )
            .expect(reason);
            let settled = chunks.iter().find_map(|chunk| match chunk {
                StreamChunk::Known(KnownStreamChunk::Finish { reason, .. }) => Some(reason.clone()),
                _ => None,
            });
            let settled = settled.expect(reason);
            let kind = match &settled {
                fr_wire::call_config::FinishReason::Known(
                    fr_wire::call_config::FinishReasonKind::Stop,
                ) => "stop",
                fr_wire::call_config::FinishReason::Known(
                    fr_wire::call_config::FinishReasonKind::MaxTokens,
                ) => "length",
                fr_wire::call_config::FinishReason::Known(
                    fr_wire::call_config::FinishReasonKind::ToolCalls,
                ) => "toolUse",
                _ => "",
            };
            assert_eq!(kind, expected, "{reason} settled {settled:?}");
        }
        let mut refusal_stream = translator();
        let error = drain(
            &mut refusal_stream,
            vec![
                start(&base_usage()),
                frame("message_delta", &json!({
                    "type": "message_delta",
                    "delta": { "stop_reason": "refusal", "stop_details": { "explanation": "no" } }
                })),
                frame("message_stop", &json!({ "type": "message_stop" })),
            ],
        )
        .expect_err("refusal settles as an error finish");
        assert_eq!(error, "no");
        let chunks = refusal_stream.settle_chunks(None, None);
        assert!(matches!(
            &chunks[1],
            StreamChunk::Known(KnownStreamChunk::Finish {
                reason: fr_wire::call_config::FinishReason::Known(
                    fr_wire::call_config::FinishReasonKind::Error { .. }
                ),
                ..
            })
        ));

        let mut translator = translator();
        let error = drain(
            &mut translator,
            vec![
                start(&base_usage()),
                frame(
                    "message_delta",
                    &json!({ "type": "message_delta", "delta": { "stop_reason": "wild" } }),
                ),
                frame("message_stop", &json!({ "type": "message_stop" })),
            ],
        )
        .expect_err("unknown stop reason");
        assert_eq!(error, "Unhandled stop reason: wild");
    }

    /// End-of-stream guards: a started stream without `message_stop`
    /// fails; a stream that never settled a stop reason fails with the
    /// pi-ai wording; both settle through the error face.
    #[test]
    fn stream_end_guards() {
        let mut guard_stream = translator();
        let error =
            drain(&mut guard_stream, vec![start(&base_usage())]).expect_err("no message_stop");
        assert_eq!(error, "Anthropic stream ended before message_stop");

        let mut empty_stream = translator();
        let error = drain(&mut empty_stream, Vec::new()).expect_err("no events");
        assert_eq!(error, "Anthropic stream ended without a stop reason");

        let mut error_stream = translator();
        let error = drain(
            &mut error_stream,
            vec![frame("error", &json!("overloaded"))],
        )
        .expect_err("sse error frame");
        // The raw data string throws verbatim (a JSON string body keeps
        // its quotes).
        assert_eq!(error, "\"overloaded\"");
    }

    /// Usage merging is null-guarded: absent fields in `message_delta`
    /// preserve the `message_start` baseline (proxy tolerance), and the
    /// reasoning-token subset lands.
    #[test]
    fn usage_merge_is_null_guarded() {
        let mut translator = translator();
        let chunks = drain(
            &mut translator,
            vec![
                start(&base_usage()),
                frame(
                    "message_delta",
                    &json!({
                        "type": "message_delta",
                        "delta": { "stop_reason": "end_turn" },
                        "usage": { "output_tokens_details": { "thinking_tokens": 7 } }
                    }),
                ),
                frame("message_stop", &json!({ "type": "message_stop" })),
            ],
        )
        .expect("translates");
        let usage = chunks.iter().find_map(|chunk| match chunk {
            StreamChunk::Known(KnownStreamChunk::Usage { usage }) => Some(*usage),
            _ => None,
        });
        let usage = usage.expect("usage");
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.cache_read_tokens, Some(3));
        assert_eq!(usage.reasoning_tokens, Some(7));
    }

    /// The receive-side stealth map: an OAuth stream maps the Claude
    /// Code casing back to the declared tool name.
    #[test]
    fn oauth_receive_side_maps_tool_names() {
        let tools = vec![ToolSchema {
            defer_loading: None,
            name: "bash".to_owned(),
            description: String::new(),
            parameters: fr_wire::RawObject::new(),
        }];
        let mut translator =
            AnthropicTranslator::new("claude-test", "anthropic", None, tools, true);
        let chunks = drain(
            &mut translator,
            vec![
                start(&base_usage()),
                frame("content_block_start", &json!({
                    "type": "content_block_start", "index": 0,
                    "content_block": { "type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {} }
                })),
                frame("content_block_stop", &json!({ "type": "content_block_stop", "index": 0 })),
                frame("message_delta", &json!({
                    "type": "message_delta", "delta": { "stop_reason": "end_turn" }
                })),
                frame("message_stop", &json!({ "type": "message_stop" })),
            ],
        )
        .expect("translates");
        let name = chunks.iter().find_map(|chunk| match chunk {
            StreamChunk::Known(KnownStreamChunk::BlockEnd {
                block: ContentBlock::Known(KnownContentBlock::ToolCall { name, .. }),
                ..
            }) => Some(name.clone()),
            _ => None,
        });
        assert_eq!(name, Some("bash".to_owned()));
    }
}
// Rust guideline compliant 2026-10-27
