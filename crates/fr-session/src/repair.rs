//! Pending tool-result recovery shared by failed live steps, interrupted
//! logs, and fork seeds (PRD 7.1 abort compensation; dsh repair.ts).
//!
//! Tail repair preserves closed steps and supplies only missing tool results
//! and lifecycle boundaries, with cause-specific model-visible wording taken
//! from the extracted [`crate::closer_text`] constants.

#![forbid(unsafe_code)]
use indexmap::IndexMap;

use fr_wire::content::{ContentBlock, KnownContentBlock, RawObject, TrueOnly};
use fr_wire::event::{
    AppendOp, AssistantMessageData, SessionEvent, SessionEventData, StepBoundsData, SurfaceOp,
    ToolResultData, ToolResultError, TurnEndData, TurnEndReason, TurnEndReasonKind,
};
use fr_wire::ids::{MessageId, SessionSeq, SessionSeqRangeError, ToolCallId};
use fr_wire::message::{KnownMessage, KnownMessageSource, Message, MessageSource};

use crate::closer_text::{
    FORKED_NOT_STARTED, FORKED_STARTED, INTERRUPTED_NOT_STARTED, INTERRUPTED_STARTED,
    TOOL_NOT_STARTED, TOOL_OUTCOME_UNKNOWN,
};

/// Why an open tail turn is closed with synthetic events: `Interrupted` is
/// crash recovery over a persisted log; `Forked` is a fork seed cut inside
/// the source's open turn.
///
/// The cause selects the synthetic `turn/end`
/// reason, the model-visible wording of synthetic error tool results, and
/// the deterministic synthetic message-id prefix. The error codes
/// ([`TOOL_NOT_STARTED`] / [`TOOL_OUTCOME_UNKNOWN`]) are shared: both causes
/// state the same fact about the call's recorded lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenTurnCloseCause {
    /// Crash recovery over a persisted log.
    Interrupted,
    /// Fork-seed construction over an exact source prefix.
    Forked,
}

impl OpenTurnCloseCause {
    /// The synthetic message-id prefix (`<cause>-tool-result-...`).
    #[must_use]
    pub const fn id_prefix(self) -> &'static str {
        match self {
            Self::Interrupted => "interrupted",
            Self::Forked => "forked",
        }
    }

    /// The model-visible wording pair for this cause: `(started, notStarted)`.
    #[must_use]
    pub const fn closer_text(self) -> (&'static str, &'static str) {
        match self {
            Self::Interrupted => (INTERRUPTED_STARTED, INTERRUPTED_NOT_STARTED),
            Self::Forked => (FORKED_STARTED, FORKED_NOT_STARTED),
        }
    }

    /// The `turn/end` reason kind this cause closes with.
    #[must_use]
    pub const fn turn_end_kind(self) -> TurnEndReasonKind {
        match self {
            Self::Interrupted => TurnEndReasonKind::Interrupted,
            Self::Forked => TurnEndReasonKind::Forked,
        }
    }
}

/// One unanswered assistant tool request.
#[derive(Debug, Clone)]
struct PendingCall {
    turn: u64,
    step: u64,
    call_seq: Option<SessionSeq>,
}

/// Track unanswered assistant tool requests from one Session's committed
/// events. Observe from the start of the owned step or replay prefix, and
/// recover before its step closes.
///
/// This state retains pending identities, not event history.
#[derive(Debug, Clone)]
pub struct ToolCallRecovery {
    cause: OpenTurnCloseCause,
    pending_calls: IndexMap<ToolCallId, PendingCall>,
    last: Option<(SessionSeq, i64)>,
}

impl ToolCallRecovery {
    /// Creates a recovery tracker; `cause` defaults to interrupted
    /// live/crash recovery, and fork-seed construction supplies its own.
    #[must_use]
    pub fn new(cause: OpenTurnCloseCause) -> Self {
        Self {
            cause,
            pending_calls: IndexMap::new(),
            last: None,
        }
    }

    /// Consumes the next committed event; closed steps and turn boundaries
    /// discard pending requests.
    pub fn observe(&mut self, event: &SessionEvent) {
        self.last = Some((event.seq, event.time));
        match &event.data {
            SessionEventData::TurnStart(_)
            | SessionEventData::TurnEnd(_)
            | SessionEventData::StepEnd(_) => self.pending_calls.clear(),
            SessionEventData::AssistantMessage(data) => {
                register_advertised_calls(data, &mut self.pending_calls);
            }
            SessionEventData::ToolCall(call) => {
                if let Some(entry) = self.pending_calls.get_mut(&call.call_id) {
                    entry.call_seq = Some(event.seq);
                }
            }
            SessionEventData::ToolResult(result) => {
                let Some(call_id) = result_source_call_id(result) else {
                    return;
                };
                if let Some(entry) = self.pending_calls.get(&call_id) {
                    let appended = matches!(event.surface_op, Some(SurfaceOp::Append(_)));
                    if appended && entry.turn == result.turn && entry.step == result.step {
                        self.pending_calls.swap_remove(&call_id);
                    }
                }
            }
            // SessionEvent is merge-extensible; unrelated events retain
            // pending requests.
            _ => {}
        }
    }

    /// Builds conservative error results in assistant order without changing
    /// tracked state. Sequences follow the latest observed event and
    /// timestamps reuse its time. Callers commit the results and observe
    /// those commits before recovering again.
    ///
    /// # Errors
    ///
    /// Returns [`SessionSeqRangeError`] when the synthesized sequence would
    /// leave the safe-integer domain (dsh throws from its `SessionSeq`
    /// brand constructor at the same boundary).
    pub fn results(&self) -> Result<Vec<SessionEvent>, SessionSeqRangeError> {
        let Some((last_seq, time)) = self.last else {
            return Ok(Vec::new());
        };
        let (started_text, not_started_text) = self.cause.closer_text();
        let mut seq = last_seq.get();
        let mut results = Vec::with_capacity(self.pending_calls.len());
        // Close calls before their step: providers reject dangling assistant
        // calls, and insertion order preserves their transcript order.
        for (call_id, pending) in &self.pending_calls {
            seq += 1;
            let started = pending.call_seq.is_some();
            let text = if started {
                started_text
            } else {
                not_started_text
            };
            let message = Message::Known(KnownMessage::Tool {
                id: MessageId::new(format!(
                    "{}-tool-result-{}-{seq}",
                    self.cause.id_prefix(),
                    call_id.as_str()
                )),
                content: vec![ContentBlock::Known(KnownContentBlock::Text {
                    text: text.to_owned(),
                })],
                source: MessageSource::Known(KnownMessageSource::Tool {
                    call_id: call_id.clone(),
                    extra: RawObject::new(),
                }),
                tool_call_id: call_id.clone(),
                is_error: Some(TrueOnly),
            });
            results.push(SessionEvent {
                seq: SessionSeq::new(seq)?,
                time,
                data: SessionEventData::ToolResult(ToolResultData {
                    turn: pending.turn,
                    step: pending.step,
                    message,
                    error: Some(ToolResultError {
                        name: error_name(started).to_owned(),
                        code: error_code(started).to_owned(),
                        reason: None,
                    }),
                    meta: None,
                }),
                ignorable: None,
                surface_op: Some(SurfaceOp::Append(AppendOp)),
                source_event_seqs: pending.call_seq.map(|call_seq| vec![call_seq]),
            });
        }
        Ok(results)
    }
}

const fn error_name(started: bool) -> &'static str {
    if started {
        "ToolOutcomeUnknownError"
    } else {
        "ToolNotStartedError"
    }
}

const fn error_code(started: bool) -> &'static str {
    if started {
        TOOL_OUTCOME_UNKNOWN
    } else {
        TOOL_NOT_STARTED
    }
}

/// Registers every advertised tool-call block of one assistant message.
fn register_advertised_calls(
    data: &AssistantMessageData,
    pending: &mut IndexMap<ToolCallId, PendingCall>,
) {
    let Message::Known(KnownMessage::Assistant { content, .. }) = &data.message else {
        return;
    };
    for block in content {
        let ContentBlock::Known(KnownContentBlock::ToolCall { id, .. }) = block else {
            continue;
        };
        pending.insert(
            id.clone(),
            PendingCall {
                turn: data.turn,
                step: data.step,
                call_seq: None,
            },
        );
    }
}

/// The tool-source call id of one tool result message, when present.
fn result_source_call_id(result: &ToolResultData) -> Option<ToolCallId> {
    let Message::Known(KnownMessage::Tool {
        source: MessageSource::Known(KnownMessageSource::Tool { call_id, .. }),
        ..
    }) = &result.message
    else {
        return None;
    };
    Some(call_id.clone())
}

/// Returns deterministic synthetic events that close an open tail turn.
///
/// Unmatched calls receive error results first, followed by an open
/// `step/end` and a cause-specific `turn/end`; sequences continue the log
/// and timestamps reuse the last real event. A balanced or empty log
/// returns no events.
///
/// # Errors
///
/// Returns [`SessionSeqRangeError`] when a synthesized sequence leaves the
/// safe-integer domain.
pub fn open_turn_closers(
    events: &[SessionEvent],
    cause: OpenTurnCloseCause,
) -> Result<Vec<SessionEvent>, SessionSeqRangeError> {
    let mut open_turn: Option<u64> = None;
    let mut open_step: Option<u64> = None;
    let mut recovery = ToolCallRecovery::new(cause);
    for event in events {
        recovery.observe(event);
        match &event.data {
            SessionEventData::TurnStart(data) => {
                open_turn = Some(data.turn);
                open_step = None;
            }
            SessionEventData::TurnEnd(_) => {
                open_turn = None;
                open_step = None;
            }
            SessionEventData::StepStart(data) => open_step = Some(data.step),
            SessionEventData::StepEnd(_) => open_step = None,
            // Other event types do not move the turn/step boundary cursor.
            _ => {}
        }
    }

    // Balanced log (no open tail turn): nothing to close. An open turn
    // implies `events` is non-empty (its turn/start was logged), so `last`
    // exists.
    let Some(last) = events.last() else {
        return Ok(Vec::new());
    };
    let Some(open_turn) = open_turn else {
        return Ok(Vec::new());
    };

    // The last real event supplies the seq base and the timestamp for the
    // synthetic closers (reusing the last timestamp keeps them deterministic
    // and never invents a "future" time).
    let mut closers = recovery.results()?;
    let mut seq = last.seq.get() + closers.len() as u64 + 1;
    let time = last.time;

    // Close an open step before its turn.
    if let Some(open_step) = open_step {
        closers.push(SessionEvent {
            seq: SessionSeq::new(seq)?,
            time,
            data: SessionEventData::StepEnd(StepBoundsData {
                turn: open_turn,
                step: open_step,
            }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        });
        seq += 1;
    }
    closers.push(SessionEvent {
        seq: SessionSeq::new(seq)?,
        time,
        data: SessionEventData::TurnEnd(TurnEndData {
            turn: open_turn,
            reason: TurnEndReason::Known(cause.turn_end_kind()),
        }),
        ignorable: None,
        surface_op: None,
        source_event_seqs: None,
    });
    Ok(closers)
}

/// Crash-recovery entry point: synthetic closers that balance a persisted
/// log whose tail turn was interrupted. Fork seeds receive their `forked`
/// cause closers through [`crate::fork::build_fork_seed`].
///
/// # Errors
///
/// Returns [`SessionSeqRangeError`] when a synthesized sequence leaves the
/// safe-integer domain.
pub fn interrupted_turn_closers(
    events: &[SessionEvent],
) -> Result<Vec<SessionEvent>, SessionSeqRangeError> {
    open_turn_closers(events, OpenTurnCloseCause::Interrupted)
}

#[cfg(test)]
mod tests {
    use fr_wire::event::{ToolCallData, TurnStartData};

    use super::*;
    use crate::closer_text;

    fn raw() -> RawObject {
        RawObject::new()
    }

    fn assistant_with_call(seq: u64, turn: u64, step: u64, call_id: &str) -> SessionEvent {
        SessionEvent {
            seq: SessionSeq::new(seq).expect("seq"),
            time: 1_000 + i64::try_from(seq).expect("seq fits i64"),
            data: SessionEventData::AssistantMessage(AssistantMessageData {
                turn,
                step,
                message: Message::Known(KnownMessage::Assistant {
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
                        extra: raw(),
                    }),
                }),
                stream: Vec::new(),
                usage: None,
                interrupted: None,
            }),
            ignorable: None,
            surface_op: Some(SurfaceOp::Append(AppendOp)),
            source_event_seqs: None,
        }
    }

    fn tool_call(seq: u64, turn: u64, step: u64, call_id: &str) -> SessionEvent {
        SessionEvent {
            seq: SessionSeq::new(seq).expect("seq"),
            time: 1_000 + i64::try_from(seq).expect("seq fits i64"),
            data: SessionEventData::ToolCall(ToolCallData {
                turn,
                step,
                call_id: ToolCallId::new(call_id),
                name: "bash".to_owned(),
                arguments: "{}".to_owned(),
            }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }
    }

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

    fn turn_end(seq: u64, turn: u64) -> SessionEvent {
        SessionEvent {
            seq: SessionSeq::new(seq).expect("seq"),
            time: 1_000 + i64::try_from(seq).expect("seq fits i64"),
            data: SessionEventData::TurnEnd(TurnEndData {
                turn,
                reason: TurnEndReason::Known(TurnEndReasonKind::Completed),
            }),
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

    fn step_end(seq: u64, turn: u64, step: u64) -> SessionEvent {
        SessionEvent {
            seq: SessionSeq::new(seq).expect("seq"),
            time: 1_000 + i64::try_from(seq).expect("seq fits i64"),
            data: SessionEventData::StepEnd(StepBoundsData { turn, step }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }
    }

    #[test]
    fn empty_and_balanced_logs_close_with_nothing() {
        assert_eq!(
            interrupted_turn_closers(&[]).expect("closers"),
            [] as [fr_wire::SessionEvent; 0]
        );
        let balanced = vec![turn_start(0, 1), turn_end(1, 1)];
        assert_eq!(
            interrupted_turn_closers(&balanced).expect("closers"),
            [] as [fr_wire::SessionEvent; 0]
        );
    }

    #[test]
    fn unstarted_call_closes_with_not_started_error_and_no_source_seqs() {
        let log = vec![
            turn_start(0, 1),
            step_start(1, 1, 1),
            assistant_with_call(2, 1, 1, "call-a"),
        ];
        let closers = interrupted_turn_closers(&log).expect("closers");
        assert_eq!(closers.len(), 3);
        let SessionEventData::ToolResult(result) = &closers[0].data else {
            panic!("first closer is a tool/result");
        };
        assert_eq!(closers[0].seq.get(), 3);
        assert_eq!(closers[0].time, log[2].time);
        let Message::Known(KnownMessage::Tool {
            id,
            content,
            source: MessageSource::Known(KnownMessageSource::Tool { call_id, extra }),
            tool_call_id,
            is_error,
        }) = &result.message
        else {
            panic!("tool-role message");
        };
        assert_eq!(*extra, RawObject::new());
        assert_eq!(id.as_str(), "interrupted-tool-result-call-a-3");
        assert_eq!(call_id.as_str(), "call-a");
        assert_eq!(*tool_call_id, call_id.clone());
        assert_eq!(*is_error, Some(TrueOnly));
        let error = result.error.as_ref().expect("error metadata");
        assert_eq!(error.name, "ToolNotStartedError");
        assert_eq!(error.code, closer_text::TOOL_NOT_STARTED);
        assert!(closers[0].source_event_seqs.is_none());
        assert!(matches!(closers[0].surface_op, Some(SurfaceOp::Append(_))));
        let Some(ContentBlock::Known(KnownContentBlock::Text { text })) = content.first() else {
            panic!("one text block");
        };
        assert_eq!(text.as_str(), closer_text::INTERRUPTED_NOT_STARTED);
        // Open step closes before the turn.
        let SessionEventData::StepEnd(bounds) = &closers[1].data else {
            panic!("step/end second");
        };
        assert_eq!((bounds.turn, bounds.step), (1, 1));
        let SessionEventData::TurnEnd(end) = &closers[2].data else {
            panic!("turn/end third");
        };
        assert_eq!(end.turn, 1);
        assert_eq!(
            end.reason,
            TurnEndReason::Known(TurnEndReasonKind::Interrupted)
        );
        assert_eq!(closers[2].seq.get(), 5);
    }

    #[test]
    fn started_call_closes_with_outcome_unknown_and_call_seq_source() {
        let log = vec![
            turn_start(0, 1),
            step_start(1, 1, 1),
            assistant_with_call(2, 1, 1, "call-a"),
            tool_call(3, 1, 1, "call-a"),
        ];
        let closers = interrupted_turn_closers(&log).expect("closers");
        assert_eq!(closers.len(), 3);
        let SessionEventData::ToolResult(result) = &closers[0].data else {
            panic!("tool/result first");
        };
        let error = result.error.as_ref().expect("error metadata");
        assert_eq!(error.name, "ToolOutcomeUnknownError");
        assert_eq!(error.code, closer_text::TOOL_OUTCOME_UNKNOWN);
        assert_eq!(
            closers[0].source_event_seqs.as_deref(),
            Some(&[SessionSeq::new(3).expect("seq")][..])
        );
        let Message::Known(KnownMessage::Tool { content, .. }) = &result.message else {
            panic!("tool-role message");
        };
        let Some(ContentBlock::Known(KnownContentBlock::Text { text })) = content.first() else {
            panic!("one text block");
        };
        assert_eq!(text.as_str(), closer_text::INTERRUPTED_STARTED);
    }

    #[test]
    fn appended_matching_result_settles_pending_but_non_append_does_not() {
        let mut appended = tool_call(3, 1, 1, "call-a");
        appended.data = SessionEventData::ToolResult(ToolResultData {
            turn: 1,
            step: 1,
            message: Message::Known(KnownMessage::Tool {
                id: MessageId::new("result-1"),
                content: vec![ContentBlock::Known(KnownContentBlock::Text {
                    text: "done".to_owned(),
                })],
                source: MessageSource::Known(KnownMessageSource::Tool {
                    call_id: ToolCallId::new("call-a"),
                    extra: RawObject::new(),
                }),
                tool_call_id: ToolCallId::new("call-a"),
                is_error: None,
            }),
            error: None,
            meta: None,
        });
        appended.surface_op = Some(SurfaceOp::Append(AppendOp));
        let balanced = vec![
            turn_start(0, 1),
            step_start(1, 1, 1),
            assistant_with_call(2, 1, 1, "call-a"),
            appended.clone(),
            step_end(4, 1, 1),
            turn_end(5, 1),
        ];
        assert_eq!(
            interrupted_turn_closers(&balanced).expect("closers"),
            [] as [fr_wire::SessionEvent; 0]
        );

        // A replacement-settled result does not clear the pending request:
        // its outcome never durably landed.
        let mut replacement = appended.clone();
        replacement.seq = SessionSeq::new(3).expect("seq");
        replacement.surface_op = None;
        let log = vec![
            turn_start(0, 1),
            step_start(1, 1, 1),
            assistant_with_call(2, 1, 1, "call-a"),
            replacement,
        ];
        let closers = interrupted_turn_closers(&log).expect("closers");
        assert_eq!(closers.len(), 3);

        // A result on a different step does not settle either.
        let mut foreign_step = appended;
        foreign_step.seq = SessionSeq::new(3).expect("seq");
        let SessionEventData::ToolResult(data) = &mut foreign_step.data else {
            panic!("tool/result");
        };
        data.step = 2;
        let log = vec![
            turn_start(0, 1),
            step_start(1, 1, 1),
            assistant_with_call(2, 1, 1, "call-a"),
            foreign_step,
        ];
        let closers = interrupted_turn_closers(&log).expect("closers");
        assert_eq!(closers.len(), 3);
    }

    #[test]
    fn turn_start_clears_pending_and_insertion_order_is_kept() {
        let log = vec![
            turn_start(0, 1),
            assistant_with_call(1, 1, 1, "call-a"),
            turn_start(2, 2),
            assistant_with_call(3, 2, 1, "call-b"),
            assistant_with_call(4, 2, 1, "call-a"),
        ];
        let closers = interrupted_turn_closers(&log).expect("closers");
        assert_eq!(closers.len(), 3);
        let ids: Vec<String> = closers[..2]
            .iter()
            .map(|event| match &event.data {
                SessionEventData::ToolResult(result) => match &result.message {
                    Message::Known(KnownMessage::Tool { id, .. }) => id.as_str().to_owned(),
                    _ => panic!("tool-role"),
                },
                _ => panic!("tool/result"),
            })
            .collect();
        // Insertion order of the pending map (call-b first in the open turn).
        assert_eq!(
            ids,
            [
                "interrupted-tool-result-call-b-5",
                "interrupted-tool-result-call-a-6"
            ]
        );
        // turn/end reuses the last real event's time.
        assert_eq!(closers[2].time, log[4].time);
    }

    #[test]
    fn forked_cause_uses_forked_wording_and_prefix() {
        let log = vec![
            turn_start(0, 1),
            step_start(1, 1, 1),
            assistant_with_call(2, 1, 1, "call-a"),
            tool_call(3, 1, 1, "call-a"),
        ];
        let closers = open_turn_closers(&log, OpenTurnCloseCause::Forked).expect("closers");
        let SessionEventData::ToolResult(result) = &closers[0].data else {
            panic!("tool/result first");
        };
        let Message::Known(KnownMessage::Tool { id, content, .. }) = &result.message else {
            panic!("tool-role message");
        };
        assert!(id.as_str().starts_with("forked-tool-result-call-a-"));
        let Some(ContentBlock::Known(KnownContentBlock::Text { text })) = content.first() else {
            panic!("one text block");
        };
        assert_eq!(text.as_str(), closer_text::FORKED_STARTED);
        let SessionEventData::TurnEnd(end) = &closers[2].data else {
            panic!("turn/end third");
        };
        assert_eq!(end.reason, TurnEndReason::Known(TurnEndReasonKind::Forked));
    }
}

// Rust guideline compliant 2026-10-23
