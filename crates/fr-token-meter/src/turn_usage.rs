//! Fold one complete Turn's durable attempt lifecycle into exact token
//! accounting (dsh `token-meter/turn-usage.ts` 1:1).
//!
//! No attempt is inferred from a usage sample. Any missing lifecycle
//! boundary, incomplete attempt usage, unsafe count, or contradictory
//! exact total makes the whole disclosure unavailable.

#![forbid(unsafe_code)]

use fr_wire::call_config::TokenUsage;
use fr_wire::event::{SessionEvent, SessionEventData};

/// `Number.MAX_SAFE_INTEGER`: the dsh `Number.isSafeInteger` bound for
/// the checked sums (the projections.rs judgment constant).
const MAX_SAFE_INTEGER: u128 = 9_007_199_254_740_991;

/// One provider/model route that contributed a billed request attempt
/// (dsh `TurnTokenUsageRoute`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TurnTokenUsageRoute {
    /// The provider route key.
    pub provider: String,
    /// The provider model id.
    pub model: String,
}

/// Exact provider-reported token accounting for every attempt in one
/// completed Turn (dsh `TurnTokenUsage`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnTokenUsage {
    /// Sum of uncached prompt input across all attempts.
    pub uncached_input_tokens: u64,
    /// Output tokens across all attempts.
    pub output_tokens: u64,
    /// Exact aggregate prompt plus output total across all attempts.
    pub total_tokens: u64,
    /// Present only when every attempt reported the bucket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    /// Present only when every attempt reported the bucket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    /// Output subset, present only when every attempt reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    /// Present only when every billed attempt has provider/model
    /// attribution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routes: Option<Vec<TurnTokenUsageRoute>>,
}

/// One attempt's normalized accounting (the dsh `NormalizedAttempt`).
struct NormalizedAttempt {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
    cache_read_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    route: Option<TurnTokenUsageRoute>,
}

/// The state machine phases (the dsh `AttemptState`).
#[derive(Debug, Clone)]
enum AttemptState {
    /// No open attempt.
    Idle,
    /// An attempt is streaming; the sample arms when usage arrives.
    Open {
        turn: u64,
        step: u64,
        sample: Option<TokenUsage>,
    },
    /// The attempt's finish leg closed the usage slot.
    FinishClosed { turn: u64, step: u64 },
    /// The attempt settled through a message or a retry marker.
    Settled { turn: u64, step: u64, by: CloseBy },
}

/// What settled the attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloseBy {
    /// The `assistant/message` settlement.
    Message,
    /// The `llm/retry` marker.
    Retry,
}

/// The checked sum (the dsh `safeSum`): `None` past the safe bound.
fn safe_sum(values: impl IntoIterator<Item = u64>) -> Option<u64> {
    values
        .into_iter()
        .try_fold(0u128, |total, value| {
            let total = total + u128::from(value);
            (total <= MAX_SAFE_INTEGER).then_some(total)
        })
        .and_then(|total| u64::try_from(total).ok())
}

/// The billed route of one assistant message (the dsh `messageRoute`):
/// present only with a non-empty provider and model.
fn message_route(message: &fr_wire::message::Message) -> Option<TurnTokenUsageRoute> {
    let fr_wire::message::Message::Known(known) = message else {
        return None;
    };
    let fr_wire::message::MessageSource::Known(source) = known.source() else {
        return None;
    };
    let fr_wire::message::KnownMessageSource::Model {
        provider, model, ..
    } = source
    else {
        return None;
    };
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    Some(TurnTokenUsageRoute {
        provider: provider.clone(),
        model: model.clone(),
    })
}

/// The usage the attempt stream reports (the dsh `streamUsage`).
fn stream_usage(stream: &[fr_wire::stream::AssistantStreamRecord]) -> Option<TokenUsage> {
    let chunk = fr_llm::assistant_stream::last_assistant_stream_chunk(
        stream,
        fr_llm::assistant_stream::RawChunkKind::Usage,
    )?;
    match chunk {
        fr_wire::stream::KnownStreamChunk::Usage { usage, .. } => Some(*usage),
        _ => None,
    }
}

/// Normalize one attempt's usage sample (the dsh `normalizeUsage`):
/// `None` when the buckets are incomplete or contradictory.
fn normalize_usage(
    usage: &TokenUsage,
    route: Option<TurnTokenUsageRoute>,
) -> Option<NormalizedAttempt> {
    if usage
        .reasoning_tokens
        .is_some_and(|reasoning| reasoning > usage.output_tokens)
    {
        return None;
    }
    let mut terms = vec![usage.input_tokens];
    if let Some(cache_read) = usage.cache_read_tokens {
        terms.push(cache_read);
    }
    if let Some(cache_write) = usage.cache_write_tokens {
        terms.push(cache_write);
    }
    let known_prompt = safe_sum(terms)?;

    let exact_total = if let Some(total) = usage.total_tokens {
        let exact_prompt = total.checked_sub(usage.output_tokens)?;
        if exact_prompt < known_prompt {
            return None;
        }
        if usage.cache_read_tokens.is_some()
            && usage.cache_write_tokens.is_some()
            && exact_prompt != known_prompt
        {
            return None;
        }
        total
    } else {
        if usage.cache_read_tokens.is_none() || usage.cache_write_tokens.is_none() {
            return None;
        }
        safe_sum([known_prompt, usage.output_tokens])?
    };

    Some(NormalizedAttempt {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        total_tokens: exact_total,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        route,
    })
}

/// Aggregate the normalized attempts (the dsh `aggregateAttempts`).
fn aggregate_attempts(attempts: &[NormalizedAttempt]) -> Option<TurnTokenUsage> {
    if attempts.is_empty() {
        return None;
    }
    let input_tokens = safe_sum(attempts.iter().map(|attempt| attempt.input_tokens))?;
    let output_tokens = safe_sum(attempts.iter().map(|attempt| attempt.output_tokens))?;
    let total_tokens = safe_sum(attempts.iter().map(|attempt| attempt.total_tokens))?;
    let bucket_sum = |pick: fn(&NormalizedAttempt) -> Option<u64>| {
        if attempts.iter().all(|attempt| pick(attempt).is_some()) {
            safe_sum(attempts.iter().filter_map(pick))
        } else {
            None
        }
    };
    let cache_read_tokens = bucket_sum(|attempt| attempt.cache_read_tokens);
    let cache_write_tokens = bucket_sum(|attempt| attempt.cache_write_tokens);
    let reasoning_tokens = bucket_sum(|attempt| attempt.reasoning_tokens);
    let routes = attempts
        .iter()
        .all(|attempt| attempt.route.is_some())
        .then(|| {
            let mut unique: Vec<TurnTokenUsageRoute> = Vec::new();
            for route in attempts.iter().filter_map(|attempt| attempt.route.as_ref()) {
                let key = (route.provider.as_str(), route.model.as_str());
                if !unique
                    .iter()
                    .any(|seen| (seen.provider.as_str(), seen.model.as_str()) == key)
                {
                    unique.push(route.clone());
                }
            }
            unique
        });
    Some(TurnTokenUsage {
        uncached_input_tokens: input_tokens,
        output_tokens,
        total_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens,
        routes,
    })
}

/// The turn/step pair an event carries, reading the retry markers'
/// verbatim retained data for the plugin-declared types.
fn bounds_of(event: &SessionEvent) -> Option<(u64, u64)> {
    match &event.data {
        SessionEventData::StepStart(data) | SessionEventData::StepEnd(data) => {
            Some((data.turn, data.step))
        }
        SessionEventData::AssistantMessage(data) => Some((data.turn, data.step)),
        SessionEventData::AssistantAttempt(data) => Some((data.turn, data.step)),
        SessionEventData::Unknown(unknown) => {
            let turn = unknown.data.get("turn").and_then(serde_json::Value::as_u64);
            let step = unknown.data.get("step").and_then(serde_json::Value::as_u64);
            match unknown.type_name.as_str() {
                "llm/retry-started" | "llm/retry" => turn.zip(step),
                _ => None,
            }
        }
        _ => None,
    }
}

const fn same_attempt(state: &AttemptState, turn: u64, step: u64) -> bool {
    match state {
        AttemptState::Idle => false,
        AttemptState::Open {
            turn: state_turn,
            step: state_step,
            ..
        }
        | AttemptState::FinishClosed {
            turn: state_turn,
            step: state_step,
        }
        | AttemptState::Settled {
            turn: state_turn,
            step: state_step,
            ..
        } => *state_turn == turn && *state_step == step,
    }
}

/// Close one open attempt's usage slot (the dsh `closeOpen`): the
/// normalized attempt joins the list, or the attempt stays open
/// unclosable.
fn close_open(
    state: &mut AttemptState,
    attempts: &mut Vec<NormalizedAttempt>,
    route: Option<TurnTokenUsageRoute>,
) -> bool {
    let AttemptState::Open { sample, .. } = state else {
        return false;
    };
    let Some(sample) = sample.as_ref() else {
        return false;
    };
    let Some(normalized) = normalize_usage(sample, route) else {
        return false;
    };
    attempts.push(normalized);
    true
}

/// The open attempt's armed sample, if any (the dsh `state.sample`
/// read the message arm falls back to).
const fn open_sample(state: &AttemptState) -> Option<TokenUsage> {
    match state {
        AttemptState::Open { sample, .. } => *sample,
        _ => None,
    }
}

/// Fold one complete Turn's durable attempt lifecycle into exact token
/// accounting (the dsh `deriveTurnTokenUsage`).
///
/// `events` are the Turn-local durable events from `turn/start` through
/// `turn/end`.
#[must_use]
/// Fold one plugin-declared retry marker into the state machine (the
/// dsh `llm/retry-started` and `llm/retry` arms); `false` marks the
/// lifecycle invalid.
fn fold_retry_marker(
    state: &mut AttemptState,
    attempts: &mut Vec<NormalizedAttempt>,
    type_name: &str,
    current_turn: u64,
    event_turn: u64,
    event_step: u64,
) -> bool {
    let guarded =
        !matches!(state, AttemptState::Idle) && same_attempt(state, event_turn, event_step);
    if event_turn != current_turn || !guarded {
        return false;
    }
    if type_name == "llm/retry-started" {
        let settled_by_retry = matches!(
            state,
            AttemptState::Settled {
                by: CloseBy::Retry,
                ..
            }
        );
        if !settled_by_retry {
            return false;
        }
        *state = AttemptState::Open {
            turn: current_turn,
            step: event_step,
            sample: None,
        };
        return true;
    }
    if matches!(state, AttemptState::Settled { .. }) {
        return false;
    }
    if matches!(state, AttemptState::Open { .. }) && !close_open(state, attempts, None) {
        return false;
    }
    *state = AttemptState::Settled {
        turn: current_turn,
        step: event_step,
        by: CloseBy::Retry,
    };
    true
}

/// Fold one lifecycle event into the state machine (the dsh
/// per-event arms); `false` marks the lifecycle invalid.
fn fold_lifecycle_event(
    state: &mut AttemptState,
    attempts: &mut Vec<NormalizedAttempt>,
    event: &SessionEvent,
    current_turn: u64,
    event_turn: u64,
    event_step: u64,
) -> bool {
    match &event.data {
        SessionEventData::StepStart(_) => {
            if event_turn != current_turn || !matches!(state, AttemptState::Idle) {
                return false;
            }
            *state = AttemptState::Open {
                turn: current_turn,
                step: event_step,
                sample: None,
            };
            true
        }
        SessionEventData::Unknown(unknown)
            if unknown.type_name == "llm/retry-started" || unknown.type_name == "llm/retry" =>
        {
            fold_retry_marker(
                state,
                attempts,
                &unknown.type_name,
                current_turn,
                event_turn,
                event_step,
            )
        }
        SessionEventData::AssistantAttempt(data) => {
            let open = matches!(state, AttemptState::Open { .. })
                && same_attempt(state, event_turn, event_step);
            if event_turn != current_turn || !open {
                return false;
            }
            let sample = stream_usage(&data.stream).or_else(|| open_sample(state));
            *state = AttemptState::Open {
                turn: current_turn,
                step: event_step,
                sample,
            };
            if close_open(state, attempts, None) {
                *state = AttemptState::FinishClosed {
                    turn: current_turn,
                    step: event_step,
                };
                true
            } else {
                false
            }
        }
        SessionEventData::AssistantMessage(data) => {
            let open = matches!(state, AttemptState::Open { .. })
                && same_attempt(state, event_turn, event_step);
            if event_turn != current_turn || !open {
                return false;
            }
            let sample = data.usage.or_else(|| stream_usage(&data.stream));
            if let (AttemptState::Open { sample: slot, .. }, Some(sample)) = (&mut *state, sample) {
                *slot = Some(sample);
            }
            let route = message_route(&data.message);
            if close_open(state, attempts, route) {
                *state = AttemptState::Settled {
                    turn: current_turn,
                    step: event_step,
                    by: CloseBy::Message,
                };
                true
            } else {
                false
            }
        }
        SessionEventData::StepEnd(_) => {
            let guarded =
                !matches!(state, AttemptState::Idle) && same_attempt(state, event_turn, event_step);
            if event_turn != current_turn || !guarded {
                return false;
            }
            if matches!(state, AttemptState::Open { .. }) && !close_open(state, attempts, None) {
                return false;
            }
            *state = AttemptState::Idle;
            true
        }
        _ => true,
    }
}

/// Fold one complete Turn's durable attempt lifecycle into exact token
/// accounting (the dsh `deriveTurnTokenUsage`).
///
/// `events` are the Turn-local durable events from `turn/start` through
/// `turn/end`.
#[must_use]
pub fn derive_turn_token_usage(events: &[SessionEvent]) -> Option<TurnTokenUsage> {
    let mut state = AttemptState::Idle;
    let mut attempts: Vec<NormalizedAttempt> = Vec::new();
    let mut turn: Option<u64> = None;
    let mut saw_end = false;
    let mut invalid = false;

    for event in events {
        if invalid {
            break;
        }
        if let SessionEventData::TurnStart(data) = &event.data {
            if turn.is_some() || !matches!(state, AttemptState::Idle) {
                invalid = true;
            } else {
                turn = Some(data.turn);
            }
            continue;
        }
        let Some(current_turn) = turn else {
            invalid = true;
            break;
        };
        if let SessionEventData::TurnEnd(data) = &event.data {
            if data.turn != current_turn || !matches!(state, AttemptState::Idle) || saw_end {
                invalid = true;
            } else {
                saw_end = true;
            }
            continue;
        }
        if saw_end {
            invalid = true;
            break;
        }
        let Some((event_turn, event_step)) = bounds_of(event) else {
            // Unrelated events (headers, tool traffic, plugin records)
            // ride through without touching the lifecycle.
            continue;
        };
        if !fold_lifecycle_event(
            &mut state,
            &mut attempts,
            event,
            current_turn,
            event_turn,
            event_step,
        ) {
            invalid = true;
        }
    }

    if invalid || !saw_end || !matches!(state, AttemptState::Idle) {
        return None;
    }
    aggregate_attempts(&attempts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fr_wire::event::{AppendOp, SurfaceOp};
    use serde_json::json;

    fn turn_start(seq: u64, turn: u64) -> SessionEvent {
        SessionEvent {
            seq: fr_wire::SessionSeq::new(seq).unwrap_or_else(|e| panic!("seq: {e}")),
            time: 1,
            data: SessionEventData::TurnStart(fr_wire::event::TurnStartData { turn }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }
    }

    fn turn_end(seq: u64, turn: u64) -> SessionEvent {
        SessionEvent {
            seq: fr_wire::SessionSeq::new(seq).unwrap_or_else(|e| panic!("seq: {e}")),
            time: 1,
            data: SessionEventData::TurnEnd(fr_wire::event::TurnEndData {
                turn,
                reason: fr_wire::event::TurnEndReason::Known(
                    fr_wire::event::TurnEndReasonKind::Completed,
                ),
            }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }
    }

    fn step(seq: u64, kind: &'static str, turn: u64, step: u64) -> SessionEvent {
        let data = if kind == "start" {
            SessionEventData::StepStart(fr_wire::event::StepBoundsData { turn, step })
        } else {
            SessionEventData::StepEnd(fr_wire::event::StepBoundsData { turn, step })
        };
        SessionEvent {
            seq: fr_wire::SessionSeq::new(seq).unwrap_or_else(|e| panic!("seq: {e}")),
            time: 1,
            data,
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }
    }

    fn assistant_message(
        seq: u64,
        turn: u64,
        step: u64,
        usage: Option<TokenUsage>,
        provider: &str,
        model: &str,
    ) -> SessionEvent {
        SessionEvent {
            seq: fr_wire::SessionSeq::new(seq).unwrap_or_else(|e| panic!("seq: {e}")),
            time: 1,
            data: SessionEventData::AssistantMessage(fr_wire::event::AssistantMessageData {
                turn,
                step,
                message: fr_wire::message::Message::Known(Box::new(
                    fr_wire::message::KnownMessage::Assistant {
                        id: fr_wire::ids::MessageId::new("a"),
                        content: Vec::new(),
                        source: fr_wire::message::MessageSource::Known(
                            fr_wire::message::KnownMessageSource::Model {
                                provider: provider.to_owned(),
                                model: model.to_owned(),
                                replay_state: None,
                                extra: fr_wire::content::RawObject::new(),
                            },
                        ),
                        extra: fr_wire::content::RawObject::new(),
                    },
                )),
                stream: Vec::new(),
                usage,
                interrupted: None,
            }),
            ignorable: None,
            surface_op: Some(SurfaceOp::Append(AppendOp)),
            source_event_seqs: None,
        }
    }

    fn retry_marker(seq: u64, name: &str, turn: u64, step: u64) -> SessionEvent {
        SessionEvent {
            seq: fr_wire::SessionSeq::new(seq).unwrap_or_else(|e| panic!("seq: {e}")),
            time: 1,
            data: SessionEventData::Unknown(fr_wire::event::UnknownEventData {
                type_name: name.to_owned(),
                data: serde_json::from_value(json!({"turn": turn, "step": step}))
                    .unwrap_or_else(|e| panic!("data: {e}")),
            }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }
    }

    /// A completed turn with two attributed attempts aggregates the
    /// buckets and dedupes the routes (the dsh aggregate rule).
    #[test]
    fn complete_turn_aggregates_and_dedupes_routes() {
        let events = [
            turn_start(1, 1),
            step(2, "start", 1, 0),
            assistant_message(
                3,
                1,
                0,
                Some(TokenUsage {
                    input_tokens: 100,
                    output_tokens: 20,
                    cache_read_tokens: Some(30),
                    cache_write_tokens: Some(5),
                    ..TokenUsage::default()
                }),
                "p",
                "m",
            ),
            step(4, "end", 1, 0),
            step(5, "start", 1, 1),
            assistant_message(
                6,
                1,
                1,
                Some(TokenUsage {
                    input_tokens: 90,
                    output_tokens: 30,
                    cache_read_tokens: Some(25),
                    cache_write_tokens: Some(5),
                    ..TokenUsage::default()
                }),
                "p",
                "m",
            ),
            step(7, "end", 1, 1),
            turn_end(8, 1),
        ];
        let usage = derive_turn_token_usage(&events).expect("usage");
        assert_eq!(usage.uncached_input_tokens, 190);
        assert_eq!(usage.output_tokens, 50);
        // No totalTokens declared and both cache buckets present: the
        // derived exact total = prompt + output per attempt.
        assert_eq!(usage.total_tokens, (100 + 30 + 5 + 20) + (90 + 25 + 5 + 30));
        assert_eq!(usage.cache_read_tokens, Some(55));
        assert_eq!(usage.cache_write_tokens, Some(10));
        assert_eq!(
            usage.routes,
            Some(vec![TurnTokenUsageRoute {
                provider: "p".to_owned(),
                model: "m".to_owned(),
            }])
        );
    }

    /// The retry lifecycle: the attempt stream settles at finish, the
    /// retry marker settles it by-retry, the started marker re-opens,
    /// and the retried message settles with its route.
    #[test]
    fn retry_lifecycle_settles_through_markers() {
        let attempt_usage = || TokenUsage {
            input_tokens: 100,
            output_tokens: 20,
            cache_read_tokens: Some(30),
            cache_write_tokens: Some(5),
            ..TokenUsage::default()
        };
        let attempt = |seq: u64| SessionEvent {
            seq: fr_wire::SessionSeq::new(seq).unwrap_or_else(|e| panic!("seq: {e}")),
            time: 1,
            data: SessionEventData::AssistantAttempt(fr_wire::event::AssistantAttemptData {
                turn: 1,
                step: 0,
                stream: vec![fr_wire::stream::AssistantStreamRecord::Chunk {
                    time: 1,
                    chunk: fr_wire::stream::StreamChunk::Known(
                        fr_wire::stream::KnownStreamChunk::Usage {
                            usage: attempt_usage(),
                        },
                    ),
                }],
            }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        };
        let events = [
            turn_start(1, 1),
            step(2, "start", 1, 0),
            attempt(3),
            retry_marker(4, "llm/retry", 1, 0),
            retry_marker(5, "llm/retry-started", 1, 0),
            assistant_message(
                6,
                1,
                0,
                Some(TokenUsage {
                    input_tokens: 110,
                    output_tokens: 25,
                    cache_read_tokens: Some(20),
                    cache_write_tokens: Some(6),
                    ..TokenUsage::default()
                }),
                "p",
                "m",
            ),
            step(7, "end", 1, 0),
            turn_end(8, 1),
        ];
        let usage = derive_turn_token_usage(&events).expect("usage");
        // Both attempts add (the stream attempt and the retried one).
        assert_eq!(usage.uncached_input_tokens, 210);
        assert_eq!(usage.output_tokens, 45);
        assert_eq!(
            usage.total_tokens,
            (100 + 30 + 5 + 20) + (110 + 20 + 6 + 25)
        );
        // The stream attempt carries no message attribution, so the
        // whole disclosure drops routes while keeping the totals.
        assert_eq!(usage.routes, None);
    }

    /// A turn that never ends, or an assistant message with no matching
    /// step/start, makes the disclosure unavailable.
    #[test]
    fn broken_lifecycles_are_unavailable() {
        let no_end = [turn_start(1, 1), step(2, "start", 1, 0)];
        assert_eq!(derive_turn_token_usage(&no_end), None);

        let orphan_message = [
            turn_start(1, 1),
            assistant_message(
                2,
                1,
                0,
                Some(TokenUsage {
                    input_tokens: 1,
                    output_tokens: 1,
                    ..TokenUsage::default()
                }),
                "p",
                "m",
            ),
            turn_end(3, 1),
        ];
        assert_eq!(derive_turn_token_usage(&orphan_message), None);

        let double_start = [turn_start(1, 1), turn_start(2, 1), turn_end(3, 1)];
        assert_eq!(derive_turn_token_usage(&double_start), None);

        // Reasoning above output is contradictory: unavailable.
        let contradictory = [
            turn_start(1, 1),
            step(2, "start", 1, 0),
            assistant_message(
                3,
                1,
                0,
                Some(TokenUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                    reasoning_tokens: Some(9),
                    cache_read_tokens: Some(1),
                    cache_write_tokens: Some(1),
                    ..TokenUsage::default()
                }),
                "p",
                "m",
            ),
            step(4, "end", 1, 0),
            turn_end(5, 1),
        ];
        assert_eq!(derive_turn_token_usage(&contradictory), None);
    }
}
