// Rust guideline compliant 2026-10-27
//! Process-local assistant attempt framing and durable stream accumulation.
//!
//! Mirrors dsh `agent-loop/src/assistant-stream.ts`: one model attempt folds
//! into a compact durable stream (the `fr-llm` accumulator) plus ordered
//! transient frames published through the injected emit seam (the
//! `agent/assistant-stream` agent event). The attempt id embeds the session
//! identity and the attached-session-local attempt counter; frame revisions
//! come from the injected allocator so one agent lifecycle keeps them
//! monotone. Time is an explicit parameter (the fr-session clock seam),
//! not a wall-clock read.

use fr_llm::assembler::{AssemblyError, BlockAssembler};
use fr_llm::assistant_stream::{AssistantStreamAccumulator, StreamRejection, TimedStreamChunk};
use fr_wire::call_config::{FinishReason, ReplayEnvelope, TokenUsage};
use fr_wire::ids::{LlmAttemptId, SessionId, SessionSeq};
use fr_wire::stream::StreamChunk;
use fr_wire::{AssistantStreamRecord, ContentBlock};
use serde::{Deserialize, Serialize};

/// The durable settlement type an attempt commits through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettlementType {
    /// A completed assistant message joined the surface.
    #[serde(rename = "assistant/message")]
    AssistantMessage,
    /// The attempt ended without a surface message.
    #[serde(rename = "assistant/attempt")]
    AssistantAttempt,
}

/// The terminal outcome an end frame carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum FrameOutcome {
    /// The durable settlement committed before this notification.
    #[serde(rename_all = "camelCase")]
    Committed {
        /// The durable settlement event type.
        event_type: SettlementType,
        /// The committed event's sequence number.
        seq: SessionSeq,
    },
    /// No durable attempt event could be committed.
    Abandoned,
}

/// One process-local live assistant streaming publication (dsh
/// `AssistantStreamFrame`): the wire shape of the `agent/assistant-stream`
/// event payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AssistantStreamFrame {
    /// The opening marker, published before the first delivered chunk.
    #[serde(rename_all = "camelCase")]
    Start {
        /// The attempt this stream belongs to.
        attempt_id: LlmAttemptId,
        /// Monotone within one attached agent lifecycle; a replacement
        /// restarts at 1.
        revision: u64,
        /// The durable turn owning the request.
        turn: u64,
        /// The durable step owning the request.
        step: u64,
    },
    /// One delivered chunk.
    #[serde(rename_all = "camelCase")]
    Chunk {
        /// The attempt this chunk belongs to.
        attempt_id: LlmAttemptId,
        /// This frame's revision.
        revision: u64,
        /// Dense zero-based position within the attempt.
        index: u64,
        /// Safe-integer timestamp reused by the durable embedded stream.
        time: i64,
        /// The chunk exactly as emitted, boxed to keep the frame variants
        /// within a sane size spread.
        chunk: Box<StreamChunk>,
    },
    /// The terminal settlement or abandonment.
    #[serde(rename_all = "camelCase")]
    End {
        /// The attempt being closed.
        attempt_id: LlmAttemptId,
        /// This frame's revision.
        revision: u64,
        /// The number of chunk frames this attempt emitted.
        index: u64,
        /// How the attempt ended.
        outcome: FrameOutcome,
    },
}

/// Allocates the next emitted frame revision.
pub type RevisionAllocator = Box<dyn FnMut() -> u64>;
/// Publishes one frame (the `agent/assistant-stream` emit seam).
pub type FrameSink = Box<dyn FnMut(&AssistantStreamFrame)>;

/// Folds one model attempt into one compact stream plus ordered transient
/// frames (dsh `AssistantStreamAttempt`).
pub struct AssistantStreamAttempt {
    accumulator: AssistantStreamAccumulator,
    assembler: BlockAssembler,
    index: u64,
    terminal: bool,
    /// Attempt identity unique within this agent lifecycle.
    pub attempt_id: LlmAttemptId,
    /// The durable turn owning the request.
    pub turn: u64,
    /// The durable step owning the request.
    pub step: u64,
    next_revision: RevisionAllocator,
    emit: FrameSink,
}

impl std::fmt::Debug for AssistantStreamAttempt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The injected seams are not debug-printable; the identity and
        // progress fields carry the useful state.
        f.debug_struct("AssistantStreamAttempt")
            .field("attempt_id", &self.attempt_id)
            .field("turn", &self.turn)
            .field("step", &self.step)
            .field("index", &self.index)
            .field("terminal", &self.terminal)
            .finish_non_exhaustive()
    }
}

impl AssistantStreamAttempt {
    /// Builds one attempt: the id embeds `` `<sessionId>:<attempt>` ``, and
    /// both the revision allocator and the frame publisher are injected
    /// seams.
    #[must_use]
    pub fn new(
        session_id: &SessionId,
        attempt: u64,
        turn: u64,
        step: u64,
        next_revision: RevisionAllocator,
        emit: FrameSink,
    ) -> Self {
        Self {
            accumulator: AssistantStreamAccumulator::new(),
            assembler: BlockAssembler::new(),
            index: 0,
            terminal: false,
            attempt_id: LlmAttemptId::new(format!("{session_id}:{attempt}")),
            turn,
            step,
            next_revision,
            emit,
        }
    }

    /// Whether this started attempt has emitted its terminal frame.
    #[must_use]
    pub const fn ended(&self) -> bool {
        self.terminal
    }

    /// Publishes the opening marker before the first delivered chunk.
    pub fn start(&mut self) {
        let revision = (self.next_revision)();
        (self.emit)(&AssistantStreamFrame::Start {
            attempt_id: self.attempt_id.clone(),
            revision,
            turn: self.turn,
            step: self.step,
        });
    }

    /// Snapshots one chunk once, then feeds durable compaction, assembly,
    /// and live publication (dsh `push`; the timestamp is injected).
    ///
    /// # Errors
    ///
    /// Propagates [`StreamRejection`] from the accumulator when the
    /// timestamp or block index leaves the safe-integer range; a rejected
    /// chunk reaches neither the assembler nor the frames.
    pub fn push(&mut self, time: i64, chunk: StreamChunk) -> Result<(), StreamRejection> {
        let timed = self.accumulator.push(TimedStreamChunk { time, chunk })?;
        self.assembler.push(&timed.chunk);
        let revision = (self.next_revision)();
        let index = self.index;
        self.index += 1;
        (self.emit)(&AssistantStreamFrame::Chunk {
            attempt_id: self.attempt_id.clone(),
            revision,
            index,
            time: timed.time,
            chunk: Box::new(timed.chunk),
        });
        Ok(())
    }

    /// Publishes terminal settlement after the matching durable event
    /// commits (dsh `settle`): a rejected durable append abandons the
    /// attempt and re-raises.
    ///
    /// # Errors
    ///
    /// Re-raises the durable append's error after publishing the
    /// abandonment.
    pub fn settle<E>(
        &mut self,
        event_type: SettlementType,
        append: impl FnOnce() -> Result<SessionSeq, E>,
    ) -> Result<(), E> {
        let seq = match append() {
            Ok(seq) => seq,
            Err(error) => {
                self.abandon();
                return Err(error);
            }
        };
        self.terminal = true;
        let revision = (self.next_revision)();
        (self.emit)(&AssistantStreamFrame::End {
            attempt_id: self.attempt_id.clone(),
            revision,
            index: self.index,
            outcome: FrameOutcome::Committed { event_type, seq },
        });
        Ok(())
    }

    /// Publishes abandonment when no durable attempt event can be committed.
    pub fn abandon(&mut self) {
        self.terminal = true;
        let revision = (self.next_revision)();
        (self.emit)(&AssistantStreamFrame::End {
            attempt_id: self.attempt_id.clone(),
            revision,
            index: self.index,
            outcome: FrameOutcome::Abandoned,
        });
    }

    /// The exact compact stream for the final durable event.
    #[must_use]
    pub fn stream(&self) -> Vec<AssistantStreamRecord> {
        self.accumulator.snapshot()
    }

    /// Canonical completed-message blocks from the same chunks.
    ///
    /// # Errors
    ///
    /// [`AssemblyError::IncompleteUnknownBlock`] when an unknown block type
    /// was never closed.
    pub fn blocks(&self) -> Result<Vec<ContentBlock>, AssemblyError> {
        self.assembler.blocks()
    }

    /// The safe visible prefix when cancellation interrupts the attempt.
    #[must_use]
    pub fn interrupted_blocks(&self) -> Vec<ContentBlock> {
        self.assembler.interrupted_blocks()
    }

    /// Latest adapter-reported usage in the stream.
    #[must_use]
    pub const fn usage(&self) -> Option<TokenUsage> {
        self.assembler.usage()
    }

    /// Terminal reason, defaulting to stop when the stream omitted one.
    #[must_use]
    pub fn finish(&self) -> FinishReason {
        self.assembler.finish()
    }

    /// Replay state carried by the terminal finish record.
    ///
    /// # Errors
    ///
    /// [`AssemblyError::IncompleteUnknownBlock`] under the same rules as
    /// [`AssistantStreamAttempt::blocks`].
    pub fn replay_state(&self) -> Result<Option<ReplayEnvelope>, AssemblyError> {
        self.assembler.replay_state()
    }
}

#[cfg(test)]
mod tests {
    use super::{AssistantStreamAttempt, AssistantStreamFrame, FrameOutcome, SettlementType};
    use fr_wire::AssistantStreamRecord;
    use fr_wire::call_config::{FinishReasonKind, TokenUsage};
    use fr_wire::content::{ContentBlock, KnownContentBlock};
    use fr_wire::ids::{SessionId, SessionSeq, ToolCallId};
    use fr_wire::stream::{KnownStreamChunk, StreamChunk};
    use std::cell::RefCell;
    use std::rc::Rc;

    fn session_seq(n: u64) -> SessionSeq {
        SessionSeq::new(n).expect("fixture seq values sit far below the safe-integer cap")
    }

    fn text_delta(index: u64, text: &str) -> StreamChunk {
        StreamChunk::Known(KnownStreamChunk::TextDelta {
            index,
            text: text.to_owned(),
        })
    }

    /// The recorded frames shared between the rig and its assertions.
    type FrameLog = Rc<RefCell<Vec<AssistantStreamFrame>>>;
    /// The rig's monotone revision allocator seam.
    type RigAllocator = Box<dyn FnMut() -> u64>;
    /// The rig's frame publication seam.
    type RigEmitter = Box<dyn FnMut(&AssistantStreamFrame)>;

    /// A shared frame recorder plus a monotone revision allocator.
    fn rig() -> (FrameLog, RigAllocator, RigEmitter) {
        let frames = Rc::new(RefCell::new(Vec::new()));
        let revision = Rc::new(RefCell::new(0u64));
        let frames_for_emit = Rc::clone(&frames);
        let revision_for_alloc = Rc::clone(&revision);
        (
            frames,
            Box::new(move || {
                let mut value = revision_for_alloc.borrow_mut();
                *value += 1;
                *value
            }),
            Box::new(move |frame| frames_for_emit.borrow_mut().push(frame.clone())),
        )
    }

    fn attempt(
        emit: Box<dyn FnMut(&AssistantStreamFrame)>,
        next_revision: Box<dyn FnMut() -> u64>,
    ) -> AssistantStreamAttempt {
        AssistantStreamAttempt::new(&SessionId::new("s-1"), 3, 7, 2, next_revision, emit)
    }

    /// Covers the frame sequence and wire shape: the id embeds
    /// `<sessionId>:<attempt>`, revisions are allocator-monotone, chunk
    /// indexes are dense zero-based, and the committed end frame carries the
    /// settled seq and event type.
    #[test]
    fn frames_pin_sequence_and_wire_shape() {
        let (frames, next_revision, emit) = rig();
        let mut stream = attempt(emit, next_revision);
        assert_eq!(stream.attempt_id.as_str(), "s-1:3");
        assert!(!stream.ended());

        stream.start();
        stream.push(1_000, text_delta(0, "He")).expect("legal push");
        stream
            .push(1_010, text_delta(0, "llo"))
            .expect("legal push");
        stream
            .settle(SettlementType::AssistantMessage, || {
                Ok::<SessionSeq, &str>(session_seq(42))
            })
            .expect("durable append succeeds");
        assert!(stream.ended());

        let frames = frames.borrow();
        assert_eq!(frames.len(), 4);
        assert_eq!(
            frames[0],
            AssistantStreamFrame::Start {
                attempt_id: stream.attempt_id.clone(),
                revision: 1,
                turn: 7,
                step: 2,
            }
        );
        assert_eq!(
            frames[1],
            AssistantStreamFrame::Chunk {
                attempt_id: stream.attempt_id.clone(),
                revision: 2,
                index: 0,
                time: 1_000,
                chunk: Box::new(text_delta(0, "He")),
            }
        );
        assert_eq!(
            frames[2],
            AssistantStreamFrame::Chunk {
                attempt_id: stream.attempt_id.clone(),
                revision: 3,
                index: 1,
                time: 1_010,
                chunk: Box::new(text_delta(0, "llo")),
            }
        );
        assert_eq!(
            frames[3],
            AssistantStreamFrame::End {
                attempt_id: stream.attempt_id.clone(),
                revision: 4,
                index: 2,
                outcome: FrameOutcome::Committed {
                    event_type: SettlementType::AssistantMessage,
                    seq: session_seq(42),
                },
            }
        );

        // Wire shape: type tag and camelCase keys on every frame family.
        let start = serde_json::to_value(&frames[0]).expect("start serializes");
        assert_eq!(
            start,
            serde_json::json!({
                "type": "start",
                "attemptId": "s-1:3",
                "revision": 1,
                "turn": 7,
                "step": 2
            })
        );
        let chunk = serde_json::to_value(&frames[1]).expect("chunk serializes");
        assert_eq!(chunk.get("type"), Some(&serde_json::json!("chunk")));
        assert_eq!(chunk.get("attemptId"), Some(&serde_json::json!("s-1:3")));
        assert_eq!(
            chunk.get("chunk"),
            Some(&serde_json::json!({"type": "text-delta", "index": 0, "text": "He"}))
        );
        let end = serde_json::to_value(&frames[3]).expect("end serializes");
        assert_eq!(
            end,
            serde_json::json!({
                "type": "end",
                "attemptId": "s-1:3",
                "revision": 4,
                "index": 2,
                "outcome": {
                    "kind": "committed",
                    "eventType": "assistant/message",
                    "seq": 42
                }
            })
        );
        let back: AssistantStreamFrame =
            serde_json::from_value(end).expect("end frame round-trips");
        assert_eq!(back, frames[3]);
    }

    /// Covers the settle failure path: a rejected durable append abandons
    /// the attempt (abandoned end frame) and re-raises the error.
    #[test]
    fn settle_failure_abandons_and_reraises() {
        let (frames, next_revision, emit) = rig();
        let mut stream = attempt(emit, next_revision);
        stream.start();
        stream.push(5, text_delta(0, "x")).expect("legal push");
        let result = stream.settle(SettlementType::AssistantAttempt, || Err("log closed"));
        assert_eq!(result, Err("log closed"));
        assert!(stream.ended());
        let frames = frames.borrow();
        let AssistantStreamFrame::End { outcome, .. } = frames.last().expect("end frame") else {
            panic!("the terminal frame is an end frame");
        };
        assert_eq!(*outcome, FrameOutcome::Abandoned);
        let encoded = serde_json::to_value(outcome).expect("outcome serializes");
        assert_eq!(encoded, serde_json::json!({"kind": "abandoned"}));
    }

    /// Covers the produce face: compact stream records, assembled blocks,
    /// the interrupted prefix (tool calls omitted), usage, the defaulting
    /// finish, and the replay envelope.
    #[test]
    fn produce_face_tracks_pushes() {
        let (_frames, next_revision, emit) = rig();
        let mut stream = attempt(emit, next_revision);
        stream
            .push(10, text_delta(0, "answer"))
            .expect("legal push");
        stream
            .push(11, {
                StreamChunk::Known(KnownStreamChunk::ToolCallDelta {
                    index: 1,
                    id: ToolCallId::new("c-1"),
                    name: Some("bash".to_owned()),
                    arguments_delta: "{\"".to_owned(),
                })
            })
            .expect("legal push");
        stream
            .push(12, {
                StreamChunk::Known(KnownStreamChunk::Usage {
                    usage: TokenUsage {
                        input_tokens: 5,
                        ..TokenUsage::default()
                    },
                })
            })
            .expect("legal push");

        // Compact stream: one packed text run, one packed tool run, one raw.
        let records = stream.stream();
        assert_eq!(records.len(), 3);
        assert_eq!(
            records[0],
            AssistantStreamRecord::TextChunks {
                time0: 10,
                index: 0,
                dt: vec![],
                texts: vec!["answer".to_owned()],
            }
        );
        assert!(matches!(
            records[1],
            AssistantStreamRecord::ToolCallChunks { .. }
        ));
        assert!(matches!(records[2], AssistantStreamRecord::Chunk { .. }));

        // Blocks assemble from the same chunks; usage reports; finish
        // defaults to stop before a finish chunk.
        assert_eq!(
            stream.usage(),
            Some(TokenUsage {
                input_tokens: 5,
                ..TokenUsage::default()
            })
        );
        assert_eq!(
            stream.finish(),
            fr_wire::FinishReason::Known(FinishReasonKind::Stop)
        );
        let blocks = stream.blocks().expect("blocks assemble");
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            blocks[0],
            ContentBlock::Known(KnownContentBlock::Text {
                text: "answer".to_owned()
            })
        );

        // The interrupted prefix keeps only visible text/reasoning blocks.
        let interrupted = stream.interrupted_blocks();
        assert_eq!(interrupted.len(), 1);
        assert_eq!(
            interrupted[0],
            ContentBlock::Known(KnownContentBlock::Text {
                text: "answer".to_owned()
            })
        );
        assert!(stream.replay_state().expect("replay aligns").is_none());
    }
}
