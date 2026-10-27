// Rust guideline compliant 2026-10-27
//! Durable projection state for agent turn and step boundaries.
//!
//! Mirrors dsh `agent-loop/src/index.ts` `turnBoundaryProjectionDefinition`
//! (stateVersion 2): `turn/start` opens a turn at its own seq and records the
//! turn ordinal, `turn/end` closes it, and the step boundaries retain the
//! latest step's start seq and boundary kind. Every other event type passes
//! through unchanged. `lastTurn` is the turn ordinal an idle agent resumes
//! from — the driver seeds a fresh turn as `lastTurn + 1`.

use fr_wire::event::{SessionEvent, SessionEventData};
use fr_wire::ids::SessionSeq;
use serde::{Deserialize, Serialize};

/// The projection key this fold registers under.
pub const PROJECTION_KEY: &str = "turnBoundary";

/// The projection state schema version (dsh stateVersion 2).
pub const STATE_VERSION: u32 = 2;

/// Which boundary a step currently sits at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepBoundaryKind {
    /// The step has opened and not yet closed.
    #[serde(rename = "start")]
    Start,
    /// The step has closed.
    #[serde(rename = "end")]
    End,
}

/// The latest observed step boundary and its event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepBoundary {
    /// Whether the boundary opened or closed the step.
    pub kind: StepBoundaryKind,
    /// The boundary event's sequence number.
    pub seq: SessionSeq,
}

/// The `turnBoundary` projection state (wire shape camelCase, verbatim to
/// the dsh view schema).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnBoundaryState {
    /// The open `turn/start` seq; `None` while no turn is open.
    pub open_turn_start_seq: Option<SessionSeq>,
    /// The latest `step/start` seq, retained after the step closes.
    pub last_step_start_seq: Option<SessionSeq>,
    /// The latest step boundary event.
    pub last_step_boundary: Option<StepBoundary>,
    /// The turn ordinal an idle agent resumes from; zero before any turn.
    pub last_turn: u64,
}

/// The projection's initial state (dsh `init`).
#[must_use]
pub const fn init() -> TurnBoundaryState {
    TurnBoundaryState {
        open_turn_start_seq: None,
        last_step_start_seq: None,
        last_step_boundary: None,
        last_turn: 0,
    }
}

/// Applies one committed session event to the projection state (dsh `apply`):
/// turn events track the open turn, step events track the latest boundary,
/// and every other event type leaves the state untouched.
#[must_use]
pub const fn fold_turn_boundary(
    mut state: TurnBoundaryState,
    event: &SessionEvent,
) -> TurnBoundaryState {
    match &event.data {
        SessionEventData::TurnStart(data) => {
            state.open_turn_start_seq = Some(event.seq);
            state.last_turn = data.turn;
        }
        SessionEventData::TurnEnd(_) => {
            state.open_turn_start_seq = None;
        }
        SessionEventData::StepStart(_) => {
            state.last_step_start_seq = Some(event.seq);
            state.last_step_boundary = Some(StepBoundary {
                kind: StepBoundaryKind::Start,
                seq: event.seq,
            });
        }
        SessionEventData::StepEnd(_) => {
            state.last_step_boundary = Some(StepBoundary {
                kind: StepBoundaryKind::End,
                seq: event.seq,
            });
        }
        _ => {}
    }
    state
}

#[cfg(test)]
mod tests {
    use super::{StepBoundary, StepBoundaryKind, TurnBoundaryState, fold_turn_boundary, init};
    use fr_wire::event::{
        SessionEvent, SessionEventData, StepBoundsData, TurnEndData, TurnEndReason,
        TurnEndReasonKind, TurnStartData,
    };
    use fr_wire::ids::SessionSeq;
    use serde_json::json;

    fn session_seq(n: u64) -> SessionSeq {
        SessionSeq::new(n).expect("fixture seq values sit far below the safe-integer cap")
    }

    fn event(seq: u64, data: SessionEventData) -> SessionEvent {
        SessionEvent {
            seq: session_seq(seq),
            time: 0,
            data,
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }
    }

    fn turn_start(seq: u64, turn: u64) -> SessionEvent {
        event(seq, SessionEventData::TurnStart(TurnStartData { turn }))
    }

    fn turn_end(seq: u64, turn: u64) -> SessionEvent {
        event(
            seq,
            SessionEventData::TurnEnd(TurnEndData {
                turn,
                reason: TurnEndReason::Known(TurnEndReasonKind::Completed),
            }),
        )
    }

    fn step_bounds(turn: u64, step: u64) -> StepBoundsData {
        StepBoundsData { turn, step }
    }

    /// Covers the four fold transitions plus the retained fields the dsh
    /// spread keeps: step boundaries survive `turn/end`, and `last_turn`
    /// survives everything after the turn opened.
    #[test]
    fn fold_tracks_boundaries() {
        let state = init();
        assert_eq!(state.last_turn, 0);

        let state = fold_turn_boundary(state, &turn_start(1, 1));
        assert_eq!(state.open_turn_start_seq, Some(session_seq(1)));
        assert_eq!(state.last_turn, 1);

        let state = fold_turn_boundary(
            state,
            &SessionEvent {
                seq: session_seq(2),
                time: 0,
                data: SessionEventData::StepStart(step_bounds(1, 1)),
                ignorable: None,
                surface_op: None,
                source_event_seqs: None,
            },
        );
        assert_eq!(state.last_step_start_seq, Some(session_seq(2)));
        assert_eq!(
            state.last_step_boundary,
            Some(StepBoundary {
                kind: StepBoundaryKind::Start,
                seq: session_seq(2),
            })
        );

        let state = fold_turn_boundary(
            state,
            &SessionEvent {
                seq: session_seq(5),
                time: 0,
                data: SessionEventData::StepEnd(step_bounds(1, 1)),
                ignorable: None,
                surface_op: None,
                source_event_seqs: None,
            },
        );
        assert_eq!(state.last_step_start_seq, Some(session_seq(2)));
        assert_eq!(
            state.last_step_boundary,
            Some(StepBoundary {
                kind: StepBoundaryKind::End,
                seq: session_seq(5),
            })
        );

        let state = fold_turn_boundary(state, &turn_end(6, 1));
        assert_eq!(state.open_turn_start_seq, None);
        assert_eq!(
            state.last_step_boundary.map(|b| b.seq),
            Some(session_seq(5))
        );
        assert_eq!(state.last_turn, 1);
    }

    /// Covers the pass-through arm: message and boundary-irrelevant events
    /// leave every field unchanged.
    #[test]
    fn unrelated_events_pass_through() {
        let state = fold_turn_boundary(init(), &turn_start(3, 2));
        let untouched = fold_turn_boundary(
            state,
            &event(
                4,
                SessionEventData::UserMessage(
                    serde_json::from_value(json!({
                        "role": "user",
                        "id": "m-1",
                        "content": [{ "type": "text", "text": "hi" }],
                        "source": { "kind": "user" }
                    }))
                    .expect("fixture matches the fr-wire user-message wire shape"),
                ),
            ),
        );
        assert_eq!(untouched, state);
    }

    /// Covers the camelCase wire shape and the `start`/`end` boundary tags
    /// the dsh view schema pins.
    #[test]
    fn wire_shape_pins_camel_case() {
        let initial = init();
        let encoded = serde_json::to_value(initial).expect("state serializes");
        assert_eq!(
            encoded,
            json!({
                "openTurnStartSeq": null,
                "lastStepStartSeq": null,
                "lastStepBoundary": null,
                "lastTurn": 0
            })
        );
        let back: TurnBoundaryState = serde_json::from_value(encoded).expect("state round-trips");
        assert_eq!(back, initial);

        let boundary = serde_json::to_value(StepBoundary {
            kind: StepBoundaryKind::Start,
            seq: session_seq(9),
        })
        .expect("boundary serializes");
        assert_eq!(boundary, json!({"kind": "start", "seq": 9}));
    }
}
