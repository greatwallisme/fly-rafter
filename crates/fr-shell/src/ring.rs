// Rust guideline compliant 2026-10-29
//! The bounded output ring behind one job (dsh `jobs-local/src/ring.ts`):
//! chunks at absolute byte offsets, head eviction that never moves an
//! assigned offset, and non-consuming reads from any offset.

use super::jobs_types::JobAppendOptions;
use super::jobs_types::JobChannel;
use super::jobs_types::JobChunk;
use super::jobs_types::JobOutputRead;

/// One retained ring entry; `bytes` caches the chunk's UTF-8 length.
#[derive(Debug, Clone)]
struct RingChunk {
    at: u64,
    text: String,
    bytes: u64,
    channel: Option<JobChannel>,
    gap_before: bool,
}

/// The UTF-8-safe tail of `text` no longer than `max_bytes`: the byte cut
/// advances past continuation bytes so the surviving text never starts
/// inside a code point.
fn utf8_tail(text: &str, max_bytes: u64) -> (String, u64) {
    let raw = text.as_bytes();
    let raw_len = u64::try_from(raw.len()).unwrap_or(u64::MAX);
    let start = raw_len.saturating_sub(max_bytes);
    let start_u = usize::try_from(start).unwrap_or(raw.len());
    let mut index = start_u;
    while index < raw.len() && (raw[index] & 0xC0) == 0x80 {
        index += 1;
    }
    let tail = &text[index.min(text.len())..];
    let bytes = u64::try_from(tail.len()).unwrap_or(0);
    (tail.to_owned(), bytes.min(raw_len))
}

/// Offsets stay absolute across eviction: `earliest` only ever advances.
#[derive(Debug)]
pub struct OutputRing {
    /// Retained chunks in offset order.
    chunks: Vec<RingChunk>,
    /// Sum of the retained chunks' byte lengths.
    retained_bytes: u64,
    /// Total UTF-8 bytes ever appended — the offset the next chunk starts
    /// at.
    total: u64,
    /// Offset of the oldest retained byte (equals `total` when nothing is
    /// retained).
    earliest: u64,
}

impl Default for OutputRing {
    fn default() -> Self {
        Self::new()
    }
}

impl OutputRing {
    /// Creates an empty ring.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            chunks: Vec::new(),
            retained_bytes: 0,
            total: 0,
            earliest: 0,
        }
    }

    /// Total UTF-8 bytes ever appended.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.total
    }

    /// Offset of the oldest retained byte.
    #[must_use]
    pub const fn earliest(&self) -> u64 {
        self.earliest
    }

    /// Appends one chunk and trims the head to `cap`; returns `false` when
    /// the chunk was empty and nothing changed.
    pub fn append(&mut self, text: &str, options: JobAppendOptions, cap: u64) -> bool {
        if text.is_empty() {
            return false;
        }
        let bytes = u64::try_from(text.len()).unwrap_or(u64::MAX);
        self.chunks.push(RingChunk {
            at: self.total,
            text: text.to_owned(),
            bytes,
            channel: options.channel,
            gap_before: options.gap_before,
        });
        self.total = self.total.saturating_add(bytes);
        self.retained_bytes = self.retained_bytes.saturating_add(bytes);
        self.trim(cap);
        true
    }

    /// Drops retained head chunks until the ring fits `cap`; a single
    /// oversized chunk keeps only its UTF-8-safe tail with a `gapBefore`
    /// marker.
    pub fn trim(&mut self, cap: u64) {
        while self.retained_bytes > cap && self.chunks.len() > 1 {
            let dropped = self.chunks.remove(0);
            self.retained_bytes = self.retained_bytes.saturating_sub(dropped.bytes);
        }
        if self.chunks.len() == 1
            && let Some(single) = self.chunks.first_mut()
            && single.bytes > cap
        {
            let (tail, tail_bytes) = utf8_tail(&single.text, cap);
            single.at += single.bytes - tail_bytes;
            single.text = tail;
            single.bytes = tail_bytes;
            single.gap_before = true;
            self.retained_bytes = tail_bytes;
        }
        self.earliest = self.chunks.first().map_or(self.total, |c| c.at);
    }

    /// Retained chunks overlapping `[from, total)` as fresh wire chunks.
    #[must_use]
    pub fn read_from(&self, from: u64) -> JobOutputRead {
        let chunks = self
            .chunks
            .iter()
            .filter(|chunk| chunk.at.saturating_add(chunk.bytes) > from)
            .map(|chunk| JobChunk {
                at: chunk.at,
                text: chunk.text.clone(),
                channel: chunk.channel,
                gap_before: chunk.gap_before.then_some(true),
            })
            .collect();
        JobOutputRead {
            chunks,
            next: self.total,
            lossy: from < self.earliest,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::OutputRing;
    use crate::jobs_types::JobAppendOptions;
    use crate::jobs_types::JobChannel;

    #[test]
    fn empty_appends_change_nothing() {
        let mut ring = OutputRing::new();
        assert!(!ring.append("", JobAppendOptions::default(), 100));
        assert_eq!(ring.total(), 0);
    }

    #[test]
    fn offsets_stay_absolute_across_eviction() {
        let mut ring = OutputRing::new();
        assert!(ring.append("hello", JobAppendOptions::default(), 100));
        assert!(ring.append("world", JobAppendOptions::default(), 8));
        // The head chunk is evicted; its offset band stays reserved.
        assert_eq!(ring.earliest(), 5);
        assert_eq!(ring.total(), 10);
        let read = ring.read_from(0);
        assert!(read.lossy);
        assert_eq!(read.chunks.len(), 1);
        assert_eq!(read.chunks[0].at, 5);
        assert_eq!(read.chunks[0].text, "world");
    }

    #[test]
    fn an_oversized_single_chunk_keeps_its_utf8_safe_tail() {
        let mut ring = OutputRing::new();
        // "hé" is 3 bytes (h = 1, é = 2); a 2-byte cap keeps only "é".
        assert!(ring.append("h\u{e9}", JobAppendOptions::default(), 100));
        ring.trim(2);
        assert_eq!(ring.earliest(), 1);
        let read = ring.read_from(0);
        assert_eq!(read.chunks[0].text, "\u{e9}");
        assert_eq!(read.chunks[0].gap_before, Some(true));
    }

    #[test]
    fn channels_and_gap_markers_survive_reads() {
        let mut ring = OutputRing::new();
        assert!(ring.append(
            "out",
            JobAppendOptions {
                channel: Some(JobChannel::Stdout),
                gap_before: false,
            },
            100
        ));
        assert!(ring.append(
            "err",
            JobAppendOptions {
                channel: Some(JobChannel::Stderr),
                gap_before: true,
            },
            100
        ));
        let read = ring.read_from(0);
        assert_eq!(read.chunks[0].channel, Some(JobChannel::Stdout));
        assert_eq!(read.chunks[1].channel, Some(JobChannel::Stderr));
        assert_eq!(read.chunks[1].gap_before, Some(true));
        assert_eq!(read.next, 6);
        assert!(!read.lossy);
    }
}
