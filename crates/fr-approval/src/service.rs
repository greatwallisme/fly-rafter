// Rust guideline compliant 2026-10-28
//! The `'approval'` method face over the dougong dynamic channel.
//!
//! Method map (fr-internal DTOs are `snake_case`; the persisted audit
//! events they append carry the dsh wire camelCase vocabulary):
//! - 0 describe: `{}` — shell metadata echo;
//! - 1 request: `{agent, tool_name, call_id?, reason?, display_reason?}`
//!   → `{outcome}` — the turn-enclosed audit ask (Q108-A: answerers
//!   register here, dispatch in registration order, M1 composes none);
//! - 2 set-policy: `{agent, policy}` → `{}` — the live switch with the
//!   verbatim user notice;
//! - 3 override-of: `{session_id}` → `{policy}` or `{}` — the session's
//!   own fold without the configured default;
//! - 4 register-answerer: `{endpoint: {service, method}}` → `{token}`;
//! - 5 unregister-answerer: `{token}` → `{}`;
//! - 6 context-text: `{agent?}` → `{text}` — the runtime-context
//!   statement endpoint (`approval:policy`).
//!
//! The invalid-policy refusal (dsh user-approval/src/index.ts:102,
//! verbatim) lives here, at the decode boundary where an untrusted word
//! first appears.

#![forbid(unsafe_code)]

use serde_json::Value;
use serde_json::json;

use crate::plugin_shell::{PLUGIN_NAME, SERVICE_NAME};
use crate::{Answer, Answerer, Approval, ApprovalAsk, ApprovalOutcome, ApprovalPolicy};

/// Method selector: shell metadata echo (`{}`).
pub const METHOD_DESCRIBE: u32 = 0;
/// Method selector: one turn-enclosed approval ask.
pub const METHOD_REQUEST: u32 = 1;
/// Method selector: the live policy switch with its user notice.
pub const METHOD_SET_POLICY: u32 = 2;
/// Method selector: the session override fold without defaults.
pub const METHOD_OVERRIDE_OF: u32 = 3;
/// Method selector: append one answerer endpoint to the chain (Q108-A).
pub const METHOD_REGISTER_ANSWERER: u32 = 4;
/// Method selector: remove one answerer by token.
pub const METHOD_UNREGISTER_ANSWERER: u32 = 5;
/// Method selector: the `approval:policy` runtime-context endpoint.
pub const METHOD_CONTEXT_TEXT: u32 = 6;

/// The invalid-policy refusal (dsh user-approval/src/index.ts:102,
/// verbatim — it guards every untrusted policy word entering the wire).
pub const INVALID_POLICY: &str = "approval policy must be one of \"ask\" or \"never\"";

/// The `'approval'` service over the domain.
///
/// Registrations arrive as wire endpoints, so the shell's
/// `WireAnswerer` adapter turns each into a composed [`Answerer`]
/// carrying one stable token space through from register to unregister.
pub struct ApprovalService {
    domain: Approval,
    answerer_factory:
        std::sync::Arc<dyn Fn(AnswererEndpoint) -> std::sync::Arc<dyn Answerer> + Send + Sync>,
}

impl std::fmt::Debug for ApprovalService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalService")
            .field("domain", &self.domain)
            .finish_non_exhaustive()
    }
}

/// One answerer endpoint registration on the wire.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct AnswererEndpoint {
    /// The providing dougong service name.
    pub service: String,
    /// The method selector on that service.
    pub method: u32,
}

impl ApprovalService {
    /// Wraps the domain with the answerer factory the host link
    /// supplies (tests inject a scripted one).
    #[must_use]
    pub fn new(
        domain: Approval,
        answerer_factory: std::sync::Arc<
            dyn Fn(AnswererEndpoint) -> std::sync::Arc<dyn Answerer> + Send + Sync,
        >,
    ) -> Self {
        Self {
            domain,
            answerer_factory,
        }
    }

    /// The hosted domain (tests drive the folds directly).
    #[must_use]
    pub const fn domain(&self) -> &Approval {
        &self.domain
    }

    /// Serves one method invocation.
    ///
    /// # Errors
    ///
    /// Returns the failure text the host surfaces as `STATUS_FAILED`.
    pub fn serve(&self, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
        let reply = match method {
            METHOD_DESCRIBE => json!({
                "name": PLUGIN_NAME,
                "service": SERVICE_NAME,
                "abiVersion": dougong_abi::ABI_VERSION,
            }),
            METHOD_REQUEST => {
                let request: Value = serde_json::from_slice(payload)
                    .map_err(|error| format!("request payload must be JSON: {error}"))?;
                let ask = ApprovalAsk {
                    agent: word(&request, "agent")?,
                    tool_name: word(&request, "tool_name")?,
                    call_id: optional_word(&request, "call_id")?,
                    reason: optional_word(&request, "reason")?,
                    display_reason: request.get("display_reason").cloned(),
                };
                let outcome = self.domain.request(&ask)?;
                json!({ "outcome": outcome.as_str() })
            }
            METHOD_SET_POLICY => {
                let request: Value = serde_json::from_slice(payload)
                    .map_err(|error| format!("set-policy payload must be JSON: {error}"))?;
                let agent = word(&request, "agent")?;
                let policy = policy_word(&request, "policy")?;
                self.domain.set_policy(&agent, policy)?;
                json!({})
            }
            METHOD_OVERRIDE_OF => {
                let request: Value = serde_json::from_slice(payload)
                    .map_err(|error| format!("override-of payload must be JSON: {error}"))?;
                let session = word(&request, "session_id")?;
                self.domain
                    .override_of(&session)?
                    .map_or_else(|| json!({}), |policy| json!({ "policy": policy.as_str() }))
            }
            METHOD_REGISTER_ANSWERER => {
                let request: Value = serde_json::from_slice(payload)
                    .map_err(|error| format!("register-answerer payload must be JSON: {error}"))?;
                let endpoint: AnswererEndpoint = serde_json::from_value(
                    request
                        .get("endpoint")
                        .cloned()
                        .ok_or_else(|| "register-answerer requires an endpoint".to_owned())?,
                )
                .map_err(|error| format!("register-answerer endpoint decode: {error}"))?;
                let token = self
                    .domain
                    .register_answerer((self.answerer_factory)(endpoint));
                json!({ "token": token })
            }
            METHOD_UNREGISTER_ANSWERER => {
                let request: Value = serde_json::from_slice(payload).map_err(|error| {
                    format!("unregister-answerer payload must be JSON: {error}")
                })?;
                let token = request
                    .get("token")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "unregister-answerer requires a token".to_owned())?;
                self.domain.unregister_answerer(token)?;
                json!({})
            }
            METHOD_CONTEXT_TEXT => {
                let request: Value = serde_json::from_slice(payload)
                    .map_err(|error| format!("context-text payload must be JSON: {error}"))?;
                let agent = optional_word(&request, "agent")?;
                let text = self.domain.context_text(agent.as_deref())?;
                json!({ "text": text })
            }
            other => return Err(format!("unknown {PLUGIN_NAME} method {other}")),
        };
        serde_json::to_vec(&reply).map_err(|error| format!("approval reply encode failed: {error}"))
    }
}

/// Decodes one required `snake_case` word field.
fn word(request: &Value, key: &str) -> Result<String, String> {
    request.get(key).and_then(Value::as_str).map_or_else(
        || Err(format!("{key} must be a string")),
        |word| Ok(word.to_owned()),
    )
}

/// Decodes one optional word field (`null` and absence both mean none).
fn optional_word(request: &Value, key: &str) -> Result<Option<String>, String> {
    match request.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(word)) => Ok(Some(word.clone())),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

/// Decodes one policy word with the verbatim refusal outside the
/// vocabulary.
fn policy_word(request: &Value, key: &str) -> Result<ApprovalPolicy, String> {
    request
        .get(key)
        .and_then(Value::as_str)
        .and_then(ApprovalPolicy::parse)
        .ok_or_else(|| INVALID_POLICY.to_owned())
}

/// Normalizes one answerer reply word to the chain vocabulary.
///
/// `Ok` carries the final/delegate shapes; the `Err` arm is the rogue
/// word contained to the fail-closed outcome (the caller treats it as a
/// claim with that outcome, not a transport failure).
///
/// # Errors
///
/// Returns [`ApprovalOutcome::Unavailable`] for any word outside the
/// vocabulary except `delegate`.
pub fn normalize_answer(word: &str) -> Result<Answer, ApprovalOutcome> {
    match word {
        "delegate" => Ok(Answer::Delegate),
        other => Ok(Answer::Final(
            ApprovalOutcome::parse(other).unwrap_or(ApprovalOutcome::Unavailable),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AnswererEndpoint, ApprovalService, INVALID_POLICY, METHOD_CONTEXT_TEXT, METHOD_DESCRIBE,
        METHOD_OVERRIDE_OF, METHOD_REGISTER_ANSWERER, METHOD_REQUEST, METHOD_SET_POLICY,
        METHOD_UNREGISTER_ANSWERER, normalize_answer,
    };
    use crate::ApprovalAsk;
    use crate::{AgentInject, Answer, Approval, ApprovalOutcome, ApprovalPolicy, SessionLog};
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default)]
    struct FakeLog {
        events: Mutex<Vec<Value>>,
    }

    impl SessionLog for FakeLog {
        fn append(&self, _session: &str, type_name: &str, data: Value) -> Result<(), String> {
            self.events.lock().expect("lock").push(json!({
                "type": type_name, "data": data,
            }));
            Ok(())
        }

        fn read_all(&self, _session: &str) -> Result<Vec<Value>, String> {
            Ok(self.events.lock().expect("lock").clone())
        }
    }

    #[derive(Debug, Default)]
    struct FakeInject {
        queued: Mutex<Vec<Value>>,
    }

    impl AgentInject for FakeInject {
        fn inject(&self, _session: &str, message: Value) -> Result<(), String> {
            self.queued.lock().expect("lock").push(message);
            Ok(())
        }
    }

    fn service() -> (ApprovalService, Arc<FakeLog>) {
        let log = Arc::new(FakeLog::default());
        let domain = Approval::new(
            ApprovalPolicy::Ask,
            Arc::clone(&log) as Arc<dyn SessionLog>,
            Arc::new(FakeInject::default()) as Arc<dyn AgentInject>,
        );
        let factory = Arc::new(|_endpoint: super::AnswererEndpoint| {
            Arc::new(NoopAnswerer) as Arc<dyn crate::Answerer>
        });
        (ApprovalService::new(domain, factory), log)
    }

    /// A scripted factory product: endpoints are irrelevant to the
    /// token round trip under test.
    struct NoopAnswerer;

    impl crate::Answerer for NoopAnswerer {
        fn answer(&self, _ask: &ApprovalAsk) -> Result<crate::Answer, String> {
            Ok(crate::Answer::Delegate)
        }
    }

    fn call(service: &ApprovalService, method: u32, body: &Value) -> Result<Value, String> {
        let reply = service.serve(method, body.to_string().as_bytes())?;
        serde_json::from_slice(&reply).map_err(|error| error.to_string())
    }

    #[test]
    fn describe_reports_the_shell_metadata() {
        let (service, _log) = service();
        let reply = call(&service, METHOD_DESCRIBE, &json!({})).expect("describe");
        assert_eq!(reply["name"], "fr-approval");
        assert_eq!(reply["service"], "approval");
        assert_eq!(reply["abiVersion"], dougong_abi::ABI_VERSION);
    }

    #[test]
    fn request_round_trips_the_audit_ask() {
        let (service, _log) = service();
        let error = service
            .serve(METHOD_REQUEST, b"not json")
            .expect_err("bad json");
        assert!(error.contains("request payload must be JSON"));
        let error = call(
            &service,
            METHOD_REQUEST,
            &json!({ "tool_name": "fs_write" }),
        )
        .expect_err("missing agent");
        assert_eq!(error, "agent must be a string");

        let log = Arc::new(FakeLog::default());
        let domain = Approval::new(
            ApprovalPolicy::Ask,
            Arc::clone(&log) as Arc<dyn SessionLog>,
            Arc::new(FakeInject::default()) as Arc<dyn AgentInject>,
        );
        let factory = Arc::new(|_endpoint: AnswererEndpoint| {
            Arc::new(NoopAnswerer) as Arc<dyn crate::Answerer>
        });
        let service = ApprovalService::new(domain, factory);
        let _unused = log.append("s1", "turn/start", json!({ "turn": 1 }));
        let reply = call(
            &service,
            METHOD_REQUEST,
            &json!({
                "agent": "s1",
                "tool_name": "fs_write",
                "call_id": "call-9",
                "reason": "writes outside the workspace",
                "display_reason": { "en": "outside write" },
            }),
        )
        .expect("request");
        assert_eq!(reply["outcome"], "unavailable");
        let events = log.read_all("s1").expect("read");
        assert_eq!(events[1]["data"]["toolName"], "fs_write");
        assert_eq!(events[1]["data"]["callId"], "call-9");
        assert_eq!(events[1]["data"]["reason"], "writes outside the workspace");
        assert!(
            events[1]["data"].get("displayReason").is_none(),
            "display reason never persists"
        );
    }

    #[test]
    fn set_policy_decodes_the_vocabulary_and_refuses_verbatim() {
        let (service, _log) = service();
        let error = call(
            &service,
            METHOD_SET_POLICY,
            &json!({ "agent": "s1", "policy": "sometimes" }),
        )
        .expect_err("rogue word");
        assert_eq!(error, INVALID_POLICY);
        assert_eq!(
            call(
                &service,
                METHOD_SET_POLICY,
                &json!({ "agent": "s1", "policy": "never" })
            )
            .expect("switch"),
            json!({})
        );
    }

    #[test]
    fn override_of_reports_the_fold_or_absence() {
        let (service, log) = service();
        let reply =
            call(&service, METHOD_OVERRIDE_OF, &json!({ "session_id": "s1" })).expect("absent");
        assert_eq!(reply, json!({}));
        let _unused = log.append("s1", "approval/policy", json!({ "policy": "never" }));
        let reply =
            call(&service, METHOD_OVERRIDE_OF, &json!({ "session_id": "s1" })).expect("fold");
        assert_eq!(reply, json!({ "policy": "never" }));
    }

    #[test]
    fn context_text_round_trips() {
        let (service, _log) = service();
        let reply = call(&service, METHOD_CONTEXT_TEXT, &json!({})).expect("bare");
        assert_eq!(reply["text"], "");
        let reply =
            call(&service, METHOD_CONTEXT_TEXT, &json!({ "agent": "s1" })).expect("ask text");
        assert!(
            reply["text"]
                .as_str()
                .expect("text")
                .starts_with("Approval policy: ask.")
        );
    }

    #[test]
    fn answerer_registration_round_trips_tokens() {
        let (service, _log) = service();
        let reply = call(
            &service,
            METHOD_REGISTER_ANSWERER,
            &json!({ "endpoint": AnswererEndpoint {
                service: "acp-answers".to_owned(),
                method: 7,
            }}),
        )
        .expect("register");
        let token = reply["token"].as_u64().expect("token");
        assert_eq!(
            call(
                &service,
                METHOD_UNREGISTER_ANSWERER,
                &json!({ "token": token })
            )
            .expect("unregister"),
            json!({})
        );
        let error = call(
            &service,
            METHOD_UNREGISTER_ANSWERER,
            &json!({ "token": token }),
        )
        .expect_err("double unregister");
        assert_eq!(error, format!("unknown approval answerer token {token}"));
    }

    #[test]
    fn unknown_methods_refuse_with_text() {
        let (service, _log) = service();
        let error = service.serve(42, b"null").expect_err("unknown method");
        assert_eq!(error, "unknown fr-approval method 42");
    }

    #[test]
    fn rogue_answer_words_contain_to_unavailable() {
        assert_eq!(normalize_answer("delegate"), Ok(Answer::Delegate));
        assert_eq!(
            normalize_answer("allowed-once"),
            Ok(Answer::Final(ApprovalOutcome::AllowedOnce))
        );
        assert_eq!(
            normalize_answer("allowed-forever"),
            Ok(Answer::Final(ApprovalOutcome::Unavailable)),
            "a rogue word must normalize to the fail-closed outcome"
        );
    }
    #[test]
    fn service_face_constants_match_the_wire_table() {
        use fr_wire::services::approval as wire;
        assert_eq!(super::SERVICE_NAME, wire::SERVICE_NAME);
        assert_eq!(super::METHOD_DESCRIBE, wire::METHOD_DESCRIBE);
        assert_eq!(super::METHOD_REQUEST, wire::METHOD_REQUEST);
        assert_eq!(super::METHOD_SET_POLICY, wire::METHOD_SET_POLICY);
        assert_eq!(super::METHOD_OVERRIDE_OF, wire::METHOD_OVERRIDE_OF);
        assert_eq!(
            super::METHOD_REGISTER_ANSWERER,
            wire::METHOD_REGISTER_ANSWERER
        );
        assert_eq!(
            super::METHOD_UNREGISTER_ANSWERER,
            wire::METHOD_UNREGISTER_ANSWERER
        );
        assert_eq!(super::METHOD_CONTEXT_TEXT, wire::METHOD_CONTEXT_TEXT);
    }
}

// Rust guideline compliant 2026-10-28
