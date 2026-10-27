//! Durable agent inbox: the fold over `agent/inbox/spliced` events and the
//! JS-splice-normalizing planner the loop driver commits through.
//!
//! Mirrors dsh `agent-loop/src/inbox.ts`: the persisted projection validates
//! every splice (index bounds, cross-queue message-id uniqueness) before
//! applying it; a rejected history reads as `invalid persisted inbox splice
//! at session seq <n>`. The mutation side normalizes JavaScript
//! `Array.prototype.splice` arguments (truncation, negative start counting
//! back from the end, clamped delete count) before an event is appended,
//! and a no-op splice never reaches the log.
//!
//! The event type itself is loop-owned: `agent/inbox/spliced` rides the v4
//! log through the unknown-event fallback and is declared in the plugin
//! vocabulary at activation (PRD decision 28 / Q34A).

use std::collections::HashSet;
use std::error::Error;
use std::fmt;

use fr_wire::ids::MessageId;
use fr_wire::message::Message;
use serde::{Deserialize, Serialize};

/// One of the two ordered pending-message lists owned by an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InboxTarget {
    /// Prompts awaiting individual turns.
    NextTurn,
    /// Input awaiting the next step boundary.
    NextStep,
}

/// Whether one splice dropped the messages it removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InboxOutcome {
    /// The removed messages were canceled out of the inbox.
    #[serde(rename = "canceled")]
    Canceled,
}

/// One normalized mutation of an agent's durable pending-message lists.
///
/// `start` and `removed_count` are already-normalized non-negative values;
/// `removed_count` and `outcome` are absent exactly when zero and
/// non-canceled, matching the dsh wire shape (inbox.ts:198-207).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxSplice {
    /// Pending list this splice mutates.
    pub target: InboxTarget,
    /// Position the splice applies at, in `0..=len`.
    pub start: u64,
    /// Removed count; absent means zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_count: Option<u64>,
    /// Messages inserted at `start`, in order.
    pub inserted: Vec<Message>,
    /// `canceled` when the removed messages were dropped; absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<InboxOutcome>,
}

/// Complete pending-inbox value reconstructed from durable splices.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InboxState {
    /// Prompts awaiting individual turns.
    pub next_turn: Vec<Message>,
    /// Input awaiting the next step boundary.
    pub next_step: Vec<Message>,
}

impl InboxState {
    /// The pending list named by `target`.
    #[must_use]
    pub const fn list(&self, target: InboxTarget) -> &Vec<Message> {
        match target {
            InboxTarget::NextTurn => &self.next_turn,
            InboxTarget::NextStep => &self.next_step,
        }
    }
}

/// Why one splice is not a valid inbox mutation (dsh inbox.ts:67-72, 79-86).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboxSpliceError {
    /// Bounds violation, or an inserted message whose shape carries no id.
    InvalidSplice,
    /// The id is already pending in either queue.
    DuplicateId(MessageId),
}

impl fmt::Display for InboxSpliceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSplice => f.write_str("invalid inbox splice"),
            Self::DuplicateId(id) => write!(f, "message \"{id}\" is already pending"),
        }
    }
}

impl Error for InboxSpliceError {}

/// A rejected splice together with the log position it was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxFoldError {
    /// Sequence number of the rejected `agent/inbox/spliced` event.
    pub seq: u64,
    /// The validation failure, chained as this error's source.
    pub cause: InboxSpliceError,
}

impl fmt::Display for InboxFoldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid persisted inbox splice at session seq {}",
            self.seq
        )
    }
}

impl Error for InboxFoldError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.cause)
    }
}

/// The stable identity of one inbox message, if its shape carries one.
///
/// Known-role messages always do; an unknown-shape entry cannot participate
/// in cross-queue uniqueness, so the fold rejects it instead of trusting it.
pub(crate) fn message_id(message: &Message) -> Option<&MessageId> {
    match message {
        Message::Known(known) => Some(known.id()),
        Message::Other(_) => None,
    }
}

/// Rejects ids that would be pending twice once `candidate` replaces its
/// target list: every rebuilt entry is checked against the other queue too
/// (dsh checks `[...next, ...other]`, inbox.ts:73-78).
fn reject_duplicate_ids(candidate: &[Message], other: &[Message]) -> Result<(), InboxSpliceError> {
    let mut seen: HashSet<&MessageId> = HashSet::new();
    for message in candidate.iter().chain(other.iter()) {
        let id = message_id(message).ok_or(InboxSpliceError::InvalidSplice)?;
        if !seen.insert(id) {
            return Err(InboxSpliceError::DuplicateId(id.clone()));
        }
    }
    Ok(())
}

/// Applies one committed splice to `state`, validating it exactly as the dsh
/// projection does (inbox.ts:57-90): bounds first, then cross-queue id
/// uniqueness over the rebuilt list.
///
/// # Errors
///
/// Returns an [`InboxFoldError`] wrapping the validation failure; its text
/// is the dsh restore error, with the underlying cause chained.
///
/// # Panics
///
/// Never panics in practice: the `expect` calls guard conversions that were
/// already bounded to the list length earlier in the same call.
pub fn fold_inbox(
    state: &InboxState,
    seq: u64,
    splice: &InboxSplice,
) -> Result<InboxState, InboxFoldError> {
    fold_inner(state, splice).map_err(|cause| InboxFoldError { seq, cause })
}

fn fold_inner(state: &InboxState, splice: &InboxSplice) -> Result<InboxState, InboxSpliceError> {
    let inbox = state.list(splice.target);
    let len = u64::try_from(inbox.len())
        .expect("an in-memory list length always fits the u64 safe-integer domain");
    let removed = splice.removed_count.unwrap_or(0);
    if splice.start > len || removed > len - splice.start {
        return Err(InboxSpliceError::InvalidSplice);
    }
    let start =
        usize::try_from(splice.start).expect("bounded to the list length before this point");
    let end =
        start + usize::try_from(removed).expect("bounded to the list length before this point");
    let mut candidate = inbox.clone();
    candidate.splice(start..end, splice.inserted.iter().cloned());
    match splice.target {
        InboxTarget::NextTurn => reject_duplicate_ids(&candidate, &state.next_step)?,
        InboxTarget::NextStep => reject_duplicate_ids(&candidate, &state.next_turn)?,
    }
    Ok(match splice.target {
        InboxTarget::NextTurn => InboxState {
            next_turn: candidate,
            next_step: state.next_step.clone(),
        },
        InboxTarget::NextStep => InboxState {
            next_turn: state.next_turn.clone(),
            next_step: candidate,
        },
    })
}

/// Normalizes JavaScript `Array.prototype.splice` indices.
///
/// dsh inbox.ts:150-166: `start` counts back from the end when negative and
/// clamps into `0..=len`; `delete_count` clamps into `0..=len - start`. The
/// `NaN -> 0` arm of the dsh original is structurally absent because the
/// arguments here are integers.
///
/// # Panics
///
/// Never panics in practice: the `expect` calls guard list-length
/// conversions that fit the signed domain on every supported target.
#[must_use]
pub fn normalize_splice(len: usize, start: i64, delete_count: i64) -> (usize, usize) {
    let len_signed = i64::try_from(len).expect("a list length fits i64 on every supported target");
    let resolved_start = if start < 0 {
        (len_signed + start).max(0)
    } else {
        start.min(len_signed)
    };
    let actual_start = usize::try_from(resolved_start).expect("clamped into 0..=len above");
    let max_delete = i64::try_from(len - actual_start)
        .expect("a list length fits i64 on every supported target");
    let actual_delete =
        usize::try_from(delete_count.clamp(0, max_delete)).expect("clamped non-negative above");
    (actual_start, actual_delete)
}

/// Plans one durable mutation: normalizes the splice arguments against the
/// target list, applies the cross-queue uniqueness check, and returns the
/// event payload plus the messages it removes.
///
/// Returns `Ok(None)` for a no-op splice (nothing removed, nothing
/// inserted): dsh never records it (inbox.ts:167-168). The `canceled`
/// outcome is stamped exactly when the caller discards the removed
/// messages (inbox.ts:186).
///
/// # Errors
///
/// Returns [`InboxSpliceError`] when the normalized result would leave a
/// duplicate id pending in either queue, or an inserted message has no
/// readable identity.
///
/// # Panics
///
/// Never panics in practice: the `expect` calls guard conversions that were
/// already clamped into the list-length domain earlier in the same call.
pub fn plan_splice(
    state: &InboxState,
    target: InboxTarget,
    start: i64,
    delete_count: i64,
    inserted: Vec<Message>,
    discard_removed: bool,
) -> Result<Option<(InboxSplice, Vec<Message>)>, InboxSpliceError> {
    let inbox = state.list(target);
    let (actual_start, actual_delete) = normalize_splice(inbox.len(), start, delete_count);
    if actual_delete == 0 && inserted.is_empty() {
        return Ok(None);
    }
    let mut candidate = Vec::with_capacity(inbox.len() - actual_delete + inserted.len());
    candidate.extend_from_slice(&inbox[..actual_start]);
    candidate.extend(inserted.iter().cloned());
    candidate.extend_from_slice(&inbox[actual_start + actual_delete..]);
    let removed = inbox[actual_start..actual_start + actual_delete].to_vec();
    match target {
        InboxTarget::NextTurn => reject_duplicate_ids(&candidate, &state.next_step)?,
        InboxTarget::NextStep => reject_duplicate_ids(&candidate, &state.next_turn)?,
    }
    let payload = InboxSplice {
        target,
        start: u64::try_from(actual_start).expect("clamped into 0..=len above"),
        removed_count: (actual_delete > 0)
            .then(|| u64::try_from(actual_delete).expect("clamped non-negative above")),
        inserted,
        outcome: (discard_removed && actual_delete > 0).then_some(InboxOutcome::Canceled),
    };
    Ok(Some((payload, removed)))
}

#[cfg(test)]
mod tests {
    use super::{
        InboxFoldError, InboxOutcome, InboxSplice, InboxSpliceError, InboxState, InboxTarget,
        fold_inbox, normalize_splice, plan_splice,
    };
    use fr_wire::message::Message;
    use serde_json::json;

    fn user_message(id: &str) -> Message {
        serde_json::from_value(json!({
            "role": "user",
            "id": id,
            "content": [{ "type": "text", "text": "task text" }],
            "source": { "kind": "user" }
        }))
        .expect("fixture matches the fr-wire user-message wire shape")
    }

    /// Covers the payload wire shape (camelCase keys, `removedCount` and
    /// `outcome` absent at zero/non-canceled) and the append fold.
    #[test]
    fn wire_shape_and_append_fold() {
        let splice = InboxSplice {
            target: InboxTarget::NextStep,
            start: 0,
            removed_count: None,
            inserted: vec![user_message("m-1")],
            outcome: None,
        };
        let encoded = serde_json::to_string(&splice).expect("payload serializes");
        assert!(encoded.contains(r#""target":"next-step""#));
        assert!(encoded.contains(r#""start":0"#));
        assert!(!encoded.contains("removedCount"));
        assert!(!encoded.contains("outcome"));
        let back: InboxSplice = serde_json::from_str(&encoded).expect("payload round-trips");
        assert_eq!(back, splice);

        let state = InboxState::default();
        let folded = fold_inbox(&state, 1, &splice).expect("append applies");
        assert_eq!(folded.next_step, splice.inserted);
        assert_eq!(folded.next_turn, [] as [fr_wire::message::Message; 0]);
    }

    /// Covers both bounds rejections: start past the end, and a removal
    /// range longer than the list tail.
    #[test]
    fn fold_rejects_out_of_bounds() {
        let state = InboxState {
            next_turn: vec![user_message("m-1")],
            next_step: Vec::new(),
        };
        let past_end = InboxSplice {
            target: InboxTarget::NextTurn,
            start: 2,
            removed_count: None,
            inserted: Vec::new(),
            outcome: None,
        };
        let too_long = InboxSplice {
            target: InboxTarget::NextTurn,
            start: 0,
            removed_count: Some(2),
            inserted: Vec::new(),
            outcome: None,
        };
        for splice in [past_end, too_long] {
            let error = fold_inbox(&state, 5, &splice).expect_err("bounds violation is rejected");
            assert_eq!(
                error,
                InboxFoldError {
                    seq: 5,
                    cause: InboxSpliceError::InvalidSplice,
                }
            );
            assert_eq!(
                error.to_string(),
                "invalid persisted inbox splice at session seq 5"
            );
            assert_eq!(error.cause.to_string(), "invalid inbox splice");
        }
    }

    /// Covers the cross-queue duplicate-id rejection and its exact text.
    #[test]
    fn fold_rejects_duplicate_across_queues() {
        let state = InboxState {
            next_turn: vec![user_message("m-1")],
            next_step: Vec::new(),
        };
        let splice = InboxSplice {
            target: InboxTarget::NextStep,
            start: 0,
            removed_count: None,
            inserted: vec![user_message("m-1")],
            outcome: None,
        };
        let error = fold_inbox(&state, 7, &splice).expect_err("duplicate id is rejected");
        assert_eq!(
            error.cause.to_string(),
            r#"message "m-1" is already pending"#
        );
    }

    /// Covers a removal-plus-insert fold and the `canceled` payload shape.
    #[test]
    fn fold_applies_removal() {
        let state = InboxState {
            next_turn: Vec::new(),
            next_step: vec![user_message("m-1"), user_message("m-2")],
        };
        let splice = InboxSplice {
            target: InboxTarget::NextStep,
            start: 0,
            removed_count: Some(1),
            inserted: vec![user_message("m-3")],
            outcome: Some(InboxOutcome::Canceled),
        };
        let encoded = serde_json::to_string(&splice).expect("payload serializes");
        assert!(encoded.contains(r#""removedCount":1"#));
        assert!(encoded.contains(r#""outcome":"canceled""#));
        let folded = fold_inbox(&state, 2, &splice).expect("removal applies");
        let ids: Vec<&str> = folded
            .next_step
            .iter()
            .map(|message| {
                let Message::Known(known) = message else {
                    panic!("fixture messages keep their known shape");
                };
                known.id().as_str()
            })
            .collect();
        assert_eq!(ids, ["m-3", "m-2"]);
    }

    /// Covers the JS splice normalization arms: negative start counting
    /// back, start past the end clamping to `len`, and delete clamping.
    #[test]
    fn plan_normalizes_js_semantics() {
        assert_eq!(normalize_splice(3, -1, 1), (2, 1));
        assert_eq!(normalize_splice(3, -99, 1), (0, 1));
        assert_eq!(normalize_splice(3, 99, 1), (3, 0));
        assert_eq!(normalize_splice(3, 1, 99), (1, 2));
        assert_eq!(normalize_splice(3, 1, -4), (1, 0));
    }

    /// Covers the no-op rule, the `canceled` stamping rule, and the removed
    /// slice the driver hands back to its discard notifications.
    #[test]
    fn plan_stamps_canceled_only_when_discarding() {
        let state = InboxState {
            next_turn: Vec::new(),
            next_step: vec![user_message("m-1"), user_message("m-2")],
        };
        let noop = plan_splice(&state, InboxTarget::NextStep, 0, 0, Vec::new(), true)
            .expect("no-op plans");
        assert!(noop.is_none());

        let (payload, removed) = plan_splice(
            &state,
            InboxTarget::NextStep,
            -1,
            1,
            vec![user_message("m-3")],
            true,
        )
        .expect("replacement plans")
        .expect("removal plus insertion is not a no-op");
        assert_eq!(payload.start, 1);
        assert_eq!(payload.removed_count, Some(1));
        assert_eq!(payload.outcome, Some(InboxOutcome::Canceled));
        assert_eq!(removed.len(), 1);

        let (payload, _removed) = plan_splice(
            &state,
            InboxTarget::NextStep,
            0,
            1,
            vec![user_message("m-3")],
            false,
        )
        .expect("claim-style splice plans")
        .expect("removal plus insertion is not a no-op");
        assert_eq!(payload.outcome, None);
    }
}
