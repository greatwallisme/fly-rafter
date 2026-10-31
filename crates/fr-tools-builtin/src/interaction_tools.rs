// Rust guideline compliant 2026-10-31
//! The interaction tools: `ask_user_question` (dsh `tool-ask-user`)
//! and `exit_plan_mode` (the plan-mode controller's tool), both over
//! the in-crate user-questions seam.
//!
//! M1 rulings (the Q16d batch):
//! - the tools register unconditionally (Q16d-1A), so the baked
//!   registration mirrors the dsh `mode: 'legacy'` default schema (the
//!   dsh base adds no tool-ask-user row, and the headless bundle adds
//!   no override — the baked-default judgment);
//! - the timed variant body stays pure and test-pinned (the Q16d-3A
//!   ask+askTimed ruling); its registration variant composes only with
//!   a `mode: timed` config the M1 bundle never carries;
//! - the `ASK_CANCELLED` dismissal arm is a pure branch on the error
//!   code a client answerer produces; its dsh `exec.concludeTurn` flag
//!   rides the M1-absent answerer face and stays documented;
//! - the projection reads ride the goal-tools seam shape.

#![forbid(unsafe_code)]

use serde_json::{Value, json};

use crate::plan_mode;
use crate::tools::BodyError;
use crate::user_questions::{
    AnswerItem, AskRequest, AskSeams, Question, QuestionOption, TimedAskResult, TimedWaits,
    UserQuestionError,
};

/// The instruction the timed pending result carries in its `message`
/// field (the dsh `pendingNotice` constant, verbatim).
pub const TIMED_PENDING_NOTICE: &str = "No answer batch arrived before the timeout. This is pending, not a skipped answer. Continue useful independent work. The user can still answer; their reply will be a user message identified as answer_to_pending_question with this callId and the original questions. Do not treat this as permission.";

/// The baked foreground wait (dsh `Config.timeout` default; the
/// headless bundle adds no tool-ask-user config row).
pub const BAKED_TIMEOUT_SECONDS: i64 = 120;

/// The approved-plan render text (catalog-verbatim).
pub const EXIT_RENDER_APPROVED: &str =
    "Plan approved — plan mode exited; carry out the plan starting with your next step.";

/// The dismissed-plan render text (catalog-verbatim).
pub const EXIT_RENDER_DISMISSED: &str =
    "The user dismissed the plan review to reply in their own words; plan mode remains active.";

/// The keep-planning refusal without feedback.
pub const KEEP_PLANNING_TEXT: &str =
    "The user chose to keep planning; revise the plan and present it again.";

/// The outbound seams the interaction bodies need (the goal-service
/// judgment: the wire seam type implements this beside `GoalSeams`).
pub trait InteractionSeam: Send + Sync {
    /// Reads one projection's relayed state for one session.
    ///
    /// # Errors
    ///
    /// The relay transport failure.
    fn projection_state(&self, session: &str, key: &str) -> Result<Option<Value>, String>;
}

/// The ask face (the in-crate user-questions service behind the seam
/// flags; the boxed alias keeps the wire types named).
pub type AskFace =
    Box<dyn Fn(&AskRequest, &AskSeams) -> Result<Value, UserQuestionError> + Send + Sync>;

/// The composed interaction runtime: the timed-wait registry, the plan
/// projection read, the ask face, and the reload fact.
pub struct InteractionState {
    /// The foreground-wait registry (dsh `waits`).
    pub questions: TimedWaits,
    /// The plan projection read (the `plan` key).
    pub plans: Box<dyn InteractionSeam>,
    /// The ask face (the in-crate user-questions service; the seam
    /// carries the answerer so tests pin the success arms).
    pub ask: AskFace,
    /// The dsh `disposed` reload fact; false across the M1 single
    /// composition.
    pub disposed: bool,
    /// The userQuestions seam presence (dsh
    /// `ctx.get('userQuestions')`); true across M1, the in-crate
    /// service always composes.
    pub user_questions_present: bool,
}

impl std::fmt::Debug for InteractionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InteractionState")
            .field("disposed", &self.disposed)
            .field("user_questions_present", &self.user_questions_present)
            .finish_non_exhaustive()
    }
}

impl InteractionState {
    /// The production ask face: the in-crate service with the dsh
    /// headless seams (no answerer, single live root).
    pub fn production_ask() -> AskFace {
        Box::new(crate::user_questions::ask)
    }
}

/// Maps a service error onto the wire body shape (the dsh throw: the
/// `UserQuestionError` name carries).
fn ask_body_error(error: UserQuestionError) -> BodyError {
    BodyError {
        code: Some(error.code.to_owned()),
        name: "UserQuestionError".to_owned(),
        message: error.message,
    }
}

/// Reads the model question batch out of the tool arguments (the dsh
/// legacy/timed mapping: the wire `multi_select` key folds into the
/// service `multiSelect`).
///
/// # Errors
///
/// `invalid questions` when the batch is unreadable.
pub fn questions_from_args(args: &Value) -> Result<Vec<Question>, BodyError> {
    let Some(items) = args.get("questions").and_then(Value::as_array) else {
        return Err(BodyError::plain(
            "invalid questions: the questions array is required",
        ));
    };
    let mut questions = Vec::with_capacity(items.len());
    for item in items {
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            return Err(BodyError::plain(
                "invalid questions: every question requires a string id",
            ));
        };
        let Some(question) = item.get("question").and_then(Value::as_str) else {
            return Err(BodyError::plain(
                "invalid questions: every question requires a string question",
            ));
        };
        let options = item.get("options").and_then(Value::as_array).map(|rows| {
            rows.iter()
                .map(|row| QuestionOption {
                    label: row
                        .get("label")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    description: row
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
                .collect::<Vec<_>>()
        });
        questions.push(Question {
            id: id.to_owned(),
            question: question.to_owned(),
            detail: None,
            header: item
                .get("header")
                .and_then(Value::as_str)
                .map(str::to_owned),
            options,
            multi_select: item.get("multi_select").and_then(Value::as_bool),
            intent: None,
        });
    }
    Ok(questions)
}

/// The legacy `ask_user_question` execute (dsh `tool-ask-user` apply's
/// blocking body): the mapped request asks the seam, and `None`
/// composes the dsh headless `NO_PROVIDER` fact.
///
/// # Errors
///
/// The unreadable batch or the ask taxonomy.
pub fn execute_ask(
    args: &Value,
    agent: Option<&str>,
    state: &InteractionState,
) -> Result<Value, BodyError> {
    let questions = questions_from_args(args)?;
    let request = AskRequest {
        questions,
        agent: agent.map(str::to_owned),
        aborted: false,
    };
    let seams = AskSeams::default();
    (state.ask)(&request, &seams).map_err(ask_body_error)
}

/// Validates the timed variant's timeout (dsh `validateTimeout`).
///
/// # Errors
///
/// The dsh text, verbatim.
pub fn validate_timed_timeout(timeout: i64) -> Result<i64, BodyError> {
    if timeout != -1 && !(1..=2_147_483).contains(&timeout) {
        return Err(BodyError::plain(
            "timeout must be -1 or a positive integer up to 2147483 seconds",
        ));
    }
    Ok(timeout)
}

/// Validates the timed variant's question ids (dsh
/// `validateQuestionIds`).
///
/// # Errors
///
/// The dsh text, verbatim.
pub fn validate_question_ids(questions: &[Question]) -> Result<(), BodyError> {
    let mut seen = std::collections::HashSet::new();
    for question in questions {
        if !seen.insert(question.id.clone()) {
            return Err(BodyError::plain(format!(
                "question id {} must be unique within this call",
                json!(question.id)
            )));
        }
    }
    Ok(())
}

/// The timed variant body (dsh `registerTimedAskUser` execute).
///
/// The timeout and id validations run first, an indefinite wait keys
/// the ask by the call id, and a timed wait maps its expiry to the
/// pending result.
///
/// # Errors
///
/// The validation texts and the ask taxonomy.
pub fn execute_timed_ask(
    args: &Value,
    agent: Option<&str>,
    call_id: &str,
    state: &InteractionState,
) -> Result<Value, BodyError> {
    let timeout = validate_timed_timeout(
        args.get("timeout")
            .and_then(Value::as_i64)
            .unwrap_or(BAKED_TIMEOUT_SECONDS),
    )?;
    let questions = questions_from_args(args)?;
    validate_question_ids(&questions)?;
    let Some(agent) = agent else {
        return Err(BodyError::plain("timed questions require a live agent"));
    };
    let request = AskRequest {
        questions,
        agent: Some(agent.to_owned()),
        aborted: false,
    };
    let seams = AskSeams::default();
    if timeout != -1 {
        let result = crate::user_questions::ask_timed(
            &request,
            call_id,
            timeout
                .checked_mul(1_000)
                .and_then(|ms| u64::try_from(ms).ok())
                .ok_or_else(|| {
                    BodyError::plain(
                        "timeout must be -1 or a positive integer up to 2147483 seconds",
                    )
                })?,
            &state.questions,
            &seams,
        )
        .map_err(ask_body_error)?;
        return Ok(match result {
            TimedAskResult::Pending { call_id } => json!({
                "pending": true,
                "callId": call_id,
                "message": TIMED_PENDING_NOTICE,
            }),
            TimedAskResult::Answer(value) => value,
        });
    }
    // The indefinite keyed form still asks the plain face (the dsh
    // `wait` without `timed`; the M1 no-answerer fact applies).
    (state.ask)(&request, &seams).map_err(ask_body_error)
}

/// The `ask_user_question` output render (dsh `output.render`): the
/// value JSON stringifies into one text block.
#[must_use]
pub fn render_ask(_args: &Value, value: &Value) -> Value {
    json!([{ "type": "text", "text": value.to_string() }])
}

/// The `exit_plan_mode` execute (dsh plan-mode `execute`): the caller
/// fence, the logged plan gate, the heading gate, and the review flow
/// run in the dsh order.
///
/// # Errors
///
/// The dsh refusal texts, verbatim.
#[expect(
    clippy::too_many_lines,
    reason = "the body mirrors the dsh execute one arm per gate; splitting it would hide the refusal order"
)]
pub fn execute_exit(
    args: &Value,
    agent: Option<&str>,
    state: &InteractionState,
) -> Result<Value, BodyError> {
    let Some(agent) = agent else {
        return Err(BodyError::plain(format!(
            "{} requires a calling agent (no session to switch)",
            plan_mode::EXIT_TOOL
        )));
    };
    let plan_state = state
        .plans
        .projection_state(agent, plan_mode::PROJECTION_KEY)
        .map_err(BodyError::plain)?;
    let Some(plan_state) = plan_state else {
        return Err(BodyError::plain(
            "plan-mode requires the plan session projection",
        ));
    };
    if !plan_mode::logged_active(Some(&plan_state)) {
        return Err(BodyError::plain(format!(
            "{} is only available in plan mode",
            plan_mode::EXIT_TOOL
        )));
    }
    let plan = args.get("plan").and_then(Value::as_str).unwrap_or("");
    if !plan_mode::has_heading(plan) {
        return Err(BodyError::plain(format!(
            "{} requires a non-empty markdown plan starting with a # heading",
            plan_mode::EXIT_TOOL
        )));
    }
    if !state.user_questions_present {
        return Err(BodyError::plain(
            "no user-questions channel is available to review the plan; ask the user to switch the session mode instead",
        ));
    }
    let request = AskRequest {
        questions: vec![Question {
            id: plan_mode::REVIEW_ID.to_owned(),
            question: "Approve this plan and leave plan mode?".to_owned(),
            detail: Some(plan.to_owned()),
            header: Some("Plan review".to_owned()),
            options: Some(vec![
                QuestionOption {
                    label: plan_mode::APPROVE_LABEL.to_owned(),
                    description: Some(
                        "Leave plan mode; the plan is carried out from the next step.".to_owned(),
                    ),
                },
                QuestionOption {
                    label: plan_mode::KEEP_PLANNING_LABEL.to_owned(),
                    description: Some(
                        "Stay in plan mode; feedback goes back to the model.".to_owned(),
                    ),
                },
            ]),
            multi_select: None,
            intent: Some(crate::user_questions::QuestionIntent {
                kind: "plan-review".to_owned(),
                approve: plan_mode::APPROVE_LABEL.to_owned(),
                call_id: None,
            }),
        }],
        agent: Some(agent.to_owned()),
        aborted: false,
    };
    let seams = AskSeams::default();
    let answer = match (state.ask)(&request, &seams) {
        Ok(answer) => answer,
        Err(error) if error.code == plan_mode::ASK_CANCELLED => {
            // The dsh ends the turn here (`exec.concludeTurn` + the
            // unapproved result); the conclude flag rides the M1-absent
            // answerer face and stays documented.
            return Ok(json!({ "approved": false }));
        }
        Err(error) => return Err(ask_body_error(error)),
    };
    if state.disposed {
        return Err(BodyError::plain(
            "the plan-mode service was reloaded while the plan was under review; present the plan again",
        ));
    }
    let items: Vec<AnswerItem> = answer
        .get("answers")
        .cloned()
        .map(serde_json::from_value::<Vec<AnswerItem>>)
        .transpose()
        .map_err(|error| BodyError::plain(format!("the review answer is unreadable: {error}")))?
        .unwrap_or_default();
    let review: Vec<&AnswerItem> = items
        .iter()
        .filter(|entry| entry.id == plan_mode::REVIEW_ID)
        .collect();
    let item = (review.len() == 1).then(|| review[0]);
    let approves = item.is_some_and(|entry| {
        entry.selected.len() == 1
            && entry.selected[0] == plan_mode::APPROVE_LABEL
            && entry.custom.is_none()
    });
    if !approves {
        let feedback = item
            .and_then(|entry| entry.custom.clone())
            .unwrap_or_default();
        return Err(BodyError::plain(if feedback.is_empty() {
            KEEP_PLANNING_TEXT.to_owned()
        } else {
            format!("The user chose to keep planning; their feedback: {feedback}")
        }));
    }
    Ok(json!({ "approved": true }))
}

/// The `exit_plan_mode` output render (dsh `output.render`).
#[must_use]
pub fn render_exit(_args: &Value, value: &Value) -> Value {
    let approved = value
        .get("approved")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    json!([{
        "type": "text",
        "text": if approved { EXIT_RENDER_APPROVED } else { EXIT_RENDER_DISMISSED },
    }])
}

/// The `exit_plan_mode` call card (dsh `presentCall`).
#[must_use]
pub fn present_call_exit(args: &Value) -> Value {
    let plan = args.get("plan").and_then(Value::as_str).unwrap_or("");
    let title = plan_mode::first_heading(plan).unwrap_or_else(|| "Plan".to_owned());
    json!({
        "card": "generic",
        "title": title,
        "kind": "other",
        "content": [{ "type": "text", "text": plan }],
    })
}

/// The `exit_plan_mode` result card (dsh `presentResult`).
#[must_use]
pub fn present_result_exit(_args: &Value, result: &Value) -> Option<Value> {
    let content = result.get("content").cloned()?;
    Some(json!({
        "card": "generic",
        "title": "Plan review",
        "content": content,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A seam rig over a scripted projection and a scripted answerer.
    struct Rig {
        plan_state: Value,
        answer: std::sync::Mutex<Result<Value, UserQuestionError>>,
    }

    impl InteractionSeam for Rig {
        fn projection_state(&self, _session: &str, _key: &str) -> Result<Option<Value>, String> {
            Ok(Some(self.plan_state.clone()))
        }
    }

    fn state_with(plan: Value, answer: Result<Value, UserQuestionError>) -> InteractionState {
        let owned = std::sync::Arc::new(Rig {
            plan_state: plan,
            answer: std::sync::Mutex::new(answer),
        });
        let ask_owned = std::sync::Arc::clone(&owned);
        InteractionState {
            questions: TimedWaits::default(),
            plans: Box::new(OwnedRig(owned)),
            ask: Box::new(move |_request, _seams| {
                ask_owned.answer.lock().expect("answer locks").clone()
            }),
            disposed: false,
            user_questions_present: true,
        }
    }

    /// An owned seam wrapper (the rig outlives the boxed trait object).
    struct OwnedRig(std::sync::Arc<Rig>);

    impl InteractionSeam for OwnedRig {
        fn projection_state(&self, session: &str, key: &str) -> Result<Option<Value>, String> {
            self.0.projection_state(session, key)
        }
    }

    fn exit_args(plan: &str) -> Value {
        json!({ "plan": plan })
    }

    #[test]
    fn ask_body_maps_and_refuses_without_answerer() {
        let state = state_with(
            json!({"active": false}),
            Err(UserQuestionError::no_provider()),
        );
        let args = json!({ "questions": [
            { "id": "q1", "question": "Pick", "header": "H",
              "options": [{ "label": "A", "description": "d" }], "multi_select": true }
        ] });
        let error = execute_ask(&args, Some("s1"), &state).unwrap_err();
        assert_eq!(error.code.as_deref(), Some("NO_PROVIDER"));
        assert_eq!(
            error.message,
            "no user-questions answerer accepted the request"
        );
        // The unreadable batch refuses first.
        let broken = json!({ "questions": [{ "question": "no id" }] });
        assert_eq!(
            execute_ask(&broken, Some("s1"), &state)
                .unwrap_err()
                .message,
            "invalid questions: every question requires a string id"
        );
    }

    #[test]
    fn timed_variant_pins_the_validation_arms() {
        let state = state_with(
            json!({"active": false}),
            Err(UserQuestionError::no_provider()),
        );
        assert_eq!(
            validate_timed_timeout(0).unwrap_err().message,
            "timeout must be -1 or a positive integer up to 2147483 seconds"
        );
        assert_eq!(
            validate_timed_timeout(-1).expect("the -1 arm validates"),
            -1
        );
        assert_eq!(
            validate_timed_timeout(2_147_483).expect("the cap validates"),
            2_147_483
        );
        assert_eq!(
            validate_question_ids(&[
                Question {
                    id: "a".to_owned(),
                    question: "x".to_owned(),
                    detail: None,
                    header: None,
                    options: None,
                    multi_select: None,
                    intent: None
                },
                Question {
                    id: "a".to_owned(),
                    question: "y".to_owned(),
                    detail: None,
                    header: None,
                    options: None,
                    multi_select: None,
                    intent: None
                },
            ])
            .unwrap_err()
            .message,
            "question id \"a\" must be unique within this call"
        );
        // A timed call without an agent refuses before the wait starts.
        let single = json!({ "questions": [{ "id": "q1", "question": "Pick" }], "timeout": 1 });
        assert_eq!(
            execute_timed_ask(&single, None, "call-1", &state)
                .unwrap_err()
                .message,
            "timed questions require a live agent"
        );
    }

    #[test]
    fn exit_body_pins_the_refusal_order() {
        let state = state_with(
            json!({"active": false}),
            Err(UserQuestionError::no_provider()),
        );
        // No agent first.
        let error = execute_exit(&exit_args("# Plan"), None, &state).unwrap_err();
        assert_eq!(
            error.message,
            "exit_plan_mode requires a calling agent (no session to switch)"
        );
        // Plan mode inactive next (the M1 structural gate: no writer).
        let error = execute_exit(&exit_args("# Plan"), Some("s1"), &state).unwrap_err();
        assert_eq!(
            error.message,
            "exit_plan_mode is only available in plan mode"
        );
        // With plan active, the heading gate runs.
        let active = state_with(
            json!({"active": true}),
            Err(UserQuestionError::no_provider()),
        );
        let error = execute_exit(&exit_args("just text"), Some("s1"), &active).unwrap_err();
        assert_eq!(
            error.message,
            "exit_plan_mode requires a non-empty markdown plan starting with a # heading"
        );
        // The channel-absence arm stays pinnable through the flag.
        let mut channel_off = state_with(
            json!({"active": true}),
            Err(UserQuestionError::no_provider()),
        );
        channel_off.user_questions_present = false;
        let error = execute_exit(&exit_args("# Plan"), Some("s1"), &channel_off).unwrap_err();
        assert_eq!(
            error.message,
            "no user-questions channel is available to review the plan; ask the user to switch the session mode instead"
        );
    }

    #[test]
    fn exit_review_pins_the_keep_planning_and_reload_arms() {
        // The review question carries the plan as detail and the intent.
        let rejecting = UserQuestionError {
            message: "rejected".to_owned(),
            code: "ASK_CANCELLED",
        };
        let cancelled = state_with(json!({"active": true}), Err(rejecting));
        let value = execute_exit(&exit_args("# The plan"), Some("s1"), &cancelled)
            .expect("the dismissal returns unapproved");
        assert_eq!(value, json!({ "approved": false }));

        // A keep-planning answer with no feedback.
        let state = state_with(
            json!({"active": true}),
            Ok(json!({ "answers": [{ "id": "plan-review", "selected": [] }] })),
        );
        assert_eq!(
            execute_exit(&exit_args("# The plan"), Some("s1"), &state)
                .unwrap_err()
                .message,
            KEEP_PLANNING_TEXT
        );
        // Feedback rides the message.
        let state = state_with(
            json!({"active": true}),
            Ok(json!({ "answers": [
                { "id": "plan-review", "selected": ["Keep planning"], "custom": "add tests" }
            ] })),
        );
        assert_eq!(
            execute_exit(&exit_args("# The plan"), Some("s1"), &state)
                .unwrap_err()
                .message,
            "The user chose to keep planning; their feedback: add tests"
        );
        // Approval needs exactly the Approve label with no custom.
        let state = state_with(
            json!({"active": true}),
            Ok(json!({ "answers": [{ "id": "plan-review", "selected": ["Approve"] }] })),
        );
        assert_eq!(
            execute_exit(&exit_args("# The plan"), Some("s1"), &state).expect("approved"),
            json!({ "approved": true })
        );
        // The reload arm stays pinnable.
        let mut disposed = state_with(
            json!({"active": true}),
            Ok(json!({ "answers": [{ "id": "plan-review", "selected": ["Approve"] }] })),
        );
        disposed.disposed = true;
        assert_eq!(
            execute_exit(&exit_args("# The plan"), Some("s1"), &disposed)
                .unwrap_err()
                .message,
            "the plan-mode service was reloaded while the plan was under review; present the plan again"
        );
    }

    #[test]
    fn renders_and_cards_pin_the_shapes() {
        let ask_value = json!({ "answers": [{ "id": "q", "selected": ["A"] }] });
        let ask_render = render_ask(&json!({}), &ask_value);
        assert_eq!(
            ask_render,
            json!([{ "type": "text", "text": ask_value.to_string() }])
        );
        assert_eq!(
            render_exit(&json!({}), &json!({ "approved": true })),
            json!([{ "type": "text", "text": EXIT_RENDER_APPROVED }])
        );
        assert_eq!(
            render_exit(&json!({}), &json!({ "approved": false })),
            json!([{ "type": "text", "text": EXIT_RENDER_DISMISSED }])
        );
        let card = present_call_exit(&exit_args("# Ship the thing\ndetails"));
        assert_eq!(card["title"], json!("Ship the thing"));
        assert_eq!(card["kind"], json!("other"));
        assert_eq!(card["card"], json!("generic"));
        let no_heading = present_call_exit(&exit_args("plain"));
        assert_eq!(no_heading["title"], json!("Plan"));
        let result_card = present_result_exit(
            &json!({}),
            &json!({ "content": [{ "type": "text", "text": "x" }] }),
        )
        .expect("the card renders");
        assert_eq!(result_card["title"], json!("Plan review"));
        assert!(present_result_exit(&json!({}), &json!({})).is_none());
    }
}
