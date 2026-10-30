// Rust guideline compliant 2026-10-30
//! The goal domain's durable vocabulary and the strict replay fold
//! (dsh `@deepseek-ai/dsh-goal` `types.ts` + `fold.ts` + `runtime.ts`).
//!
//! Every durable `goal/change` snapshot and clear tombstone decodes
//! with exact key sets, phase rules, and counter/timestamp
//! preservation, and matching `user/message` goal-round sources
//! advance `roundsStarted` (the Q16c-2A full-port ruling; the round
//! producer machine stays outside this slice). The projection apply
//! latches the first replay failure into the state instead of
//! throwing (the dsh `applyGoalProjection` shape).

use serde_json::{Value, json};

use fr_wire::event::event_type;

/// Version of the goal change embedded in a round-zero message source
/// (dsh `GOAL_CHANGE_VERSION`).
pub const GOAL_CHANGE_VERSION: i64 = 1;

/// The durable change event (dsh `goal/change`; declared at activation).
pub const CHANGE_EVENT: &str = event_type::GOAL_CHANGE;

/// The projection key (dsh `goal`).
pub const PROJECTION_KEY: &str = "goal";

/// The projection format version (dsh `stateVersion: 6`).
pub const STATE_VERSION: u64 = 6;

/// The dsh `Number.MAX_SAFE_INTEGER` bound (the safe-integer family).
pub const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// The durable phases (dsh `GoalPhase`).
pub const PHASES: [&str; 4] = ["active", "paused", "blocked", "complete"];

/// The durable verbs (dsh `GoalOperation`).
pub const OPERATIONS: [&str; 7] = [
    "create", "edit", "pause", "resume", "complete", "block", "clear",
];

/// Compare-and-set identity for one exact goal revision (dsh
/// `GoalRef`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalRef {
    /// Stable goal identity.
    pub id: String,
    /// Positive revision; every durable mutation increments it.
    pub revision: i64,
}

/// One canonical blocker explanation (dsh `GoalBlockReason`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalBlockReason {
    /// Stable lower-kebab-case classification.
    pub code: String,
    /// Non-empty normalized explanation.
    pub message: String,
}

/// Full durable state written by every non-clear goal mutation (dsh
/// `GoalSnapshot`; the JSON key order follows the dsh constructor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalSnapshot {
    /// Stable goal identity.
    pub id: String,
    /// Positive revision; every durable mutation increments it.
    pub revision: i64,
    /// Human-requested completion objective.
    pub objective: String,
    /// Durable lifecycle phase.
    pub phase: String,
    /// The total admitted goal-round cap.
    pub max_goal_rounds: i64,
    /// Present exactly while the phase is `blocked`.
    pub blocked_reason: Option<GoalBlockReason>,
}

impl GoalSnapshot {
    /// The wire object (the dsh constructor key order; the camelCase
    /// protocol exemption).
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut object = serde_json::Map::new();
        object.insert("id".to_owned(), json!(self.id));
        object.insert("revision".to_owned(), json!(self.revision));
        object.insert("objective".to_owned(), json!(self.objective));
        object.insert("phase".to_owned(), json!(self.phase));
        object.insert("maxGoalRounds".to_owned(), json!(self.max_goal_rounds));
        if let Some(reason) = &self.blocked_reason {
            object.insert(
                "blockedReason".to_owned(),
                json!({ "code": reason.code, "message": reason.message }),
            );
        }
        Value::Object(object)
    }

    /// Decodes the wire object with the dsh strict key-set rules.
    ///
    /// # Errors
    ///
    /// The dsh refusal texts, verbatim.
    pub fn from_value(value: &Value) -> Result<Self, String> {
        decode_snapshot(value)
    }
}

/// The strict replay accumulator kept private to the pure fold (dsh
/// `GoalFoldState`).
#[derive(Debug, Clone, Default)]
pub struct GoalFoldState {
    /// Current goal, absent after a clear or before the first create.
    pub goal: Option<GoalSnapshot>,
    /// Highest admitted round for the current goal.
    pub rounds_started: i64,
    /// Current goal creation time.
    pub created_at: Option<i64>,
    /// Current goal mutation time.
    pub updated_at: Option<i64>,
    /// Latest mutation ref, including a clear tombstone.
    pub last_ref: Option<GoalRef>,
    /// Goal identities already created, in first-seen order (the JS
    /// `Set` iteration order).
    pub seen_goal_ids: Vec<String>,
}

impl GoalFoldState {
    /// Builds an empty replay accumulator (dsh `emptyGoalFoldState`).
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }
}

/// Whether a value is a JSON record rather than an array (dsh
/// `isRecord`).
const fn is_record(value: &Value) -> bool {
    matches!(value, Value::Object(_))
}

/// The sorted key join the dsh strict key checks compare (JS
/// `Object.keys(v).sort().join(',')`).
fn sorted_keys(value: &Value) -> String {
    let mut names: Vec<&str> = value
        .as_object()
        .map(|object| object.keys().map(String::as_str).collect())
        .unwrap_or_default();
    names.sort_unstable();
    names.join(",")
}

/// Requires one positive safe integer (dsh `positiveInteger`; the
/// field name rides the refusal text verbatim).
fn positive_integer(value: &Value, field: &str) -> Result<i64, String> {
    let integral = value.as_f64().is_some_and(|n| n.fract() == 0.0);
    match value.as_i64() {
        Some(number) if integral && (1..=MAX_SAFE_INTEGER).contains(&number) => Ok(number),
        _ => Err(format!(
            "goal change {field} must be a positive safe integer"
        )),
    }
}

/// Requires one non-negative safe integer (dsh `nonNegativeInteger`).
fn non_negative_integer(value: &Value, field: &str) -> Result<i64, String> {
    let integral = value.as_f64().is_some_and(|n| n.fract() == 0.0);
    match value.as_i64() {
        Some(number) if integral && (0..=MAX_SAFE_INTEGER).contains(&number) => Ok(number),
        _ => Err(format!(
            "goal change {field} must be a non-negative safe integer"
        )),
    }
}

/// Decodes one canonical blocker explanation (dsh `decodeBlockReason`).
fn decode_block_reason(value: &Value) -> Result<GoalBlockReason, String> {
    if !is_record(value) || sorted_keys(value) != "code,message" {
        return Err(
            "goal change goal.blockedReason must have exactly code and message fields".to_owned(),
        );
    }
    let code = value
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let lower_kebab = |word: &str| {
        let mut chars = word.chars();
        match chars.next() {
            Some(first) if first.is_ascii_lowercase() => {
                chars.all(|rest| rest.is_ascii_lowercase() || rest.is_ascii_digit() || rest == '-')
            }
            _ => false,
        }
    };
    if !lower_kebab(code) {
        return Err("goal change goal.blockedReason.code must be lower-kebab-case".to_owned());
    }
    // The lower-kebab shape also bans a trailing `-name-dash` run beyond
    // the word grammar: `-` alone between words only.
    if code.ends_with('-') || code.contains("--") {
        return Err("goal change goal.blockedReason.code must be lower-kebab-case".to_owned());
    }
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if message.trim().is_empty() || message.trim() != message {
        return Err(
            "goal change goal.blockedReason.message must be non-empty and normalized".to_owned(),
        );
    }
    Ok(GoalBlockReason {
        code: code.to_owned(),
        message: message.to_owned(),
    })
}

/// Decodes and validates one snapshot (dsh `decodeSnapshot`; the check
/// order follows the dsh decoder: record, id, objective, phase,
/// phase-dependent key set, then the numeric and blocker fields).
fn decode_snapshot(value: &Value) -> Result<GoalSnapshot, String> {
    if !is_record(value) {
        return Err("goal change goal must be a record".to_owned());
    }
    let id = value.get("id").and_then(Value::as_str).unwrap_or_default();
    if id.is_empty() {
        return Err("goal change goal.id must be a non-empty string".to_owned());
    }
    let objective = value
        .get("objective")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if objective.trim().is_empty() || objective.trim() != objective {
        return Err("goal change goal.objective must be non-empty and normalized".to_owned());
    }
    let phase = value
        .get("phase")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !PHASES.contains(&phase) {
        return Err("goal change goal.phase is invalid".to_owned());
    }
    let expected_keys = if phase == "blocked" {
        "blockedReason,id,maxGoalRounds,objective,phase,revision"
    } else {
        "id,maxGoalRounds,objective,phase,revision"
    };
    if sorted_keys(value) != expected_keys {
        return Err(format!(
            "goal change goal for phase {phase} must have exactly {expected_keys} fields"
        ));
    }
    let revision = positive_integer(&value["revision"], "goal.revision")?;
    let max_goal_rounds = positive_integer(&value["maxGoalRounds"], "goal.maxGoalRounds")?;
    let blocked_reason = if phase == "blocked" {
        Some(decode_block_reason(&value["blockedReason"])?)
    } else {
        None
    };
    Ok(GoalSnapshot {
        id: id.to_owned(),
        revision,
        objective: objective.to_owned(),
        phase: phase.to_owned(),
        max_goal_rounds,
        blocked_reason,
    })
}

/// Decodes and validates one ref (dsh `decodeRef`).
fn decode_ref(value: &Value) -> Result<GoalRef, String> {
    if !is_record(value) || sorted_keys(value) != "id,revision" {
        return Err("goal clear tombstone must have exactly id and revision fields".to_owned());
    }
    let id = value.get("id").and_then(Value::as_str).unwrap_or_default();
    if id.is_empty() {
        return Err("goal clear tombstone id must be a non-empty string".to_owned());
    }
    let revision = positive_integer(&value["revision"], "cleared.revision")?;
    Ok(GoalRef {
        id: id.to_owned(),
        revision,
    })
}

/// The decoded durable mutation (dsh `GoalChangeMeta`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalChange {
    /// The full-snapshot mutation.
    Snapshot(GoalSnapshotChange),
    /// The clear tombstone.
    Clear(GoalClearChange),
}

/// The full-snapshot mutation payload (dsh `GoalSnapshotChangeMeta`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalSnapshotChange {
    /// The state-changing verb.
    pub operation: String,
    /// The complete post-mutation snapshot.
    pub goal: GoalSnapshot,
    /// Highest admitted round at the mutation.
    pub rounds_started: i64,
    /// Epoch milliseconds of the create mutation.
    pub created_at: i64,
    /// Epoch milliseconds of the latest mutation.
    pub updated_at: i64,
}

/// The clear tombstone payload (dsh `GoalClearChangeMeta`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalClearChange {
    /// The tombstone ref one past the cleared snapshot.
    pub cleared: GoalRef,
    /// Epoch milliseconds of the clear.
    pub cleared_at: i64,
}

impl GoalChange {
    /// The revision identity carried by a snapshot or tombstone (dsh
    /// `goalChangeRef`).
    #[must_use]
    pub fn ref_of(&self) -> GoalRef {
        match self {
            Self::Snapshot(change) => GoalRef {
                id: change.goal.id.clone(),
                revision: change.goal.revision,
            },
            Self::Clear(change) => change.cleared.clone(),
        }
    }

    /// The state-changing verb.
    #[must_use]
    pub fn operation(&self) -> &str {
        match self {
            Self::Snapshot(change) => &change.operation,
            Self::Clear(_) => "clear",
        }
    }
}

/// Decodes a value that declares itself as a goal change (dsh
/// `decodeGoalChange`); unrelated values return `None`, malformed goal
/// changes fail replay loudly.
///
/// # Errors
///
/// The dsh refusal texts, verbatim.
pub fn decode_goal_change(value: &Value) -> Result<Option<GoalChange>, String> {
    if !is_record(value) || value.get("kind").and_then(Value::as_str) != Some(CHANGE_EVENT) {
        return Ok(None);
    }
    if value.get("version").and_then(Value::as_i64) != Some(GOAL_CHANGE_VERSION) {
        let rendered = value
            .get("version")
            .map_or_else(|| "undefined".to_owned(), render_js_value);
        return Err(format!("unsupported goal change version {rendered}"));
    }
    if value.get("operation").and_then(Value::as_str) == Some("clear") {
        let allowed = ["cleared", "clearedAt", "kind", "operation", "version"];
        if sorted_keys(value) != allowed.join(",") {
            return Err(format!(
                "goal clear change must have exactly {} fields",
                allowed.join(",")
            ));
        }
        return Ok(Some(GoalChange::Clear(GoalClearChange {
            cleared: decode_ref(&value["cleared"])?,
            cleared_at: non_negative_integer(&value["clearedAt"], "clearedAt")?,
        })));
    }
    let operation = value
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !OPERATIONS.contains(&operation) || operation == "clear" {
        return Err("goal change operation is invalid".to_owned());
    }
    let allowed = [
        "createdAt",
        "goal",
        "kind",
        "operation",
        "roundsStarted",
        "updatedAt",
        "version",
    ];
    if sorted_keys(value) != allowed.join(",") {
        return Err(format!(
            "goal snapshot change must have exactly {} fields",
            allowed.join(",")
        ));
    }
    let created_at = non_negative_integer(&value["createdAt"], "createdAt")?;
    let updated_at = non_negative_integer(&value["updatedAt"], "updatedAt")?;
    if updated_at < created_at {
        return Err("goal change updatedAt cannot precede createdAt".to_owned());
    }
    Ok(Some(GoalChange::Snapshot(GoalSnapshotChange {
        operation: operation.to_owned(),
        goal: decode_snapshot(&value["goal"])?,
        rounds_started: non_negative_integer(&value["roundsStarted"], "roundsStarted")?,
        created_at,
        updated_at,
    })))
}

/// Renders a JSON value the way a JS template literal would `String()`
/// it (the version-refusal argument form).
fn render_js_value(raw: &Value) -> String {
    match raw {
        Value::String(text) => text.clone(),
        Value::Number(number) => number
            .as_f64()
            .map_or_else(|| number.to_string(), |number| format!("{number}")),
        Value::Bool(flag) => flag.to_string(),
        Value::Null => "null".to_owned(),
        other => other.to_string(),
    }
}

/// Requires two snapshots to retain fields that only `edit` may replace
/// (dsh `requireSameDefinition`).
fn require_same_definition(
    current: &GoalSnapshot,
    next: &GoalSnapshot,
    operation: &str,
) -> Result<(), String> {
    if next.objective != current.objective || next.max_goal_rounds != current.max_goal_rounds {
        return Err(format!(
            "goal {operation} cannot change objective or maxGoalRounds"
        ));
    }
    Ok(())
}

/// Requires one exact next revision of the current goal (dsh
/// `requireNextRevision`).
fn require_next_revision(
    current: &GoalSnapshot,
    next: &GoalRef,
    operation: &str,
) -> Result<(), String> {
    if next.id != current.id || next.revision != current.revision + 1 {
        return Err(format!(
            "goal {operation} must advance the current goal by one revision"
        ));
    }
    Ok(())
}

/// Validates one non-create snapshot operation against the preceding
/// projection (dsh `validateSnapshotTransition`; the arm order follows
/// the dsh switch).
fn validate_snapshot_transition(
    state: &GoalFoldState,
    change: &GoalSnapshotChange,
    current: &GoalSnapshot,
) -> Result<(), String> {
    let next = &change.goal;
    require_next_revision(
        current,
        &GoalRef {
            id: next.id.clone(),
            revision: next.revision,
        },
        change.operation.as_str(),
    )?;
    let Some(updated_at) = state.updated_at else {
        return Err("current goal fold lacks updatedAt".to_owned());
    };
    if change.created_at != state.created_at.unwrap_or_default()
        || change.updated_at < updated_at
        || change.rounds_started != state.rounds_started
    {
        return Err(format!(
            "goal {} does not preserve the current counters and timestamps",
            change.operation
        ));
    }
    match change.operation.as_str() {
        "edit" => {
            if next.phase != current.phase || next.blocked_reason != current.blocked_reason {
                return Err("goal edit cannot change phase or blocked reason".to_owned());
            }
            Ok(())
        }
        "pause" => {
            require_same_definition(current, next, "pause")?;
            if current.phase != "active" || next.phase != "paused" {
                return Err("goal pause has an invalid phase transition".to_owned());
            }
            Ok(())
        }
        "resume" => {
            require_same_definition(current, next, "resume")?;
            let resumable = ["active", "paused", "blocked"];
            if !resumable.contains(&current.phase.as_str())
                || next.phase != "active"
                || state.rounds_started >= next.max_goal_rounds
            {
                return Err(
                    "goal resume has an invalid phase transition or exhausted round budget"
                        .to_owned(),
                );
            }
            Ok(())
        }
        "complete" => {
            require_same_definition(current, next, "complete")?;
            if current.phase == "complete" || next.phase != "complete" {
                return Err("goal complete has an invalid phase transition".to_owned());
            }
            Ok(())
        }
        "block" => {
            require_same_definition(current, next, "block")?;
            if current.phase != "active" || next.phase != "blocked" {
                return Err("goal block has an invalid phase transition".to_owned());
            }
            Ok(())
        }
        // The caller excludes create (the dsh fail-loud arm).
        "create" => Err("goal create cannot be validated as a current-goal transition".to_owned()),
        other => Err(format!("unknown goal snapshot operation {other}")),
    }
}

/// Validates and applies one decoded change to the accumulator (dsh
/// `applyGoalChange`).
///
/// # Errors
///
/// The dsh refusal texts, verbatim.
pub fn apply_goal_change(state: &mut GoalFoldState, change: &GoalChange) -> Result<(), String> {
    let ref_of = change.ref_of();
    if let GoalChange::Clear(clear) = change {
        let Some(current) = &state.goal else {
            return Err("goal clear requires a current goal".to_owned());
        };
        require_next_revision(current, &clear.cleared, "clear")?;
        let Some(updated_at) = state.updated_at else {
            return Err("current goal fold lacks updatedAt".to_owned());
        };
        if clear.cleared_at < updated_at {
            return Err("goal clear timestamp cannot precede the current goal update".to_owned());
        }
        state.goal = None;
        state.rounds_started = 0;
        state.created_at = None;
        state.updated_at = None;
        state.last_ref = Some(ref_of);
        return Ok(());
    }
    let GoalChange::Snapshot(snapshot) = change else {
        return Ok(());
    };
    if snapshot.operation == "create" {
        if snapshot.goal.revision != 1
            || snapshot.goal.phase != "active"
            || snapshot.rounds_started != 0
            || state
                .goal
                .as_ref()
                .is_some_and(|current| current.phase != "complete")
            || state.seen_goal_ids.contains(&snapshot.goal.id)
        {
            return Err(
                "goal create requires a fresh active revision-one goal with zero rounds".to_owned(),
            );
        }
        state.seen_goal_ids.push(snapshot.goal.id.clone());
    } else {
        let Some(current) = state.goal.clone() else {
            return Err(format!(
                "goal {} requires a current goal",
                snapshot.operation
            ));
        };
        validate_snapshot_transition(state, snapshot, &current)?;
    }
    state.goal = Some(snapshot.goal.clone());
    state.rounds_started = snapshot.rounds_started;
    state.created_at = Some(snapshot.created_at);
    state.updated_at = Some(snapshot.updated_at);
    state.last_ref = Some(ref_of);
    Ok(())
}

/// Narrows model attribution to a valid goal source (dsh `goalSource`).
fn goal_source(source: &Value) -> Result<Option<(String, i64, i64)>, String> {
    if source.get("kind").and_then(Value::as_str) != Some("goal") {
        return Ok(None);
    }
    let goal_id = source
        .get("goalId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let revision = source.get("revision").and_then(Value::as_i64).unwrap_or(0);
    let revision_integral = source
        .get("revision")
        .and_then(Value::as_f64)
        .is_some_and(|n| n.fract() == 0.0);
    let round = source.get("round").and_then(Value::as_i64).unwrap_or(0);
    let round_integral = source
        .get("round")
        .and_then(Value::as_f64)
        .is_some_and(|n| n.fract() == 0.0);
    let safe_positive =
        |number: i64, integral: bool| (1..=MAX_SAFE_INTEGER).contains(&number) && integral;
    if goal_id.is_empty()
        || !safe_positive(revision, revision_integral)
        || !safe_positive(round, round_integral)
    {
        return Err("goal message source is invalid".to_owned());
    }
    Ok(Some((goal_id.to_owned(), revision, round)))
}

/// Applies one session event to the strict durable goal fold (dsh
/// `applyGoalEvent`), including the admitted goal-round advancement.
///
/// # Errors
///
/// The dsh refusal texts, verbatim.
pub fn apply_goal_event(state: &mut GoalFoldState, event: &Value) -> Result<(), String> {
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if event_type == CHANGE_EVENT {
        let change = decode_goal_change(&event["data"])?.ok_or_else(|| {
            format!(
                "goal change at session event {} has an invalid kind",
                event_seq(event)
            )
        })?;
        apply_goal_change(state, &change)
    } else if event_type == "user/message" {
        let Some((goal_id, revision, round)) = goal_source(&event["data"]["source"])? else {
            return Ok(());
        };
        let Some(current) = &state.goal else {
            return Err(format!(
                "goal round at session event {} is not the next admitted round of the active goal",
                event_seq(event)
            ));
        };
        if current.phase != "active"
            || goal_id != current.id
            || revision != current.revision
            || round != state.rounds_started + 1
            || round > current.max_goal_rounds
        {
            return Err(format!(
                "goal round at session event {} is not the next admitted round of the active goal",
                event_seq(event)
            ));
        }
        state.rounds_started = round;
        Ok(())
    } else {
        Ok(())
    }
}

/// The event's sequence number (the fold refusal texts cite it).
fn event_seq(event: &Value) -> i64 {
    event.get("seq").and_then(Value::as_i64).unwrap_or_default()
}

/// The projection state value (dsh `GoalProjectionState`; the JSON key
/// order follows the dsh object).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GoalProjectionState {
    /// Latest valid current goal projection.
    pub current: Option<GoalProjection>,
    /// Goal identities already created, in first-seen order.
    pub seen_goal_ids: Vec<String>,
    /// First strict replay failure.
    pub failure: Option<String>,
}

/// The projection's current-goal value (dsh `GoalProjection`;
/// activation is process-local and deliberately absent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalProjection {
    /// Current durable goal snapshot.
    pub goal: GoalSnapshot,
    /// Highest admitted round number for this goal.
    pub rounds_started: i64,
    /// Epoch milliseconds of the create mutation.
    pub created_at: i64,
    /// Epoch milliseconds of the latest mutation.
    pub updated_at: i64,
}

impl GoalProjection {
    /// The wire object (the dsh key order; camelCase protocol
    /// exemption).
    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({
            "goal": self.goal.to_value(),
            "roundsStarted": self.rounds_started,
            "createdAt": self.created_at,
            "updatedAt": self.updated_at,
        })
    }
}

impl GoalProjectionState {
    /// The wire object (the dsh key order).
    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({
            "current": self
                .current
                .as_ref()
                .map_or_else(|| Value::Null, GoalProjection::to_value),
            "seenGoalIds": self.seen_goal_ids,
            "failure": self
                .failure
                .as_ref()
                .map_or_else(|| Value::Null, |text| json!(text)),
        })
    }

    /// Builds strict fold state from one checkpoint-safe projection
    /// state (dsh `goalFoldState`).
    #[must_use]
    pub fn to_fold_state(&self) -> GoalFoldState {
        GoalFoldState {
            goal: self.current.as_ref().map(|current| current.goal.clone()),
            rounds_started: self
                .current
                .as_ref()
                .map_or(0, |current| current.rounds_started),
            created_at: self.current.as_ref().map(|current| current.created_at),
            updated_at: self.current.as_ref().map(|current| current.updated_at),
            last_ref: None,
            seen_goal_ids: self.seen_goal_ids.clone(),
        }
    }

    /// Converts strict fold state into checkpoint-safe projection state
    /// (dsh `goalProjectionState`).
    ///
    /// # Errors
    ///
    /// The missing-timestamp refusal (the dsh `current goal fold lacks
    /// timestamps` text).
    pub fn from_fold_state(state: &GoalFoldState) -> Result<Self, String> {
        let current = if let Some(goal) = &state.goal {
            let (Some(created_at), Some(updated_at)) = (state.created_at, state.updated_at) else {
                return Err("current goal fold lacks timestamps".to_owned());
            };
            Some(GoalProjection {
                goal: goal.clone(),
                rounds_started: state.rounds_started,
                created_at,
                updated_at,
            })
        } else {
            None
        };
        Ok(Self {
            current,
            seen_goal_ids: state.seen_goal_ids.clone(),
            failure: None,
        })
    }
}

/// Folds durable goal events through the strict replay rules without
/// throwing from the projection registry's event drive (dsh
/// `applyGoalProjection`).
///
/// The first invalid owned event is retained in `failure`; later events
/// keep the latched state. The event filter mirrors the dsh gate: only
/// `goal/change` and goal-sourced `user/message` events touch the
/// state.
#[must_use]
pub fn apply_goal_projection(state: &GoalProjectionState, event: &Value) -> GoalProjectionState {
    if state.failure.is_some() {
        return state.clone();
    }
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let goal_sourced = event_type == "user/message"
        && event
            .pointer("/data/source/kind")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind == "goal");
    if event_type != CHANGE_EVENT && !goal_sourced {
        return state.clone();
    }
    let mut folded = state.to_fold_state();
    if let Err(message) = apply_goal_event(&mut folded, event) {
        return GoalProjectionState {
            failure: Some(format!(
                "goal replay failed at session event {}: {}",
                event_seq(event),
                message
            )),
            ..state.clone()
        };
    }
    GoalProjectionState::from_fold_state(&folded).unwrap_or_else(|message| GoalProjectionState {
        failure: Some(format!(
            "goal replay failed at session event {}: {}",
            event_seq(event),
            message
        )),
        ..state.clone()
    })
}

/// Folds the full committed log onto the projection state (the fold
/// endpoint body: init then one apply per event).
#[must_use]
pub fn fold_log(events: &[Value]) -> GoalProjectionState {
    let mut state = GoalProjectionState::default();
    for event in events {
        state = apply_goal_projection(&state, event);
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds one strict snapshot-change payload the way the dsh
    /// service commits it.
    fn snapshot_change(operation: &str, revision: i64, phase: &str) -> Value {
        let mut goal = json!({
            "id": "goal-1",
            "revision": revision,
            "objective": "ship the release",
            "phase": phase,
            "maxGoalRounds": 5,
        });
        if phase == "blocked" {
            goal["blockedReason"] = json!({ "code": "model-reported", "message": "stuck" });
        }
        json!({
            "kind": "goal/change",
            "version": 1,
            "operation": operation,
            "goal": goal,
            "roundsStarted": 0,
            "createdAt": 1_000,
            "updatedAt": 1_500,
        })
    }

    #[test]
    fn decode_pins_the_strict_key_sets_and_numeric_bounds() {
        // A valid create decodes.
        let change = decode_goal_change(&snapshot_change("create", 1, "active"))
            .expect("valid")
            .expect("a goal change");
        assert_eq!(change.operation(), "create");
        // An unrelated value returns None.
        assert_eq!(
            decode_goal_change(&json!({ "kind": "todo/write" })).expect("unrelated"),
            None
        );
        // A wrong version refuses with the JS-stringified value.
        let mut wrong = snapshot_change("create", 1, "active");
        wrong["version"] = json!(2);
        assert_eq!(
            decode_goal_change(&wrong).unwrap_err(),
            "unsupported goal change version 2"
        );
        let mut missing = snapshot_change("create", 1, "active");
        missing.as_object_mut().expect("object").remove("version");
        assert_eq!(
            decode_goal_change(&missing).unwrap_err(),
            "unsupported goal change version undefined"
        );
        // Extra fields refuse with the sorted-key texts.
        let mut extra = snapshot_change("create", 1, "active");
        extra["extra"] = json!(1);
        assert_eq!(
            decode_goal_change(&extra).unwrap_err(),
            "goal snapshot change must have exactly createdAt,goal,kind,operation,roundsStarted,updatedAt,version fields"
        );
        // The blocked snapshot requires the blocker key set.
        assert_eq!(
            decode_snapshot(&json!({
                "id": "g", "revision": 1, "objective": "o", "phase": "blocked",
                "maxGoalRounds": 2,
            }))
            .unwrap_err(),
            "goal change goal for phase blocked must have exactly blockedReason,id,maxGoalRounds,objective,phase,revision fields"
        );
        // The blocker decode pins code shape and message normalization.
        assert_eq!(
            decode_block_reason(&json!({ "code": "Bad", "message": "m" })).unwrap_err(),
            "goal change goal.blockedReason.code must be lower-kebab-case"
        );
        assert_eq!(
            decode_block_reason(&json!({ "code": "a", "message": " m " })).unwrap_err(),
            "goal change goal.blockedReason.message must be non-empty and normalized"
        );
        // Non-positive revisions refuse with the field-verbatim text.
        assert_eq!(
            positive_integer(&json!(0), "goal.revision").unwrap_err(),
            "goal change goal.revision must be a positive safe integer"
        );
    }

    #[test]
    fn fold_pins_the_lifecycle_transitions() {
        // Create seeds the current goal and retains the id.
        let mut state = GoalFoldState::empty();
        apply_goal_event(
            &mut state,
            &json!({
                "seq": 0, "type": "goal/change",
                "data": snapshot_change("create", 1, "active"),
            }),
        )
        .expect("create folds");
        assert_eq!(state.goal.as_ref().expect("goal").id, "goal-1");
        assert_eq!(state.rounds_started, 0);
        assert_eq!(state.seen_goal_ids, vec!["goal-1".to_owned()]);
        // A goal round advances the counter; an out-of-order round
        // refuses with the event-cited text.
        apply_goal_event(&mut state, &json!({
            "seq": 1, "type": "user/message",
            "data": { "source": { "kind": "goal", "goalId": "goal-1", "revision": 1, "round": 1 } },
        }))
        .expect("round folds");
        assert_eq!(state.rounds_started, 1);
        assert_eq!(
            apply_goal_event(&mut state, &json!({
                "seq": 2, "type": "user/message",
                "data": { "source": { "kind": "goal", "goalId": "goal-1", "revision": 1, "round": 3 } },
            }))
            .unwrap_err(),
            "goal round at session event 2 is not the next admitted round of the active goal"
        );
        // Pause then resume round-trips; a non-adjacent revision
        // refuses. The counters ride the mutation (the dsh
        // `commitCurrent` preservation), so the payloads carry the
        // advanced round count.
        let mut pause = snapshot_change("pause", 2, "paused");
        pause["roundsStarted"] = json!(1);
        apply_goal_event(
            &mut state,
            &json!({
                "seq": 3, "type": "goal/change",
                "data": pause,
            }),
        )
        .expect("pause folds");
        assert_eq!(
            apply_goal_event(
                &mut state,
                &json!({
                    "seq": 4, "type": "goal/change",
                    "data": snapshot_change("resume", 4, "active"),
                })
            )
            .unwrap_err(),
            "goal resume must advance the current goal by one revision"
        );
        let mut resume = snapshot_change("resume", 3, "active");
        resume["roundsStarted"] = json!(1);
        apply_goal_event(
            &mut state,
            &json!({
                "seq": 4, "type": "goal/change",
                "data": resume,
            }),
        )
        .expect("resume folds");
        assert_eq!(state.goal.as_ref().expect("goal").phase, "active");
        // A create reuses the seen id and refuses.
        assert_eq!(
            apply_goal_change(
                &mut state,
                &decode_goal_change(&snapshot_change("create", 1, "active"))
                    .expect("decode")
                    .expect("change")
            )
            .unwrap_err(),
            "goal create requires a fresh active revision-one goal with zero rounds"
        );
        // Complete, then clear; the clear tombstone resets the fold.
        let mut complete = snapshot_change("complete", 4, "complete");
        complete["roundsStarted"] = json!(1);
        apply_goal_event(
            &mut state,
            &json!({
                "seq": 5, "type": "goal/change",
                "data": complete,
            }),
        )
        .expect("complete folds");
        apply_goal_event(
            &mut state,
            &json!({
                "seq": 6, "type": "goal/change",
                "data": {
                    "kind": "goal/change", "version": 1, "operation": "clear",
                    "cleared": { "id": "goal-1", "revision": 5 },
                    "clearedAt": 1_600,
                },
            }),
        )
        .expect("clear folds");
        assert_eq!(state.goal, None);
        assert_eq!(state.last_ref.expect("tombstone").revision, 5);
    }

    #[test]
    fn projection_latches_failures_and_keeps_later_events_inert() {
        // A valid log folds to the projection state.
        let log = vec![
            json!({ "seq": 0, "type": "goal/change", "data": snapshot_change("create", 1, "active") }),
            json!({ "seq": 1, "type": "user/message", "data": {} }),
            json!({ "seq": 2, "type": "user/message", "data": {
                "source": { "kind": "goal", "goalId": "goal-1", "revision": 1, "round": 1 } } }),
        ];
        let state = fold_log(&log);
        assert!(state.failure.is_none(), "{:?}", state.failure);
        let current = state.current.as_ref().expect("current");
        assert_eq!(current.rounds_started, 1);
        assert_eq!(state.seen_goal_ids, vec!["goal-1".to_owned()]);
        // The wire object carries the dsh key order and camelCase keys.
        let wire = state.to_value();
        assert_eq!(
            wire.as_object().expect("object").keys().collect::<Vec<_>>(),
            vec!["current", "seenGoalIds", "failure"]
        );
        assert_eq!(
            current
                .to_value()
                .as_object()
                .expect("object")
                .keys()
                .collect::<Vec<_>>(),
            vec!["goal", "roundsStarted", "createdAt", "updatedAt"]
        );
        // A corrupt change latches the failure and later valid events
        // stay inert.
        let mut corrupt = snapshot_change("create", 1, "active");
        corrupt["roundsStarted"] = json!(3);
        let latched = fold_log(&[json!({
            "seq": 0, "type": "goal/change", "data": corrupt,
        })]);
        let latched = apply_goal_projection(
            &latched,
            &json!({ "seq": 1, "type": "user/message", "data": {
                "source": { "kind": "goal", "goalId": "goal-1", "revision": 1, "round": 1 } } }),
        );
        assert_eq!(
            latched.failure.as_deref(),
            Some(
                "goal replay failed at session event 0: goal create requires a fresh active revision-one goal with zero rounds"
            )
        );
        // An unrelated event never touches the state.
        let untouched = apply_goal_projection(
            &GoalProjectionState::default(),
            &json!({ "seq": 9, "type": "step/start", "data": {} }),
        );
        assert_eq!(untouched, GoalProjectionState::default());
    }
}
