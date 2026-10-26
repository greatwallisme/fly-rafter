//! SSE framing and event validation (dsh `sse.ts`; the WHATWG framing that
//! dsh delegates to `eventsource-parser`).
//!
//! Heartbeat comments surface as activity pulses — the observable behavior
//! the two mainstream Rust SSE crates discard, which is why this parser is
//! hand-rolled (Q65A evidence in `findings.md`).
//!
//! UTF-8 byte decoding (dsh's `TextDecoderStream` leg) belongs to the
//! transport seam; this layer consumes decoded text, so a mid-codepoint
//! split is the real transport's problem to reassemble.

use fr_llm::error::LlmError;

use crate::codes::MALFORMED_RESPONSE_CODE;
use crate::transport::provider_error;

/// One dispatched SSE frame (dsh `frame.event` / `frame.data`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    /// The `event:` field; `None` when the frame named none.
    pub event: Option<String>,
    /// The joined `data:` lines.
    pub data: String,
}

/// One parser output: a dispatched frame or a heartbeat comment pulse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseOutput {
    /// A complete frame (blank-line dispatch with non-empty data).
    Frame(SseFrame),
    /// A comment line (`: …`); carries no payload, only liveness.
    Comment,
}

/// Incremental WHATWG SSE parser: feed decoded text, dispatch on blank
/// lines, drop an unterminated tail.
///
/// Lines split on `\r\n`, `\r`, or `\n`; a leading BOM is stripped once;
/// field values drop one optional leading space; `event:` last-wins within
/// a frame; unknown fields (`id`/`retry`/extension names) parse and are
/// ignored per spec.
#[derive(Debug, Default)]
pub struct SseParser {
    buf: String,
    first_feed: bool,
    data: Vec<String>,
    event: Option<String>,
}

impl SseParser {
    /// Feeds one decoded text chunk, returning everything it dispatched.
    pub fn feed(&mut self, chunk: &str) -> Vec<SseOutput> {
        let chunk = if self.first_feed {
            chunk
        } else {
            self.first_feed = true;
            chunk.strip_prefix('\u{FEFF}').unwrap_or(chunk)
        };
        self.buf.push_str(chunk);
        let mut outputs = Vec::new();
        while let Some((line, consumed)) =
            take_line(&self.buf).map(|(line, consumed)| (line.to_string(), consumed))
        {
            self.buf.drain(..consumed);
            if let Some(output) = self.consume(&line) {
                outputs.push(output);
            }
        }
        outputs
    }
}

impl SseParser {
    /// Processes one terminated line; `Some` when it dispatches output.
    fn consume(&mut self, line: &str) -> Option<SseOutput> {
        if line.is_empty() {
            return self.dispatch();
        }
        if let Some(comment) = line.strip_prefix(':') {
            let _ = comment;
            return Some(SseOutput::Comment);
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "data" => self.data.push(value.to_string()),
            "event" => self.event = Some(value.to_string()),
            _ => {}
        }
        None
    }

    /// Dispatches the accumulated frame; an empty data buffer dispatches
    /// nothing but still resets the frame state (spec rule).
    fn dispatch(&mut self) -> Option<SseOutput> {
        if self.data.is_empty() {
            self.event = None;
            return None;
        }
        let frame = SseFrame {
            event: self.event.take(),
            data: std::mem::take(&mut self.data).join("\n"),
        };
        Some(SseOutput::Frame(frame))
    }
}

/// Takes the next terminated line: `\n`, `\r\n`, or a lone mid-buffer `\r`.
/// A trailing lone `\r` waits for more input (`\r\n` may straddle chunks).
fn take_line(buf: &str) -> Option<(&str, usize)> {
    for (index, character) in buf.char_indices() {
        match character {
            '\n' => return Some((&buf[..index], index + 1)),
            '\r' if index + 1 < buf.len() => {
                let consumed = if buf[index + 1..].starts_with('\n') {
                    index + 2
                } else {
                    index + 1
                };
                return Some((&buf[..index], consumed));
            }
            '\r' => return None,
            _ => {}
        }
    }
    None
}

/// Decodes complete SSE frames without treating an unterminated tail as an
/// event (dsh `parseSse`).
///
/// `activity` pulses once per heartbeat comment and once per dispatched
/// frame (the idle-watchdog signal the dsh feed supplies).
///
/// # Errors
///
/// In order per frame: invalid frame JSON → `MALFORMED_RESPONSE`
/// `"DeepSeek Messages SSE contains invalid JSON"`; non-object payload →
/// `MALFORMED_RESPONSE` `"DeepSeek Messages expected a JSON object"`;
/// `event.type` missing or a named `event:` field disagreeing with it →
/// `MALFORMED_RESPONSE` `"DeepSeek Messages SSE event type mismatch"`; an
/// in-band `error` event classifies through [`provider_error`] with no
/// status or headers.
pub fn parse_sse(
    body: &str,
    activity: &mut dyn FnMut(),
) -> Result<Vec<serde_json::Value>, LlmError> {
    let mut parser = SseParser::default();
    let mut events = Vec::new();
    for output in parser.feed(body) {
        match output {
            SseOutput::Comment => activity(),
            SseOutput::Frame(frame) => {
                activity();
                let raw: serde_json::Value =
                    serde_json::from_str(&frame.data).map_err(|_invalid_json| {
                        sse_error("DeepSeek Messages SSE contains invalid JSON")
                    })?;
                let kind = validate_frame(&raw, frame.event.as_deref())?;
                if kind == "error" {
                    return Err(provider_error(&raw, None, None, 0));
                }
                events.push(raw);
            }
        }
    }
    Ok(events)
}

/// The frame checks dsh runs between JSON decode and yield: object shape
/// (dsh `object()`) and the `event.type` cross-check.
fn validate_frame<'a>(
    raw: &'a serde_json::Value,
    event: Option<&str>,
) -> Result<&'a str, LlmError> {
    let Some(map) = raw.as_object() else {
        return Err(sse_error("DeepSeek Messages expected a JSON object"));
    };
    let Some(kind) = map.get("type").and_then(serde_json::Value::as_str) else {
        return Err(sse_error("DeepSeek Messages SSE event type mismatch"));
    };
    if event.is_some_and(|name| name != kind) {
        return Err(sse_error("DeepSeek Messages SSE event type mismatch"));
    }
    Ok(kind)
}

/// One static `MALFORMED_RESPONSE` failure (dsh throws; fr returns).
fn sse_error(message: &str) -> LlmError {
    LlmError::try_new(message, MALFORMED_RESPONSE_CODE, None, None, None, None)
        .expect("static message and code satisfy the LlmError invariants")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parser_frames_comments_and_tails() {
        let mut parser = SseParser::default();
        // BOM strips once; CRLF, lone CR, and LF all terminate; multi-line
        // data joins with \n; unknown fields parse and drop; empty-data
        // frames dispatch nothing; an unterminated tail stays pending.
        let outputs = parser
            .feed("\u{FEFF}: ping\r\nevent: message_start\r\nid: 1\r\nretry: 500\r\ndata: {\"a\"");
        assert_eq!(outputs, vec![SseOutput::Comment]);
        // The leading \n terminates chunk 1's unterminated `data:` line.
        let outputs = parser.feed("\n: again\ndata: :1}\n\ndata: second\r\rdata: tail");
        assert_eq!(
            outputs,
            vec![
                SseOutput::Comment,
                SseOutput::Frame(SseFrame {
                    event: Some("message_start".into()),
                    data: "{\"a\"\n:1}".into()
                }),
                SseOutput::Frame(SseFrame {
                    event: None,
                    data: "second".into()
                }),
            ]
        );
        assert_eq!(parser.feed(""), Vec::<SseOutput>::new());
        // A blank line dispatches the accumulated frame; the comment needs
        // its own terminator to pulse.
        let outputs = parser.feed("\n\n: end\n");
        assert_eq!(
            outputs[0],
            SseOutput::Frame(SseFrame {
                event: None,
                data: "tail".into()
            })
        );
        assert_eq!(outputs[1], SseOutput::Comment);
    }

    #[test]
    fn parse_sse_pipeline_validates_dsh_checks() {
        let body = ": ka\n\n\
            event: message_start\ndata: {\"type\":\"message_start\"}\n\n\
            event: content_block_start\ndata: [1,2]\n\n\
            event: ping\ndata: {\"type\":\"message_stop\"}\n\n\
            data: {\"type\":\"error\",\"error\":{\"type\":\"api_error\"}}\n\n";
        // The named event disagreeing with the in-band type rejects before
        // the array shape check runs? No — object shape runs first (dsh).
        let mut pulses = 0_u32;
        let mut activity = || pulses += 1;
        let err = parse_sse(body, &mut activity).expect_err("array payload rejected");
        assert_eq!(err.code(), "MALFORMED_RESPONSE");
        assert_eq!(err.message(), "DeepSeek Messages expected a JSON object");
        let good = ": ka\n\n\
            event: message_start\ndata: {\"type\":\"message_start\"}\n\n\
            data: {\"type\":\"error\",\"error\":{\"type\":\"api_error\"}}\n\n";
        let mut activity = || pulses += 1;
        let err = parse_sse(good, &mut activity).expect_err("in-band error");
        assert_eq!(err.code(), "SERVER");
        let mismatch = "event: ping\ndata: {\"type\":\"message_start\"}\n\n";
        let mut activity = || pulses += 1;
        let err = parse_sse(mismatch, &mut activity).expect_err("name/type mismatch");
        assert_eq!(err.message(), "DeepSeek Messages SSE event type mismatch");
        let broken = "data: not-json\n\n";
        let mut activity = || pulses += 1;
        let err = parse_sse(broken, &mut activity).expect_err("bad json");
        assert_eq!(err.message(), "DeepSeek Messages SSE contains invalid JSON");
        let happy = "event: message_start\ndata: {\"type\":\"message_start\"}\n\n\
            data: {\"type\":\"message_stop\"}\n\n";
        let mut activity = || pulses += 1;
        let events = parse_sse(happy, &mut activity).expect("happy path");
        assert_eq!(
            events,
            vec![
                json!({"type": "message_start"}),
                json!({"type": "message_stop"})
            ]
        );
        assert!(pulses > 0);
    }
}
