//! The pi-ai SSE decoder for anthropic-messages (dsh pi-ai
//! `api/anthropic-messages.ts` `iterateSseMessages`/`decodeSseLine`/
//! `consumeLine`, anthropic-messages.ts:379-528).
//!
//! This is the pi-ai in-file decoder, not the openai-SDK
//! `LineDecoder`/`SSEDecoder` (fr-llm-openai `sse.rs` is a different
//! source) and not WHATWG: only `event` and `data` fields are
//! recognized, `:`-lines are comments, one leading space is stripped
//! from a field value (never from the field name), `data` lines join
//! with `\n`, and the frame dispatches on an empty line.
//!
//! Line termination follows pi-ai `consumeLine`: `\r`, `\n`, or `\r\n`
//! each end one line, and a lone `\r` at a chunk boundary terminates
//! immediately — a `\r\n` split across chunks therefore emits the
//! pending frame at the empty continuation line, exactly as the JS loop
//! does. `0x0a`/`0x0d` never occur inside a multi-byte UTF-8 sequence,
//! so byte-level line splitting is safe; each complete line decodes with
//! replacement semantics (the JS `TextDecoder` default).

/// One decoded server-sent event (pi-ai `ServerSentEvent`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    /// The `event:` field value, when the frame carried one.
    pub event: Option<String>,
    /// The `data:` lines joined with `\n`.
    pub data: String,
    /// Every non-empty raw line of the frame, in arrival order.
    pub raw: Vec<String>,
}

/// Incremental pi-ai SSE decoder: feed byte chunks, collect completed
/// frames, finish at end-of-stream (the unterminated tail line decodes,
/// then the trailing frame flushes).
#[derive(Debug, Default)]
pub struct SseDecoder {
    /// The pending `event:` value.
    event: Option<String>,
    /// The pending `data:` lines.
    data: Vec<String>,
    /// The raw lines of the pending frame.
    raw: Vec<String>,
    /// Undecoded bytes (a line may continue into the next chunk).
    buf: Vec<u8>,
}

impl SseDecoder {
    /// Creates an empty decoder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one byte chunk, returning every frame the chunk completed.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseFrame> {
        self.buf.extend_from_slice(chunk);
        let mut frames = Vec::new();
        while let Some((line, consumed)) = consume_line(&self.buf) {
            self.buf.drain(..consumed);
            if let Some(frame) = self.decode_line(&line) {
                frames.push(frame);
            }
        }
        frames
    }

    /// Ends the stream (pi-ai post-read tail, anthropic-messages.ts:503-524):
    /// an unterminated tail line decodes as one line, then the trailing
    /// frame flushes.
    pub fn finish(&mut self) -> Vec<SseFrame> {
        let mut frames = Vec::new();
        if !self.buf.is_empty() {
            let line = String::from_utf8_lossy(&self.buf).into_owned();
            self.buf.clear();
            if let Some(frame) = self.decode_line(&line) {
                frames.push(frame);
            }
        }
        if let Some(frame) = self.flush() {
            frames.push(frame);
        }
        frames
    }

    /// Decodes one complete line; an empty line dispatches the pending
    /// frame (pi-ai `decodeSseLine`).
    fn decode_line(&mut self, line: &str) -> Option<SseFrame> {
        if line.is_empty() {
            return self.flush();
        }
        self.raw.push(line.to_owned());
        if line.starts_with(':') {
            return None;
        }
        let (name, value) = line.find(':').map_or((line, ""), |index| {
            let raw = &line[index + 1..];
            // One leading space is stripped from the value only.
            (&line[..index], raw.strip_prefix(' ').unwrap_or(raw))
        });
        match name {
            "event" => self.event = Some(value.to_owned()),
            "data" => self.data.push(value.to_owned()),
            _ => {}
        }
        None
    }

    /// Flushes the pending frame (pi-ai `flushSseEvent`): a frame with no
    /// event and no data lines is nothing.
    fn flush(&mut self) -> Option<SseFrame> {
        if self.event.is_none() && self.data.is_empty() {
            return None;
        }
        Some(SseFrame {
            event: self.event.take(),
            data: std::mem::take(&mut self.data).join("\n"),
            raw: std::mem::take(&mut self.raw),
        })
    }
}

/// Splits the first line off the buffer (pi-ai `consumeLine`,
/// anthropic-messages.ts:454-469): the first `\r`, `\n`, or `\r\n` ends
/// the line; a lone trailing `\r` terminates without waiting for a
/// possible `\n` (the split-`\r\n` quirk is pi-ai behavior and is
/// preserved).
fn consume_line(buf: &[u8]) -> Option<(String, usize)> {
    let index = buf
        .iter()
        .position(|&byte| byte == b'\r' || byte == b'\n')?;
    let mut consumed = index + 1;
    if buf[index] == b'\r' && buf.get(consumed) == Some(&b'\n') {
        consumed += 1;
    }
    let line = String::from_utf8_lossy(&buf[..index]).into_owned();
    Some((line, consumed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(decoder: &mut SseDecoder, input: &[u8]) -> Vec<SseFrame> {
        let mut all = decoder.feed(input);
        all.extend(decoder.finish());
        all
    }

    #[test]
    fn joins_data_lines_and_dispatches_on_empty_line() {
        let mut decoder = SseDecoder::new();
        let out = frames(
            &mut decoder,
            b"event: message_start\ndata: {\"a\":1}\ndata: {\"b\":2}\n\n",
        );
        assert_eq!(
            out,
            vec![SseFrame {
                event: Some("message_start".to_owned()),
                data: "{\"a\":1}\n{\"b\":2}".to_owned(),
                raw: vec![
                    "event: message_start".to_owned(),
                    "data: {\"a\":1}".to_owned(),
                    "data: {\"b\":2}".to_owned(),
                ],
            }]
        );
    }

    #[test]
    fn crlf_and_lone_cr_terminate_lines() {
        let mut decoder = SseDecoder::new();
        let out = frames(&mut decoder, b"event: e\r\ndata: a\r\r");
        assert_eq!(out.len(), 1, "the lone \\r emits the empty dispatch line");
        assert_eq!(out[0].event.as_deref(), Some("e"));
        assert_eq!(out[0].data, "a");
    }

    #[test]
    fn split_crlf_across_chunks_keeps_the_pi_ai_quirk() {
        let mut decoder = SseDecoder::new();
        assert_eq!(decoder.feed(b"data: a\r"), [] as [SseFrame; 0]);
        let mut out = decoder.feed(b"\ndata: b\n\n");
        out.extend(decoder.finish());
        // The continuation \n is an empty line: the pending frame
        // dispatches before "data: b" joins it.
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].data, "a");
        assert_eq!(out[1].data, "b");
    }

    #[test]
    fn comments_and_unknown_fields_are_ignored() {
        let mut decoder = SseDecoder::new();
        assert_eq!(
            frames(&mut decoder, b": keep-alive\nid: 7\nretry: 100\n\n"),
            [] as [SseFrame; 0]
        );
    }

    #[test]
    fn field_name_is_not_stripped() {
        let mut decoder = SseDecoder::new();
        let out = frames(&mut decoder, b" data: x\n\n");
        // " data" (leading space on the name) is not the data field.
        assert_eq!(out, [] as [SseFrame; 0]);
    }

    #[test]
    fn value_strips_exactly_one_leading_space() {
        let mut decoder = SseDecoder::new();
        let out = frames(&mut decoder, b"data:  two\n\n");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].data, " two");
    }

    #[test]
    fn colonless_line_is_its_own_field_name() {
        let mut decoder = SseDecoder::new();
        let out = frames(&mut decoder, b"data\n\n");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].data, "");
    }

    #[test]
    fn unterminated_tail_line_and_trailing_flush() {
        let mut decoder = SseDecoder::new();
        assert_eq!(decoder.feed(b"event: e\ndata: tail"), [] as [SseFrame; 0]);
        let out = decoder.finish();
        assert_eq!(
            out,
            vec![SseFrame {
                event: Some("e".to_owned()),
                data: "tail".to_owned(),
                raw: vec!["event: e".to_owned(), "data: tail".to_owned(),],
            }]
        );
    }

    #[test]
    fn terminated_last_line_flushes_at_finish() {
        let mut decoder = SseDecoder::new();
        assert_eq!(decoder.feed(b"data: x\n"), [] as [SseFrame; 0]);
        let out = decoder.finish();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].data, "x");
    }

    #[test]
    fn multibyte_payload_survives_line_splitting() {
        let mut decoder = SseDecoder::new();
        let out = frames(&mut decoder, "data: 思考\n\n".as_bytes());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].data, "思考");
    }
}
// Rust guideline compliant 2026-10-27
