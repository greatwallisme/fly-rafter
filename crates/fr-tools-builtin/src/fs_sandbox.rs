// Rust guideline compliant 2026-10-30
//! The sandbox-escalation API shared by the `write` and `edit` tools
//! (dsh `tool-fs/src/sandbox.ts`).
//!
//! The per-call policy resolution, the advertised escalation fields,
//! and the denial-marker mapping all delegate the vocabulary and the
//! fail-closed approval sequence to [`fr_sandbox::escalation`] (the
//! same pieces the shell tools use), so bash and fs escalate
//! identically.
//!
//! The controller fact is the mounted backend's default mode
//! ([`crate::fs_domain::FsDomain::sandbox_mode`]; `None` advertises
//! nothing), resolved once at registration and carried as the
//! advertisement flag.

use serde_json::Value;

use crate::fs_types::FsError;
use crate::fs_types::FsErrorCode;
use crate::tools::BodyError;
use crate::tools::Seams;

/// The escalation schema fields for a mutating tool's `parameters`
/// (dsh `schemaFields`).
///
/// Call only under a confining backend: the enum pins the closed
/// target vocabulary; the strict-wider check happens per call at
/// execution.
#[must_use]
pub fn schema_fields() -> (Value, Value) {
    (
        serde_json::json!({
            "type": "string",
            "enum": ["workspace-write", "danger-full-access"],
            "description": fr_sandbox::escalation::sandbox_permissions_description("operation"),
        }),
        serde_json::json!({
            "type": "string",
            "description": "Required with sandbox_permissions: one sentence for the user \
        explaining why this exact file operation needs the wider access. \
        Use the language of the user’s current request."
        }),
    )
}

/// The approval bridge: the tool-layer approval seam folded into the
/// facet [`fr_sandbox::escalation::EscalationApprover`] consumes (every
/// transport failure resolves `unavailable` — the ask never throws).
struct FsApprover<'a>(&'a dyn crate::tools::ApprovalSeam);

impl fr_sandbox::escalation::EscalationApprover for FsApprover<'_> {
    fn request(
        &self,
        ask: &fr_sandbox::escalation::EscalationAsk,
    ) -> fr_sandbox::escalation::EscalationOutcome {
        let Ok(word) = self.0.request(ask) else {
            return fr_sandbox::escalation::EscalationOutcome::Unavailable;
        };
        fr_sandbox::escalation::EscalationOutcome::parse(&word)
            .unwrap_or(fr_sandbox::escalation::EscalationOutcome::Unavailable)
    }
}

/// The policy to stamp onto this mutation (dsh `resolvePolicy`).
///
/// An approved escalation grant — a strictly wider retry resolved
/// through the approval seam BEFORE anything executes — else the
/// session's standing mode. Repeating the standing mode requires no
/// approval. Validates the escalation argument pairing first; a call
/// carrying no escalation pair returns the standing policy untouched.
///
/// # Errors
///
/// The pairing refusals, the standing-policy resolution failure, the
/// unadvertised-escalation refusal, and the fail-closed approval
/// sequence — all as plain body errors (dsh plain `Error` throws).
///
/// # Panics
///
/// The escalation arm asserts the standing policy is present (the
/// unadvertised arm refuses first), so the checked unwrap cannot fire.
pub fn resolve_policy(
    seams: &dyn Seams,
    arguments: &Value,
    agent: Option<&str>,
    call_id: &str,
    tool_name: &str,
    advertised: bool,
) -> Result<Option<Value>, BodyError> {
    let requested = arguments
        .get("sandbox_permissions")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let justification = arguments
        .get("justification")
        .and_then(Value::as_str)
        .map(str::to_owned);
    fr_sandbox::escalation::validate_escalation_args(
        requested.as_deref(),
        justification.as_deref(),
    )
    .map_err(BodyError::plain)?;
    let standing = seams
        .policy()
        .resolve_policy(agent)
        .map_err(BodyError::plain)?;
    let (Some(requested), Some(justification)) = (requested, justification) else {
        return Ok(standing);
    };
    if !advertised || standing.is_none() {
        return Err(BodyError::plain(
            "sandbox_permissions is not available in this composition (no sandboxing \
filesystem to escalate)",
        ));
    }
    let standing = standing.expect("checked above");
    let effective = fr_sandbox::seam::SandboxMode::parse(
        standing
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )
    .ok_or_else(|| BodyError::plain("the standing policy mode is not a sandbox mode"))?;
    let approver = FsApprover(seams.approval());
    let approval = fr_sandbox::escalation::EscalationApproval {
        approver: Some(&approver),
        agent,
        call_id,
        tool_name,
    };
    let request = fr_sandbox::escalation::EscalationRequest {
        requested_mode: requested,
        justification,
        effective_mode: effective,
        subject: "operation",
    };
    let granted = fr_sandbox::escalation::approve_escalation(&request, &approval)
        .map_err(BodyError::plain)?;
    let mut merged = standing;
    if let Some(object) = merged.as_object_mut() {
        object.insert(
            "mode".to_owned(),
            Value::String(granted.as_str().to_owned()),
        );
    }
    Ok(Some(merged))
}

/// Map a failed mutation error for the model (dsh `mapError`).
///
/// An `FS_SANDBOX_DENIED` becomes the shared `[sandbox: …]` denial
/// marker plus the same-turn escalation hint, keeping the structured
/// `FS_SANDBOX_DENIED` code. Any other error passes through unchanged.
/// A denial only arises under a confining backend, whose call path
/// always resolves a policy first — the mode word comes from the
/// stamped policy (an absent word is the documented-unreachable arm and
/// falls back to the floor mode).
pub fn map_error(error: FsError, policy: Option<&Value>) -> FsError {
    if error.code != FsErrorCode::SandboxDenied {
        return error;
    }
    let mode = policy
        .and_then(|policy| policy.get("mode"))
        .and_then(Value::as_str)
        .and_then(fr_sandbox::seam::SandboxMode::parse)
        .unwrap_or(fr_sandbox::seam::SandboxMode::ReadOnly);
    FsError::new(
        FsErrorCode::SandboxDenied,
        format!(
            "{}\n{}",
            fr_sandbox::escalation::sandbox_denial_marker(mode),
            fr_sandbox::escalation::escalation_hint_marker("operation")
        ),
    )
}

/// Render the stable model-facing diagnostic for a guarded-mutation
/// failure (dsh `remediateFsError`).
///
/// `FS_NOT_OBSERVED` replaces the operation-specific text with one
/// path-aware reason and read remedy, `FS_STALE_VERSION` appends its
/// re-read remedy; both keep the code. Anything else passes through
/// untouched.
#[must_use]
pub fn remediate_fs_error(error: FsError, display_path: &str) -> FsError {
    match error.code {
        FsErrorCode::NotObserved => FsError::new(
            error.code,
            format!(
                "cannot modify \"{display_path}\": file has not been read — read the file, \
then retry"
            ),
        ),
        FsErrorCode::StaleVersion => FsError::new(
            error.code,
            format!("{} — re-read the file, then retry", error.message),
        ),
        _ => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The scripted seams: one standing policy value and one approval
    /// outcome word.
    struct ScriptSeams {
        policy: Option<Value>,
        approval_word: String,
    }

    impl crate::tools::PolicySeam for ScriptSeams {
        fn resolve_policy(&self, _session: Option<&str>) -> Result<Option<Value>, String> {
            Ok(self.policy.clone())
        }
    }

    impl crate::tools::ApprovalSeam for ScriptSeams {
        fn request(&self, _ask: &fr_sandbox::escalation::EscalationAsk) -> Result<String, String> {
            Ok(self.approval_word.clone())
        }
    }

    impl Seams for ScriptSeams {
        fn attachments(&self) -> Option<&dyn crate::tools::AttachmentSeam> {
            None
        }

        fn llm(&self) -> Option<&dyn crate::tools::LlmSeam> {
            None
        }

        fn route_header(&self) -> Option<&dyn crate::tools::RouteHeaderSeam> {
            None
        }

        fn policy(&self) -> &dyn crate::tools::PolicySeam {
            self
        }

        fn approval(&self) -> &dyn crate::tools::ApprovalSeam {
            self
        }

        fn shell_resolve(&self, _shell: &str, _request: &Value) -> Result<Value, String> {
            Err("unused".to_owned())
        }

        fn shell_start(
            &self,
            _shell: &str,
            _spec: &Value,
            _command: &str,
            _owner: Option<&str>,
            _call_id: &str,
            _root_call_id: &str,
            _escalation_modes: &[&str],
        ) -> Result<(String, String), String> {
            Err("unused".to_owned())
        }

        fn shell_result(&self, _job_id: &str) -> Result<Value, String> {
            Err("unused".to_owned())
        }

        fn jobs_wait(
            &self,
            _id: &str,
            _timeout_ms: f64,
            _caller: Option<&str>,
        ) -> Result<Value, String> {
            Err("unused".to_owned())
        }

        fn jobs_list(&self, _caller: Option<&str>) -> Result<Value, String> {
            Ok(json!([]))
        }

        fn jobs_get(&self, _id: &str, _caller: Option<&str>) -> Result<Value, String> {
            Err("unknown job".to_owned())
        }

        fn jobs_read(&self, _id: &str, _caller: Option<&str>) -> Result<Value, String> {
            Err("unused".to_owned())
        }

        fn jobs_kill(
            &self,
            _id: &str,
            _caller: Option<&str>,
            _reason: Option<&str>,
        ) -> Result<String, String> {
            Err("unused".to_owned())
        }

        fn jobs_remove(&self, _id: &str, _caller: Option<&str>) -> Result<(), String> {
            Err("unused".to_owned())
        }

        fn working_directory_ensure(&self, _agent: &str) -> Result<String, String> {
            Err("unused".to_owned())
        }

        fn shell_env_collect(
            &self,
            _agent: Option<&str>,
        ) -> Result<crate::shell_env::FrozenEnv, String> {
            Err("unused".to_owned())
        }

        fn subprocess_spawn(&self, _spec: &Value) -> Result<String, String> {
            Err("unused".to_owned())
        }

        fn subprocess_read(
            &self,
            _handle: &str,
            _stream: &str,
            _from: u64,
        ) -> Result<Value, String> {
            Err("unused".to_owned())
        }

        fn subprocess_wait(&self, _handle: &str, _timeout_ms: f64) -> Result<Value, String> {
            Err("unused".to_owned())
        }

        fn subprocess_resolve_executable(&self, _command: &str) -> Result<String, String> {
            Err("unused".to_owned())
        }

        fn spill_store_save_text(&self, _save: &Value) -> Result<Option<Value>, String> {
            Err("unused".to_owned())
        }
    }

    fn seams() -> ScriptSeams {
        ScriptSeams {
            policy: Some(json!({ "mode": "workspace-write", "workspace_root": "/ws" })),
            approval_word: "allowed-once".to_owned(),
        }
    }

    /// A bare call returns the standing policy; an approved escalation
    /// merges the granted mode into the standing policy.
    #[test]
    fn standing_passthrough_and_approved_merge() {
        let seams = seams();
        let bare =
            resolve_policy(&seams, &json!({}), Some("s1"), "c1", "write", true).expect("bare call");
        assert_eq!(bare, seams.policy);

        let escalated = resolve_policy(
            &seams,
            &json!({
                "sandbox_permissions": "danger-full-access",
                "justification": "the log lives outside the workspace",
            }),
            Some("s1"),
            "c1",
            "write",
            true,
        )
        .expect("granted")
        .expect("the escalated call stamps a policy");
        assert_eq!(escalated["mode"], "danger-full-access");
        assert_eq!(escalated["workspace_root"], "/ws");
    }

    /// The fail-closed arms: the pairing refusals ride verbatim, an
    /// unadvertised escalation refuses, a rejected ask refuses, and an
    /// unparseable standing mode refuses.
    #[test]
    fn refusal_arms_pin_the_texts() {
        let scripted = seams();
        let orphan = resolve_policy(
            &scripted,
            &json!({ "sandbox_permissions": "danger-full-access" }),
            Some("s1"),
            "c1",
            "write",
            true,
        )
        .expect_err("orphan refused");
        assert_eq!(
            orphan.message,
            "invalid escalation: sandbox_permissions requires a justification"
        );
        let unadvertised = resolve_policy(
            &scripted,
            &json!({
                "sandbox_permissions": "danger-full-access",
                "justification": "need it",
            }),
            Some("s1"),
            "c1",
            "write",
            false,
        )
        .expect_err("unadvertised refused");
        assert_eq!(
            unadvertised.message,
            "sandbox_permissions is not available in this composition (no sandboxing \
filesystem to escalate)"
        );
        let mut rejected = seams();
        rejected.approval_word = "rejected".to_owned();
        let error = resolve_policy(
            &rejected,
            &json!({
                "sandbox_permissions": "danger-full-access",
                "justification": "need it",
            }),
            Some("s1"),
            "c1",
            "write",
            true,
        )
        .expect_err("rejected");
        assert_eq!(
            error.message,
            "the user rejected escalating this operation to \"danger-full-access\"; it stays \
denied, so stop and explain instead of working around it"
        );
        let mut odd = seams();
        odd.policy = Some(json!({ "mode": "banana" }));
        let error = resolve_policy(
            &odd,
            &json!({
                "sandbox_permissions": "danger-full-access",
                "justification": "need it",
            }),
            Some("s1"),
            "c1",
            "write",
            true,
        )
        .expect_err("bad standing mode");
        assert_eq!(
            error.message,
            "the standing policy mode is not a sandbox mode"
        );
    }

    /// The denial marker mapping keeps the code and composes the two
    /// markers; other codes pass through untouched.
    #[test]
    fn denial_maps_and_others_pass_through() {
        let denied = FsError::new(
            FsErrorCode::SandboxDenied,
            "cannot write \"/x/a\": file access denied under workspace-write mode",
        );
        let mapped = map_error(denied, Some(&json!({ "mode": "workspace-write" })));
        assert_eq!(mapped.code, FsErrorCode::SandboxDenied);
        assert_eq!(
            mapped.message,
            "[sandbox: file access denied under workspace-write mode]\n\
[sandbox: escalation available — retry this exact operation once with sandbox_permissions \
(the narrowest wider mode that suffices) + justification; the approval prompt asks the user]"
        );
        let stale = FsError::new(FsErrorCode::StaleVersion, "changed");
        assert_eq!(map_error(stale, None).message, "changed");
    }

    /// The remediation rewrites the two guarded-mutation codes and
    /// preserves everything else.
    #[test]
    fn remediation_pins_the_two_rewrites() {
        let not_observed = FsError::new(FsErrorCode::NotObserved, "edit requires reading");
        let remediated = remediate_fs_error(not_observed, "/w/a.txt");
        assert_eq!(remediated.code, FsErrorCode::NotObserved);
        assert_eq!(
            remediated.message,
            "cannot modify \"/w/a.txt\": file has not been read — read the file, then retry"
        );
        let stale = FsError::new(
            FsErrorCode::StaleVersion,
            "cannot edit \"/w/a.txt\": file changed since it was read",
        );
        let remediated = remediate_fs_error(stale, "/w/a.txt");
        assert_eq!(remediated.code, FsErrorCode::StaleVersion);
        assert_eq!(
            remediated.message,
            "cannot edit \"/w/a.txt\": file changed since it was read — re-read the file, \
then retry"
        );
        let other = FsError::new(FsErrorCode::NotFound, "gone");
        assert_eq!(remediate_fs_error(other, "/w/a.txt").message, "gone");
    }

    /// The schema fields pin the operation-subject description and the
    /// closed enum.
    #[test]
    fn schema_fields_pin_the_pair() {
        let (permissions, justification) = schema_fields();
        assert_eq!(
            permissions["enum"],
            json!(["workspace-write", "danger-full-access"])
        );
        assert!(
            permissions["description"]
                .as_str()
                .expect("description")
                .starts_with(
                    "The narrowest wider sandbox mode for a one-shot retry of the exact \
operation"
                )
        );
        assert_eq!(
            justification["description"],
            "Required with sandbox_permissions: one sentence for the user explaining why this \
exact file operation needs the wider access. Use the language of the user’s current request."
        );
    }
}
