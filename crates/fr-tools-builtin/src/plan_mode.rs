// Rust guideline compliant 2026-10-31
//! The plan-mode domain (dsh `@deepseek-ai/dsh-plan-mode`): the logged
//! `plan/mode` fold, the wire view, the exit tool's support pieces, and
//! the deployment guidance constant.
//!
//! M1 rulings (the Q16d batch): the tool registers unconditionally and
//! its `loggedActive` gate reads the `plan` projection, so with no
//! `plan/mode` writer composed (the `/plan` command lands in Phase 18)
//! the first refusal arm stays the only reachable one — the arm-in-place
//! plus synthetic-pin discipline, the schedule subagent-refusal family.
//! The pre-step selection machinery (`pendingIntents`, `onBoundary`,
//! the narration messages, the `/plan` command rows) needs the agents
//! registry and the commands face, stays absent, and rides Phase 18;
//! the `ASK_CANCELLED` dismissal arm is a pure branch on the error code
//! a client answerer produces.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The projection key this fold registers under.
pub const PROJECTION_KEY: &str = "plan";

/// The projection state schema version (dsh stateVersion 3).
pub const STATE_VERSION: u64 = 3;

/// The durable plan-mode event (the dsh `SessionEventMap` merge
/// extension; log-only, non-surface, whole-value replace, the last one
/// wins).
pub const CHANGE_EVENT: &str = "plan/mode";

/// The model-facing exit tool's name (dsh `EXIT_PLAN_MODE`).
pub const EXIT_TOOL: &str = "exit_plan_mode";

/// The `plan:policy` prompt-section name.
pub const POLICY_SECTION: &str = "plan:policy";

/// The review question's id, echoed in the answer this tool reads.
pub const REVIEW_ID: &str = "plan-review";

/// The review question's approve option label.
pub const APPROVE_LABEL: &str = "Approve";

/// The review question's keep-planning option label.
pub const KEEP_PLANNING_LABEL: &str = "Keep planning";

/// The dismissal code a client answerer produces (the dsh
/// `ASK_CANCELLED` catch; the tool ends the turn with an unapproved
/// plan instead of failing).
pub const ASK_CANCELLED: &str = "ASK_CANCELLED";

/// The host plan-mode fold state (dsh `PlanUnitState`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanUnitState {
    /// Logged plan mode.
    #[serde(default)]
    pub active: bool,
    /// The selection's target mode; null when nothing is outstanding.
    #[serde(default)]
    pub wanted: Option<bool>,
    /// The latest plan command awaiting its paired settlement.
    #[serde(default)]
    pub running: Option<RunningCommand>,
    /// Active state recorded by the latest request header, or null.
    #[serde(default)]
    pub active_at_last_header: Option<bool>,
}

/// The latest plan command awaiting its paired settlement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningCommand {
    /// The command id awaiting its `command/done`.
    #[serde(rename = "commandId")]
    pub command_id: String,
    /// The selection's target mode.
    pub wanted: bool,
}

/// The wire view (dsh `PlanProjection`).
///
/// `pending` is true while a logged `/plan` selection targets a state
/// other than `active`, has not failed through its paired settlement,
/// and no later `plan/mode` has recorded that state.
#[must_use]
pub fn wire_view(state: &PlanUnitState) -> Value {
    let wanted = state
        .running
        .as_ref()
        .map(|command| command.wanted)
        .or(state.wanted);
    json!({
        "active": state.active,
        "pending": wanted.is_some() && wanted != Some(state.active),
    })
}

/// Applies one session event to the fold (dsh
/// `planProjectionDefinition.apply`).
#[must_use]
pub fn apply_event(state: &PlanUnitState, event: &Value) -> PlanUnitState {
    let event_type = event.get("type").and_then(Value::as_str);
    let data = event.get("data");
    match event_type {
        Some("command/run") => {
            if data
                .and_then(|data| data.get("name"))
                .and_then(Value::as_str)
                != Some("plan")
            {
                return state.clone();
            }
            match data.and_then(|data| data.get("args")) {
                None | Some(Value::Null) => state.clone(),
                Some(args) => {
                    let wanted = args
                        .as_str()
                        .map(str::trim)
                        .is_none_or(|trimmed| trimmed != "off");
                    PlanUnitState {
                        running: Some(RunningCommand {
                            command_id: data
                                .and_then(|data| data.get("commandId"))
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            wanted,
                        }),
                        ..state.clone()
                    }
                }
            }
        }
        Some("command/done") => {
            let Some(running) = state.running.as_ref() else {
                return state.clone();
            };
            let matches = data
                .and_then(|data| data.get("commandId"))
                .and_then(Value::as_str)
                == Some(running.command_id.as_str());
            if !matches {
                return state.clone();
            }
            let success = data
                .and_then(|data| data.get("kind"))
                .and_then(Value::as_str)
                == Some("success");
            let wanted = (success && running.wanted != state.active).then_some(running.wanted);
            PlanUnitState {
                wanted,
                running: None,
                ..state.clone()
            }
        }
        Some("plan/mode") => PlanUnitState {
            active: data
                .and_then(|data| data.get("active"))
                .and_then(Value::as_bool)
                .unwrap_or(state.active),
            wanted: None,
            ..state.clone()
        },
        Some("request/header") => PlanUnitState {
            active_at_last_header: Some(state.active),
            ..state.clone()
        },
        _ => state.clone(),
    }
}

/// Folds a whole session log into the plan state.
#[must_use]
pub fn fold(events: &Value) -> Value {
    let mut state = PlanUnitState::default();
    if let Value::Array(items) = events {
        for event in items {
            state = apply_event(&state, event);
        }
    }
    serde_json::to_value(state).unwrap_or_else(|_| json!({}))
}

/// Reads the logged plan state (the dsh `loggedActive` gate input).
#[must_use]
pub fn logged_active(state: Option<&Value>) -> bool {
    state
        .and_then(|state| state.get("active"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// The plan's first markdown heading (any level), or `None` when it has
/// none (dsh `firstHeading`).
#[must_use]
pub fn first_heading(plan: &str) -> Option<String> {
    for line in plan.split('\n') {
        let trimmed = line.trim();
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if hashes == 0 || hashes > 6 {
            continue;
        }
        let rest = &trimmed[hashes..];
        let Some(rest) = rest.strip_prefix(char::is_whitespace) else {
            continue;
        };
        let rest = rest.trim();
        if !rest.is_empty() {
            return Some(rest.to_owned());
        }
    }
    None
}

/// Validates the deployment-owned guidance (dsh `resolveConfig`).
///
/// # Errors
///
/// The dsh refusal texts, verbatim.
pub fn resolve_config(section: Option<&str>) -> Result<String, String> {
    let Some(section) = section else {
        return Err("PlanModeConfig needs a string `section`".to_owned());
    };
    if section.trim().is_empty() {
        return Err("PlanModeConfig needs a non-empty `section`".to_owned());
    }
    Ok(section.to_owned())
}

/// Whether the plan starts with a non-empty `#` heading (the dsh
/// `/^#\s+\S/` gate over the trimmed plan).
#[must_use]
pub fn has_heading(plan: &str) -> bool {
    let trimmed = plan.trim();
    let Some(after) = trimmed.strip_prefix('#') else {
        return false;
    };
    let rest = after.trim_start_matches(char::is_whitespace);
    rest.len() < after.len() && !rest.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: &str, data: Value) -> Value {
        let mut object = serde_json::Map::new();
        object.insert("type".to_owned(), Value::from(kind));
        object.insert("data".to_owned(), data);
        Value::Object(object)
    }

    #[test]
    fn fold_pins_the_selection_lifecycle() {
        // A command/run arms the running selection.
        let armed = fold(&json!([event(
            "command/run",
            json!({
                "name": "plan", "commandId": "cmd-1", "args": "plan carefully"
            })
        )]));
        assert_eq!(armed["running"]["wanted"], json!(true));
        // `off` targets inactive.
        let off = fold(&json!([event(
            "command/run",
            json!({
                "name": "plan", "commandId": "cmd-2", "args": "off"
            })
        )]));
        assert_eq!(off["running"]["wanted"], json!(false));
        // A failed settlement clears the selection without committing.
        let cleared = fold(&json!([
            event(
                "command/run",
                json!({ "name": "plan", "commandId": "c", "args": "x" })
            ),
            event("command/done", json!({ "commandId": "c", "kind": "error" })),
        ]));
        assert_eq!(cleared["running"], Value::Null);
        assert_eq!(cleared["wanted"], Value::Null);
        // A successful settlement targeting another state keeps it pending.
        let pending = fold(&json!([
            event(
                "command/run",
                json!({ "name": "plan", "commandId": "c", "args": "x" })
            ),
            event(
                "command/done",
                json!({ "commandId": "c", "kind": "success" })
            ),
        ]));
        assert_eq!(pending["wanted"], json!(true));
        assert_eq!(pending["active"], json!(false));
        let state: PlanUnitState = serde_json::from_value(pending).expect("state");
        assert_eq!(wire_view(&state)["pending"], json!(true));
        // The mode event commits and clears the selection; the header
        // records what the last assembly told the model.
        let committed = fold(&json!([
            event(
                "command/run",
                json!({ "name": "plan", "commandId": "c", "args": "x" })
            ),
            event(
                "command/done",
                json!({ "commandId": "c", "kind": "success" })
            ),
            event(CHANGE_EVENT, json!({ "active": true })),
            event("request/header", json!({})),
        ]));
        assert_eq!(committed["active"], json!(true));
        assert_eq!(committed["wanted"], Value::Null);
        assert_eq!(committed["activeAtLastHeader"], json!(true));
        assert_eq!(
            wire_view(&serde_json::from_value::<PlanUnitState>(committed).expect("state"))["pending"],
            json!(false)
        );
    }

    #[test]
    fn heading_gates_pin_the_dsh_regex() {
        assert!(has_heading("# Ship it"));
        // The dsh regex demands whitespace after the first hash, so a
        // deeper level never passes the gate.
        assert!(!has_heading("## Sub plan"));
        assert!(!has_heading("no heading"));
        assert!(!has_heading("#"));
        assert!(!has_heading("# "));
        assert!(!has_heading("#\t"));
        assert!(!has_heading("text\n# later heading"));
        assert_eq!(
            first_heading("intro\n### The middle path\nrest"),
            Some("The middle path".to_owned())
        );
        assert_eq!(first_heading("no heading anywhere"), None);
        // Seven hashes are not a heading level.
        assert_eq!(first_heading("####### too deep"), None);
    }

    #[test]
    fn resolve_config_pins_the_three_refusals() {
        assert_eq!(
            resolve_config(None).unwrap_err(),
            "PlanModeConfig needs a string `section`"
        );
        assert_eq!(
            resolve_config(Some("   ")).unwrap_err(),
            "PlanModeConfig needs a non-empty `section`"
        );
        assert_eq!(
            resolve_config(Some("You are in plan mode.")).expect("the guidance validates"),
            "You are in plan mode."
        );
    }
}
