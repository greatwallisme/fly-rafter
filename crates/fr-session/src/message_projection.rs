//! The message projection mechanism: pure interpreters for
//! plugin-owned events that change the content of existing messages,
//! applied inside the surface fold.
//!
//! The dsh face is `SessionMessageProjection`; PRD 19.2 item 14 lands
//! it with the `image/offload` writer. Registration topology: the dsh interpreters register per plugin on
//! the live sessions service; over the dougong boundary a pure
//! interpreter cannot cross artifacts, so the fold owner ships the
//! built-in registry.
//!
//! Every reachable composition reproduces the dsh semantics: a declared
//! `image/offload` row interprets through the built-in registry, while
//! an undeclared row (the owning plugin absent) is refused by the read
//! path's known-type gate before any fold runs.
//! [`crate::surface::fold_surface_with`] over an empty registry
//! exercises the missing-interpreter refusal verbatim (the dsh throw
//! shape).

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use fr_wire::content::{ContentBlock, KnownContentBlock, TrueOnly};
use fr_wire::event::{SessionEvent, SessionEventData, event_type};
use fr_wire::ids::SessionSeq;
use fr_wire::message::{KnownMessage, Message};

/// The `image/offload` wire event type name (the dsh
/// compaction-image-offload decision event).
pub const IMAGE_OFFLOAD: &str = "image/offload";

/// The event types a session log may carry that require a registered
/// interpreter (the dsh `MESSAGE_PROJECTION_EVENT_TYPES` set; one
/// member ships in M1).
pub const MESSAGE_PROJECTION_EVENT_TYPES: &[&str] = &[IMAGE_OFFLOAD];

/// `Number.MAX_SAFE_INTEGER`: the dsh `Number.isSafeInteger` bound for
/// the durable occurrence indexes and sequences.
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Whether a durable value is a canonical occurrence index or sequence
/// (the dsh `isIndex`: a non-negative safe integer; a negative zero
/// parses to the f64 `-0.0`, which carries no u64 form, so the
/// `Object.is(value, -0)` rejection rides the same arm).
fn is_index(value: &serde_json::Value) -> bool {
    value
        .as_u64()
        .is_some_and(|index| index <= MAX_SAFE_INTEGER)
}

/// Readonly history immediately before a message-projection event (the
/// dsh `SessionMessageProjectionContext`; the fr window is the full log
/// with base seq zero, so no base offset rides the context).
#[derive(Debug)]
pub struct ProjectionContext<'a> {
    /// Current message-producing event sequences in model-visible
    /// order.
    pub nodes: &'a [SessionSeq],
    /// The contiguous event window; entries at or beyond the candidate
    /// seq are not committed inputs.
    pub events: &'a [SessionEvent],
    /// Previously projected messages keyed by their original event
    /// sequences.
    pub messages: &'a HashMap<SessionSeq, Message>,
}

/// Pure interpretation of one plugin-owned event that changes existing
/// message content (the dsh `SessionMessageProjection`).
pub trait MessageProjection: Send + Sync {
    /// The event type interpreted by this definition.
    fn type_name(&self) -> &'static str;

    /// Validates the complete durable decision before returning any
    /// updates. Preserves message identities and publishes immutable
    /// copies without mutating the input.
    ///
    /// # Errors
    ///
    /// Returns the verbatim refusal when the durable decision cannot
    /// apply to this history.
    fn project(
        &self,
        event: &SessionEvent,
        context: &ProjectionContext<'_>,
    ) -> Result<Vec<(SessionSeq, Message)>, String>;
}

/// Projects one event through the registry, or refuses a required type
/// with no interpreter (the dsh `planSurfaceEvent` order: the registry
/// wins, then the required-type throw, then surface-op handling). The
/// state read is limited to the fold fields the interpreters see.
pub(crate) fn plan_projection(
    projections: &[&dyn MessageProjection],
    nodes: &[SessionSeq],
    projected_messages: &HashMap<SessionSeq, Message>,
    event: &SessionEvent,
    events: &[SessionEvent],
) -> Result<Option<Vec<(SessionSeq, Message)>>, String> {
    let name = event.event_type();
    let Some(projection) = projections.iter().find(|item| item.type_name() == name) else {
        if MESSAGE_PROJECTION_EVENT_TYPES.contains(&name) {
            return Err(format!(
                "session event \"{name}\" requires a message projection; \
                 load its owning plugin or supply its projection definition"
            ));
        }
        return Ok(None);
    };
    let context = ProjectionContext {
        nodes,
        events,
        messages: projected_messages,
    };
    projection.project(event, &context).map(Some)
}

/// The built-in registry the canonical fold runs (the dsh construction
/// face: the sessions service hands every session its registered
/// interpreters; the fr fold owner ships them statically).
#[must_use]
pub fn built_in_projections() -> &'static [&'static dyn MessageProjection] {
    &[&ImageOffloadProjection]
}

/// The `image/offload` interpreter (dsh compaction-image-offload
/// `projection.ts`): the browser-safe durable image selection
/// declaration and its pure replay definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageOffloadProjection;

/// Decodes the decision record off one event (the typed variants carry
/// no decision; the writer's rows decode as the retained-verbatim
/// unknown shape).
fn decision_data(event: &SessionEvent) -> Result<&RawObjectRef, String> {
    match &event.data {
        SessionEventData::Unknown(unknown) if unknown.type_name == IMAGE_OFFLOAD => {
            Ok(&unknown.data)
        }
        _ => Err("image/offload: data must contain a nonempty targets array".to_owned()),
    }
}

/// The borrowed decision-object view.
type RawObjectRef = serde_json::Map<String, serde_json::Value>;

impl MessageProjection for ImageOffloadProjection {
    fn type_name(&self) -> &'static str {
        IMAGE_OFFLOAD
    }

    fn project(
        &self,
        event: &SessionEvent,
        context: &ProjectionContext<'_>,
    ) -> Result<Vec<(SessionSeq, Message)>, String> {
        let data = serde_json::Value::Object(decision_data(event)?.clone());
        let Some(targets_value) = data.get("targets") else {
            return Err("image/offload: data must contain a nonempty targets array".to_owned());
        };
        let Some(targets) = targets_value.as_array() else {
            return Err("image/offload: data must contain a nonempty targets array".to_owned());
        };
        if targets.is_empty() {
            return Err("image/offload: data must contain a nonempty targets array".to_owned());
        }
        if data.as_object().is_none_or(|object| object.len() != 1) {
            return Err("image/offload: data must contain a nonempty targets array".to_owned());
        }
        let mut messages = Vec::new();
        let mut seen = HashSet::new();
        let nodes: HashSet<u64> = context.nodes.iter().map(|seq| seq.get()).collect();
        for target in targets {
            let projected = project_target(target, context, &mut seen, &nodes)?;
            messages.push(projected);
        }
        Ok(messages)
    }
}

/// Validates and applies one decision target, returning the projected
/// (sequence, message) pair.
fn project_target(
    target: &serde_json::Value,
    context: &ProjectionContext<'_>,
    seen: &mut HashSet<u64>,
    nodes: &HashSet<u64>,
) -> Result<(SessionSeq, Message), String> {
    let Some(object) = target.as_object() else {
        return Err(
            "image/offload: each target must contain a seq and nonempty imageIndexes".to_owned(),
        );
    };
    if object.len() != 2 {
        return Err(
            "image/offload: each target must contain a seq and nonempty imageIndexes".to_owned(),
        );
    }
    let Some(seq_value) = object.get("seq") else {
        return Err(
            "image/offload: each target must contain a seq and nonempty imageIndexes".to_owned(),
        );
    };
    if !is_index(seq_value) {
        return Err(
            "image/offload: each target must contain a seq and nonempty imageIndexes".to_owned(),
        );
    }
    let Some(indexes_value) = object.get("imageIndexes") else {
        return Err(
            "image/offload: each target must contain a seq and nonempty imageIndexes".to_owned(),
        );
    };
    let Some(indexes) = indexes_value.as_array() else {
        return Err(
            "image/offload: each target must contain a seq and nonempty imageIndexes".to_owned(),
        );
    };
    if indexes.is_empty() {
        return Err(
            "image/offload: each target must contain a seq and nonempty imageIndexes".to_owned(),
        );
    }
    let seq = seq_value.as_u64().expect("validated index");
    if !seen.insert(seq) {
        return Err(format!("image/offload: duplicate target seq {seq}"));
    }
    if !nodes.contains(&seq) {
        return Err(format!(
            "image/offload: target seq {seq} is not a current surface node"
        ));
    }
    let source = context
        .events
        .get(usize::try_from(seq).expect("validated index"));
    let source_type = source.map_or("", |event| event.event_type());
    if source_type != event_type::USER_MESSAGE && source_type != event_type::TOOL_RESULT {
        return Err(format!(
            "image/offload: target seq {seq} must be user/message or tool/result"
        ));
    }
    let mut previous = -1_i64;
    for index in indexes {
        if !is_index(index) {
            return Err(
                "image/offload: imageIndexes must be strictly increasing non-negative safe integers"
                    .to_owned(),
            );
        }
        let index =
            i64::try_from(index.as_u64().expect("validated index")).expect("safe integer fits i64");
        if index <= previous {
            return Err(
                "image/offload: imageIndexes must be strictly increasing non-negative safe integers"
                    .to_owned(),
            );
        }
        previous = index;
    }
    let clean_indexes: Vec<u64> = indexes
        .iter()
        .map(|index| index.as_u64().expect("validated index"))
        .collect();
    let seq_id = SessionSeq::new(seq).expect("validated index");
    let message = context
        .messages
        .get(&seq_id)
        .cloned()
        .or_else(|| source.and_then(source_event_data_message))
        .expect("a message-producing source always carries a message");
    let projected = offload_message_images(&message, &clean_indexes)?;
    Ok((seq_id, projected))
}

/// The message a decision targets: the projected value when one
/// exists, else the source event's own message (the dsh
/// `context.messages.get(seq) ?? source.data` shape).
fn source_event_data_message(event: &SessionEvent) -> Option<Message> {
    match &event.data {
        SessionEventData::UserMessage(message) => Some(message.clone()),
        SessionEventData::ToolResult(data) => Some(data.message.clone()),
        _ => None,
    }
}

/// Applies selected image occurrences as immutable offloaded blocks
/// (the dsh `offloadMessageImages`): identity and node unchanged,
/// selected occurrences marked.
///
/// # Errors
///
/// Returns the verbatim refusal when a selected occurrence is missing
/// or already offloaded.
pub fn offload_message_images(message: &Message, indexes: &[u64]) -> Result<Message, String> {
    let mut image_index = 0_u64;
    let mut selected = 0_usize;
    let mut content: Vec<ContentBlock> = message_content(message).to_vec();
    for block in &mut content {
        let ContentBlock::Known(KnownContentBlock::Image { offloaded, .. }) = block else {
            continue;
        };
        if selected < indexes.len() && image_index == indexes[selected] {
            if offloaded.is_some() {
                return Err(format!(
                    "image/offload: image index {image_index} is already offloaded"
                ));
            }
            *offloaded = Some(TrueOnly);
            selected += 1;
        }
        image_index += 1;
    }
    if selected != indexes.len() {
        return Err(format!(
            "image/offload: image index {} does not exist",
            indexes[selected]
        ));
    }
    with_content(message, content)
        .ok_or_else(|| "image/offload: image index 0 does not exist".to_owned())
}

/// The message's model-facing blocks.
fn message_content(message: &Message) -> &[ContentBlock] {
    match message {
        Message::Known(known) => known.content(),
        // Unknown message shapes carry no interpretable content; the
        // admission gate refuses them on the message-producing events a
        // decision targets, so the empty walk stays unreachable there.
        Message::Other(_) => &[],
    }
}

/// Rebuilds one known message over replacement content, identity and
/// source preserved (the dsh `freezeMessage({...original, content})`
/// shape the prune replacement rides).
#[must_use]
pub fn with_content(message: &Message, content: Vec<ContentBlock>) -> Option<Message> {
    let Message::Known(known) = message else {
        return None;
    };
    let rebuilt = match known.as_ref() {
        KnownMessage::System {
            id, source, extra, ..
        } => KnownMessage::System {
            id: id.clone(),
            content,
            source: source.clone(),
            extra: extra.clone(),
        },
        KnownMessage::User {
            id, source, extra, ..
        } => KnownMessage::User {
            id: id.clone(),
            content,
            source: source.clone(),
            extra: extra.clone(),
        },
        KnownMessage::Assistant {
            id, source, extra, ..
        } => KnownMessage::Assistant {
            id: id.clone(),
            content,
            source: source.clone(),
            extra: extra.clone(),
        },
        KnownMessage::Developer {
            id, source, extra, ..
        } => KnownMessage::Developer {
            id: id.clone(),
            content,
            source: source.clone(),
            extra: extra.clone(),
        },
        KnownMessage::Tool {
            id,
            source,
            tool_call_id,
            is_error,
            extra,
            ..
        } => KnownMessage::Tool {
            id: id.clone(),
            content,
            source: source.clone(),
            tool_call_id: tool_call_id.clone(),
            is_error: *is_error,
            extra: extra.clone(),
        },
    };
    Some(Message::Known(Box::new(rebuilt)))
}

#[cfg(test)]
mod tests {
    use super::{MESSAGE_PROJECTION_EVENT_TYPES, built_in_projections, is_index};
    use crate::message_projection::MessageProjection;
    use crate::surface::{fold_surface, fold_surface_with};
    use fr_wire::content::{
        ContentBlock, ImageAttachmentRef, ImageMediaType, KnownContentBlock, RawObject, TrueOnly,
    };
    use fr_wire::event::{
        AppendOp, SessionEvent, SessionEventData, SurfaceOp, ToolResultData, UnknownEventData,
    };
    use fr_wire::ids::{AttachmentId, MessageId, SessionSeq, ToolCallId};
    use fr_wire::message::{KnownMessage, KnownMessageSource, Message, MessageSource};

    fn seq(n: u64) -> SessionSeq {
        SessionSeq::new(n).expect("seq")
    }

    fn attachment(name: &str) -> ImageAttachmentRef {
        ImageAttachmentRef {
            attachment_id: AttachmentId::new(name.to_owned()),
            media_type: ImageMediaType::Png,
            bytes: 10,
            width: 1,
            height: 1,
            name: None,
            original_dimensions: None,
        }
    }

    fn image_block(name: &str, offloaded: bool) -> ContentBlock {
        ContentBlock::Known(KnownContentBlock::Image {
            attachment: attachment(name),
            offloaded: offloaded.then_some(TrueOnly),
        })
    }

    fn user_message_event(seq_value: u64, id: &str, blocks: Vec<ContentBlock>) -> SessionEvent {
        SessionEvent {
            seq: seq(seq_value),
            time: 1,
            data: SessionEventData::UserMessage(Message::Known(Box::new(KnownMessage::User {
                id: MessageId::new(id),
                content: blocks,
                source: MessageSource::Known(KnownMessageSource::User {
                    extra: RawObject::new(),
                }),
                extra: RawObject::new(),
            }))),
            ignorable: None,
            surface_op: Some(SurfaceOp::Append(AppendOp)),
            source_event_seqs: None,
        }
    }

    fn tool_result_event(seq_value: u64, call: &str, blocks: Vec<ContentBlock>) -> SessionEvent {
        SessionEvent {
            seq: seq(seq_value),
            time: 1,
            data: SessionEventData::ToolResult(ToolResultData {
                turn: 0,
                step: 0,
                message: Message::Known(Box::new(KnownMessage::Tool {
                    id: MessageId::new(format!("t-{call}")),
                    content: blocks,
                    source: MessageSource::Known(KnownMessageSource::Tool {
                        call_id: ToolCallId::new(call),
                        extra: RawObject::new(),
                    }),
                    tool_call_id: ToolCallId::new(call),
                    is_error: None,
                    extra: RawObject::new(),
                })),
                error: None,
                meta: None,
            }),
            ignorable: None,
            surface_op: Some(SurfaceOp::Append(AppendOp)),
            source_event_seqs: None,
        }
    }

    fn offload_event(seq_value: u64, data: &serde_json::Value) -> SessionEvent {
        SessionEvent {
            seq: seq(seq_value),
            time: 1,
            data: SessionEventData::Unknown(UnknownEventData {
                type_name: super::IMAGE_OFFLOAD.to_owned(),
                data: data.as_object().expect("object").clone(),
            }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }
    }

    fn decision(target_seq: u64, indexes: &[u64]) -> serde_json::Value {
        serde_json::json!({
            "targets": [{ "seq": target_seq, "imageIndexes": indexes }],
        })
    }

    fn image_user(seq_value: u64, id: &str, count: usize) -> SessionEvent {
        let blocks = (0..count)
            .map(|index| image_block(&format!("a-{id}-{index}"), false))
            .collect();
        user_message_event(seq_value, id, blocks)
    }

    fn projected_of(events: &[SessionEvent], at: u64) -> fr_wire::message::Message {
        fold_surface(events)
            .expect("fold")
            .projected_messages
            .get(&seq(at))
            .cloned()
            .expect("projected message")
    }

    fn content_images(message: &Message) -> Vec<Option<bool>> {
        let Message::Known(known) = message else {
            panic!("known message");
        };
        known
            .content()
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Known(KnownContentBlock::Image { offloaded, .. }) => {
                    Some(offloaded.map(|_| true))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn required_type_table_carries_the_one_member() {
        assert_eq!(MESSAGE_PROJECTION_EVENT_TYPES, ["image/offload"]);
    }

    #[test]
    fn offload_decision_projects_selected_marks() {
        // The decision marks exactly the selected occurrence; identity,
        // node order, and the sibling image stay untouched (the dsh
        // projection contract).
        let events = vec![image_user(0, "u1", 2), offload_event(1, &decision(0, &[1]))];
        let state = fold_surface(&events).expect("fold interprets the decision");
        assert_eq!(state.content_generation, 1, "the projection bumps content");
        assert_eq!(state.nodes, vec![seq(0)], "the decision adds no node");
        assert_eq!(
            content_images(&projected_of(&events, 0)),
            vec![None, Some(true)]
        );
    }

    #[test]
    fn projected_message_feeds_the_derive_lookup() {
        let events = vec![image_user(0, "u1", 1), offload_event(1, &decision(0, &[0]))];
        let state = fold_surface(&events).expect("fold");
        let projected =
            crate::derive_event_message_with(&events[0], Some(&state.projected_messages))
                .expect("projected message");
        assert_eq!(content_images(projected), vec![Some(true)]);
        // Without the map the raw event answers.
        let raw = crate::derive_event_message(&events[0]).expect("raw message");
        assert_eq!(content_images(raw), vec![None]);
    }

    #[test]
    fn empty_registry_refuses_the_required_type_verbatim() {
        let events = vec![image_user(0, "u1", 1), offload_event(1, &decision(0, &[0]))];
        let error = fold_surface_with(&events, &[]).expect_err("no interpreter shipped");
        assert_eq!(
            error.message,
            "session event \"image/offload\" requires a message projection; load its owning plugin or supply its projection definition"
        );
    }

    #[test]
    fn targets_on_a_second_message_project_independently() {
        // Two decisions across two messages replay deterministically; the
        // second decision sees the first projection through the context
        // messages (the already-offloaded arm becomes reachable).
        let events = vec![
            image_user(0, "u1", 1),
            image_user(1, "u2", 1),
            offload_event(2, &decision(0, &[0])),
            offload_event(3, &decision(1, &[0])),
        ];
        let state = fold_surface(&events).expect("fold");
        assert_eq!(state.projected_messages.len(), 2);
        assert_eq!(
            content_images(&state.projected_messages[&seq(0)]),
            vec![Some(true)]
        );
    }

    #[test]
    fn malformed_decisions_fail_closed_verbatim() {
        let base = vec![image_user(0, "u1", 2)];
        let case = |data: serde_json::Value, expected: &str| {
            let mut events = base.clone();
            events.push(offload_event(1, &data));
            let error = fold_surface(&events).expect_err("refusal");
            assert_eq!(error.message, expected);
        };
        case(
            serde_json::json!({ "targets": [] }),
            "image/offload: data must contain a nonempty targets array",
        );
        case(
            serde_json::json!({ "targets": [], "extra": 1 }),
            "image/offload: data must contain a nonempty targets array",
        );
        case(
            serde_json::json!({ "targets": [{ "seq": 0 }] }),
            "image/offload: each target must contain a seq and nonempty imageIndexes",
        );
        case(
            serde_json::json!({ "targets": [{ "seq": 0, "imageIndexes": [] }] }),
            "image/offload: each target must contain a seq and nonempty imageIndexes",
        );
        case(
            serde_json::json!({ "targets": [{ "seq": -1, "imageIndexes": [0] }] }),
            "image/offload: each target must contain a seq and nonempty imageIndexes",
        );
        case(
            serde_json::json!({ "targets": [
                { "seq": 0, "imageIndexes": [0] },
                { "seq": 0, "imageIndexes": [1] },
            ] }),
            "image/offload: duplicate target seq 0",
        );
        case(
            decision(5, &[0]),
            "image/offload: target seq 5 is not a current surface node",
        );
        case(
            decision(0, &[2]),
            "image/offload: image index 2 does not exist",
        );
        case(
            decision(0, &[0, 0]),
            "image/offload: imageIndexes must be strictly increasing non-negative safe integers",
        );
        case(
            decision(0, &[1, 0]),
            "image/offload: imageIndexes must be strictly increasing non-negative safe integers",
        );
    }

    #[test]
    fn double_offload_of_one_occurrence_refuses() {
        let events = vec![
            image_user(0, "u1", 1),
            offload_event(1, &decision(0, &[0])),
            offload_event(2, &decision(0, &[0])),
        ];
        let error = fold_surface(&events).expect_err("already offloaded");
        assert_eq!(
            error.message,
            "image/offload: image index 0 is already offloaded"
        );
    }

    #[test]
    fn tool_result_decisions_project_the_embedded_message() {
        let events = vec![
            tool_result_event(0, "c-1", vec![image_block("a-t1", false)]),
            offload_event(1, &decision(0, &[0])),
        ];
        let projected = projected_of(&events, 0);
        assert_eq!(content_images(&projected), vec![Some(true)]);
    }

    #[test]
    fn index_validation_mirrors_the_safe_integer_bound() {
        assert!(is_index(&serde_json::json!(0)));
        assert!(is_index(&serde_json::json!(9_007_199_254_740_991_u64)));
        assert!(!is_index(&serde_json::json!(9_007_199_254_740_992_u64)));
        assert!(!is_index(&serde_json::json!(-1)));
        assert!(!is_index(&serde_json::json!(-0.0)));
        assert!(!is_index(&serde_json::json!("0")));
    }

    #[test]
    fn wire_constants_match_the_fr_wire_event_name() {
        // The type name rides the module constant everywhere (the writer
        // and the interpreter must never drift).
        assert_eq!(super::IMAGE_OFFLOAD, "image/offload");
        assert_eq!(
            super::ImageOffloadProjection.type_name(),
            super::IMAGE_OFFLOAD
        );
        assert_eq!(built_in_projections().len(), 1);
    }

    #[test]
    fn registry_lookup_short_circuits_before_the_required_type_throw() {
        // The plan order: a registered interpreter wins even though the
        // type also sits in the required table.
        let events = vec![image_user(0, "u1", 1), offload_event(1, &decision(0, &[0]))];
        let state = fold_surface(&events).expect("the built-in wins");
        assert!(state.projected_messages.contains_key(&seq(0)));
    }
}
