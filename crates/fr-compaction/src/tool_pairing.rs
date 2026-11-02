//! Tool-pairing balance over a session surface (dsh tool-pairing.ts 1:1).
//!
//! Compaction changes surface positions, so safe cuts are derived from
//! tool-call/result content in current surface order rather than step
//! markers.

use std::collections::HashMap;

use fr_wire::message::{KnownMessage, Message};
use fr_wire::{ContentBlock, KnownContentBlock, SessionEvent, SessionEventData, SessionSeq};

/// One tool-pairing balance failure (dsh throws plain `Error`s; the fr type
/// carries the verbatim message).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPairingError(String);

impl std::fmt::Display for ToolPairingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ToolPairingError {}

/// Incremental balance state for one session surface generation.
///
/// dsh keeps one cache per live session in a module-level `WeakMap`; fr
/// hands the state to the caller, which keys it by session id (eviction
/// only forces a from-scratch rebuild — the answers stay identical).
#[derive(Debug, Clone)]
pub struct BalanceCache {
    /// Surface rewrite generation this state describes.
    generation: u64,
    /// Balance of every surface cut in current order: a surface of N
    /// sequences has N + 1 cuts, entry `i` being the cut before sequence
    /// `i` and the final entry the cut after the surface tail.
    cut_balanced: Vec<bool>,
    /// Current surface position of each event seq.
    index_by_seq: HashMap<SessionSeq, usize>,
    /// In-progress tool-call count after the processed surface tail.
    in_progress_tool_calls: i64,
}

/// Return how one surface event changes the in-progress tool-call count.
fn event_delta(event: &SessionEvent) -> i64 {
    match &event.data {
        SessionEventData::AssistantMessage(data) => match &data.message {
            Message::Known(known) => match known.as_ref() {
                KnownMessage::Assistant { content, .. } => {
                    #[expect(
                        clippy::cast_possible_wrap,
                        reason = "the block count is a content length, far below the i64 range"
                    )]
                    let delta = content
                        .iter()
                        .filter(|block| {
                            matches!(
                                block,
                                ContentBlock::Known(KnownContentBlock::ToolCall { .. })
                            )
                        })
                        .count() as i64;
                    delta
                }
                _ => 0,
            },
            Message::Other(_) => 0,
        },
        SessionEventData::ToolResult(_) => -1,
        _ => 0,
    }
}

/// Return balance state synchronized with the given surface snapshot (dsh
/// `balanceCache()`).
///
/// Rebuilds when the cached state describes a different generation or more
/// cuts than the current surface holds, extends over the unseen tail
/// otherwise, and answers from the cache untouched when it already covers
/// the whole surface. `events` is the session's durable log, searched by
/// seq.
///
/// # Errors
///
/// Returns the dsh verbatim failure when a surface sequence has no matching
/// log event, indexes a different event, or a tool result has no preceding
/// open call.
pub fn balance_cache(
    cached: Option<BalanceCache>,
    generation: u64,
    seqs: &[SessionSeq],
    events: &[SessionEvent],
) -> Result<BalanceCache, ToolPairingError> {
    if let Some(cache) = cached
        && cache.generation == generation
        && cache.cut_balanced.len() - 1 <= seqs.len()
    {
        if cache.cut_balanced.len() - 1 == seqs.len() {
            return Ok(cache);
        }
        return extend_cache(cache, generation, seqs, events);
    }
    // A rebuild is the same fold started from the empty-surface state,
    // whose single leading cut is trivially balanced.
    extend_cache(
        BalanceCache {
            generation,
            cut_balanced: vec![true],
            index_by_seq: HashMap::new(),
            in_progress_tool_calls: 0,
        },
        generation,
        seqs,
        events,
    )
}

/// Fold surface sequences not yet in the cache into its balance state.
fn extend_cache(
    mut cache: BalanceCache,
    generation: u64,
    seqs: &[SessionSeq],
    events: &[SessionEvent],
) -> Result<BalanceCache, ToolPairingError> {
    let processed = cache.cut_balanced.len() - 1;
    let tail = seqs.get(processed..).unwrap_or(&[]);
    // Validate the unseen tail before mutating the live cache, so a corrupt
    // append cannot leave a partially advanced state behind.
    let mut pending_cuts = Vec::with_capacity(tail.len());
    let mut in_progress_tool_calls = cache.in_progress_tool_calls;
    for seq in tail {
        // The log is seq-ordered (append-only), so the lookup is a binary
        // search; the seq equality check guards a future seam that no
        // longer reads the log in place (dsh eventAt contract, kept
        // verbatim).
        let event = event_at(events, *seq).ok_or_else(|| {
            ToolPairingError(format!(
                "tool-pairing balance: surface seq {} has no matching session \
                 event (corrupt surface)",
                seq.get()
            ))
        })?;
        if event.seq != *seq {
            return Err(ToolPairingError(format!(
                "tool-pairing balance: surface seq {} has no matching session \
                 event (corrupt surface)",
                seq.get()
            )));
        }
        in_progress_tool_calls += event_delta(event);
        if in_progress_tool_calls < 0 {
            return Err(ToolPairingError(format!(
                "tool-pairing balance: tool/result at surface seq {} has no \
                 matching tool-call (corrupt surface)",
                seq.get()
            )));
        }
        pending_cuts.push(in_progress_tool_calls == 0);
    }

    for (offset, seq) in tail.iter().enumerate() {
        cache.index_by_seq.insert(*seq, processed + offset);
    }
    cache.cut_balanced.extend(pending_cuts);
    cache.in_progress_tool_calls = in_progress_tool_calls;
    cache.generation = generation;
    Ok(cache)
}

/// Find one event by seq in the seq-ordered durable log.
fn event_at(events: &[SessionEvent], seq: SessionSeq) -> Option<&SessionEvent> {
    let index = events.binary_search_by_key(&seq, |event| event.seq).ok()?;
    Some(&events[index])
}

/// Balance of the cut at a sequence's position plus `offset` (0 = before,
/// 1 = after), rejecting seqs outside current membership.
fn cut_balance(
    cache: &BalanceCache,
    seq: SessionSeq,
    offset: usize,
) -> Result<bool, ToolPairingError> {
    let balanced = cache
        .index_by_seq
        .get(&seq)
        .map(|index| cache.cut_balanced[index + offset]);
    balanced.ok_or_else(|| {
        ToolPairingError(format!(
            "tool-pairing balance: surface seq {} not found",
            seq.get()
        ))
    })
}

/// Whether the cut immediately before a current surface sequence is
/// tool-pairing balanced: no unanswered tool call crosses the cut.
///
/// # Errors
///
/// Returns the dsh verbatim failure when the seq is absent from the current
/// surface.
pub fn tool_pairing_balanced_before(
    cache: &BalanceCache,
    seq: SessionSeq,
) -> Result<bool, ToolPairingError> {
    cut_balance(cache, seq, 0)
}

/// Whether the cut immediately after a current surface sequence is
/// tool-pairing balanced: no unanswered tool call crosses the cut.
///
/// # Errors
///
/// Returns the dsh verbatim failure when the seq is absent from the current
/// surface.
pub fn tool_pairing_balanced_after(
    cache: &BalanceCache,
    seq: SessionSeq,
) -> Result<bool, ToolPairingError> {
    cut_balance(cache, seq, 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fr_wire::content::RawObject;
    use fr_wire::ids::MessageId;
    use fr_wire::message::{KnownMessageSource, MessageSource};
    use fr_wire::{AppendOp, SurfaceOp};

    fn user_message() -> Message {
        Message::Known(Box::new(KnownMessage::User {
            id: MessageId::new("u"),
            content: vec![ContentBlock::Known(KnownContentBlock::Text {
                text: "go".to_owned(),
            })],
            source: MessageSource::Known(KnownMessageSource::User {
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        }))
    }

    fn assistant_tool_call_message(call_ids: &[&str]) -> Message {
        let content: Vec<ContentBlock> = call_ids
            .iter()
            .map(|id| {
                ContentBlock::Known(KnownContentBlock::ToolCall {
                    id: fr_wire::ToolCallId::new(*id),
                    name: "bash".to_owned(),
                    arguments: "{}".to_owned(),
                })
            })
            .collect();
        Message::Known(Box::new(KnownMessage::Assistant {
            id: MessageId::new("a"),
            content,
            source: MessageSource::Known(KnownMessageSource::Model {
                provider: "mock".to_owned(),
                model: "mock".to_owned(),
                replay_state: None,
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        }))
    }

    fn tool_result_message() -> Message {
        Message::Known(Box::new(KnownMessage::Tool {
            id: MessageId::new("t"),
            content: vec![],
            source: MessageSource::Known(KnownMessageSource::Tool {
                call_id: fr_wire::ToolCallId::new("c1"),
                extra: RawObject::new(),
            }),
            tool_call_id: fr_wire::ToolCallId::new("c1"),
            is_error: None,
            extra: RawObject::new(),
        }))
    }

    fn seq(value: u64) -> SessionSeq {
        SessionSeq::new(value).unwrap_or_else(|error| panic!("seq: {error}"))
    }

    fn seq_list(values: &[u64]) -> Vec<SessionSeq> {
        values.iter().map(|value| seq(*value)).collect()
    }

    fn append(event_type: SessionEventData, seq_value: u64, time: i64) -> SessionEvent {
        SessionEvent {
            seq: seq(seq_value),
            time,
            data: event_type,
            ignorable: None,
            surface_op: Some(SurfaceOp::Append(AppendOp)),
            source_event_seqs: None,
        }
    }

    fn user_event(seq_value: u64) -> SessionEvent {
        append(
            SessionEventData::UserMessage(user_message()),
            seq_value,
            seq_value.cast_signed(),
        )
    }

    fn tool_call_event(seq_value: u64, call_ids: &[&str]) -> SessionEvent {
        append(
            SessionEventData::AssistantMessage(fr_wire::event::AssistantMessageData {
                turn: 1,
                step: 1,
                message: assistant_tool_call_message(call_ids),
                stream: Vec::new(),
                usage: None,
                interrupted: None,
            }),
            seq_value,
            seq_value.cast_signed(),
        )
    }

    fn tool_result_event(seq_value: u64) -> SessionEvent {
        append(
            SessionEventData::ToolResult(fr_wire::ToolResultData {
                turn: 1,
                step: 1,
                message: tool_result_message(),
                error: None,
                meta: None,
            }),
            seq_value,
            seq_value.cast_signed(),
        )
    }

    /// The dsh `closedToolStep` fixture: user, one tool call, its result.
    fn closed_tool_step() -> (Vec<SessionEvent>, Vec<SessionSeq>) {
        let events = vec![
            user_event(0),
            tool_call_event(1, &["c1"]),
            tool_result_event(2),
        ];
        (events, seq_list(&[0, 1, 2]))
    }

    #[test]
    fn classifies_closed_and_open_single_call_steps() {
        let (events, seqs) = closed_tool_step();
        let cache = balance_cache(None, 0, &seqs, &events).expect("cache");
        assert!(tool_pairing_balanced_before(&cache, seq(0)).expect("before"));
        assert!(tool_pairing_balanced_after(&cache, seq(0)).expect("after"));
        assert!(tool_pairing_balanced_before(&cache, seq(1)).expect("before"));
        assert!(!tool_pairing_balanced_after(&cache, seq(1)).expect("after"));
        assert!(!tool_pairing_balanced_before(&cache, seq(2)).expect("before"));
        assert!(tool_pairing_balanced_after(&cache, seq(2)).expect("after"));

        let open_events = vec![tool_call_event(0, &["open"])];
        let open_seqs = seq_list(&[0]);
        let open = balance_cache(None, 0, &open_seqs, &open_events).expect("cache");
        assert!(!tool_pairing_balanced_after(&open, seq(0)).expect("after"));
    }

    #[test]
    fn requires_every_result_from_a_multiple_call_assistant_message() {
        let events = vec![
            tool_call_event(0, &["c1", "c2"]),
            tool_result_event(1),
            tool_result_event(2),
        ];
        let seqs = seq_list(&[0, 1, 2]);
        let cache = balance_cache(None, 0, &seqs, &events).expect("cache");
        assert!(!tool_pairing_balanced_after(&cache, seq(1)).expect("after"));
        assert!(tool_pairing_balanced_after(&cache, seq(2)).expect("after"));
    }

    #[test]
    fn keeps_neutral_nodes_inside_an_open_pair_unbalanced() {
        let events = vec![
            tool_call_event(0, &["c1"]),
            user_event(1),
            tool_result_event(2),
        ];
        let seqs = seq_list(&[0, 1, 2]);
        let cache = balance_cache(None, 0, &seqs, &events).expect("cache");
        assert!(!tool_pairing_balanced_before(&cache, seq(1)).expect("before"));
        assert!(!tool_pairing_balanced_after(&cache, seq(1)).expect("after"));

        let free_events = vec![user_event(0)];
        let free = balance_cache(None, 0, &[seq(0)], &free_events).expect("cache");
        assert!(tool_pairing_balanced_before(&free, seq(0)).expect("before"));
        assert!(tool_pairing_balanced_after(&free, seq(0)).expect("after"));
    }

    #[test]
    fn rebuilds_after_replace_and_rejects_removed_sequences() {
        let (events, seqs) = closed_tool_step();
        let stale_tail = seq(2);
        let cache = balance_cache(None, 0, &seqs, &events).expect("cache");
        assert!(tool_pairing_balanced_after(&cache, stale_tail).expect("after"));

        // A compaction replacement: one checkpoint node shadows the whole
        // prior surface, bumping the rewrite generation.
        let mut replaced_events = events;
        replaced_events.push(user_event(3));
        let replaced_cache =
            balance_cache(Some(cache), 1, &[seq(3)], &replaced_events).expect("rebuild");
        assert!(tool_pairing_balanced_before(&replaced_cache, seq(3)).expect("before"));
        assert!(tool_pairing_balanced_after(&replaced_cache, seq(3)).expect("after"));
        assert_eq!(
            tool_pairing_balanced_before(&replaced_cache, stale_tail).expect_err("stale seq"),
            ToolPairingError("tool-pairing balance: surface seq 2 not found".to_owned())
        );
        assert_eq!(
            tool_pairing_balanced_after(&replaced_cache, stale_tail).expect_err("stale seq"),
            ToolPairingError("tool-pairing balance: surface seq 2 not found".to_owned())
        );
    }

    #[test]
    fn rejects_missing_sequences_including_an_empty_surface() {
        let missing = seq(999);
        let cache = balance_cache(None, 0, &[], &[]).expect("empty cache");
        assert_eq!(
            tool_pairing_balanced_before(&cache, missing).expect_err("missing"),
            ToolPairingError("tool-pairing balance: surface seq 999 not found".to_owned())
        );
        assert_eq!(
            tool_pairing_balanced_after(&cache, missing).expect_err("missing"),
            ToolPairingError("tool-pairing balance: surface seq 999 not found".to_owned())
        );

        let events = vec![user_event(0)];
        let next = balance_cache(None, 0, &[seq(0)], &events).expect("cache");
        assert!(tool_pairing_balanced_after(&next, seq(0)).expect("after"));
    }

    #[test]
    fn extends_incrementally_without_rebuilding_the_head() {
        let mut events = vec![
            user_event(0),
            tool_call_event(1, &["c1"]),
            tool_result_event(2),
        ];
        let mut seqs = seq_list(&[0, 1, 2]);
        let generation = 0;

        let cache = balance_cache(None, generation, &seqs, &events).expect("cache");

        // Log-only growth (a non-surface event) keeps the cache untouched:
        // the same answers come back without a rebuild.
        let cache = balance_cache(Some(cache), generation, &seqs, &events).expect("cached");
        assert!(tool_pairing_balanced_after(&cache, seq(2)).expect("after"));

        // Appended surface nodes extend the tail without re-reading the
        // head: a head that a rebuild would reject keeps answering.
        events.push(user_event(3));
        seqs.push(seq(3));
        let cache = balance_cache(Some(cache), generation, &seqs, &events).expect("extend");
        assert!(tool_pairing_balanced_after(&cache, seq(3)).expect("after"));

        // A generation bump rebuilds from scratch.
        events.push(user_event(9));
        let rebuilt = balance_cache(Some(cache), 1, &seqs, &events).expect("rebuild");
        assert!(tool_pairing_balanced_after(&rebuilt, seq(3)).expect("after"));
    }

    #[test]
    fn retries_an_orphan_result_without_committing_partial_state() {
        let events = vec![user_event(0)];
        let mut seqs = seq_list(&[0]);
        let cache = balance_cache(None, 0, &seqs, &events).expect("cache");

        let mut orphan_events = events;
        orphan_events.push(tool_result_event(1));
        seqs.push(seq(1));
        let expected = ToolPairingError(
            "tool-pairing balance: tool/result at surface seq 1 has no matching \
             tool-call (corrupt surface)"
                .to_owned(),
        );
        let failed =
            balance_cache(Some(cache.clone()), 0, &seqs, &orphan_events).expect_err("orphan");
        assert_eq!(failed, expected);
        // The failed extension commits nothing: the retry folds the same
        // orphan again (a committed partial state would answer instead).
        let retried =
            balance_cache(Some(cache), 0, &seqs, &orphan_events).expect_err("orphan retry");
        assert_eq!(retried, expected);
    }

    #[test]
    fn orphan_result_during_a_rebuild_throws() {
        let events = vec![tool_result_event(0)];
        let expected = ToolPairingError(
            "tool-pairing balance: tool/result at surface seq 0 has no matching \
             tool-call (corrupt surface)"
                .to_owned(),
        );
        assert_eq!(
            balance_cache(None, 0, &[seq(0)], &events).expect_err("orphan"),
            expected
        );
    }
}
