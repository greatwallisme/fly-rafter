// Rust guideline compliant 2026-10-31
//! The bounded scrollback and send buffers (the dsh `terminal-bash`
//! `session.ts` `BoundedTextBuffer` port: bytes-plus-lines retention with
//! per-character head eviction and chunk coalescing).
//!
//! The dsh buffer accounts UTF-16 code units and surrogate-pair byte costs
//! because its strings are UTF-16; the fr arm accounts exact UTF-8 char
//! widths over decoded text, which is byte-equivalent for the decoded
//! streams the sanitizer hands it (invalid bytes degrade to `U+FFFD` on
//! both stacks).

use std::collections::VecDeque;

/// The bound at which adjacent text chunks coalesce (the dsh
/// `COALESCED_CHUNK_UNITS`).
const COALESCED_CHUNK_BYTES: usize = 4096;

/// One retained text chunk with its consumed byte prefix.
#[derive(Debug)]
struct Chunk {
    text: String,
    start: usize,
}

/// The amortized bounded text buffer.
#[derive(Debug)]
pub struct BoundedTextBuffer {
    chunks: VecDeque<Chunk>,
    bytes: usize,
    newlines: usize,
    dropped: bool,
    max_bytes: usize,
    max_lines: Option<usize>,
}

impl BoundedTextBuffer {
    /// Builds one buffer with the byte bound and the optional line bound.
    #[must_use]
    pub const fn new(max_bytes: usize, max_lines: Option<usize>) -> Self {
        Self {
            chunks: VecDeque::new(),
            bytes: 0,
            newlines: 0,
            dropped: false,
            max_bytes,
            max_lines,
        }
    }

    /// Whether any retained text was dropped by the bounds.
    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.dropped
    }

    /// Whether no text is retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// Appends one text fragment and evicts past the bounds (the dsh
    /// `append`: coalesce small tails, evict the head per character while
    /// over either bound, and compact a half-consumed head once).
    pub fn append(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.bytes += text.len();
        self.newlines += text.matches('\n').count();
        let coalesce = self
            .chunks
            .back()
            .is_some_and(|tail| tail.text.len() - tail.start <= COALESCED_CHUNK_BYTES);
        match self.chunks.back_mut() {
            Some(tail) if coalesce => tail.text.push_str(text),
            _ => self.chunks.push_back(Chunk {
                text: text.to_owned(),
                start: 0,
            }),
        }
        while self.over_bounds() {
            self.evict_one_char();
            self.dropped = true;
        }
    }

    /// Whether either retention bound is exceeded.
    fn over_bounds(&self) -> bool {
        if self.bytes > self.max_bytes {
            return true;
        }
        self.max_lines
            .is_some_and(|max_lines| self.newlines >= max_lines)
    }

    /// Drops the head's first character (byte accounting plus the newline
    /// counter, then head compaction at half-consumed).
    fn evict_one_char(&mut self) {
        let Some(front) = self.chunks.front_mut() else {
            return;
        };
        let Some(ch) = front.text[front.start..].chars().next() else {
            return;
        };
        self.bytes -= ch.len_utf8();
        if ch == '\n' {
            self.newlines -= 1;
        }
        front.start += ch.len_utf8();
        if front.start == front.text.len() {
            self.chunks.pop_front();
        }
        // Compact a half-consumed head so a small suffix cannot retain an
        // oversized backing store (the dsh half-compaction).
        let compact_due = self
            .chunks
            .front()
            .is_some_and(|front| front.start >= front.text.len() / 2 && front.start > 0);
        if compact_due && let Some(front_mut) = self.chunks.front_mut() {
            front_mut.text = front_mut.text[front_mut.start..].to_owned();
            front_mut.start = 0;
        }
    }

    /// Assembles the retained text.
    #[must_use]
    pub fn snapshot_text(&self) -> String {
        let mut text = String::with_capacity(self.bytes);
        for chunk in &self.chunks {
            text.push_str(&chunk.text[chunk.start..]);
        }
        text
    }

    /// Drains the retained text (the dsh `consume`).
    pub fn consume(&mut self) -> (String, bool) {
        let text = self.snapshot_text();
        let truncated = self.dropped;
        self.chunks.clear();
        self.bytes = 0;
        self.newlines = 0;
        self.dropped = false;
        (text, truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::BoundedTextBuffer;

    #[test]
    fn append_evicts_head_bytes_and_counts_truncation() {
        let mut buffer = BoundedTextBuffer::new(10, None);
        buffer.append("abcdefghij");
        assert_eq!(buffer.snapshot_text(), "abcdefghij");
        assert!(!buffer.truncated());
        buffer.append("klmnop");
        // The head evicts six characters (6 bytes over the 10-byte bound).
        assert_eq!(buffer.snapshot_text(), "ghijklmnop");
        assert!(buffer.truncated());
        assert_eq!(buffer.consume(), ("ghijklmnop".to_owned(), true));
        assert!(buffer.is_empty());
        assert!(!buffer.truncated());
    }

    #[test]
    fn line_bound_evicts_whole_lines_of_overflow() {
        let mut buffer = BoundedTextBuffer::new(1024, Some(3));
        buffer.append("a\nb\nc\nd\n");
        // newlines >= 3 evicts from the head until below the line bound.
        assert_eq!(buffer.snapshot_text(), "c\nd\n");
        assert!(buffer.truncated());
    }

    #[test]
    fn multibyte_characters_evict_by_exact_byte_width() {
        let mut buffer = BoundedTextBuffer::new(6, None);
        buffer.append("aa鱼鱼");
        // 2 + 2 + 3 + 3 = 10 bytes; 4 bytes over -> two ASCII heads evict.
        assert_eq!(buffer.snapshot_text(), "鱼鱼");
        assert!(buffer.truncated());
    }

    #[test]
    fn consume_drains_every_counter() {
        let mut buffer = BoundedTextBuffer::new(1024, Some(2));
        buffer.append("x\n");
        let consumed = buffer.consume();
        assert_eq!(consumed, ("x\n".to_owned(), false));
        assert!(buffer.is_empty());
        assert_eq!(buffer.snapshot_text(), "");
        assert!(!buffer.truncated());
    }
}
