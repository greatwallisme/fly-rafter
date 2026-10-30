// Rust guideline compliant 2026-10-30
//! The same-session goal service over the owning session log (dsh
//! `@deepseek-ai/dash-goal` `index.ts` service half).
//!
//! M1 structural facts: the agents registry has no counterpart (the
//! dsh `assertLive` live-identity arm is unreachable — the composition
//! binds the caller to the live driver's session; the 16a jobs-local
//! ruling), so the mutations fence on the session id string alone; the
//! scoped `goal/changed` and process-local `goal/activation-changed`
//! notifications have no M1 consumer (the driver-side subscription
//! lands with the goal-round driver, Phase 18) and stay dropped with
//! the dsh emit faces documented here. The activation machine keeps
//! the dsh semantics minus the eager listeners: runtime states start
//! disarmed per process (the `WeakMap`-fresh shape), and the commit
//! applies its pending edge by comparing the captured pre-append
//! offset with the landed seq — the interleaving mismatch arm is the
//! defensive dead arm under the M1 single-writer fact and stays
//! implemented for parity.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{Value, json};

use crate::goal::{
    GOAL_CHANGE_VERSION, GoalBlockReason, GoalChange, GoalClearChange, GoalProjection,
    GoalProjectionState, GoalRef, GoalSnapshot, GoalSnapshotChange, MAX_SAFE_INTEGER,
};

/// The deployment default round cap (dsh `Config.defaultMaxGoalRounds`
/// default).
///
/// The headless bundle adds no goal config row, so the baked default is
/// the only arm — the dylib receives no config, the fr-shell judgment.
pub const BAKED_DEFAULT_MAX_GOAL_ROUNDS: i64 = 256;

/// Whether this live process may automatically continue an active goal
/// (dsh `GoalActivation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalActivation {
    /// The driver may continue the goal automatically.
    Armed,
    /// Continuation waits for a human-authorized resume.
    Disarmed,
}

impl GoalActivation {
    /// The wire word (the protocol exemption).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Armed => "armed",
            Self::Disarmed => "disarmed",
        }
    }
}

/// The process-local runtime state crossing the synchronous append
/// boundary (dsh `GoalRuntimeState`).
#[derive(Debug, Clone, Copy)]
struct RuntimeState {
    activation: GoalActivation,
    pending: Option<(i64, GoalActivation)>,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            activation: GoalActivation::Disarmed,
            pending: None,
        }
    }
}

/// The full live goal view the service hands out (dsh `GoalView`; the
/// detached current shape the tools serialize).
#[derive(Debug, Clone)]
pub struct GoalView {
    /// The durable snapshot.
    pub snapshot: GoalSnapshot,
    /// Highest admitted round number.
    pub rounds_started: i64,
    /// Epoch milliseconds of the create mutation.
    pub created_at: i64,
    /// Epoch milliseconds of the latest mutation.
    pub updated_at: i64,
    /// Process-local continuation eligibility.
    pub activation: GoalActivation,
}

/// The wire seams: the cold log read, the durable append (returning
/// the landed seq), and the third-party projection relay read.
pub trait GoalSeams: std::fmt::Debug + Send + Sync {
    /// Cold-reads the session's stored log (`None` when absent).
    ///
    /// # Errors
    ///
    /// The persistence refusal text.
    fn read_log(&self, session: &str) -> Result<Option<ColdGoalLog>, String>;

    /// Appends one durable `goal/change` record; returns the landed
    /// seq.
    ///
    /// # Errors
    ///
    /// The persistence refusal text.
    fn append_change(&self, session: &str, change: &Value) -> Result<i64, String>;

    /// Reads one registered projection through the relay (`None` when
    /// the key is unregistered — the omitted-state reply).
    ///
    /// # Errors
    ///
    /// The relay or decode refusal text.
    fn projection_state(&self, session: &str, key: &str) -> Result<Option<Value>, String>;
}

/// The cold-read log the fold consumes (the session-read face shape).
#[derive(Debug, Clone)]
pub struct ColdGoalLog {
    /// The exact inherited prefix length.
    pub inherited_event_count: u64,
    /// The preserved events in log order.
    pub events: Vec<Value>,
}

/// A validated create request with every default materialized.
#[derive(Debug, Clone)]
pub struct CreateGoalRequest {
    /// The objective, pre-normalization.
    pub objective: String,
    /// The caller round cap, when present.
    pub max_goal_rounds: Option<i64>,
}

/// A validated edit request.
#[derive(Debug, Clone)]
pub struct EditGoalRequest {
    /// The replacement objective, when present.
    pub objective: Option<String>,
    /// The replacement cap, when present.
    pub max_goal_rounds: Option<i64>,
}

/// The goal domain boundary error (dsh `GoalError` extends
/// `HarnessError`, so the code stays routable and the class name rides
/// as `GoalError`; the fs-domain judgment shape).
#[derive(Debug, Clone)]
pub struct GoalError {
    /// The stable machine-routable classification.
    pub code: String,
    /// The human-readable rejection reason.
    pub message: String,
}

impl GoalError {
    /// One coded boundary failure.
    #[must_use]
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
        }
    }

    /// A plain relayed failure (a dsh plain `Error`: the projection
    /// replay failure surfaces verbatim, unroutable).
    #[must_use]
    pub fn plain(message: impl Into<String>) -> Self {
        Self {
            code: String::new(),
            message: message.into(),
        }
    }
}

/// Validates a caller-visible positive safe-integer round cap (dsh
/// `resolveMaxGoalRounds`).
fn resolve_max_goal_rounds(value: i64) -> Result<i64, GoalError> {
    if (1..=MAX_SAFE_INTEGER).contains(&value) {
        Ok(value)
    } else {
        Err(GoalError::new(
            "GOAL_INVALID_MAX_ROUNDS",
            "maxGoalRounds must be a positive safe integer",
        ))
    }
}

/// Validates and normalizes an objective at the domain boundary (dsh
/// `resolveObjective`).
fn resolve_objective(value: Option<&str>) -> Result<String, GoalError> {
    let valid = value.is_some_and(|text| !text.trim().is_empty());
    if !valid {
        return Err(GoalError::new(
            "GOAL_INVALID_OBJECTIVE",
            "goal objective must be a non-empty string",
        ));
    }
    Ok(value.expect("checked non-empty").trim().to_owned())
}

/// Validates and detaches one policy-owned blocker explanation (dsh
/// `resolveBlockReason`; the shape mirrors the fold's lower-kebab
/// rule).
fn resolve_block_reason(
    code: Option<&str>,
    message: Option<&str>,
) -> Result<GoalBlockReason, GoalError> {
    let refusal = || {
        GoalError::new(
            "GOAL_INVALID_BLOCK_REASON",
            "goal block reason requires a lower-kebab-case code and a non-empty message",
        )
    };
    let Some(code) = code else {
        return Err(refusal());
    };
    let Some(message) = message.filter(|text| !text.trim().is_empty()) else {
        return Err(refusal());
    };
    let mut chars = code.chars();
    let shaped = match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {
            let body =
                chars.all(|rest| rest.is_ascii_lowercase() || rest.is_ascii_digit() || rest == '-');
            body && !code.ends_with('-') && !code.contains("--")
        }
        _ => false,
    };
    if !shaped {
        return Err(refusal());
    }
    Ok(GoalBlockReason {
        code: code.to_owned(),
        message: message.trim().to_owned(),
    })
}

/// Renders a stable invalid-transition error (dsh
/// `transitionError`).
fn transition_error(current: &GoalSnapshot, operation: &str, allowed: &[&str]) -> GoalError {
    GoalError::new(
        "GOAL_INVALID_TRANSITION",
        format!(
            "cannot {operation} goal \"{}\" from phase \"{}\"; expected {}",
            current.id,
            current.phase,
            allowed.join(" or ")
        ),
    )
}

/// The goal service backed exclusively by the owning session log (dsh
/// `GoalService`).
pub struct Goals {
    resolved_max_goal_rounds: i64,
    states: Mutex<HashMap<String, RuntimeState>>,
    seams: Box<dyn GoalSeams>,
    now: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl std::fmt::Debug for Goals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Goals")
            .field("resolved_max_goal_rounds", &self.resolved_max_goal_rounds)
            .finish_non_exhaustive()
    }
}

impl Goals {
    /// Assembles the service over the baked default round cap and the
    /// wire seams, under the real wall clock.
    #[must_use]
    pub fn new(seams: Box<dyn GoalSeams>) -> Self {
        Self::with_clock(
            seams,
            Box::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |duration| {
                        i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
                    })
            }),
        )
    }

    /// Assembles the service under an injected clock (the wall-clock
    /// rollback pin tests).
    ///
    /// # Panics
    ///
    /// Never in practice: the baked default is a constant in range, so
    /// the static resolution cannot refuse.
    ///
    #[must_use]
    pub fn with_clock(seams: Box<dyn GoalSeams>, now: Box<dyn Fn() -> i64 + Send + Sync>) -> Self {
        // The baked default resolves statically (a constant in range by
        // construction; the resolve fn stays for the parity pin).
        Self {
            resolved_max_goal_rounds: resolve_max_goal_rounds(BAKED_DEFAULT_MAX_GOAL_ROUNDS)
                .expect("the baked default is in range"),
            states: Mutex::new(HashMap::new()),
            seams,
            now,
        }
    }

    /// The hosted wire seams (the tool bodies' authority reads ride
    /// the same faces).
    #[must_use]
    pub fn seams(&self) -> &dyn GoalSeams {
        self.seams.as_ref()
    }

    /// Reads the current goal for one exact live agent session (dsh
    /// `get`).
    ///
    /// # Errors
    ///
    /// The relayed replay-failure text or the read refusal (plain).
    pub fn get(&self, session: &str) -> Result<Option<GoalView>, GoalError> {
        let state = self.projection(session).map_err(GoalError::plain)?;
        let activation = self.activation_of(session);
        Ok(view(&state, activation))
    }

    /// Creates and arms a goal. A completed goal may be replaced; every
    /// other current phase must be cleared or resumed instead (dsh
    /// `create`).
    ///
    /// # Errors
    ///
    /// The dsh `GoalError` family, verbatim.
    ///
    /// # Panics
    ///
    /// Never: the existence check proved the current goal present
    /// immediately before the read-back.
    pub fn create(
        &self,
        session: &str,
        request: &CreateGoalRequest,
    ) -> Result<GoalView, GoalError> {
        let max_goal_rounds = resolve_max_goal_rounds(
            request
                .max_goal_rounds
                .unwrap_or(self.resolved_max_goal_rounds),
        )?;
        let objective = resolve_objective(Some(&request.objective))?;
        let state = self.projection(session).map_err(GoalError::plain)?;
        if state
            .current
            .as_ref()
            .is_some_and(|current| current.goal.phase != "complete")
        {
            let current = state.current.as_ref().expect("checked present");
            return Err(GoalError::new(
                "GOAL_ALREADY_EXISTS",
                format!(
                    "goal \"{}\" already exists with phase \"{}\"",
                    current.goal.id, current.goal.phase
                ),
            ));
        }
        let now = (self.now)();
        let goal = GoalSnapshot {
            id: format!("goal-{}", uuid::Uuid::new_v4()),
            revision: 1,
            objective,
            phase: "active".to_owned(),
            max_goal_rounds,
            blocked_reason: None,
        };
        self.commit_snapshot(session, "create", &goal, 0, now, now, GoalActivation::Armed)
    }

    /// Edits objective and/or round cap without changing phase (dsh
    /// `edit`).
    ///
    /// # Errors
    ///
    /// The dsh `GoalError` family, verbatim.
    pub fn edit(
        &self,
        session: &str,
        reference: &GoalRef,
        request: &EditGoalRequest,
    ) -> Result<GoalView, GoalError> {
        let state = self.prepare_mutation(session, reference)?;
        let Some(current) = &state.current else {
            return Err(GoalError::new("GOAL_NOT_FOUND", "no current goal"));
        };
        if request.objective.is_none() && request.max_goal_rounds.is_none() {
            return Err(GoalError::new(
                "GOAL_INVALID_EDIT",
                "goal edit requires objective and/or maxGoalRounds",
            ));
        }
        let mut goal = current.goal.clone();
        goal.revision += 1;
        if let Some(objective) = &request.objective {
            goal.objective = resolve_objective(Some(objective))?;
        }
        if let Some(max_goal_rounds) = request.max_goal_rounds {
            goal.max_goal_rounds = resolve_max_goal_rounds(max_goal_rounds)?;
        }
        self.commit_current(session, current, "edit", &goal, self.activation_of(session))
    }

    /// Pauses an active goal and disarms automatic continuation (dsh
    /// `pause`).
    ///
    /// # Errors
    ///
    /// The dsh `GoalError` family, verbatim.
    pub fn pause(&self, session: &str, reference: &GoalRef) -> Result<GoalView, GoalError> {
        self.transition(
            session,
            reference,
            "pause",
            &["active"],
            "paused",
            GoalActivation::Disarmed,
        )
    }

    /// Resumes and arms a stopped goal, or rearms an active goal after
    /// a session-start edge, while its round budget still has capacity
    /// (dsh `resume`).
    ///
    /// # Errors
    ///
    /// The dsh `GoalError` family, verbatim.
    pub fn resume(&self, session: &str, reference: &GoalRef) -> Result<GoalView, GoalError> {
        let state = self.prepare_mutation(session, reference)?;
        let Some(current) = &state.current else {
            return Err(GoalError::new("GOAL_NOT_FOUND", "no current goal"));
        };
        let resumable = ["active", "paused", "blocked"];
        if !resumable.contains(&current.goal.phase.as_str()) {
            return Err(transition_error(&current.goal, "resume", &resumable));
        }
        if current.goal.phase == "active" && self.activation_of(session) == GoalActivation::Armed {
            return Err(GoalError::new(
                "GOAL_INVALID_TRANSITION",
                format!("goal \"{}\" is already active and armed", current.goal.id),
            ));
        }
        if current.rounds_started >= current.goal.max_goal_rounds {
            return Err(GoalError::new(
                "GOAL_INVALID_TRANSITION",
                format!(
                    "goal \"{}\" exhausted {} goal rounds; increase maxGoalRounds before resuming",
                    current.goal.id, current.goal.max_goal_rounds
                ),
            ));
        }
        let mut goal = current.goal.clone();
        goal.revision += 1;
        goal.phase = "active".to_string();
        goal.blocked_reason = None;
        self.commit_current(session, current, "resume", &goal, GoalActivation::Armed)
    }

    /// Marks a current non-complete goal complete and disarms it (dsh
    /// `complete`).
    ///
    /// # Errors
    ///
    /// The dsh `GoalError` family, verbatim.
    pub fn complete(&self, session: &str, reference: &GoalRef) -> Result<GoalView, GoalError> {
        self.transition(
            session,
            reference,
            "complete",
            &["active", "paused", "blocked"],
            "complete",
            GoalActivation::Disarmed,
        )
    }

    /// Marks an active goal blocked and disarms it, validating and
    /// detaching the policy-owned explanation (dsh `block` over
    /// `resolveBlockReason`).
    ///
    /// # Errors
    ///
    /// The dsh `GoalError` family, verbatim.
    pub fn block(
        &self,
        session: &str,
        reference: &GoalRef,
        code: &str,
        message: &str,
    ) -> Result<GoalView, GoalError> {
        let reason = resolve_block_reason(Some(code), Some(message))?;
        let state = self.prepare_mutation(session, reference)?;
        let Some(current) = &state.current else {
            return Err(GoalError::new("GOAL_NOT_FOUND", "no current goal"));
        };
        if current.goal.phase != "active" {
            return Err(transition_error(&current.goal, "block", &["active"]));
        }
        let mut goal = current.goal.clone();
        goal.revision += 1;
        goal.phase = "blocked".to_string();
        goal.blocked_reason = Some(reason);
        self.commit_current(session, current, "block", &goal, GoalActivation::Disarmed)
    }

    /// Clears the current goal while retaining a durable tombstone (dsh
    /// `clear`); returns the tombstone ref one past the cleared
    /// snapshot.
    ///
    /// # Errors
    ///
    /// The dsh `GoalError` family, verbatim.
    pub fn clear(&self, session: &str, reference: &GoalRef) -> Result<GoalRef, GoalError> {
        let state = self.prepare_mutation(session, reference)?;
        let Some(current) = &state.current else {
            return Err(GoalError::new("GOAL_NOT_FOUND", "no current goal"));
        };
        let tombstone = GoalRef {
            id: current.goal.id.clone(),
            revision: current.goal.revision + 1,
        };
        let change = GoalChange::Clear(GoalClearChange {
            cleared: tombstone.clone(),
            cleared_at: self.next_mutation_time(current.updated_at),
        });
        self.commit(session, &change, GoalActivation::Disarmed)?;
        Ok(tombstone)
    }

    /// Resolves the durable projection used by a mutation and rejects
    /// stale or missing refs (dsh `prepareMutation` +
    /// `expectCurrent`).
    fn prepare_mutation(
        &self,
        session: &str,
        reference: &GoalRef,
    ) -> Result<GoalProjectionState, GoalError> {
        let state = self.projection(session).map_err(GoalError::plain)?;
        let Some(current) = &state.current else {
            return Err(GoalError::new("GOAL_NOT_FOUND", "no current goal"));
        };
        if reference.id != current.goal.id || reference.revision != current.goal.revision {
            return Err(GoalError::new(
                "GOAL_STALE_REVISION",
                format!(
                    "stale goal ref \"{}\" revision {}; current is \"{}\" revision {}",
                    reference.id, reference.revision, current.goal.id, current.goal.revision
                ),
            ));
        }
        Ok(state)
    }

    /// The shared validated phase transition (dsh `transition`).
    fn transition(
        &self,
        session: &str,
        reference: &GoalRef,
        operation: &str,
        allowed: &[&str],
        phase: &str,
        activation: GoalActivation,
    ) -> Result<GoalView, GoalError> {
        let state = self.prepare_mutation(session, reference)?;
        let Some(current) = &state.current else {
            return Err(GoalError::new("GOAL_NOT_FOUND", "no current goal"));
        };
        if !allowed.contains(&current.goal.phase.as_str()) {
            return Err(transition_error(&current.goal, operation, allowed));
        }
        let mut goal = current.goal.clone();
        goal.revision += 1;
        goal.phase = phase.to_string();
        // The dsh `withPhase` rebuild carries no blocker: only `block`
        // attaches one, so leaving the blocked phase drops it.
        goal.blocked_reason = None;
        self.commit_current(session, current, operation, &goal, activation)
    }

    /// Commits a mutation that retains the current goal's derived
    /// counters and times (dsh `commitCurrent`).
    fn commit_current(
        &self,
        session: &str,
        current: &GoalProjection,
        operation: &str,
        goal: &GoalSnapshot,
        activation: GoalActivation,
    ) -> Result<GoalView, GoalError> {
        self.commit_snapshot(
            session,
            operation,
            goal,
            current.rounds_started,
            current.created_at,
            self.next_mutation_time(current.updated_at),
            activation,
        )
    }

    /// Clamps a current goal's next timestamp across backward
    /// wall-clock movement (dsh `nextMutationTime`).
    fn next_mutation_time(&self, updated_at: i64) -> i64 {
        (self.now)().max(updated_at)
    }

    /// Builds and commits one full-snapshot mutation (dsh
    /// `commitSnapshot`).
    #[expect(
        clippy::too_many_arguments,
        reason = "the tuple mirrors the dsh commitSnapshot signature one field per parameter"
    )]
    fn commit_snapshot(
        &self,
        session: &str,
        operation: &str,
        goal: &GoalSnapshot,
        rounds_started: i64,
        created_at: i64,
        updated_at: i64,
        activation: GoalActivation,
    ) -> Result<GoalView, GoalError> {
        let change = GoalChange::Snapshot(GoalSnapshotChange {
            operation: operation.to_owned(),
            goal: goal.clone(),
            rounds_started,
            created_at,
            updated_at,
        });
        let view_activation = self.commit(session, &change, activation)?;
        Ok(GoalView {
            snapshot: goal.clone(),
            rounds_started,
            created_at,
            updated_at,
            activation: view_activation,
        })
    }

    /// Commits one mutation into the goal log and applies the
    /// process-local activation edge (dsh `commit`; the pending-edge
    /// offset comparison rides the append reply's landed seq).
    fn commit(
        &self,
        session: &str,
        change: &GoalChange,
        activation: GoalActivation,
    ) -> Result<GoalActivation, GoalError> {
        // The pre-append offset capture (the dsh `agent.session.seq`):
        // one cold read under the mutation, before the lock.
        let offset = self.next_seq(session).map_err(GoalError::plain)?;
        {
            let mut states = self.states.lock().expect("goal runtime states lock");
            states.entry(session.to_owned()).or_default().pending = Some((offset, activation));
        }
        let landed = match self.seams.append_change(session, &encode_change(change)) {
            Ok(seq) => seq,
            Err(message) => {
                self.states
                    .lock()
                    .expect("goal runtime states lock")
                    .get_mut(session)
                    .expect("the pending entry")
                    .pending = None;
                return Err(GoalError::plain(format!(
                    "goal commit append failed: {message}"
                )));
            }
        };
        let mut states = self.states.lock().expect("goal runtime states lock");
        let runtime = states.get_mut(session).expect("the pending entry");
        let applied = runtime.pending == Some((landed, activation));
        runtime.pending = None;
        runtime.activation = if applied {
            activation
        } else {
            // The interleaving mismatch (the defensive dead arm under
            // the single-writer fact): the dsh `session/event` listener
            // collapses to the same disarm.
            GoalActivation::Disarmed
        };
        let view_activation = runtime.activation;
        drop(states);
        Ok(view_activation)
    }

    /// The next sequence number the session's log will land (the
    /// pre-append offset capture).
    fn next_seq(&self, session: &str) -> Result<i64, String> {
        self.seams.read_log(session)?.map_or(Ok(0), |log| {
            let inherited = i64::try_from(log.inherited_event_count).unwrap_or(i64::MAX);
            let count = i64::try_from(log.events.len()).unwrap_or(i64::MAX);
            Ok(inherited.saturating_add(count))
        })
    }

    /// Reads the durable projection through the self-held fold (the
    /// Q110A-prime registrant self-read: cold-read the log tail, fold
    /// locally).
    fn projection(&self, session: &str) -> Result<GoalProjectionState, String> {
        let Some(log) = self.seams.read_log(session)? else {
            return Ok(GoalProjectionState::default());
        };
        let state = crate::goal::fold_log(&log.events);
        // The dsh `state()` reads: a latched replay failure surfaces
        // verbatim (a plain Error) while the failure is set.
        if let Some(failure) = &state.failure {
            return Err(failure.clone());
        }
        Ok(state)
    }

    /// Returns the process-local activation state, initially disarmed
    /// (dsh `runtimeState`; the lazy fresh-process shape).
    fn activation_of(&self, session: &str) -> GoalActivation {
        self.states
            .lock()
            .expect("goal runtime states lock")
            .get(session)
            .map_or(GoalActivation::Disarmed, |runtime| runtime.activation)
    }
}

/// Builds a detached current view (dsh `view`).
fn view(state: &GoalProjectionState, activation: GoalActivation) -> Option<GoalView> {
    let current = state.current.as_ref()?;
    Some(GoalView {
        snapshot: current.goal.clone(),
        rounds_started: current.rounds_started,
        created_at: current.created_at,
        updated_at: current.updated_at,
        activation,
    })
}

/// Encodes one mutation onto the durable wire shape (the dsh change
/// key order).
fn encode_change(change: &GoalChange) -> Value {
    match change {
        GoalChange::Snapshot(snapshot) => {
            let mut object = serde_json::Map::new();
            object.insert("kind".to_owned(), json!(crate::goal::CHANGE_EVENT));
            object.insert("version".to_owned(), json!(GOAL_CHANGE_VERSION));
            object.insert("operation".to_owned(), json!(snapshot.operation));
            object.insert("goal".to_owned(), snapshot.goal.to_value());
            object.insert("roundsStarted".to_owned(), json!(snapshot.rounds_started));
            object.insert("createdAt".to_owned(), json!(snapshot.created_at));
            object.insert("updatedAt".to_owned(), json!(snapshot.updated_at));
            Value::Object(object)
        }
        GoalChange::Clear(clear) => {
            let mut object = serde_json::Map::new();
            object.insert("kind".to_owned(), json!(crate::goal::CHANGE_EVENT));
            object.insert("version".to_owned(), json!(GOAL_CHANGE_VERSION));
            object.insert("operation".to_owned(), json!("clear"));
            object.insert(
                "cleared".to_owned(),
                json!({ "id": clear.cleared.id, "revision": clear.cleared.revision }),
            );
            object.insert("clearedAt".to_owned(), json!(clear.cleared_at));
            Value::Object(object)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// One scripted seam core: an in-memory log per session and the
    /// relay answers.
    #[derive(Debug, Default)]
    struct FakeSeams {
        logs: std::sync::Mutex<HashMap<String, Vec<Value>>>,
    }

    /// The shared handle (services over one log).
    #[derive(Debug, Default, Clone)]
    struct SharedSeams(std::sync::Arc<FakeSeams>);

    impl std::ops::Deref for SharedSeams {
        type Target = FakeSeams;

        fn deref(&self) -> &FakeSeams {
            &self.0
        }
    }

    impl GoalSeams for SharedSeams {
        fn read_log(&self, session: &str) -> Result<Option<ColdGoalLog>, String> {
            let logs = self.logs.lock().expect("lock");
            Ok(logs.get(session).map(|events| ColdGoalLog {
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

        fn projection_state(&self, _session: &str, _key: &str) -> Result<Option<Value>, String> {
            Ok(None)
        }
    }

    /// A clock seam returning a fixed stamp.
    fn fixed_clock(at: i64) -> Box<dyn Fn() -> i64 + Send + Sync> {
        Box::new(move || at)
    }

    #[test]
    fn create_commits_the_dsh_shape_and_arms() {
        let seams = SharedSeams::default();
        let service = Goals::with_clock(Box::new(seams.clone()), fixed_clock(1_000));
        let view = service
            .create(
                "s1",
                &CreateGoalRequest {
                    objective: "  ship it  ".to_owned(),
                    max_goal_rounds: None,
                },
            )
            .expect("create");
        // The objective normalizes; the id is a fresh goal- uuid; the
        // view arms and carries the create stamps.
        assert_eq!(view.snapshot.objective, "ship it");
        assert!(view.snapshot.id.starts_with("goal-"));
        assert_eq!(view.snapshot.revision, 1);
        assert_eq!(view.snapshot.phase, "active");
        assert_eq!(view.rounds_started, 0);
        assert_eq!(view.created_at, 1_000);
        assert_eq!(view.updated_at, 1_000);
        assert_eq!(view.activation, GoalActivation::Armed);
        // The durable record carries the dsh key order and shape.
        let events = seams
            .logs
            .lock()
            .expect("lock")
            .get("s1")
            .expect("session log")
            .clone();
        assert_eq!(events.len(), 1);
        let data = &events[0]["data"];
        assert_eq!(
            data.as_object().expect("object").keys().collect::<Vec<_>>(),
            vec![
                "kind",
                "version",
                "operation",
                "goal",
                "roundsStarted",
                "createdAt",
                "updatedAt"
            ]
        );
        assert_eq!(data["operation"], json!("create"));
        assert_eq!(data["goal"]["maxGoalRounds"], json!(256));
        // A second create refuses while active.
        let error = service
            .create(
                "s1",
                &CreateGoalRequest {
                    objective: "another".to_owned(),
                    max_goal_rounds: None,
                },
            )
            .unwrap_err();
        assert_eq!(error.code, "GOAL_ALREADY_EXISTS");
        assert_eq!(
            error.message,
            format!(
                "goal \"{}\" already exists with phase \"active\"",
                view.snapshot.id
            )
        );
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the family test mirrors the dsh lifecycle one arm per action; splitting it would hide the correspondence"
    )]
    fn lifecycle_pins_the_transition_family() {
        let seams = SharedSeams::default();
        let service = Goals::with_clock(Box::new(seams), fixed_clock(1_000));
        let created = service
            .create(
                "s1",
                &CreateGoalRequest {
                    objective: "ship it".to_owned(),
                    max_goal_rounds: Some(2),
                },
            )
            .expect("create");
        let reference = GoalRef {
            id: created.snapshot.id.clone(),
            revision: created.snapshot.revision,
        };
        // An edit without fields refuses.
        assert_eq!(
            service
                .edit(
                    "s1",
                    &reference,
                    &EditGoalRequest {
                        objective: None,
                        max_goal_rounds: None,
                    }
                )
                .unwrap_err()
                .message,
            "goal edit requires objective and/or maxGoalRounds"
        );
        // The edit advances the revision and keeps the phase.
        let edited = service
            .edit(
                "s1",
                &reference,
                &EditGoalRequest {
                    objective: Some("ship it faster".to_owned()),
                    max_goal_rounds: Some(3),
                },
            )
            .expect("edit");
        assert_eq!(edited.snapshot.revision, 2);
        assert_eq!(edited.snapshot.objective, "ship it faster");
        assert_eq!(edited.snapshot.max_goal_rounds, 3);
        // A stale ref refuses with the dsh text.
        let error = service
            .pause(
                "s1",
                &GoalRef {
                    id: created.snapshot.id.clone(),
                    revision: 1,
                },
            )
            .unwrap_err();
        assert_eq!(error.code, "GOAL_STALE_REVISION");
        // Pause disarms; resume rearms.
        let paused = service
            .pause(
                "s1",
                &GoalRef {
                    id: edited.snapshot.id.clone(),
                    revision: 2,
                },
            )
            .expect("pause");
        assert_eq!(paused.snapshot.phase, "paused");
        assert_eq!(paused.activation, GoalActivation::Disarmed);
        let resumed = service
            .resume(
                "s1",
                &GoalRef {
                    id: edited.snapshot.id.clone(),
                    revision: 3,
                },
            )
            .expect("resume");
        assert_eq!(resumed.activation, GoalActivation::Armed);
        // The exhausted budget refuses the resume (rounds 0 < 3 here,
        // so drive rounds through the fold first).
        assert_eq!(edited.snapshot.max_goal_rounds, 3);
        // Block an active goal with the policy reason.
        let blocked = service
            .block(
                "s1",
                &GoalRef {
                    id: edited.snapshot.id.clone(),
                    revision: 4,
                },
                "model-reported",
                "  upstream down  ",
            )
            .expect("block");
        assert_eq!(blocked.snapshot.phase, "blocked");
        assert_eq!(
            blocked
                .snapshot
                .blocked_reason
                .as_ref()
                .expect("reason")
                .message,
            "upstream down"
        );
        assert_eq!(blocked.activation, GoalActivation::Disarmed);
        // Complete lands the terminal phase; a fresh create replaces.
        let completed = service
            .complete(
                "s1",
                &GoalRef {
                    id: edited.snapshot.id,
                    revision: 5,
                },
            )
            .expect("complete");
        assert_eq!(completed.snapshot.phase, "complete");
        let replaced = service
            .create(
                "s1",
                &CreateGoalRequest {
                    objective: "next".to_owned(),
                    max_goal_rounds: None,
                },
            )
            .expect("replace after complete");
        assert_ne!(replaced.snapshot.id, created.snapshot.id);
        // Clear tombstones one past the cleared snapshot.
        let tombstone = service
            .clear(
                "s1",
                &GoalRef {
                    id: replaced.snapshot.id.clone(),
                    revision: replaced.snapshot.revision,
                },
            )
            .expect("clear");
        assert_eq!(tombstone.revision, replaced.snapshot.revision + 1);
        assert!(service.get("s1").expect("get").is_none());
    }

    #[test]
    fn budget_exhaustion_and_block_reason_pins() {
        let seams = SharedSeams::default();
        let service = Goals::with_clock(Box::new(seams.clone()), fixed_clock(1_000));
        let created = service
            .create(
                "s1",
                &CreateGoalRequest {
                    objective: "ship it".to_owned(),
                    max_goal_rounds: Some(1),
                },
            )
            .expect("create");
        // A goal round folds into the log and exhausts the budget.
        seams
            .logs
            .lock()
            .expect("lock")
            .get_mut("s1")
            .expect("session log")
            .push(json!({
            "seq": 1, "type": "user/message",
            "data": { "source": { "kind": "goal", "goalId": created.snapshot.id, "revision": 1, "round": 1 } },
        }));
        // The restart shape (the dsh process-local activation): a
        // fresh service over the same log reads disarmed, so the
        // armed-arm skips and the exhausted budget refuses.
        let restarted = Goals::with_clock(Box::new(seams), fixed_clock(1_000));
        let error = restarted
            .resume(
                "s1",
                &GoalRef {
                    id: created.snapshot.id.clone(),
                    revision: 1,
                },
            )
            .unwrap_err();
        assert_eq!(error.code, "GOAL_INVALID_TRANSITION");
        assert_eq!(
            error.message,
            format!(
                "goal \"{}\" exhausted 1 goal rounds; increase maxGoalRounds before resuming",
                created.snapshot.id
            )
        );
        // The block reason shape refuses.
        assert_eq!(
            resolve_block_reason(Some("Bad"), Some("m"))
                .unwrap_err()
                .message,
            "goal block reason requires a lower-kebab-case code and a non-empty message"
        );
        assert_eq!(
            resolve_block_reason(Some("ok-code"), Some("  "))
                .unwrap_err()
                .message,
            "goal block reason requires a lower-kebab-case code and a non-empty message"
        );
        assert_eq!(
            resolve_objective(Some("   ")).unwrap_err().message,
            "goal objective must be a non-empty string"
        );
    }

    #[test]
    fn wall_clock_rollback_clamps_and_replay_failure_surfaces() {
        let seams = SharedSeams::default();
        // Create at 2_000, then run the edit under a clock rolled back
        // to 1_000: the mutation clamps to the previous updatedAt.
        let service = Goals::with_clock(Box::new(seams.clone()), fixed_clock(2_000));
        let created = service
            .create(
                "s1",
                &CreateGoalRequest {
                    objective: "ship it".to_owned(),
                    max_goal_rounds: None,
                },
            )
            .expect("create");
        let rolled = Goals::with_clock(Box::new(seams.clone()), fixed_clock(1_000));
        let edited = rolled
            .edit(
                "s1",
                &GoalRef {
                    id: created.snapshot.id,
                    revision: 1,
                },
                &EditGoalRequest {
                    objective: Some("reworded".to_owned()),
                    max_goal_rounds: None,
                },
            )
            .expect("edit");
        assert_eq!(edited.created_at, 2_000);
        assert_eq!(edited.updated_at, 2_000, "the clamp keeps the later stamp");
        // A corrupt log surfaces the replay failure verbatim (plain,
        // unroutable).
        seams
            .logs
            .lock()
            .expect("lock")
            .get_mut("s1")
            .expect("session log")
            .push(json!({
                "seq": 9, "type": crate::goal::CHANGE_EVENT,
                "data": { "kind": "goal/change", "version": 99 },
            }));
        let error = service.get("s1").unwrap_err();
        assert_eq!(error.code, "");
        assert!(
            error
                .message
                .starts_with("goal replay failed at session event 9:"),
            "{}",
            error.message
        );
    }
}
