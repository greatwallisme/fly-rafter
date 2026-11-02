//! The context-pressure projection's O(1) surface-token fold (dsh
//! `token-meter/surface-projection.ts` 1:1).
//!
//! Its checkpoint keeps scalar totals rather than the retained surface
//! used by measurement. Replacements ride the compact seam's
//! shadow-price protocol: the metering event immediately before a
//! surface `replace` (`compaction/summary` or `compaction/prune`)
//! states the heuristic price of the exact replaced range, so the fold
//! keeps a running total plus at most one pending claim and never
//! retains per-node prices. The counts are exact by construction:
//! producers derive them from the same fixed estimator this module
//! prices appends with. A replacement without an armed claim folds with
//! zero delta because bounded state cannot reconstruct the replaced
//! range; this preserves replay at the cost of possible drift.

#![forbid(unsafe_code)]

use fr_wire::event::{SessionEvent, SessionEventData, SurfaceOp};

/// One armed shadow price: the heuristic tokens of the surface range
/// the IMMEDIATELY following event replaces. Plain JSON — it is part of
/// the persisted unit state while armed (dsh `ShadowPriceClaim`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShadowPriceClaim {
    /// Declared inclusive first surface-node seq of the priced range.
    pub start: u64,
    /// Declared inclusive last surface-node seq of the priced range.
    pub end: u64,
    /// Heuristic tokens of the priced range under the fixed estimator.
    pub tokens: u64,
}

/// One event's effect on a running surface-token total (dsh
/// `SurfaceTokensFold`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceTokensFold {
    /// Signed change in the surface total; 0 for events off the
    /// surface.
    pub delta_tokens: i64,
    /// Claim to carry into the next event; `None` when none survives.
    pub claim: Option<ShadowPriceClaim>,
}

/// Reads one `compaction/summary` / `compaction/prune` payload's
/// shadow-price triple from the verbatim retained data (the event types
/// are plugin-declared, so the fr envelope keeps them `Unknown`).
fn shadow_price_of(
    type_name: &str,
    data: &fr_wire::content::RawObject,
) -> Option<ShadowPriceClaim> {
    if type_name != "compaction/summary" && type_name != "compaction/prune" {
        return None;
    }
    let range = data.get("shadowedRange")?;
    let start = range.get("start")?.as_u64()?;
    let end = range.get("end")?.as_u64()?;
    let tokens = data.get("shadowedTokenCount")?.as_u64()?;
    Some(ShadowPriceClaim { start, end, tokens })
}

/// Fold one committed event onto a running surface-token total (dsh
/// `foldSurfaceProjection`).
///
/// A shadow-price event arms a claim; any other event expires it, and a
/// surface `replace` consumes the claim naming its exact range — the
/// producers append the metering event and the replacement
/// synchronously adjacent, so a surviving claim always prices the very
/// next event. A replace with no claim folds with zero delta because
/// the bounded state cannot reconstruct the replaced range. An armed
/// claim for another range still fails because the adjacent events
/// contradict each other.
///
/// # Errors
///
/// Returns the dsh verbatim text when a replacement arrives with an
/// armed claim for a different range: the metering event was adjacent,
/// so this is a live producer's shadow-price contract violation, not
/// historical data, and must fail loud rather than let the total drift.
pub fn fold_surface_projection(
    claim: Option<ShadowPriceClaim>,
    event: &SessionEvent,
) -> Result<SurfaceTokensFold, String> {
    if let SessionEventData::Unknown(unknown) = &event.data
        && let Some(armed) = shadow_price_of(&unknown.type_name, &unknown.data)
    {
        return Ok(SurfaceTokensFold {
            delta_tokens: 0,
            claim: Some(armed),
        });
    }
    let surface_op = match event.surface_op {
        Some(op) if fr_session::surface::is_surface_eligible(event) => Some(op),
        _ => {
            return Ok(SurfaceTokensFold {
                delta_tokens: 0,
                claim: None,
            });
        }
    };
    let message = fr_session::derive_event_message(event);
    let tokens = message.map_or(0u64, crate::estimate::estimate_message);
    match surface_op {
        Some(SurfaceOp::Append(_)) => Ok(SurfaceTokensFold {
            delta_tokens: i64::try_from(tokens).unwrap_or(i64::MAX),
            claim: None,
        }),
        None => Ok(SurfaceTokensFold {
            delta_tokens: 0,
            claim: None,
        }),
        Some(SurfaceOp::Replace(replace)) => {
            let Some(armed) = claim else {
                // Sessions recorded before the shadow-price protocol log
                // replacements with no adjacent metering event; the
                // bounded state cannot reconstruct the replaced range's
                // price, so fold those neutrally — historical replay
                // degrades to drift instead of failing.
                return Ok(SurfaceTokensFold {
                    delta_tokens: 0,
                    claim: None,
                });
            };
            if armed.start != replace.start_seq.get() || armed.end != replace.end_seq.get() {
                return Err(format!(
                    "token surface: replace at seq {} over range {}-{} has no adjacent shadow price (armed claim covers {}-{})",
                    event.seq.get(),
                    replace.start_seq.get(),
                    replace.end_seq.get(),
                    armed.start,
                    armed.end
                ));
            }
            Ok(SurfaceTokensFold {
                delta_tokens: i64::try_from(tokens).unwrap_or(i64::MAX)
                    - i64::try_from(armed.tokens).unwrap_or(i64::MAX),
                claim: None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::estimate::ROLE_OVERHEAD;
    use fr_wire::event::AppendOp;
    use fr_wire::message::{KnownMessage, KnownMessageSource, Message, MessageSource};
    use serde_json::json;

    fn text_user_event(seq: u64, text: &str) -> SessionEvent {
        SessionEvent {
            seq: fr_wire::SessionSeq::new(seq).unwrap_or_else(|e| panic!("seq {seq}: {e}")),
            time: 1,
            data: SessionEventData::UserMessage(Message::Known(Box::new(KnownMessage::User {
                id: fr_wire::ids::MessageId::new("m"),
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

    fn shadow_event(seq: u64, start: u64, end: u64, tokens: u64) -> SessionEvent {
        SessionEvent {
            seq: fr_wire::SessionSeq::new(seq).unwrap_or_else(|e| panic!("seq {seq}: {e}")),
            time: 1,
            data: SessionEventData::Unknown(fr_wire::event::UnknownEventData {
                type_name: "compaction/summary".to_owned(),
                data: serde_json::from_value(json!({
                    "shadowedRange": {"start": start, "end": end},
                    "shadowedTokenCount": tokens,
                    "shadowedSeqs": [start, end],
                }))
                .unwrap_or_else(|e| panic!("shadow data: {e}")),
            }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }
    }

    /// A shadow-price event arms the claim with zero delta; a plain
    /// non-surface event expires it.
    #[test]
    fn shadow_arms_and_others_expire() {
        let shadow = shadow_event(1, 3, 5, 42);
        let fold = fold_surface_projection(None, &shadow).expect("shadow fold");
        assert_eq!(fold.delta_tokens, 0);
        assert_eq!(
            fold.claim,
            Some(ShadowPriceClaim {
                start: 3,
                end: 5,
                tokens: 42
            })
        );

        let turn = SessionEvent {
            seq: fr_wire::SessionSeq::new(2).unwrap_or_else(|e| panic!("seq: {e}")),
            time: 1,
            data: SessionEventData::TurnStart(fr_wire::event::TurnStartData { turn: 1 }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        };
        let fold = fold_surface_projection(fold.claim, &turn).expect("expire fold");
        assert_eq!(fold.delta_tokens, 0);
        assert_eq!(fold.claim, None);
    }

    /// An append prices its message; a claimed replacement prices
    /// `tokens - claim`; an unclaimed one folds neutral zero.
    #[test]
    fn appends_and_claimed_replaces_price() {
        let claim = ShadowPriceClaim {
            start: 0,
            end: 0,
            tokens: 100,
        };
        let replace = {
            let mut event = text_user_event(1, "abcd");
            event.surface_op = Some(SurfaceOp::Replace(fr_wire::event::ReplaceOp {
                op: fr_wire::event::ReplaceTag,
                start_seq: fr_wire::SessionSeq::new(0).unwrap_or_else(|e| panic!("seq: {e}")),
                end_seq: fr_wire::SessionSeq::new(0).unwrap_or_else(|e| panic!("seq: {e}")),
            }));
            event
        };
        let tokens = crate::estimate::estimate_message(&match &replace.data {
            SessionEventData::UserMessage(message) => message.clone(),
            _ => panic!("user message"),
        });
        let fold = fold_surface_projection(Some(claim), &replace).expect("claimed replace");
        assert_eq!(
            fold.delta_tokens,
            i64::try_from(tokens).unwrap_or(i64::MAX) - 100
        );
        assert_eq!(fold.claim, None);

        let fold = fold_surface_projection(None, &replace).expect("unclaimed replace");
        assert_eq!(fold.delta_tokens, 0);

        let append = text_user_event(2, "abcd");
        let fold = fold_surface_projection(None, &append).expect("append");
        assert_eq!(
            fold.delta_tokens,
            i64::try_from(
                4_u64
                    .div_ceil(4)
                    .saturating_add(4)
                    .saturating_add(ROLE_OVERHEAD),
            )
            .unwrap_or(i64::MAX)
        );
    }

    /// A replacement whose armed claim names another range fails loud
    /// with the dsh verbatim text.
    #[test]
    fn mismatched_claim_fails_loud() {
        let claim = ShadowPriceClaim {
            start: 1,
            end: 2,
            tokens: 10,
        };
        let mut replace = text_user_event(3, "x");
        replace.surface_op = Some(SurfaceOp::Replace(fr_wire::event::ReplaceOp {
            op: fr_wire::event::ReplaceTag,
            start_seq: fr_wire::SessionSeq::new(4).unwrap_or_else(|e| panic!("seq: {e}")),
            end_seq: fr_wire::SessionSeq::new(6).unwrap_or_else(|e| panic!("seq: {e}")),
        }));
        let error = fold_surface_projection(Some(claim), &replace).expect_err("mismatch");
        assert_eq!(
            error,
            "token surface: replace at seq 3 over range 4-6 has no adjacent shadow price (armed claim covers 1-2)"
        );
    }
}
