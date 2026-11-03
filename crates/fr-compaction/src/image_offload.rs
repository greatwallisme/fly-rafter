//! The provider-driven image offload recovery: select and log
//! permanent image omissions in current model-request order, and
//! consume the two recovery waterfalls.
//!
//! The dsh source is compaction-image-offload. The durable decision is
//! one `image/offload` event; its projection lives in fr-session (the
//! fold owner — PRD 19.2 item 14's mechanism landing), so every later
//! derivation sees the occurrences marked.

use crate::region::{AppendRequest, SessionView};
use fr_wire::event::{SessionEventData, event_type};
use fr_wire::ids::SessionSeq;

/// One exact input-image occurrence set selected by a durable decision
/// (the dsh `ImageOffloadTarget`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageOffloadTarget {
    /// Current message-producing event containing these occurrences.
    pub seq: SessionSeq,
    /// Zero-based depth-first image indexes within the immutable
    /// message.
    pub image_indexes: Vec<u64>,
}

/// Records one decision omitting the oldest retained input-image
/// occurrences in request order (the dsh `offloadOldestImages`).
///
/// Assistant nodes carry model output and are excluded. Image indexes
/// count every occurrence, including previously offloaded ones, within
/// each message.
///
/// # Errors
///
/// Returns the wire diagnostic when the session refuses the decision.
pub fn offload_oldest_images(
    view: &mut dyn SessionView,
    source_event_seqs: &[SessionSeq],
    count: u64,
) -> Result<bool, String> {
    let mut remaining = count;
    let mut targets: Vec<ImageOffloadTarget> = Vec::new();
    for seq in source_event_seqs {
        if remaining == 0 {
            break;
        }
        let Some(event) = view
            .events()
            .get(usize::try_from(seq.get()).unwrap_or(usize::MAX))
        else {
            continue;
        };
        if event.event_type() != event_type::USER_MESSAGE
            && event.event_type() != event_type::TOOL_RESULT
        {
            continue;
        }
        // Both input node types produce a message; the projected value
        // wins so earlier decisions count (the dsh `deriveEventMessage`
        // live read).
        let content: &[fr_wire::content::ContentBlock] = match &event.data {
            SessionEventData::UserMessage(message) => content_of(message),
            SessionEventData::ToolResult(data) => content_of(&data.message),
            _ => continue,
        };
        let projected = view.projected_message(*seq);
        let content = projected.map_or(content, content_of);
        let mut image_indexes = Vec::new();
        let mut image_index = 0_u64;
        for block in content {
            let fr_wire::content::ContentBlock::Known(fr_wire::content::KnownContentBlock::Image {
                offloaded,
                ..
            }) = block
            else {
                continue;
            };
            if remaining == 0 {
                break;
            }
            if offloaded.is_none() {
                image_indexes.push(image_index);
                remaining -= 1;
            }
            image_index += 1;
        }
        if !image_indexes.is_empty() {
            targets.push(ImageOffloadTarget {
                seq: *seq,
                image_indexes,
            });
        }
    }
    if targets.is_empty() {
        return Ok(false);
    }
    let data = serde_json::json!({
        "targets": targets
            .iter()
            .map(|target| serde_json::json!({
                "seq": target.seq.get(),
                "imageIndexes": target.image_indexes,
            }))
            .collect::<Vec<_>>(),
    });
    view.append(AppendRequest {
        type_name: fr_session::message_projection::IMAGE_OFFLOAD,
        data,
        surface_op: None,
        source_event_seqs: None,
    })?;
    Ok(true)
}

/// The message's model-facing blocks (the fr-session
/// `message_content` shape; a decision only ever targets known-shape
/// messages, which the admission gate enforces).
fn content_of(message: &fr_wire::message::Message) -> &[fr_wire::content::ContentBlock] {
    match message {
        fr_wire::message::Message::Known(known) => known.content(),
        fr_wire::message::Message::Other(_) => &[],
    }
}
