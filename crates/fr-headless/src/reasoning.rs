// Rust guideline compliant 2026-11-04
//! Provider reasoning projected to stderr as it streams (the dsh
//! `index.ts` `streamReasoning` face).
//!
//! The header prints once per burst, an unterminated line is closed with a
//! newline at block and attempt boundaries, and final-outcome derivation
//! stays on the durable log.
//!
//! Frames arrive from the consumed `agent` service's
//! `agent/assistant-stream` publication (the fr-agent-loop frame
//! vocabulary; the bus batches them, the caller iterates per frame).

use fr_agent_loop::assistant_stream::{AssistantStreamFrame, FrameOutcome};
use fr_wire::stream::{KnownStreamChunk, StreamChunk};

use crate::json_stream::JsonSink;

/// The stderr header opening one reasoning burst (PRD 18.1 names the
/// `fr: reasoning:` title).
const REASONING_HEADER: &str = "fr: reasoning:\n";

/// The streaming state of one reasoning projection.
#[derive(Debug, Default)]
pub struct ReasoningStream {
    /// A reasoning burst is open on stderr (the header has printed).
    open: bool,
    /// The last written delta ended with a newline (no closing write due).
    ends_with_newline: bool,
}

impl ReasoningStream {
    /// An idle projection: no header printed yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            open: false,
            ends_with_newline: true,
        }
    }

    /// Closes an open burst: newline only when the tail lacks one.
    pub fn close(&mut self, sink: &mut dyn JsonSink) {
        if !self.open {
            return;
        }
        if !self.ends_with_newline {
            sink.write("\n");
        }
        self.open = false;
        self.ends_with_newline = true;
    }

    /// Folds one assistant-stream frame into the projection.
    pub fn observe(&mut self, frame: &AssistantStreamFrame, sink: &mut dyn JsonSink) {
        match frame {
            AssistantStreamFrame::Start { .. } | AssistantStreamFrame::End { .. } => {
                self.close(sink);
            }
            AssistantStreamFrame::Chunk { chunk, .. } => match chunk.as_ref() {
                StreamChunk::Known(KnownStreamChunk::ReasoningDelta { text, .. }) => {
                    if text.is_empty() {
                        return;
                    }
                    if !self.open {
                        sink.write(REASONING_HEADER);
                        self.open = true;
                    }
                    sink.write(text);
                    self.ends_with_newline = text.ends_with('\n');
                }
                StreamChunk::Known(KnownStreamChunk::BlockStart { block_type, .. }) => {
                    if block_type != "reasoning" {
                        self.close(sink);
                    }
                }
                StreamChunk::Known(KnownStreamChunk::BlockEnd { block, .. }) => {
                    let reasoning = matches!(
                        block,
                        fr_wire::ContentBlock::Known(fr_wire::KnownContentBlock::Reasoning { .. })
                    );
                    if !reasoning {
                        self.close(sink);
                    }
                }
                // The accounting chunk never interrupts a reasoning burst
                // (dsh ignores it); every other delta kind and the terminal
                // finish do.
                // The accounting chunk never interrupts a reasoning burst,
                // and adapter-added kinds stay inert (the closed-union guard
                // is the projection's default arm, not a failure); every
                // other known kind and the terminal finish close it.
                StreamChunk::Known(KnownStreamChunk::Usage { .. }) | StreamChunk::Other(_) => {}
                StreamChunk::Known(_) => self.close(sink),
            },
        }
    }

    /// Whether an attempt's terminal frame carries a committed settlement
    /// (diagnostic helper for the runner's logging; the projection itself
    /// closes on every `End` regardless of outcome).
    #[must_use]
    pub const fn settled(outcome: &FrameOutcome) -> bool {
        matches!(outcome, FrameOutcome::Committed { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::ReasoningStream;
    use crate::json_stream::JsonSink;
    use fr_agent_loop::assistant_stream::AssistantStreamFrame;
    use fr_wire::stream::{KnownStreamChunk, StreamChunk};

    #[derive(Default)]
    struct Buffer(Vec<String>);

    impl JsonSink for Buffer {
        fn write(&mut self, chunk: &str) {
            self.0.push(chunk.to_owned());
        }
    }

    fn chunk_frame(chunk: KnownStreamChunk) -> AssistantStreamFrame {
        AssistantStreamFrame::Chunk {
            attempt_id: fr_wire::LlmAttemptId::new("a1"),
            revision: 1,
            index: 0,
            time: 0,
            chunk: Box::new(StreamChunk::Known(chunk)),
        }
    }

    fn end_frame() -> AssistantStreamFrame {
        AssistantStreamFrame::End {
            attempt_id: fr_wire::LlmAttemptId::new("a1"),
            revision: 2,
            index: 1,
            outcome: fr_agent_loop::assistant_stream::FrameOutcome::Committed {
                event_type: fr_agent_loop::assistant_stream::SettlementType::AssistantMessage,
                seq: fr_wire::SessionSeq::new(5).expect("in range"),
            },
        }
    }

    fn written(buffer: &Buffer) -> String {
        buffer.0.concat()
    }

    #[test]
    fn header_prints_once_per_burst_and_deltas_append() {
        let mut buffer = Buffer::default();
        let mut stream = ReasoningStream::new();
        let delta = |text: &str| {
            chunk_frame(KnownStreamChunk::ReasoningDelta {
                index: 0,
                text: text.to_owned(),
            })
        };
        stream.observe(&delta(""), &mut buffer);
        stream.observe(&delta("th"), &mut buffer);
        stream.observe(&delta("ink"), &mut buffer);
        stream.observe(&delta("ing"), &mut buffer);
        assert_eq!(written(&buffer), "fr: reasoning:\nthinking");
        // A second burst after an explicit close prints the header again.
        stream.close(&mut buffer);
        stream.observe(&delta(" more"), &mut buffer);
        assert!(written(&buffer).ends_with("\nfr: reasoning:\n more"));
    }

    #[test]
    fn unterminated_tails_close_with_a_newline_at_boundaries() {
        let mut buffer = Buffer::default();
        let mut stream = ReasoningStream::new();
        stream.observe(
            &chunk_frame(KnownStreamChunk::ReasoningDelta {
                index: 0,
                text: "open".to_owned(),
            }),
            &mut buffer,
        );
        // A visible text block interrupts the burst: the tail is closed.
        stream.observe(
            &chunk_frame(KnownStreamChunk::BlockStart {
                index: 1,
                block_type: "text".to_owned(),
            }),
            &mut buffer,
        );
        assert_eq!(written(&buffer), "fr: reasoning:\nopen\n");
    }

    #[test]
    fn reasoning_block_edges_do_not_close_the_burst() {
        let mut buffer = Buffer::default();
        let mut stream = ReasoningStream::new();
        stream.observe(
            &chunk_frame(KnownStreamChunk::BlockStart {
                index: 0,
                block_type: "reasoning".to_owned(),
            }),
            &mut buffer,
        );
        stream.observe(
            &chunk_frame(KnownStreamChunk::ReasoningDelta {
                index: 0,
                text: "mid".to_owned(),
            }),
            &mut buffer,
        );
        stream.observe(
            &chunk_frame(KnownStreamChunk::BlockEnd {
                index: 0,
                block: fr_wire::ContentBlock::Known(fr_wire::KnownContentBlock::Reasoning {
                    text: "mid".to_owned(),
                }),
            }),
            &mut buffer,
        );
        // The burst stayed open across the reasoning edges; the final close
        // finishes the line.
        stream.close(&mut buffer);
        assert_eq!(written(&buffer), "fr: reasoning:\nmid\n");
    }

    #[test]
    fn usage_does_not_interrupt_but_deltas_and_finish_do() {
        let mut buffer = Buffer::default();
        let mut stream = ReasoningStream::new();
        stream.observe(
            &chunk_frame(KnownStreamChunk::Usage {
                usage: fr_wire::call_config::TokenUsage {
                    input_tokens: 1,
                    output_tokens: 1,
                    total_tokens: None,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                },
            }),
            &mut buffer,
        );
        assert!(written(&buffer).is_empty(), "usage before any burst");
        stream.observe(
            &chunk_frame(KnownStreamChunk::ReasoningDelta {
                index: 0,
                text: "x".to_owned(),
            }),
            &mut buffer,
        );
        stream.observe(&end_frame(), &mut buffer);
        assert_eq!(written(&buffer), "fr: reasoning:\nx\n");
    }
}
