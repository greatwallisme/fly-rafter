//! Heuristic composition of the current retained surface, independent
//! of route image pricing and provider usage (dsh
//! `token-meter/breakdown-projection.ts` 1:1): the `contextBreakdown`
//! unit (stateVersion 6).
//!
//! The state keeps the surviving system prompts in surface order and one
//! running surface total; every other node is priced only as a delta, so
//! state size follows the system prompts, not the message count.
//! Appends add their price; a replacement subtracts the shadow price of
//! the adjacent `compaction/summary` or `compaction/prune` event, whose
//! `shadowedSeqs` also name the system prompts it removes, and a
//! system-prompt rewrite subtracts the retained price of the system node
//! it replaces. Tools are priced from the latest request header. No
//! route pricing applies.

#![forbid(unsafe_code)]

use fr_wire::event::{SessionEvent, SessionEventData, SurfaceOp};
use serde_json::Value;

use crate::estimate::{estimate_message, estimate_tools_tokens};
use fr_session::canonical_header;

/// One surviving `system/message` node and its heuristic price (the
/// `systemNodeSchema` shape).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SystemNode {
    /// Durable sequence number of the surface event.
    pub seq: u64,
    /// The node's heuristic price.
    pub tokens: u64,
}

/// The published composition (the `breakdownSchema` shape).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Breakdown {
    /// Heuristic tokens of the last nonempty surviving system prompt in
    /// surface order; 0 when none exists.
    pub system_tokens: u64,
    /// Heuristic tokens of the newest request envelope's tool schemas;
    /// 0 before any request.
    pub tools_tokens: u64,
    /// Heuristic tokens of every other visible surface node, including
    /// superseded system prompts.
    pub message_tokens: u64,
}

/// Shadow price armed by the `compaction/summary` or `compaction/prune`
/// event immediately before a replacement: the replaced range, its
/// heuristic price, and the surviving system prompts inside it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BreakdownClaim {
    /// Declared inclusive first surface-node seq of the priced range.
    pub start: u64,
    /// Declared inclusive last surface-node seq of the priced range.
    pub end: u64,
    /// Heuristic tokens of the priced range under the fixed estimator.
    pub tokens: u64,
    /// The system prompts the metering event removes.
    pub system_seqs: Vec<u64>,
}

/// Plain-JSON checkpoint: surviving system prompts in surface order, the
/// surface total, and an armed claim (the state schema verbatim).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextBreakdownState {
    /// The surviving system prompts in surface order.
    pub systems: Vec<SystemNode>,
    /// The running surface total (clamped non-negative).
    pub surface_tokens: u64,
    /// The claim armed by the immediately preceding metering event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<BreakdownClaim>,
    /// The published composition.
    pub breakdown: Breakdown,
}

/// Place one surface event's priced node (the dsh `placeSurfaceEvent`).
///
/// # Errors
///
/// Returns the dsh verbatim texts when a metered replacement names
/// another range than its claim, or puts a system message in place of a
/// metered range.
fn place_surface_event(
    state: &ContextBreakdownState,
    claim: Option<&BreakdownClaim>,
    surface_op: SurfaceOp,
    event: &SessionEvent,
    tokens: u64,
) -> Result<(Vec<SystemNode>, u64), String> {
    let is_system = matches!(&event.data, SessionEventData::SystemMessage(_));
    let SurfaceOp::Replace(replace) = surface_op else {
        let mut systems = state.systems.clone();
        if is_system {
            systems.push(SystemNode {
                seq: event.seq.get(),
                tokens,
            });
        }
        let total = state.surface_tokens.saturating_add(tokens);
        return Ok((systems, total));
    };
    if let Some(claim) = claim {
        if claim.start != replace.start_seq.get() || claim.end != replace.end_seq.get() {
            return Err(format!(
                "context breakdown: replace at seq {} over range {}-{} has no adjacent shadow price (armed claim covers {}-{})",
                event.seq.get(),
                replace.start_seq.get(),
                replace.end_seq.get(),
                claim.start,
                claim.end
            ));
        }
        if is_system {
            return Err(format!(
                "context breakdown: replace at seq {} puts a system message in place of a metered range; its order among the surviving system prompts is unknown",
                event.seq.get()
            ));
        }
        let shadowed: std::collections::HashSet<u64> = claim.system_seqs.iter().copied().collect();
        let systems = state
            .systems
            .iter()
            .filter(|system| !shadowed.contains(&system.seq))
            .copied()
            .collect();
        let total = state
            .surface_tokens
            .saturating_add(tokens)
            .saturating_sub(claim.tokens);
        return Ok((systems, total));
    }
    let index = if replace.start_seq == replace.end_seq {
        state
            .systems
            .iter()
            .position(|system| system.seq == replace.start_seq.get())
    } else {
        None
    };
    let Some(index) = index else {
        // Bounded state cannot price an unmetered non-system range, so
        // it folds at zero delta; current producers meter every such
        // replacement.
        return Ok((state.systems.clone(), state.surface_tokens));
    };
    let replaced = state.systems[index].tokens;
    let mut systems = state.systems.clone();
    if is_system {
        systems[index] = SystemNode {
            seq: event.seq.get(),
            tokens,
        };
    } else {
        systems.remove(index);
    }
    let total = state
        .surface_tokens
        .saturating_add(tokens)
        .saturating_sub(replaced);
    Ok((systems, total))
}

/// Publish totals, keeping the previous breakdown object when its
/// figures are unchanged (the dsh `withTotals`).
fn with_totals(
    state: &ContextBreakdownState,
    systems: Vec<SystemNode>,
    surface_tokens: u64,
) -> ContextBreakdownState {
    let system_tokens = systems
        .iter()
        .rev()
        .find(|system| system.tokens > 0)
        .map_or(0, |system| system.tokens);
    let message_tokens = surface_tokens.saturating_sub(system_tokens);
    let breakdown = if system_tokens == state.breakdown.system_tokens
        && message_tokens == state.breakdown.message_tokens
    {
        state.breakdown
    } else {
        Breakdown {
            system_tokens,
            tools_tokens: state.breakdown.tools_tokens,
            message_tokens,
        }
    };
    ContextBreakdownState {
        systems,
        surface_tokens,
        claim: None,
        breakdown,
    }
}

/// Apply one event to the `contextBreakdown` unit state (the dsh
/// `apply`). A claim prices only the event immediately after its
/// metering event.
///
/// # Errors
///
/// Returns the dsh verbatim texts from the placement arm; the full-log
/// fold surfaces them to the relay caller.
pub fn apply_context_breakdown(
    state: &ContextBreakdownState,
    event: &SessionEvent,
) -> Result<ContextBreakdownState, String> {
    if let fr_wire::event::SessionEventData::Unknown(unknown) = &event.data
        && (unknown.type_name == "compaction/summary" || unknown.type_name == "compaction/prune")
    {
        let shadowed: std::collections::HashSet<u64> = unknown
            .data
            .get("shadowedSeqs")
            .and_then(|seqs| seqs.as_array())
            .map(|seqs| seqs.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default();
        let range = unknown.data.get("shadowedRange");
        let start = range.and_then(|r| r.get("start")).and_then(Value::as_u64);
        let end = range.and_then(|r| r.get("end")).and_then(Value::as_u64);
        let tokens = unknown
            .data
            .get("shadowedTokenCount")
            .and_then(Value::as_u64);
        let (Some(start), Some(end), Some(tokens)) = (start, end, tokens) else {
            return Ok(state.clone());
        };
        let system_seqs = state
            .systems
            .iter()
            .filter(|system| shadowed.contains(&system.seq))
            .map(|system| system.seq)
            .collect();
        let mut next = state.clone();
        next.claim = Some(BreakdownClaim {
            start,
            end,
            tokens,
            system_seqs,
        });
        return Ok(next);
    }
    let mut unclaimed = state.clone();
    unclaimed.claim = None;
    if let SessionEventData::RequestHeader(data) = &event.data {
        let tools_tokens = estimate_tools_tokens(Some(&canonical_header(&data.header)));
        if tools_tokens != unclaimed.breakdown.tools_tokens {
            unclaimed.breakdown.tools_tokens = tools_tokens;
        }
        return Ok(unclaimed);
    }
    if !fr_session::surface::is_surface_eligible(event) {
        return Ok(unclaimed);
    }
    let Some(surface_op) = event.surface_op else {
        return Ok(unclaimed);
    };
    let message = fr_session::derive_event_message(event);
    let tokens = message.map_or(0, estimate_message);
    let (systems, surface_tokens) =
        place_surface_event(&unclaimed, state.claim.as_ref(), surface_op, event, tokens)?;
    Ok(with_totals(&unclaimed, systems, surface_tokens))
}

/// Fold a full log through the `contextBreakdown` unit (the relay fold
/// endpoint face).
///
/// # Errors
///
/// Returns the first placement refusal the log raises.
pub fn fold_context_breakdown(events: &[SessionEvent]) -> Result<ContextBreakdownState, String> {
    events
        .iter()
        .try_fold(ContextBreakdownState::default(), |state, event| {
            apply_context_breakdown(&state, event)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::estimate::ROLE_OVERHEAD;
    use fr_wire::event::AppendOp;
    use fr_wire::message::{KnownMessage, KnownMessageSource, Message, MessageSource};
    use serde_json::json;

    fn system_event(seq: u64, text: &str) -> SessionEvent {
        SessionEvent {
            seq: fr_wire::SessionSeq::new(seq).unwrap_or_else(|e| panic!("seq {seq}: {e}")),
            time: 1,
            data: SessionEventData::SystemMessage(fr_wire::event::SystemMessageData {
                turn: 1,
                step: 0,
                message: Message::Known(Box::new(KnownMessage::System {
                    id: fr_wire::ids::MessageId::new("s"),
                    content: vec![fr_wire::content::ContentBlock::Known(
                        fr_wire::content::KnownContentBlock::Text {
                            text: text.to_owned(),
                        },
                    )],
                    source: MessageSource::Known(KnownMessageSource::User {
                        extra: fr_wire::content::RawObject::new(),
                    }),
                    extra: fr_wire::content::RawObject::new(),
                })),
            }),
            ignorable: None,
            surface_op: Some(SurfaceOp::Append(AppendOp)),
            source_event_seqs: None,
        }
    }

    fn user_event(seq: u64, text: &str) -> SessionEvent {
        SessionEvent {
            seq: fr_wire::SessionSeq::new(seq).unwrap_or_else(|e| panic!("seq {seq}: {e}")),
            time: 1,
            data: SessionEventData::UserMessage(Message::Known(Box::new(KnownMessage::User {
                id: fr_wire::ids::MessageId::new("u"),
                content: vec![fr_wire::content::ContentBlock::Known(
                    fr_wire::content::KnownContentBlock::Text {
                        text: text.to_owned(),
                    },
                )],
                source: MessageSource::Known(KnownMessageSource::User {
                    extra: fr_wire::content::RawObject::new(),
                }),
                extra: fr_wire::content::RawObject::new(),
            }))),
            ignorable: None,
            surface_op: Some(SurfaceOp::Append(AppendOp)),
            source_event_seqs: None,
        }
    }

    fn header_event(seq: u64, tool_name: &str) -> SessionEvent {
        let header = fr_wire::event::EpochHeader {
            config: fr_wire::call_config::CallConfig::default(),
            adapter_defaults: None,
            tools: Some(vec![fr_wire::call_config::ToolSchema {
                defer_loading: None,
                name: tool_name.to_owned(),
                description: "Reads.".to_owned(),
                parameters: fr_wire::content::RawObject::new(),
            }]),
        };
        SessionEvent {
            seq: fr_wire::SessionSeq::new(seq).unwrap_or_else(|e| panic!("seq {seq}: {e}")),
            time: 1,
            data: SessionEventData::RequestHeader(fr_wire::event::RequestHeaderData {
                header,
                reason: fr_wire::event::RequestHeaderReason::Initial,
                starts_series: None,
            }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }
    }

    /// System prompts classify as system tokens (last nonempty
    /// surviving); everything else is message tokens; tools price from
    /// the newest request header.
    #[test]
    fn composition_splits_system_tools_message() {
        let state = ContextBreakdownState::default();
        let system = system_event(1, "You are an agent.");
        let state = apply_context_breakdown(&state, &system).expect("system apply");
        let user = user_event(2, "hello world now");
        let state = apply_context_breakdown(&state, &user).expect("user apply");
        let header = header_event(3, "read");
        let state = apply_context_breakdown(&state, &header).expect("header apply");

        let system_tokens = crate::estimate::estimate_message(&match &system.data {
            SessionEventData::SystemMessage(data) => data.message.clone(),
            _ => panic!("system message"),
        });
        let message_tokens = crate::estimate::estimate_message(&match &user.data {
            SessionEventData::UserMessage(message) => message.clone(),
            _ => panic!("user message"),
        });
        let tools_tokens = crate::estimate::estimate_tools_tokens(Some(
            &fr_session::canonical_header(&match &header.data {
                SessionEventData::RequestHeader(data) => data.header.clone(),
                _ => panic!("header"),
            }),
        ));
        assert_eq!(state.breakdown.system_tokens, system_tokens);
        assert_eq!(state.breakdown.message_tokens, message_tokens);
        assert_eq!(state.breakdown.tools_tokens, tools_tokens);
        assert_eq!(
            state.surface_tokens,
            system_tokens.saturating_add(message_tokens)
        );
        assert_eq!(state.systems.len(), 1);
        let _ = ROLE_OVERHEAD;
    }

    /// A claimed replacement subtracts the shadow price and drops the
    /// shadowed system prompts; a metered system replacement refuses
    /// with the dsh verbatim text.
    #[test]
    fn claimed_replaces_shadow_and_refuse_system_swaps() {
        let state = ContextBreakdownState::default();
        let state =
            apply_context_breakdown(&state, &system_event(1, "prompt one text")).expect("system 1");
        let state = apply_context_breakdown(&state, &user_event(2, "body text")).expect("user");
        let shadow = SessionEvent {
            seq: fr_wire::SessionSeq::new(3).unwrap_or_else(|e| panic!("seq: {e}")),
            time: 1,
            data: SessionEventData::Unknown(fr_wire::event::UnknownEventData {
                type_name: "compaction/summary".to_owned(),
                data: serde_json::from_value(json!({
                    "shadowedRange": {"start": 1, "end": 2},
                    "shadowedTokenCount": 0,
                    "shadowedSeqs": [1, 2],
                }))
                .unwrap_or_else(|e| panic!("data: {e}")),
            }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        };
        let state = apply_context_breakdown(&state, &shadow).expect("shadow");
        assert!(
            state
                .claim
                .as_ref()
                .is_some_and(|claim| claim.system_seqs == vec![1])
        );

        // The checkpoint user message replaces the range: system prompt
        // 1 drops out of the survivors.
        let mut checkpoint = user_event(4, "checkpoint body");
        checkpoint.surface_op = Some(SurfaceOp::Replace(fr_wire::event::ReplaceOp {
            op: fr_wire::event::ReplaceTag,
            start_seq: fr_wire::SessionSeq::new(1).unwrap_or_else(|e| panic!("seq: {e}")),
            end_seq: fr_wire::SessionSeq::new(2).unwrap_or_else(|e| panic!("seq: {e}")),
        }));
        let state = apply_context_breakdown(&state, &checkpoint).expect("checkpoint");
        assert_eq!(state.systems.len(), 0);

        // A system message replacing a metered range refuses.
        let shadow = SessionEvent {
            seq: fr_wire::SessionSeq::new(5).unwrap_or_else(|e| panic!("seq: {e}")),
            time: 1,
            data: SessionEventData::Unknown(fr_wire::event::UnknownEventData {
                type_name: "compaction/summary".to_owned(),
                data: serde_json::from_value(json!({
                    "shadowedRange": {"start": 4, "end": 4},
                    "shadowedTokenCount": 5,
                    "shadowedSeqs": [],
                }))
                .unwrap_or_else(|e| panic!("data: {e}")),
            }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        };
        let state = apply_context_breakdown(&state, &shadow).expect("shadow 2");
        let mut system_swap = system_event(6, "new prompt");
        system_swap.surface_op = Some(SurfaceOp::Replace(fr_wire::event::ReplaceOp {
            op: fr_wire::event::ReplaceTag,
            start_seq: fr_wire::SessionSeq::new(4).unwrap_or_else(|e| panic!("seq: {e}")),
            end_seq: fr_wire::SessionSeq::new(4).unwrap_or_else(|e| panic!("seq: {e}")),
        }));
        let error = apply_context_breakdown(&state, &system_swap).expect_err("system swap refuses");
        assert_eq!(
            error,
            "context breakdown: replace at seq 6 puts a system message in place of a metered range; its order among the surviving system prompts is unknown"
        );
    }

    /// An unmetered single-node system rewrite subtracts the retained
    /// price of the node it replaces.
    #[test]
    fn unmetered_system_rewrite_folds_retained_price() {
        let state = ContextBreakdownState::default();
        let state =
            apply_context_breakdown(&state, &system_event(1, "prompt one text")).expect("system");
        let original = state.surface_tokens;
        let mut rewrite = system_event(2, "prompt two text");
        rewrite.surface_op = Some(SurfaceOp::Replace(fr_wire::event::ReplaceOp {
            op: fr_wire::event::ReplaceTag,
            start_seq: fr_wire::SessionSeq::new(1).unwrap_or_else(|e| panic!("seq: {e}")),
            end_seq: fr_wire::SessionSeq::new(1).unwrap_or_else(|e| panic!("seq: {e}")),
        }));
        let state = apply_context_breakdown(&state, &rewrite).expect("rewrite");
        let rewritten = crate::estimate::estimate_message(&match &rewrite.data {
            SessionEventData::SystemMessage(data) => data.message.clone(),
            _ => panic!("system message"),
        });
        assert_eq!(
            state.surface_tokens,
            original.saturating_sub(original).saturating_add(rewritten)
        );
        assert_eq!(state.systems[0].seq, 2);
    }
}
