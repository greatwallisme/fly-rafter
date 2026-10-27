//! SSE decoding for the `OpenAI` wire (the npm `OpenAI` SDK's streaming
//! face: `core/streaming.js` + `internal/decoders/line.js`, degenerated
//! to the blocking read loop).
//!
//! This is NOT the WHATWG parser the deepseek face uses: the SDK's decoder
//! recognizes only `event`/`data` fields (no `id`/`retry`), treats `:`
//! lines as comments, joins multi-line `data` with `\n`, drops one leading
//! space from a field value, and flushes one pending event at EOF when the
//! server omitted the trailing blank line. `[DONE]` is a data sentinel.
//! In-band errors (`event: error` or a `data.error` payload) and malformed
//! JSON raise here exactly where the SDK raises, so the adapter's catch
//! chain sees the SDK-shaped message texts.

use crate::codes::PI_AI_ERROR_CODE;
use fr_llm::error::{ConstructionRejection, LlmError};

/// One decoded frame from the SSE body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseFrame {
    /// One data event, JSON-parsed.
    Message {
        /// The `event` field value, when the server set one.
        event: Option<String>,
        /// The parsed `data` JSON.
        data: serde_json::Value,
    },
    /// The `data: [DONE]` sentinel (streaming.js:101).
    Done,
}

/// One decode failure, raised where the SDK raises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseError {
    /// The data JSON did not parse (SDK `SyntaxError` message verbatim).
    Malformed,
    /// An in-band error payload (`event: error`, or `data.error` present):
    /// the message follows the SDK `makeMessage` no-status arm
    /// (`error.message` string, else the JSON of the error object).
    InBand {
        /// The composed error message.
        message: String,
    },
}

impl From<SseError> for LlmError {
    fn from(error: SseError) -> Self {
        let message = match error {
            SseError::Malformed => {
                "Error reading response: malformed server-sent event JSON.".to_owned()
            }
            SseError::InBand { message } => message,
        };
        try_new(&message).unwrap_or_else(|rejection| match rejection {
            ConstructionRejection::EmptyMessage => {
                try_new("Error reading response: malformed server-sent event JSON.")
                    .expect("static fallback text is a valid LlmError")
            }
            _ => unreachable!("only an empty in-band message can reject here"),
        })
    }
}

/// `LlmError::try_new` with the openai-face defaults filled in.
fn try_new(message: &str) -> Result<LlmError, ConstructionRejection> {
    LlmError::try_new(message, PI_AI_ERROR_CODE, None, None, None, None)
}

/// Byte-level line decoding over `\n`, `\r`, and `\r\n` (SDK
/// `LineDecoder`): 0x0a/0x0d never occur inside a multi-byte UTF-8
/// sequence, so byte-split lines decode safely; a trailing `\r` completes
/// its line immediately and a following `\n` (even across chunks) is
/// swallowed as its continuation.
#[derive(Debug, Default)]
struct LineDecoder {
    buffer: Vec<u8>,
    skip_leading_lf: bool,
}

impl LineDecoder {
    /// Append one chunk and return every newly completed line.
    fn decode(&mut self, chunk: &[u8]) -> Vec<String> {
        let mut chunk = chunk;
        if self.skip_leading_lf {
            self.skip_leading_lf = false;
            if chunk.first() == Some(&0x0a) {
                chunk = &chunk[1..];
            }
            if chunk.is_empty() {
                return Vec::new();
            }
        }
        self.buffer.extend_from_slice(chunk);
        let mut lines = Vec::new();
        while let Some((preceding, index, carriage)) = find_newline_index(&self.buffer) {
            let line = String::from_utf8_lossy(&self.buffer[..preceding]).into_owned();
            lines.push(line);
            self.buffer.drain(..index);
            if carriage {
                if self.buffer.first() == Some(&0x0a) {
                    self.buffer.remove(0);
                } else if self.buffer.is_empty() {
                    self.skip_leading_lf = true;
                }
            }
        }
        lines
    }

    /// Emit the remaining unterminated line, if any (SDK `flush` =
    /// `decode('\n')`; the injected newline never surfaces as an empty
    /// line because the buffer is nonempty here).
    fn flush(&mut self) -> Vec<String> {
        self.skip_leading_lf = false;
        if self.buffer.is_empty() {
            return Vec::new();
        }
        self.decode(b"\n")
    }
}

/// Find the next CR/LF byte: (line-end offset, post-terminator offset,
/// whether it was a carriage return).
fn find_newline_index(buffer: &[u8]) -> Option<(usize, usize, bool)> {
    const NEWLINE: u8 = 0x0a;
    const CARRIAGE: u8 = 0x0d;
    buffer
        .iter()
        .position(|&b| b == NEWLINE || b == CARRIAGE)
        .map(|i| {
            let carriage = buffer[i] == CARRIAGE;
            (i, i + 1, carriage)
        })
}

/// Line-level SSE record assembly (SDK `SSEDecoder`): `event`/`data`
/// fields only, `:` comment lines dropped, one leading value space
/// stripped, multi-line `data` joined with `\n`, blank line dispatches the
/// pending record, and EOF flush delivers one pending record exactly once.
#[derive(Debug, Default)]
struct SseDecoder {
    event: Option<String>,
    data: Vec<String>,
}

impl SseDecoder {
    /// Feed one line; `Some((event, data))` when a blank line completed a
    /// record.
    fn decode(&mut self, mut line: &str) -> Option<(Option<String>, String)> {
        if line.ends_with('\r') {
            line = &line[..line.len() - 1];
        }
        if line.is_empty() {
            // Empty line with nothing pending is not an event.
            if self.event.is_none() && self.data.is_empty() {
                return None;
            }
            let event = self.event.take();
            let data = self.data.join("\n");
            self.data.clear();
            return Some((event, data));
        }
        if line.starts_with(':') {
            return None;
        }
        let (fieldname, value) = match line.split_once(':') {
            Some((fieldname, value)) => (fieldname, value),
            None => (line, ""),
        };
        let value = value.strip_prefix(' ').unwrap_or(value);
        if fieldname == "event" {
            self.event = Some(value.to_owned());
        } else if fieldname == "data" {
            self.data.push(value.to_owned());
        }
        // `id`, `retry`, and unknown fields are unrecognized by the SDK.
        None
    }

    /// EOF flush: deliver a pending record whose terminating blank line
    /// the server omitted, exactly once (SDK `SSEDecoder.flush`).
    fn flush(&mut self) -> Option<(Option<String>, String)> {
        self.decode("")
    }
}

/// The blocking-face SSE stream parser: feed body bytes, receive frames.
#[derive(Debug, Default)]
pub struct SseParser {
    lines: LineDecoder,
    records: SseDecoder,
}

impl SseParser {
    /// A parser with nothing buffered.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one body chunk and decode every completed frame.
    ///
    /// # Errors
    ///
    /// [`SseError`] where the SDK raises: malformed data JSON, or an
    /// in-band error payload.
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<SseFrame>, SseError> {
        let mut frames = Vec::new();
        for line in self.lines.decode(chunk) {
            if let Some(frame) = self.decode_line(&line)? {
                frames.push(frame);
            }
        }
        Ok(frames)
    }

    /// Flush the tail line and any pending record at body EOF.
    ///
    /// # Errors
    ///
    /// Same as [`Self::feed`].
    pub fn flush(&mut self) -> Result<Vec<SseFrame>, SseError> {
        let mut frames = Vec::new();
        for line in self.lines.flush() {
            if let Some(frame) = self.decode_line(&line)? {
                frames.push(frame);
            }
        }
        if let Some((event, data)) = self.records.flush()
            && let Some(frame) = Self::decode_record(event, &data)?
        {
            frames.push(frame);
        }
        Ok(frames)
    }

    fn decode_line(&mut self, line: &str) -> Result<Option<SseFrame>, SseError> {
        match self.records.decode(line) {
            Some((event, data)) => Self::decode_record(event, &data),
            None => Ok(None),
        }
    }

    /// One completed record: `[DONE]`, a `thread.*` envelope the openai
    /// face ignores (streaming.js routes those to the assistant-stream
    /// arm pi-ai never consumes here), or a JSON message with the
    /// in-band error checks (streaming.js:101-120: `event: error` uses
    /// `data.error ?? data` as the error object; any `data.error` raises;
    /// the message follows `makeMessage`'s no-status arm).
    fn decode_record(event: Option<String>, data: &str) -> Result<Option<SseFrame>, SseError> {
        if data == "[DONE]" {
            return Ok(Some(SseFrame::Done));
        }
        if event
            .as_deref()
            .is_some_and(|name| name.starts_with("thread."))
        {
            return Ok(None);
        }
        let parsed: serde_json::Value =
            serde_json::from_str(data).map_err(|error: serde_json::Error| {
                let _ = error;
                SseError::Malformed
            })?;
        let error_object = if event.as_deref() == Some("error") {
            parsed
                .get("error")
                .filter(|v| !v.is_null())
                .unwrap_or(&parsed)
        } else {
            parsed
                .get("error")
                .filter(|v| !v.is_null())
                .unwrap_or(&serde_json::Value::Null)
        };
        if !error_object.is_null() {
            let message = match error_object.get("message") {
                Some(message) if message.is_string() => {
                    message.as_str().unwrap_or_default().to_owned()
                }
                _ => serde_json::to_string(error_object).unwrap_or_default(),
            };
            return Err(SseError::InBand { message });
        }
        Ok(Some(SseFrame::Message {
            event,
            data: parsed,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `\n`, `\r`, `\r\n` terminators. A `\r` at one chunk's end whose
    /// `\n` continuation opens the next chunk is one terminator, and a
    /// `\r`-only terminator leaves no blank record line (`\n\r` is an
    /// LF terminator then a CR terminator, not CRLF reversed).
    #[test]
    fn line_terminators_pin_all_three_forms() {
        let mut parser = SseParser::new();
        // "data: 1" (LF), blank (CR alone terminates an empty line), so the
        // first record dispatches; then "data: 2" (CRLF) followed by a
        // blank line dispatches the second.
        let frames = parser
            .feed(b"data: 1\n\r\rdata: 2\r\n\r\n")
            .expect("frames");
        let payloads: Vec<_> = frames
            .iter()
            .map(|f| match f {
                SseFrame::Message { data, .. } => data.clone(),
                SseFrame::Done => serde_json::json!("[DONE]"),
            })
            .collect();
        assert_eq!(payloads, vec![serde_json::json!(1), serde_json::json!(2)]);
        // A trailing CR whose LF arrives in the next chunk: the "3" record
        // is unterminated until the continuation line lands.
        let none = parser.feed(b"data: 3\r").expect("no frames yet");
        assert!(none.is_empty(), "an unterminated record yields no frames");
        let tail = parser.feed(b"\n\n").expect("tail");
        assert_eq!(
            tail,
            vec![SseFrame::Message {
                event: None,
                data: serde_json::json!(3)
            }]
        );
    }

    /// Multi-line `data` joins with `\n`, one leading value space strips,
    /// comment lines and unrecognized fields never become frames, and a
    /// blank line with nothing pending is dropped.
    #[test]
    fn fields_join_and_unknown_lines_drop() {
        let mut parser = SseParser::new();
        let frames = parser
            .feed(b": keep-alive\nid: 7\nretry: 100\n\nevent: named\ndata: [1,\ndata:2]\n\n")
            .expect("frames");
        assert_eq!(frames.len(), 1);
        let SseFrame::Message { event, data } = &frames[0] else {
            panic!("expected a message frame");
        };
        assert_eq!(event.as_deref(), Some("named"));
        assert_eq!(*data, serde_json::json!([1, 2]));
    }

    /// `[DONE]` decodes as the sentinel, the malformed-JSON text is the
    /// SDK `SyntaxError` message, and EOF flush delivers the pending
    /// record exactly once.
    #[test]
    fn done_sentinel_and_malformed_and_eof_flush() {
        let mut parser = SseParser::new();
        let frames = parser
            .feed(b"data: [DONE]\n\ndata: {}\n\n")
            .expect("frames");
        assert_eq!(frames[0], SseFrame::Done);

        let mut broken = SseParser::new();
        let error = broken.feed(b"data: not-json\n\n").expect_err("malformed");
        assert_eq!(error, SseError::Malformed);

        let mut tail = SseParser::new();
        let frames = tail.feed(b"data: 42").expect("no frames yet");
        assert!(frames.is_empty(), "the pending record stays buffered");
        let flushed = tail.flush().expect("flush");
        assert_eq!(flushed.len(), 1);
        assert!(
            tail.flush().expect("second flush").is_empty(),
            "EOF flush delivers the pending record exactly once"
        );
    }

    /// In-band errors: `event: error` without an `error` key uses the
    /// whole data object; `data.error.message` strings pass through; a
    /// non-string message stringifies the object.
    #[test]
    fn band_errors_pin_the_make_message_arms() {
        let mut event_error = SseParser::new();
        let error = event_error
            .feed(b"event: error\ndata: {\"message\":\"boom\"}\n\n")
            .expect_err("in-band");
        assert_eq!(
            error,
            SseError::InBand {
                message: "boom".to_owned()
            }
        );

        let mut data_error = SseParser::new();
        let error = data_error
            .feed(b"data: {\"error\":{\"code\":\"x\",\"message\":\"denied\"}}\n\n")
            .expect_err("in-band");
        assert_eq!(
            error,
            SseError::InBand {
                message: "denied".to_owned()
            }
        );

        let mut non_string = SseParser::new();
        let error = non_string
            .feed(b"data: {\"error\":{\"message\":42}}\n\n")
            .expect_err("in-band");
        assert_eq!(
            error,
            SseError::InBand {
                message: r#"{"message":42}"#.to_owned()
            }
        );
    }

    /// `thread.*` events route to the assistant-stream arm the openai
    /// face never consumes, so they decode to nothing here.
    #[test]
    fn thread_events_are_ignored() {
        let mut parser = SseParser::new();
        let frames = parser
            .feed(b"event: thread.run.created\ndata: {\"a\":1}\n\ndata: 7\n\n")
            .expect("frames");
        assert_eq!(frames.len(), 1);
        assert_eq!(
            frames[0],
            SseFrame::Message {
                event: None,
                data: serde_json::json!(7)
            }
        );
    }
}

// Rust guideline compliant 2026-10-27
