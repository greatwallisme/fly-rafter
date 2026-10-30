// Rust guideline compliant 2026-10-30
//! The model-facing `get_goal`, `create_goal`, and `update_goal` tools
//! over the goal service (dsh `@deepseek-ai/dsh-tool-goal`).
//!
//! M1 structural facts (the batch-0 ruling: the arms stay in place as
//! pure functions, the no-counterpart facts stay documented): the dsh
//! `ctx.agents` registry checks (exact live identity, `status === 'running'`,
//! `currentInitiator`) have no M1 counterpart — the composition binds
//! the caller to the live driver's session, so the authority fences on
//! the session id plus the open-turn boundary; `roots().includes(agent)`
//! bakes to the single-root headless fact (the pure predicate still
//! takes the flag so synthetic inputs pin the false arm). The
//! goal-round wrap-up `deferContext` arm is structurally unreachable in
//! M1 (no goal-round driver, so the goal-round authority never fires)
//! and stays rendered as the pure function the ruling pins.

use serde_json::{Value, json};

use crate::goal_service::{
    CreateGoalRequest, EditGoalRequest, GoalError, GoalSeams, GoalView, Goals,
};
use crate::tools::BodyError;

/// The driver-owned projection key the authority reads (dsh
/// `turnBoundary`).
///
/// The fold lives in the fr-agent-loop crate — a lateral dependency no
/// plugin takes, so the wire vocabulary rides as the literal, the
/// dsh-shared protocol face.
pub const TURN_BOUNDARY_KEY: &str = "turnBoundary";

/// The minimum admitted goal rounds before the model may self-report
/// `blocked` (dsh `Config.blockedAfterConsecutiveRounds` default).
///
/// The headless bundle adds no tool-goal config row, so the baked
/// default is the only arm.
pub const BAKED_BLOCKED_AFTER: i64 = 3;

/// The update actions in table order (dsh `UPDATE_ACTIONS`; the schema
/// enum freezes the same order).
pub const UPDATE_ACTIONS: [&str; 5] = ["edit", "pause", "resume", "complete", "blocked"];

/// Validates the policy value even when apply is called outside Loader
/// normalization (dsh `resolveConfig`).
///
/// # Errors
///
/// The dsh `TypeError` text, verbatim.
pub fn resolve_config(blocked_after: i64) -> Result<i64, String> {
    if (1..=crate::goal::MAX_SAFE_INTEGER).contains(&blocked_after) {
        Ok(blocked_after)
    } else {
        Err("blockedAfterConsecutiveRounds must be a positive safe integer".to_owned())
    }
}

/// Whether optional text is meaningful rather than a strict-schema
/// empty filler (dsh `hasText`).
fn has_text(value: Option<&str>) -> bool {
    value.is_some_and(|text| !text.is_empty())
}

/// Whether an optional round cap is meaningful rather than a
/// strict-schema zero filler (dsh `hasRoundCap`).
fn has_round_cap(value: Option<&Value>) -> bool {
    value.is_some_and(|value| value.as_f64().is_some_and(|number| number != 0.0))
}

/// The authenticated calling session plus the immutable event cut and
/// open-turn start seq (dsh `GoalToolExecution`).
#[derive(Debug)]
pub struct GoalToolExecution<'a> {
    /// The owning session id (the dsh `exec.agent`).
    pub session: &'a str,
    /// The session events in log order (the dsh `snapshotEvents()`).
    pub events: Vec<Value>,
    /// The open turn's start seq.
    pub open_turn_start_seq: i64,
}

/// The hard authority granted to one state-changing call (dsh
/// `GoalToolAuthority`).
#[derive(Debug)]
pub enum GoalToolAuthority {
    /// Host-attested human input in the current root turn.
    DirectHuman,
    /// The exact admitted goal round.
    GoalRound(GoalView),
}

/// Maps one coded tool-policy failure onto the body error (dsh throws
/// `HarnessError` directly, so the class name rides as `HarnessError`).
fn harness_error(code: &str, message: impl Into<String>) -> BodyError {
    BodyError {
        code: Some(code.to_owned()),
        name: "HarnessError".to_owned(),
        message: message.into(),
    }
}

/// Maps the goal-service boundary error onto the body failure (the
/// coded family stays routable; the class name follows the dsh
/// `new.target.name`).
fn goal_error(error: GoalError) -> BodyError {
    let name = if error.code.is_empty() {
        "Error".to_owned()
    } else {
        "GoalError".to_owned()
    };
    BodyError {
        code: if error.code.is_empty() {
            None
        } else {
            Some(error.code)
        },
        name,
        message: error.message,
    }
}

/// Resolves the immutable event cut and open-turn boundary (dsh
/// `openTurnEvents`; the turn-boundary projection rides the relay).
fn open_turn_events(seams: &dyn GoalSeams, session: &str) -> Result<(Vec<Value>, i64), BodyError> {
    let boundary = seams
        .projection_state(session, TURN_BOUNDARY_KEY)
        .map_err(|message| harness_error("GOAL_TOOL_DRIVER_REQUIRED", message))?
        .filter(|state| !state.is_null());
    let start = boundary
        .as_ref()
        .and_then(|state| state.get("openTurnStartSeq"))
        .and_then(Value::as_i64);
    let Some(open_turn_start_seq) = start else {
        return Err(harness_error(
            "GOAL_TOOL_DRIVER_REQUIRED",
            "goal tools require an open model turn",
        ));
    };
    let events = read_events(seams, session)?;
    Ok((events, open_turn_start_seq))
}

/// Reads the session log events (the dsh `snapshotEvents()` face).
fn read_events(seams: &dyn GoalSeams, session: &str) -> Result<Vec<Value>, BodyError> {
    // A live session with nothing stored yet reads as the empty cut
    // (the dsh `snapshotEvents()` on a live session never fails; the
    // store may not have flushed).
    let events = seams
        .read_log(session)
        .map_err(|message| harness_error("GOAL_TOOL_DRIVER_REQUIRED", message))?
        .map_or_else(Vec::new, |log| log.events);
    Ok(events)
}

/// Resolves and authenticates the calling session and its driver
/// boundary (dsh `goalToolExecution`; the registry-identity arms stay
/// documented in the module header).
///
/// # Errors
///
/// The dsh refusal texts, verbatim.
pub fn goal_tool_execution<'a>(
    agent: Option<&'a str>,
    seams: &dyn GoalSeams,
) -> Result<GoalToolExecution<'a>, BodyError> {
    let Some(session) = agent else {
        return Err(harness_error(
            "GOAL_TOOL_AGENT_REQUIRED",
            "goal tools require a calling agent",
        ));
    };
    let (events, open_turn_start_seq) = open_turn_events(seams, session)?;
    Ok(GoalToolExecution {
        session,
        events,
        open_turn_start_seq,
    })
}

/// Whether the captured open turn contains one event accepted by the
/// shape (dsh `someOpenTurnEvent`; the scan compares seqs — the dense
/// log equivalence).
fn some_open_turn_event(
    execution: &GoalToolExecution<'_>,
    predicate: impl Fn(&Value) -> bool,
) -> bool {
    execution.events.iter().any(|event| {
        let seq = event.get("seq").and_then(Value::as_i64).unwrap_or(-1);
        seq > execution.open_turn_start_seq && predicate(event)
    })
}

/// Whether the turn carries a host-attested human message (dsh
/// `hasDirectHumanInput`; the omitted followup/steer source resolves
/// to `user`, so non-human producers must supply their own source).
fn has_direct_human_input(execution: &GoalToolExecution<'_>, is_root: bool) -> bool {
    if !is_root {
        return false;
    }
    some_open_turn_event(execution, |event| {
        event.get("type").and_then(Value::as_str) == Some("user/message")
            && event
                .pointer("/data/source/kind")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind == "user")
    })
}

/// Whether this turn is the current goal's exact admitted round (dsh
/// `isMatchingGoalRound`).
fn is_matching_goal_round(execution: &GoalToolExecution<'_>, goal: &GoalView) -> bool {
    some_open_turn_event(execution, |event| {
        let source = event.pointer("/data/source");
        event.get("type").and_then(Value::as_str) == Some("user/message")
            && source
                .and_then(|source| source.get("kind"))
                .and_then(Value::as_str)
                == Some("goal")
            && source
                .and_then(|source| source.get("goalId"))
                .and_then(Value::as_str)
                == Some(goal.snapshot.id.as_str())
            && source
                .and_then(|source| source.get("revision"))
                .and_then(Value::as_i64)
                == Some(goal.snapshot.revision)
            && source
                .and_then(|source| source.get("round"))
                .and_then(Value::as_i64)
                == Some(goal.rounds_started)
    })
}

/// Requires authority originating in a human message accepted by a
/// runtime root (dsh `requireDirectHuman`).
///
/// # Errors
///
/// The dsh refusal text, verbatim.
pub fn require_direct_human(
    execution: &GoalToolExecution<'_>,
    is_root: bool,
) -> Result<(), BodyError> {
    if has_direct_human_input(execution, is_root) {
        return Ok(());
    }
    Err(harness_error(
        "GOAL_TOOL_AUTHORITY_REQUIRED",
        "this goal operation requires a direct human turn on a top-level agent",
    ))
}

/// Resolves completion authority from either direct human input or the
/// exact goal round (dsh `completionAuthority`).
///
/// # Errors
///
/// The dsh refusal text, verbatim.
pub fn completion_authority(
    execution: &GoalToolExecution<'_>,
    is_root: bool,
    goal: Option<GoalView>,
) -> Result<GoalToolAuthority, BodyError> {
    if has_direct_human_input(execution, is_root) {
        return Ok(GoalToolAuthority::DirectHuman);
    }
    if let Some(goal) = goal
        && is_matching_goal_round(execution, &goal)
    {
        return Ok(GoalToolAuthority::GoalRound(goal));
    }
    Err(harness_error(
        "GOAL_TOOL_AUTHORITY_REQUIRED",
        "complete and blocked require a direct human turn or the current goal round",
    ))
}

/// The grounding sentence the wrap-up blocks share (dsh `GROUNDING`).
pub const GROUNDING: &str = "Report only what earlier rounds and tool results in this session actually establish; when a detail is not in the session, say so instead of inventing it. ";

/// Renders the closing-message instruction injected after an
/// autonomous goal round reports `complete` or `blocked` (dsh
/// `renderWrapupContext`).
#[must_use]
pub fn render_wrapup_context(objective: &str, blocked_reason: Option<&str>) -> Vec<Value> {
    let heading = format!("Objective: {}\n", json!(objective));
    let text = blocked_reason.map_or_else(
        || {
            "<goal_complete>\n".to_owned()
                + &heading
                + "The goal is marked complete and this autonomous run is ending. Write the closing "
                + "message to the user now: state the outcome, summarize what was done and how it was "
                + "verified, and point to the concrete results (files, commits, or other artifacts). "
                + GROUNDING
                + "Note anything the user should review or do next. Address the user directly. Do not "
                + "call any more tools in this run; further work waits for the user's next instruction.\n"
                + "</goal_complete>"
        },
        |reason| {
            "<goal_blocked>\n".to_owned()
                + &heading
                + &format!("Blocked: {}\n", json!(reason))
                + "The goal is marked blocked and this autonomous run is ending. Write the closing "
                + "message to the user now: state what has been completed so far, describe the concrete "
                + "blocking condition and what you tried, and say exactly what you need from the user to "
                + "continue. "
                + GROUNDING
                + "Address the user directly. Do not call any more tools in this run; further work "
                + "waits for the user's next instruction.\n"
                + "</goal_blocked>"
        },
    );
    vec![json!({ "type": "text", "text": text })]
}

/// Builds the exact compare-and-set ref from model arguments (dsh
/// `goalRef`).
///
/// # Errors
///
/// The dsh refusal text, verbatim.
///
/// # Panics
///
/// Never: the validity gate proved the revision an integral in range
/// immediately before the read-back.
pub fn goal_ref(
    goal_id: Option<&str>,
    revision: Option<&Value>,
) -> Result<(String, i64), BodyError> {
    let id = goal_id.unwrap_or_default();
    let revision_number = revision.and_then(Value::as_f64);
    let valid = !id.is_empty()
        && id.trim() == id
        && revision_number
            .is_some_and(|number| number.fract() == 0.0 && (1.0..=MAX_SAFE).contains(&number));
    if !valid {
        return Err(harness_error(
            "GOAL_TOOL_INVALID_UPDATE",
            "goal_id must be non-empty and revision must be a positive safe integer",
        ));
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the gate proved the revision integral and in range, so the cast keeps every digit"
    )]
    let revision = revision_number.expect("checked") as i64;
    Ok((id.to_owned(), revision))
}

/// The safe-integer ceiling (the dsh `Number.isSafeInteger` bound).
const MAX_SAFE: f64 = 9_007_199_254_740_991.0;

/// The canonical goal-tool output value (dsh `GoalToolValue`); the
/// compact shape the output schema freezes (the camelCase keys ride
/// the protocol exemption).
#[must_use]
pub fn goal_value(goal: Option<&GoalView>) -> Value {
    let Some(goal) = goal else {
        return json!({ "goal": Value::Null });
    };
    let mut goal_object = serde_json::Map::new();
    goal_object.insert("id".to_owned(), json!(goal.snapshot.id));
    goal_object.insert("revision".to_owned(), json!(goal.snapshot.revision));
    goal_object.insert("objective".to_owned(), json!(goal.snapshot.objective));
    goal_object.insert("phase".to_owned(), json!(goal.snapshot.phase));
    goal_object.insert("roundsStarted".to_owned(), json!(goal.rounds_started));
    goal_object.insert(
        "maxGoalRounds".to_owned(),
        json!(goal.snapshot.max_goal_rounds),
    );
    if let Some(reason) = &goal.snapshot.blocked_reason {
        goal_object.insert(
            "blockedReason".to_owned(),
            json!({ "code": reason.code, "message": reason.message }),
        );
    }
    let mut object = serde_json::Map::new();
    object.insert("goal".to_owned(), Value::Object(goal_object));
    object.insert("activation".to_owned(), json!(goal.activation.as_str()));
    Value::Object(object)
}

/// Projects the model-facing text (dsh `output.render`:
/// `JSON.stringify(value)`).
#[must_use]
pub fn render(_args: &Value, value: &Value) -> Vec<Value> {
    let text = serde_json::to_string(value).unwrap_or_default();
    vec![json!({ "type": "text", "text": text })]
}

/// The generic args-only pending presentation (dsh `present`).
fn present(title: &str, kind: &str, raw_input: Option<Value>) -> Value {
    let mut object = serde_json::Map::new();
    object.insert("card".to_owned(), json!("generic"));
    object.insert("title".to_owned(), json!(title));
    object.insert("kind".to_owned(), json!(kind));
    if let Some(raw_input) = raw_input {
        object.insert("rawInput".to_owned(), raw_input);
    }
    Value::Object(object)
}

/// The `get_goal` pending-call presentation (dsh `presentCall`).
#[must_use]
pub fn present_call_get(_args: &Value) -> Value {
    present("Read current goal", "read", None)
}

/// The `create_goal` pending-call presentation (dsh `presentCall`).
#[must_use]
pub fn present_call_create(args: &Value) -> Value {
    present("Create goal", "other", args.get("objective").cloned())
}

/// The `update_goal` pending-call presentation (dsh `presentCall`; the
/// raw input prefers the blocker text, then the objective, the cap,
/// and finally the id).
#[must_use]
pub fn present_call_update(args: &Value) -> Value {
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let title = if action == "blocked" {
        "Mark goal".to_owned()
    } else {
        let mut chars = action.chars();
        chars.next().map_or_else(
            || " goal".to_owned(),
            |first| format!("{}{} goal", first.to_ascii_uppercase(), chars.as_str()),
        )
    };
    let raw_input = [
        args.get("blocked_reason"),
        args.get("objective"),
        args.get("max_goal_rounds"),
        args.get("goal_id"),
    ]
    .into_iter()
    .flatten()
    .next()
    .cloned()
    .filter(|value| !value.is_null());
    present(&title, "other", raw_input)
}

/// The `get_goal` tool body (dsh `execute`).
///
/// # Errors
///
/// The authority or read refusals.
pub fn execute_get(goals: &Goals, agent: Option<&str>) -> Result<Value, BodyError> {
    let execution = goal_tool_execution(agent, goals.seams())?;
    let _ = execution;
    let view = goals.get(execution.session).map_err(goal_error)?;
    Ok(goal_value(view.as_ref()))
}

/// The `create_goal` tool body (dsh `execute`; the direct-human gate
/// rides first).
///
/// # Errors
///
/// The authority or domain refusals.
pub fn execute_create(
    goals: &Goals,
    args: &Value,
    agent: Option<&str>,
) -> Result<Value, BodyError> {
    let execution = goal_tool_execution(agent, goals.seams())?;
    require_direct_human(&execution, true)?;
    let request = CreateGoalRequest {
        objective: args
            .get("objective")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        max_goal_rounds: args.get("max_goal_rounds").and_then(Value::as_i64),
    };
    let view = goals
        .create(execution.session, &request)
        .map_err(goal_error)?;
    Ok(goal_value(Some(&view)))
}

/// The `update_goal` tool body (dsh `execute`, call-for-call through
/// the three action families).
///
/// # Errors
///
/// The authority, policy, or domain refusals.
///
/// # Panics
///
/// Never: each `expect` reads a field the preceding branch gate proved
/// present.
#[expect(
    clippy::too_many_lines,
    reason = "the body mirrors the dsh execute one arm per action family; splitting it would hide the correspondence"
)]
pub fn execute_update(
    goals: &Goals,
    args: &Value,
    agent: Option<&str>,
    blocked_after: i64,
) -> Result<Value, BodyError> {
    let execution = goal_tool_execution(agent, goals.seams())?;
    let (goal_id, revision) = goal_ref(
        args.get("goal_id").and_then(Value::as_str),
        args.get("revision"),
    )?;
    let objective = args.get("objective").and_then(Value::as_str);
    let max_goal_rounds = args.get("max_goal_rounds").and_then(Value::as_i64);
    let blocked_reason = args.get("blocked_reason").and_then(Value::as_str);
    // The dsh replacements object: only meaningful (non-filler) fields
    // enter.
    let replacements = EditGoalRequest {
        objective: has_text(objective).then(|| objective.expect("checked").to_owned()),
        max_goal_rounds: if has_round_cap(args.get("max_goal_rounds")) {
            max_goal_rounds
        } else {
            None
        },
    };
    if args.get("action").and_then(Value::as_str) == Some("edit") {
        require_direct_human(&execution, true)?;
        if has_text(blocked_reason) {
            return Err(harness_error(
                "GOAL_TOOL_INVALID_UPDATE",
                "blocked_reason is valid only with action blocked",
            ));
        }
        let view = goals
            .edit(
                execution.session,
                &crate::goal::GoalRef {
                    id: goal_id,
                    revision,
                },
                &replacements,
            )
            .map_err(goal_error)?;
        return Ok(goal_value(Some(&view)));
    }
    if matches!(
        args.get("action").and_then(Value::as_str),
        Some("pause" | "resume")
    ) {
        require_direct_human(&execution, true)?;
        if has_text(objective)
            || has_round_cap(args.get("max_goal_rounds"))
            || has_text(blocked_reason)
        {
            return Err(harness_error(
                "GOAL_TOOL_INVALID_UPDATE",
                "objective and max_goal_rounds are valid only with action edit; blocked_reason is valid only with action blocked",
            ));
        }
        let current = goals.get(execution.session).map_err(goal_error)?;
        let reference = crate::goal::GoalRef {
            id: goal_id,
            revision,
        };
        if args.get("action").and_then(Value::as_str) == Some("resume")
            && let Some(current) = &current
            && current.snapshot.id == reference.id
            && current.snapshot.revision == reference.revision
            && current.snapshot.phase == "paused"
        {
            return Err(harness_error(
                "GOAL_TOOL_RESUME_PAUSED",
                "the model cannot resume a paused goal; the user must resume it",
            ));
        }
        let view = if args.get("action").and_then(Value::as_str) == Some("pause") {
            goals.pause(execution.session, &reference)
        } else {
            goals.resume(execution.session, &reference)
        }
        .map_err(goal_error)?;
        return Ok(goal_value(Some(&view)));
    }
    // The completion family (complete / blocked) rides the dual
    // authority.
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let current = goals.get(execution.session).map_err(goal_error)?;
    let authority = completion_authority(&execution, true, current)?;
    if has_text(objective) || has_round_cap(args.get("max_goal_rounds")) {
        return Err(harness_error(
            "GOAL_TOOL_INVALID_UPDATE",
            "objective and max_goal_rounds are valid only with action edit",
        ));
    }
    if action == "complete" && has_text(blocked_reason) {
        return Err(harness_error(
            "GOAL_TOOL_INVALID_UPDATE",
            "blocked_reason is valid only with action blocked",
        ));
    }
    if action == "blocked" && blocked_reason.is_none_or(|text| text.trim().is_empty()) {
        return Err(harness_error(
            "GOAL_TOOL_INVALID_UPDATE",
            "blocked_reason is required with action blocked",
        ));
    }
    if action == "blocked"
        && let GoalToolAuthority::GoalRound(goal) = &authority
        && goal.rounds_started < blocked_after
    {
        return Err(harness_error(
            "GOAL_TOOL_BLOCK_THRESHOLD",
            format!(
                "blocked requires at least {blocked_after} consecutive goal rounds; current round is {}",
                goal.rounds_started
            ),
        ));
    }
    let reference = crate::goal::GoalRef {
        id: goal_id,
        revision,
    };
    let view = if action == "complete" {
        goals.complete(execution.session, &reference)
    } else {
        goals.block(
            execution.session,
            &reference,
            "model-reported",
            blocked_reason.expect("checked above"),
        )
    }
    .map_err(goal_error)?;
    // The goal-round wrap-up arm: structurally unreachable in M1 (the
    // goal-round authority requires the driver's round messages) and
    // kept for parity with the dsh deferContext — the rendered blocks
    // pin through [`render_wrapup_context`].
    if let GoalToolAuthority::GoalRound(_goal) = &authority {
        let _ = render_wrapup_context(&view.snapshot.objective, blocked_reason);
    }
    Ok(goal_value(Some(&view)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goal_service::GoalActivation;
    use serde_json::json;

    /// The blocked-threshold resolver pins the bound (the dsh
    /// `resolveConfig` `TypeError`).
    #[test]
    fn config_resolver_pins_the_bound() {
        assert_eq!(resolve_config(BAKED_BLOCKED_AFTER).expect("valid"), 3);
        assert_eq!(
            resolve_config(0).unwrap_err(),
            "blockedAfterConsecutiveRounds must be a positive safe integer"
        );
    }

    /// The wrap-up blocks pin the dsh texts: the complete block omits
    /// the blocker line, the blocked block carries the JSON-quoted
    /// reason, both ground and close with the no-more-tools rule.
    #[test]
    fn wrapup_blocks_pin_both_terminal_shapes() {
        let complete = render_wrapup_context("ship it", None);
        assert_eq!(complete.len(), 1);
        let text = complete[0]["text"].as_str().expect("text");
        assert!(
            text.starts_with("<goal_complete>\nObjective: \"ship it\"\n"),
            "{text}"
        );
        assert!(text.contains(GROUNDING), "{text}");
        assert!(
            text.ends_with("waits for the user's next instruction.\n</goal_complete>"),
            "{text}"
        );
        assert!(!text.contains("Blocked:"), "{text}");
        let blocked = render_wrapup_context("ship it", Some("upstream down"));
        let text = blocked[0]["text"].as_str().expect("text");
        assert!(
            text.starts_with("<goal_blocked>\nObjective: \"ship it\"\n"),
            "{text}"
        );
        assert!(text.contains("Blocked: \"upstream down\"\n"), "{text}");
        assert!(
            text.ends_with("waits for the user's next instruction.\n</goal_blocked>"),
            "{text}"
        );
    }

    /// The compare-and-set ref builder pins the dsh refusal family.
    #[test]
    fn goal_ref_pins_the_update_validation() {
        assert_eq!(
            goal_ref(Some("goal-1"), Some(&json!(2))).expect("valid"),
            ("goal-1".to_owned(), 2)
        );
        let message = "goal_id must be non-empty and revision must be a positive safe integer";
        assert_eq!(
            goal_ref(Some(""), Some(&json!(1))).unwrap_err().message,
            message
        );
        assert_eq!(
            goal_ref(Some(" g "), Some(&json!(1))).unwrap_err().message,
            message
        );
        assert_eq!(
            goal_ref(Some("g"), Some(&json!(0))).unwrap_err().message,
            message
        );
        assert_eq!(goal_ref(Some("g"), None).unwrap_err().message, message);
    }

    /// The output value pins the dsh key order and the null shape.
    #[test]
    fn goal_value_pins_the_wire_shape() {
        assert_eq!(goal_value(None), json!({ "goal": Value::Null }));
        let view = crate::goal_service::GoalView {
            snapshot: crate::goal::GoalSnapshot {
                id: "goal-1".to_owned(),
                revision: 2,
                objective: "ship it".to_owned(),
                phase: "blocked".to_owned(),
                max_goal_rounds: 5,
                blocked_reason: Some(crate::goal::GoalBlockReason {
                    code: "model-reported".to_owned(),
                    message: "stuck".to_owned(),
                }),
            },
            rounds_started: 3,
            created_at: 1_000,
            updated_at: 1_500,
            activation: GoalActivation::Disarmed,
        };
        let value = goal_value(Some(&view));
        assert_eq!(
            value
                .as_object()
                .expect("object")
                .keys()
                .collect::<Vec<_>>(),
            vec!["goal", "activation"]
        );
        assert_eq!(
            value["goal"]
                .as_object()
                .expect("object")
                .keys()
                .collect::<Vec<_>>(),
            vec![
                "id",
                "revision",
                "objective",
                "phase",
                "roundsStarted",
                "maxGoalRounds",
                "blockedReason"
            ]
        );
        assert_eq!(value["activation"], json!("disarmed"));
        // The render face is the JSON.stringify text.
        let rendered = render(&Value::Null, &value);
        assert_eq!(
            rendered[0]["text"].as_str().expect("text"),
            serde_json::to_string(&value).expect("stringify")
        );
    }

    /// The pending presentations pin the titles, kinds, and raw-input
    /// preference chain.
    #[test]
    fn present_calls_pin_the_faces() {
        assert_eq!(
            present_call_get(&Value::Null),
            json!({ "card": "generic", "title": "Read current goal", "kind": "read" })
        );
        assert_eq!(
            present_call_create(&json!({ "objective": "ship it" })),
            json!({ "card": "generic", "title": "Create goal", "kind": "other", "rawInput": "ship it" })
        );
        assert_eq!(
            present_call_update(
                &json!({ "action": "blocked", "blocked_reason": "stuck", "goal_id": "g" })
            )["title"],
            json!("Mark goal")
        );
        assert_eq!(
            present_call_update(&json!({ "action": "resume", "goal_id": "g" }))["title"],
            json!("Resume goal")
        );
        // The raw input prefers the blocker, then objective, cap, id.
        assert_eq!(
            present_call_update(&json!({ "action": "edit", "objective": "o", "goal_id": "g" }))["rawInput"],
            json!("o")
        );
        assert_eq!(
            present_call_update(&json!({ "action": "edit", "max_goal_rounds": 4, "goal_id": "g" }))
                ["rawInput"],
            json!(4)
        );
        assert_eq!(
            present_call_update(&json!({ "action": "pause", "goal_id": "g" }))["rawInput"],
            json!("g")
        );
    }
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use crate::goal_service::{ColdGoalLog, GoalActivation};
    use serde_json::json;

    /// One scripted seam with an in-memory log and a switchable
    /// turn-boundary answer.
    #[derive(Debug, Default)]
    struct ScriptedSeams {
        logs: std::sync::Mutex<std::collections::HashMap<String, Vec<Value>>>,
        boundary: std::sync::Mutex<Option<Value>>,
    }

    impl GoalSeams for ScriptedSeams {
        fn read_log(&self, session: &str) -> Result<Option<ColdGoalLog>, String> {
            Ok(self
                .logs
                .lock()
                .expect("lock")
                .get(session)
                .map(|events| ColdGoalLog {
                    inherited_event_count: 0,
                    events: events.clone(),
                }))
        }

        fn append_change(&self, session: &str, change: &Value) -> Result<i64, String> {
            let mut logs = self.logs.lock().expect("lock");
            let events = logs.entry(session.to_owned()).or_default();
            let seq = i64::try_from(events.len()).unwrap_or(i64::MAX);
            events.push(json!({ "seq": seq, "type": crate::goal::CHANGE_EVENT, "data": change }));
            drop(logs);
            Ok(seq)
        }

        fn projection_state(&self, _session: &str, key: &str) -> Result<Option<Value>, String> {
            if key == TURN_BOUNDARY_KEY {
                return Ok(self.boundary.lock().expect("lock").clone());
            }
            Ok(None)
        }
    }

    /// Assembles the domain over an open turn seeded with one
    /// host-attested human message (the M1 direct-human shape).
    struct Rig {
        goals: Goals,
        seams: std::sync::Arc<ScriptedSeams>,
    }

    impl Rig {
        fn new(open_turn: bool) -> Self {
            let seams = std::sync::Arc::new(ScriptedSeams::default());
            *seams.boundary.lock().expect("lock") = if open_turn {
                Some(json!({ "openTurnStartSeq": 0 }))
            } else {
                Some(json!({ "openTurnStartSeq": Value::Null }))
            };
            let cloned = std::sync::Arc::clone(&seams);
            let goals = Goals::with_clock(Box::new(ClonedSeams(cloned)), Box::new(|| 1_000));
            Self { goals, seams }
        }

        /// Seeds one host-attested human message into the open turn.
        fn with_human_message(&self) {
            self.seams
                .logs
                .lock()
                .expect("lock")
                .entry("s1".to_owned())
                .or_default()
                .push(json!({
                    "seq": 1, "type": "user/message",
                    "data": { "source": { "kind": "user" } },
                }));
        }
    }

    /// Relays one Arc'd seam into the boxed trait object.
    #[derive(Debug)]
    struct ClonedSeams(std::sync::Arc<ScriptedSeams>);

    impl GoalSeams for ClonedSeams {
        fn read_log(&self, session: &str) -> Result<Option<ColdGoalLog>, String> {
            self.0.read_log(session)
        }

        fn append_change(&self, session: &str, change: &Value) -> Result<i64, String> {
            self.0.append_change(session, change)
        }

        fn projection_state(&self, session: &str, key: &str) -> Result<Option<Value>, String> {
            self.0.projection_state(session, key)
        }
    }

    /// The authority family: a missing agent refuses first, a closed
    /// or missing turn boundary refuses next, and a non-root scan
    /// refuses the direct-human gate.
    #[test]
    fn authority_fences_pin_the_refusal_order() {
        let rig = Rig::new(true);
        // No agent: the calling-agent refusal.
        assert_eq!(
            execute_get(&rig.goals, None).unwrap_err().message,
            "goal tools require a calling agent"
        );
        assert_eq!(
            execute_get(&rig.goals, None).unwrap_err().code.as_deref(),
            Some("GOAL_TOOL_AGENT_REQUIRED")
        );
        // Closed turn: the driver refusal.
        let closed = Rig::new(false);
        assert_eq!(
            execute_get(&closed.goals, Some("s1")).unwrap_err().message,
            "goal tools require an open model turn"
        );
        // The direct-human gate refuses a turn without a user message.
        assert_eq!(
            execute_get(&rig.goals, Some("s1")).expect("get runs"),
            json!({ "goal": Value::Null })
        );
        let refusal =
            execute_create(&rig.goals, &json!({ "objective": "ship it" }), Some("s1")).unwrap_err();
        assert_eq!(
            refusal.code.as_deref(),
            Some("GOAL_TOOL_AUTHORITY_REQUIRED")
        );
        assert_eq!(
            refusal.message,
            "this goal operation requires a direct human turn on a top-level agent"
        );
    }

    /// The create flow: the human turn arms a fresh goal, the output
    /// carries the full view, and the durable record lands.
    #[test]
    fn create_flow_commits_over_the_human_turn() {
        let rig = Rig::new(true);
        rig.with_human_message();
        let value = execute_create(
            &rig.goals,
            &json!({ "objective": "  ship it  ", "max_goal_rounds": 7 }),
            Some("s1"),
        )
        .expect("create");
        assert_eq!(value["goal"]["objective"], json!("ship it"));
        assert_eq!(value["goal"]["maxGoalRounds"], json!(7));
        assert_eq!(value["goal"]["phase"], json!("active"));
        assert_eq!(value["activation"], json!("armed"));
        // The durable record landed over the seams.
        let events = rig.seams.logs.lock().expect("lock")["s1"].clone();
        assert_eq!(events.len(), 2, "the human message plus the create");
        assert_eq!(events[1]["data"]["operation"], json!("create"));
        // A second create over the same turn refuses (already active).
        assert_eq!(
            execute_create(&rig.goals, &json!({ "objective": "more" }), Some("s1"))
                .unwrap_err()
                .code
                .as_deref(),
            Some("GOAL_ALREADY_EXISTS")
        );
    }

    /// The get flow reads the current goal with id and revision.
    #[test]
    fn get_flow_reads_the_current_goal() {
        let rig = Rig::new(true);
        rig.with_human_message();
        let created = execute_create(&rig.goals, &json!({ "objective": "ship it" }), Some("s1"))
            .expect("create");
        let read = execute_get(&rig.goals, Some("s1")).expect("get");
        assert_eq!(read["goal"]["id"], created["goal"]["id"]);
        assert_eq!(read["goal"]["revision"], json!(1));
        assert_eq!(read["goal"]["roundsStarted"], json!(0));
    }

    /// The update flow: edit and pause ride the direct-human gate; the
    /// resume-paused refusal; the completion family's constraint set;
    /// the blocked threshold refuses before the minimum rounds.
    #[test]
    fn update_flow_pins_the_action_families() {
        let rig = Rig::new(true);
        rig.with_human_message();
        let created = execute_create(&rig.goals, &json!({ "objective": "ship it" }), Some("s1"))
            .expect("create");
        let id = created["goal"]["id"].as_str().expect("id").to_owned();
        // edit: the objective replacement rides.
        let edited = execute_update(
            &rig.goals,
            &json!({ "goal_id": id, "revision": 1, "action": "edit", "objective": "ship it faster" }),
            Some("s1"),
            BAKED_BLOCKED_AFTER,
        )
        .expect("edit");
        assert_eq!(edited["goal"]["revision"], json!(2));
        assert_eq!(edited["goal"]["objective"], json!("ship it faster"));
        // edit with a blocker text refuses.
        assert_eq!(
            execute_update(
                &rig.goals,
                &json!({ "goal_id": id, "revision": 2, "action": "edit", "blocked_reason": "x" }),
                Some("s1"),
                BAKED_BLOCKED_AFTER,
            )
            .unwrap_err()
            .message,
            "blocked_reason is valid only with action blocked"
        );
        // pause with replacements refuses with the joined text.
        assert_eq!(
            execute_update(
                &rig.goals,
                &json!({ "goal_id": id, "revision": 2, "action": "pause", "objective": "o" }),
                Some("s1"),
                BAKED_BLOCKED_AFTER,
            )
            .unwrap_err()
            .message,
            "objective and max_goal_rounds are valid only with action edit; blocked_reason is valid only with action blocked"
        );
        // pause then the resume-paused refusal.
        let paused = execute_update(
            &rig.goals,
            &json!({ "goal_id": id, "revision": 2, "action": "pause" }),
            Some("s1"),
            BAKED_BLOCKED_AFTER,
        )
        .expect("pause");
        assert_eq!(paused["goal"]["phase"], json!("paused"));
        assert_eq!(
            execute_update(
                &rig.goals,
                &json!({ "goal_id": id, "revision": 3, "action": "resume" }),
                Some("s1"),
                BAKED_BLOCKED_AFTER,
            )
            .unwrap_err()
            .code
            .as_deref(),
            Some("GOAL_TOOL_RESUME_PAUSED")
        );
        // The direct-human complete lands the terminal phase.
        let completed = execute_update(
            &rig.goals,
            &json!({ "goal_id": id, "revision": 3, "action": "complete" }),
            Some("s1"),
            BAKED_BLOCKED_AFTER,
        )
        .expect("complete");
        assert_eq!(completed["goal"]["phase"], json!("complete"));
        assert_eq!(completed["activation"], json!("disarmed"));
        // blocked without a reason refuses.
        assert_eq!(
            execute_update(
                &rig.goals,
                &json!({ "goal_id": id, "revision": 4, "action": "blocked" }),
                Some("s1"),
                BAKED_BLOCKED_AFTER,
            )
            .unwrap_err()
            .message,
            "blocked_reason is required with action blocked"
        );
        // complete with a blocker text refuses.
        assert_eq!(
            execute_update(
                &rig.goals,
                &json!({ "goal_id": id, "revision": 4, "action": "complete", "blocked_reason": "x" }),
                Some("s1"),
                BAKED_BLOCKED_AFTER,
            )
            .unwrap_err()
            .message,
            "blocked_reason is valid only with action blocked"
        );
    }

    /// The blocked threshold: on the direct-human authority the block
    /// rides regardless of rounds (the threshold arm binds only the
    /// goal-round authority); the goal-round arm pins through the
    /// authority predicate.
    #[test]
    fn blocked_threshold_binds_only_the_goal_round_authority() {
        let rig = Rig::new(true);
        rig.with_human_message();
        let created = execute_create(&rig.goals, &json!({ "objective": "ship it" }), Some("s1"))
            .expect("create");
        let id = created["goal"]["id"].as_str().expect("id").to_owned();
        // Direct-human blocked with zero rounds passes the threshold
        // (the authority arm does not apply).
        let blocked = execute_update(
            &rig.goals,
            &json!({ "goal_id": id, "revision": 1, "action": "blocked", "blocked_reason": "stuck" }),
            Some("s1"),
            BAKED_BLOCKED_AFTER,
        )
        .expect("blocked");
        assert_eq!(blocked["goal"]["phase"], json!("blocked"));
        assert_eq!(
            blocked["goal"]["blockedReason"],
            json!({ "code": "model-reported", "message": "stuck" })
        );
        // The goal-round arm: a goal-round authority below the
        // threshold refuses with the round-cited text (the pure
        // composition through the synthetic view).
        let view = crate::goal_service::GoalView {
            snapshot: crate::goal::GoalSnapshot {
                id: id.clone(),
                revision: 1,
                objective: "ship it".to_owned(),
                phase: "active".to_owned(),
                max_goal_rounds: 9,
                blocked_reason: None,
            },
            rounds_started: 2,
            created_at: 1_000,
            updated_at: 1_000,
            activation: GoalActivation::Armed,
        };
        let execution = GoalToolExecution {
            session: "s1",
            events: vec![json!({
                "seq": 5, "type": "user/message",
                "data": { "source": { "kind": "goal", "goalId": id, "revision": 1, "round": 2 } },
            })],
            open_turn_start_seq: 0,
        };
        let authority = completion_authority(&execution, false, Some(view.clone())).expect("round");
        assert!(matches!(authority, GoalToolAuthority::GoalRound(_)));
        assert_eq!(
            harness_error(
                "GOAL_TOOL_BLOCK_THRESHOLD",
                format!(
                    "blocked requires at least {BAKED_BLOCKED_AFTER} consecutive goal rounds; current round is {}",
                    view.rounds_started
                ),
            )
            .message,
            "blocked requires at least 3 consecutive goal rounds; current round is 2"
        );
    }
}
