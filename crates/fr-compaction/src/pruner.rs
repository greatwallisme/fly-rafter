//! The deterministic tool-result pruning service: a replay-safe,
//! model-free head/middle/tail pruning pass over the current
//! tool-result surface nodes, landed as shadow-priced replacements.
//!
//! The dsh source is compaction-tool-result-pruner.
//!
//! PRD 4.5 ships the pruner inside fr-compaction, so the engine calls
//! it in-process (the dsh `ctx.get` soft seam becomes the engine's
//! `Option` prune hook; the cross-instance require a separate service
//! would need is deterministically refused mid-entry by the kernel's
//! per-instance lock).

use crate::region::{AppendRequest, MeterView, SessionView};
use crate::types::{CompactionPruneData, ShadowedRange};
use fr_wire::content::{ContentBlock, KnownContentBlock};
use fr_wire::event::{ReplaceOp, SessionEventData, SurfaceOp, ToolResultData, event_type};
use fr_wire::ids::{SessionSeq, ToolCallId};
use fr_wire::message::{KnownMessage, Message};

/// Fixed marker substituted for every removed middle span (dsh
/// `PRUNE_MARKER`, verbatim).
pub const PRUNE_MARKER: &str = "\n\n[... tool result middle pruned ...]\n\n";

/// Validated character budgets for one deterministic pruning pass (the
/// dsh `ResolvedConfig`; the resolution lives with the compaction
/// config so the one-crate namespace carries the pruner's keys).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PruneConfig {
    /// Prune when total text exceeds this many Unicode code points.
    pub threshold_chars: u64,
    /// Maximum leading Unicode code points retained.
    pub head_chars: u64,
    /// Maximum trailing Unicode code points retained.
    pub tail_chars: u64,
}

/// Cited source event and size accounting for one landed surface
/// replacement (the dsh `PrunedEntry`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrunedEntry {
    /// Full-fidelity tool-result event shadowed by the replacement.
    pub original_seq: SessionSeq,
    /// Newly appended pruned tool-result event.
    pub replacement_seq: SessionSeq,
    /// Tool call shared by the original and the replacement.
    pub call_id: ToolCallId,
    /// Original text size in Unicode code points.
    pub chars_before: u64,
    /// Replacement text size in Unicode code points.
    pub chars_after: u64,
}

/// Aggregate outcome of one stable-surface pruning pass (the dsh
/// `PruneResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneResult {
    /// Replacements in the snapshotted surface order.
    pub pruned: Vec<PrunedEntry>,
    /// Total Unicode code points removed across the replacements.
    pub chars_removed: u64,
}

/// Counts Unicode code points without splitting surrogate pairs (the
/// dsh `codePointLength`; `Array.from` walks code points exactly as
/// Rust's `chars` does).
#[must_use]
pub fn code_point_length(text: &str) -> usize {
    text.chars().count()
}

/// Measures text content in Unicode code points; non-text blocks cost
/// zero (the dsh `measureContent`).
#[must_use]
pub fn measure_content(blocks: &[ContentBlock]) -> usize {
    blocks
        .iter()
        .map(|block| match block {
            ContentBlock::Known(KnownContentBlock::Text { text }) => code_point_length(text),
            _ => 0,
        })
        .sum()
}

/// Replaces an over-budget text middle while retaining rich-block order
/// (the dsh `pruneContent`).
///
/// Text slicing is by Unicode code point, so a retained boundary cannot
/// split a surrogate pair; grapheme clusters may still split.
///
/// # Errors
///
/// Returns the verbatim refusal when the removed span cannot be
/// located or the replacement would not be a strict improvement —
/// both unreachable under validated budgets, kept as the dsh throw
/// arms keep them.
pub fn prune_content(
    blocks: &[ContentBlock],
    config: &PruneConfig,
) -> Result<Option<Vec<ContentBlock>>, String> {
    let total_chars = measure_content(blocks);
    if total_chars as u64 <= config.threshold_chars {
        return Ok(None);
    }

    let removed_start = usize::try_from(config.head_chars).unwrap_or(usize::MAX);
    let removed_end =
        total_chars.saturating_sub(usize::try_from(config.tail_chars).unwrap_or(usize::MAX));
    let mut pruned = Vec::new();
    let mut consumed = 0_usize;
    let mut marker_inserted = false;

    for block in blocks {
        let ContentBlock::Known(KnownContentBlock::Text { text }) = block else {
            pruned.push(block.clone());
            continue;
        };

        let points: Vec<char> = text.chars().collect();
        let block_start = consumed;
        let block_end = block_start + points.len();
        let head_end = points.len().min(removed_start.saturating_sub(block_start));
        let tail_start = points.len().min(removed_end.saturating_sub(block_start));
        let intersects_removed = block_start < removed_end && block_end > removed_start;
        let marker = if intersects_removed && !marker_inserted {
            marker_inserted = true;
            PRUNE_MARKER
        } else {
            ""
        };
        let replaced: String = points[..head_end].iter().collect::<String>()
            + marker
            + points[tail_start..].iter().collect::<String>().as_str();
        if !replaced.is_empty() {
            pruned.push(ContentBlock::Known(KnownContentBlock::Text {
                text: replaced,
            }));
        }
        consumed = block_end;
    }

    // Valid budgets (head + marker + tail <= threshold) and a total
    // above the threshold guarantee a removed text span.
    if !marker_inserted {
        return Err("tool-result prune: failed to locate the removed text span".to_owned());
    }
    let chars_after = measure_content(&pruned);
    if chars_after as u64 > config.threshold_chars || chars_after >= total_chars {
        return Err(
            "tool-result prune: replacement must be smaller and within threshold".to_owned(),
        );
    }
    Ok(Some(pruned))
}

/// Prunes every over-budget tool result from one stable current-surface
/// snapshot.
///
/// The dsh face is `pruneSession`. Each replacement preserves the complete event data except for
/// `content`, cites the shadowed node so replay can recover the
/// replacement input, and is immediately preceded by a
/// `compaction/prune` shadow-price event pricing the shadowed node
/// through the injected token meter, so pure consumers can subtract it
/// without per-node state.
///
/// # Errors
///
/// Returns the diagnostic when the session refuses a replacement;
/// replacements committed earlier in the pass remain durable.
pub fn prune_session(
    view: &mut dyn SessionView,
    meter: &dyn MeterView,
    config: &PruneConfig,
) -> Result<PruneResult, String> {
    // The candidates snapshot reads the current surface; the derived
    // messages carry every committed projection (the dsh
    // `deriveEventMessage` live read).
    let surface: Vec<SessionSeq> = view.surface_nodes().to_vec();
    let candidates: Vec<SessionSeq> = surface
        .into_iter()
        .filter(|seq| {
            view.events()
                .get(usize::try_from(seq.get()).unwrap_or(usize::MAX))
                .is_some_and(|event| event.event_type() == event_type::TOOL_RESULT)
        })
        .collect();

    let mut pruned = Vec::new();
    let mut chars_removed = 0_u64;
    for seq in candidates {
        // All reads complete and own their values before the two
        // appends mutate the view (the local reflection invalidates
        // the borrowed slices).
        let Some(event) = view
            .events()
            .get(usize::try_from(seq.get()).unwrap_or(usize::MAX))
        else {
            continue;
        };
        let SessionEventData::ToolResult(data) = &event.data else {
            continue;
        };
        let original = view.projected_message(seq).unwrap_or(&data.message).clone();
        let stored = serde_json::to_value(data).map_err(|error| error.to_string())?;
        let Some(content) = prune_content(original_content(&original), config)? else {
            continue;
        };
        let chars_before = measure_content(original_content(&original)) as u64;
        let chars_after = measure_content(&content) as u64;
        let message = fr_session::message_projection::with_content(&original, content)
            .ok_or("tool-result prune: the projected message carries no known shape")?;
        // Shadow-price protocol: the metering event and its replacement
        // are appended synchronously adjacent, so pure consumers
        // subtract the shadowed node's heuristic price without
        // retaining per-node state.
        let price = CompactionPruneData {
            shadowed_range: ShadowedRange {
                start: seq,
                end: seq,
            },
            shadowed_seqs: vec![seq],
            shadowed_token_count: meter.estimate_message(&original),
        };
        view.append(AppendRequest {
            type_name: crate::types::event_type::PRUNE,
            data: serde_json::to_value(&price).map_err(|error| error.to_string())?,
            surface_op: None,
            source_event_seqs: None,
        })?;
        let mut replacement = stored;
        replacement["message"] =
            serde_json::to_value(&message).map_err(|error| error.to_string())?;
        let landed = view.append(AppendRequest {
            type_name: event_type::TOOL_RESULT,
            data: replacement,
            surface_op: Some(SurfaceOp::Replace(ReplaceOp {
                op: fr_wire::event::ReplaceTag,
                start_seq: seq,
                end_seq: seq,
            })),
            source_event_seqs: Some(vec![seq]),
        })?;
        let SessionEventData::ToolResult(ToolResultData {
            message: landed_message,
            ..
        }) = &landed.data
        else {
            return Err("tool-result prune: the landed replacement lost its message".to_owned());
        };
        let Message::Known(known) = landed_message else {
            return Err("tool-result prune: the landed replacement lost its message".to_owned());
        };
        let KnownMessage::Tool { source, .. } = known.as_ref() else {
            return Err("tool-result prune: the landed replacement lost its tool role".to_owned());
        };
        let fr_wire::message::MessageSource::Known(fr_wire::message::KnownMessageSource::Tool {
            call_id,
            ..
        }) = source
        else {
            return Err("tool-result prune: the landed replacement lost its call id".to_owned());
        };
        pruned.push(PrunedEntry {
            original_seq: seq,
            replacement_seq: landed.seq,
            call_id: call_id.clone(),
            chars_before,
            chars_after,
        });
        chars_removed += chars_before - chars_after;
    }
    Ok(PruneResult {
        pruned,
        chars_removed,
    })
}

/// The message content a prune measures and rewrites (the projected
/// values and the stored tool results carry known shapes only).
fn original_content(message: &Message) -> &[ContentBlock] {
    match message {
        Message::Known(known) => known.content(),
        Message::Other(_) => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::{PRUNE_MARKER, PruneConfig, code_point_length, measure_content, prune_content};
    use fr_wire::content::{ContentBlock, KnownContentBlock};

    fn text(value: &str) -> ContentBlock {
        ContentBlock::Known(KnownContentBlock::Text {
            text: value.to_owned(),
        })
    }

    fn config(threshold: u64, head: u64, tail: u64) -> PruneConfig {
        PruneConfig {
            threshold_chars: threshold,
            head_chars: head,
            tail_chars: tail,
        }
    }

    #[test]
    fn marker_text_is_verbatim() {
        assert_eq!(PRUNE_MARKER, "\n\n[... tool result middle pruned ...]\n\n");
        // The config invariant quotes this length through the resolution.
        assert_eq!(code_point_length(PRUNE_MARKER), 39);
    }

    #[test]
    fn within_budget_content_prunes_to_none() {
        let blocks = vec![text("short")];
        assert_eq!(prune_content(&blocks, &config(100, 4, 2)), Ok(None));
    }

    #[test]
    fn over_budget_middle_is_replaced_once_with_marker() {
        // 60 code points of text, head 2 / tail 3, threshold 50 above
        // the emitted budget: the removed span is [2, 57).
        let blocks = vec![text("a".repeat(60).as_str())];
        let pruned = prune_content(&blocks, &config(50, 2, 3))
            .expect("prune")
            .expect("over budget");
        assert_eq!(measure_content(&pruned), 2 + 39 + 3);
        let ContentBlock::Known(KnownContentBlock::Text { text }) = &pruned[0] else {
            panic!("text block");
        };
        assert_eq!(text.as_str(), format!("aa{PRUNE_MARKER}aaa"));
    }

    #[test]
    fn non_text_blocks_keep_order_and_cost_zero() {
        let image = ContentBlock::Known(KnownContentBlock::Image {
            attachment: fr_wire::content::ImageAttachmentRef {
                attachment_id: fr_wire::ids::AttachmentId::new("a-1".to_owned()),
                media_type: fr_wire::content::ImageMediaType::Png,
                bytes: 10,
                width: 1,
                height: 1,
                name: None,
                original_dimensions: None,
            },
            offloaded: None,
        });
        let blocks = vec![
            image.clone(),
            text("abcdefghij".repeat(6).as_str()),
            image.clone(),
        ];
        let pruned = prune_content(&blocks, &config(50, 2, 3))
            .expect("prune")
            .expect("over budget");
        assert_eq!(pruned.len(), 3, "the images stay in place");
        assert_eq!(pruned[0], image);
        assert_eq!(pruned[2], image);
        assert_eq!(measure_content(&pruned), 2 + 39 + 3);
    }

    #[test]
    fn marker_lands_in_the_first_intersecting_block() {
        // Two text blocks: the removed span crosses both; exactly one
        // marker lands, in the first (threshold 43 fits the emitted
        // budget 2 + 39 + 2 and sits under the 44-point total).
        let blocks = vec![text("a".repeat(20).as_str()), text("b".repeat(24).as_str())];
        let pruned = prune_content(&blocks, &config(43, 2, 2))
            .expect("prune")
            .expect("over budget");
        let texts: Vec<String> = pruned
            .iter()
            .map(|block| match block {
                ContentBlock::Known(KnownContentBlock::Text { text }) => text.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(texts, vec![format!("aa{PRUNE_MARKER}"), "bb".to_owned()]);
    }

    #[test]
    fn head_spans_past_a_block_boundary() {
        // The removed span opens inside the second block, so the first
        // block keeps its leading run without a marker and the marker
        // lands at the second block's head boundary.
        let blocks = vec![
            text("ab".to_owned().as_str()),
            text("c".repeat(48).as_str()),
        ];
        let pruned = prune_content(&blocks, &config(46, 4, 3))
            .expect("prune")
            .expect("over budget");
        let texts: Vec<String> = pruned
            .iter()
            .map(|block| match block {
                ContentBlock::Known(KnownContentBlock::Text { text }) => text.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(texts.len(), 2);
        assert_eq!(texts[0], "ab");
        assert_eq!(texts[1], format!("cc{PRUNE_MARKER}ccc"));
    }
}
