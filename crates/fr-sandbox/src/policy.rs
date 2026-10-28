// Rust guideline compliant 2026-10-29
//! The sandbox POLICY home (dsh `sandbox-policy/src/index.ts` +
//! `session-mode.ts`).
//!
//! The single owner of the deployment's sandbox fallbacks plus
//! per-session resolution: the file-effect mode, the `workspace-write`
//! root, and the override kit (the `sandbox/mode` event, its fold, and
//! its meaning — the last logged mode is the session's override, folded
//! by the `sandboxMode` projection unit registered through
//! `'session-projections'`).
//!
//! Enforcing filesystem, one-shot bash, and terminal backends read the
//! SAME resolved policy here. The context describes that policy without
//! inventorying capabilities, while each backend retains its own
//! enforcement dialect and each tool owns its operation-specific denial
//! and escalation guidance.

use serde_json::Value;

use crate::seam::SandboxExecutionPolicy;
use crate::seam::SandboxMode;

/// The session log event this home owns (dsh `sandbox/mode`): log-only (the `approval/*` precedent — durable and replayable, never in the model transcript).
///
/// `source: "delegation"` marks an override seeded
/// into a child; an absent source is a runtime switch.
pub const SANDBOX_MODE_EVENT: &str = "sandbox/mode";

/// Preserves execution-world spelling; enforcing providers resolve
/// filesystem identity on their host (dsh `resolveWorkspaceRoot`).
fn resolve_workspace_root(path: &str) -> Result<String, String> {
    if !std::path::Path::new(path).is_absolute() {
        return Err(
            "sandbox-policy: workspace root must be an absolute execution-world path".to_owned(),
        );
    }
    Ok(path.to_owned())
}

/// Plugin config: the deployment's sandbox default (dsh `Config`).
///
/// All
/// optional — [`PolicyConfig::default`] supplies the schema defaults
/// (`read-only` is the fail-safe default; a deployment that wants a
/// workspace-writable agent opts in explicitly through the assembly
/// layer's `FR_PERMISSION_MODE` expression). The runner choice is NOT
/// here (it is the confinement seam's config), nor is any per-family
/// knob: this is the one shared policy home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyConfig {
    /// File-sandbox mode a session starts from (plugin schema default:
    /// `read-only`).
    pub mode: SandboxMode,
    /// Absolute fallback root for agentless calls and sessions without a
    /// cwd; `None` resolves to the process cwd at construction (dsh has
    /// no schema default — the fallback is real branching, resolved
    /// absolute either way).
    pub workspace_root: Option<String>,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            mode: SandboxMode::ReadOnly,
            workspace_root: None,
        }
    }
}

/// The calling session's policy facts, pre-folded by the reader (the
/// shell's self-read judgment: the session log's header cwd plus the
/// folded `sandbox/mode` override).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFacts {
    /// The session's immutable cwd — its workspace-write boundary.
    pub cwd: Option<String>,
    /// The last logged `sandbox/mode` override, if the log carries one.
    pub override_mode: Option<SandboxMode>,
}

/// Inputs that select the sandbox policy for one capability call (dsh
/// `SandboxPolicyRequest`).
#[derive(Debug, Clone, Default)]
pub struct ResolveRequest {
    /// The calling session's facts; its immutable cwd becomes the
    /// workspace boundary.
    pub session: Option<(String, SessionFacts)>,
    /// Explicit approved mode override, which outranks session policy.
    pub mode: Option<SandboxMode>,
}

/// The sandbox-policy service's domain state (dsh
/// `SandboxPolicyService`): the deployment default mode and the absolute
/// `workspace-write` fallback root, both fixed at construction.
#[derive(Debug, Clone)]
pub struct PolicyHome {
    /// The deployment default mode — the fallback beneath a session
    /// override.
    pub default_mode: SandboxMode,
    /// The absolute `workspace-write` fallback root for calls without a
    /// session cwd.
    pub workspace_root: String,
}

impl PolicyHome {
    /// Builds the home from plugin config, resolving the fallback root
    /// to the process cwd when the config omits it (dsh resolves
    /// `process.cwd()` in the constructor so the stored root is always
    /// absolute regardless of how it was supplied).
    ///
    /// # Errors
    ///
    /// Returns the verbatim dsh refusal when a configured root is not an
    /// absolute execution-world path, or the process cwd is unavailable.
    pub fn new(config: &PolicyConfig) -> Result<Self, String> {
        let configured = config.workspace_root.as_deref();
        let root = match configured {
            Some(root) => resolve_workspace_root(root)?,
            None => std::env::current_dir()
                .map_err(|error| format!("sandbox-policy: workspace root fell back to the process cwd, which is unavailable: {error}"))?
                .to_string_lossy()
                .into_owned(),
        };
        Ok(Self {
            default_mode: config.mode,
            workspace_root: root,
        })
    }

    /// Resolves the complete policy for one capability call (dsh
    /// `resolve`): an approved explicit mode outranks the session's last
    /// `sandbox/mode` event, which outranks the deployment default. A
    /// session cwd is its workspace-write boundary; the configured root
    /// is the fallback for agentless calls and sessions without a cwd.
    ///
    /// # Errors
    ///
    /// Returns the workspace-root refusal when the session cwd is not an
    /// absolute execution-world path.
    pub fn resolve(&self, request: &ResolveRequest) -> Result<SandboxExecutionPolicy, String> {
        let (session_id, facts) = match &request.session {
            Some((id, facts)) => (Some(id.as_str()), Some(facts)),
            None => (None, None),
        };
        let mode = request
            .mode
            .or_else(|| facts.and_then(|facts| facts.override_mode))
            .unwrap_or(self.default_mode);
        let workspace_root = match facts.and_then(|facts| facts.cwd.as_deref()) {
            Some(cwd) => resolve_workspace_root(cwd)?,
            None => self.workspace_root.clone(),
        };
        Ok(SandboxExecutionPolicy {
            mode,
            workspace_root,
            session_id: session_id.map(str::to_owned),
        })
    }
}

/// Renders the policy without claiming which capabilities are mounted
/// (dsh `renderPolicyContext`, all three segments verbatim).
#[must_use]
pub fn render_policy_context(policy: &SandboxExecutionPolicy) -> String {
    match policy.mode {
        SandboxMode::ReadOnly => "Current DSH file policy: read-only. Any available operation \
enforced by the DSH file sandbox cannot modify files in the standing mode. Do not refuse a \
required modification from this policy alone: try an available tool normally and follow any \
denial and escalation guidance it returns."
            .to_owned(),
        SandboxMode::WorkspaceWrite => {
            let root = serde_json::to_string(&policy.workspace_root)
                .unwrap_or_else(|_| policy.workspace_root.clone());
            format!(
                "Current DSH file policy: workspace-write. Any available operation enforced by \
the DSH file sandbox may modify files under the session workspace: {root}. Some platform \
temporary areas may also be writable."
            )
        }
        SandboxMode::DangerFullAccess => "Current DSH file policy: danger-full-access. The DSH \
file sandbox does not restrict file modifications by available operations."
            .to_owned(),
    }
}

/// Folds the session log into the `sandboxMode` projection state (dsh
/// `init: () => null` + `apply`): the last `sandbox/mode` event's mode,
/// or `None` before one (the deployment default applies at resolve
/// time).
///
/// # Errors
///
/// Fails closed on a `sandbox/mode` event whose data is not the typed
/// shape (the fr structural difference over dsh's typed union, the
/// `fr-permission-presets` fold precedent).
pub fn fold_sandbox_mode(events: &[Value]) -> Result<Option<SandboxMode>, String> {
    let mut state: Option<SandboxMode> = None;
    for event in events {
        if event.get("type").and_then(Value::as_str) != Some(SANDBOX_MODE_EVENT) {
            continue;
        }
        let word = event
            .pointer("/data/mode")
            .and_then(Value::as_str)
            .ok_or_else(|| "sandbox: sandbox/mode event carries no mode word".to_owned())?;
        state = Some(SandboxMode::parse(word).ok_or_else(|| {
            format!("sandbox: sandbox/mode event carries an invalid mode word \"{word}\"")
        })?);
    }
    Ok(state)
}

/// The wire data one `sandbox/mode` append carries for a runtime switch
/// (dsh `setSandboxMode` — the switch IS its event; `delegation` seeding
/// adds its source at the delegation site).
#[must_use]
pub fn sandbox_mode_event_data(mode: SandboxMode) -> Value {
    serde_json::json!({ "mode": mode.as_str() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn facts(cwd: Option<&str>, mode: Option<SandboxMode>) -> (String, SessionFacts) {
        (
            "s1".to_owned(),
            SessionFacts {
                cwd: cwd.map(str::to_owned),
                override_mode: mode,
            },
        )
    }

    #[test]
    fn non_absolute_roots_refuse_verbatim() {
        assert_eq!(
            resolve_workspace_root("relative/path").unwrap_err(),
            "sandbox-policy: workspace root must be an absolute execution-world path"
        );
    }

    #[test]
    fn resolution_ladders_request_over_session_over_default() {
        let home = PolicyHome::new(&PolicyConfig {
            mode: SandboxMode::ReadOnly,
            workspace_root: Some("/fallback".to_owned()),
        })
        .expect("home");
        // Agentless: the deployment default and fallback root, no session id.
        assert_eq!(
            home.resolve(&ResolveRequest::default()).expect("bare"),
            SandboxExecutionPolicy {
                mode: SandboxMode::ReadOnly,
                workspace_root: "/fallback".to_owned(),
                session_id: None,
            }
        );
        // Session override outranks the default; the session cwd wins the
        // root and the session id rides along.
        assert_eq!(
            home.resolve(&ResolveRequest {
                session: Some(facts(Some("/ws"), Some(SandboxMode::WorkspaceWrite))),
                mode: None,
            })
            .expect("session"),
            SandboxExecutionPolicy {
                mode: SandboxMode::WorkspaceWrite,
                workspace_root: "/ws".to_owned(),
                session_id: Some("s1".to_owned()),
            }
        );
        // An approved explicit mode outranks the session override.
        assert_eq!(
            home.resolve(&ResolveRequest {
                session: Some(facts(Some("/ws"), Some(SandboxMode::WorkspaceWrite))),
                mode: Some(SandboxMode::DangerFullAccess),
            })
            .expect("request"),
            SandboxExecutionPolicy {
                mode: SandboxMode::DangerFullAccess,
                workspace_root: "/ws".to_owned(),
                session_id: Some("s1".to_owned()),
            }
        );
        // A session without a cwd falls back to the configured root.
        assert_eq!(
            home.resolve(&ResolveRequest {
                session: Some(facts(None, None)),
                mode: None
            })
            .expect("cwdless"),
            SandboxExecutionPolicy {
                mode: SandboxMode::ReadOnly,
                workspace_root: "/fallback".to_owned(),
                session_id: Some("s1".to_owned()),
            }
        );
        // A relative session cwd refuses (the verbatim guard).
        assert_eq!(
            home.resolve(&ResolveRequest {
                session: Some(facts(Some("rel"), None)),
                mode: None
            })
            .unwrap_err(),
            "sandbox-policy: workspace root must be an absolute execution-world path"
        );
    }

    #[test]
    fn the_policy_context_renders_all_three_segments_verbatim() {
        let policy = SandboxExecutionPolicy {
            mode: SandboxMode::ReadOnly,
            workspace_root: "/ws".to_owned(),
            session_id: None,
        };
        assert_eq!(
            render_policy_context(&policy),
            "Current DSH file policy: read-only. Any available operation enforced by the DSH \
file sandbox cannot modify files in the standing mode. Do not refuse a required modification \
from this policy alone: try an available tool normally and follow any denial and escalation \
guidance it returns."
        );
        let policy = SandboxExecutionPolicy {
            mode: SandboxMode::WorkspaceWrite,
            workspace_root: "/ws \"quoted\"".to_owned(),
            session_id: None,
        };
        assert_eq!(
            render_policy_context(&policy),
            "Current DSH file policy: workspace-write. Any available operation enforced by the \
DSH file sandbox may modify files under the session workspace: \"/ws \\\"quoted\\\"\". Some \
platform temporary areas may also be writable."
        );
        let policy = SandboxExecutionPolicy {
            mode: SandboxMode::DangerFullAccess,
            workspace_root: "/ws".to_owned(),
            session_id: None,
        };
        assert_eq!(
            render_policy_context(&policy),
            "Current DSH file policy: danger-full-access. The DSH file sandbox does not restrict \
file modifications by available operations."
        );
    }

    #[test]
    fn the_fold_keeps_the_last_override_and_fails_closed_on_bad_words() {
        assert_eq!(fold_sandbox_mode(&[]).expect("empty"), None);
        let events = vec![
            json!({ "type": "session/start", "data": {} }),
            json!({ "type": SANDBOX_MODE_EVENT, "data": { "mode": "workspace-write" } }),
            json!({ "type": "approval/policy", "data": { "policy": "ask" } }),
            json!({ "type": SANDBOX_MODE_EVENT, "data": { "mode": "read-only", "source": "delegation" } }),
        ];
        assert_eq!(
            fold_sandbox_mode(&events).expect("fold"),
            Some(SandboxMode::ReadOnly)
        );
        let bad = vec![json!({ "type": SANDBOX_MODE_EVENT, "data": { "mode": "root" } })];
        assert_eq!(
            fold_sandbox_mode(&bad).unwrap_err(),
            "sandbox: sandbox/mode event carries an invalid mode word \"root\""
        );
        let bare = vec![json!({ "type": SANDBOX_MODE_EVENT, "data": {} })];
        assert_eq!(
            fold_sandbox_mode(&bare).unwrap_err(),
            "sandbox: sandbox/mode event carries no mode word"
        );
    }

    #[test]
    fn the_switch_event_data_is_the_mode_word() {
        assert_eq!(
            sandbox_mode_event_data(SandboxMode::WorkspaceWrite),
            json!({ "mode": "workspace-write" })
        );
    }
}
