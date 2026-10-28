// Rust guideline compliant 2026-10-29
//! The escalation vocabulary and choreography (dsh
//! `sandbox/src/escalation.ts`, verbatim semantics).
//!
//! The strictly-wider ladder, the argument-pairing validation, the
//! model-facing denial/hint markers, and [`approve_escalation`] — the
//! ordered fail-closed sequence that resolves a `sandbox_permissions`
//! request through a user-approval channel BEFORE anything executes. One
//! home keeps the enforcing families' approval ordering and verbatim
//! error texts from drifting apart.
//!
//! The approval channel is a minimal STRUCTURAL trait shape
//! ([`EscalationApprover`]), not the approval service type: the tool
//! layer — which owns the agent, the call id, and the tool name — closes
//! over the approval service and hands the implementation down, so this
//! module never depends on the approval crate.

use crate::seam::SandboxMode;

/// The strictly-wider table: what a call whose effective mode is the key may escalate TO (dsh `WIDER_MODES`).
///
/// Checked at execution, never
/// baked into a tool schema — the schema's enum is
/// [`ESCALATION_TARGETS`], because schemas are registry-global while the
/// effective mode is per-call truth.
#[must_use]
pub const fn wider_modes(mode: SandboxMode) -> &'static [SandboxMode] {
    match mode {
        SandboxMode::ReadOnly => &[SandboxMode::WorkspaceWrite, SandboxMode::DangerFullAccess],
        SandboxMode::WorkspaceWrite => &[SandboxMode::DangerFullAccess],
        SandboxMode::DangerFullAccess => &[],
    }
}

/// The closed escalation-target vocabulary — every mode a call could
/// ever escalate TO (`read-only` is the floor; nothing escalates to it).
/// Advertised whenever the mounted capability confines.
pub const ESCALATION_TARGETS: [SandboxMode; 2] =
    [SandboxMode::WorkspaceWrite, SandboxMode::DangerFullAccess];

/// Validates the escalation argument pairing a tool schema cannot
/// express (dsh `validateEscalationArgs`).
///
/// `sandbox_permissions` and `justification` travel together — an
/// approval prompt without a reason, or a reason driving nothing, is a
/// malformed ask — and the justification must be a non-empty sentence.
///
/// # Errors
///
/// Returns the verbatim dsh refusal for the first violated pairing.
pub fn validate_escalation_args(
    sandbox_permissions: Option<&str>,
    justification: Option<&str>,
) -> Result<(), String> {
    if sandbox_permissions.is_some() && justification.is_none() {
        return Err("invalid escalation: sandbox_permissions requires a justification".to_owned());
    }
    if justification.is_some() && sandbox_permissions.is_none() {
        return Err(
            "invalid escalation: justification is only valid together with sandbox_permissions"
                .to_owned(),
        );
    }
    if let Some(justification) = justification
        && justification.trim().is_empty()
    {
        return Err("invalid justification: expected a non-empty sentence".to_owned());
    }
    Ok(())
}

/// The model-facing denial marker (dsh `sandboxDenialMarker`).
///
/// The one vocabulary both enforcing families teach and report, so the
/// model recognizes a policy denial identically whether the kernel
/// refused a bash file effect or the filesystem provider's fence
/// refused a mutation.
#[must_use]
pub fn sandbox_denial_marker(mode: SandboxMode) -> String {
    format!("[sandbox: file access denied under {} mode]", mode.as_str())
}

/// The same-turn escalation hint that rides a denial when the
/// composition advertises the escalation fields (dsh
/// `escalationHintMarker`).
///
/// The nudge lives at the decision point so the sanctioned retry does
/// not depend on the model recalling the tool description.
#[must_use]
pub fn escalation_hint_marker(subject: &str) -> String {
    format!(
        "[sandbox: escalation available — retry this exact {subject} once with sandbox_permissions \
(the narrowest wider mode that suffices) + justification; the approval prompt asks the user]"
    )
}

/// The model-facing `sandbox_permissions` parameter description (dsh
/// `sandboxPermissionsDescription`), which carries the escalation rules
/// for every enforcing family.
#[must_use]
pub fn sandbox_permissions_description(subject: &str) -> String {
    format!(
        "The narrowest wider sandbox mode for a one-shot retry of the exact {subject} the sandbox \
just denied; the retry asks the user for approval."
    )
}

/// The closed outcome vocabulary of one escalation ask (dsh
/// `EscalationOutcome`) — structurally identical to the approval seam's
/// outcome so an approval service return maps without this crate
/// importing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationOutcome {
    /// The human approved this one escalation.
    AllowedOnce,
    /// The human rejected the escalation.
    Rejected,
    /// The ask was cancelled before an answer.
    Cancelled,
    /// No approval channel produced an answer.
    Unavailable,
}

impl EscalationOutcome {
    /// Parses the wire vocabulary; `None` outside it.
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

    /// The wire word for this outcome.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AllowedOnce => "allowed-once",
            Self::Rejected => "rejected",
            Self::Cancelled => "cancelled",
            Self::Unavailable => "unavailable",
        }
    }
}

/// One localized display-reason bundle (dsh `displayReason`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayReason {
    /// The English sentence shown to the user.
    pub en: String,
    /// The Chinese sentence shown to the user.
    pub zh: String,
}

/// The approval request shape [`approve_escalation`] sends (the audit
/// request minus the structural generics — the wire face carries these
/// as plain words).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalationAsk {
    /// The calling agent's session identity.
    pub agent: String,
    /// The tool name recorded on the approval request.
    pub tool_name: String,
    /// The tool-call id the approval prompt attaches to.
    pub call_id: String,
    /// The audit reason (verbatim, part of the grant's identity).
    pub reason: String,
    /// The localized display sentences for the prompt.
    pub display_reason: DisplayReason,
}

/// The minimal approval-request channel [`approve_escalation`] needs (dsh `EscalationApprover`): structurally the approval seam's request face.
///
/// The implementation resolves every failure to
/// [`EscalationOutcome::Unavailable`] — an ask never throws.
pub trait EscalationApprover: Send + Sync {
    /// Asks the human to approve one action, resolving to a closed
    /// outcome.
    fn request(&self, ask: &EscalationAsk) -> EscalationOutcome;
}

/// The approval ingredients an escalating tool hands
/// [`approve_escalation`] (dsh `EscalationApproval`).
///
/// The approval channel (or `None` when none is composed), the calling
/// agent (or `None` for an agent-less execution), and the call's
/// identity.
#[derive(Clone, Copy)]
pub struct EscalationApproval<'a> {
    /// The approval channel, or `None` when none is composed.
    pub approver: Option<&'a dyn EscalationApprover>,
    /// The calling agent's session identity, or `None` for an agent-less
    /// execution (fails closed).
    pub agent: Option<&'a str>,
    /// The tool-call id the approval prompt attaches to.
    pub call_id: &'a str,
    /// The tool name recorded on the approval request.
    pub tool_name: &'a str,
}

impl std::fmt::Debug for EscalationApproval<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EscalationApproval")
            .field("approver", &self.approver.is_some())
            .field("agent", &self.agent)
            .field("call_id", &self.call_id)
            .field("tool_name", &self.tool_name)
            .finish()
    }
}

/// One escalation request, as [`approve_escalation`] judges it (dsh
/// `EscalationRequest`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalationRequest {
    /// The requested target mode (schema-pinned to
    /// [`ESCALATION_TARGETS`] when advertised).
    pub requested_mode: String,
    /// The model's one-sentence reason, shown verbatim to the user
    /// inside the audit reason.
    pub justification: String,
    /// The call's effective mode (session override ?? composition
    /// default); repeating it needs no approval.
    pub effective_mode: SandboxMode,
    /// The family's noun for the escalated action in user-facing texts
    /// (`command` for bash, `operation` for fs).
    pub subject: &'static str,
}

/// Resolves a sandbox permission request before execution (dsh `approveEscalation`, verbatim refusals).
///
/// Repeating the call's
/// effective mode returns it without approval. A strictly wider mode
/// requires approval and applies only to this call. Narrower or
/// unsupported targets, missing approval services or agents for
/// widening, and non-grant outcomes throw before execution.
///
/// # Errors
///
/// Returns the verbatim dsh refusal for the failed arm; the granted mode
/// is the success value, consumed by the one call that asked.
pub fn approve_escalation(
    request: &EscalationRequest,
    approval: &EscalationApproval<'_>,
) -> Result<SandboxMode, String> {
    let mode_word = request.requested_mode.as_str();
    let requested = SandboxMode::parse(mode_word);
    if Some(request.effective_mode) == requested {
        return Ok(request.effective_mode);
    }
    // Strict widening is an execution check against the call's effective
    // mode — deliberately not a schema constraint (the enum is the
    // closed target vocabulary; the effective mode is per-call truth).
    let granted_mode = match requested {
        Some(mode) if wider_modes(request.effective_mode).contains(&mode) => mode,
        _ => {
            return Err(format!(
                "sandbox escalation to \"{mode_word}\" is not strictly wider than this call's \
current \"{}\" mode",
                request.effective_mode.as_str()
            ));
        }
    };
    let Some(approver) = approval.approver else {
        return Err(format!(
            "sandbox escalation to \"{mode_word}\" requires approval, but no approval service is \
composed"
        ));
    };
    let Some(agent) = approval.agent else {
        return Err(format!(
            "sandbox escalation to \"{mode_word}\" requires approval, but the call has no agent \
to route it through"
        ));
    };
    let outcome = approver.request(&EscalationAsk {
        agent: agent.to_owned(),
        tool_name: approval.tool_name.to_owned(),
        call_id: approval.call_id.to_owned(),
        reason: format!(
            "escalate sandbox to {}: {}",
            granted_mode.as_str(),
            request.justification
        ),
        display_reason: DisplayReason {
            en: format!(
                "Allow this operation with {} permissions: {}",
                granted_mode.as_str(),
                request.justification
            ),
            zh: format!(
                "允许本次操作使用 {} 权限：{}",
                granted_mode.as_str(),
                request.justification
            ),
        },
    });
    match outcome {
        EscalationOutcome::AllowedOnce => Ok(granted_mode),
        EscalationOutcome::Rejected => Err(format!(
            "the user rejected escalating this {} to \"{}\"; it stays denied, so stop and explain \
instead of working around it",
            request.subject,
            granted_mode.as_str()
        )),
        EscalationOutcome::Cancelled => Err(format!(
            "approval for escalating to \"{}\" was cancelled",
            granted_mode.as_str()
        )),
        EscalationOutcome::Unavailable => Err(format!(
            "sandbox escalation to \"{}\" requires approval, but no approval channel is available",
            granted_mode.as_str()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn request(mode: &str, effective: SandboxMode) -> EscalationRequest {
        EscalationRequest {
            requested_mode: mode.to_owned(),
            justification: "need to write the log".to_owned(),
            effective_mode: effective,
            subject: "command",
        }
    }

    fn approval<'a>(
        approver: Option<&'a dyn EscalationApprover>,
        agent: Option<&'a str>,
    ) -> EscalationApproval<'a> {
        EscalationApproval {
            approver,
            agent,
            call_id: "c1",
            tool_name: "bash",
        }
    }

    struct Fixed(EscalationOutcome);
    impl EscalationApprover for Fixed {
        fn request(&self, _ask: &EscalationAsk) -> EscalationOutcome {
            self.0
        }
    }

    #[test]
    fn pairing_validation_reproduces_the_three_refusals() {
        assert_eq!(
            validate_escalation_args(Some("workspace-write"), None).unwrap_err(),
            "invalid escalation: sandbox_permissions requires a justification"
        );
        assert_eq!(
            validate_escalation_args(None, Some("why")).unwrap_err(),
            "invalid escalation: justification is only valid together with sandbox_permissions"
        );
        assert_eq!(
            validate_escalation_args(Some("workspace-write"), Some("   ")).unwrap_err(),
            "invalid justification: expected a non-empty sentence"
        );
        validate_escalation_args(None, None).expect("bare call is fine");
        validate_escalation_args(Some("workspace-write"), Some("need it"))
            .expect("paired call is fine");
    }

    #[test]
    fn markers_are_verbatim() {
        assert_eq!(
            sandbox_denial_marker(SandboxMode::ReadOnly),
            "[sandbox: file access denied under read-only mode]"
        );
        assert_eq!(
            escalation_hint_marker("operation"),
            "[sandbox: escalation available — retry this exact operation once with \
sandbox_permissions (the narrowest wider mode that suffices) + justification; the approval \
prompt asks the user]"
        );
        assert_eq!(
            sandbox_permissions_description("command"),
            "The narrowest wider sandbox mode for a one-shot retry of the exact command the \
sandbox just denied; the retry asks the user for approval."
        );
    }

    #[test]
    fn repeating_the_effective_mode_needs_no_approval() {
        let outcome = approve_escalation(
            &request("workspace-write", SandboxMode::WorkspaceWrite),
            &approval(None, None),
        )
        .expect("same mode is free");
        assert_eq!(outcome, SandboxMode::WorkspaceWrite);
    }

    #[test]
    fn non_wider_targets_refuse_before_the_channel() {
        assert_eq!(
            approve_escalation(
                &request("read-only", SandboxMode::WorkspaceWrite),
                &approval(None, None)
            )
            .unwrap_err(),
            "sandbox escalation to \"read-only\" is not strictly wider than this call's current \
\"workspace-write\" mode"
        );
        let unknown = request("root", SandboxMode::ReadOnly);
        assert_eq!(
            approve_escalation(&unknown, &approval(None, None)).unwrap_err(),
            "sandbox escalation to \"root\" is not strictly wider than this call's current \
\"read-only\" mode"
        );
        let narrow_from_danger = request("workspace-write", SandboxMode::DangerFullAccess);
        assert_eq!(
            approve_escalation(&narrow_from_danger, &approval(None, None)).unwrap_err(),
            "sandbox escalation to \"workspace-write\" is not strictly wider than this call's \
current \"danger-full-access\" mode"
        );
    }

    #[test]
    fn missing_channel_or_agent_fail_closed() {
        let asking = request("workspace-write", SandboxMode::ReadOnly);
        assert_eq!(
            approve_escalation(&asking, &approval(None, Some("s1"))).unwrap_err(),
            "sandbox escalation to \"workspace-write\" requires approval, but no approval service \
is composed"
        );
        assert_eq!(
            approve_escalation(&asking, &approval(None, None)).unwrap_err(),
            "sandbox escalation to \"workspace-write\" requires approval, but no approval service \
is composed"
        );
        let granted = Fixed(EscalationOutcome::AllowedOnce);
        assert_eq!(
            approve_escalation(&asking, &approval(Some(&granted), None)).unwrap_err(),
            "sandbox escalation to \"workspace-write\" requires approval, but the call has no \
agent to route it through"
        );
    }

    #[test]
    fn outcomes_map_to_the_grant_or_verbatim_refusals() {
        let asking = request("danger-full-access", SandboxMode::WorkspaceWrite);
        let grant = Fixed(EscalationOutcome::AllowedOnce);
        assert_eq!(
            approve_escalation(&asking, &approval(Some(&grant), Some("s1"))).unwrap(),
            SandboxMode::DangerFullAccess
        );
        let rejected = Fixed(EscalationOutcome::Rejected);
        assert_eq!(
            approve_escalation(&asking, &approval(Some(&rejected), Some("s1"))).unwrap_err(),
            "the user rejected escalating this command to \"danger-full-access\"; it stays \
denied, so stop and explain instead of working around it"
        );
        let cancelled = Fixed(EscalationOutcome::Cancelled);
        assert_eq!(
            approve_escalation(&asking, &approval(Some(&cancelled), Some("s1"))).unwrap_err(),
            "approval for escalating to \"danger-full-access\" was cancelled"
        );
        let unavailable = Fixed(EscalationOutcome::Unavailable);
        assert_eq!(
            approve_escalation(&asking, &approval(Some(&unavailable), Some("s1"))).unwrap_err(),
            "sandbox escalation to \"danger-full-access\" requires approval, but no approval \
channel is available"
        );
    }

    #[test]
    fn the_audit_reason_carries_the_target_and_justification() {
        struct Capture(Mutex<Option<EscalationAsk>>);
        impl EscalationApprover for Capture {
            fn request(&self, ask: &EscalationAsk) -> EscalationOutcome {
                *self.0.lock().expect("lock") = Some(ask.clone());
                EscalationOutcome::AllowedOnce
            }
        }
        let capture = Capture(Mutex::new(None));
        approve_escalation(
            &request("workspace-write", SandboxMode::ReadOnly),
            &approval(Some(&capture), Some("agent-7")),
        )
        .expect("granted");
        let ask = capture.0.lock().expect("lock").clone().expect("captured");
        assert_eq!(ask.agent, "agent-7");
        assert_eq!(ask.tool_name, "bash");
        assert_eq!(ask.call_id, "c1");
        assert_eq!(
            ask.reason,
            "escalate sandbox to workspace-write: need to write the log"
        );
        assert_eq!(
            ask.display_reason.en,
            "Allow this operation with workspace-write permissions: need to write the log"
        );
        assert_eq!(
            ask.display_reason.zh,
            "允许本次操作使用 workspace-write 权限：need to write the log"
        );
    }

    #[test]
    fn wider_modes_and_targets_pin_the_ladder() {
        assert_eq!(
            wider_modes(SandboxMode::ReadOnly),
            &[SandboxMode::WorkspaceWrite, SandboxMode::DangerFullAccess]
        );
        assert_eq!(
            wider_modes(SandboxMode::WorkspaceWrite),
            &[SandboxMode::DangerFullAccess]
        );
        assert_eq!(wider_modes(SandboxMode::DangerFullAccess), &[]);
        assert_eq!(
            ESCALATION_TARGETS,
            [SandboxMode::WorkspaceWrite, SandboxMode::DangerFullAccess]
        );
    }
}
