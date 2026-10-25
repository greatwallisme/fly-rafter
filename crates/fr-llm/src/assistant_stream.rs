//! Lossless compact representation of one model-stream attempt (dsh
//! `dsh-llm/assistant-stream.ts` verbatim), plus record-level readers that
//! answer common consumer questions without materializing members.
//!
//! Readers trust the static record type; [`expand_assistant_stream`] is the
//! validating path for records read at a durable boundary. Structural
//! exact-key validation (dsh `exactKeys`) is carried by the typed
//! [`AssistantStreamRecord`] enum itself: the durable-boundary serde parse
//! rejects missing, wrongly typed, unknown-key (`deny_unknown_fields`,
//! Q61A), and unknown record shapes, so the
//! obligations validated here are the semantic ones serde cannot express
//! (non-empty members, `dt` length, safe-integer time bounds, non-empty
//! tool-call identity).

use crate::assembler::BlockAssembler;
use fr_wire::ids::SAFE_INTEGER_MAX;
use fr_wire::{
    AssistantStreamRecord, ContentBlock, KnownContentBlock, KnownStreamChunk, StreamChunk,
    ToolCallId,
};

/// One model chunk paired with its original session timestamp.
#[derive(Debug, Clone, PartialEq)]
pub struct TimedStreamChunk {
    /// Epoch milliseconds; bounded to the safe-integer range by
    /// [`AssistantStreamAccumulator::push`].
    pub time: i64,
    /// The chunk exactly as emitted.
    pub chunk: StreamChunk,
}

/// Why a push or a durable-boundary expansion was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamRejection {
    /// The timestamp exceeded the safe-integer range.
    TimeNotSafeInteger(i64),
    /// The block index was beyond the safe-integer cap (`-0` is
    /// unrepresentable at the `u64` type level).
    IndexNotSafeInteger {
        /// The record or chunk type dsh names in the message.
        label: &'static str,
    },
    /// A packed run carried no members.
    RunEmpty {
        /// The record type dsh names in the message.
        label: &'static str,
    },
    /// A run's inter-event gaps held a value beyond the safe-integer range.
    DtNotSafeIntegers {
        /// The record type dsh names in the message.
        label: &'static str,
    },
    /// A run's gap count did not equal its member count minus one.
    DtLengthNotMembersMinusOne {
        /// The record type dsh names in the message.
        label: &'static str,
    },
    /// A run's reconstructed member times left the safe-integer range.
    MemberTimesNotSafeIntegers {
        /// The record type dsh names in the message.
        label: &'static str,
    },
    /// A tool-call run carried an empty provider call id.
    ToolCallIdEmpty,
    /// A tool-call run carried a present-but-empty tool name.
    ToolCallNameEmpty,
}

impl std::fmt::Display for StreamRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimeNotSafeInteger(value) => {
                write!(
                    f,
                    "Assistant stream time must be a safe integer, got {value}"
                )
            }
            Self::IndexNotSafeInteger { label } => {
                write!(f, "{label} index must be a non-negative safe integer")
            }
            Self::RunEmpty { label } => match *label {
                "tool-call-chunks" => write!(f, "tool-call-chunks args must be non-empty"),
                _ => write!(f, "{label} texts must be non-empty"),
            },
            Self::DtNotSafeIntegers { label } => {
                write!(f, "{label} dt must contain safe integers")
            }
            Self::DtLengthNotMembersMinusOne { label } => {
                write!(f, "{label} dt length must be one less than its members")
            }
            Self::MemberTimesNotSafeIntegers { label } => {
                write!(f, "{label} member times must stay safe integers")
            }
            Self::ToolCallIdEmpty => {
                write!(f, "tool-call-chunks id must be a non-empty string")
            }
            Self::ToolCallNameEmpty => {
                write!(f, "tool-call-chunks name must be a non-empty string")
            }
        }
    }
}

impl std::error::Error for StreamRejection {}

const fn safe_time(time: i64) -> Result<i64, StreamRejection> {
    if time.unsigned_abs() > SAFE_INTEGER_MAX {
        return Err(StreamRejection::TimeNotSafeInteger(time));
    }
    Ok(time)
}

const fn safe_index(index: u64, label: &'static str) -> Result<(), StreamRejection> {
    if index > SAFE_INTEGER_MAX {
        return Err(StreamRejection::IndexNotSafeInteger { label });
    }
    Ok(())
}

fn safe_gap(previous: i64, next: i64) -> Option<i64> {
    let gap = next.checked_sub(previous)?;
    // Reversibility: previous + gap must reconstruct next exactly. Exact in
    // i64 arithmetic, so only the safe-integer bound is meaningful here.
    if gap.unsigned_abs() > SAFE_INTEGER_MAX {
        return None;
    }
    (previous.checked_add(gap) == Some(next)).then_some(gap)
}

/// Which packed delta flavor a run accumulates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeltaKind {
    /// Visible text deltas.
    Text,
    /// Reasoning deltas.
    Reasoning,
}

/// The in-progress record list; runs carry `last_time` so the next gap is
/// computable without rescanning.
#[derive(Debug)]
enum MutableRecord {
    DeltaRun {
        kind: DeltaKind,
        time0: i64,
        index: u64,
        dt: Vec<i64>,
        texts: Vec<String>,
        last_time: i64,
    },
    ToolCallRun {
        time0: i64,
        index: u64,
        dt: Vec<i64>,
        id: ToolCallId,
        name: Option<String>,
        args: Vec<String>,
        last_time: i64,
    },
    Raw {
        time: i64,
        chunk: StreamChunk,
    },
}

/// Incrementally compacts one attempt without retaining a second raw-chunk
/// list (dsh `AssistantStreamAccumulator`).
#[derive(Debug, Default)]
pub struct AssistantStreamAccumulator {
    records: Vec<MutableRecord>,
}

impl AssistantStreamAccumulator {
    /// An empty accumulator.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one timed chunk to the compact attempt stream.
    ///
    /// The lossless-JSON snapshot step (dsh `snapshotChunk`) is structural in
    /// Rust: the chunk is an owned serde value, so the returned copy is the
    /// detached snapshot. Unknown chunk kinds (the merge-extensible `Other`
    /// face) are kept as raw `chunk` records: dsh's static exhaustiveness
    /// guard has no runtime path, and a raw record preserves them verbatim.
    ///
    /// # Errors
    ///
    /// [`StreamRejection::TimeNotSafeInteger`] for a timestamp beyond the
    /// safe-integer range, or [`StreamRejection::IndexNotSafeInteger`] for a
    /// block index beyond it.
    pub fn push(&mut self, value: TimedStreamChunk) -> Result<TimedStreamChunk, StreamRejection> {
        let time = safe_time(value.time)?;
        let chunk = value.chunk;
        let timed = TimedStreamChunk {
            time,
            chunk: chunk.clone(),
        };
        match chunk {
            StreamChunk::Known(KnownStreamChunk::TextDelta { index, text }) => {
                safe_index(index, "text-delta")?;
                self.push_delta(DeltaKind::Text, time, index, text);
            }
            StreamChunk::Known(KnownStreamChunk::ReasoningDelta { index, text }) => {
                safe_index(index, "reasoning-delta")?;
                self.push_delta(DeltaKind::Reasoning, time, index, text);
            }
            StreamChunk::Known(KnownStreamChunk::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            }) => {
                safe_index(index, "tool-call-delta")?;
                if id.as_str().is_empty() || name.as_deref() == Some("") {
                    self.records.push(MutableRecord::Raw {
                        time,
                        chunk: StreamChunk::Known(KnownStreamChunk::ToolCallDelta {
                            index,
                            id,
                            name,
                            arguments_delta,
                        }),
                    });
                    return Ok(timed);
                }
                self.push_tool_delta(time, index, id, name, arguments_delta);
            }
            other => self.records.push(MutableRecord::Raw { time, chunk: other }),
        }
        Ok(timed)
    }

    fn push_delta(&mut self, kind: DeltaKind, time: i64, index: u64, text: String) {
        let gap = match self.records.last() {
            Some(MutableRecord::DeltaRun {
                kind: run_kind,
                index: run_index,
                last_time,
                ..
            }) if *run_kind == kind && *run_index == index => safe_gap(*last_time, time),
            _ => None,
        };
        match (self.records.last_mut(), gap) {
            (
                Some(MutableRecord::DeltaRun {
                    dt,
                    texts,
                    last_time,
                    ..
                }),
                Some(gap),
            ) => {
                dt.push(gap);
                texts.push(text);
                *last_time = time;
            }
            _ => self.records.push(MutableRecord::DeltaRun {
                kind,
                time0: time,
                index,
                dt: Vec::new(),
                texts: vec![text],
                last_time: time,
            }),
        }
    }

    fn push_tool_delta(
        &mut self,
        time: i64,
        index: u64,
        id: ToolCallId,
        name: Option<String>,
        arguments_delta: String,
    ) {
        // dsh packs when the presence bit and value of `name` match
        // (`Object.hasOwn` equality plus string equality); `Option<String>`
        // equality carries both.
        let gap = match self.records.last() {
            Some(MutableRecord::ToolCallRun {
                index: run_index,
                id: run_id,
                name: run_name,
                last_time,
                ..
            }) if *run_index == index && *run_id == id && *run_name == name => {
                safe_gap(*last_time, time)
            }
            _ => None,
        };
        match (self.records.last_mut(), gap) {
            (
                Some(MutableRecord::ToolCallRun {
                    dt,
                    args,
                    last_time,
                    ..
                }),
                Some(gap),
            ) => {
                dt.push(gap);
                args.push(arguments_delta);
                *last_time = time;
            }
            _ => self.records.push(MutableRecord::ToolCallRun {
                time0: time,
                index,
                dt: Vec::new(),
                id,
                name,
                args: vec![arguments_delta],
                last_time: time,
            }),
        }
    }

    /// Return the current compact attempt stream: a detached durable copy.
    #[must_use]
    pub fn snapshot(&self) -> Vec<AssistantStreamRecord> {
        self.records
            .iter()
            .map(|record| match record {
                MutableRecord::DeltaRun {
                    kind,
                    time0,
                    index,
                    dt,
                    texts,
                    ..
                } => match kind {
                    DeltaKind::Text => AssistantStreamRecord::TextChunks {
                        time0: *time0,
                        index: *index,
                        dt: dt.clone(),
                        texts: texts.clone(),
                    },
                    DeltaKind::Reasoning => AssistantStreamRecord::ReasoningChunks {
                        time0: *time0,
                        index: *index,
                        dt: dt.clone(),
                        texts: texts.clone(),
                    },
                },
                MutableRecord::ToolCallRun {
                    time0,
                    index,
                    dt,
                    id,
                    name,
                    args,
                    ..
                } => AssistantStreamRecord::ToolCallChunks {
                    time0: *time0,
                    index: *index,
                    dt: dt.clone(),
                    id: id.clone(),
                    name: name.clone(),
                    args: args.clone(),
                },
                MutableRecord::Raw { time, chunk } => AssistantStreamRecord::Chunk {
                    time: *time,
                    chunk: chunk.clone(),
                },
            })
            .collect()
    }
}

/// Expand compact records into the exact timed chunk sequence (dsh
/// `expandAssistantStream`): every original delta boundary preserved.
///
/// The record list is trusted structurally (the typed enum), and validated
/// semantically here.
///
/// # Errors
///
/// [`StreamRejection`] with the dsh obligation text when a record carries an
/// out-of-range time or index, empty members, empty tool-call identity, a
/// `dt` gap beyond the safe-integer range, a `dt` length that is not
/// `members - 1`, or member times that leave the safe-integer range.
pub fn expand_assistant_stream(
    stream: &[AssistantStreamRecord],
) -> Result<Vec<TimedStreamChunk>, StreamRejection> {
    let mut chunks = Vec::new();
    for record in stream {
        match record {
            AssistantStreamRecord::Chunk { time, chunk } => {
                chunks.push(TimedStreamChunk {
                    time: safe_time(*time)?,
                    chunk: chunk.clone(),
                });
            }
            AssistantStreamRecord::TextChunks {
                time0,
                index,
                dt,
                texts,
            }
            | AssistantStreamRecord::ReasoningChunks {
                time0,
                index,
                dt,
                texts,
            } => {
                let label = match record {
                    AssistantStreamRecord::TextChunks { .. } => "text-chunks",
                    _ => "reasoning-chunks",
                };
                if texts.is_empty() {
                    return Err(StreamRejection::RunEmpty { label });
                }
                validate_run(*time0, *index, dt, texts.len(), label)?;
                let mut time = *time0;
                for (member, text) in texts.iter().enumerate() {
                    if member > 0 {
                        time += dt[member - 1];
                    }
                    let chunk = if label == "text-chunks" {
                        KnownStreamChunk::TextDelta {
                            index: *index,
                            text: text.clone(),
                        }
                    } else {
                        KnownStreamChunk::ReasoningDelta {
                            index: *index,
                            text: text.clone(),
                        }
                    };
                    chunks.push(TimedStreamChunk {
                        time,
                        chunk: StreamChunk::Known(chunk),
                    });
                }
            }

            AssistantStreamRecord::ToolCallChunks {
                time0,
                index,
                dt,
                id,
                name,
                args,
            } => {
                if args.is_empty() {
                    return Err(StreamRejection::RunEmpty {
                        label: "tool-call-chunks",
                    });
                }
                if id.as_str().is_empty() {
                    return Err(StreamRejection::ToolCallIdEmpty);
                }
                if name.as_deref() == Some("") {
                    return Err(StreamRejection::ToolCallNameEmpty);
                }
                validate_run(*time0, *index, dt, args.len(), "tool-call-chunks")?;
                let mut time = *time0;
                for (member, fragment) in args.iter().enumerate() {
                    if member > 0 {
                        time += dt[member - 1];
                    }
                    chunks.push(TimedStreamChunk {
                        time,
                        chunk: StreamChunk::Known(KnownStreamChunk::ToolCallDelta {
                            index: *index,
                            id: id.clone(),
                            name: name.clone(),
                            arguments_delta: fragment.clone(),
                        }),
                    });
                }
            }
        }
    }
    Ok(chunks)
}

fn validate_run(
    time0: i64,
    index: u64,
    dt: &[i64],
    members: usize,
    label: &'static str,
) -> Result<(), StreamRejection> {
    safe_time(time0)?;
    safe_index(index, label)?;
    if dt.iter().any(|gap| gap.unsigned_abs() > SAFE_INTEGER_MAX) {
        return Err(StreamRejection::DtNotSafeIntegers { label });
    }
    if dt.len() != members.saturating_sub(1) {
        return Err(StreamRejection::DtLengthNotMembersMinusOne { label });
    }
    let mut time = time0;
    for gap in dt {
        time += gap;
        if time.unsigned_abs() > SAFE_INTEGER_MAX {
            return Err(StreamRejection::MemberTimesNotSafeIntegers { label });
        }
    }
    Ok(())
}

/// `/\S/`: any character outside the JavaScript `\s` set. `\u{FEFF}` is in
/// the JS set but not Unicode `White_Space`, so it is spelled out.
fn has_non_whitespace(text: &str) -> bool {
    !text.chars().all(|c| c == '\u{FEFF}' || c.is_whitespace())
}

fn block_is_visible(block: &ContentBlock) -> bool {
    match block {
        ContentBlock::Known(KnownContentBlock::ToolCall { .. }) => false,
        ContentBlock::Known(
            KnownContentBlock::Text { text } | KnownContentBlock::Reasoning { text },
        ) => has_non_whitespace(text),
        ContentBlock::Known(_) | ContentBlock::Other(_) => true,
    }
}

/// Whether one chunk carries the model's first output token for latency
/// measurement (dsh `isTokenDelta`).
#[must_use]
pub const fn is_token_delta(chunk: &StreamChunk) -> bool {
    match chunk {
        StreamChunk::Known(
            KnownStreamChunk::TextDelta { text, .. }
            | KnownStreamChunk::ReasoningDelta { text, .. },
        ) => !text.is_empty(),
        StreamChunk::Known(KnownStreamChunk::ToolCallDelta {
            name,
            arguments_delta,
            ..
        }) => !arguments_delta.is_empty() || name.is_some(),
        _ => false,
    }
}

/// Whether one chunk by itself contributes reader-visible transcript
/// content (dsh `isVisibleChunk`).
///
/// Text and reasoning count only with non-whitespace content; a block of
/// any other kind counts at start and end except a tool call, which is
/// protocol rather than content; usage and finish never count.
#[must_use]
pub fn is_visible_chunk(chunk: &StreamChunk) -> bool {
    match chunk {
        StreamChunk::Known(
            KnownStreamChunk::TextDelta { text, .. }
            | KnownStreamChunk::ReasoningDelta { text, .. },
        ) => has_non_whitespace(text),
        StreamChunk::Known(KnownStreamChunk::BlockStart { block_type, .. }) => {
            block_type != "text" && block_type != "reasoning" && block_type != "tool-call"
        }
        StreamChunk::Known(KnownStreamChunk::BlockEnd { block, .. }) => block_is_visible(block),
        _ => false,
    }
}

/// Whether one chunk carries non-whitespace text, as a text delta or a
/// completed text block (dsh `chunkHasVisibleText`); reasoning, tool calls,
/// and other block kinds never count.
#[must_use]
pub fn chunk_has_visible_text(chunk: &StreamChunk) -> bool {
    match chunk {
        StreamChunk::Known(KnownStreamChunk::TextDelta { text, .. }) => has_non_whitespace(text),
        StreamChunk::Known(KnownStreamChunk::BlockEnd { block, .. }) => {
            let ContentBlock::Known(KnownContentBlock::Text { text }) = block else {
                return false;
            };
            has_non_whitespace(text)
        }
        _ => false,
    }
}

/// The run's `(dt, fragments, time0)`, or `None` for a raw `chunk` record.
type RunMembers<'a> = Option<(&'a [i64], &'a [String], i64)>;

fn run_members(record: &AssistantStreamRecord) -> RunMembers<'_> {
    match record {
        AssistantStreamRecord::TextChunks {
            dt, texts, time0, ..
        }
        | AssistantStreamRecord::ReasoningChunks {
            dt, texts, time0, ..
        } => Some((dt, texts, *time0)),
        AssistantStreamRecord::ToolCallChunks {
            dt, args, time0, ..
        } => Some((dt, args, *time0)),
        AssistantStreamRecord::Chunk { .. } => None,
    }
}

fn first_run_member_time(
    record: &AssistantStreamRecord,
    predicate: impl Fn(&str) -> bool,
) -> Option<i64> {
    let (dt, fragments, time0) = run_members(record)?;
    // Trusted-path reads answer only for structurally sound runs: a run with
    // fewer gaps than members has no trustworthy member times (dsh propagates
    // NaN; i64 has none, so the reader declines instead).
    if dt.len() != fragments.len().saturating_sub(1) {
        return None;
    }
    let mut time = time0;
    for (member, fragment) in fragments.iter().enumerate() {
        if member > 0 {
            time += dt[member - 1];
        }
        if predicate(fragment) {
            return Some(time);
        }
    }
    None
}

/// Time of the first member of one packed run that [`is_token_delta`]
/// accepts (dsh `runFirstTokenTime`): a name-bearing tool-call run starts at
/// its first member, otherwise the first non-empty fragment.
#[must_use]
pub fn run_first_token_time(record: &AssistantStreamRecord) -> Option<i64> {
    if let AssistantStreamRecord::ToolCallChunks {
        name: Some(_),
        time0,
        ..
    } = record
    {
        return Some(*time0);
    }
    first_run_member_time(record, |fragment| !fragment.is_empty())
}

/// Time of the first member of one packed run that [`is_visible_chunk`]
/// accepts (dsh `runFirstVisibleTime`): the first non-whitespace text or
/// reasoning fragment; a tool-call run has none.
#[must_use]
pub fn run_first_visible_time(record: &AssistantStreamRecord) -> Option<i64> {
    if matches!(record, AssistantStreamRecord::ToolCallChunks { .. }) {
        return None;
    }
    first_run_member_time(record, has_non_whitespace)
}

/// Time of the first token in one compact stream per [`is_token_delta`]
/// (dsh `assistantStreamFirstTokenTime`), read from the records and stopping
/// at the first qualifying member.
#[must_use]
pub fn assistant_stream_first_token_time(stream: &[AssistantStreamRecord]) -> Option<i64> {
    for record in stream {
        let time = match record {
            AssistantStreamRecord::Chunk { time, chunk } => is_token_delta(chunk).then_some(*time),
            _ => run_first_token_time(record),
        };
        if time.is_some() {
            return time;
        }
    }
    None
}

/// Whether one compact stream carries reader-visible content per
/// [`is_visible_chunk`] (dsh `assistantStreamHasVisibleContent`).
#[must_use]
pub fn assistant_stream_has_visible_content(stream: &[AssistantStreamRecord]) -> bool {
    stream.iter().any(|record| match record {
        AssistantStreamRecord::Chunk { chunk, .. } => is_visible_chunk(chunk),
        _ => run_first_visible_time(record).is_some(),
    })
}

/// Whether one compact stream carries non-whitespace text per
/// [`chunk_has_visible_text`] (dsh `assistantStreamHasVisibleText`).
#[must_use]
pub fn assistant_stream_has_visible_text(stream: &[AssistantStreamRecord]) -> bool {
    stream.iter().any(|record| match record {
        AssistantStreamRecord::TextChunks { texts, .. } => {
            texts.iter().any(|text| has_non_whitespace(text))
        }
        AssistantStreamRecord::Chunk { chunk, .. } => chunk_has_visible_text(chunk),
        _ => false,
    })
}

/// The raw chunk kinds the accumulator never packs (dsh
/// `RawStreamChunkType`: every type except the three delta types).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawChunkKind {
    /// `block-start`.
    BlockStart,
    /// `block-end`.
    BlockEnd,
    /// `usage`.
    Usage,
    /// `finish`.
    Finish,
}

const fn chunk_matches_kind(chunk: &KnownStreamChunk, kind: RawChunkKind) -> bool {
    matches!(
        (chunk, kind),
        (
            KnownStreamChunk::BlockStart { .. },
            RawChunkKind::BlockStart
        ) | (KnownStreamChunk::BlockEnd { .. }, RawChunkKind::BlockEnd)
            | (KnownStreamChunk::Usage { .. }, RawChunkKind::Usage)
            | (KnownStreamChunk::Finish { .. }, RawChunkKind::Finish)
    )
}

/// The last raw chunk of one never-packed type, scanning backwards (dsh
/// `lastAssistantStreamChunk`).
#[must_use]
pub fn last_assistant_stream_chunk(
    stream: &[AssistantStreamRecord],
    kind: RawChunkKind,
) -> Option<&KnownStreamChunk> {
    stream.iter().rev().find_map(|record| match record {
        AssistantStreamRecord::Chunk {
            chunk: StreamChunk::Known(known),
            ..
        } if chunk_matches_kind(known, kind) => Some(known),
        _ => None,
    })
}

/// Every raw chunk of one never-packed type, in stream order (dsh
/// `assistantStreamChunks`).
#[must_use]
pub fn assistant_stream_chunks(
    stream: &[AssistantStreamRecord],
    kind: RawChunkKind,
) -> Vec<&KnownStreamChunk> {
    stream
        .iter()
        .filter_map(|record| match record {
            AssistantStreamRecord::Chunk {
                chunk: StreamChunk::Known(known),
                ..
            } if chunk_matches_kind(known, kind) => Some(known),
            _ => None,
        })
        .collect()
}

/// Every streamed text-delta fragment joined in stream order (dsh
/// `joinAssistantStreamText`); reasoning and tool-call fragments excluded.
#[must_use]
pub fn join_assistant_stream_text(stream: &[AssistantStreamRecord]) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for record in stream {
        match record {
            AssistantStreamRecord::TextChunks { texts, .. } => {
                parts.extend(texts.iter().map(String::as_str));
            }
            AssistantStreamRecord::Chunk {
                chunk: StreamChunk::Known(KnownStreamChunk::TextDelta { text, .. }),
                ..
            } => parts.push(text),
            _ => {}
        }
    }
    parts.concat()
}

/// Feed one compact stream into a [`BlockAssembler`] without materializing
/// members (dsh `assembleAssistantStream`).
///
/// Each run contributes one delta carrying its joined fragments, which
/// assembles the same blocks as the original per-member deltas because
/// assembly only concatenates them. The records are trusted, not validated;
/// validate a stream read at a durable boundary with
/// [`expand_assistant_stream`] first.
pub fn assemble_assistant_stream(stream: &[AssistantStreamRecord], assembler: &mut BlockAssembler) {
    for record in stream {
        match record {
            AssistantStreamRecord::Chunk { chunk, .. } => assembler.push(chunk),
            AssistantStreamRecord::TextChunks { index, texts, .. } => {
                assembler.push(&StreamChunk::Known(KnownStreamChunk::TextDelta {
                    index: *index,
                    text: texts.concat(),
                }));
            }
            AssistantStreamRecord::ReasoningChunks { index, texts, .. } => {
                assembler.push(&StreamChunk::Known(KnownStreamChunk::ReasoningDelta {
                    index: *index,
                    text: texts.concat(),
                }));
            }
            AssistantStreamRecord::ToolCallChunks {
                index,
                id,
                name,
                args,
                ..
            } => assembler.push(&StreamChunk::Known(KnownStreamChunk::ToolCallDelta {
                index: *index,
                id: id.clone(),
                name: name.clone(),
                arguments_delta: args.concat(),
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_delta(index: u64, text: &str) -> StreamChunk {
        StreamChunk::Known(KnownStreamChunk::TextDelta {
            index,
            text: text.to_owned(),
        })
    }

    fn reasoning_delta(index: u64, text: &str) -> StreamChunk {
        StreamChunk::Known(KnownStreamChunk::ReasoningDelta {
            index,
            text: text.to_owned(),
        })
    }

    fn tool_delta(index: u64, id: &str, name: Option<&str>, fragment: &str) -> StreamChunk {
        StreamChunk::Known(KnownStreamChunk::ToolCallDelta {
            index,
            id: ToolCallId::new(id),
            name: name.map(str::to_owned),
            arguments_delta: fragment.to_owned(),
        })
    }

    fn push_all(accumulator: &mut AssistantStreamAccumulator, timed: &[(i64, StreamChunk)]) {
        for (time, chunk) in timed {
            accumulator
                .push(TimedStreamChunk {
                    time: *time,
                    chunk: chunk.clone(),
                })
                .expect("legal push");
        }
    }

    /// The bound scenario: one successful step's chunk set packs into the
    /// four record classes with `dt` gaps, incomplete tool-call identity
    /// falls to raw, and the never-packed types stay raw.
    #[test]
    fn assistant_stream_packs_runs_and_raw_chunks() {
        let mut accumulator = AssistantStreamAccumulator::new();
        let block = ContentBlock::Known(KnownContentBlock::Text {
            text: "Hello".to_owned(),
        });
        let timed = vec![
            (
                1_000i64,
                StreamChunk::Known(KnownStreamChunk::BlockStart {
                    index: 0,
                    block_type: "text".to_owned(),
                }),
            ),
            (1_010, text_delta(0, "He")),
            (1_023, text_delta(0, "llo")),
            (
                1_100,
                StreamChunk::Known(KnownStreamChunk::BlockEnd { index: 0, block }),
            ),
            (
                1_105,
                StreamChunk::Known(KnownStreamChunk::Usage {
                    usage: fr_wire::call_config::TokenUsage::default(),
                }),
            ),
            (
                1_110,
                StreamChunk::Known(KnownStreamChunk::Finish {
                    reason: fr_wire::FinishReason::Known(
                        fr_wire::call_config::FinishReasonKind::Stop,
                    ),
                    replay_state: None,
                }),
            ),
            (1_200, reasoning_delta(1, "a")),
            (1_250, reasoning_delta(1, "b")),
            (1_300, tool_delta(2, "c-1", Some("bash"), "{")),
            (1_315, tool_delta(2, "c-1", Some("bash"), "}")),
            // An incomplete identity never packs.
            (1_400, tool_delta(3, "", None, "?")),
            // A name-presence change starts a new run.
            (1_500, tool_delta(2, "c-1", None, "!")),
        ];
        push_all(&mut accumulator, &timed);
        let records = accumulator.snapshot();
        assert_eq!(records.len(), 9);
        assert!(matches!(&records[0], AssistantStreamRecord::Chunk { .. }));
        assert_eq!(
            records[1],
            AssistantStreamRecord::TextChunks {
                time0: 1_010,
                index: 0,
                dt: vec![13],
                texts: vec!["He".to_owned(), "llo".to_owned()],
            }
        );
        assert!(matches!(&records[2], AssistantStreamRecord::Chunk { .. }));
        assert!(matches!(&records[3], AssistantStreamRecord::Chunk { .. }));
        assert!(matches!(&records[4], AssistantStreamRecord::Chunk { .. }));
        assert_eq!(
            records[5],
            AssistantStreamRecord::ReasoningChunks {
                time0: 1_200,
                index: 1,
                dt: vec![50],
                texts: vec!["a".to_owned(), "b".to_owned()],
            }
        );
        assert_eq!(
            records[6],
            AssistantStreamRecord::ToolCallChunks {
                time0: 1_300,
                index: 2,
                dt: vec![15],
                id: ToolCallId::new("c-1"),
                name: Some("bash".to_owned()),
                args: vec!["{".to_owned(), "}".to_owned()],
            }
        );
        assert!(matches!(&records[7], AssistantStreamRecord::Chunk { .. }));
        assert_eq!(
            records[8],
            AssistantStreamRecord::ToolCallChunks {
                time0: 1_500,
                index: 2,
                dt: vec![],
                id: ToolCallId::new("c-1"),
                name: None,
                args: vec!["!".to_owned()],
            }
        );
        // Expansion reconstructs the exact timed chunk sequence.
        let expanded = expand_assistant_stream(&records).expect("valid records");
        let member_times: Vec<i64> = expanded.iter().map(|entry| entry.time).collect();
        assert_eq!(
            member_times,
            vec![
                1_000, 1_010, 1_023, 1_100, 1_105, 1_110, 1_200, 1_250, 1_300, 1_315, 1_400, 1_500
            ]
        );
        assert_eq!(expanded[1].chunk, text_delta(0, "He"));
        assert_eq!(expanded[8].chunk, tool_delta(2, "c-1", Some("bash"), "{"));
    }

    /// The validating path rejects every semantic obligation with the dsh
    /// message text.
    #[test]
    fn expand_validates_durable_records() {
        let run = AssistantStreamRecord::TextChunks {
            time0: 10,
            index: 0,
            dt: vec![5],
            texts: vec!["a".to_owned(), "b".to_owned()],
        };
        // Empty members.
        let empty = AssistantStreamRecord::ToolCallChunks {
            time0: 10,
            index: 0,
            dt: vec![],
            id: ToolCallId::new("c-1"),
            name: Some("bash".to_owned()),
            args: vec![],
        };
        assert_eq!(
            expand_assistant_stream(&[empty]).expect_err("empty args"),
            StreamRejection::RunEmpty {
                label: "tool-call-chunks"
            }
        );
        // dt length must be members - 1.
        let bad_length = AssistantStreamRecord::TextChunks {
            time0: 10,
            index: 0,
            dt: vec![],
            texts: vec!["a".to_owned(), "b".to_owned()],
        };
        assert_eq!(
            expand_assistant_stream(&[bad_length]).expect_err("dt length"),
            StreamRejection::DtLengthNotMembersMinusOne {
                label: "text-chunks"
            }
        );
        // Member times must stay safe integers.
        let overflow = AssistantStreamRecord::TextChunks {
            time0: 4_500_000_000_000_000,
            index: 0,
            dt: vec![4_600_000_000_000_000],
            texts: vec!["a".to_owned(), "b".to_owned()],
        };
        assert_eq!(
            expand_assistant_stream(&[overflow]).expect_err("overflow"),
            StreamRejection::MemberTimesNotSafeIntegers {
                label: "text-chunks"
            }
        );
        // Empty tool-call identity.
        let empty_id = AssistantStreamRecord::ToolCallChunks {
            time0: 10,
            index: 0,
            dt: vec![],
            id: ToolCallId::new(""),
            name: None,
            args: vec!["{".to_owned()],
        };
        assert_eq!(
            expand_assistant_stream(&[empty_id]).expect_err("empty id"),
            StreamRejection::ToolCallIdEmpty
        );
        // Out-of-range time on a raw chunk.
        let raw = AssistantStreamRecord::Chunk {
            time: 9_007_199_254_740_992,
            chunk: text_delta(0, "x"),
        };
        assert_eq!(
            expand_assistant_stream(&[raw]).expect_err("time"),
            StreamRejection::TimeNotSafeInteger(9_007_199_254_740_992)
        );
        // The well-formed run passes and expands with exact member times.
        let expanded = expand_assistant_stream(&[run]).expect("valid run");
        assert_eq!(expanded[0].time, 10);
        assert_eq!(expanded[1].time, 15);
    }

    /// Judgment-family semantics pinned against dsh behavior.
    #[test]
    fn judgment_family_matches_dsh_readers() {
        assert!(is_token_delta(&text_delta(0, "x")));
        assert!(!is_token_delta(&text_delta(0, "")));
        assert!(is_token_delta(&tool_delta(0, "c-1", Some("bash"), "")));
        assert!(!is_token_delta(&tool_delta(0, "c-1", None, "")));
        // `/\S/`: whitespace-only text is not visible; `\u{FEFF}` counts as
        // whitespace in the JS `\s` set.
        assert!(!is_visible_chunk(&text_delta(0, "  \n")));
        assert!(is_visible_chunk(&text_delta(0, "\u{FEFF}x")));
        assert!(is_visible_chunk(&StreamChunk::Known(
            KnownStreamChunk::BlockStart {
                index: 4,
                block_type: "plugin/widget".to_owned(),
            }
        )));
        assert!(!is_visible_chunk(&StreamChunk::Known(
            KnownStreamChunk::BlockStart {
                index: 4,
                block_type: "tool-call".to_owned(),
            }
        )));
        assert!(chunk_has_visible_text(&text_delta(0, "hi")));
        assert!(!chunk_has_visible_text(&reasoning_delta(0, "hi")));
        let record = AssistantStreamRecord::ToolCallChunks {
            time0: 100,
            index: 2,
            dt: vec![10],
            id: ToolCallId::new("c-1"),
            name: Some("bash".to_owned()),
            args: vec![String::new(), "}".to_owned()],
        };
        // A name-bearing tool-call run starts at its first member.
        assert_eq!(run_first_token_time(&record), Some(100));
        let text_run = AssistantStreamRecord::TextChunks {
            time0: 100,
            index: 0,
            dt: vec![10],
            texts: vec![" ".to_owned(), "hi".to_owned()],
        };
        // The first non-whitespace fragment carries the first visible time.
        assert_eq!(run_first_visible_time(&text_run), Some(110));
        // A whitespace fragment is still a token for latency purposes.
        assert_eq!(run_first_token_time(&text_run), Some(100));
        let stream = vec![
            AssistantStreamRecord::TextChunks {
                time0: 5,
                index: 0,
                dt: vec![5],
                texts: vec!["a".to_owned(), "b".to_owned()],
            },
            AssistantStreamRecord::Chunk {
                time: 99,
                chunk: text_delta(1, "z"),
            },
        ];
        assert_eq!(assistant_stream_first_token_time(&stream), Some(5));
        assert!(assistant_stream_has_visible_content(&stream));
        assert!(assistant_stream_has_visible_text(&stream));
        assert_eq!(join_assistant_stream_text(&stream), "abz");
        // A finish has no raw record in this stream.
        assert_eq!(
            last_assistant_stream_chunk(&stream, RawChunkKind::Finish),
            None
        );
        // Packing into the assembler yields the same blocks as raw deltas:
        // one text run plus one raw delta make two blocks.
        let mut assembler = BlockAssembler::new();
        assemble_assistant_stream(&stream, &mut assembler);
        assert_eq!(assembler.blocks().expect("assembled").len(), 2);
    }
}
