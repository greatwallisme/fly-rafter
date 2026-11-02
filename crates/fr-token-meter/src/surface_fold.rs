//! Positional pricing for measurement (dsh `token-meter/surface-fold.ts`
//! 1:1).
//!
//! Each retained node keeps its identity, heuristic price, and the
//! attachment details route pricing replaces. The occupancy and
//! context-breakdown projections use the scalar shadow-price protocol
//! instead.
//!
//! The fold is a plan/commit pair: [`plan_surface_tokens`] runs every
//! fallible step read-only and [`commit_surface_tokens`] mutates in
//! place, so a failure leaves the caller's state untouched and the same
//! malformed event fails identically on every retry. Nodes also carry
//! durable attachment occurrences and their structural prices, so
//! `measure` can price the request representation sent to the model.

#![forbid(unsafe_code)]

use fr_wire::content::{ContentBlock, KnownContentBlock};
use fr_wire::event::{SessionEvent, SurfaceOp};

use crate::estimate::{estimate_message, estimate_structural_block};

/// One priced surface node with the image occurrences route pricing
/// replaces (dsh `MeterSurfaceNode`).
#[derive(Debug, Clone, PartialEq)]
pub struct MeterSurfaceNode {
    /// Durable sequence number of the surface event.
    pub seq: u64,
    /// Fixed-heuristic price of the node's exact message.
    pub heuristic_tokens: u64,
    /// Structural JSON price replaced when the routed request projects
    /// images.
    pub image_structural_tokens: u64,
    /// Structural JSON price replaced when request assembly projects
    /// files to text.
    pub file_structural_tokens: u64,
    /// Durable image blocks in message order, `offloaded` marks
    /// included; empty for image-free nodes.
    pub images: Vec<ContentBlock>,
    /// Durable file occurrences in message order; empty for file-free
    /// nodes.
    pub files: Vec<fr_wire::content::FileAttachmentRef>,
}

/// Commit position: append at the tail, or the inclusive replaced index
/// range (dsh `'append' | {startIdx, endIdx}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanTarget {
    /// Insert at the surface tail.
    Append,
    /// Replace the inclusive closed index range.
    Replace {
        /// First replaced index.
        start_idx: usize,
        /// Last replaced index.
        end_idx: usize,
    },
}

/// One validated surface transition that has not mutated the priced
/// surface yet (dsh `SurfaceTokenPlan`).
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceTokenPlan {
    /// Heuristic price of the event's own message; 0 when it derives
    /// none.
    pub tokens: u64,
    /// Signed change in the surface total: `tokens` minus anything
    /// shadowed.
    pub delta_tokens: i64,
    /// The priced node the commit inserts for this event.
    pub node: MeterSurfaceNode,
    /// Commit position.
    pub target: PlanTarget,
}

/// Collect projected attachment occurrences and their structural prices
/// (the dsh `collectProjectedAttachments`).
fn collect_projected_attachments(
    blocks: &[ContentBlock],
    images: &mut Vec<ContentBlock>,
    files: &mut Vec<fr_wire::content::FileAttachmentRef>,
) -> (u64, u64) {
    let mut image_tokens = 0u64;
    let mut file_tokens = 0u64;
    for block in blocks {
        match block {
            ContentBlock::Known(KnownContentBlock::Image { .. }) => {
                images.push(block.clone());
                image_tokens = image_tokens.saturating_add(estimate_structural_block(block));
            }
            ContentBlock::Known(KnownContentBlock::File { attachment }) => {
                files.push(attachment.clone());
                file_tokens = file_tokens.saturating_add(estimate_structural_block(block));
            }
            _ => {}
        }
    }
    (image_tokens, file_tokens)
}

/// Build one priced node from a surface event's derived message (the
/// dsh `analyzeNode`).
fn analyze_node(seq: u64, message: Option<&fr_wire::message::Message>) -> MeterSurfaceNode {
    let Some(message) = message else {
        return MeterSurfaceNode {
            seq,
            heuristic_tokens: 0,
            image_structural_tokens: 0,
            file_structural_tokens: 0,
            images: Vec::new(),
            files: Vec::new(),
        };
    };
    let heuristic_tokens = estimate_message(message);
    let mut images = Vec::new();
    let mut files = Vec::new();
    let (image_tokens, file_tokens) = {
        let content = message_content(message);
        collect_projected_attachments(&content, &mut images, &mut files)
    };
    MeterSurfaceNode {
        seq,
        heuristic_tokens,
        image_structural_tokens: image_tokens,
        file_structural_tokens: file_tokens,
        images,
        files,
    }
}

/// The surface message an event contributes: the typed derivation for
/// known events, or the raw message of an unknown-typed surface event
/// retained verbatim (the fr envelope keeps both).
fn event_message(event: &SessionEvent) -> Option<fr_wire::message::Message> {
    match &event.data {
        fr_wire::event::SessionEventData::Unknown(_) => None,
        _ => fr_session::derive_event_message(event).cloned(),
    }
}

/// Validate and price one surface event without mutating the surface
/// (dsh `planSurfaceTokens`).
///
/// # Errors
///
/// Returns the dsh verbatim text when a replacement names a range
/// absent from `nodes`: committed logs are surface-validated at append
/// time, so an unresolvable range is log corruption and must fail loud
/// rather than skip the event.
pub fn plan_surface_tokens(
    nodes: &[MeterSurfaceNode],
    event: &SessionEvent,
) -> Result<SurfaceTokenPlan, String> {
    let node = analyze_node(event.seq.get(), event_message(event).as_ref());
    let tokens = node.heuristic_tokens;
    let Some(surface_op) = event.surface_op else {
        return Ok(SurfaceTokenPlan {
            tokens,
            delta_tokens: i64::try_from(tokens).unwrap_or(i64::MAX),
            node,
            target: PlanTarget::Append,
        });
    };
    let SurfaceOp::Replace(replace) = surface_op else {
        return Ok(SurfaceTokenPlan {
            tokens,
            delta_tokens: i64::try_from(tokens).unwrap_or(i64::MAX),
            node,
            target: PlanTarget::Append,
        });
    };
    let start_idx = nodes
        .iter()
        .position(|candidate| candidate.seq == replace.start_seq.get());
    let end_idx = nodes
        .iter()
        .position(|candidate| candidate.seq == replace.end_seq.get());
    let (Some(start_idx), Some(end_idx)) = (start_idx, end_idx) else {
        return Err(format!(
            "token surface: replace at seq {} has invalid current range {}-{}",
            event.seq.get(),
            replace.start_seq.get(),
            replace.end_seq.get()
        ));
    };
    if start_idx > end_idx {
        return Err(format!(
            "token surface: replace at seq {} has invalid current range {}-{}",
            event.seq.get(),
            replace.start_seq.get(),
            replace.end_seq.get()
        ));
    }
    let removed: u64 = nodes[start_idx..=end_idx]
        .iter()
        .map(|candidate| candidate.heuristic_tokens)
        .sum();
    let delta =
        i64::try_from(tokens).unwrap_or(i64::MAX) - i64::try_from(removed).unwrap_or(i64::MAX);
    Ok(SurfaceTokenPlan {
        tokens,
        delta_tokens: delta,
        node,
        target: PlanTarget::Replace { start_idx, end_idx },
    })
}

/// Apply one validated plan to the priced surface in place; infallible,
/// so it cannot leave a half-applied surface behind (dsh
/// `commitSurfaceTokens`).
pub fn commit_surface_tokens(nodes: &mut Vec<MeterSurfaceNode>, plan: SurfaceTokenPlan) {
    match plan.target {
        PlanTarget::Append => nodes.push(plan.node),
        PlanTarget::Replace { start_idx, end_idx } => {
            nodes.splice(start_idx..=end_idx, std::iter::once(plan.node));
        }
    }
}

/// The exact content blocks of a message across both envelope shapes:
/// the typed known content, or the raw blocks of a verbatim unknown
/// message parsed back through the wire shapes.
fn message_content(message: &fr_wire::message::Message) -> Vec<ContentBlock> {
    match message {
        fr_wire::message::Message::Known(known) => known.content().to_vec(),
        fr_wire::message::Message::Other(raw) => raw
            .get("content")
            .and_then(|content| content.as_array())
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|block| serde_json::from_value(block.clone()).ok())
                    .collect()
            })
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::estimate::{ROLE_OVERHEAD, utf16_len};
    use fr_wire::event::SessionEventData;

    fn text_message(text: &str) -> fr_wire::message::Message {
        fr_wire::message::Message::Known(Box::new(fr_wire::message::KnownMessage::User {
            id: fr_wire::ids::MessageId::new("m-1"),
            content: vec![ContentBlock::Known(KnownContentBlock::Text {
                text: text.to_owned(),
            })],
            source: fr_wire::message::MessageSource::Known(
                fr_wire::message::KnownMessageSource::User {
                    extra: fr_wire::content::RawObject::new(),
                },
            ),
            extra: fr_wire::content::RawObject::new(),
        }))
    }

    fn append_event(seq: u64, text: &str) -> SessionEvent {
        SessionEvent {
            seq: fr_wire::SessionSeq::new(seq)
                .unwrap_or_else(|e| panic!("seq {seq} rejected: {e}")),
            time: 1,
            data: SessionEventData::UserMessage(text_message(text)),
            ignorable: None,
            surface_op: Some(SurfaceOp::Append(fr_wire::event::AppendOp)),
            source_event_seqs: None,
        }
    }

    fn replace_event(seq: u64, start: u64, end: u64, text: &str) -> SessionEvent {
        let mut event = append_event(seq, text);
        event.surface_op = Some(SurfaceOp::Replace(fr_wire::event::ReplaceOp {
            op: fr_wire::event::ReplaceTag,
            start_seq: fr_wire::SessionSeq::new(start)
                .unwrap_or_else(|e| panic!("seq {start} rejected: {e}")),
            end_seq: fr_wire::SessionSeq::new(end)
                .unwrap_or_else(|e| panic!("seq {end} rejected: {e}")),
        }));
        event
    }

    fn append_nodes(surface: &mut Vec<MeterSurfaceNode>, event: &SessionEvent) -> i64 {
        let plan = plan_surface_tokens(surface, event).expect("plan");
        let delta = plan.delta_tokens;
        commit_surface_tokens(surface, plan);
        delta
    }

    /// Appends price ceil(len/4)+4 plus role framing; the running total
    /// grows by the plan delta.
    #[test]
    fn appends_price_and_accumulate() {
        let mut surface = Vec::new();
        let delta = append_nodes(&mut surface, &append_event(0, "abcdefgh"));
        let expected = utf16_len("abcdefgh").div_ceil(4) + 4 + ROLE_OVERHEAD;
        assert_eq!(delta, i64::try_from(expected).unwrap_or(i64::MAX));
        assert_eq!(surface.len(), 1);
        assert_eq!(surface[0].seq, 0);
        assert_eq!(surface[0].heuristic_tokens, expected);
    }

    /// A replacement subtracts the removed range's heuristic price and
    /// swaps the nodes in place; an unresolvable range fails loud with
    /// the dsh verbatim text.
    #[test]
    fn replace_prices_shadow_and_fail_loud() {
        let mut surface = Vec::new();
        let _ = append_nodes(&mut surface, &append_event(0, "abcdefgh"));
        let _ = append_nodes(&mut surface, &append_event(1, "ijklmnop"));
        let delta = append_nodes(&mut surface, &replace_event(2, 0, 1, "xy"));
        let added = utf16_len("xy").div_ceil(4) + 4 + ROLE_OVERHEAD;
        let removed = 2 * (utf16_len("abcdefgh").div_ceil(4) + 4 + ROLE_OVERHEAD);
        assert_eq!(
            delta,
            i64::try_from(added).unwrap_or(i64::MAX) - i64::try_from(removed).unwrap_or(i64::MAX)
        );
        assert_eq!(surface.len(), 1);
        assert_eq!(surface[0].seq, 2);

        let error = plan_surface_tokens(&surface, &replace_event(3, 5, 9, "z"))
            .expect_err("unresolvable range");
        assert_eq!(
            error,
            "token surface: replace at seq 3 has invalid current range 5-9"
        );
    }
}
