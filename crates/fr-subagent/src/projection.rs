// Rust guideline compliant 2026-11-06
//! The pure session projections (dsh `subagent/src/projection.ts` 1:1):
//! the descriptor-backed identity (last-wins, the `null` sentinel),
//! the active-turn timing fold around the child's own descriptor, and
//! the M1 `agentPreset` fold fr-subagent owns in the agent-presets
//! service's absence.
//!
//! A projection fold must never throw: damage folds to no value (dsh
//! `descriptorIdentity` catches every parse failure).

use serde_json::{Map, Value, json};

use crate::descriptor::{self, FoldedDescriptor};

/// Folds the durable mode/label identity from `subagent/descriptor`
/// events, last-wins (dsh `subagentIdentityProjectionDefinition`).
///
/// A fork seed may replay an ancestor's descriptor; the child's own
/// descriptor overrides it, and a malformed or unknown-version payload
/// resets to the JSON `null` sentinel — deliberately undistinguished,
/// and deliberately serializable so a pushed frame replaces a stale
/// value instead of keeping it.
#[must_use]
pub fn fold_identity(state: Option<Value>, event: &Value) -> Value {
    if event.get("type").and_then(Value::as_str) != Some(crate::wire::DESCRIPTOR_EVENT) {
        return state.unwrap_or(Value::Null);
    }
    let data = event.get("data").cloned().unwrap_or(Value::Null);
    let folded = descriptor::parse(&data).ok().flatten();
    match folded {
        None => Value::Null,
        Some(FoldedDescriptor::OneShot { label, .. }) => {
            let mut identity = Map::new();
            identity.insert("mode".to_owned(), json!("one-shot"));
            if let Some(label) = label {
                identity.insert("label".to_owned(), json!(label));
            }
            identity.insert(
                "seq".to_owned(),
                event.get("seq").cloned().unwrap_or(Value::Null),
            );
            Value::Object(identity)
        }
        Some(FoldedDescriptor::Continuable(descriptor)) => {
            let mut identity = Map::new();
            identity.insert("mode".to_owned(), json!("continuable"));
            identity.insert("label".to_owned(), json!(descriptor.label));
            identity.insert(
                "seq".to_owned(),
                event.get("seq").cloned().unwrap_or(Value::Null),
            );
            Value::Object(identity)
        }
    }
}

/// Folds one turn boundary into the timing state (dsh
/// `subagentTimingProjectionDefinition`): every descriptor resets the
/// accumulated state — a fork seed's replayed ancestor descriptor
/// promotes its pending start, and the child's own descriptor is the
/// authoritative timing origin.
#[must_use]
pub fn fold_timing(state: Option<Value>, event: &Value) -> Value {
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut state = state.unwrap_or_else(|| json!({ "descriptorSeen": false, "settledMs": 0 }));
    let time = event.get("time").and_then(Value::as_u64).unwrap_or(0);
    match event_type {
        "turn/start" => {
            let mut next = state.as_object_mut().expect("object").clone();
            next.remove("lastTurnCompleted");
            if next.get("descriptorSeen").and_then(Value::as_bool) == Some(true) {
                next.insert(
                    "active".to_owned(),
                    json!({ "since": time, "through": time }),
                );
            } else {
                next.insert("pendingTurnStart".to_owned(), json!(time));
            }
            Value::Object(next)
        }
        crate::wire::DESCRIPTOR_EVENT => {
            let since = state
                .get("active")
                .and_then(|active| active.get("since"))
                .and_then(Value::as_u64)
                .or_else(|| state.get("pendingTurnStart").and_then(Value::as_u64));
            let mut next = Map::new();
            next.insert("descriptorSeen".to_owned(), json!(true));
            next.insert("settledMs".to_owned(), json!(0));
            if let Some(since) = since {
                next.insert(
                    "active".to_owned(),
                    json!({ "since": since, "through": time }),
                );
            }
            Value::Object(next)
        }
        "turn/end" => {
            if state.get("descriptorSeen").and_then(Value::as_bool) != Some(true) {
                let mut next = state.as_object_mut().expect("object").clone();
                next.remove("pendingTurnStart");
                return Value::Object(next);
            }
            let Some(active) = state.get("active").and_then(Value::as_object) else {
                return state;
            };
            let since = active.get("since").and_then(Value::as_u64).unwrap_or(0);
            let settled = state.get("settledMs").and_then(Value::as_u64).unwrap_or(0);
            let completed = event
                .get("data")
                .and_then(|data| data.get("reason"))
                .and_then(|reason| reason.get("kind"))
                .and_then(Value::as_str)
                == Some("completed");
            let mut next = state.as_object_mut().expect("object").clone();
            next.remove("active");
            next.insert(
                "settledMs".to_owned(),
                json!(settled + time.saturating_sub(since)),
            );
            next.insert("lastTurnCompleted".to_owned(), json!(completed));
            Value::Object(next)
        }
        _ => {
            if state.get("active").is_none() {
                return state;
            }
            let mut next = state.as_object_mut().expect("object").clone();
            if let Some(active) = next.get("active").and_then(Value::as_object).cloned() {
                let since = active.get("since").cloned().unwrap_or(json!(0));
                next.insert(
                    "active".to_owned(),
                    json!({ "since": since, "through": time }),
                );
            }
            Value::Object(next)
        }
    }
}

/// Folds the independently-selected preset identity (dsh
/// `agent-preset/selected`; the M1 owner is fr-subagent in the
/// agent-presets service's absence). Last-wins; the view is the bare
/// preset id string or the `null` sentinel.
#[must_use]
pub fn fold_agent_preset(state: Option<Value>, event: &Value) -> Value {
    if event.get("type").and_then(Value::as_str) != Some(crate::wire::PRESET_SELECTED_EVENT) {
        return state.unwrap_or(Value::Null);
    }
    event
        .get("data")
        .and_then(|data| data.get("agentPreset"))
        .cloned()
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::{fold_agent_preset, fold_identity, fold_timing};
    use crate::descriptor::{ContinuableDescriptor, snapshot};
    use serde_json::{Value, json};

    fn descriptor_event(seq: u64, label: &str, time: u64) -> Value {
        let descriptor = ContinuableDescriptor {
            provider: "fork".to_owned(),
            label: label.to_owned(),
            agent_provider: None,
            agent_model: None,
            agent_reasoning_effort: None,
            persona: None,
            tool_filter: None,
        };
        json!({
            "type": crate::wire::DESCRIPTOR_EVENT,
            "seq": seq,
            "time": time,
            "data": snapshot(&descriptor),
        })
    }

    #[test]
    fn the_identity_fold_is_last_wins_and_damage_resets_to_null() {
        let first = fold_identity(None, &descriptor_event(3, "ancestor", 0));
        assert_eq!(first["mode"], json!("continuable"));
        assert_eq!(first["label"], json!("ancestor"));
        assert_eq!(first["seq"], json!(3));
        let own = fold_identity(Some(first), &descriptor_event(9, "own", 1));
        assert_eq!(own["label"], json!("own"));
        assert_eq!(own["seq"], json!(9));
        // A malformed descriptor resets the identity to the null sentinel.
        let damaged = json!({
            "type": crate::wire::DESCRIPTOR_EVENT, "seq": 10,
            "data": { "version": 3, "mode": "continuable", "provider": 9 },
        });
        assert_eq!(fold_identity(Some(own), &damaged), Value::Null);
        // An unrelated event keeps the state.
        assert_eq!(
            fold_identity(Some(json!("keep")), &json!({ "type": "turn/start" })),
            json!("keep")
        );
    }

    #[test]
    fn the_timing_fold_resets_at_the_own_descriptor() {
        let mut state = json!({ "descriptorSeen": false, "settledMs": 0 });
        state = fold_timing(
            Some(state),
            &json!({
                "type": "turn/start", "time": 100, "data": {},
            }),
        );
        assert_eq!(state["pendingTurnStart"], json!(100));
        // The fork seed's replayed ancestor descriptor promotes the pending start.
        state = fold_timing(Some(state), &descriptor_event(4, "ancestor", 150));
        assert_eq!(state["settledMs"], json!(0));
        assert_eq!(state["active"], json!({ "since": 100, "through": 150 }));
        // A completed turn after the descriptor accumulates the interval.
        state = fold_timing(
            Some(state),
            &json!({
                "type": "turn/end", "time": 400,
                "data": { "reason": { "kind": "completed" } },
            }),
        );
        assert_eq!(state["settledMs"], json!(300));
        assert_eq!(state["lastTurnCompleted"], json!(true));
        // The child's own descriptor resets the origin: the pending
        // start was consumed by the ancestor descriptor's promotion, so
        // no active interval reopens (dsh `activeSince` is undefined).
        state = fold_timing(Some(state), &descriptor_event(9, "own", 500));
        assert_eq!(state["settledMs"], json!(0));
        assert!(state.get("active").is_none());
    }

    #[test]
    fn the_preset_fold_reads_the_selected_event_last_wins() {
        let event = |preset: &str| {
            json!({
                "type": crate::wire::PRESET_SELECTED_EVENT,
                "data": { "agentPreset": preset },
            })
        };
        assert_eq!(fold_agent_preset(None, &event("coding")), json!("coding"));
        assert_eq!(
            fold_agent_preset(Some(json!("coding")), &event("reviewing")),
            json!("reviewing")
        );
        assert_eq!(
            fold_agent_preset(Some(json!("coding")), &json!({ "type": "turn/start" })),
            json!("coding")
        );
    }
}
