// Rust guideline compliant 2026-10-29
//! The `'sandbox-policy'` service face (methods 0-4) and the `'sandbox'`
//! confinement seam face (the 1000+ band, one export table — the
//! session-projections band judgment) over the domain.

use std::sync::Arc;

use serde_json::Value;
use serde_json::json;

use crate::policy::PolicyHome;
use crate::policy::ResolveRequest;
use crate::policy::SessionFacts;
use crate::policy::fold_sandbox_mode;
use crate::policy::render_policy_context;
use crate::provider::ConfineFailure;
use crate::provider::LocalSandboxProvider;
use crate::seam::SandboxMode;

/// The `'sandbox-policy'` provided-service name (fr-wire equality pin).
pub const SERVICE_NAME: &str = fr_wire::services::sandbox_policy::SERVICE_NAME;
/// The `'sandbox'` seam provided-service name (fr-wire equality pin).
pub const SEAM_SERVICE_NAME: &str = fr_wire::services::sandbox::SERVICE_NAME;

/// The `'sandbox-policy'` face's shell metadata echo.
pub const METHOD_DESCRIBE: u32 = fr_wire::services::sandbox_policy::METHOD_DESCRIBE;
/// The deployment's default sandbox mode (`{}` → `{mode}`).
pub const METHOD_SANDBOX_MODE: u32 = fr_wire::services::sandbox_policy::METHOD_SANDBOX_MODE;
/// One capability call's complete policy resolution.
pub const METHOD_RESOLVE: u32 = fr_wire::services::sandbox_policy::METHOD_RESOLVE;
/// The `sandbox:policy` runtime-context endpoint.
pub const METHOD_CONTEXT_TEXT: u32 = fr_wire::services::sandbox_policy::METHOD_CONTEXT_TEXT;
/// The `sandboxMode` projection's pure folding endpoint.
pub const METHOD_FOLD: u32 = fr_wire::services::sandbox_policy::METHOD_FOLD;

/// The `'sandbox'` seam face's shell metadata echo.
pub const METHOD_SEAM_DESCRIBE: u32 = fr_wire::services::sandbox::METHOD_DESCRIBE;
/// The `confine` wrap (Q100′A seam; Phase 16 consumers).
pub const METHOD_CONFINE: u32 = fr_wire::services::sandbox::METHOD_CONFINE;

/// The refusal prefix carrying [`crate::seam::SANDBOX_UNAVAILABLE`]
/// through the `STATUS_FAILED` text channel so tool layers can
/// distinguish a missing backend from a command failure.
pub const SANDBOX_UNAVAILABLE_PREFIX: &str = "SANDBOX_UNAVAILABLE: ";

/// The session-facts read seam behind the service.
///
/// One session read returning the header cwd plus the full event log
/// (the self-read judgment — the tail-pull feeds the local fold, never
/// a reentrant stateOf).
pub trait SessionFactsRead: Send + Sync {
    /// Reads one session's cwd and events.
    ///
    /// # Errors
    ///
    /// Returns the session read refusal text.
    fn facts(&self, session: &str) -> Result<(Option<String>, Vec<Value>), String>;
}

/// The `'sandbox-policy'` service over the policy home.
pub struct SandboxPolicyService {
    home: PolicyHome,
    sessions: Arc<dyn SessionFactsRead>,
}

impl std::fmt::Debug for SandboxPolicyService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxPolicyService")
            .field("home", &self.home)
            .finish_non_exhaustive()
    }
}

impl SandboxPolicyService {
    /// Builds the service over one policy home and the session-facts
    /// seam.
    #[must_use]
    pub fn new(home: PolicyHome, sessions: Arc<dyn SessionFactsRead>) -> Self {
        Self { home, sessions }
    }

    /// The deployment default mode (the capability fact consumers
    /// advertise).
    #[must_use]
    pub const fn default_mode(&self) -> SandboxMode {
        self.home.default_mode
    }

    fn optional_mode_word(request: &Value) -> Result<Option<SandboxMode>, String> {
        match request.get("mode") {
            None | Some(Value::Null) => Ok(None),
            Some(word) => word
                .as_str()
                .and_then(SandboxMode::parse)
                .map(Some)
                .ok_or_else(|| format!("sandbox-policy: unknown mode word in request: {word}")),
        }
    }

    fn optional_session_word(request: &Value) -> Result<Option<String>, String> {
        match request.get("session") {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(id)) => Ok(Some(id.clone())),
            Some(other) => Err(format!(
                "sandbox-policy: session must be a session id word, got {other}"
            )),
        }
    }

    /// Resolves one request into the session facts + resolution.
    fn resolve_request(&self, request: &ResolveRequest) -> Result<Value, String> {
        let policy = self.home.resolve(request)?;
        let mut reply = json!({
            "mode": policy.mode.as_str(),
            "workspace_root": policy.workspace_root,
        });
        if let Some(session_id) = policy.session_id {
            reply["session_id"] = json!(session_id);
        }
        Ok(reply)
    }

    /// Serves one `'sandbox-policy'` method invocation.
    ///
    /// # Errors
    ///
    /// Returns the failure the host surfaces as `STATUS_FAILED`.
    pub fn serve(&self, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
        match method {
            METHOD_DESCRIBE => Ok(json!({
                "plugin": "fr-sandbox",
                "service": SERVICE_NAME,
                "methods": [
                    "describe", "sandbox-mode", "resolve", "context-text", "fold",
                ],
            })
            .to_string()
            .into_bytes()),
            METHOD_SANDBOX_MODE => Ok(json!({ "mode": self.home.default_mode.as_str() })
                .to_string()
                .into_bytes()),
            METHOD_RESOLVE => {
                let request: Value = serde_json::from_slice(payload)
                    .map_err(|error| format!("resolve payload must be JSON: {error}"))?;
                let mode = Self::optional_mode_word(&request)?;
                let session = Self::optional_session_word(&request)?;
                let facts = match &session {
                    Some(id) => {
                        let (cwd, events) = self.sessions.facts(id)?;
                        Some((
                            id.clone(),
                            SessionFacts {
                                cwd,
                                override_mode: fold_sandbox_mode(&events)?,
                            },
                        ))
                    }
                    None => None,
                };
                let reply = self.resolve_request(&ResolveRequest {
                    session: facts,
                    mode,
                })?;
                Ok(reply.to_string().into_bytes())
            }
            METHOD_CONTEXT_TEXT => {
                let request: Value = serde_json::from_slice(payload)
                    .map_err(|error| format!("context-text payload must be JSON: {error}"))?;
                let agent = match request.get("agent") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(agent)) => Some(agent.clone()),
                    Some(other) => {
                        return Err(format!(
                            "sandbox-policy: agent must be a session id word, got {other}"
                        ));
                    }
                };
                let Some(agent) = agent else {
                    // A bare context assembly (no agent) states nothing.
                    return Ok(json!({ "text": "" }).to_string().into_bytes());
                };
                let (cwd, events) = self.sessions.facts(&agent)?;
                let text = render_policy_context(&self.home.resolve(&ResolveRequest {
                    session: Some((
                        agent,
                        SessionFacts {
                            cwd,
                            override_mode: fold_sandbox_mode(&events)?,
                        },
                    )),
                    mode: None,
                })?);
                Ok(json!({ "text": text }).to_string().into_bytes())
            }
            METHOD_FOLD => {
                let request: Value = serde_json::from_slice(payload)
                    .map_err(|error| format!("fold payload must be JSON: {error}"))?;
                let events = request
                    .get("events")
                    .and_then(Value::as_array)
                    .cloned()
                    .ok_or_else(|| {
                        "sandbox-policy fold payload requires an events array".to_owned()
                    })?;
                let state = fold_sandbox_mode(&events)?;
                Ok(
                    json!({ "state": state.map(super::seam::SandboxMode::as_str) })
                        .to_string()
                        .into_bytes(),
                )
            }
            other => Err(format!("unknown {SERVICE_NAME} method {other}")),
        }
    }
}

/// Serves one `'sandbox'` seam method invocation against the provider.
///
/// # Errors
///
/// Returns the failure the host surfaces as `STATUS_FAILED`; a missing
/// backend carries the [`SANDBOX_UNAVAILABLE_PREFIX`] so tool layers
/// can classify it.
pub fn serve_seam(
    provider: &LocalSandboxProvider,
    method: u32,
    payload: &[u8],
) -> Result<Vec<u8>, String> {
    match method {
        METHOD_SEAM_DESCRIBE => Ok(json!({
            "plugin": "fr-sandbox",
            "service": SEAM_SERVICE_NAME,
            "methods": ["describe", "confine"],
        })
        .to_string()
        .into_bytes()),
        METHOD_CONFINE => {
            let request: Value = serde_json::from_slice(payload)
                .map_err(|error| format!("confine payload must be JSON: {error}"))?;
            let argv: Vec<String> = request
                .get("argv")
                .and_then(Value::as_array)
                .map(|words| {
                    words
                        .iter()
                        .map(|word| {
                            word.as_str().map(str::to_owned).ok_or_else(|| {
                                "sandbox confine argv entries must be strings".to_owned()
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?
                .ok_or_else(|| "sandbox confine payload requires an argv array".to_owned())?;
            let policy: crate::seam::SandboxPolicy = serde_json::from_value(
                request
                    .get("policy")
                    .cloned()
                    .ok_or_else(|| "sandbox confine payload requires a policy".to_owned())?,
            )
            .map_err(|error| format!("sandbox confine policy decode failed: {error}"))?;
            match provider.confine(&argv, &policy) {
                Ok(confined) => serde_json::to_vec(&confined)
                    .map_err(|error| format!("sandbox confine reply encode failed: {error}")),
                Err(ConfineFailure::Unavailable(error)) => {
                    Err(format!("{SANDBOX_UNAVAILABLE_PREFIX}{error}"))
                }
                Err(ConfineFailure::Unsupported(reason)) => Err(reason),
            }
        }
        other => Err(format!("unknown {SEAM_SERVICE_NAME} method {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        METHOD_CONTEXT_TEXT, METHOD_DESCRIBE, METHOD_FOLD, METHOD_RESOLVE, METHOD_SANDBOX_MODE,
        SANDBOX_UNAVAILABLE_PREFIX, SERVICE_NAME, SandboxPolicyService, SessionFactsRead,
        serve_seam,
    };
    use crate::policy::PolicyConfig;
    use crate::policy::PolicyHome;
    use crate::provider::ProviderConfig;
    use crate::provider::SandboxInternals;
    use crate::seam::SandboxMode;
    use serde_json::{Value, json};
    use std::sync::Arc;
    use std::sync::Mutex;

    /// One fake session's log: id, header cwd, events.
    type FakeLog = (String, Option<String>, Vec<Value>);

    struct FakeFacts {
        logs: Mutex<Vec<FakeLog>>,
    }

    impl SessionFactsRead for FakeFacts {
        fn facts(&self, session: &str) -> Result<(Option<String>, Vec<Value>), String> {
            let found = {
                let logs = self.logs.lock().expect("lock");
                logs.iter().rev().find(|(id, _, _)| id == session).cloned()
            };
            let Some((_, cwd, events)) = found else {
                return Err(format!(
                    "session read range 0..1 is outside the log for {session}"
                ));
            };
            Ok((cwd, events))
        }
    }

    fn service_with(
        mode: SandboxMode,
        root: &str,
        logs: Vec<(&str, Option<&str>, Vec<Value>)>,
    ) -> SandboxPolicyService {
        let home = PolicyHome::new(&PolicyConfig {
            mode,
            workspace_root: Some(root.to_owned()),
        })
        .expect("home");
        let logs = logs
            .into_iter()
            .map(|(id, cwd, events)| (id.to_owned(), cwd.map(str::to_owned), events))
            .collect();
        SandboxPolicyService::new(
            home,
            Arc::new(FakeFacts {
                logs: Mutex::new(logs),
            }),
        )
    }

    fn call(service: &SandboxPolicyService, method: u32, payload: &Value) -> Result<Value, String> {
        service
            .serve(method, &payload.to_string().into_bytes())
            .map(|reply| serde_json::from_slice(&reply).expect("reply json"))
    }

    #[test]
    fn the_face_pins_the_fr_wire_selectors() {
        assert_eq!(
            METHOD_DESCRIBE,
            fr_wire::services::sandbox_policy::METHOD_DESCRIBE
        );
        assert_eq!(
            METHOD_SANDBOX_MODE,
            fr_wire::services::sandbox_policy::METHOD_SANDBOX_MODE
        );
        assert_eq!(
            METHOD_RESOLVE,
            fr_wire::services::sandbox_policy::METHOD_RESOLVE
        );
        assert_eq!(
            METHOD_CONTEXT_TEXT,
            fr_wire::services::sandbox_policy::METHOD_CONTEXT_TEXT
        );
        assert_eq!(METHOD_FOLD, fr_wire::services::sandbox_policy::METHOD_FOLD);
        assert_eq!(SERVICE_NAME, "sandbox-policy");
    }

    #[test]
    fn sandbox_mode_reports_the_deployment_default() {
        let service = service_with(SandboxMode::ReadOnly, "/fallback", Vec::new());
        assert_eq!(
            call(&service, METHOD_SANDBOX_MODE, &json!({})).expect("mode"),
            json!({ "mode": "read-only" })
        );
    }

    #[test]
    fn resolve_ladders_the_three_sources() {
        let events = vec![
            json!({ "type": "session/start", "data": {} }),
            json!({ "type": "sandbox/mode", "data": { "mode": "read-only" } }),
        ];
        let service = service_with(
            SandboxMode::WorkspaceWrite,
            "/fallback",
            vec![("s1", Some("/ws"), events)],
        );
        // Agentless: the default and the fallback root.
        assert_eq!(
            call(&service, METHOD_RESOLVE, &json!({})).expect("bare"),
            json!({ "mode": "workspace-write", "workspace_root": "/fallback" })
        );
        // The session override (read-only) outranks the default; its cwd
        // wins the root and the session id rides along.
        assert_eq!(
            call(&service, METHOD_RESOLVE, &json!({ "session": "s1" })).expect("session"),
            json!({ "mode": "read-only", "workspace_root": "/ws", "session_id": "s1" })
        );
        // An approved explicit mode outranks the override.
        assert_eq!(
            call(
                &service,
                METHOD_RESOLVE,
                &json!({ "session": "s1", "mode": "danger-full-access" })
            )
            .expect("request"),
            json!({
                "mode": "danger-full-access",
                "workspace_root": "/ws",
                "session_id": "s1"
            })
        );
        // An unknown session refuses through the read seam.
        call(&service, METHOD_RESOLVE, &json!({ "session": "gone" })).unwrap_err();
        // An unknown mode word refuses at the decode face.
        call(&service, METHOD_RESOLVE, &json!({ "mode": "root" })).unwrap_err();
    }

    #[test]
    fn context_text_renders_the_resolved_policy_or_nothing() {
        let service = service_with(SandboxMode::WorkspaceWrite, "/fallback", Vec::new());
        let bare = call(&service, METHOD_CONTEXT_TEXT, &json!({})).expect("bare");
        assert_eq!(bare, json!({ "text": "" }));
        let service = service_with(
            SandboxMode::WorkspaceWrite,
            "/fallback",
            vec![("s1", Some("/ws"), vec![])],
        );
        let text = call(&service, METHOD_CONTEXT_TEXT, &json!({ "agent": "s1" }))
            .expect("agent text")
            .get("text")
            .and_then(Value::as_str)
            .expect("text word")
            .to_owned();
        assert_eq!(
            text,
            "Current DSH file policy: workspace-write. Any available operation enforced by the \
DSH file sandbox may modify files under the session workspace: \"/ws\". Some platform \
temporary areas may also be writable."
        );
    }

    #[test]
    fn fold_reduces_the_full_log_to_the_last_mode() {
        let service = service_with(SandboxMode::ReadOnly, "/fallback", Vec::new());
        let reply = call(
            &service,
            METHOD_FOLD,
            &json!({
                "header": { "cwd": "/ws" },
                "inheritedEventCount": 0,
                "events": [
                    { "type": "sandbox/mode", "data": { "mode": "workspace-write" } },
                    { "type": "approval/policy", "data": { "policy": "ask" } },
                    { "type": "sandbox/mode", "data": { "mode": "read-only" } },
                ],
            }),
        )
        .expect("fold");
        assert_eq!(reply, json!({ "state": "read-only" }));
        let empty = call(
            &service,
            METHOD_FOLD,
            &json!({ "header": { "cwd": "/ws" }, "inheritedEventCount": 0, "events": [] }),
        )
        .expect("empty fold");
        assert_eq!(empty, json!({ "state": null }));
        // The dsh sandboxMode projection's stateVersion stays pinned.
        assert_eq!(crate::policy::SANDBOX_MODE_EVENT, "sandbox/mode");
    }

    #[test]
    fn the_seam_face_wraps_or_fails_closed_with_the_code() {
        let provider = crate::provider::LocalSandboxProvider::new(
            &ProviderConfig::default(),
            SandboxInternals {
                platform: Some("sunos".to_owned()),
                ..SandboxInternals::default()
            },
        )
        .expect("provider");
        let refusal = serve_seam(
            &provider,
            super::METHOD_CONFINE,
            br#"{"argv":["bash","-c","true"],"policy":{"mode":"read-only","workspace_root":"/ws"}}"#,
        )
        .unwrap_err();
        assert!(
            refusal.starts_with(SANDBOX_UNAVAILABLE_PREFIX),
            "the refusal carries the code prefix: {refusal}"
        );
        assert!(refusal.contains("no sandbox backend is usable"));
    }

    #[test]
    fn unknown_methods_refuse_with_the_service_name() {
        let service = service_with(SandboxMode::ReadOnly, "/fallback", Vec::new());
        assert_eq!(
            service.serve(42, b"{}").unwrap_err(),
            format!("unknown {SERVICE_NAME} method 42")
        );
    }
}
