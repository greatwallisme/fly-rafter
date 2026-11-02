//! Pure folds for durable provider-reported token usage and context
//! occupancy (dsh `token-meter/usage-projection.ts` 1:1).
//!
//! Two units: `tokenUsage` (stateVersion 2) and `contextPressure`
//! (stateVersion 5).
//!
//! Each v2 Assistant settlement contributes the last usage sample
//! embedded in its stream; `llm/retry-started` closes the replacement
//! slot so the retried attempt adds to the total. The pressure unit
//! keeps independent last-wins slots: the newest usage sample supplies
//! the provider numerator, the newest `request/context` record the
//! denominator. Both are whole values, so replay order alone decides
//! the result and no cross-field consistency is claimed — the pair is
//! explicitly not one atomic request observation.

#![forbid(unsafe_code)]

use fr_wire::call_config::TokenUsage;
use fr_wire::event::{SessionEvent, SessionEventData};

use crate::surface_projection::{ShadowPriceClaim, fold_surface_projection};

/// Disjoint provider usage buckets of one sample or a running total
/// (dsh `TokenUsageProjection`; reasoning output is already included in
/// the output bucket and is never accumulated again).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageBuckets {
    /// Uncached prompt input across the folded samples.
    pub uncached_input_tokens: u64,
    /// Output tokens across the folded samples.
    pub output_tokens: u64,
    /// Cache reads when reported.
    pub cache_read_tokens: u64,
    /// Cache writes when reported.
    pub cache_write_tokens: u64,
}

/// The `tokenUsage` unit state (the zod schema shape verbatim).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TokenUsageState {
    /// The running bucket totals.
    pub totals: TokenUsageBuckets,
    /// The newest settled attempt's replacement slot.
    pub last: Option<LastSample>,
}

/// One last-wins sample record (the `last` slot).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastSample {
    /// The owning turn ordinal.
    pub turn: u64,
    /// The owning step ordinal.
    pub step: u64,
    /// The sample's buckets.
    pub buckets: TokenUsageBuckets,
}

/// The `contextPressure` unit state (stateVersion 5 shape verbatim).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextPressureState {
    /// Newest recorded route capacity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Provider-reported prompt size of the most recent request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressure_tokens: Option<u64>,
    /// Running surface total under the shadow-price protocol.
    pub surface_tokens: u64,
    /// The surface total when the pressure sample was taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampled_surface_tokens: Option<u64>,
    /// The claim armed by the immediately preceding metering event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<ShadowPriceClaim>,
}

fn buckets_from(usage: &TokenUsage) -> TokenUsageBuckets {
    TokenUsageBuckets {
        uncached_input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens.unwrap_or(0),
        cache_write_tokens: usage.cache_write_tokens.unwrap_or(0),
    }
}

fn add_replacing(
    totals: TokenUsageBuckets,
    previous: Option<TokenUsageBuckets>,
    next: TokenUsageBuckets,
) -> TokenUsageBuckets {
    let sub = |total: u64, previous: Option<u64>| total.saturating_sub(previous.unwrap_or(0));
    TokenUsageBuckets {
        uncached_input_tokens: sub(
            totals.uncached_input_tokens,
            previous.map(|b| b.uncached_input_tokens),
        )
        .saturating_add(next.uncached_input_tokens),
        output_tokens: sub(totals.output_tokens, previous.map(|b| b.output_tokens))
            .saturating_add(next.output_tokens),
        cache_read_tokens: sub(
            totals.cache_read_tokens,
            previous.map(|b| b.cache_read_tokens),
        )
        .saturating_add(next.cache_read_tokens),
        cache_write_tokens: sub(
            totals.cache_write_tokens,
            previous.map(|b| b.cache_write_tokens),
        )
        .saturating_add(next.cache_write_tokens),
    }
}

/// The usage one durable Assistant settlement reports for its attempt,
/// if any (the dsh `usageOf`): the event's own usage bucket, or the
/// stream's last usage chunk.
fn usage_of(event: &SessionEvent) -> Option<TokenUsage> {
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
    match &event.data {
        SessionEventData::AssistantMessage(data) => {
            data.usage.or_else(|| stream_usage(&data.stream))
        }
        SessionEventData::AssistantAttempt(data) => stream_usage(&data.stream),
        _ => None,
    }
}

/// Apply one event to the `tokenUsage` unit state (the dsh `apply`).
#[must_use]
pub fn apply_token_usage(state: &TokenUsageState, event: &SessionEvent) -> TokenUsageState {
    if let SessionEventData::Unknown(unknown) = &event.data {
        if unknown.type_name == "llm/retry-started" {
            let turn = unknown.data.get("turn").and_then(serde_json::Value::as_u64);
            let step = unknown.data.get("step").and_then(serde_json::Value::as_u64);
            if let (Some(turn), Some(step)) = (turn, step) {
                let closes_slot = state
                    .last
                    .as_ref()
                    .is_some_and(|last| last.turn == turn && last.step == step);
                if closes_slot {
                    let mut next = state.clone();
                    next.last = None;
                    return next;
                }
            }
            return state.clone();
        }
        return state.clone();
    }
    let bounds = match &event.data {
        SessionEventData::AssistantMessage(data) => Some((data.turn, data.step)),
        SessionEventData::AssistantAttempt(data) => Some((data.turn, data.step)),
        _ => None,
    };
    let Some((turn, step)) = bounds else {
        return state.clone();
    };
    let Some(sample) = usage_of(event) else {
        return state.clone();
    };
    let buckets = buckets_from(&sample);
    let previous = state
        .last
        .as_ref()
        .filter(|last| last.turn == turn && last.step == step)
        .map(|last| last.buckets);
    if previous.is_some_and(|previous| previous == buckets) {
        return state.clone();
    }
    TokenUsageState {
        totals: add_replacing(state.totals, previous, buckets),
        last: Some(LastSample {
            turn,
            step,
            buckets,
        }),
    }
}

/// Prompt-side pressure of one request: input plus cache traffic, no
/// output (the dsh `pressureFrom`).
fn pressure_from(usage: &TokenUsage) -> u64 {
    usage
        .input_tokens
        .saturating_add(usage.cache_read_tokens.unwrap_or(0))
        .saturating_add(usage.cache_write_tokens.unwrap_or(0))
}

/// Apply one event to the `contextPressure` unit state (the dsh
/// `apply`).
///
/// A usage sample is stamped BEFORE the same event joins the surface,
/// so an `assistant/message` anchors against the surface its own
/// request saw.
///
/// # Errors
///
/// Returns the shadow-price fold refusal.
pub fn apply_context_pressure(
    state: &ContextPressureState,
    event: &SessionEvent,
) -> Result<ContextPressureState, String> {
    let fold = fold_surface_projection(state.claim, event)?;
    let mut next = state.clone();
    if let SessionEventData::RequestContext(holder) = &event.data {
        let context_window = holder.context.context_window;
        if context_window != next.context_window {
            next.context_window = context_window;
        }
    }
    if let Some(usage) = usage_of(event) {
        let pressure_tokens = pressure_from(&usage);
        if Some(pressure_tokens) != next.pressure_tokens
            || next.sampled_surface_tokens != Some(next.surface_tokens)
        {
            next.pressure_tokens = Some(pressure_tokens);
            next.sampled_surface_tokens = Some(next.surface_tokens);
        }
    }
    if fold.delta_tokens != 0 {
        let moved = i64::try_from(next.surface_tokens)
            .unwrap_or(i64::MAX)
            .saturating_add(fold.delta_tokens)
            .max(0);
        next.surface_tokens = u64::try_from(moved).unwrap_or(u64::MAX);
    }
    if state.claim.is_none() && fold.claim.is_none() {
        return Ok(next);
    }
    next.claim = fold.claim;
    Ok(next)
}

/// Fold a full log through the `tokenUsage` unit (the relay fold
/// endpoint face).
#[must_use]
pub fn fold_token_usage(events: &[SessionEvent]) -> TokenUsageState {
    events
        .iter()
        .fold(TokenUsageState::default(), |state, event| {
            apply_token_usage(&state, event)
        })
}

/// Fold a full log through the `contextPressure` unit (the relay fold
/// endpoint face).
///
/// # Errors
///
/// Returns the shadow-price fold refusal.
pub fn fold_context_pressure(events: &[SessionEvent]) -> Result<ContextPressureState, String> {
    events
        .iter()
        .try_fold(ContextPressureState::default(), |state, event| {
            apply_context_pressure(&state, event)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn usage(input: u64, output: u64) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            output_tokens: output,
            ..TokenUsage::default()
        }
    }

    fn assistant_event(seq: u64, turn: u64, step: u64, usage: Option<TokenUsage>) -> SessionEvent {
        SessionEvent {
            seq: fr_wire::SessionSeq::new(seq).unwrap_or_else(|e| panic!("seq {seq}: {e}")),
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
                                provider: "p".to_owned(),
                                model: "m".to_owned(),
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
            surface_op: Some(fr_wire::event::SurfaceOp::Append(fr_wire::event::AppendOp)),
            source_event_seqs: None,
        }
    }

    /// One settlement adds its buckets; the same (turn, step) replaces
    /// them instead of double-adding (the dsh addReplacing rule).
    #[test]
    fn settlements_add_and_replace_by_turn_step() {
        let state = TokenUsageState::default();
        let first = assistant_event(1, 1, 0, Some(usage(10, 5)));
        let state = apply_token_usage(&state, &first);
        assert_eq!(
            state.totals,
            TokenUsageBuckets {
                uncached_input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            }
        );
        // Same slot reports the SAME buckets: no change at all.
        let same = assistant_event(2, 1, 0, Some(usage(10, 5)));
        let state = apply_token_usage(&state, &same);
        assert_eq!(state.totals.output_tokens, 5);
        // Same slot reports CHANGED buckets: replace, don't accumulate.
        let corrected = assistant_event(3, 1, 0, Some(usage(12, 5)));
        let state = apply_token_usage(&state, &corrected);
        assert_eq!(state.totals.uncached_input_tokens, 12);
        assert_eq!(state.totals.output_tokens, 5);
        // A new step adds on top.
        let next_step = assistant_event(4, 1, 1, Some(usage(7, 3)));
        let state = apply_token_usage(&state, &next_step);
        assert_eq!(state.totals.uncached_input_tokens, 19);
        assert_eq!(state.totals.output_tokens, 8);
    }

    /// The retry-started marker closes the replacement slot so the
    /// retried attempt adds to the total.
    #[test]
    fn retry_started_closes_the_slot() {
        let state = TokenUsageState::default();
        let first = assistant_event(1, 1, 0, Some(usage(10, 5)));
        let state = apply_token_usage(&state, &first);
        let retry_marker = SessionEvent {
            seq: fr_wire::SessionSeq::new(2).unwrap_or_else(|e| panic!("seq: {e}")),
            time: 1,
            data: SessionEventData::Unknown(fr_wire::event::UnknownEventData {
                type_name: "llm/retry-started".to_owned(),
                data: serde_json::from_value(json!({"turn": 1, "step": 0}))
                    .unwrap_or_else(|e| panic!("data: {e}")),
            }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        };
        let state = apply_token_usage(&state, &retry_marker);
        assert_eq!(state.last, None);
        // The retried attempt now adds instead of replacing.
        let retried = assistant_event(3, 1, 0, Some(usage(11, 5)));
        let state = apply_token_usage(&state, &retried);
        assert_eq!(state.totals.uncached_input_tokens, 21);
    }

    /// A usage sample stamps the pressure and anchors the surface total;
    /// the projected view adds the surface movement since the sample.
    #[test]
    fn pressure_stamps_and_projects() {
        let state = ContextPressureState::default();
        let settle = assistant_event(1, 1, 0, Some(usage(100, 20)));
        let state = apply_context_pressure(&state, &settle).expect("pressure apply");
        assert_eq!(state.pressure_tokens, Some(100));
        assert_eq!(state.sampled_surface_tokens, Some(0));

        // A shadow-price arm plus a claimed replacement moves the
        // surface total after the sample.
        let shadow = SessionEvent {
            seq: fr_wire::SessionSeq::new(2).unwrap_or_else(|e| panic!("seq: {e}")),
            time: 1,
            data: SessionEventData::Unknown(fr_wire::event::UnknownEventData {
                type_name: "compaction/prune".to_owned(),
                data: serde_json::from_value(json!({
                    "shadowedRange": {"start": 0, "end": 0},
                    "shadowedTokenCount": 60,
                    "shadowedSeqs": [0],
                }))
                .unwrap_or_else(|e| panic!("data: {e}")),
            }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        };
        let state = apply_context_pressure(&state, &shadow).expect("shadow apply");
        let replacement = {
            let mut event = assistant_event(3, 1, 1, None);
            event.surface_op = Some(fr_wire::event::SurfaceOp::Replace(
                fr_wire::event::ReplaceOp {
                    op: fr_wire::event::ReplaceTag,
                    start_seq: fr_wire::SessionSeq::new(0).unwrap_or_else(|e| panic!("seq: {e}")),
                    end_seq: fr_wire::SessionSeq::new(0).unwrap_or_else(|e| panic!("seq: {e}")),
                },
            ));
            event
        };
        let state = apply_context_pressure(&state, &replacement).expect("replace apply");
        // The replacement's own message is empty-content assistant: 0
        // heuristic tokens; delta = 0 - 60.
        assert_eq!(state.surface_tokens, 0);
        let view = crate::projection::ContextPressureProjection::view(&state);
        assert_eq!(view.pressure_tokens, Some(100));
        assert_eq!(view.projected_tokens, Some(100));
    }
}
