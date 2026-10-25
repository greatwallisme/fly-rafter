//! Incremental chunk-to-block assembly settlement (dsh `dsh-llm/assembler.ts`
//! verbatim).
//!
//! This is the single canonical algorithm the agent loop uses to build
//! assistant blocks from a chunk stream while logging the raw chunks for
//! replay fidelity.

use fr_wire::call_config::{FinishReasonKind, TokenUsage};
use fr_wire::{
    ContentBlock, FinishReason, KnownContentBlock, KnownStreamChunk, ReplayEnvelope, StreamChunk,
    ToolCallId,
};

/// Why a stream could not be assembled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssemblyError {
    /// An unknown block type was never closed by a `block-end`, so no
    /// provider-neutral representation exists.
    IncompleteUnknownBlock(String),
}

impl std::fmt::Display for AssemblyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IncompleteUnknownBlock(block_type) => {
                write!(
                    f,
                    "cannot assemble incomplete block of type \"{block_type}\""
                )
            }
        }
    }
}

impl std::error::Error for AssemblyError {}

/// One in-progress block keyed by its stream index.
#[derive(Debug)]
struct PartialBlock {
    block_type: String,
    text: String,
    tool_call_id: Option<ToolCallId>,
    tool_call_name: Option<String>,
    tool_call_arguments: String,
    /// Set by `block-end` — authoritative, and freezes the partial.
    block: Option<ContentBlock>,
}

impl PartialBlock {
    fn new(block_type: impl Into<String>) -> Self {
        Self {
            block_type: block_type.into(),
            text: String::new(),
            tool_call_id: None,
            tool_call_name: None,
            tool_call_arguments: String::new(),
            block: None,
        }
    }
}

/// Incrementally assembles raw [`StreamChunk`]s into complete
/// [`ContentBlock`]s.
///
/// Tolerant of delta-only protocols (no block-start/end): a delta for an
/// unseen index creates its partial on the spot. Deltas arriving for an
/// index already closed by `block-end` are ignored (malformed stream), so a
/// misbehaving adapter cannot grow memory or corrupt a completed block.
#[derive(Debug, Default)]
pub struct BlockAssembler {
    partials: std::collections::BTreeMap<u64, PartialBlock>,
    order: Vec<u64>,
    usage: Option<TokenUsage>,
    finish: Option<FinishReason>,
    replay_state: Option<ReplayEnvelope>,
}

impl BlockAssembler {
    /// An empty assembler.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one chunk into the assembly state, in stream order. Unknown
    /// chunk kinds (the merge-extensible `Other` face) are ignored.
    pub fn push(&mut self, chunk: &StreamChunk) {
        let StreamChunk::Known(chunk) = chunk else {
            return;
        };
        match chunk {
            KnownStreamChunk::BlockStart { index, block_type } => {
                if !self.partials.contains_key(index) {
                    self.order.push(*index);
                    self.partials
                        .insert(*index, PartialBlock::new(block_type.clone()));
                }
            }
            KnownStreamChunk::TextDelta { index, text } => {
                let partial = self.ensure(*index, "text");
                if partial.block.is_some() {
                    return; // closed by block-end; ignore stragglers
                }
                partial.text.push_str(text);
            }
            KnownStreamChunk::ReasoningDelta { index, text } => {
                let partial = self.ensure(*index, "reasoning");
                if partial.block.is_some() {
                    return; // closed by block-end; ignore stragglers
                }
                partial.text.push_str(text);
            }
            KnownStreamChunk::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            } => {
                let partial = self.ensure(*index, "tool-call");
                if partial.block.is_some() {
                    return; // closed by block-end; ignore stragglers
                }
                let slot = &mut partial.tool_call_id;
                slot.clone_from(&Some(id.clone()));
                // A truthy name overwrites (dsh `if (chunk.name)`): an empty
                // string does not.
                if name.as_ref().is_some_and(|candidate| !candidate.is_empty()) {
                    partial.tool_call_name.clone_from(name);
                }
                partial.tool_call_arguments.push_str(arguments_delta);
            }
            KnownStreamChunk::BlockEnd { index, block } => {
                let partial = self.ensure(*index, block_type_of(block));
                // First close wins; ignoring re-close stragglers keeps
                // streamed output and the final assembled block in agreement.
                if partial.block.is_some() {
                    return;
                }
                partial.block = Some(block.clone());
            }
            KnownStreamChunk::Usage { usage } => {
                self.usage = Some(*usage);
            }
            KnownStreamChunk::Finish {
                reason,
                replay_state,
            } => {
                self.finish = Some(reason.clone());
                self.replay_state.clone_from(replay_state);
            }
        }
    }

    /// Delta-only protocol tolerance: a delta for an unseen index creates
    /// its partial on the spot.
    fn ensure(&mut self, index: u64, block_type: &str) -> &mut PartialBlock {
        self.partials.entry(index).or_insert_with(|| {
            self.order.push(index);
            PartialBlock::new(block_type)
        })
    }

    /// Assemble one partial; an authoritative `block-end` block wins, an
    /// open text/reasoning/tool-call block assembles from its deltas, and an
    /// unknown open block type has no representation.
    fn assemble(&self, index: u64) -> Result<ContentBlock, AssemblyError> {
        let partial = self
            .partials
            .get(&index)
            .ok_or_else(|| AssemblyError::IncompleteUnknownBlock(format!("{index}")))?;
        if let Some(block) = &partial.block {
            return Ok(block.clone());
        }
        match partial.block_type.as_str() {
            "text" => Ok(ContentBlock::Known(KnownContentBlock::Text {
                text: partial.text.clone(),
            })),
            "reasoning" => Ok(ContentBlock::Known(KnownContentBlock::Reasoning {
                text: partial.text.clone(),
            })),
            "tool-call" => Ok(ContentBlock::Known(KnownContentBlock::ToolCall {
                id: partial
                    .tool_call_id
                    .clone()
                    .unwrap_or_else(|| ToolCallId::new(format!("call-{index}"))),
                name: partial.tool_call_name.clone().unwrap_or_default(),
                arguments: partial.tool_call_arguments.clone(),
            })),
            other => Err(AssemblyError::IncompleteUnknownBlock(other.to_owned())),
        }
    }

    /// The one shared keep/drop decision over all seen blocks: max-token
    /// truncation drops tool calls that cannot be executed safely. Emitted
    /// blocks and replay metadata both derive from this result, so they
    /// cannot disagree.
    fn assembled(&self) -> Result<(Vec<ContentBlock>, Option<ReplayEnvelope>), AssemblyError> {
        let all: Vec<ContentBlock> = self
            .order
            .iter()
            .map(|index| self.assemble(*index))
            .collect::<Result<_, _>>()?;
        let kept: Option<Vec<bool>> = (self.finish()
            == FinishReason::Known(FinishReasonKind::MaxTokens))
        .then(|| all.iter().map(|block| !is_tool_call(block)).collect());
        let blocks: Vec<ContentBlock> = kept.as_ref().map_or_else(
            || all.clone(),
            |kept| {
                all.iter()
                    .zip(kept)
                    .filter(|(_, keep)| **keep)
                    .map(|(block, _)| block.clone())
                    .collect()
            },
        );
        let envelope = &self.replay_state;
        let Some(envelope) = envelope else {
            return Ok((blocks, None));
        };
        let Some(envelope_blocks) = &envelope.blocks else {
            return Ok((blocks, Some(envelope.clone())));
        };
        if envelope_blocks.len() != all.len() {
            // A misaligned envelope cannot be pruned safely: drop it whole.
            return Ok((blocks, None));
        }
        let replay = match kept {
            None => Some(envelope.clone()),
            Some(_) if blocks.len() == all.len() => Some(envelope.clone()),
            Some(kept) => Some(ReplayEnvelope {
                response: envelope.response.clone(),
                blocks: Some(
                    envelope_blocks
                        .iter()
                        .zip(kept)
                        .filter(|(_, keep)| *keep)
                        .map(|(entry, _)| entry.clone())
                        .collect(),
                ),
            }),
        };
        Ok((blocks, replay))
    }

    /// Assemble all blocks seen so far, in stream order: one block per seen
    /// index, except that max-token truncation drops tool calls that cannot
    /// be executed safely; an open block assembles from its accumulated
    /// deltas.
    ///
    /// # Errors
    /// [`AssemblyError::IncompleteUnknownBlock`] when an unknown block type
    /// was never closed by a `block-end`.
    pub fn blocks(&self) -> Result<Vec<ContentBlock>, AssemblyError> {
        self.assembled().map(|(blocks, _)| blocks)
    }

    /// Assemble the prefix an interrupted stream can safely finalize: closed
    /// and open text/reasoning blocks with non-whitespace content, in stream
    /// order. Tool calls are omitted because interruption precedes dispatch;
    /// retaining one would require a fabricated result. Open unknown blocks
    /// are also omitted.
    #[must_use]
    pub fn interrupted_blocks(&self) -> Vec<ContentBlock> {
        self.order
            .iter()
            .filter_map(|index| {
                let partial = self.partials.get(index)?;
                let block_type = partial.block.as_ref().map_or(
                    partial.block_type.as_str(),
                    block_type_of,
                );
                if block_type != "text" && block_type != "reasoning" {
                    return None;
                }
                self.assemble(*index).ok()
            })
            .filter(|block| {
                matches!(block, ContentBlock::Known(KnownContentBlock::Text { text } | KnownContentBlock::Reasoning { text })
                    if !text.trim().is_empty())
            })
            .collect()
    }

    /// Usage from the `usage` chunk; `None` until one arrives.
    #[must_use]
    pub const fn usage(&self) -> Option<TokenUsage> {
        self.usage
    }

    /// Finish reason from the `finish` chunk; a natural stop when the stream
    /// ended without one (dsh default `{kind: 'stop'}`).
    #[must_use]
    pub fn finish(&self) -> FinishReason {
        self.finish
            .clone()
            .unwrap_or(FinishReason::Known(FinishReasonKind::Stop))
    }

    /// Replay metadata from the terminal finish chunk, with per-block
    /// entries pruned in step with [`BlockAssembler::blocks`]. `None` when
    /// the envelope's entries do not align with the emitted blocks.
    ///
    /// # Errors
    /// [`AssemblyError::IncompleteUnknownBlock`] under the same rules as
    /// [`BlockAssembler::blocks`].
    pub fn replay_state(&self) -> Result<Option<ReplayEnvelope>, AssemblyError> {
        self.assembled().map(|(_, replay)| replay)
    }
}

/// The wire type tag of one block (dsh reads `block.type`).
fn block_type_of(block: &ContentBlock) -> &str {
    match block {
        ContentBlock::Known(known) => match known {
            KnownContentBlock::Text { .. } => "text",
            KnownContentBlock::Reasoning { .. } => "reasoning",
            KnownContentBlock::Image { .. } => "image",
            KnownContentBlock::File { .. } => "file",
            KnownContentBlock::ToolCall { .. } => "tool-call",
            KnownContentBlock::ToolAddition { .. } => "tool-addition",
            KnownContentBlock::ToolRemoval { .. } => "tool-removal",
        },
        ContentBlock::Other(raw) => raw
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default(),
    }
}

/// Whether one block is a tool call (dropped by max-token truncation).
const fn is_tool_call(block: &ContentBlock) -> bool {
    matches!(
        block,
        ContentBlock::Known(KnownContentBlock::ToolCall { .. })
    )
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

    fn tool_delta(index: u64, id: &str, name: Option<&str>, fragment: &str) -> StreamChunk {
        StreamChunk::Known(KnownStreamChunk::ToolCallDelta {
            index,
            id: ToolCallId::new(id),
            name: name.map(str::to_owned),
            arguments_delta: fragment.to_owned(),
        })
    }

    fn text_block(index: u64, text: &str) -> StreamChunk {
        StreamChunk::Known(KnownStreamChunk::BlockEnd {
            index,
            block: ContentBlock::Known(KnownContentBlock::Text {
                text: text.to_owned(),
            }),
        })
    }

    fn tool_block(index: u64, id: &str, name: &str, arguments: &str) -> StreamChunk {
        StreamChunk::Known(KnownStreamChunk::BlockEnd {
            index,
            block: ContentBlock::Known(KnownContentBlock::ToolCall {
                id: ToolCallId::new(id),
                name: name.to_owned(),
                arguments: arguments.to_owned(),
            }),
        })
    }

    #[test]
    fn assembled_usage_precedes_finish() {
        let mut assembler = BlockAssembler::new();
        assembler.push(&text_block(0, "hello"));
        let usage = TokenUsage {
            input_tokens: 7,
            output_tokens: 3,
            ..TokenUsage::default()
        };
        assembler.push(&StreamChunk::Known(KnownStreamChunk::Usage { usage }));
        assembler.push(&StreamChunk::Known(KnownStreamChunk::Finish {
            reason: FinishReason::Known(FinishReasonKind::Stop),
            replay_state: None,
        }));
        // The usage survives to the reader and the finish reason is the one
        // the stream carried; the protocol emits usage before finish, and
        // settlement exposes both with that order preserved.
        assert_eq!(assembler.usage(), Some(usage));
        assert_eq!(
            assembler.finish(),
            FinishReason::Known(FinishReasonKind::Stop)
        );
        let blocks = assembler.blocks().expect("settlement");
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn assembler_tool_arguments_raw_roundtrip() {
        let mut assembler = BlockAssembler::new();
        assembler.push(&StreamChunk::Known(KnownStreamChunk::BlockStart {
            index: 0,
            block_type: "tool-call".to_owned(),
        }));
        // Fragments split mid-string; concatenation must reproduce the raw
        // provider JSON byte for byte.
        let raw = r#"{"cmd":"ls -la","n":42}"#;
        let (a, b) = raw.split_at(raw.len() / 2);
        assembler.push(&tool_delta(0, "call-9", Some("bash"), a));
        assembler.push(&tool_delta(0, "call-9", None, b));
        let blocks = assembler.blocks().expect("settlement");
        let [ContentBlock::Known(KnownContentBlock::ToolCall { arguments, .. })] =
            blocks.as_slice()
        else {
            panic!("tool-call block expected");
        };
        assert_eq!(arguments, raw);
    }

    #[test]
    fn assembler_ignores_repeated_close() {
        let mut assembler = BlockAssembler::new();
        assembler.push(&text_block(0, "first"));
        assembler.push(&text_block(0, "second"));
        let blocks = assembler.blocks().expect("settlement");
        assert_eq!(
            blocks,
            vec![ContentBlock::Known(KnownContentBlock::Text {
                text: "first".to_owned()
            })]
        );
    }

    #[test]
    fn assembler_ignores_delta_after_close() {
        let mut assembler = BlockAssembler::new();
        assembler.push(&text_block(0, "closed"));
        assembler.push(&text_delta(0, " straggler"));
        let blocks = assembler.blocks().expect("settlement");
        assert_eq!(
            blocks,
            vec![ContentBlock::Known(KnownContentBlock::Text {
                text: "closed".to_owned()
            })]
        );
    }

    #[test]
    fn assembler_default_finish_is_stop() {
        let mut assembler = BlockAssembler::new();
        assembler.push(&text_delta(0, "partial"));
        assert_eq!(
            assembler.finish(),
            FinishReason::Known(FinishReasonKind::Stop)
        );
        assert!(assembler.usage().is_none());
    }

    #[test]
    fn assembler_max_tokens_and_interrupt() {
        // Max-tokens truncation drops every tool call and prunes the replay
        // envelope's per-block entries in step.
        let mut assembler = BlockAssembler::new();
        assembler.push(&text_block(0, "partial answer"));
        assembler.push(&tool_block(1, "call-1", "bash", "{}"));
        assembler.push(&StreamChunk::Known(KnownStreamChunk::Finish {
            reason: FinishReason::Known(FinishReasonKind::MaxTokens),
            replay_state: Some(ReplayEnvelope {
                response: serde_json::json!({"kind": "k", "version": 1}),
                blocks: Some(vec![
                    serde_json::json!({"type": "text"}),
                    serde_json::json!({"type": "tool-call"}),
                ]),
            }),
        }));
        let blocks = assembler.blocks().expect("settlement");
        assert_eq!(blocks.len(), 1);
        assert!(!is_tool_call(&blocks[0]));
        let replay = assembler.replay_state().expect("settlement");
        let replay = replay.expect("aligned envelope");
        assert_eq!(replay.blocks.expect("pruned entries").len(), 1);
        // A misaligned envelope (two blocks seen, one envelope entry) is
        // dropped whole rather than pruned blindly.
        let mut misaligned = BlockAssembler::new();
        misaligned.push(&text_block(0, "text"));
        misaligned.push(&tool_block(1, "call-1", "bash", "{}"));
        misaligned.push(&StreamChunk::Known(KnownStreamChunk::Finish {
            reason: FinishReason::Known(FinishReasonKind::MaxTokens),
            replay_state: Some(ReplayEnvelope {
                response: serde_json::json!({"kind": "k", "version": 1}),
                blocks: Some(vec![serde_json::json!({"type": "text"})]),
            }),
        }));
        assert!(misaligned.replay_state().expect("settlement").is_none());
        // An aligned envelope with no dropped blocks rides through verbatim.
        let mut aligned = BlockAssembler::new();
        aligned.push(&text_block(0, "text"));
        aligned.push(&StreamChunk::Known(KnownStreamChunk::Finish {
            reason: FinishReason::Known(FinishReasonKind::MaxTokens),
            replay_state: Some(ReplayEnvelope {
                response: serde_json::json!({"kind": "k", "version": 1}),
                blocks: Some(vec![serde_json::json!({"type": "text"})]),
            }),
        }));
        let aligned_replay = aligned.replay_state().expect("settlement");
        assert_eq!(
            aligned_replay
                .expect("aligned envelope")
                .blocks
                .expect("entries")
                .len(),
            1
        );
        // Interruption keeps non-whitespace text/reasoning in stream order
        // and omits tool calls entirely.
        let mut interrupted = BlockAssembler::new();
        interrupted.push(&text_block(0, "  "));
        interrupted.push(&text_block(1, "kept"));
        interrupted.push(&StreamChunk::Known(KnownStreamChunk::BlockStart {
            index: 2,
            block_type: "reasoning".to_owned(),
        }));
        interrupted.push(&StreamChunk::Known(KnownStreamChunk::ReasoningDelta {
            index: 2,
            text: "thinking".to_owned(),
        }));
        interrupted.push(&tool_delta(3, "call-2", Some("bash"), "{}"));
        let kept = interrupted.interrupted_blocks();
        assert_eq!(kept.len(), 2);
        // Stream order: index 1 (kept text) precedes index 2 (open reasoning).
        assert!(matches!(
            kept[0],
            ContentBlock::Known(KnownContentBlock::Text { .. })
        ));
        assert!(matches!(
            kept[1],
            ContentBlock::Known(KnownContentBlock::Reasoning { .. })
        ));
    }
}
