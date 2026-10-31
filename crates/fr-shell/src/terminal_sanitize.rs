// Rust guideline compliant 2026-10-31
//! The streaming terminal-control sanitizer (the dsh `terminal-bash`
//! `sanitize.ts` port: OSC/CSI stripping with split-sequence carry, the
//! owned prompt marker protocol, and CRLF normalization).
//!
//! Full terminal emulation is deliberately out of scope here: ordinary line
//! output and the private prompt marker are the supported contract, while
//! the protocol-reply layer rides the emulator in
//! [`crate::terminal_session`].

/// The OSC marker prefix the controlled shell emits before each prompt.
pub const PROMPT_MARKER_PREFIX: &str = "133;D;";

/// The exact printable prompt emitted after the private marker.
pub const CONTROLLED_PROMPT: &str = "fr> ";

/// One sanitized chunk plus whether it contained the owned prompt marker.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SanitizedChunk {
    /// Printable text after control stripping.
    text: String,
    /// Whether the owned prompt marker completed in this chunk.
    prompt: bool,
    /// Printable text after the latest owned marker in this chunk.
    prompt_tail: Option<String>,
}

impl SanitizedChunk {
    /// The printable text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether the owned prompt marker completed.
    #[must_use]
    pub const fn prompt(&self) -> bool {
        self.prompt
    }

    /// The printable tail after the latest owned marker.
    #[must_use]
    pub fn prompt_tail(&self) -> Option<&str> {
        self.prompt_tail.as_deref()
    }
}

/// The discard continuation state across chunk boundaries.
#[derive(Debug, Default, PartialEq, Eq)]
enum DiscardMode {
    /// An unterminated OSC sequence.
    #[default]
    None,
    /// An unterminated OSC sequence continuing into the next chunk.
    Osc,
    /// An unterminated CSI sequence continuing into the next chunk.
    Csi,
    /// An OSC escape-continuation (`ESC \`) whose leading backslash lands
    /// in the next chunk.
    OscEscape,
}

/// The streaming sanitizer with its split-sequence carry state.
#[derive(Debug)]
pub struct TerminalSanitizer {
    pending: String,
    discard_mode: DiscardMode,
    trailing_carriage_return: bool,
    tracking_prompt_tail: bool,
    max_pending_bytes: usize,
}

impl TerminalSanitizer {
    /// Builds one sanitizer with the pending-fragment byte bound.
    #[must_use]
    pub const fn new(max_pending_bytes: usize) -> Self {
        Self {
            pending: String::new(),
            discard_mode: DiscardMode::None,
            trailing_carriage_return: false,
            tracking_prompt_tail: false,
            max_pending_bytes,
        }
    }

    /// Consumes one decoded pty data chunk (the dsh `push`).
    pub fn push(&mut self, chunk: &str) -> SanitizedChunk {
        let prefix = self.discard_prefix(chunk);
        self.pending.push_str(&prefix);
        let mut text = String::new();
        let mut prompt = false;
        let mut include_prompt_tail = self.tracking_prompt_tail;
        let mut prompt_tail = String::new();
        let mut index = 0;
        let pending = self.pending.clone();
        let byte_len = pending.len();
        let append_text =
            |include: bool, segment: &str, text: &mut String, prompt_tail: &mut String| {
                text.push_str(segment);
                if include {
                    prompt_tail.push_str(segment);
                }
            };
        while index < byte_len {
            let Some(escape) = pending[index..].find('\x1b').map(|offset| offset + index) else {
                append_text(
                    include_prompt_tail,
                    &pending[index..],
                    &mut text,
                    &mut prompt_tail,
                );
                index = byte_len;
                break;
            };
            append_text(
                include_prompt_tail,
                &pending[index..escape],
                &mut text,
                &mut prompt_tail,
            );
            if escape + 1 >= byte_len {
                index = escape;
                break;
            }
            let kind = pending.as_bytes()[escape + 1];
            if kind == b']' {
                let bel = pending[escape + 2..]
                    .find('\x07')
                    .map(|offset| offset + escape + 2);
                let string_terminator = pending[escape + 2..]
                    .find("\x1b\\")
                    .map(|offset| offset + escape + 2);
                let end = match (bel, string_terminator) {
                    (Some(bel), Some(st)) => Some(bel.min(st + 1) + 1),
                    (Some(bel), None) => Some(bel + 1),
                    (None, Some(st)) => Some(st + 2),
                    (None, None) => None,
                };
                let Some(end) = end else {
                    index = escape;
                    break;
                };
                let terminator_bytes = if pending.as_bytes()[end - 1] == 0x07 {
                    1
                } else {
                    2
                };
                let content = &pending[escape + 2..end - terminator_bytes];
                if content.starts_with(PROMPT_MARKER_PREFIX) {
                    prompt = true;
                    self.tracking_prompt_tail = true;
                    include_prompt_tail = true;
                    prompt_tail.clear();
                }
                index = end;
                continue;
            }
            if kind == b'[' {
                let mut end = escape + 2;
                while end < byte_len {
                    let code = pending.as_bytes()[end];
                    if (0x40..=0x7e).contains(&code) {
                        break;
                    }
                    end += 1;
                }
                if end >= byte_len {
                    index = escape;
                    break;
                }
                index = end + 1;
                continue;
            }
            // The two-byte escape family (save/restore cursor and similar).
            index = escape + 2;
        }
        pending[index..].clone_into(&mut self.pending);
        self.enforce_pending_bound();
        SanitizedChunk {
            text: self.normalize_text(&text),
            prompt,
            prompt_tail: include_prompt_tail.then_some(prompt_tail),
        }
    }

    /// Flushes a trailing printable fragment when the pty exits (incomplete
    /// escapes are discarded).
    pub fn flush(&mut self) -> String {
        let text = std::mem::take(&mut self.pending);
        let text = if text.starts_with('\x1b') {
            String::new()
        } else {
            text
        };
        self.discard_mode = DiscardMode::None;
        self.tracking_prompt_tail = false;
        let normalized = self.normalize_text(&text);
        if !self.trailing_carriage_return {
            return normalized;
        }
        self.trailing_carriage_return = false;
        format!("{normalized}\n")
    }

    /// Holds a trailing CR across pushes, then normalizes CRLF and lone CR
    /// to LF and strips BEL (the dsh `normalizeText`).
    fn normalize_text(&mut self, text: &str) -> String {
        let complete = if self.trailing_carriage_return {
            let mut joined = String::with_capacity(text.len() + 1);
            joined.push('\r');
            joined.push_str(text);
            joined
        } else {
            text.to_owned()
        };
        self.trailing_carriage_return = false;
        let ends_with_cr = complete.ends_with('\r');
        let body = if ends_with_cr {
            &complete[..complete.len() - 1]
        } else {
            &complete
        };
        self.trailing_carriage_return = ends_with_cr;
        body.replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\x07', "")
    }

    /// Discards the pending fragment when it exceeds the byte bound (the
    /// dsh `enforcePendingBound`; the discard arm resumes on the next
    /// terminator in `discard_prefix`).
    fn enforce_pending_bound(&mut self) {
        if self.pending.len() <= self.max_pending_bytes {
            return;
        }
        self.discard_mode = if self.pending.as_bytes().get(1) == Some(&b']') {
            DiscardMode::Osc
        } else {
            DiscardMode::Csi
        };
        self.pending.clear();
    }

    /// Strips a continuing discard arm off the chunk head (the dsh
    /// `discardPrefix`).
    fn discard_prefix(&mut self, chunk: &str) -> String {
        match self.discard_mode {
            DiscardMode::None => return chunk.to_owned(),
            DiscardMode::Csi => {
                for (index, byte) in chunk.bytes().enumerate() {
                    if (0x40..=0x7e).contains(&byte) {
                        self.discard_mode = DiscardMode::None;
                        return chunk[index + 1..].to_owned();
                    }
                }
                return String::new();
            }
            DiscardMode::Osc | DiscardMode::OscEscape => {}
        }
        let mut index = 0;
        let bytes = chunk.as_bytes();
        if self.discard_mode == DiscardMode::OscEscape {
            self.discard_mode = DiscardMode::None;
            if let Some(rest) = chunk.strip_prefix('\\') {
                return rest.to_owned();
            }
        }
        while index < bytes.len() {
            if bytes[index] == 0x07 {
                self.discard_mode = DiscardMode::None;
                return chunk[index + 1..].to_owned();
            }
            if bytes[index] == 0x1b {
                if bytes.get(index + 1) == Some(&b'\\') {
                    self.discard_mode = DiscardMode::None;
                    return chunk[index + 2..].to_owned();
                }
                if index + 1 == bytes.len() {
                    self.discard_mode = DiscardMode::OscEscape;
                }
            }
            index += 1;
        }
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::TerminalSanitizer;

    #[test]
    fn strips_osc_and_csi_while_keeping_plain_text() {
        let mut sanitizer = TerminalSanitizer::new(4096);
        let chunk = sanitizer.push("abc\x1b]0;title\x07def\x1b[2Kghi");
        assert_eq!(chunk.text(), "abcdefghi");
        assert!(!chunk.prompt());
    }

    #[test]
    fn detects_the_owned_prompt_marker_and_tail() {
        let mut sanitizer = TerminalSanitizer::new(4096);
        let marker = "\x1b]133;D;0\x07fr> ".to_owned();
        let chunk = sanitizer.push(&marker);
        assert!(chunk.prompt());
        assert_eq!(chunk.prompt_tail(), Some("fr> "));
    }

    #[test]
    fn carries_split_escape_sequences_across_chunks() {
        let mut sanitizer = TerminalSanitizer::new(4096);
        let head = sanitizer.push("text\x1b]0;ti");
        assert_eq!(head.text(), "text");
        let tail = sanitizer.push("tle\x07more");
        assert_eq!(tail.text(), "more");
    }

    #[test]
    fn normalizes_crlf_and_holds_trailing_cr() {
        let mut sanitizer = TerminalSanitizer::new(4096);
        let head = sanitizer.push("a\r\nb\r");
        assert_eq!(head.text(), "a\nb");
        let tail = sanitizer.push("\nc");
        assert_eq!(tail.text(), "\nc");
    }

    #[test]
    fn flush_folds_the_held_cr_and_drops_incomplete_escapes() {
        let mut sanitizer = TerminalSanitizer::new(4096);
        let _chunk = sanitizer.push("x\r\x1b");
        let flushed = sanitizer.flush();
        // "x" already rode the push text; the held CR folds the flush tail.
        assert_eq!(flushed, "\n");
    }
}
