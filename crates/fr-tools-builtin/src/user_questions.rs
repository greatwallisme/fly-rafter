// Rust guideline compliant 2026-10-31
//! The user-questions domain (dsh `@deepseek-ai/dsh-user-questions`).
//!
//! The question/answer vocabulary, the service validation chains, the
//! timed foreground wait, and the `userQuestions` session projection
//! fold.
//!
//! M1 rulings (Q16d-3A and the batch-0 archival family):
//! - no answerer is composed in M1, so [`ask`] ends at `NO_PROVIDER`
//!   after the validation chains — the dsh headless fact (the answerer
//!   waterfall composes in the client bundles); the answerer rides the
//!   seams so synthetic inputs pin the success arm;
//! - `answer` and `attachWait` need the client remote face and the
//!   `agent/inbox/discarded` event emission and stay absent (the Q16d
//!   archival ruling: arm absence documented, not dead-coded);
//! - the agents-registry checks (exact live identity, root membership)
//!   bake to the single-root headless fact like the goal tools'
//!   authority family — the pure predicates ride the seam flags so
//!   synthetic inputs pin the false arms.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The model-facing tool whose calls this projection tracks.
pub const ASK_USER_QUESTION_TOOL: &str = "ask_user_question";

/// The model-facing parameter only the timed schema declares; its
/// presence in a logged request header tells the fold that the
/// `ask_user_question`-schema calls that follow can continue past a
/// timeout.
pub const TIMED_WAIT_PARAMETER: &str = "timeout";

/// The user-message source kind a late reply carries (the dsh
/// `MessageSourceMap` merge extension).
pub const REPLY_SOURCE_KIND: &str = "user-question-reply";

/// The resume-repair synthetic result code the fold reads as
/// `continued` (dsh `TOOL_OUTCOME_UNKNOWN`).
pub const TOOL_OUTCOME_UNKNOWN: &str = "TOOL_OUTCOME_UNKNOWN";

/// The projected question states (dsh `UserQuestionState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuestionState {
    /// The call may still return the answer.
    #[serde(rename = "open")]
    Open,
    /// The answer can only arrive as a new turn.
    #[serde(rename = "continued")]
    Continued,
}

/// One selectable answer offered to the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    /// User-facing label.
    pub label: String,
    /// Optional extra context rendered by capable UIs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The caller-declared presentation intent: a plan submitted for review
/// (the dsh `AskUserQuestionIntent` tagged shape; only the
/// `plan-review` kind exists).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionIntent {
    /// The intent kind; `plan-review` is the only current member.
    pub kind: String,
    /// The option label that approves the plan; every other declines.
    pub approve: String,
    /// Logged tool invocation whose arguments contain the plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
}

/// One question in a user-questions request (the service vocabulary;
/// the wire `multiSelect` key).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    /// Stable caller-provided question id, echoed in the answer.
    pub id: String,
    /// The question to display.
    pub question: String,
    /// Optional supporting detail rendered with the question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Optional short heading or group label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// Optional choices the UI can render as a menu.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<QuestionOption>>,
    /// Whether more than one option may be selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_select: Option<bool>,
    /// Optional presentation intent for capable UIs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<QuestionIntent>,
}

/// Answer to one question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerItem {
    /// The answered question id.
    pub id: String,
    /// Selected option labels; empty with no custom is an explicit skip.
    pub selected: Vec<String>,
    /// Optional free-text answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<String>,
}

/// One unanswered timed call reconstructed from the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingUserQuestion {
    /// The answered call identity.
    pub call_id: String,
    /// The questions the call carried.
    pub questions: Vec<Question>,
    /// The answerable state.
    pub state: QuestionState,
}

/// One timed call and the answers it settled with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettledUserQuestion {
    /// The settled call.
    pub call_id: String,
    /// The recorded batch; empty when a late reply carried none.
    pub answers: Vec<AnswerItem>,
}

/// The projection's wire value: answerable calls in ask order, settled
/// ones in settlement order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionView {
    /// Calls that can still take an answer, in ask order.
    pub active: Vec<PendingUserQuestion>,
    /// Calls an answer settled, in settlement order.
    pub settled: Vec<SettledUserQuestion>,
}

/// The projection's host state (dsh `UserQuestionFold`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoldState {
    /// Whether the in-effect request header declares the timed schema.
    pub timed: bool,
    /// The question view that schema's calls build.
    pub questions: ProjectionView,
}

/// The stable error taxonomy (dsh `UserQuestionError`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct UserQuestionError {
    /// Raw user-facing message.
    pub message: String,
    /// Stable machine-routing code.
    pub code: &'static str,
}

impl UserQuestionError {
    /// `ASK_ABORTED`: the ask was aborted before the user answered.
    #[must_use]
    pub fn aborted() -> Self {
        Self {
            message: "ask_user_question was aborted before the user answered".to_owned(),
            code: "ASK_ABORTED",
        }
    }

    /// `EMPTY_QUESTIONS`: the batch carried no question.
    #[must_use]
    pub fn empty_questions() -> Self {
        Self {
            message: "ask_user_question requires at least one question".to_owned(),
            code: "EMPTY_QUESTIONS",
        }
    }

    /// `BAD_INTENT`: the approve label names none of the question's own
    /// options (the dsh `JSON.stringify` quoting).
    #[must_use]
    pub fn intent_label(id: &str, kind: &str, approve: &str) -> Self {
        Self {
            message: format!(
                "question {id} declares intent {kind} whose approve label {} names none of its options",
                json!(approve)
            ),
            code: "BAD_INTENT",
        }
    }

    /// `BAD_INTENT`: the intent carries no detail it reviews.
    #[must_use]
    pub fn intent_detail(id: &str, kind: &str) -> Self {
        Self {
            message: format!("question {id} declares intent {kind} without the detail it reviews"),
            code: "BAD_INTENT",
        }
    }

    /// `CALLER_NOT_LIVE`: the supplied agent is not the exact live
    /// calling agent.
    #[must_use]
    pub fn caller_not_live() -> Self {
        Self {
            message:
                "human interaction requires the exact live calling agent when an agent is supplied"
                    .to_owned(),
            code: "CALLER_NOT_LIVE",
        }
    }

    /// `DELEGATED_CALLER`: the calling agent is owned by another live
    /// agent.
    #[must_use]
    pub fn delegated_caller() -> Self {
        Self {
            message: "human interaction is unavailable while the calling agent is owned by another live agent; include the unresolved question or decision in the child agent's final result".to_owned(),
            code: "DELEGATED_CALLER",
        }
    }

    /// `NO_PROVIDER`: no answerer accepted the request.
    #[must_use]
    pub fn no_provider() -> Self {
        Self {
            message: "no user-questions answerer accepted the request".to_owned(),
            code: "NO_PROVIDER",
        }
    }

    /// `ASK_TIMED_OUT`: the foreground wait expired unattended.
    #[must_use]
    pub fn ask_timed_out() -> Self {
        Self {
            message: "ask_user_question timed out before the user answered".to_owned(),
            code: "ASK_TIMED_OUT",
        }
    }

    /// `BAD_TIMEOUT`: the wait does not fit a positive platform timer.
    #[must_use]
    pub fn bad_timeout() -> Self {
        Self {
            message: "timeout must fit a positive platform timer".to_owned(),
            code: "BAD_TIMEOUT",
        }
    }

    /// `DUPLICATE_WAIT`: the call already holds a foreground wait.
    #[must_use]
    pub fn duplicate_wait() -> Self {
        Self {
            message: "the question call already has a foreground wait".to_owned(),
            code: "DUPLICATE_WAIT",
        }
    }

    /// `BAD_ANSWER`: the batch does not name each question exactly once.
    #[must_use]
    pub fn bad_answer(call_id: &str, count: usize) -> Self {
        Self {
            message: format!(
                "the answer batch for {call_id} must name each of its {count} questions exactly once"
            ),
            code: "BAD_ANSWER",
        }
    }
}

/// One ask request (the service vocabulary; the agent is the calling
/// session id, the dsh `exec.agent`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AskRequest {
    /// The questions to display.
    pub questions: Vec<Question>,
    /// The live calling session id, when supplied.
    pub agent: Option<String>,
    /// The cancellation snapshot (the dsh `signal.aborted` entry check).
    pub aborted: bool,
}

/// The composed answerer face (the dsh waterfall; the alias keeps the
/// seam type named).
pub type Answerer<'a> = &'a dyn Fn(&[Question]) -> Result<Value, UserQuestionError>;

/// The ask seams: the composed answerer and the agents-registry facts
/// (the goal-tools authority-family judgment: the pure predicates ride
/// the flags so synthetic inputs pin the false arms).
pub struct AskSeams<'a> {
    /// The composed answerer (the dsh `user-questions/request`
    /// waterfall); `None` composes the dsh headless fact.
    pub answerer: Option<Answerer<'a>>,
    /// The exact-live registry fact (dsh `agents.get(id) === agent`).
    pub caller_live: bool,
    /// The root-membership fact (dsh `roots().includes(agent)`).
    pub caller_root: bool,
}

impl std::fmt::Debug for AskSeams<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AskSeams")
            .field("answerer", &self.answerer.is_some())
            .field("caller_live", &self.caller_live)
            .field("caller_root", &self.caller_root)
            .finish()
    }
}

impl Default for AskSeams<'_> {
    fn default() -> Self {
        Self {
            answerer: None,
            caller_live: true,
            caller_root: true,
        }
    }
}

/// Fences the caller identity (dsh `assertLiveRoot`).
///
/// # Errors
///
/// `CALLER_NOT_LIVE` on a non-live caller, `DELEGATED_CALLER` on a
/// caller owned by another agent.
pub fn assert_live_root(seams: &AskSeams) -> Result<(), UserQuestionError> {
    if !seams.caller_live {
        return Err(UserQuestionError::caller_not_live());
    }
    if !seams.caller_root {
        return Err(UserQuestionError::delegated_caller());
    }
    Ok(())
}

/// Validates one question's presentation intent against its own shape
/// (dsh `ask` intent loop).
///
/// # Errors
///
/// The two `BAD_INTENT` texts, verbatim.
pub fn validate_intent(question: &Question) -> Result<(), UserQuestionError> {
    let Some(intent) = question.intent.as_ref() else {
        return Ok(());
    };
    let matches = question
        .options
        .as_ref()
        .is_some_and(|options| options.iter().any(|option| option.label == intent.approve));
    if !matches {
        return Err(UserQuestionError::intent_label(
            &question.id,
            &intent.kind,
            &intent.approve,
        ));
    }
    if question.detail.is_none() {
        return Err(UserQuestionError::intent_detail(&question.id, &intent.kind));
    }
    Ok(())
}

/// Ask the answerer waterfall and wait for the human's answer (dsh
/// `UserQuestionService.ask`).
///
/// The validation chains run in the dsh order, then the composed
/// answerer claims the request, and `None` composes the dsh headless
/// `NO_PROVIDER` fact.
///
/// # Errors
///
/// The taxonomy above, in the dsh order.
pub fn ask(request: &AskRequest, seams: &AskSeams) -> Result<Value, UserQuestionError> {
    if request.aborted {
        return Err(UserQuestionError::aborted());
    }
    if request.questions.is_empty() {
        return Err(UserQuestionError::empty_questions());
    }
    if request.agent.is_some() {
        assert_live_root(seams)?;
    }
    for question in &request.questions {
        validate_intent(question)?;
    }
    seams.answerer.map_or_else(
        || Err(UserQuestionError::no_provider()),
        |answerer| answerer(&request.questions),
    )
}

/// The real epoch-millisecond clock (the dsh `Date.now()` carrier).
#[must_use]
pub fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(0))
}

/// The shared wait state inside one [`TimedQuestionWait`].
#[derive(Debug)]
struct WaitState {
    /// Whether the wait closed (the dsh `signal.aborted` fact).
    closed: bool,
    /// The close reason; always present once closed.
    reason: Option<UserQuestionError>,
    /// Live Client claims (the dsh `claims` set size; zero in M1).
    claims: usize,
}

/// One live foreground wait (the dsh `TimedQuestionWait` port): the
/// Host-clock deadline closes it with the timeout error while no Client
/// claim holds it, and an explicit close delivers its own reason.
///
/// The Q16d-3A archival ruling keeps the claim arm shape (the dsh
/// `attach`/`release` counter) — no Client claim attaches in M1, so the
/// deadline always governs.
pub struct TimedQuestionWait {
    deadline_ms: u64,
    timeout_error: UserQuestionError,
    state: Mutex<WaitState>,
    signal: Condvar,
}

impl std::fmt::Debug for TimedQuestionWait {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimedQuestionWait")
            .field("deadline_ms", &self.deadline_ms)
            .finish_non_exhaustive()
    }
}

impl TimedQuestionWait {
    /// Builds one wait over a host-clock deadline; an already-aborted
    /// parent closes it immediately with the parent's reason (the dsh
    /// constructor's parent-aborted arm).
    #[must_use]
    pub fn new(
        deadline_ms: u64,
        parent_aborted: bool,
        parent_reason: Option<UserQuestionError>,
    ) -> Arc<Self> {
        let wait = Arc::new(Self {
            deadline_ms,
            timeout_error: UserQuestionError::ask_timed_out(),
            state: Mutex::new(WaitState {
                closed: false,
                reason: None,
                claims: 0,
            }),
            signal: Condvar::new(),
        });
        if parent_aborted {
            wait.close(parent_reason.unwrap_or_else(UserQuestionError::aborted));
        }
        wait
    }

    /// Whether the wait closed (the dsh `signal.aborted` fact).
    ///
    /// # Panics
    ///
    /// Panics when the wait state mutex is poisoned.
    #[must_use]
    pub fn aborted(&self) -> bool {
        self.state.lock().expect("wait state locks").closed
    }

    /// The close reason, when closed.
    ///
    /// # Panics
    ///
    /// Panics when the wait state mutex is poisoned.
    #[must_use]
    pub fn reason(&self) -> Option<UserQuestionError> {
        self.state.lock().expect("wait state locks").reason.clone()
    }

    /// Closes the wait with `reason` and releases every waiter (the dsh
    /// `close`).
    ///
    /// # Panics
    ///
    /// Panics when the wait state mutex is poisoned.
    pub fn close(&self, reason: UserQuestionError) {
        let mut state = self.state.lock().expect("wait state locks");
        if state.closed {
            return;
        }
        state.closed = true;
        state.reason = Some(reason);
        drop(state);
        self.signal.notify_all();
    }

    /// Settles when the wait is cancelled, expires, or is disposed (the
    /// dsh `done`).
    ///
    /// Blocks until an explicit close, or expires the deadline with the
    /// timeout error while no claim holds the wait. Every iteration
    /// releases the guard before the next lock, so a waiter never holds
    /// the state across re-locks (the fr-shell jobs-wait lesson).
    ///
    /// # Errors
    ///
    /// The close reason, always.
    ///
    /// # Panics
    ///
    /// Panics when the wait state mutex is poisoned.
    pub fn done(&self) -> Result<(), UserQuestionError> {
        loop {
            let mut state = self.state.lock().expect("wait state locks");
            if state.closed {
                let reason = state.reason.clone().expect("closed carries a reason");
                drop(state);
                return Err(reason);
            }
            let now = epoch_ms();
            if state.claims == 0 && now >= self.deadline_ms {
                state.closed = true;
                state.reason = Some(self.timeout_error.clone());
                drop(state);
                self.signal.notify_all();
                return Err(self.timeout_error.clone());
            }
            if state.claims > 0 {
                // A claim holds the wait: the timer is suspended (the
                // dsh `schedule` rule) and the release arm re-arms it.
                state = self.signal.wait(state).expect("wait state locks");
            } else {
                let remaining = self.deadline_ms.saturating_sub(now);
                let (guard, _timed_out) = self
                    .signal
                    .wait_timeout(state, std::time::Duration::from_millis(remaining))
                    .expect("wait state locks");
                // The loop head re-checks the deadline and the close
                // state with a fresh lock.
                state = guard;
            }
            drop(state);
        }
    }

    /// Holds the wait for one answer UI (the dsh `attach` claim).
    ///
    /// # Panics
    ///
    /// Panics when the wait state mutex is poisoned.
    pub fn attach(&self) {
        let mut state = self.state.lock().expect("wait state locks");
        state.claims += 1;
    }

    /// Releases one claim (the dsh `release`).
    ///
    /// # Panics
    ///
    /// Panics when the wait state mutex is poisoned.
    pub fn release(&self) {
        let mut state = self.state.lock().expect("wait state locks");
        state.claims = state.claims.saturating_sub(1);
        drop(state);
        self.signal.notify_all();
    }
}

/// The foreground-wait registry (dsh `waits`: per agent, per call).
#[derive(Debug, Default)]
pub struct TimedWaits {
    waits: Mutex<HashMap<(String, String), Arc<TimedQuestionWait>>>,
}

impl TimedWaits {
    /// Registers one wait, refusing a duplicate (the dsh
    /// `DUPLICATE_WAIT` arm).
    ///
    /// # Errors
    ///
    /// `DUPLICATE_WAIT` when the call already holds a wait.
    ///
    /// # Panics
    ///
    /// Panics when the wait registry mutex is poisoned.
    pub fn register(
        &self,
        agent: &str,
        call_id: &str,
        wait: Arc<TimedQuestionWait>,
    ) -> Result<(), UserQuestionError> {
        let mut waits = self.waits.lock().expect("wait registry locks");
        if waits.contains_key(&(agent.to_owned(), call_id.to_owned())) {
            drop(waits);
            return Err(UserQuestionError::duplicate_wait());
        }
        waits.insert((agent.to_owned(), call_id.to_owned()), wait);
        drop(waits);
        Ok(())
    }

    /// Removes one wait entry (the dsh `finally` cleanup).
    ///
    /// # Panics
    ///
    /// Panics when the wait registry mutex is poisoned.
    pub fn remove(&self, agent: &str, wait_id: &str) {
        self.waits
            .lock()
            .expect("wait registry locks")
            .remove(&(agent.to_owned(), wait_id.to_owned()));
    }
}

/// The timed ask outcome (dsh `TimedUserQuestionResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimedAskResult {
    /// The answer arrived inside the window.
    Answer(Value),
    /// The window closed unattended; the questions remain answerable.
    Pending {
        /// The call whose questions remain pending.
        call_id: String,
    },
}

/// Foreground wait whose first settlement the Client decides (dsh
/// `askTimed`).
///
/// The validations run first, the wait joins the registry, the ask
/// runs under the wait's cancellation, a `NO_PROVIDER` ask parks on
/// the wait (the deadline maps to the pending result), and every exit
/// cleans the registry entry.
///
/// # Errors
///
/// `BAD_TIMEOUT` for a wait outside `1..=2_147_483_647` ms, the ask
/// taxonomy otherwise; the `ASK_TIMED_OUT` expiry maps to
/// [`TimedAskResult::Pending`].
///
/// # Panics
///
/// Panics when the wait registry mutex is poisoned.
pub fn ask_timed(
    request: &AskRequest,
    call_id: &str,
    timeout_ms: u64,
    waits: &TimedWaits,
    seams: &AskSeams,
) -> Result<TimedAskResult, UserQuestionError> {
    if !(1..=2_147_483_647).contains(&timeout_ms) {
        return Err(UserQuestionError::bad_timeout());
    }
    // The dsh type contract requires the owner agent on the timed path.
    if request.agent.is_none() {
        return Err(UserQuestionError::caller_not_live());
    }
    assert_live_root(seams)?;
    let agent = request.agent.clone().expect("agent checked above");
    let wait = TimedQuestionWait::new(epoch_ms() + timeout_ms, request.aborted, None);
    waits.register(&agent, call_id, Arc::clone(&wait))?;
    let parked = ask(
        &AskRequest {
            aborted: wait.aborted(),
            ..request.clone()
        },
        seams,
    );
    let mapped = match parked {
        Ok(answer) => Ok(TimedAskResult::Answer(answer)),
        Err(error) => {
            if error.code == "NO_PROVIDER" {
                // The dsh parks on `wait.done` when no answerer claims
                // the request; the deadline then maps to the pending
                // result. Every arm below falls through to the shared
                // `finally` cleanup.
                match wait.done() {
                    Err(reason) if reason.code == "ASK_TIMED_OUT" => Ok(TimedAskResult::Pending {
                        call_id: call_id.to_owned(),
                    }),
                    Err(_) => Err(UserQuestionError::aborted()),
                    Ok(()) => Err(UserQuestionError::no_provider()),
                }
            } else if wait.aborted() {
                Err(UserQuestionError::aborted())
            } else {
                Err(error)
            }
        }
    };
    wait.close(UserQuestionError::aborted());
    waits.remove(&agent, call_id);
    mapped
}

/// The parsed optional options array (the dsh `toolQuestionSchema`
/// strictness: a present-but-unreadable row fails the whole batch).
enum ParsedOptions {
    /// The key is absent.
    Absent,
    /// The rows all read.
    Present(Vec<QuestionOption>),
}

/// Parses the optional options array.
fn parse_options(value: Option<&Value>) -> Option<ParsedOptions> {
    let Some(value) = value else {
        return Some(ParsedOptions::Absent);
    };
    let rows = value.as_array()?;
    let mut parsed = Vec::with_capacity(rows.len());
    for row in rows {
        parsed.push(QuestionOption {
            label: row.get("label")?.as_str()?.to_owned(),
            description: row
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned),
        });
    }
    Some(ParsedOptions::Present(parsed))
}

/// Reads the question batch out of logged `ask_user_question`
/// arguments (dsh `questionsOf`; the loose tool schema, the wire
/// `multi_select` key folding into the service `multiSelect`).
#[must_use]
pub fn questions_of(arguments: &str) -> Option<Vec<Question>> {
    let parsed: Value = serde_json::from_str(arguments).ok()?;
    let items = parsed.get("questions")?.as_array()?;
    if items.is_empty() {
        return None;
    }
    let mut questions = Vec::with_capacity(items.len());
    for item in items {
        let id = item.get("id").and_then(Value::as_str)?;
        let prompt = item.get("question").and_then(Value::as_str)?;
        let header = item
            .get("header")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let options = match parse_options(item.get("options"))? {
            ParsedOptions::Absent => None,
            ParsedOptions::Present(rows) => Some(rows),
        };
        questions.push(Question {
            id: id.to_owned(),
            question: prompt.to_owned(),
            detail: None,
            header,
            options,
            multi_select: item.get("multi_select").and_then(Value::as_bool),
            intent: None,
        });
    }
    Some(questions)
}

/// The first `text` block's string (dsh `content.find(type === 'text')`).
#[must_use]
fn first_text_of(content: &Value) -> Option<String> {
    let blocks = content.as_array()?;
    let text = blocks
        .iter()
        .find(|block| block.get("type").and_then(Value::as_str) == Some("text"))?;
    text.get("text").and_then(Value::as_str).map(str::to_owned)
}

/// Whether one recorded text is the pending payload (dsh
/// `isPendingResult`).
#[must_use]
pub fn is_pending_result(content: &Value) -> bool {
    let Some(text) = first_text_of(content) else {
        return false;
    };
    serde_json::from_str::<Value>(&text)
        .is_ok_and(|parsed| parsed.get("pending").and_then(Value::as_bool) == Some(true))
}

/// Reads the answer batch out of one recorded text (dsh
/// `answerBatchOf`); `None` when the text carries none this reader can
/// use.
#[must_use]
pub fn answer_batch_of(content: &Value) -> Option<Vec<AnswerItem>> {
    let text = first_text_of(content)?;
    let parsed: Value = serde_json::from_str(&text).ok()?;
    let items = parsed.get("answers")?.as_array()?;
    let mut answers = Vec::with_capacity(items.len());
    for item in items {
        let id = item.get("id")?.as_str()?.to_owned();
        let selected = item
            .get("selected")?
            .as_array()?
            .iter()
            .map(|label| label.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()?;
        answers.push(AnswerItem {
            id,
            selected,
            custom: item
                .get("custom")
                .and_then(Value::as_str)
                .map(str::to_owned),
        });
    }
    Some(answers)
}

/// Closes one answerable question and keeps the answers it settled
/// with (dsh `settleQuestion`); an unanswerable call changes nothing.
#[must_use]
fn settle_question(
    view: &ProjectionView,
    call_id: &str,
    answers: Vec<AnswerItem>,
) -> ProjectionView {
    let Some(question) = view.active.iter().find(|item| item.call_id == call_id) else {
        return view.clone();
    };
    ProjectionView {
        active: view
            .active
            .iter()
            .filter(|item| item.call_id != call_id)
            .cloned()
            .collect(),
        settled: [
            view.settled.as_slice(),
            std::slice::from_ref(&SettledUserQuestion {
                call_id: question.call_id.clone(),
                answers,
            }),
        ]
        .concat(),
    }
}

/// Applies one session event to the fold (dsh `applyUserQuestionEvent`).
#[expect(
    clippy::too_many_lines,
    reason = "the fold mirrors the dsh apply one arm per event type; splitting it would hide the correspondence"
)]
#[must_use]
pub fn apply_event(fold: &FoldState, event: &Value) -> FoldState {
    let event_type = event.get("type").and_then(Value::as_str);
    let data = event.get("data");
    match event_type {
        Some("request/header") => {
            let timed = data
                .and_then(|data| data.pointer("/header/tools"))
                .and_then(Value::as_array)
                .is_some_and(|tools| {
                    tools.iter().any(|tool| {
                        tool.get("name").and_then(Value::as_str) == Some(ASK_USER_QUESTION_TOOL)
                            && tool
                                .get("parameters")
                                .and_then(|parameters| parameters.get("properties"))
                                .is_some_and(|properties| {
                                    properties.get(TIMED_WAIT_PARAMETER).is_some()
                                })
                    })
                });
            if timed == fold.timed {
                fold.clone()
            } else {
                FoldState {
                    timed,
                    questions: fold.questions.clone(),
                }
            }
        }
        Some("tool/call") => {
            if !fold.timed
                || data
                    .and_then(|data| data.get("name"))
                    .and_then(Value::as_str)
                    != Some(ASK_USER_QUESTION_TOOL)
            {
                return fold.clone();
            }
            let arguments = data
                .and_then(|data| data.get("arguments"))
                .and_then(Value::as_str);
            let call_id = data
                .and_then(|data| data.get("callId"))
                .and_then(Value::as_str);
            let (Some(arguments), Some(call_id)) = (arguments, call_id) else {
                return fold.clone();
            };
            let Some(questions) = questions_of(arguments) else {
                return fold.clone();
            };
            let mut view = fold.questions.clone();
            view.active.retain(|item| item.call_id != call_id);
            view.active.push(PendingUserQuestion {
                call_id: call_id.to_owned(),
                questions,
                state: QuestionState::Open,
            });
            FoldState {
                timed: fold.timed,
                questions: view,
            }
        }
        Some("tool/result") => {
            let Some(call_id) = data
                .and_then(|data| data.pointer("/message/toolCallId"))
                .and_then(Value::as_str)
            else {
                return fold.clone();
            };
            if !fold
                .questions
                .active
                .iter()
                .any(|item| item.call_id == call_id)
            {
                return fold.clone();
            }
            let pending = data
                .and_then(|data| data.pointer("/message/content"))
                .is_some_and(is_pending_result);
            let unknown = data
                .and_then(|data| data.pointer("/error/code"))
                .and_then(Value::as_str)
                == Some(TOOL_OUTCOME_UNKNOWN);
            if pending || unknown {
                let mut view = fold.questions.clone();
                for item in &mut view.active {
                    if item.call_id == call_id {
                        item.state = QuestionState::Continued;
                    }
                }
                return FoldState {
                    timed: fold.timed,
                    questions: view,
                };
            }
            let error_present = data
                .and_then(|data| data.get("error"))
                .is_some_and(|error| !error.is_null());
            let is_error = data
                .and_then(|data| data.pointer("/message/isError"))
                .and_then(Value::as_bool)
                == Some(true);
            let answers = if !error_present && !is_error {
                data.and_then(|data| data.pointer("/message/content"))
                    .and_then(answer_batch_of)
            } else {
                None
            };
            let mut view = fold.questions.clone();
            match answers {
                Some(answers) => view = settle_question(&view, call_id, answers),
                None => view.active.retain(|item| item.call_id != call_id),
            }
            FoldState {
                timed: fold.timed,
                questions: view,
            }
        }
        Some("tool/ptc-dispatch") => {
            if data
                .and_then(|data| data.get("name"))
                .and_then(Value::as_str)
                != Some(ASK_USER_QUESTION_TOOL)
            {
                return fold.clone();
            }
            let is_error = data
                .and_then(|data| data.get("isError"))
                .and_then(Value::as_bool)
                == Some(true);
            if is_error
                || !data
                    .and_then(|data| data.get("content"))
                    .is_some_and(is_pending_result)
            {
                return fold.clone();
            }
            let json_text = data
                .and_then(|data| data.get("arguments"))
                .map(Value::to_string);
            let Some(json_text) = json_text else {
                return fold.clone();
            };
            let Some(questions) = questions_of(&json_text) else {
                return fold.clone();
            };
            let Some(call_id) = data
                .and_then(|data| data.get("subCallId"))
                .and_then(Value::as_str)
            else {
                return fold.clone();
            };
            let mut view = fold.questions.clone();
            view.active.retain(|item| item.call_id != call_id);
            view.active.push(PendingUserQuestion {
                call_id: call_id.to_owned(),
                questions,
                state: QuestionState::Continued,
            });
            FoldState {
                timed: fold.timed,
                questions: view,
            }
        }
        Some("user/message") => {
            if data
                .and_then(|data| data.pointer("/source/kind"))
                .and_then(Value::as_str)
                != Some(REPLY_SOURCE_KIND)
            {
                return fold.clone();
            }
            let Some(call_id) = data
                .and_then(|data| data.pointer("/source/callId"))
                .and_then(Value::as_str)
            else {
                return fold.clone();
            };
            let answers = data
                .and_then(|data| data.get("content"))
                .and_then(answer_batch_of)
                .unwrap_or_default();
            let questions = settle_question(&fold.questions, call_id, answers);
            if questions == fold.questions {
                fold.clone()
            } else {
                FoldState {
                    timed: fold.timed,
                    questions,
                }
            }
        }
        _ => fold.clone(),
    }
}

/// Folds a whole session log into its question state (dsh
/// `foldUserQuestions`).
#[must_use]
pub fn fold(events: &Value) -> Value {
    let mut state = FoldState {
        timed: false,
        questions: ProjectionView::default(),
    };
    if let Value::Array(items) = events {
        for event in items {
            state = apply_event(&state, event);
        }
    }
    serde_json::to_value(state).unwrap_or_else(|_| json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_question(id: &str) -> Question {
        Question {
            id: id.to_owned(),
            question: "Pick".to_owned(),
            detail: None,
            header: None,
            options: None,
            multi_select: None,
            intent: None,
        }
    }

    /// The seam answerer returning a fixed batch (the Q101 family seam
    /// input; the success arm is structurally unreachable in M1).
    fn fixed_answerer(expected: Value) -> impl Fn(&[Question]) -> Result<Value, UserQuestionError> {
        move |_| Ok(expected.clone())
    }

    #[test]
    fn ask_chain_pins_the_refusal_order() {
        let seams = AskSeams::default();
        let aborted = AskRequest {
            aborted: true,
            questions: vec![plain_question("q")],
            ..AskRequest::default()
        };
        assert_eq!(ask(&aborted, &seams).unwrap_err().code, "ASK_ABORTED");
        assert_eq!(
            ask(&AskRequest::default(), &seams).unwrap_err(),
            UserQuestionError::empty_questions()
        );
        let request = AskRequest {
            questions: vec![plain_question("q")],
            agent: Some("s1".to_owned()),
            ..AskRequest::default()
        };
        assert_eq!(
            ask(&request, &seams).unwrap_err(),
            UserQuestionError::no_provider()
        );
        let delegated = AskSeams {
            caller_root: false,
            ..AskSeams::default()
        };
        assert_eq!(
            ask(&request, &delegated).unwrap_err().code,
            "DELEGATED_CALLER"
        );
        let stale = AskSeams {
            caller_live: false,
            ..AskSeams::default()
        };
        assert_eq!(ask(&request, &stale).unwrap_err().code, "CALLER_NOT_LIVE");
    }

    #[test]
    fn intent_validation_pins_the_two_texts() {
        let question = Question {
            id: "r1".to_owned(),
            question: "Approve?".to_owned(),
            detail: None,
            header: None,
            options: Some(vec![QuestionOption {
                label: "Keep planning".to_owned(),
                description: None,
            }]),
            multi_select: None,
            intent: Some(QuestionIntent {
                kind: "plan-review".to_owned(),
                approve: "Approve".to_owned(),
                call_id: None,
            }),
        };
        assert_eq!(
            validate_intent(&question).unwrap_err().message,
            "question r1 declares intent plan-review whose approve label \"Approve\" names none of its options"
        );
        let with_label = Question {
            options: Some(vec![QuestionOption {
                label: "Approve".to_owned(),
                description: None,
            }]),
            ..question
        };
        assert_eq!(
            validate_intent(&with_label).unwrap_err().message,
            "question r1 declares intent plan-review without the detail it reviews"
        );
        let complete = Question {
            detail: Some("# Plan".to_owned()),
            ..with_label
        };
        assert_eq!(validate_intent(&complete), Ok(()));
    }

    #[test]
    fn ask_success_rides_the_seam_answerer() {
        let answerer = fixed_answerer(json!({
            "answers": [{ "id": "q", "selected": ["A"] }]
        }));
        let seams = AskSeams {
            answerer: Some(&answerer),
            ..AskSeams::default()
        };
        let value = ask(
            &AskRequest {
                questions: vec![plain_question("q")],
                ..AskRequest::default()
            },
            &seams,
        )
        .expect("the answerer claims the request");
        assert_eq!(
            value,
            json!({ "answers": [{ "id": "q", "selected": ["A"] }] })
        );
    }

    #[test]
    fn timed_expiry_maps_to_pending() {
        let waits = TimedWaits::default();
        let result = ask_timed(
            &AskRequest {
                questions: vec![plain_question("q")],
                agent: Some("s1".to_owned()),
                ..AskRequest::default()
            },
            "call-1",
            1,
            &waits,
            &AskSeams::default(),
        )
        .expect("the expiry maps to the pending result");
        assert_eq!(
            result,
            TimedAskResult::Pending {
                call_id: "call-1".to_owned()
            }
        );
        // The registry entry cleaned up with the outcome.
        let again = ask_timed(
            &AskRequest {
                questions: vec![plain_question("q")],
                agent: Some("s1".to_owned()),
                ..AskRequest::default()
            },
            "call-1",
            1,
            &waits,
            &AskSeams::default(),
        )
        .expect("the registry entry was cleaned");
        assert!(matches!(again, TimedAskResult::Pending { .. }));
    }

    #[test]
    fn timed_boundaries_pin_the_rejections() {
        let waits = TimedWaits::default();
        let request = AskRequest {
            questions: vec![plain_question("q")],
            agent: Some("s1".to_owned()),
            ..AskRequest::default()
        };
        assert_eq!(
            ask_timed(&request, "c", 0, &waits, &AskSeams::default())
                .unwrap_err()
                .message,
            "timeout must fit a positive platform timer"
        );
        assert_eq!(
            ask_timed(&request, "c", 2_147_483_648, &waits, &AskSeams::default())
                .unwrap_err()
                .code,
            "BAD_TIMEOUT"
        );
        // No agent on the timed path: the caller fence fires.
        assert_eq!(
            ask_timed(&AskRequest::default(), "c", 1, &waits, &AskSeams::default())
                .unwrap_err()
                .code,
            "CALLER_NOT_LIVE"
        );
    }

    #[test]
    fn duplicate_wait_refuses_the_second_call() {
        let waits = TimedWaits::default();
        let held = TimedQuestionWait::new(epoch_ms() + 60_000, false, None);
        waits
            .register("s1", "call-1", held)
            .expect("the first wait registers");
        let request = AskRequest {
            questions: vec![plain_question("q")],
            agent: Some("s1".to_owned()),
            ..AskRequest::default()
        };
        assert_eq!(
            ask_timed(&request, "call-1", 60_000, &waits, &AskSeams::default())
                .unwrap_err()
                .message,
            "the question call already has a foreground wait"
        );
    }

    #[test]
    fn aborted_parent_closes_the_wait_before_asking() {
        let waits = TimedWaits::default();
        let request = AskRequest {
            questions: vec![plain_question("q")],
            agent: Some("s1".to_owned()),
            aborted: true,
        };
        let result = ask_timed(&request, "call-1", 60_000, &waits, &AskSeams::default());
        assert_eq!(result.unwrap_err().code, "ASK_ABORTED");
    }

    #[test]
    fn wait_expiry_and_close_deliver_their_reasons() {
        // An already-expired deadline fires the timeout error.
        let expired = TimedQuestionWait::new(epoch_ms() - 1, false, None);
        assert_eq!(expired.done().unwrap_err().code, "ASK_TIMED_OUT");
        // An explicit close delivers its own reason.
        let closed = TimedQuestionWait::new(epoch_ms() + 60_000, false, None);
        closed.close(UserQuestionError::aborted());
        assert_eq!(closed.done().unwrap_err().code, "ASK_ABORTED");
        // A claim suspends the timer until it releases.
        let claimed = TimedQuestionWait::new(epoch_ms() - 1, false, None);
        claimed.attach();
        let answer = std::thread::scope(|scope| {
            let waiter = scope.spawn(|| claimed.done().unwrap_err().code);
            std::thread::sleep(std::time::Duration::from_millis(30));
            claimed.release();
            waiter.join().expect("the waiter joins")
        });
        assert_eq!(answer, "ASK_TIMED_OUT");
    }

    fn event(kind: &str, data: Value) -> Value {
        let mut object = serde_json::Map::new();
        object.insert("type".to_owned(), Value::from(kind));
        object.insert("data".to_owned(), data);
        Value::Object(object)
    }

    fn fold_events(events: &[Value]) -> (bool, ProjectionView) {
        let folded = fold(&json!(events));
        (
            folded
                .get("timed")
                .and_then(Value::as_bool)
                .expect("timed flag"),
            serde_json::from_value(folded.get("questions").cloned().expect("questions"))
                .expect("the view parses"),
        )
    }

    fn timed_header() -> Value {
        event(
            "request/header",
            json!({ "header": { "tools": [
                { "name": "ask_user_question", "parameters": { "properties": {
                    "questions": {}, "timeout": {}
                } } }
            ] } }),
        )
    }

    #[test]
    fn fold_tracks_timed_calls_through_their_lifecycle() {
        let call = event(
            "tool/call",
            json!({
                "name": "ask_user_question",
                "arguments": r#"{"questions":[{"id":"q1","question":"Pick","multi_select":true}]}"#,
                "callId": "c1",
            }),
        );
        // Without a timed header the call never enters the fold.
        let (timed, view) = fold_events(std::slice::from_ref(&call));
        assert!(!timed);
        assert_eq!(view.active.len(), 0);
        // With one, the call lands open and the multi_select folds.
        let (timed, view) = fold_events(&[timed_header(), call.clone()]);
        assert!(timed);
        assert_eq!(view.active.len(), 1);
        assert_eq!(view.active[0].state, QuestionState::Open);
        assert_eq!(view.active[0].questions[0].multi_select, Some(true));
        assert_eq!(view.active[0].questions[0].header, None);
        // The pending result keeps the call answerable as continued.
        let pending_result = event(
            "tool/result",
            json!({
                "message": { "toolCallId": "c1", "content": [
                    { "type": "text", "text": r#"{"pending":true,"callId":"c1","message":"m"}"#
                    }
                ] }
            }),
        );
        let (_, view) = fold_events(&[timed_header(), call.clone(), pending_result]);
        assert_eq!(view.active[0].state, QuestionState::Continued);
        // The answer batch settles it.
        let answered = event(
            "tool/result",
            json!({
                "message": { "toolCallId": "c1", "isError": false, "content": [
                    { "type": "text", "text": r#"{"answers":[{"id":"q1","selected":["A"]}]}"# }
                ] }
            }),
        );
        let (_, view) = fold_events(&[timed_header(), call, answered]);
        assert_eq!(view.active.len(), 0);
        assert_eq!(view.settled.len(), 1);
        assert_eq!(view.settled[0].answers[0].selected, vec!["A"]);
    }

    #[test]
    fn fold_pins_the_failure_and_repair_arms() {
        let call = event(
            "tool/call",
            json!({
                "name": "ask_user_question",
                "arguments": r#"{"questions":[{"id":"q1","question":"Pick"}]}"#,
                "callId": "c1",
            }),
        );
        // A failed result drops the call.
        let failed = event(
            "tool/result",
            json!({
                "message": { "toolCallId": "c1", "isError": true, "content": [
                    { "type": "text", "text": "boom" }
                ] }
            }),
        );
        let (_, view) = fold_events(&[timed_header(), call.clone(), failed]);
        assert_eq!(view.active.len(), 0);
        assert_eq!(view.settled.len(), 0);
        // The resume-repair synthetic code keeps the call answerable.
        let unknown = event(
            "tool/result",
            json!({
                "message": { "toolCallId": "c1", "content": [] },
                "error": { "name": "ToolOutcomeUnknownError", "code": "TOOL_OUTCOME_UNKNOWN" }
            }),
        );
        let (_, view) = fold_events(&[timed_header(), call, unknown]);
        assert_eq!(view.active[0].state, QuestionState::Continued);
    }

    #[test]
    fn fold_lands_the_late_reply_and_the_ptc_subcall() {
        let call = event(
            "tool/call",
            json!({
                "name": "ask_user_question",
                "arguments": r#"{"questions":[{"id":"q1","question":"Pick"}]}"#,
                "callId": "c1",
            }),
        );
        let pending_result = event(
            "tool/result",
            json!({
                "message": { "toolCallId": "c1", "content": [
                    { "type": "text", "text": r#"{"pending":true,"callId":"c1"}"# }
                ] }
            }),
        );
        // A late reply settles with its batch.
        let reply = event(
            "user/message",
            json!({
                "content": [
                    { "type": "text", "text": r#"{"answers":[{"id":"q1","selected":[],"custom":"go"}]}"# }
                ],
                "source": { "kind": "user-question-reply", "callId": "c1", "outcome": "answered" }
            }),
        );
        let (_, view) = fold_events(&[timed_header(), call, pending_result, reply]);
        assert_eq!(view.active.len(), 0);
        assert_eq!(view.settled.len(), 1);
        assert_eq!(view.settled[0].answers[0].custom.as_deref(), Some("go"));
        // A PTC sub-call with a pending result lands as continued.
        let sub = event(
            "tool/ptc-dispatch",
            json!({
                "name": "ask_user_question",
                "isError": false,
                "arguments": { "questions": [{ "id": "q2", "question": "Where" }] },
                "content": [{ "type": "text", "text": r#"{"pending":true,"callId":"s1"}"# }],
                "subCallId": "s1",
            }),
        );
        let (_, view) = fold_events(&[timed_header(), sub]);
        assert_eq!(view.active.len(), 1);
        assert_eq!(view.active[0].call_id, "s1");
        assert_eq!(view.active[0].state, QuestionState::Continued);
    }

    #[test]
    fn questions_of_strictness_pins_the_unreadable_arms() {
        assert!(questions_of("not json").is_none());
        assert!(questions_of(r#"{"questions":[]}"#).is_none());
        assert!(questions_of(r#"{"questions":[{"id":"a"}]}"#).is_none());
        assert!(questions_of(r#"{"questions":[{"question":"x"}]}"#).is_none());
        // A present options row without a label fails the whole batch.
        assert!(
            questions_of(
                r#"{"questions":[{"id":"a","question":"x","options":[{"description":"d"}]}]}"#
            )
            .is_none()
        );
        let parsed = questions_of(
            r#"{"questions":[{"id":"a","question":"x","header":"H","options":[{"label":"L","description":"D"}],"extra":1}]}"#,
        )
        .expect("the loose shape parses");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].header.as_deref(), Some("H"));
        assert_eq!(parsed[0].options.as_ref().expect("options")[0].label, "L");
        assert_eq!(parsed[0].detail, None);
        assert_eq!(parsed[0].intent, None);
    }
}
