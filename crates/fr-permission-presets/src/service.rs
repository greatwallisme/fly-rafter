//! The `'permission-presets'` service face over the dougong dynamic
//! channel.
//!
//! Method map:
//! - 0 describe: `{}` — shell metadata echo;
//! - 1 catalog: `{}` — the process-level preset catalog (the live
//!   mode read feeds the composed default);
//! - 2 fold: `{header, inheritedEventCount, events}` → `{state}` — the
//!   `permissions` projection's pure folding endpoint (Q110′A′: the
//!   endpoint the session-projections relay calls; it makes no outbound
//!   requires).

#![forbid(unsafe_code)]

use std::sync::Arc;

use serde_json::Value;

use crate::{ApprovalSetPolicy, PermissionPresets, SandboxModeRead, SessionLog, fold_permissions};

/// Decodes one required string field.
fn word(request: &Value, key: &str) -> Result<String, String> {
    request.get(key).and_then(Value::as_str).map_or_else(
        || Err(format!("{key} must be a string")),
        |word| Ok(word.to_owned()),
    )
}

/// The provided-service name consumers call.
pub const SERVICE_NAME: &str = "permission-presets";

/// Method selector: shell metadata echo (`{}`).
pub const METHOD_DESCRIBE: u32 = 0;
/// Method selector: the process-level preset catalog (`{}`).
pub const METHOD_CATALOG: u32 = 1;
/// Method selector: the `permissions` projection's folding endpoint.
pub const METHOD_FOLD: u32 = 2;
/// Method selector: the `/permission` command's behavior face
/// (`{agent, rawInput}`).
pub const METHOD_COMMAND: u32 = 3;

/// The `'permission-presets'` service over one assembled domain and its
/// read seams.
#[derive(Clone)]
pub struct PresetService {
    domain: PermissionPresets,
    log: Arc<dyn SessionLog>,
    mode_read: Arc<dyn SandboxModeRead>,
    approval: Arc<dyn ApprovalSetPolicy>,
}

impl std::fmt::Debug for PresetService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PresetService")
            .field("domain", &self.domain)
            .finish_non_exhaustive()
    }
}

impl PresetService {
    /// Assembles the service over the validated domain and its seams.
    #[must_use]
    pub const fn new(
        domain: PermissionPresets,
        log: Arc<dyn SessionLog>,
        mode_read: Arc<dyn SandboxModeRead>,
        approval: Arc<dyn ApprovalSetPolicy>,
    ) -> Self {
        Self {
            domain,
            log,
            mode_read,
            approval,
        }
    }

    /// Serves one method invocation.
    ///
    /// # Errors
    ///
    /// Returns the failure text the host surfaces as `STATUS_FAILED`.
    pub fn serve(&self, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
        match method {
            METHOD_DESCRIBE => Ok(serde_json::to_vec(&serde_json::json!({
                "name": SERVICE_NAME,
                "abiVersion": dougong_abi::ABI_VERSION,
            }))
            .unwrap_or_default()),
            METHOD_CATALOG => {
                expect_empty(payload, "catalog")?;
                let catalog = self.domain.catalog(self.mode_read.as_ref())?;
                serde_json::to_vec(&catalog).map_err(|error| format!("catalog encode: {error}"))
            }
            METHOD_FOLD => {
                let request: Value = serde_json::from_slice(payload)
                    .map_err(|error| format!("fold payload must be JSON: {error}"))?;
                let events = request
                    .get("events")
                    .cloned()
                    .and_then(|events| events.as_array().cloned())
                    .ok_or_else(|| "fold payload requires an events array".to_owned())?;
                let state = fold_permissions(&events)?;
                Ok(serde_json::to_vec(&serde_json::json!({ "state": state })).unwrap_or_default())
            }
            METHOD_COMMAND => {
                let request: Value = serde_json::from_slice(payload)
                    .map_err(|error| format!("command payload must be JSON: {error}"))?;
                let agent = word(&request, "agent")?;
                let raw_input = request
                    .get("rawInput")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let outcome = self.domain.command_permission(
                    &agent,
                    raw_input,
                    self.log.as_ref(),
                    self.mode_read.as_ref(),
                    self.approval.as_ref(),
                )?;
                Ok(serde_json::to_vec(&serde_json::json!({
                    "kind": outcome.kind,
                    "text": outcome.text,
                }))
                .unwrap_or_default())
            }
            other => Err(format!("unknown permission-presets method {other}")),
        }
    }
}

/// Accepts the no-argument payload forms (empty bytes, `null`, or `{}`).
fn expect_empty(payload: &[u8], face: &str) -> Result<(), String> {
    let parsed: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(payload)
            .map_err(|error| format!("{face} payload must be JSON: {error}"))?
    };
    if parsed == Value::Null || parsed == serde_json::json!({}) {
        Ok(())
    } else {
        Err(format!("{face} payload must be empty"))
    }
}

#[cfg(test)]
mod tests {
    use super::{METHOD_CATALOG, METHOD_DESCRIBE, METHOD_FOLD, PresetService};
    use crate::{
        ApprovalPolicy, PermissionProjectionState, SandboxMode, SessionLog, baked_presets,
        fold_permissions,
    };
    use serde_json::{Value, json};
    use std::sync::Arc;

    struct EmptyLog;

    impl SessionLog for EmptyLog {
        fn append(&self, _session: &str, _type_name: &str, _data: Value) -> Result<(), String> {
            Err("no appends in this test".to_owned())
        }

        fn read_all(&self, _session: &str) -> Result<Vec<Value>, String> {
            Ok(Vec::new())
        }
    }

    struct FixedMode(SandboxMode);

    impl crate::SandboxModeRead for FixedMode {
        fn sandbox_mode(&self) -> Result<SandboxMode, String> {
            Ok(self.0)
        }
    }

    struct RecordingApproval;

    impl crate::ApprovalSetPolicy for RecordingApproval {
        fn set_policy(&self, _agent: &str, _policy: ApprovalPolicy) -> Result<(), String> {
            Err("no policy writes in this test".to_owned())
        }
    }

    fn service() -> PresetService {
        let domain = crate::PermissionPresets::try_new(
            baked_presets(),
            None,
            Some(&SandboxMode::WorkspaceWrite),
            ApprovalPolicy::Ask,
        )
        .expect("assembles");
        PresetService::new(
            domain,
            Arc::new(EmptyLog),
            Arc::new(FixedMode(SandboxMode::WorkspaceWrite)),
            Arc::new(RecordingApproval),
        )
    }

    fn call(service: &PresetService, method: u32, payload: &Value) -> Result<Value, String> {
        service
            .serve(method, payload.to_string().as_bytes())
            .and_then(|reply| serde_json::from_slice(&reply).map_err(|error| error.to_string()))
    }

    #[test]
    fn wire_constants_match_the_fr_wire_table() {
        assert_eq!(
            super::SERVICE_NAME,
            fr_wire::services::permission_presets::SERVICE_NAME
        );
        assert_eq!(
            super::METHOD_DESCRIBE,
            fr_wire::services::permission_presets::METHOD_DESCRIBE
        );
        assert_eq!(
            super::METHOD_CATALOG,
            fr_wire::services::permission_presets::METHOD_CATALOG
        );
        assert_eq!(
            super::METHOD_FOLD,
            fr_wire::services::permission_presets::METHOD_FOLD
        );
        assert_eq!(
            super::METHOD_COMMAND,
            fr_wire::services::permission_presets::METHOD_COMMAND
        );
    }

    #[test]
    fn describe_and_catalog_answer_their_faces() {
        let service = service();
        let reply = call(&service, METHOD_DESCRIBE, &json!({})).expect("describe");
        assert_eq!(reply["name"], "permission-presets");
        assert_eq!(reply["abiVersion"], dougong_abi::ABI_VERSION);

        let reply = call(&service, METHOD_CATALOG, &json!({})).expect("catalog");
        assert_eq!(reply["default_preset"], "workspace-write");
        assert_eq!(reply["options"].as_array().expect("options").len(), 3);
        assert_eq!(
            reply["options"][0],
            json!({ "value": "read-only", "name": "read-only" })
        );
    }

    #[test]
    fn fold_serves_the_projection_state() {
        let service = service();
        let payload = json!({
            "header": { "id": "s1" },
            "inheritedEventCount": 0,
            "events": [
                { "type": "permission/preset", "data": { "preset": "read-only" } },
                { "type": "session/end-seed", "data": {} },
            ],
        });
        let reply = call(&service, METHOD_FOLD, &payload).expect("fold");
        let expected = fold_permissions(&payload["events"].as_array().cloned().unwrap_or_default())
            .expect("same fold");
        assert_eq!(
            reply["state"],
            serde_json::to_value(expected).expect("json")
        );
        let state: PermissionProjectionState =
            serde_json::from_value(reply["state"].clone()).expect("state decodes");
        assert_eq!(state.knobs.preset.as_deref(), Some("read-only"));
        assert!(state.seeded);
    }

    #[test]
    fn the_command_face_settles_over_the_seams() {
        let service = service();
        let payload = json!({ "agent": "s1", "rawInput": "  " });
        let reply = call(&service, super::METHOD_COMMAND, &payload).expect("command");
        assert_eq!(reply["kind"], "success");
        assert_eq!(
            reply["text"],
            "current preset workspace-write (available: read-only, workspace-write, danger-full-access)"
        );
    }

    #[test]
    fn malformed_payloads_and_unknown_methods_fail_with_text() {
        let service = service();
        assert!(
            service
                .serve(METHOD_FOLD, b"not json")
                .expect_err("bad json")
                .contains("fold payload must be JSON")
        );
        assert_eq!(
            service.serve(4, b"{}").expect_err("unknown method"),
            "unknown permission-presets method 4"
        );
    }
}

// Rust guideline compliant 2026-10-29
