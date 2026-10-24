//! The ordered surface over message-producing events: eligibility, replace-op
//! hard constraints, and the dual generation counters (dsh surface.ts).

#![forbid(unsafe_code)]
use crate::error::AdmissionError;
use fr_wire::content::{ContentBlock, KnownContentBlock};
use fr_wire::event::event_type;
use fr_wire::event::{SessionEvent, SessionEventData, SurfaceOp, ToolResultData};
use fr_wire::ids::SessionSeq;

/// One replacement operation observed while folding a session surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceReplacement {
    /// Seq of the event that replaced the prior surface range.
    pub seq: SessionSeq,
    /// Declared inclusive start seq of the replaced surface range.
    pub start: SessionSeq,
    /// Declared inclusive end seq of the replaced surface range.
    pub end: SessionSeq,
    /// Actual surface entries removed by the operation, in surface order.
    pub shadowed_seqs: Vec<SessionSeq>,
}

/// Complete result of replaying the surface operations in a session log.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SurfaceFoldResult {
    /// Current surface event sequences in model-visible order.
    pub nodes: Vec<SessionSeq>,
    /// Replacement operations in event order.
    pub replacements: Vec<SurfaceReplacement>,
    /// Monotonic count of committed positional replacements.
    pub replace_generation: u64,
    /// Monotonic count of committed replacements (message projections are
    /// not implemented; see PRD 19.2 item 14).
    pub content_generation: u64,
    /// The surface node holding the first `system/message`: established by
    /// the first append onto an empty surface, transferable only by a
    /// single-node system rewrite (PRD 19.2 item 18; mirrors the stored-side
    /// fold rule that refuses headless `system/message` on open).
    pub protected_head: Option<SessionSeq>,
}

/// Whether an event's payload produces an LLM message eligible for the surface.
#[must_use]
pub const fn is_surface_eligible(event: &SessionEvent) -> bool {
    matches!(
        event.data,
        SessionEventData::UserMessage(_)
            | SessionEventData::DeveloperMessage(_)
            | SessionEventData::SystemMessage(_)
            | SessionEventData::AssistantMessage(_)
            | SessionEventData::ToolResult(_)
    )
}

/// Validates one event's event-local surface metadata and returns its
/// operation, or `None` for a log-only event (or an unknown ignorable
/// record, which retains opaque metadata without effect).
///
/// # Errors
///
/// Returns the dsh check message when a non-surface event carries surface
/// metadata, a surface-eligible event lacks its marker, a replace range is
/// not strictly earlier, or `sourceEventSeqs` violate event-local rules.
pub fn validate_surface_metadata(
    event: &SessionEvent,
) -> Result<Option<SurfaceOp>, AdmissionError> {
    let op = surface_op_of(event)?;
    if let Some(op) = &op {
        if let SurfaceOp::Replace(replace) = op {
            let current = event.seq.get();
            if replace.start_seq.get() >= current || replace.end_seq.get() >= current {
                return Err(AdmissionError::new(format!(
                    "surface replace at seq {}: startSeq and endSeq must reference earlier events",
                    event.seq.get()
                )));
            }
        }
        assert_source_event_references(event, &[])?;
    }
    Ok(op)
}

fn surface_op_of(event: &SessionEvent) -> Result<Option<SurfaceOp>, AdmissionError> {
    if !is_surface_eligible(event) {
        if !is_known_type(event.event_type()) && event.ignorable.is_some() {
            return Ok(None);
        }
        if event.surface_op.is_some() {
            return Err(AdmissionError::new(format!(
                "session event \"{}\" is not surface-eligible and cannot carry surfaceOp",
                event.event_type()
            )));
        }
        if event.source_event_seqs.is_some() {
            return Err(AdmissionError::new(format!(
                "session event \"{}\" is not surface-eligible and cannot carry sourceEventSeqs",
                event.event_type()
            )));
        }
        return Ok(None);
    }
    event.surface_op.map_or_else(
        || {
            Err(AdmissionError::new(format!(
                "session event \"{}\" is surface-eligible and requires a surfaceOp marker",
                event.event_type()
            )))
        },
        |op| Ok(Some(op)),
    )
}

fn is_known_type(name: &str) -> bool {
    matches!(
        name,
        event_type::TURN_START
            | event_type::TURN_END
            | event_type::STEP_START
            | event_type::STEP_END
            | event_type::USER_MESSAGE
            | event_type::DEVELOPER_MESSAGE
            | event_type::SYSTEM_MESSAGE
            | event_type::ASSISTANT_MESSAGE
            | event_type::ASSISTANT_ATTEMPT
            | event_type::TOOL_CALL
            | event_type::TOOL_RESULT
            | event_type::REQUEST_HEADER
            | event_type::REQUEST_CONTEXT
            | event_type::SESSION_END_SEED
    )
}

fn assert_source_event_references(
    event: &SessionEvent,
    shadowed_seqs: &[SessionSeq],
) -> Result<(), AdmissionError> {
    if event.event_type() == event_type::ASSISTANT_MESSAGE && event.source_event_seqs.is_some() {
        return Err(AdmissionError::new(
            "assistant/message embeds its source stream and cannot carry sourceEventSeqs",
        ));
    }
    // Absent `sourceEventSeqs` behaves as an empty source set (dsh keeps the
    // coverage check unconditional), so a replace that shadows nodes without
    // citing them is refused, never silently applied.
    let sources: std::collections::HashSet<u64> = match &event.source_event_seqs {
        Some(sources) => {
            if sources.is_empty() {
                return Err(AdmissionError::new("sourceEventSeqs must not be empty"));
            }
            let mut seen = std::collections::HashSet::new();
            for source in sources {
                if !seen.insert(source.get()) {
                    return Err(AdmissionError::new(
                        "sourceEventSeqs must not contain duplicates",
                    ));
                }
                if source.get() >= event.seq.get() {
                    return Err(AdmissionError::new(format!(
                        "sourceEventSeqs must reference earlier events: {} >= current seq {}",
                        source.get(),
                        event.seq.get()
                    )));
                }
            }
            seen
        }
        None => std::collections::HashSet::new(),
    };
    let missing: Vec<String> = shadowed_seqs
        .iter()
        .filter(|seq| !sources.contains(&seq.get()))
        .map(|seq| seq.get().to_string())
        .collect();
    if !missing.is_empty() {
        return Err(AdmissionError::new(format!(
            "surface replace: sourceEventSeqs must include every shadowed surface node; missing {}",
            missing.join(", ")
        )));
    }
    Ok(())
}

/// One validated transition awaiting application to the fold state.
pub(crate) enum SurfacePlan {
    Append,
    Replace {
        start: SessionSeq,
        end: SessionSeq,
        start_idx: usize,
        end_idx: usize,
        shadowed: Vec<SessionSeq>,
    },
}

/// Plans one event against the current fold state without mutating it.
pub(crate) fn plan_event(
    state: &SurfaceFoldResult,
    event: &SessionEvent,
    events: &[SessionEvent],
) -> Result<Option<SurfacePlan>, AdmissionError> {
    assert_developer_header(event, events)?;
    let op = validate_surface_metadata(event)?;
    if event.event_type() == event_type::SYSTEM_MESSAGE
        && !state.nodes.is_empty()
        && state.protected_head.is_none()
    {
        return Err(AdmissionError::new(
            "system/message requires a protected first surface head",
        ));
    }
    let Some(SurfaceOp::Replace(replace)) = op else {
        return Ok(op.map(|_| SurfacePlan::Append));
    };
    let start_idx = state
        .nodes
        .iter()
        .position(|seq| *seq == replace.start_seq)
        .ok_or_else(|| {
            AdmissionError::new(format!(
                "surface replace: start seq {} not found in surface",
                replace.start_seq.get()
            ))
        })?;
    let end_idx = state
        .nodes
        .iter()
        .position(|seq| *seq == replace.end_seq)
        .ok_or_else(|| {
            AdmissionError::new(format!(
                "surface replace: end seq {} not found in surface",
                replace.end_seq.get()
            ))
        })?;
    if start_idx > end_idx {
        return Err(AdmissionError::new(format!(
            "surface replace: start seq {} (index {start_idx}) is after end seq {} (index {end_idx})",
            replace.start_seq.get(),
            replace.end_seq.get()
        )));
    }
    let shadowed = state.nodes[start_idx..=end_idx].to_vec();
    assert_source_event_references(event, &shadowed)?;
    assert_tool_result_rewrite(event, &shadowed, events)?;
    assert_system_head_rewrite(event, state, start_idx, &shadowed, events)?;
    Ok(Some(SurfacePlan::Replace {
        start: replace.start_seq,
        end: replace.end_seq,
        start_idx,
        end_idx,
        shadowed,
    }))
}

/// Applies one plan to the fold state, recording a replacement when one occurred.
pub(crate) fn apply_plan(
    state: &mut SurfaceFoldResult,
    event: &SessionEvent,
    plan: Option<SurfacePlan>,
) -> Option<SurfaceReplacement> {
    match plan {
        Some(SurfacePlan::Append) => {
            if event.event_type() == event_type::SYSTEM_MESSAGE && state.nodes.is_empty() {
                state.protected_head = Some(event.seq);
            }
            state.nodes.push(event.seq);
        }
        Some(SurfacePlan::Replace {
            start,
            end,
            start_idx,
            end_idx,
            shadowed,
        }) => {
            // The plan stage has already refused any protected-head rewrite
            // other than a single-node system replacement, so containment
            // alone decides the transfer.
            if state
                .protected_head
                .is_some_and(|head| shadowed.contains(&head))
            {
                state.protected_head = Some(event.seq);
            }
            state
                .nodes
                .splice(start_idx..=end_idx, std::iter::once(event.seq));
            state.replace_generation += 1;
            state.content_generation += 1;
            return Some(SurfaceReplacement {
                seq: event.seq,
                start,
                end,
                shadowed_seqs: shadowed,
            });
        }
        None => {}
    }
    None
}

/// Resolves tool additions against their immutable historical request header
/// (dsh `assertDeveloperHeader`).
fn assert_developer_header(
    event: &SessionEvent,
    events: &[SessionEvent],
) -> Result<(), AdmissionError> {
    let SessionEventData::DeveloperMessage(data) = &event.data else {
        return Ok(());
    };
    let Some(header_seq) = data.header_seq else {
        return Ok(());
    };
    let header_event = if header_seq.get() < event.seq.get() {
        events.get(usize::try_from(header_seq.get()).unwrap_or(usize::MAX))
    } else {
        None
    };
    let Some(SessionEventData::RequestHeader(header_data)) =
        header_event.map(|candidate| &candidate.data)
    else {
        return Err(AdmissionError::new(
            "developer/message headerSeq must reference an earlier request/header",
        ));
    };
    let message_blocks: &[ContentBlock] = match &data.message {
        fr_wire::message::Message::Known(known) => known.content(),
        fr_wire::message::Message::Other(_) => {
            return Err(AdmissionError::new(
                "session event developer/message and developer role must occur together",
            ));
        }
    };
    for block in message_blocks {
        let ContentBlock::Known(KnownContentBlock::ToolAddition { tool_name }) = block else {
            continue;
        };
        let matches = header_data
            .header
            .tools
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|tool| tool.name == *tool_name)
            .count();
        if matches != 1 {
            return Err(AdmissionError::new(format!(
                "developer/message tool-addition \"{tool_name}\" must name exactly one tool in headerSeq {}",
                header_seq.get()
            )));
        }
    }
    Ok(())
}

/// Restricts a tool-result replacement to changing one current result's
/// content only (dsh compares both sides with `message.content` nulled).
fn assert_tool_result_rewrite(
    event: &SessionEvent,
    shadowed_seqs: &[SessionSeq],
    events: &[SessionEvent],
) -> Result<(), AdmissionError> {
    let SessionEventData::ToolResult(replacement) = &event.data else {
        return Ok(());
    };
    if shadowed_seqs.len() != 1 {
        return Err(AdmissionError::new(
            "tool/result surface replacement must rewrite exactly one current node",
        ));
    }
    for original_seq in shadowed_seqs {
        let original = events.get(usize::try_from(original_seq.get()).unwrap_or(usize::MAX));
        let original_data = original.and_then(|candidate| match &candidate.data {
            SessionEventData::ToolResult(data) => Some(data),
            _ => None,
        });
        let Some(original_data) = original_data else {
            return Err(AdmissionError::new(
                "tool/result surface replacement must target a current tool/result",
            ));
        };
        if comparable_tool_result(original_data) != comparable_tool_result(replacement) {
            return Err(AdmissionError::new(
                "tool/result surface replacement may change only content",
            ));
        }
    }
    Ok(())
}

/// Projects one tool-result payload to its content-insensitive comparison
/// form (every field with `message.content` nulled).
fn comparable_tool_result(data: &ToolResultData) -> serde_json::Value {
    let mut value = serde_json::to_value(data).unwrap_or_else(|_| serde_json::Value::Null);
    if let Some(message) = value.get_mut("message")
        && let Some(message_object) = message.as_object_mut()
    {
        message_object.insert("content".to_owned(), serde_json::Value::Null);
    }
    value
}

/// Protects the system prompt at surface node 0: a replacement covering the
/// head while it is a `system/message` must itself be a `system/message`
/// over exactly that node.
fn assert_system_head_rewrite(
    event: &SessionEvent,
    state: &SurfaceFoldResult,
    start_idx: usize,
    shadowed_seqs: &[SessionSeq],
    events: &[SessionEvent],
) -> Result<(), AdmissionError> {
    if start_idx != 0 {
        return Ok(());
    }
    let head_seq = state.nodes[0];
    let head_is_system = events
        .get(usize::try_from(head_seq.get()).unwrap_or(usize::MAX))
        .is_some_and(|head| head.event_type() == event_type::SYSTEM_MESSAGE);
    if !head_is_system {
        return Ok(());
    }
    if event.event_type() != event_type::SYSTEM_MESSAGE || shadowed_seqs.len() != 1 {
        return Err(AdmissionError::new(
            "surface replace: node 0 holds the system prompt and may be rewritten only by a system/message over exactly that node",
        ));
    }
    Ok(())
}

/// Replays a complete session log through the canonical surface fold.
///
/// # Errors
///
/// Returns the owning check's message when an event violates surface
/// metadata, source attribution, replacement, developer-header, or
/// message-admission rules.
pub fn fold_surface(events: &[SessionEvent]) -> Result<SurfaceFoldResult, AdmissionError> {
    let mut state = SurfaceFoldResult::default();
    for (index, event) in events.iter().enumerate() {
        if event.seq.get() != index as u64 {
            return Err(AdmissionError::new(format!(
                "session event seq {} is not contiguous; expected {index}",
                event.seq.get()
            )));
        }
        let plan = plan_event(&state, event, events)?;
        if let Some(replacement) = apply_plan(&mut state, event, plan) {
            state.replacements.push(replacement);
        }
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests_support::{developer_addition, header_tool};
    use fr_wire::content::{RawObject, TrueOnly};
    use fr_wire::event::{
        AppendOp, AssistantMessageData, ReplaceOp, ReplaceTag, ToolResultError, TurnStartData,
        UnknownEventData,
    };
    use fr_wire::ids::{MessageId, ToolCallId};
    use fr_wire::message::{KnownMessage, KnownMessageSource, Message, MessageSource};

    fn seq(n: u64) -> SessionSeq {
        SessionSeq::new(n).unwrap_or_else(|e| panic!("seq {n} rejected: {e}"))
    }

    fn user_text(id: &str, text: &str) -> Message {
        Message::Known(Box::new(KnownMessage::User {
            id: MessageId::new(id),
            content: vec![ContentBlock::Known(KnownContentBlock::Text {
                text: text.to_owned(),
            })],
            source: MessageSource::Known(KnownMessageSource::User {
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        }))
    }

    fn empty_event(n: u64) -> SessionEvent {
        SessionEvent {
            seq: seq(n),
            time: 1,
            data: SessionEventData::TurnStart(TurnStartData { turn: n }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }
    }

    fn append_user(n: u64, text: &str) -> SessionEvent {
        SessionEvent {
            seq: seq(n),
            time: 1,
            data: SessionEventData::UserMessage(user_text(&format!("m-{n}"), text)),
            ignorable: None,
            surface_op: Some(SurfaceOp::Append(AppendOp)),
            source_event_seqs: None,
        }
    }

    fn tool_result(n: u64, call_id: &str, text: &str) -> SessionEvent {
        SessionEvent {
            seq: seq(n),
            time: 1,
            data: SessionEventData::ToolResult(ToolResultData {
                turn: 0,
                step: 0,
                message: Message::Known(Box::new(KnownMessage::Tool {
                    id: MessageId::new(format!("t-{n}")),
                    content: vec![ContentBlock::Known(KnownContentBlock::Text {
                        text: text.to_owned(),
                    })],
                    source: MessageSource::Known(KnownMessageSource::Tool {
                        call_id: ToolCallId::new(call_id),
                        extra: RawObject::new(),
                    }),
                    tool_call_id: ToolCallId::new(call_id),
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

    #[test]
    fn fold_appends_surface_events_and_counts_generations() {
        let events = vec![
            header_tool(0, "bash"),
            append_user(1, "hello"),
            developer_addition(2, 0, "bash"),
            tool_result(3, "c-1", "done"),
            empty_event(4),
        ];
        let result = fold_surface(&events).unwrap_or_else(|e| panic!("fold: {}", e.message));
        assert_eq!(result.nodes, vec![seq(1), seq(2), seq(3)]);
        assert_eq!(result.replace_generation, 0);
        assert_eq!(result.content_generation, 0);
        assert_eq!(result.replacements, Vec::new());
    }

    #[test]
    fn replace_shadowed_range_updates_nodes_and_both_generations() {
        let mut replacement = append_user(2, "compacted");
        replacement.surface_op = Some(SurfaceOp::Replace(ReplaceOp {
            op: ReplaceTag,
            start_seq: seq(0),
            end_seq: seq(1),
        }));
        replacement.source_event_seqs = Some(vec![seq(0), seq(1)]);
        let events = vec![append_user(0, "a"), append_user(1, "b"), replacement];
        let result = fold_surface(&events).unwrap_or_else(|e| panic!("fold: {}", e.message));
        assert_eq!(result.nodes, vec![seq(2)]);
        assert_eq!(result.replace_generation, 1);
        assert_eq!(result.content_generation, 1);
        assert_eq!(result.replacements.len(), 1);
        assert_eq!(result.replacements[0].start, seq(0));
        assert_eq!(result.replacements[0].shadowed_seqs, vec![seq(0), seq(1)]);
    }

    #[test]
    fn illegal_replace_is_rejected() {
        // Both refusals the illegal-replace scenario names: a replacement
        // must cite every shadowed surface node, and assistant messages
        // embed their stream so they never carry source seqs.
        let mut replacement = tool_result(2, "c-1", "compacted");
        replacement.surface_op = Some(SurfaceOp::Replace(ReplaceOp {
            op: ReplaceTag,
            start_seq: seq(0),
            end_seq: seq(1),
        }));
        replacement.source_event_seqs = Some(vec![seq(0)]);
        let events = vec![append_user(0, "a"), append_user(1, "b"), replacement];
        let err = fold_surface(&events).expect_err("coverage");
        assert!(
            err.message
                .contains("must include every shadowed surface node")
        );

        let mut assistant = append_user(0, "x");
        assistant.data = SessionEventData::AssistantMessage(AssistantMessageData {
            turn: 0,
            step: 0,
            message: user_text("a-1", "x"),
            stream: vec![],
            usage: None,
            interrupted: None,
        });
        assistant.source_event_seqs = Some(vec![seq(0)]);
        let events = vec![assistant];
        let err = fold_surface(&events).expect_err("assistant refs");
        assert_eq!(
            err.message,
            "assistant/message embeds its source stream and cannot carry sourceEventSeqs"
        );
    }

    #[test]
    fn duplicate_source_seqs_are_rejected() {
        let mut event = tool_result(2, "c-1", "x");
        event.source_event_seqs = Some(vec![seq(0), seq(0)]);
        let events = vec![append_user(0, "a"), append_user(1, "b"), event];
        let err = fold_surface(&events).expect_err("dups");
        assert!(err.message.contains("must not contain duplicates"));
    }

    #[test]
    fn unknown_ignorable_event_retains_metadata_without_effect() {
        let mut event = SessionEvent {
            seq: seq(2),
            time: 1,
            data: SessionEventData::Unknown(UnknownEventData {
                type_name: "plugin:example/state".to_owned(),
                data: RawObject::new(),
            }),
            ignorable: Some(TrueOnly),
            surface_op: Some(SurfaceOp::Append(AppendOp)),
            source_event_seqs: Some(vec![seq(0)]),
        };
        event.source_event_seqs = None;
        event.surface_op = None;
        let events = vec![append_user(0, "a"), append_user(1, "b"), event];
        let result = fold_surface(&events).unwrap_or_else(|e| panic!("fold: {}", e.message));
        assert_eq!(result.nodes.len(), 2);
    }

    #[test]
    fn node_zero_system_head_rejects_non_system_rewrite() {
        let mut system_head = append_user(0, "prompt");
        system_head.data = SessionEventData::SystemMessage(fr_wire::event::SystemMessageData {
            turn: 0,
            step: 0,
            message: user_text("s-0", "prompt"),
        });
        let mut rewrite = append_user(2, "rewrite");
        rewrite.surface_op = Some(SurfaceOp::Replace(ReplaceOp {
            op: ReplaceTag,
            start_seq: seq(0),
            end_seq: seq(1),
        }));
        rewrite.source_event_seqs = Some(vec![seq(0), seq(1)]);
        let events = vec![system_head, append_user(1, "hi"), rewrite];
        let err = fold_surface(&events).expect_err("system head");
        assert!(err.message.contains("node 0 holds the system prompt"));
    }

    fn system_append(n: u64) -> SessionEvent {
        let mut event = append_user(n, "prompt");
        event.data = SessionEventData::SystemMessage(fr_wire::event::SystemMessageData {
            turn: 1,
            step: 1,
            message: user_text(&format!("s-{n}"), "prompt"),
        });
        event
    }

    #[test]
    fn first_system_message_becomes_protected_head() {
        let events = vec![system_append(0), append_user(1, "hi")];
        let result = fold_surface(&events).unwrap_or_else(|e| panic!("fold: {}", e.message));
        assert_eq!(result.protected_head, Some(seq(0)));
        assert_eq!(result.nodes, vec![seq(0), seq(1)]);
    }

    #[test]
    fn protected_head_transfers_on_single_node_rewrite() {
        let mut rewrite = system_append(1);
        rewrite.surface_op = Some(SurfaceOp::Replace(ReplaceOp {
            op: ReplaceTag,
            start_seq: seq(0),
            end_seq: seq(0),
        }));
        rewrite.source_event_seqs = Some(vec![seq(0)]);
        let events = vec![system_append(0), rewrite];
        let result = fold_surface(&events).unwrap_or_else(|e| panic!("fold: {}", e.message));
        assert_eq!(result.protected_head, Some(seq(1)));
        assert_eq!(result.nodes, vec![seq(1)]);
    }

    #[test]
    fn tool_result_rewrite_may_change_only_content() {
        let original = tool_result(2, "c-1", "raw output");
        let mut rewritten = tool_result(3, "c-1", "raw output");
        let SessionEventData::ToolResult(data) = &mut rewritten.data else {
            panic!("rewritten must be a tool result");
        };
        if let Message::Known(known) = &mut data.message
            && let KnownMessage::Tool { id, .. } = known.as_mut()
        {
            *id = MessageId::new("t-2");
        }
        rewritten.surface_op = Some(SurfaceOp::Replace(ReplaceOp {
            op: ReplaceTag,
            start_seq: seq(2),
            end_seq: seq(2),
        }));
        rewritten.source_event_seqs = Some(vec![seq(2)]);
        let events = vec![
            append_user(0, "q"),
            append_user(1, "more"),
            original,
            rewritten,
        ];
        let result = fold_surface(&events).unwrap_or_else(|e| panic!("fold: {}", e.message));
        assert_eq!(result.replace_generation, 1);
    }

    #[test]
    fn tool_result_rewrite_with_changed_error_is_rejected() {
        let original = tool_result(2, "c-1", "raw output");
        let mut rewritten = tool_result(3, "c-1", "raw output");
        let SessionEventData::ToolResult(data) = &mut rewritten.data else {
            panic!("rewritten must be a tool result");
        };
        {
            if let Message::Known(known) = &mut data.message
                && let KnownMessage::Tool { id, .. } = known.as_mut()
            {
                *id = MessageId::new("t-2");
            }
            data.error = Some(ToolResultError {
                name: "X".to_owned(),
                code: "X".to_owned(),
                reason: None,
            });
        }
        rewritten.surface_op = Some(SurfaceOp::Replace(ReplaceOp {
            op: ReplaceTag,
            start_seq: seq(2),
            end_seq: seq(2),
        }));
        rewritten.source_event_seqs = Some(vec![seq(2)]);
        let events = vec![
            append_user(0, "q"),
            append_user(1, "more"),
            original,
            rewritten,
        ];
        let err = fold_surface(&events).expect_err("changed error");
        assert_eq!(
            err.message,
            "tool/result surface replacement may change only content"
        );
    }

    #[test]
    fn developer_header_seq_must_resolve_exactly_one_tool() {
        let events = vec![header_tool(0, "bash"), developer_addition(1, 0, "missing")];
        let err = fold_surface(&events).expect_err("missing tool");
        assert!(
            err.message
                .contains("must name exactly one tool in headerSeq 0")
        );
    }

    #[test]
    fn header_seq_required_exactly_when_additions_present() {
        let mut event = developer_addition(1, 0, "bash");
        if let SessionEventData::DeveloperMessage(data) = &mut event.data {
            data.header_seq = None;
        }
        let err = crate::admission::validate_event_data(&event).expect_err("header seq required");
        assert!(
            err.message
                .contains("headerSeq exactly when tool additions are present")
        );
    }

    #[test]
    fn eligible_event_without_marker_is_rejected() {
        let events = vec![append_user(0, "a"), {
            let mut bare = tool_result(1, "c-1", "x");
            bare.surface_op = None;
            bare
        }];
        let err = fold_surface(&events).expect_err("marker");
        assert_eq!(
            err.message,
            "session event \"tool/result\" is surface-eligible and requires a surfaceOp marker"
        );
    }

    #[test]
    fn contiguity_gap_is_rejected() {
        let mut gap = append_user(2, "b");
        gap.seq = seq(5);
        let events = vec![append_user(0, "a"), gap];
        let err = fold_surface(&events).expect_err("contiguity");
        assert!(err.message.contains("is not contiguous"));
    }

    #[test]
    fn replace_bounds_not_earlier_are_rejected() {
        let mut replacement = tool_result(1, "c-1", "x");
        replacement.surface_op = Some(SurfaceOp::Replace(ReplaceOp {
            op: ReplaceTag,
            start_seq: seq(1),
            end_seq: seq(1),
        }));
        replacement.source_event_seqs = Some(vec![seq(0)]);
        let events = vec![append_user(0, "a"), replacement];
        let err = fold_surface(&events).expect_err("bounds");
        assert!(err.message.contains("must reference earlier events"));
    }
}
