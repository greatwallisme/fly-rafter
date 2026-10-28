// Rust guideline compliant 2026-10-28
//! The approval capability seam (dsh `user-approval`, verbatim semantics).
//!
//! One session's approval policy is decided before any interactive
//! answerer sees an ask; every ask/outcome pair lands on the session log
//! as the `approval/asked` + `approval/decided` audit pair inside the
//! open turn that needed the decision. Missing or misbehaving answerers
//! fail closed (`unavailable`); `'never'` rejects deterministically
//! before dispatch; the only grant is the one-shot `'allowed-once'`.
//!
//! Wire carriers live in [`service`] (the `'approval'` method face) and
//! [`plugin_shell`] (the dylib artifact); this module owns the policy
//! fold, the audit protocol, and the answerer chain over injected seams:
//!
//! - [`SessionLog`]: append + read the session log (the
//!   `'session-persistence'` service behind the shell);
//! - [`AgentInject`]: queue the policy-switch notice for the agent's
//!   next model step (the `'agent'` service's inject face);
//! - [`Answerer`]: one composed decision endpoint (registered through
//!   the service face, dispatched in registration order — Q108′A; M1
//!   ships none, so every ask resolves `'unavailable'`).

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

pub mod plugin_shell;
pub mod service;

/// A session's approval policy: `'ask'` delegates to the composed
/// answerers (fail-closed with none), `'never'` auto-rejects every ask
/// without prompting (the deterministic CI/unattended stance).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalPolicy {
    /// Delegate asks to the composed answerer chain.
    Ask,
    /// Never prompt: every ask resolves `'rejected'` deterministically.
    Never,
}

impl ApprovalPolicy {
    /// The wire word for this policy.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Never => "never",
        }
    }

    /// Parses the wire vocabulary; `None` outside it.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "ask" => Some(Self::Ask),
            "never" => Some(Self::Never),
            _ => None,
        }
    }
}

/// Every policy, for option advertisement and runtime validation of
/// untrusted policy strings (dsh `APPROVAL_POLICIES`).
pub const APPROVAL_POLICIES: &[ApprovalPolicy] = &[ApprovalPolicy::Ask, ApprovalPolicy::Never];

/// Model-facing statement for the deterministic `'never'` policy
/// (dsh user-approval/src/index.ts:73, verbatim).
pub const NEVER_SENTENCE: &str = "Approval prompts are disabled in this session: actions that require approval are rejected automatically — do not request sandbox escalation (do not set `sandbox_permissions`).";

/// Model-facing statement for an interactive policy that may still fail
/// closed (dsh user-approval/src/index.ts:75, verbatim).
pub const ASK_SENTENCE: &str = "Approval policy: ask. Operations that require approval may ask through the configured answerers; without an available answerer, the request fails closed.";

/// One closed approval outcome: a one-shot grant, an explicit rejection,
/// a withdrawn request, or an unavailable answerer (callers fail closed
/// on the last).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOutcome {
    /// The one-shot grant.
    AllowedOnce,
    /// An answerer (or the `'never'` policy) rejected the ask.
    Rejected,
    /// The request was withdrawn before an answer.
    Cancelled,
    /// No answerer was available or one misbehaved (fail closed).
    Unavailable,
}

impl ApprovalOutcome {
    /// The wire word for this outcome (dsh `OUTCOMES` vocabulary).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AllowedOnce => "allowed-once",
            Self::Rejected => "rejected",
            Self::Cancelled => "cancelled",
            Self::Unavailable => "unavailable",
        }
    }

    /// Parses the wire vocabulary; `None` outside it (a rogue return is
    /// normalized to `Unavailable` by the caller, never leaked).
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "allowed-once" => Some(Self::AllowedOnce),
            "rejected" => Some(Self::Rejected),
            "cancelled" => Some(Self::Cancelled),
            "unavailable" => Some(Self::Unavailable),
            _ => None,
        }
    }
}

/// The appended audit pair's precondition text (dsh
/// user-approval/src/index.ts:218-222, verbatim).
const OUTSIDE_TURN: &str = concat!(
    "approval.request() outside an open turn: the approval/asked + approval/decided audit pair ",
    "must be turn-enclosed (a bare event between turns is crash-tail garbage on reload). ",
    "Ask from inside the turn that needs the decision.",
);

/// The readonly ask the answerer chain sees: the agent (session id), the
/// tool, the optional exact call, the asker's reason, and the localized
/// presentation (never persisted — dsh `ApprovalRequestEvent`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalAsk {
    /// The agent (session) on whose behalf the question is asked.
    pub agent: String,
    /// The tool the question is about.
    pub tool_name: String,
    /// The exact tool call being decided, when the asker has one.
    pub call_id: Option<String>,
    /// The asker's human-readable explanation of why it is asking.
    pub reason: Option<String>,
    /// Localized presentation only; never persisted in audit events.
    pub display_reason: Option<Value>,
}

/// The session-log seam: append one log-only event and read the whole
/// committed log in seq order (the `'session-persistence'` service).
pub trait SessionLog: Send + Sync {
    /// Appends one log-only event; the failure text is the append
    /// refusal.
    ///
    /// # Errors
    ///
    /// Returns the session service's refusal text.
    fn append(&self, session: &str, type_name: &str, data: Value) -> Result<(), String>;

    /// Reads every committed event of one live session in seq order.
    ///
    /// # Errors
    ///
    /// Returns the session service's refusal text.
    fn read_all(&self, session: &str) -> Result<Vec<Value>, String>;
}

/// The agent-notice seam: queue one non-waking user message for the
/// agent's next model step (the `'agent'` service's inject face).
pub trait AgentInject: Send + Sync {
    /// Queues the wire message for the session's next step.
    ///
    /// # Errors
    ///
    /// Returns the agent service's refusal text.
    fn inject(&self, session: &str, message: Value) -> Result<(), String>;
}

/// One answerer's reply: claim the request with an outcome, or delegate
/// to the rest of the chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Claim the request with this outcome (normalized to the
    /// fail-closed outcome when outside the vocabulary).
    Final(ApprovalOutcome),
    /// Pass the request to the next answerer.
    Delegate,
}

/// One composed answerer endpoint. Dispatch is synchronous and
/// contained: an error fails the question closed (`unavailable`), never
/// the caller.
pub trait Answerer: Send + Sync {
    /// Answers one ask.
    ///
    /// # Errors
    ///
    /// Returns the endpoint failure text (contained to `unavailable`).
    fn answer(&self, ask: &ApprovalAsk) -> Result<Answer, String>;
}

/// The approval service over injected seams: the session policy fold,
/// the turn-enclosed audit pair, and the answerer chain in registration
/// order (dsh `ApprovalService`).
pub struct Approval {
    /// The deployment's configured default for sessions without an
    /// override (dsh `Config.policy`, schema-defaulted before it lands
    /// here).
    config_policy: ApprovalPolicy,
    log: Arc<dyn SessionLog>,
    inject: Arc<dyn AgentInject>,
    /// The chain in registration order with stable ids (a positional
    /// token would drift when an earlier entry unregisters).
    answerers: Mutex<Vec<(u64, Arc<dyn Answerer>)>>,
    next_answerer_id: AtomicU64,
    /// Fresh ids pair each asked with its decided (dsh mints a
    /// `randomUUID`; a process-unique counter serves the same audit
    /// contract).
    request_ids: AtomicU64,
    notice_ids: AtomicU64,
}

impl std::fmt::Debug for Approval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Approval")
            .field("config_policy", &self.config_policy)
            .field(
                "answerers",
                &self.answerers.lock().expect("answerer lock").len(),
            )
            .finish_non_exhaustive()
    }
}

/// Whether the log currently sits inside an open turn (dsh
/// `hasOpenTurn`).
///
/// A `turn/start` not yet closed by a `turn/end` is the request
/// precondition: the audit pair must be turn-enclosed — the turn is the
/// durable log's commit/replay boundary.
#[must_use]
pub fn has_open_turn(log: &[Value]) -> bool {
    for event in log.iter().rev() {
        let kind = event["type"].as_str().unwrap_or_default();
        if kind == "turn/start" {
            return true;
        }
        if kind == "turn/end" {
            return false;
        }
    }
    false
}

/// Folds the last `approval/policy` payload without applying the
/// configured default (dsh `overrideOf`).
#[must_use]
pub fn override_of(log: &[Value]) -> Option<ApprovalPolicy> {
    for event in log.iter().rev() {
        if event["type"].as_str() == Some("approval/policy") {
            return event["data"]["policy"]
                .as_str()
                .and_then(ApprovalPolicy::parse);
        }
    }
    None
}

/// Appends the sole durable representation of a session policy override
/// (dsh `setApprovalPolicy`).
///
/// A free function: no agent notice rides along — session
/// initialization and knob setters use it directly; the invalid-word
/// refusal is the wire decode face's, pinned there.
///
/// # Errors
///
/// Returns the append refusal text.
pub fn set_approval_policy(
    log: &dyn SessionLog,
    session: &str,
    policy: ApprovalPolicy,
) -> Result<(), String> {
    log.append(
        session,
        "approval/policy",
        serde_json::json!({ "policy": policy.as_str() }),
    )
}

impl Approval {
    /// Assembles the service over its seams.
    #[must_use]
    pub fn new(
        config_policy: ApprovalPolicy,
        log: Arc<dyn SessionLog>,
        inject: Arc<dyn AgentInject>,
    ) -> Self {
        Self {
            config_policy,
            log,
            inject,
            answerers: Mutex::new(Vec::new()),
            next_answerer_id: AtomicU64::new(0),
            request_ids: AtomicU64::new(0),
            notice_ids: AtomicU64::new(0),
        }
    }

    /// The session's effective policy: its own `approval/policy` fold,
    /// else the configured default (already schema-defaulted, so the
    /// dsh `?? 'ask'` type-narrowing has no runtime arm here).
    ///
    /// # Errors
    ///
    /// Returns the session read refusal text.
    pub fn effective_policy(&self, session: &str) -> Result<ApprovalPolicy, String> {
        Ok(self.override_of(session)?.unwrap_or(self.config_policy))
    }

    /// Reads the session override without applying the configured
    /// default (dsh `overrideOf`, public read).
    ///
    /// # Errors
    ///
    /// Returns the session read refusal text.
    pub fn override_of(&self, session: &str) -> Result<Option<ApprovalPolicy>, String> {
        Ok(override_of(&self.log.read_all(session)?))
    }

    /// Registers one answerer at the chain's end; the returned stable
    /// token removes it (registration order is dispatch order — Q108′A).
    ///
    /// # Panics
    ///
    /// Panics when the answerer lock is poisoned by a concurrent panic.
    pub fn register_answerer(&self, answerer: Arc<dyn Answerer>) -> u64 {
        let mut answerers = self.answerers.lock().expect("answerer lock");
        let token = self.next_answerer_id.fetch_add(1, Ordering::SeqCst) + 1;
        answerers.push((token, answerer));
        token
    }

    /// Removes the token's answerer; unknown tokens refuse.
    ///
    /// # Errors
    ///
    /// Returns the unknown-token refusal text.
    ///
    /// # Panics
    ///
    /// Panics when the answerer lock is poisoned by a concurrent panic.
    pub fn unregister_answerer(&self, token: u64) -> Result<(), String> {
        let mut answerers = self.answerers.lock().expect("answerer lock");
        let position = answerers.iter().position(|(id, _)| *id == token);
        let outcome = position.map_or_else(
            || Err(format!("unknown approval answerer token {token}")),
            |index| {
                answerers.remove(index);
                Ok(())
            },
        );
        drop(answerers);
        outcome
    }

    /// Switches one live agent's policy and queues the transition notice
    /// for its next model step (dsh `setPolicy`; a same-value switch is
    /// a no-op).
    ///
    /// # Errors
    ///
    /// Returns the session append or agent inject refusal text.
    pub fn set_policy(&self, agent: &str, policy: ApprovalPolicy) -> Result<(), String> {
        let previous = self.effective_policy(agent)?;
        if previous == policy {
            return Ok(());
        }
        set_approval_policy(&*self.log, agent, policy)?;
        let notice = self.policy_notice(previous, policy);
        self.inject.inject(agent, notice)
    }

    /// Asks the composed answerers to decide one readonly request. The
    /// audit pair is turn-enclosed or the ask refuses before appending
    /// anything; the answerer phase always produces a closed outcome,
    /// and a failure that prevents either audit append from committing
    /// still rejects (dsh `ApprovalService.request`).
    ///
    /// M1 wire note: the dsh caller races an `AbortSignal` against the
    /// answer chain (`'cancelled'` wins, late answers are discarded);
    /// signals do not cross the dylib wire (batch 1e judgment), and M1
    /// composes no answerers, so the race has no live participant — the
    /// cancellation carrier is the calling tool pipeline's
    /// worker-vs-abort race (Q109-A).
    ///
    /// # Errors
    ///
    /// Returns the outside-turn refusal, or either audit append's
    /// refusal text.
    pub fn request(&self, ask: &ApprovalAsk) -> Result<ApprovalOutcome, String> {
        let log = self.log.read_all(&ask.agent)?;
        if !has_open_turn(&log) {
            return Err(OUTSIDE_TURN.to_owned());
        }
        let id = format!(
            "approval-request-{}",
            self.request_ids.fetch_add(1, Ordering::SeqCst) + 1
        );
        let mut asked = serde_json::json!({
            "id": id,
            "toolName": ask.tool_name,
        });
        if let Some(call_id) = &ask.call_id {
            asked["callId"] = Value::String(call_id.clone());
        }
        if let Some(reason) = &ask.reason {
            asked["reason"] = Value::String(reason.clone());
        }
        self.log.append(&ask.agent, "approval/asked", asked)?;
        let outcome = self.decide(ask, &log);
        self.log.append(
            &ask.agent,
            "approval/decided",
            serde_json::json!({ "id": id, "outcome": outcome.as_str() }),
        )?;
        Ok(outcome)
    }

    /// Dispatches the answerer chain, contained: the `'never'` policy is
    /// decided before any dispatch (a prepend-registered listener can
    /// never keep the documented promise for it), a throwing answerer
    /// fails the question closed, and a rogue non-vocabulary return
    /// normalizes to the fail-closed outcome (dsh `decide`).
    fn decide(&self, ask: &ApprovalAsk, log: &[Value]) -> ApprovalOutcome {
        if override_of(log).unwrap_or(self.config_policy) == ApprovalPolicy::Never {
            return ApprovalOutcome::Rejected;
        }
        let answerers = self
            .answerers
            .lock()
            .expect("answerer lock")
            .iter()
            .map(|(_, answerer)| Arc::clone(answerer))
            .collect::<Vec<_>>();
        for answerer in answerers {
            match answerer.answer(ask) {
                Ok(Answer::Final(outcome)) => {
                    return ApprovalOutcome::parse(outcome.as_str())
                        .unwrap_or(ApprovalOutcome::Unavailable);
                }
                // A delegation falls through to the next answerer; a
                // throwing answerer fails the question closed.
                Ok(Answer::Delegate) => {}
                Err(_) => return ApprovalOutcome::Unavailable,
            }
        }
        ApprovalOutcome::Unavailable
    }

    /// The runtime-context statement for one agent's effective policy;
    /// a bare assemble (no agent) states nothing (dsh
    /// `approval:policy` context text).
    ///
    /// # Errors
    ///
    /// Returns the session read refusal text.
    pub fn context_text(&self, agent: Option<&str>) -> Result<String, String> {
        let Some(agent) = agent else {
            return Ok(String::new());
        };
        Ok(match self.effective_policy(agent)? {
            ApprovalPolicy::Never => NEVER_SENTENCE.to_owned(),
            ApprovalPolicy::Ask => ASK_SENTENCE.to_owned(),
        })
    }

    /// Builds the policy-switch notice as a wire user message (dsh
    /// `createUserMessage` with source kind `user-approval`); the
    /// sentence is verbatim.
    fn policy_notice(&self, previous: ApprovalPolicy, policy: ApprovalPolicy) -> Value {
        let id = self.notice_ids.fetch_add(1, Ordering::SeqCst) + 1;
        serde_json::json!({
            "role": "user",
            "id": format!("user-approval-{id}"),
            "content": [{
                "type": "text",
                "text": format!(
                    "The approval policy changed from \"{}\" to \"{}\" (changed by the user).",
                    previous.as_str(),
                    policy.as_str(),
                ),
            }],
            "source": { "kind": "user-approval" },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        APPROVAL_POLICIES, ASK_SENTENCE, AgentInject, Answer, Answerer, Approval, ApprovalAsk,
        ApprovalOutcome, ApprovalPolicy, NEVER_SENTENCE, SessionLog, has_open_turn, override_of,
        set_approval_policy,
    };
    use std::sync::{Arc, Mutex};

    /// An in-memory session log the folds and audit protocol run
    /// against; it can be made to fail appends on demand.
    #[derive(Debug, Default)]
    struct FakeLog {
        events: Mutex<Vec<serde_json::Value>>,
        fail_append: Mutex<Option<String>>,
    }

    impl SessionLog for FakeLog {
        fn append(
            &self,
            _session: &str,
            type_name: &str,
            data: serde_json::Value,
        ) -> Result<(), String> {
            let failure = self.fail_append.lock().expect("lock").clone();
            if let Some(text) = failure {
                return Err(text);
            }
            let seq = self.events.lock().expect("lock").len();
            self.events.lock().expect("lock").push(serde_json::json!({
                "type": type_name, "seq": seq, "time": 1, "data": data,
            }));
            Ok(())
        }

        fn read_all(&self, _session: &str) -> Result<Vec<serde_json::Value>, String> {
            Ok(self.events.lock().expect("lock").clone())
        }
    }

    #[derive(Debug, Default)]
    struct FakeInject {
        queued: Mutex<Vec<serde_json::Value>>,
    }

    impl AgentInject for FakeInject {
        fn inject(&self, _session: &str, message: serde_json::Value) -> Result<(), String> {
            self.queued.lock().expect("lock").push(message);
            Ok(())
        }
    }

    /// An answerer recording its calls with a scripted reply.
    struct ProbeAnswerer {
        calls: Mutex<Vec<String>>,
        reply: Result<Answer, String>,
    }

    impl Answerer for ProbeAnswerer {
        fn answer(&self, ask: &ApprovalAsk) -> Result<Answer, String> {
            self.calls.lock().expect("lock").push(ask.tool_name.clone());
            self.reply.clone()
        }
    }

    fn probe(reply: Result<Answer, String>) -> Arc<ProbeAnswerer> {
        Arc::new(ProbeAnswerer {
            calls: Mutex::new(Vec::new()),
            reply,
        })
    }

    fn ask(tool: &str) -> ApprovalAsk {
        ApprovalAsk {
            agent: "s1".to_owned(),
            tool_name: tool.to_owned(),
            call_id: None,
            reason: None,
            display_reason: None,
        }
    }

    fn service_with(policy: ApprovalPolicy, log: Arc<FakeLog>) -> (Approval, Arc<FakeInject>) {
        let inject = Arc::new(FakeInject::default());
        (
            Approval::new(policy, log, Arc::clone(&inject) as Arc<dyn AgentInject>),
            inject,
        )
    }

    fn open_turn(log: &FakeLog) {
        let _unused = log.append("s1", "turn/start", serde_json::json!({ "turn": 1 }));
    }

    #[test]
    fn vocabulary_round_trips_both_tables() {
        assert_eq!(
            APPROVAL_POLICIES,
            &[ApprovalPolicy::Ask, ApprovalPolicy::Never]
        );
        assert_eq!(ApprovalPolicy::parse("ask"), Some(ApprovalPolicy::Ask));
        assert_eq!(ApprovalPolicy::parse("never"), Some(ApprovalPolicy::Never));
        assert_eq!(ApprovalPolicy::parse("sometimes"), None);
        for outcome in [
            ApprovalOutcome::AllowedOnce,
            ApprovalOutcome::Rejected,
            ApprovalOutcome::Cancelled,
            ApprovalOutcome::Unavailable,
        ] {
            assert_eq!(
                ApprovalOutcome::parse(outcome.as_str()),
                Some(outcome),
                "outcome words must round trip"
            );
        }
        assert_eq!(ApprovalOutcome::parse("allowed-forever"), None);
    }

    #[test]
    fn sentences_are_verbatim() {
        assert!(NEVER_SENTENCE.starts_with("Approval prompts are disabled"));
        assert!(NEVER_SENTENCE.contains("do not set `sandbox_permissions`"));
        assert!(ASK_SENTENCE.starts_with("Approval policy: ask."));
        assert!(ASK_SENTENCE.ends_with("the request fails closed."));
    }

    #[test]
    fn folds_read_the_log_tail() {
        let log = FakeLog::default();
        assert!(!has_open_turn(&[]));
        assert_eq!(override_of(&[]), None);

        let _unused = log.append("s1", "turn/start", serde_json::json!({ "turn": 1 }));
        let _unused = log.append(
            "s1",
            "approval/policy",
            serde_json::json!({ "policy": "never" }),
        );
        let events = log.read_all("s1").expect("read");
        assert!(has_open_turn(&events));
        assert_eq!(override_of(&events), Some(ApprovalPolicy::Never));

        let _unused = log.append(
            "s1",
            "approval/policy",
            serde_json::json!({ "policy": "ask", "source": "delegation" }),
        );
        let _unused = log.append("s1", "turn/end", serde_json::json!({}));
        let events = log.read_all("s1").expect("read");
        assert!(!has_open_turn(&events));
        // The LAST policy wins; the delegation marker is transparent.
        assert_eq!(override_of(&events), Some(ApprovalPolicy::Ask));
    }

    #[test]
    fn set_approval_policy_appends_the_bare_override() {
        let log = Arc::new(FakeLog::default());
        set_approval_policy(&*log, "s1", ApprovalPolicy::Never).expect("valid append");
        let events = log.read_all("s1").expect("read");
        assert_eq!(events[0]["type"], "approval/policy");
        assert_eq!(events[0]["data"]["policy"], "never");
        assert!(events[0]["data"].get("source").is_none());
    }

    #[test]
    fn request_outside_an_open_turn_refuses_verbatim() {
        let log = Arc::new(FakeLog::default());
        let (service, _inject) = service_with(ApprovalPolicy::Ask, Arc::clone(&log));
        let error = service.request(&ask("bash")).expect_err("outside turn");
        assert_eq!(
            error,
            "approval.request() outside an open turn: the approval/asked + approval/decided audit pair must be turn-enclosed (a bare event between turns is crash-tail garbage on reload). Ask from inside the turn that needs the decision."
        );
        assert_eq!(
            log.read_all("s1").expect("read"),
            Vec::<serde_json::Value>::new()
        );
    }

    #[test]
    fn request_without_answerers_fails_closed_with_audit_pair() {
        let log = Arc::new(FakeLog::default());
        let (service, _inject) = service_with(ApprovalPolicy::Ask, Arc::clone(&log));
        open_turn(&log);
        let outcome = service.request(&ask("bash")).expect("request");
        assert_eq!(outcome, ApprovalOutcome::Unavailable);
        let events = log.read_all("s1").expect("read");
        assert_eq!(events.len(), 3);
        assert_eq!(events[1]["type"], "approval/asked");
        assert_eq!(events[1]["data"]["toolName"], "bash");
        assert_eq!(events[1]["data"]["id"], events[2]["data"]["id"]);
        assert_eq!(events[2]["type"], "approval/decided");
        assert_eq!(events[2]["data"]["outcome"], "unavailable");
    }

    #[test]
    fn never_policy_rejects_before_any_dispatch() {
        let log = Arc::new(FakeLog::default());
        let (service, _inject) = service_with(ApprovalPolicy::Ask, Arc::clone(&log));
        open_turn(&log);
        let _unused = set_approval_policy(&*log, "s1", ApprovalPolicy::Never);
        let first = probe(Ok(Answer::Final(ApprovalOutcome::AllowedOnce)));
        service.register_answerer(Arc::clone(&first) as Arc<dyn Answerer>);
        let outcome = service.request(&ask("bash")).expect("request");
        assert_eq!(outcome, ApprovalOutcome::Rejected);
        assert!(
            first.calls.lock().expect("lock").is_empty(),
            "'never' must decide before dispatch"
        );
        let events = log.read_all("s1").expect("read");
        assert_eq!(events.last().expect("pair")["data"]["outcome"], "rejected");
    }

    #[test]
    fn answerer_chain_dispatches_in_registration_order() {
        let log = Arc::new(FakeLog::default());
        let (service, _inject) = service_with(ApprovalPolicy::Ask, Arc::clone(&log));
        open_turn(&log);
        let delegator = probe(Ok(Answer::Delegate));
        let claimer = probe(Ok(Answer::Final(ApprovalOutcome::AllowedOnce)));
        let unreached = probe(Ok(Answer::Final(ApprovalOutcome::Rejected)));
        service.register_answerer(Arc::clone(&delegator) as Arc<dyn Answerer>);
        service.register_answerer(Arc::clone(&claimer) as Arc<dyn Answerer>);
        service.register_answerer(Arc::clone(&unreached) as Arc<dyn Answerer>);
        let outcome = service.request(&ask("fs_write")).expect("request");
        assert_eq!(outcome, ApprovalOutcome::AllowedOnce);
        assert_eq!(
            delegator.calls.lock().expect("lock").as_slice(),
            ["fs_write"]
        );
        assert_eq!(claimer.calls.lock().expect("lock").as_slice(), ["fs_write"]);
        assert!(unreached.calls.lock().expect("lock").is_empty());

        let token = 2;
        service.unregister_answerer(token).expect("unregister");
        let outcome = service.request(&ask("fs_write")).expect("request");
        // With the claimer removed, the delegator passes to the third
        // answerer, whose rejection now decides — removal reorders
        // nothing, it only takes its entry out.
        assert_eq!(outcome, ApprovalOutcome::Rejected);
        assert_eq!(
            unreached.calls.lock().expect("lock").as_slice(),
            ["fs_write"],
            "the tail answerer takes over once the claimer is gone"
        );
    }

    #[test]
    fn throwing_and_rogue_answerers_fail_the_question_closed() {
        let log = Arc::new(FakeLog::default());
        let (service, _inject) = service_with(ApprovalPolicy::Ask, Arc::clone(&log));
        open_turn(&log);
        let thrower = probe(Err("answerer exploded".to_owned()));
        service.register_answerer(Arc::clone(&thrower) as Arc<dyn Answerer>);
        let outcome = service.request(&ask("bash")).expect("request");
        assert_eq!(outcome, ApprovalOutcome::Unavailable);

        let token = 1;
        service.unregister_answerer(token).expect("unregister");
        // A rogue non-vocabulary return (simulated through the wire
        // normalizer's parse arm) cannot be produced by the typed enum;
        // the containment contract for it is pinned in the service
        // face's decode tests.
        let claimer = probe(Ok(Answer::Final(ApprovalOutcome::Cancelled)));
        service.register_answerer(Arc::clone(&claimer) as Arc<dyn Answerer>);
        let outcome = service.request(&ask("bash")).expect("request");
        assert_eq!(outcome, ApprovalOutcome::Cancelled);
    }

    #[test]
    fn an_uncommitted_audit_append_rejects_the_request() {
        let log = Arc::new(FakeLog::default());
        let (service, _inject) = service_with(ApprovalPolicy::Ask, Arc::clone(&log));
        open_turn(&log);
        *log.fail_append.lock().expect("lock") = Some("log is closed".to_owned());
        let error = service
            .request(&ask("bash"))
            .expect_err("asked append fails");
        assert_eq!(error, "log is closed");
        // The decision never returns unlogged.
        let events = log.read_all("s1").expect("read");
        assert_eq!(events.len(), 1, "only the turn opener is on the log");
    }

    #[test]
    fn set_policy_notices_the_change_verbatim() {
        let log = Arc::new(FakeLog::default());
        let (service, inject) = service_with(ApprovalPolicy::Ask, Arc::clone(&log));
        // A same-value switch is a no-op.
        service
            .set_policy("s1", ApprovalPolicy::Ask)
            .expect("no-op");
        assert!(inject.queued.lock().expect("lock").is_empty());
        assert_eq!(
            log.read_all("s1").expect("read"),
            Vec::<serde_json::Value>::new()
        );

        service
            .set_policy("s1", ApprovalPolicy::Never)
            .expect("switch");
        let events = log.read_all("s1").expect("read");
        assert_eq!(events[0]["type"], "approval/policy");
        assert_eq!(events[0]["data"]["policy"], "never");
        let queued = inject.queued.lock().expect("lock");
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0]["role"], "user");
        assert_eq!(queued[0]["source"]["kind"], "user-approval");
        assert_eq!(
            queued[0]["content"][0]["text"],
            "The approval policy changed from \"ask\" to \"never\" (changed by the user)."
        );
        // The notice is a valid wire message.
        let _message: fr_wire::message::Message =
            serde_json::from_value(queued[0].clone()).expect("wire message shape");
    }

    #[test]
    fn effective_policy_reads_override_then_config_then_ask() {
        let log = Arc::new(FakeLog::default());
        let (service, _inject) = service_with(ApprovalPolicy::Ask, Arc::clone(&log));
        assert_eq!(
            service.effective_policy("s1").expect("fold"),
            ApprovalPolicy::Ask
        );
        let _unused = set_approval_policy(&*log, "s1", ApprovalPolicy::Never);
        assert_eq!(
            service.effective_policy("s1").expect("fold"),
            ApprovalPolicy::Never
        );

        let (configured, _inject) =
            service_with(ApprovalPolicy::Never, Arc::new(FakeLog::default()));
        assert_eq!(
            configured.effective_policy("fresh").expect("fold"),
            ApprovalPolicy::Never
        );
    }

    #[test]
    fn context_text_states_the_effective_policy() {
        let log = Arc::new(FakeLog::default());
        let (service, _inject) = service_with(ApprovalPolicy::Ask, Arc::clone(&log));
        assert_eq!(service.context_text(None).expect("bare"), "");
        assert_eq!(
            service.context_text(Some("s1")).expect("text"),
            ASK_SENTENCE
        );
        let _unused = set_approval_policy(&*log, "s1", ApprovalPolicy::Never);
        assert_eq!(
            service.context_text(Some("s1")).expect("text"),
            NEVER_SENTENCE
        );
    }
}

// Rust guideline compliant 2026-10-28
