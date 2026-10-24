//! Fork seed construction over an exact source-event prefix (PRD 7.1
//! session/fork; dsh fork.ts).

#![forbid(unsafe_code)]
use fr_wire::content::TrueOnly;
use fr_wire::event::{EndSeedData, SessionEvent, SessionEventData};
use fr_wire::ids::SessionSeq;
use fr_wire::ids::SessionSeqRangeError;

use crate::repair::{OpenTurnCloseCause, open_turn_closers};

/// Copies an inclusive event prefix, marks its inherited cut, and closes
/// its open tail with forked results and step/turn endings. Closed steps
/// and turns are preserved unchanged.
///
/// The caller validates that the boundary is an existing contiguous event
/// seq; Session construction snapshots the borrowed events before
/// publication.
///
/// The returned seed's inherited cut is the tagged `session/end-seed` seq
/// (`boundary + 1`): the seed retains `boundary + 1` inherited source events
/// and the synthetic closers land outside the counted prefix.
///
/// # Errors
///
/// Returns [`SessionSeqRangeError`] when
/// `boundary` is out of range or a synthesized sequence leaves the
/// safe-integer domain (dsh throws from its brand constructors at the same
/// boundaries).
pub fn build_fork_seed(
    events: &[SessionEvent],
    boundary: u64,
) -> Result<Vec<SessionEvent>, SessionSeqRangeError> {
    if boundary >= events.len() as u64 {
        return Err(fr_wire::ids::SessionSeqRangeError(boundary));
    }
    let boundary_index = usize::try_from(boundary).unwrap_or(usize::MAX);
    let mut prefix: Vec<SessionEvent> = events.get(..=boundary_index).unwrap_or(&[]).to_vec();
    if prefix.len() != boundary_index + 1 {
        return Err(SessionSeqRangeError(boundary));
    }
    let cut_time = events[boundary_index].time;
    prefix.push(SessionEvent {
        seq: SessionSeq::new(boundary + 1)?,
        // The caller validated this exact source event; the cut reuses its
        // timestamp so the seed stays deterministic.
        time: cut_time,
        data: SessionEventData::SessionEndSeed(EndSeedData {
            inherited: Some(TrueOnly),
        }),
        ignorable: None,
        surface_op: None,
        source_event_seqs: None,
    });
    let closers = open_turn_closers(&prefix, OpenTurnCloseCause::Forked)?;
    prefix.extend(closers);
    Ok(prefix)
}

#[cfg(test)]
mod tests {
    use fr_wire::content::RawObject;
    use fr_wire::event::{
        SessionEvent, SessionEventData, StepBoundsData, TurnEndReason, TurnEndReasonKind,
        TurnStartData,
    };
    use fr_wire::ids::{MessageId, SessionSeq, SessionSeqRangeError, ToolCallId};
    use fr_wire::message::{KnownMessage, KnownMessageSource, Message, MessageSource};

    use super::build_fork_seed;

    fn turn_start(seq: u64, turn: u64) -> SessionEvent {
        SessionEvent {
            seq: SessionSeq::new(seq).expect("seq"),
            time: 1_000 + i64::try_from(seq).expect("seq fits i64"),
            data: SessionEventData::TurnStart(TurnStartData { turn }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }
    }

    fn step_start(seq: u64, turn: u64, step: u64) -> SessionEvent {
        SessionEvent {
            seq: SessionSeq::new(seq).expect("seq"),
            time: 1_000 + i64::try_from(seq).expect("seq fits i64"),
            data: SessionEventData::StepStart(StepBoundsData { turn, step }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }
    }

    fn assistant_with_call(seq: u64, turn: u64, step: u64, call_id: &str) -> SessionEvent {
        use fr_wire::content::{ContentBlock, KnownContentBlock};
        use fr_wire::event::{AppendOp, AssistantMessageData, SurfaceOp};
        SessionEvent {
            seq: SessionSeq::new(seq).expect("seq"),
            time: 1_000 + i64::try_from(seq).expect("seq fits i64"),
            data: SessionEventData::AssistantMessage(AssistantMessageData {
                turn,
                step,
                message: Message::Known(Box::new(KnownMessage::Assistant {
                    id: MessageId::new(format!("assistant-{seq}")),
                    content: vec![ContentBlock::Known(KnownContentBlock::ToolCall {
                        id: ToolCallId::new(call_id),
                        name: "bash".to_owned(),
                        arguments: "{}".to_owned(),
                    })],
                    source: MessageSource::Known(KnownMessageSource::Model {
                        provider: "deepseek-official".to_owned(),
                        model: "deepseek-chat".to_owned(),
                        replay_state: None,
                        extra: RawObject::new(),
                    }),
                    extra: RawObject::new(),
                })),
                stream: Vec::new(),
                usage: None,
                interrupted: None,
            }),
            ignorable: None,
            surface_op: Some(SurfaceOp::Append(AppendOp)),
            source_event_seqs: None,
        }
    }

    #[test]
    fn fork_seed_marks_cut_and_closes_open_turn_with_forked_closers() {
        use crate::closer_text::{FORKED_NOT_STARTED, TOOL_NOT_STARTED};
        use fr_wire::content::{ContentBlock, KnownContentBlock, TrueOnly};
        let source = vec![
            turn_start(0, 1),
            step_start(1, 1, 1),
            assistant_with_call(2, 1, 1, "call-a"),
        ];
        let seed = build_fork_seed(&source, 2).expect("seed");
        // Three copied prefix events + tagged end-seed + not-started closer +
        // step/end + turn/end.
        assert_eq!(seed.len(), 3 + 1 + 3);
        let SessionEventData::SessionEndSeed(marker) = &seed[3].data else {
            panic!("cut marker at boundary+1");
        };
        assert!(marker.inherited.is_some());
        assert_eq!(seed[3].seq.get(), 3);
        // The cut reuses the boundary event's time.
        assert_eq!(seed[3].time, source[2].time);
        // Closers land outside the inherited prefix with continued seqs.
        let SessionEventData::ToolResult(result) = &seed[4].data else {
            panic!("not-started closer first");
        };
        assert_eq!(seed[4].seq.get(), 4);
        assert_eq!(result.turn, 1);
        assert_eq!(result.step, 1);
        // The synthetic result carries the extracted forked not-started
        // wording verbatim (dsh repair.ts CLOSER_TEXT).
        let Message::Known(known) = &result.message else {
            panic!("tool-role message");
        };
        let KnownMessage::Tool {
            content, is_error, ..
        } = known.as_ref()
        else {
            panic!("tool-role message");
        };
        let Some(ContentBlock::Known(KnownContentBlock::Text { text })) = content.first() else {
            panic!("one text block");
        };
        assert_eq!(text.as_str(), FORKED_NOT_STARTED);
        assert_eq!(
            result.error.as_ref().map(|error| error.code.as_str()),
            Some(TOOL_NOT_STARTED)
        );
        assert_eq!(*is_error, Some(TrueOnly));
        let SessionEventData::StepEnd(bounds) = &seed[5].data else {
            panic!("step/end second");
        };
        assert_eq!((bounds.turn, bounds.step), (1, 1));
        let SessionEventData::TurnEnd(end) = &seed[6].data else {
            panic!("turn/end third");
        };
        assert_eq!(end.reason, TurnEndReason::Known(TurnEndReasonKind::Forked));
        // The inherited cut is the end-seed seq: boundary + 1.
        assert_eq!(seed[3].seq.get(), 2 + 1);
    }

    #[test]
    fn closed_tail_prefix_stays_unchanged() {
        use fr_wire::event::TurnEndData;
        let source = vec![
            turn_start(0, 1),
            SessionEvent {
                seq: SessionSeq::new(1).expect("seq"),
                time: 1_001,
                data: SessionEventData::TurnEnd(TurnEndData {
                    turn: 1,
                    reason: TurnEndReason::Known(TurnEndReasonKind::Completed),
                }),
                ignorable: None,
                surface_op: None,
                source_event_seqs: None,
            },
        ];
        // Cutting at seq 0 leaves turn 1 open in the prefix, so the seed
        // closes it with a forked turn/end after the marker.
        let seed = build_fork_seed(&source, 0).expect("seed");
        assert_eq!(seed.len(), 3);
        let SessionEventData::SessionEndSeed(marker) = &seed[1].data else {
            panic!("cut marker");
        };
        assert!(marker.inherited.is_some());
        assert_eq!(seed[1].seq.get(), 1);
        assert_eq!(seed[1].time, source[0].time);
        let SessionEventData::TurnEnd(end) = &seed[2].data else {
            panic!("forked closer");
        };
        assert_eq!(end.turn, 1);
        assert_eq!(end.reason, TurnEndReason::Known(TurnEndReasonKind::Forked));
    }

    #[test]
    fn out_of_range_boundary_is_refused() {
        let source = vec![turn_start(0, 1)];
        let error = build_fork_seed(&source, 1).expect_err("boundary out of range");
        assert_eq!(error, SessionSeqRangeError(1));
    }
}
