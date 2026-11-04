// Rust guideline compliant 2026-11-04
//! `--json` run projection: a bounded, ordered event stream derived from
//! one agent's durable session events (the dsh `json-stream.ts` face,
//! shapes verbatim).
//!
//! Every projected event is a commit point: text and reasoning come from
//! committed `assistant/message` content, never from a live attempt that
//! may still be retried or discarded.
//!
//! Bounding: every string and key is capped at 8 KiB, one serialized line at
//! 32 KiB (newline reserved), nesting at 64 levels; the terminal `final` is
//! exempt (the lossless contract). The eight-frame output contract is
//! pinned by `fr-schema`'s json-stream documents.

use serde_json::{Map, Value};

use fr_wire::call_config::TokenUsage;
use fr_wire::event::{SessionEvent, SessionEventData};
use fr_wire::stream::{AssistantStreamRecord, KnownStreamChunk, StreamChunk};

/// Default per-string and per-key cap applied to every bounded payload.
pub const MAX_STRING_BYTES: usize = 8 * 1024;

/// Default cap on one serialized event line, newline included; the terminal
/// `final` is exempt.
pub const MAX_EVENT_BYTES: usize = 32 * 1024;

/// Bytes the newline-delimited writer appends after one serialized event.
const LINE_TERMINATOR_BYTES: usize = 1;

/// Maximum container depth one payload keeps before the tail is cut (a
/// pathological argument must not overflow the stack and silently drop the
/// event; cut where no real tool schema reaches).
const MAX_DEPTH: usize = 64;

/// Truncation state threaded through one payload bound.
#[derive(Default)]
struct BoundState {
    truncated: bool,
}

/// Truncates one UTF-8 string to `max_bytes`, dropping a split trailing
/// character.
fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    let bytes = text.as_bytes();
    if bytes.len() <= max_bytes {
        return text.to_owned();
    }
    let decoded = String::from_utf8_lossy(&bytes[..max_bytes]).into_owned();
    // Drop exactly one replacement tail (the dsh slice removes one unit);
    // the byte length of the marker is a char boundary by construction.
    if decoded.ends_with('\u{FFFD}') {
        decoded[..decoded.len() - '\u{FFFD}'.len_utf8()].to_owned()
    } else {
        decoded
    }
}

/// Caps one object key, flagging the payload when it was cut.
fn bound_key(key: &str, max_bytes: usize, state: &mut BoundState) -> String {
    if key.len() <= max_bytes {
        key.to_owned()
    } else {
        state.truncated = true;
        truncate_utf8(key, max_bytes)
    }
}

/// Recursively caps every string in one JSON value, keys included.
fn bound_value(value: &Value, max_bytes: usize, state: &mut BoundState, depth: usize) -> Value {
    match value {
        Value::String(text) => {
            if text.len() <= max_bytes {
                value.clone()
            } else {
                state.truncated = true;
                Value::String(truncate_utf8(text, max_bytes))
            }
        }
        Value::Array(items) => {
            if depth >= MAX_DEPTH {
                state.truncated = true;
                return Value::from("[truncated: depth]");
            }
            Value::Array(
                items
                    .iter()
                    .map(|item| bound_value(item, max_bytes, state, depth + 1))
                    .collect(),
            )
        }
        Value::Object(object) => {
            if depth >= MAX_DEPTH {
                state.truncated = true;
                return Value::from("[truncated: depth]");
            }
            // Two keys sharing a truncated prefix collide last-wins, which
            // only a payload naming two multi-kilobyte keys can reach.
            let mut bounded = Map::new();
            for (key, item) in object {
                bounded.insert(
                    bound_key(key, max_bytes, state),
                    bound_value(item, max_bytes, state, depth + 1),
                );
            }
            Value::Object(bounded)
        }
        _ => value.clone(),
    }
}

/// Bounds every string and key in one payload, adding `truncated: true`
/// when any was cut.
fn bound_json_event(event: &Value, max_string_bytes: usize) -> Value {
    let mut state = BoundState::default();
    let mut bounded = bound_value(event, max_string_bytes, &mut state, 0);
    if state.truncated
        && let Some(object) = bounded.as_object_mut()
    {
        object.insert("truncated".into(), Value::Bool(true));
    }
    bounded
}

/// Serializes one payload under both limits: strings and keys capped at
/// `max_string_bytes`, the serialized line at `max_event_bytes` (the
/// newline reserved).
///
/// When the line is still too long, scalar fields survive and structured
/// fields are dropped; when even those are too long, only `type` and
/// `truncated` remain.
#[must_use]
pub fn bound_json_line(event: &Value, max_string_bytes: usize, max_event_bytes: usize) -> String {
    let limit = max_event_bytes - LINE_TERMINATOR_BYTES;
    let bounded = bound_json_event(event, max_string_bytes);
    let line = serde_json::to_string(&bounded).unwrap_or_else(|_| "{}".to_owned());
    if line.len() <= limit {
        return line;
    }
    let mut scalars = Map::new();
    for (key, value) in bounded.as_object().into_iter().flatten() {
        if !value.is_object() && !value.is_array() {
            scalars.insert(key.clone(), value.clone());
        }
    }
    scalars.insert("truncated".into(), Value::Bool(true));
    let short = serde_json::to_string(&Value::Object(scalars)).unwrap_or_else(|_| "{}".to_owned());
    if short.len() <= limit {
        return short;
    }
    let kind = bounded.get("type").cloned().unwrap_or(Value::Null);
    serde_json::to_string(&serde_json::json!({ "type": kind, "truncated": true }))
        .unwrap_or_else(|_| "{}".to_owned())
}

/// Parses raw tool-call arguments the way the executor sees them.
///
/// Empty input is `{}`; invalid or non-round-trippable JSON (a non-finite
/// number such as `1e400`, which would serialize back as `null`) stays raw
/// text.
#[must_use]
pub fn parse_arguments(raw: &str) -> Value {
    if raw.is_empty() {
        return serde_json::json!({});
    }
    let Ok(parsed) = serde_json::from_str::<Value>(raw) else {
        return Value::String(raw.to_owned());
    };
    if contains_non_finite(&parsed) {
        Value::String(raw.to_owned())
    } else {
        parsed
    }
}

/// Whether one parsed value carries a non-finite number anywhere (such a
/// value would serialize back as `null`, misdescribing the call).
fn contains_non_finite(value: &Value) -> bool {
    match value {
        Value::Number(number) => {
            number.as_f64().is_some_and(f64::is_infinite)
                || number.as_f64().is_some_and(f64::is_nan)
        }
        Value::Array(items) => items.iter().any(contains_non_finite),
        Value::Object(object) => object.values().any(contains_non_finite),
        _ => false,
    }
}

/// Joins the text blocks of a tool result's model-facing content.
#[must_use]
pub fn result_text(blocks: &[fr_wire::ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            fr_wire::ContentBlock::Known(fr_wire::KnownContentBlock::Text { text }) => {
                Some(text.as_str())
            }
            _ => None,
        })
        .collect()
}

/// The usage a provider reported in one attempt's stream, if any: the last
/// usage chunk of the compact durable record list.
#[must_use]
pub fn stream_usage(records: &[AssistantStreamRecord]) -> Option<TokenUsage> {
    let mut usage = None;
    for record in records {
        let AssistantStreamRecord::Chunk { chunk, .. } = record else {
            continue;
        };
        let StreamChunk::Known(KnownStreamChunk::Usage { usage: reported }) = chunk else {
            continue;
        };
        usage = Some(*reported);
    }
    usage
}

/// Accumulated usage for one step plus whether every attempt reported a
/// sample (a retried attempt keeps its usage only in its stream, so the
/// committed message alone would under-report billed tokens).
#[derive(Debug)]
struct StepUsageState {
    usage: Option<TokenUsage>,
    /// False once any attempt in the step omitted a usage sample; the
    /// step starts trusting every attempt (the dsh initial state).
    complete: bool,
}

impl Default for StepUsageState {
    fn default() -> Self {
        Self {
            usage: None,
            complete: true,
        }
    }
}

/// Sums one optional bucket; `None` only when a contribution lacks it.
const fn sum_bucket(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left + right),
        _ => None,
    }
}

/// Accumulates one step's usage across its attempts. An attempt that
/// reports no sample makes the step total unknowable, so the step omits
/// usage entirely rather than publish a partial sum as if it were
/// complete; an optional bucket is summed only when every contribution
/// reports it.
const fn add_usage(state: &StepUsageState, next: Option<TokenUsage>) -> StepUsageState {
    let Some(next) = next else {
        return StepUsageState {
            usage: state.usage,
            complete: false,
        };
    };
    let Some(total) = state.usage else {
        return StepUsageState {
            usage: Some(next),
            complete: state.complete,
        };
    };
    StepUsageState {
        usage: Some(TokenUsage {
            input_tokens: total.input_tokens + next.input_tokens,
            output_tokens: total.output_tokens + next.output_tokens,
            total_tokens: sum_bucket(total.total_tokens, next.total_tokens),
            cache_read_tokens: sum_bucket(total.cache_read_tokens, next.cache_read_tokens),
            cache_write_tokens: sum_bucket(total.cache_write_tokens, next.cache_write_tokens),
            reasoning_tokens: sum_bucket(total.reasoning_tokens, next.reasoning_tokens),
        }),
        complete: state.complete,
    }
}

/// The stdout sink a projection writes newline-delimited events to (the dsh
/// `JsonSink` face; the runner injects the process sink, tests a buffer).
pub trait JsonSink {
    /// Writes one chunk of the event stream.
    fn write(&mut self, chunk: &str);
}

impl<T: JsonSink + ?Sized> JsonSink for &mut T {
    fn write(&mut self, chunk: &str) {
        T::write(*self, chunk);
    }
}

/// The live handle of one `--json` projection over one session's events.
///
/// The opening `session` frame is written at construction, so a caller
/// must construct before submitting the task. Text and reasoning emit only
/// when the step's `assistant/message` commits them, and the terminal
/// `final` frame carries the same lossless answer the default mode prints.
#[derive(Debug)]
pub struct JsonProjection<S: JsonSink> {
    sink: S,
    session_id: String,
    cwd: String,
    max_string_bytes: usize,
    disposed: bool,
    step_usage: StepUsageState,
}

impl<S: JsonSink> JsonProjection<S> {
    /// Starts one projection: writes the opening `session` frame (before
    /// any subscription would begin, the dsh ordering).
    pub fn new(sink: S, session_id: &str, cwd: &str) -> Self {
        Self::with_string_cap(sink, session_id, cwd, MAX_STRING_BYTES)
    }

    /// Starts one projection with an explicit per-string cap (tests pin the
    /// truncation ladder with small caps).
    pub fn with_string_cap(sink: S, session_id: &str, cwd: &str, max_string_bytes: usize) -> Self {
        let mut projection = Self {
            sink,
            session_id: session_id.to_owned(),
            cwd: cwd.to_owned(),
            max_string_bytes,
            disposed: false,
            step_usage: StepUsageState::default(),
        };
        let event = serde_json::json!({
            "type": "session",
            "sessionId": projection.session_id,
            "cwd": projection.cwd,
        });
        projection.emit(&event);
        projection
    }
}

impl<S: JsonSink> JsonProjection<S> {
    /// Bounds and writes one payload line to the sink.
    fn emit(&mut self, event: &Value) {
        let line = bound_json_line(event, self.max_string_bytes, MAX_EVENT_BYTES);
        let mut chunk = line;
        chunk.push('\n');
        self.sink.write(&chunk);
    }

    /// Folds one durable session event into the stream (the dsh
    /// `onSessionEvent` arms; unlisted types are ignored).
    pub fn observe(&mut self, event: &SessionEvent) {
        if self.disposed {
            return;
        }
        match &event.data {
            SessionEventData::TurnStart(data) => {
                let frame = serde_json::json!({
                    "type": "status", "phase": "turn_start", "turn": data.turn,
                });
                self.emit(&frame);
            }
            SessionEventData::StepStart(data) => {
                let frame = serde_json::json!({
                    "type": "status", "phase": "step_start",
                    "turn": data.turn, "step": data.step,
                });
                self.emit(&frame);
            }
            // A retried attempt has no committed message; its billed tokens
            // live only in the stream, so they still reach the step total.
            SessionEventData::AssistantAttempt(data) => {
                let state = std::mem::take(&mut self.step_usage);
                self.step_usage = add_usage(&state, stream_usage(&data.stream));
            }
            SessionEventData::AssistantMessage(data) => {
                let state = std::mem::take(&mut self.step_usage);
                let reported = data.usage.or_else(|| stream_usage(&data.stream));
                self.step_usage = add_usage(&state, reported);
                self.emit_message_blocks(&data.message);
            }
            SessionEventData::StepEnd(data) => {
                let state = std::mem::take(&mut self.step_usage);
                let mut frame = serde_json::json!({
                    "type": "status", "phase": "step_end",
                    "turn": data.turn, "step": data.step,
                });
                if state.complete
                    && let Some(usage) = state.usage
                    && let Some(object) = frame.as_object_mut()
                {
                    // A partial sum would read as an exact total, so the
                    // frame omits usage entirely when any attempt in the
                    // step failed to report one.
                    object.insert(
                        "usage".into(),
                        serde_json::to_value(usage).unwrap_or(Value::Null),
                    );
                }
                self.emit(&frame);
            }
            SessionEventData::TurnEnd(data) => {
                let reason = serde_json::to_value(&data.reason).unwrap_or(Value::Null);
                let frame = serde_json::json!({
                    "type": "status", "phase": "turn_end",
                    "turn": data.turn, "reason": reason,
                });
                self.emit(&frame);
            }
            SessionEventData::ToolCall(data) => {
                let input = parse_arguments(&data.arguments);
                let frame = serde_json::json!({
                    "type": "tool_call", "callId": data.call_id.as_str(),
                    "tool": data.name, "input": input,
                });
                self.emit(&frame);
            }
            SessionEventData::ToolResult(data) => {
                // Compaction replaces older results in the surface; those
                // are history, not this run's output, and would otherwise
                // duplicate a callId.
                if !matches!(event.surface_op, Some(fr_wire::event::SurfaceOp::Append(_))) {
                    return;
                }
                let Some(fr_wire::Message::Known(message)) = Some(&data.message) else {
                    return;
                };
                let fr_wire::KnownMessage::Tool {
                    tool_call_id,
                    is_error,
                    content,
                    ..
                } = message.as_ref()
                else {
                    return;
                };
                let frame = serde_json::json!({
                    "type": "tool_result", "callId": tool_call_id.as_str(),
                    // `isError` is a true-only marker: presence is the flag.
                    "status": if is_error.is_some() { "error" } else { "completed" },
                    "result": result_text(content),
                });
                self.emit(&frame);
            }
            _ => {}
        }
    }
}

impl<S: JsonSink> JsonProjection<S> {
    /// Projects the committed assistant message's visible blocks: reasoning
    /// as `thinking`, text as `text`, in block order. A discarded or
    /// retried attempt never reaches here (it has no committed message).
    fn emit_message_blocks(&mut self, message: &fr_wire::Message) {
        let fr_wire::Message::Known(message) = message else {
            return;
        };
        let fr_wire::KnownMessage::Assistant { content, .. } = message.as_ref() else {
            return;
        };
        for block in content {
            let fr_wire::ContentBlock::Known(block) = block else {
                continue;
            };
            let frame = match block {
                fr_wire::KnownContentBlock::Reasoning { text } => {
                    serde_json::json!({ "type": "thinking", "text": text })
                }
                fr_wire::KnownContentBlock::Text { text } => {
                    serde_json::json!({ "type": "text", "text": text })
                }
                _ => continue,
            };
            self.emit(&frame);
        }
    }

    /// Writes the terminal `final` frame carrying the run's answer text:
    /// the lossless terminal contract, so it is not truncated.
    pub fn finish(&mut self, text: &str) {
        if self.disposed {
            return;
        }
        let line = serde_json::to_string(&serde_json::json!({ "type": "final", "text": text }))
            .unwrap_or_else(|_| "{}".to_owned());
        let mut chunk = line;
        chunk.push('\n');
        self.sink.write(&chunk);
    }

    /// Stops observing the session (the dsh `dispose` unsubscribe).
    pub const fn dispose(&mut self) {
        self.disposed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_EVENT_BYTES, MAX_STRING_BYTES, add_usage, bound_json_line, parse_arguments,
        stream_usage, truncate_utf8,
    };
    use fr_wire::call_config::TokenUsage;
    use fr_wire::stream::{AssistantStreamRecord, KnownStreamChunk, StreamChunk};

    /// One token accounting sample with optional reasoning bucket.
    fn usage(input: u64, output: u64, reasoning: Option<u64>) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            output_tokens: output,
            total_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: reasoning,
        }
    }

    #[test]
    fn truncate_utf8_drops_a_split_tail_and_keeps_exact_fits() {
        let text = "aé日".repeat(4);
        let whole = truncate_utf8(&text, text.len());
        assert_eq!(whole, text);
        // Cutting into `日` (three bytes) drops the split remainder.
        let cut = truncate_utf8(&text, 1 + 2 + 2);
        assert_eq!(cut, "aé");
        assert!(!cut.contains('\u{FFFD}'));
    }

    #[test]
    fn bound_line_truncates_strings_and_flags() {
        let event = serde_json::json!({ "type": "text", "text": "x".repeat(100) });
        let line = bound_json_line(&event, 8, MAX_EVENT_BYTES);
        let parsed: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(parsed["truncated"], true);
        assert_eq!(parsed["text"].as_str().map(str::len), Some(8));
    }

    #[test]
    fn line_ladder_degrades_to_scalars_then_type_only() {
        let event = serde_json::json!({
            "type": "tool_result",
            "callId": "c",
            "status": "completed",
            "result": "y".repeat(200),
            "extra": { "deep": true },
        });
        // Structured fields dropped, scalars survive under a tight line cap
        // (the bounded whole line is ~112 bytes here, the scalar residue
        // ~90).
        let line = bound_json_line(&event, 8, 100);
        let parsed: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(parsed["truncated"], true);
        // The `type` string is itself under the string cap ("tool_result"
        // is 11 bytes), matching the dsh bound.
        assert_eq!(parsed["type"], "tool_res");
        assert_eq!(parsed["callId"], "c");
        assert!(parsed["result"].as_str().expect("scalar").len() <= 8);
        assert!(parsed.get("extra").is_none());
        // Even the scalars do not fit: only type and truncated remain —
        // and the type string itself was capped by the same bound (the
        // dsh fallback reads `bounded.type`).
        let line = bound_json_line(&event, 8, 30);
        let parsed: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(
            parsed,
            serde_json::json!({ "type": "tool_res", "truncated": true })
        );
    }

    #[test]
    fn long_keys_truncate_and_collide_last_wins() {
        let key = "k".repeat(10);
        let mut object = serde_json::Map::new();
        object.insert(key.clone(), serde_json::json!(1));
        object.insert(key, serde_json::json!(2));
        let event = serde_json::json!({ "type": "text", "text": "ok" });
        let event = match event {
            serde_json::Value::Object(mut base) => {
                base.insert("long".to_owned(), serde_json::Value::Object(object));
                serde_json::Value::Object(base)
            }
            _ => unreachable!("literal is an object"),
        };
        let line = bound_json_line(&event, 4, MAX_EVENT_BYTES);
        let parsed: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(parsed["truncated"], true);
        let key = "k".repeat(4);
        // Both source keys truncate onto the same key; the later value wins.
        assert_eq!(parsed["long"][key.as_str()], 2);
    }

    #[test]
    fn depth_beyond_64_cuts_the_tail() {
        let mut deep = serde_json::json!(1);
        for _ in 0..70 {
            deep = serde_json::json!({ "nest": deep });
        }
        let event =
            serde_json::json!({ "type": "tool_call", "callId": "c", "tool": "t", "input": deep });
        let line = bound_json_line(&event, MAX_STRING_BYTES, MAX_EVENT_BYTES);
        assert!(line.contains("[truncated: depth]"));
    }

    #[test]
    fn arguments_parse_like_the_executor() {
        assert_eq!(parse_arguments(""), serde_json::json!({}));
        assert_eq!(parse_arguments("{\"a\":1}"), serde_json::json!({ "a": 1 }));
        assert_eq!(parse_arguments("not json"), serde_json::json!("not json"));
        // `1e400` parses to a non-finite number that would stringify as
        // null; the raw text is kept instead of misdescribing the call.
        assert_eq!(
            parse_arguments("{\"a\":1e400}"),
            serde_json::json!("{\"a\":1e400}")
        );
    }

    #[test]
    fn usage_accumulates_and_omits_partial_steps() {
        let state = add_usage(
            &add_usage(
                &super::StepUsageState::default(),
                Some(usage(10, 5, Some(2))),
            ),
            Some(usage(1, 2, None)),
        );
        let total = state.usage.expect("usage");
        assert_eq!(total.input_tokens, 11);
        assert_eq!(total.output_tokens, 7);
        // One attempt lacked the bucket: the sum is unknowable, omitted.
        assert_eq!(total.reasoning_tokens, None);
        assert!(state.complete);

        let state = add_usage(&super::StepUsageState::default(), None);
        assert!(!state.complete);
    }

    #[test]
    fn stream_usage_takes_the_last_usage_chunk() {
        let chunk = |usage| AssistantStreamRecord::Chunk {
            time: 0,
            chunk: StreamChunk::Known(KnownStreamChunk::Usage { usage }),
        };
        let records = vec![
            chunk(usage(1, 1, None)),
            AssistantStreamRecord::TextChunks {
                time0: 0,
                index: 0,
                dt: vec![1],
                texts: vec!["hi".into()],
            },
            chunk(usage(9, 9, Some(3))),
        ];
        assert_eq!(stream_usage(&records).map(|u| u.input_tokens), Some(9));
        assert_eq!(stream_usage(&[]), None);
    }
}

#[cfg(test)]
mod projection_tests {
    use super::{JsonProjection, JsonSink, result_text};
    use fr_schema::{FrameKind, validate_line};
    use fr_wire::event::{AppendOp, SessionEvent, SessionEventData, SurfaceOp};
    use fr_wire::ids::SessionSeq;

    #[derive(Default)]
    struct Buffer(Vec<String>);

    impl JsonSink for Buffer {
        fn write(&mut self, chunk: &str) {
            self.0.push(chunk.to_owned());
        }
    }

    fn append_event(seq: u64, data: SessionEventData) -> SessionEvent {
        SessionEvent {
            seq: SessionSeq::new(seq).expect("in range"),
            time: 0,
            data,
            ignorable: None,
            surface_op: Some(SurfaceOp::Append(AppendOp)),
            source_event_seqs: None,
        }
    }

    fn replace_event(seq: u64, data: SessionEventData) -> SessionEvent {
        let mut event = append_event(seq, data);
        event.surface_op = Some(SurfaceOp::Replace(fr_wire::event::ReplaceOp {
            op: fr_wire::event::ReplaceTag,
            start_seq: SessionSeq::new(0).expect("in range"),
            end_seq: SessionSeq::new(1).expect("in range"),
        }));
        event
    }

    fn user_message() -> SessionEventData {
        SessionEventData::from_parts(
            "user/message",
            serde_json::json!({
                "message": {
                    "role": "user", "id": "u1",
                    "content": [{ "type": "text", "text": "go" }],
                    "source": { "kind": "user" }
                },
                "source": { "kind": "user" }
            }),
        )
        .expect("shape")
    }

    fn assistant_message(blocks: &serde_json::Value) -> SessionEventData {
        SessionEventData::from_parts(
            "assistant/message",
            serde_json::json!({
                "turn": 1, "step": 1,
                "message": {
                    "role": "assistant", "id": "a1", "content": blocks,
                    "source": { "kind": "model" }
                },
                "stream": []
            }),
        )
        .expect("shape")
    }

    fn tool_call(arguments: &str) -> SessionEventData {
        SessionEventData::from_parts(
            "tool/call",
            serde_json::json!({ "turn": 1, "step": 1, "callId": "c1", "name": "bash", "arguments": arguments }),
        )
        .expect("shape")
    }

    fn tool_result(is_error: bool) -> SessionEventData {
        let mut message = serde_json::json!({
            "role": "tool", "id": "t1",
            "content": [{ "type": "text", "text": "out" }, { "type": "text", "text": "put" }],
            "source": { "kind": "tool" }, "toolCallId": "c1"
        });
        if is_error {
            message["isError"] = serde_json::json!(true);
        }
        SessionEventData::from_parts(
            "tool/result",
            serde_json::json!({ "turn": 1, "step": 1, "message": message }),
        )
        .expect("shape")
    }

    fn step_bounds(kind: &str) -> SessionEventData {
        SessionEventData::from_parts(kind, serde_json::json!({ "turn": 1, "step": 1 }))
            .expect("shape")
    }

    fn turn_bounds(kind: &str) -> SessionEventData {
        let data = match kind {
            "turn/end" => serde_json::json!({ "turn": 1, "reason": { "kind": "completed" } }),
            _ => serde_json::json!({ "turn": 1 }),
        };
        SessionEventData::from_parts(kind, data).expect("shape")
    }

    /// Every emitted line must satisfy the eight-frame contract (the Q5A
    /// schema seed; the e2e validates whole streams in slice d).
    fn assert_frames(buffer: &[String]) {
        assert!(!buffer.is_empty(), "no frames");
        for line in buffer {
            let parsed: serde_json::Value = serde_json::from_str(line.trim_end()).expect("json");
            let kind = validate_line(&parsed)
                .unwrap_or_else(|violations| panic!("{line} violates: {violations:?}"));
            let _ = kind;
        }
    }

    #[test]
    fn projection_opens_with_the_session_frame_and_folds_the_run() {
        let mut buffer = Buffer::default();
        {
            let mut projection = JsonProjection::new(&mut buffer, "session-u", "/repo");
            projection.observe(&append_event(1, user_message()));
            projection.observe(&append_event(2, step_bounds("step/start")));
            projection.observe(&append_event(3, tool_call("{\"cmd\":\"ls\"}")));
            projection.observe(&append_event(4, tool_result(false)));
            projection.observe(&append_event(5, step_bounds("step/end")));
            projection.observe(&append_event(6, turn_bounds("turn/end")));
        }
        let frames: Vec<serde_json::Value> = buffer
            .0
            .iter()
            .map(|line| serde_json::from_str(line.trim_end()).expect("json"))
            .collect();
        assert_frames(&buffer.0);
        // The opening frame names the session and cwd; every line ends with
        // a newline.
        assert_eq!(
            frames[0],
            serde_json::json!({"type": "session", "sessionId": "session-u", "cwd": "/repo"})
        );
        assert!(buffer.0.iter().all(|line| line.ends_with('\n')));
        // Ordered frames: status step_start, tool_call (re-parsed input),
        // tool_result (joined text, completed).
        assert_eq!(frames[1]["type"], "status");
        assert_eq!(frames[1]["phase"], "step_start");
        assert_eq!(
            frames[2],
            serde_json::json!({
                "type": "tool_call", "callId": "c1", "tool": "bash", "input": { "cmd": "ls" }
            })
        );
        assert_eq!(
            frames[3],
            serde_json::json!({
                "type": "tool_result", "callId": "c1", "status": "completed", "result": "output"
            })
        );
        assert_eq!(frames[4]["phase"], "step_end");
        assert_eq!(frames[5]["phase"], "turn_end");
        assert_eq!(
            frames[5]["reason"],
            serde_json::json!({ "kind": "completed" })
        );
    }

    #[test]
    fn committed_blocks_project_as_text_and_thinking() {
        let mut buffer = Buffer::default();
        {
            let mut projection = JsonProjection::new(&mut buffer, "s", "/repo");
            projection.observe(&append_event(
                2,
                assistant_message(&serde_json::json!([
                    { "type": "reasoning", "text": "why" },
                    { "type": "text", "text": "what" }
                ])),
            ));
        }
        let frames: Vec<serde_json::Value> = buffer
            .0
            .iter()
            .skip(1)
            .map(|line| serde_json::from_str(line.trim_end()).expect("json"))
            .collect();
        assert_frames(&buffer.0);
        assert_eq!(
            frames[0],
            serde_json::json!({"type": "thinking", "text": "why"})
        );
        assert_eq!(
            frames[1],
            serde_json::json!({"type": "text", "text": "what"})
        );
    }

    #[test]
    fn replaced_results_never_project() {
        let mut buffer = Buffer::default();
        {
            let mut projection = JsonProjection::new(&mut buffer, "s", "/repo");
            projection.observe(&replace_event(4, tool_result(false)));
        }
        // Only the opening session frame exists.
        assert_eq!(buffer.0.len(), 1);
    }

    #[test]
    fn error_results_project_with_the_error_status() {
        let mut buffer = Buffer::default();
        {
            let mut projection = JsonProjection::new(&mut buffer, "s", "/repo");
            projection.observe(&append_event(4, tool_result(true)));
        }
        let frame: serde_json::Value = serde_json::from_str(buffer.0[1].trim_end()).expect("json");
        assert_eq!(frame["status"], "error");
    }

    #[test]
    fn final_frame_is_lossless_and_dispose_stops_the_stream() {
        let mut buffer = Buffer::default();
        let answer = "z".repeat(10_000);
        {
            let mut projection = JsonProjection::new(&mut buffer, "s", "/repo");
            projection.finish(&answer);
            projection.dispose();
            projection.observe(&append_event(9, user_message()));
            projection.finish("late");
        }
        assert_eq!(buffer.0.len(), 2, "dispose stops observes and re-finish");
        // The final line is the untruncated answer (the lossless contract):
        // the `{"type":"final","text":"` prefix, the `"}` suffix, one
        // newline.
        assert_eq!(buffer.0[1].len(), answer.len() + 24 + 2 + 1);
        let frame: serde_json::Value = serde_json::from_str(buffer.0[1].trim_end()).expect("json");
        assert_eq!(frame["type"], "final");
        assert_eq!(frame["text"].as_str().map(str::len), Some(10_000));
    }

    #[test]
    fn result_text_joins_only_text_blocks() {
        use fr_wire::ContentBlock;
        let blocks = vec![
            ContentBlock::Known(fr_wire::KnownContentBlock::Text { text: "a".into() }),
            ContentBlock::Known(fr_wire::KnownContentBlock::Reasoning {
                text: "hidden".into(),
            }),
            ContentBlock::Known(fr_wire::KnownContentBlock::Text { text: "b".into() }),
        ];
        assert_eq!(result_text(&blocks), "ab");
    }

    #[test]
    fn frame_kind_from_the_stream_dispatches() {
        let line = serde_json::json!({"type": "session", "sessionId": "s", "cwd": "/r"});
        assert_eq!(fr_schema::validate_line(&line), Ok(FrameKind::Session));
    }
}
