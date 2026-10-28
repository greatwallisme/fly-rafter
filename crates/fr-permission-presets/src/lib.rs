//! User-facing permission presets over the independent sandbox-mode and
//! approval-policy knobs (dsh `@deepseek-ai/dsh-permission-presets`
//! parity, Q104′A: an independent plugin crate).
//!
//! A switch records the selected preset, then writes each changed knob
//! through its canonical write path: `sandbox/mode` and `approval/policy`
//! appends for the free-function setters (the `set` path and constructor
//! seeding), the `approval` service's notice-carrying `set-policy` for
//! the `/permission` command path. The read side exposes the process
//! catalog plus the current-value-only `permissions` projection.
//!
//! Folding ownership (Q110′A′): this crate self-holds the fold. Its own
//! reads pull the log over the `session-persistence` read face and fold
//! locally with [`fold_permissions`] — the same single implementation
//! its wire fold endpoint serves, so the self-read and the relayed
//! third-party read can never disagree.
//!
//! Structurally absent versus dsh (documented, not behavior deltas): the
//! Auto registration hook and catalog-changed emit (Q98-B ruling — the
//! derive auto-match arm is unreachable without it, and a stored `auto`
//! identity still refuses at pin); the optional settings and commands
//! children (no host service composes them in M1); the startup
//! `sessions.list()` seeding loop (Q106-A — no list face); the volatile
//! default-preset re-read (dylib config is baked immutable, so the
//! constructor's resolution stands).
//!
//! The sandbox-mode and approval-policy word lists are local mirrors of
//! the owner packages' exports: a value import across plugin crates would
//! collide on each artifact's `DOUGONG_PLUGIN_EXPORTS` symbol at link
//! time (the batch 2a fr-wire placement judgment), so each consumer
//! carries its own copy with this cross-reference.

use serde_json::Value;

pub mod plugin_shell;
pub mod service;

/// Every sandbox mode (dsh `SANDBOX_MODES`).
pub const SANDBOX_MODES: [&str; 3] = ["read-only", "workspace-write", "danger-full-access"];

/// Every approval policy word (dsh `APPROVAL_POLICIES`).
pub const APPROVAL_POLICIES: [&str; 2] = ["ask", "never"];

/// Returned when effective knob values match no available preset; never
/// a switch target or event payload.
pub const CUSTOM_PRESET: &str = "custom";

/// Canonical identity of the experimental per-call review preset (the
/// Auto integration's fixed name; the hook itself is Q98-B-absent).
pub const AUTO_PRESET: &str = "auto";

/// The `permissions` projection's registered key.
pub const PROJECTION_KEY: &str = "permissions";

/// The `permissions` projection's state version (dsh `stateVersion: 2`).
pub const PROJECTION_STATE_VERSION: u64 = 2;

/// One file-effect sandbox mode (dsh `SandboxMode`, local mirror).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    /// `read-only`.
    ReadOnly,
    /// `workspace-write`.
    WorkspaceWrite,
    /// `danger-full-access`.
    DangerFullAccess,
}

impl SandboxMode {
    /// The wire word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }

    /// Parses one wire word (`None` on any other spelling).
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "read-only" => Some(Self::ReadOnly),
            "workspace-write" => Some(Self::WorkspaceWrite),
            "danger-full-access" => Some(Self::DangerFullAccess),
            _ => None,
        }
    }
}

/// One approval policy word (dsh `ApprovalPolicy`, local mirror).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalPolicy {
    /// `ask`.
    Ask,
    /// `never`.
    Never,
}

impl ApprovalPolicy {
    /// The wire word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Never => "never",
        }
    }

    /// Parses one wire word (`None` on any other spelling).
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "ask" => Some(Self::Ask),
            "never" => Some(Self::Never),
            _ => None,
        }
    }
}

/// One preset's sandbox/approval bundle and optional client presentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetSpec {
    /// The `sandbox/mode` value the preset writes through.
    pub sandbox: SandboxMode,
    /// The `approval/policy` value the preset writes through.
    pub approval: ApprovalPolicy,
    /// The display label a client shows; the raw table key when omitted.
    pub name: Option<String>,
    /// One user-facing sentence; omitted when not configured.
    pub description: Option<String>,
}

/// The projection unit's knob state: the last seen value of each knob
/// event, `None` before an override (composition defaults apply at view
/// time).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KnobState {
    /// Last `permission/preset` payload.
    pub preset: Option<String>,
    /// Last `sandbox/mode` payload.
    pub sandbox: Option<SandboxMode>,
    /// Last `approval/policy` payload.
    pub approval: Option<ApprovalPolicy>,
}

/// Projection state for permission overrides and constructor-seed status
/// (the wire shape is the dsh flat object: the knob fields inline, then
/// `seeded`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PermissionProjectionState {
    /// The knob state.
    #[serde(flatten)]
    pub knobs: KnobState,
    /// Whether the log contains a constructor-seed boundary.
    pub seeded: bool,
}

/// One-event permission-state transition (the projection unit's `apply`):
/// unrelated events leave the state untouched.
///
/// # Errors
///
/// dsh's typed apply cannot fail; over the wire an event whose knob data
/// is not a vocabulary word fails closed rather than folding garbage.
pub fn apply_permission_event(
    state: &mut PermissionProjectionState,
    event: &Value,
) -> Result<(), String> {
    let type_name = event.get("type").and_then(Value::as_str).unwrap_or("");
    let data = event.get("data").cloned().unwrap_or(Value::Null);
    match type_name {
        "permission/preset" => {
            state.knobs.preset = data
                .get("preset")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        "sandbox/mode" => {
            let word = data.get("mode").and_then(Value::as_str).unwrap_or("");
            state.knobs.sandbox = Some(SandboxMode::parse(word).ok_or_else(|| {
                format!("permission: sandbox/mode event carries an invalid mode word \"{word}\"")
            })?);
        }
        "approval/policy" => {
            let word = data.get("policy").and_then(Value::as_str).unwrap_or("");
            state.knobs.approval = Some(ApprovalPolicy::parse(word).ok_or_else(|| {
                format!(
                    "permission: approval/policy event carries an invalid policy word \"{word}\""
                )
            })?);
        }
        "session/end-seed" => {
            state.seeded = true;
        }
        _ => {}
    }
    Ok(())
}

/// Folds a full session log into the `permissions` projection state (the
/// single implementation behind both the self-read and the wire fold
/// endpoint; dsh `init` starts every knob at its composition default).
///
/// # Errors
///
/// Propagates the first knob-data refusal (see
/// [`apply_permission_event`]).
pub fn fold_permissions(events: &[Value]) -> Result<PermissionProjectionState, String> {
    let mut state = PermissionProjectionState::default();
    for event in events {
        apply_permission_event(&mut state, event)?;
    }
    Ok(state)
}

/// The session-log seam: append one typed event, read one session's full
/// log (the same shape the approval shell carries).
pub trait SessionLog: Send + Sync {
    /// Appends one typed event.
    ///
    /// # Errors
    ///
    /// Returns the append refusal text.
    fn append(&self, session: &str, type_name: &str, data: Value) -> Result<(), String>;

    /// Reads one session's full event list in log order.
    ///
    /// # Errors
    ///
    /// Returns the read refusal text.
    fn read_all(&self, session: &str) -> Result<Vec<Value>, String>;
}

/// The live sandbox-mode read seam (dsh `ctx.shell.sandboxMode`; the
/// `'sandbox-policy'` service face over the wire).
pub trait SandboxModeRead: Send + Sync {
    /// Reads the deployment's default sandbox mode.
    ///
    /// # Errors
    ///
    /// Returns the seam refusal text (service absent or failed).
    fn sandbox_mode(&self) -> Result<SandboxMode, String>;
}

/// The notice-carrying approval write seam (dsh `ctx.approval.setPolicy`;
/// the command path only — the free-function path appends directly).
pub trait ApprovalSetPolicy: Send + Sync {
    /// Switches the session's policy with the user notice.
    ///
    /// # Errors
    ///
    /// Returns the set-policy refusal text.
    fn set_policy(&self, agent: &str, policy: ApprovalPolicy) -> Result<(), String>;
}

/// One command settlement (dsh `/permission` handler return).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    /// `success` or `error`.
    pub kind: &'static str,
    /// The settlement text.
    pub text: String,
}

impl CommandOutcome {
    /// A success settlement.
    #[must_use]
    pub const fn success(text: String) -> Self {
        Self {
            kind: "success",
            text,
        }
    }

    /// An error settlement.
    #[must_use]
    pub const fn error(text: String) -> Self {
        Self {
            kind: "error",
            text,
        }
    }
}

/// Presentation for one available preset or the derived `custom` value.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PresetOption {
    /// Stable option value.
    pub value: String,
    /// The display label.
    pub name: String,
    /// One user-facing sentence; omitted when not configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Process-level permission catalog.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PermissionCatalog {
    /// Every currently selectable preset, in contribution order.
    pub options: Vec<PresetOption>,
    /// Configured presets eligible as defaults for future sessions.
    pub default_options: Vec<PresetOption>,
    /// The effective default for new sessions.
    pub default_preset: String,
}

/// Owns the deployment's configured permission presets and their write
/// path. Requires a confining sandbox policy seam and the approval
/// default; unmatched knob values report [`CUSTOM_PRESET`], never an
/// error.
#[derive(Debug, Clone)]
pub struct PermissionPresets {
    presets: Vec<(String, PresetSpec)>,
    default_preset: Option<String>,
    approval_default: ApprovalPolicy,
}

impl PermissionPresets {
    /// Validates and assembles the service: the reserved-name refusals,
    /// the confining-executor requirement, and the composed-default
    /// checks, in the dsh constructor's order.
    ///
    /// # Errors
    ///
    /// Returns the dsh constructor refusal texts verbatim.
    pub fn try_new(
        presets: Vec<(String, PresetSpec)>,
        default_preset: Option<String>,
        mode: Option<&SandboxMode>,
        approval_default: ApprovalPolicy,
    ) -> Result<Self, String> {
        if presets.iter().any(|(name, _)| name == CUSTOM_PRESET) {
            return Err(format!(
                "permission: \"{CUSTOM_PRESET}\" is reserved for the derived not-a-preset state and cannot name a table entry"
            ));
        }
        if presets.iter().any(|(name, _)| name == AUTO_PRESET) {
            return Err(format!(
                "permission: \"{AUTO_PRESET}\" is reserved and cannot name a configured preset"
            ));
        }
        let Some(mode) = mode else {
            return Err(
                "permission: the mounted bash executor does not confine (no sandboxMode) \
— presets bundle a sandbox mode, so composing this plugin over an unconfined executor \
is a misconfiguration"
                    .to_owned(),
            );
        };
        let empty = KnobState::default();
        let inferred = derive_of(&presets, &empty, *mode, approval_default);
        let default_preset = default_preset.unwrap_or(inferred);
        if default_preset == CUSTOM_PRESET {
            return Err(
                "permission: composed sandbox and approval defaults match no preset; \
configure defaultPreset explicitly"
                    .to_owned(),
            );
        }
        resolve_of(&presets, &default_preset)?;
        Ok(Self {
            presets,
            default_preset: Some(default_preset),
            approval_default,
        })
    }

    /// The advertised preset names: configured entries in declaration
    /// order (Auto joins only while its integration is live — Q98-B
    /// leaves that hook absent, so it never joins here).
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.presets.iter().map(|(name, _)| name.as_str()).collect()
    }

    /// The preset currently selected as the default for future sessions.
    #[must_use]
    pub fn default_preset(&self) -> &str {
        // The baked config is immutable, so the constructor's resolution
        // stands (dsh re-reads a volatile here).
        self.default_preset.as_deref().unwrap_or(CUSTOM_PRESET)
    }

    /// Resolves the preset matching the effective knob values: a
    /// still-matching last selection wins shared-bundle ties, otherwise
    /// the first configured match wins, otherwise [`CUSTOM_PRESET`].
    #[must_use]
    pub fn derive(&self, state: &KnobState, mode: &SandboxMode) -> String {
        derive_of(&self.presets, state, *mode, self.approval_default)
    }

    /// Resolves an available preset's knob bundle.
    ///
    /// # Errors
    ///
    /// Returns the dsh `resolve` refusal verbatim.
    pub fn resolve(&self, name: &str) -> Result<&PresetSpec, String> {
        resolve_of(&self.presets, name)
    }

    /// Builds the client option for an available preset or
    /// [`CUSTOM_PRESET`]; a missing label falls back to the preset key.
    ///
    /// # Errors
    ///
    /// Returns the dsh `resolve` refusal verbatim.
    pub fn option_of(&self, name: &str) -> Result<PresetOption, String> {
        if name == CUSTOM_PRESET {
            return Ok(PresetOption {
                value: CUSTOM_PRESET.to_owned(),
                name: "Custom".to_owned(),
                description: Some(
                    "Current sandbox and approval settings do not match a preset.".to_owned(),
                ),
            });
        }
        let spec = self.resolve(name)?;
        Ok(PresetOption {
            value: name.to_owned(),
            name: spec.name.clone().unwrap_or_else(|| name.to_owned()),
            description: spec.description.clone(),
        })
    }

    /// Reads the complete process-level catalog.
    ///
    /// # Errors
    ///
    /// Returns the live mode-read refusal.
    pub fn catalog(&self, mode_read: &dyn SandboxModeRead) -> Result<PermissionCatalog, String> {
        let mode = mode_read.sandbox_mode()?;
        let empty = KnobState::default();
        let default_preset = derive_of(&self.presets, &empty, mode, self.approval_default);
        let options = self
            .names()
            .into_iter()
            .map(|name| self.option_of(name))
            .collect::<Result<Vec<_>, _>>()?;
        let default_options = self
            .presets
            .iter()
            .map(|(name, _)| self.option_of(name))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PermissionCatalog {
            options,
            default_options,
            default_preset,
        })
    }

    /// The preset currently effective for one session: fold its log, then
    /// derive (Q110′A′ self-read).
    ///
    /// # Errors
    ///
    /// Returns the read or fold refusal.
    pub fn current(
        &self,
        session: &str,
        log: &dyn SessionLog,
        mode_read: &dyn SandboxModeRead,
    ) -> Result<String, String> {
        let state = Self::projection_state(session, log)?;
        let mode = mode_read.sandbox_mode()?;
        Ok(self.derive(&state.knobs, &mode))
    }

    /// Records a changed preset, then updates each changed knob through
    /// the free-function write path (dsh `set`).
    ///
    /// # Errors
    ///
    /// Returns the read, fold, or append refusal.
    pub fn set(
        &self,
        session: &str,
        name: &str,
        log: &dyn SessionLog,
        mode_read: &dyn SandboxModeRead,
    ) -> Result<(), String> {
        let writer = DirectApproval { log };
        self.apply(session, name, log, mode_read, &writer)
    }

    /// The `/permission` command's behavior face (the one write path a
    /// client uses): empty input reports the current preset, an unknown
    /// name settles as an error, and a known name applies through the
    /// notice-carrying approval write path.
    ///
    /// # Errors
    ///
    /// Returns the read, fold, or write refusal (a settlement is not an
    /// error).
    pub fn command_permission(
        &self,
        agent_session: &str,
        raw_input: &str,
        log: &dyn SessionLog,
        mode_read: &dyn SandboxModeRead,
        approval: &dyn ApprovalSetPolicy,
    ) -> Result<CommandOutcome, String> {
        let name = raw_input.trim();
        let available = self.names().join(", ");
        if name.is_empty() {
            let current = self.current(agent_session, log, mode_read)?;
            return Ok(CommandOutcome::success(format!(
                "current preset {current} (available: {available})"
            )));
        }
        if !self.names().contains(&name) {
            return Ok(CommandOutcome::error(format!(
                "unknown preset \"{name}\" (available: {available})"
            )));
        }
        self.apply(agent_session, name, log, mode_read, approval)?;
        Ok(CommandOutcome::success(format!("preset {name}")))
    }

    /// Fills every missing permission fact before a session is published
    /// (the `session/created` seeding hook): a genuinely fresh session
    /// uses the current default; seeded or partially initialized sessions
    /// preserve their effective knob values and only gain the missing
    /// durable facts; a stored Auto identity refuses without its live
    /// integration.
    ///
    /// # Errors
    ///
    /// Returns the dsh pin refusals verbatim, or the read/append
    /// refusal.
    pub fn pin_initial_permission(
        &self,
        session: &str,
        log: &dyn SessionLog,
        mode_read: &dyn SandboxModeRead,
    ) -> Result<(), String> {
        let state = Self::projection_state(session, log)?;
        if state.knobs.preset.as_deref() == Some(AUTO_PRESET) {
            // The admission call that would follow needs the Q98-B-absent
            // Auto hook, so a stored Auto identity always refuses here.
            return Err(
                "permission: cannot restore preset \"auto\" without its active integration"
                    .to_owned(),
            );
        }
        if state.knobs.preset.is_none()
            && state.knobs.sandbox.is_none()
            && state.knobs.approval.is_none()
            && !state.seeded
        {
            let name = self.default_preset().to_owned();
            let spec = self.resolve(&name)?.clone();
            log.append(
                session,
                "permission/preset",
                serde_json::json!({ "preset": name }),
            )?;
            log.append(
                session,
                "sandbox/mode",
                serde_json::json!({ "mode": spec.sandbox.as_str() }),
            )?;
            log.append(
                session,
                "approval/policy",
                serde_json::json!({ "policy": spec.approval.as_str() }),
            )?;
            return Ok(());
        }
        let mode = mode_read.sandbox_mode()?;
        let effective = self.derive(&state.knobs, &mode);
        if state.knobs.preset.is_none() && effective != CUSTOM_PRESET {
            log.append(
                session,
                "permission/preset",
                serde_json::json!({ "preset": effective }),
            )?;
        }
        if state.knobs.sandbox.is_none() {
            log.append(
                session,
                "sandbox/mode",
                serde_json::json!({ "mode": mode.as_str() }),
            )?;
        }
        if state.knobs.approval.is_none() {
            log.append(
                session,
                "approval/policy",
                serde_json::json!({ "policy": self.approval_default.as_str() }),
            )?;
        }
        Ok(())
    }

    /// Apply one preset through its durable identity and canonical knob
    /// setters: the preset event first (only on a change), then each
    /// changed knob through its own write path.
    fn apply(
        &self,
        session: &str,
        name: &str,
        log: &dyn SessionLog,
        mode_read: &dyn SandboxModeRead,
        set_approval: &dyn ApprovalSetPolicy,
    ) -> Result<(), String> {
        let spec = self.resolve(name)?.clone();
        let knobs = Self::projection_state(session, log)?.knobs;
        let mode = mode_read.sandbox_mode()?;
        let current = self.derive(&knobs, &mode);
        if current != name {
            log.append(
                session,
                "permission/preset",
                serde_json::json!({ "preset": name }),
            )?;
        }
        if spec.sandbox != knobs.sandbox.unwrap_or(mode) {
            log.append(
                session,
                "sandbox/mode",
                serde_json::json!({ "mode": spec.sandbox.as_str() }),
            )?;
        }
        if spec.approval != knobs.approval.unwrap_or(self.approval_default) {
            set_approval.set_policy(session, spec.approval)?;
        }
        Ok(())
    }

    /// Folds one session's log into the projection state (the Q110′A′
    /// self-read; the registration this crate files at setup makes the
    /// same fold reachable for third parties through the relay).
    fn projection_state(
        session: &str,
        log: &dyn SessionLog,
    ) -> Result<PermissionProjectionState, String> {
        let events = log.read_all(session)?;
        fold_permissions(&events)
    }
}

/// The free-function approval write path (dsh `setApprovalPolicy`): one
/// plain `approval/policy` append, no notice.
struct DirectApproval<'a> {
    log: &'a dyn SessionLog,
}

impl ApprovalSetPolicy for DirectApproval<'_> {
    fn set_policy(&self, agent: &str, policy: ApprovalPolicy) -> Result<(), String> {
        self.log.append(
            agent,
            "approval/policy",
            serde_json::json!({ "policy": policy.as_str() }),
        )
    }
}

/// The shared mathematics of `current` and the projection unit's view.
fn derive_of(
    presets: &[(String, PresetSpec)],
    state: &KnobState,
    mode: SandboxMode,
    approval_default: ApprovalPolicy,
) -> String {
    let sandbox = state.sandbox.unwrap_or(mode);
    let approval = state.approval.unwrap_or(approval_default);
    // A still-matching selection returns its own name. dsh's
    // still-selected-Auto arm (matching `never` with the stored Auto's
    // sandbox) needs the live Auto integration; without the Q98-B-absent
    // hook `auto` never resolves to a spec, so that arm cannot fire and
    // stays documented rather than dead-coded.
    if let Some(preset) = &state.preset
        && let Some((_, spec)) = presets.iter().find(|(name, _)| name == preset)
        && spec.sandbox == sandbox
        && spec.approval == approval
    {
        return preset.clone();
    }
    for (name, spec) in presets {
        if spec.sandbox == sandbox && spec.approval == approval {
            return name.clone();
        }
    }
    CUSTOM_PRESET.to_owned()
}

/// The spec resolver over one table.
fn resolve_of<'a>(
    presets: &'a [(String, PresetSpec)],
    name: &str,
) -> Result<&'a PresetSpec, String> {
    presets.iter().find(|(known, _)| known == name).map_or_else(
        || {
            let names = presets
                .iter()
                .map(|(known, _)| known.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Err(format!(
                "permission: unknown preset \"{name}\" (known: {names})"
            ))
        },
        |(_, spec)| Ok(spec),
    )
}

/// The deployment's baked preset table — the exact three bare-key
/// entries the headless bundle pins (dsh base `cordis.patch.yml`).
#[must_use]
pub fn baked_presets() -> Vec<(String, PresetSpec)> {
    vec![
        (
            "read-only".to_owned(),
            PresetSpec {
                sandbox: SandboxMode::ReadOnly,
                approval: ApprovalPolicy::Ask,
                name: None,
                description: None,
            },
        ),
        (
            "workspace-write".to_owned(),
            PresetSpec {
                sandbox: SandboxMode::WorkspaceWrite,
                approval: ApprovalPolicy::Ask,
                name: None,
                description: None,
            },
        ),
        (
            "danger-full-access".to_owned(),
            PresetSpec {
                sandbox: SandboxMode::DangerFullAccess,
                approval: ApprovalPolicy::Never,
                name: None,
                description: None,
            },
        ),
    ]
}

/// The deployment's baked `defaultPreset` — the bundle pins no key, so
/// the composed default applies (the inferred preset).
#[must_use]
pub const fn baked_default_preset() -> Option<String> {
    None
}

/// The deployment's baked approval default — the exact expression the
/// headless bundle pins for the approval plugin's config row (the
/// baked-config judgment; fr-approval carries the same expression).
#[must_use]
pub fn baked_default_approval_policy() -> ApprovalPolicy {
    baked_default_approval_policy_from(std::env::var("FR_PERMISSION_MODE").ok().as_deref())
}

/// The pure form of the baked approval default (the bundle expression:
/// unset or anything but `danger-full-access` means the interactive
/// default).
#[must_use]
pub fn baked_default_approval_policy_from(mode: Option<&str>) -> ApprovalPolicy {
    match mode {
        Some("danger-full-access") => ApprovalPolicy::Never,
        _ => ApprovalPolicy::Ask,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AUTO_PRESET, ApprovalPolicy, ApprovalSetPolicy, CUSTOM_PRESET, CommandOutcome,
        PermissionPresets, PresetSpec, SandboxMode, baked_default_approval_policy_from,
        baked_presets, fold_permissions,
    };
    use serde_json::{Value, json};
    use std::sync::Mutex;

    fn table() -> Vec<(String, PresetSpec)> {
        baked_presets()
    }

    fn service() -> PermissionPresets {
        PermissionPresets::try_new(
            table(),
            None,
            Some(&SandboxMode::WorkspaceWrite),
            ApprovalPolicy::Ask,
        )
        .expect("assembles")
    }

    /// A recording log: appends land in one vector, reads replay a seed.
    struct FakeLog {
        appends: Mutex<Vec<(String, String, Value)>>,
        seed: Vec<Value>,
    }

    impl FakeLog {
        fn new(seed: Vec<Value>) -> Self {
            Self {
                appends: Mutex::new(Vec::new()),
                seed,
            }
        }

        fn recorded(&self) -> Vec<(String, String, Value)> {
            self.appends.lock().expect("lock").clone()
        }
    }

    impl super::SessionLog for FakeLog {
        fn append(&self, session: &str, type_name: &str, data: Value) -> Result<(), String> {
            self.appends.lock().expect("lock").push((
                session.to_owned(),
                type_name.to_owned(),
                data,
            ));
            Ok(())
        }

        fn read_all(&self, _session: &str) -> Result<Vec<Value>, String> {
            Ok(self.seed.clone())
        }
    }

    fn event(type_name: &str, data: &Value) -> Value {
        json!({ "type": type_name, "data": data.clone() })
    }

    // The fold: four arms, overwrite semantics, unrelated passthrough.

    #[test]
    fn fold_applies_the_four_event_arms() {
        let events = vec![
            event("permission/preset", &json!({ "preset": "read-only" })),
            event("sandbox/mode", &json!({ "mode": "workspace-write" })),
            event("approval/policy", &json!({ "policy": "never" })),
            event("turn/start", &json!({ "turn": 1 })),
            event("session/end-seed", &json!({})),
            event(
                "permission/preset",
                &json!({ "preset": "danger-full-access" }),
            ),
        ];
        let state = fold_permissions(&events).expect("fold");
        assert_eq!(state.knobs.preset.as_deref(), Some("danger-full-access"));
        assert_eq!(state.knobs.sandbox, Some(SandboxMode::WorkspaceWrite));
        assert_eq!(state.knobs.approval, Some(ApprovalPolicy::Never));
        assert!(state.seeded);

        let error = fold_permissions(&[event("sandbox/mode", &json!({ "mode": "wide-open" }))])
            .expect_err("invalid word fails closed");
        assert_eq!(
            error,
            "permission: sandbox/mode event carries an invalid mode word \"wide-open\""
        );
        let error = fold_permissions(&[event("approval/policy", &json!({ "policy": "always" }))])
            .expect_err("invalid word fails closed");
        assert_eq!(
            error,
            "permission: approval/policy event carries an invalid policy word \"always\""
        );
    }

    // derive: table matches, still-matching preset wins, custom.

    #[test]
    fn derive_matches_the_table_and_keeps_a_still_matching_selection() {
        let service = service();
        let knobs = |preset: Option<&str>,
                     sandbox: Option<crate::SandboxMode>,
                     approval: Option<crate::ApprovalPolicy>| {
            super::KnobState {
                preset: preset.map(str::to_owned),
                sandbox,
                approval,
            }
        };
        // Nulls default to the composition facts.
        assert_eq!(
            service.derive(&knobs(None, None, None), &SandboxMode::WorkspaceWrite),
            "workspace-write"
        );
        // A still-matching selection returns its own name.
        assert_eq!(
            service.derive(
                &knobs(
                    Some("danger-full-access"),
                    Some(SandboxMode::DangerFullAccess),
                    Some(ApprovalPolicy::Never)
                ),
                &SandboxMode::WorkspaceWrite
            ),
            "danger-full-access"
        );
        // A null approval defaults to the composition default, not the
        // selection's bundle: (danger, ask) matches nothing.
        assert_eq!(
            service.derive(
                &knobs(
                    Some("danger-full-access"),
                    Some(SandboxMode::DangerFullAccess),
                    None
                ),
                &SandboxMode::WorkspaceWrite
            ),
            CUSTOM_PRESET
        );
        // A stale selection falls through to the first configured match.
        assert_eq!(
            service.derive(
                &knobs(
                    Some("read-only"),
                    Some(SandboxMode::DangerFullAccess),
                    Some(ApprovalPolicy::Never)
                ),
                &SandboxMode::WorkspaceWrite
            ),
            "danger-full-access"
        );
        // No match is custom, never an error.
        assert_eq!(
            service.derive(
                &knobs(Some("read-only"), None, Some(ApprovalPolicy::Never)),
                &SandboxMode::WorkspaceWrite
            ),
            CUSTOM_PRESET
        );
    }

    // Constructor: the four dsh refusals verbatim, plus resolution.

    #[test]
    fn try_new_refuses_reserved_names_unconfined_executors_and_lost_defaults() {
        let mut reserved = table();
        reserved.push((CUSTOM_PRESET.to_owned(), reserved[0].1.clone()));
        let error = PermissionPresets::try_new(
            reserved,
            None,
            Some(&SandboxMode::WorkspaceWrite),
            ApprovalPolicy::Ask,
        )
        .expect_err("custom reserved");
        assert_eq!(
            error,
            "permission: \"custom\" is reserved for the derived not-a-preset state and cannot name a table entry"
        );

        let mut reserved = table();
        reserved.push((AUTO_PRESET.to_owned(), reserved[0].1.clone()));
        let error = PermissionPresets::try_new(
            reserved,
            None,
            Some(&SandboxMode::WorkspaceWrite),
            ApprovalPolicy::Ask,
        )
        .expect_err("auto reserved");
        assert_eq!(
            error,
            "permission: \"auto\" is reserved and cannot name a configured preset"
        );

        let error = PermissionPresets::try_new(Vec::new(), None, None, ApprovalPolicy::Ask)
            .expect_err("unconfined");
        assert_eq!(
            error,
            "permission: the mounted bash executor does not confine (no sandboxMode) — presets bundle a sandbox mode, so composing this plugin over an unconfined executor is a misconfiguration"
        );

        // An empty table with a mode leaves the composed default custom.
        let error = PermissionPresets::try_new(
            Vec::new(),
            None,
            Some(&SandboxMode::ReadOnly),
            ApprovalPolicy::Ask,
        )
        .expect_err("custom default");
        assert_eq!(
            error,
            "permission: composed sandbox and approval defaults match no preset; configure defaultPreset explicitly"
        );

        // An explicit default that names nothing refuses via resolve.
        let error = PermissionPresets::try_new(
            table(),
            Some("bogus".to_owned()),
            Some(&SandboxMode::WorkspaceWrite),
            ApprovalPolicy::Ask,
        )
        .expect_err("unknown default");
        assert_eq!(
            error,
            "permission: unknown preset \"bogus\" (known: read-only, workspace-write, danger-full-access)"
        );
    }

    #[test]
    fn resolve_option_of_and_catalog_report_the_table() {
        let service = service();
        assert_eq!(
            service.resolve("nope").expect_err("unknown"),
            "permission: unknown preset \"nope\" (known: read-only, workspace-write, danger-full-access)"
        );
        let option = service.option_of(CUSTOM_PRESET).expect("custom");
        assert_eq!(option.name, "Custom");
        assert_eq!(
            option.description.as_deref(),
            Some("Current sandbox and approval settings do not match a preset.")
        );
        // The baked table carries no labels: the key is the name.
        let option = service.option_of("read-only").expect("option");
        assert_eq!(option.value, "read-only");
        assert_eq!(option.name, "read-only");
        assert_eq!(option.description, None);

        let catalog = service
            .catalog(&FixedMode(SandboxMode::WorkspaceWrite))
            .expect("catalog");
        assert_eq!(catalog.default_preset, "workspace-write");
        assert_eq!(catalog.options.len(), 3);
        assert_eq!(catalog.default_options.len(), 3);
    }

    // apply/set: the preset event only on change, knobs through their
    // own write paths.

    struct FixedMode(SandboxMode);

    impl super::SandboxModeRead for FixedMode {
        fn sandbox_mode(&self) -> Result<SandboxMode, String> {
            Ok(self.0)
        }
    }

    struct RecordingApproval {
        calls: Mutex<Vec<(String, ApprovalPolicy)>>,
        fail: bool,
    }

    impl ApprovalSetPolicy for RecordingApproval {
        fn set_policy(&self, agent: &str, policy: ApprovalPolicy) -> Result<(), String> {
            if self.fail {
                return Err("set-policy refused".to_owned());
            }
            self.calls
                .lock()
                .expect("lock")
                .push((agent.to_owned(), policy));
            Ok(())
        }
    }

    #[test]
    fn set_writes_the_preset_event_then_changed_knobs() {
        let service = service();
        let log = FakeLog::new(vec![event(
            "permission/preset",
            &json!({ "preset": "read-only" }),
        )]);
        service
            .set(
                "s1",
                "danger-full-access",
                &log,
                &FixedMode(SandboxMode::ReadOnly),
            )
            .expect("set");
        assert_eq!(
            log.recorded(),
            vec![
                (
                    "s1".to_owned(),
                    "permission/preset".to_owned(),
                    json!({ "preset": "danger-full-access" })
                ),
                (
                    "s1".to_owned(),
                    "sandbox/mode".to_owned(),
                    json!({ "mode": "danger-full-access" })
                ),
                (
                    "s1".to_owned(),
                    "approval/policy".to_owned(),
                    json!({ "policy": "never" })
                ),
            ],
            "the set path writes approval through the free-function append"
        );

        // Selecting the effective preset again appends nothing.
        let log = FakeLog::new(vec![
            event("permission/preset", &json!({ "preset": "workspace-write" })),
            event("sandbox/mode", &json!({ "mode": "workspace-write" })),
            event("approval/policy", &json!({ "policy": "ask" })),
        ]);
        service
            .set(
                "s1",
                "workspace-write",
                &log,
                &FixedMode(SandboxMode::WorkspaceWrite),
            )
            .expect("no-op set");
        assert_eq!(log.recorded(), vec![]);
    }

    #[test]
    fn the_command_path_writes_approval_through_the_notice_seam() {
        let service = service();
        let log = FakeLog::new(Vec::new());
        let approval = RecordingApproval {
            calls: Mutex::new(Vec::new()),
            fail: false,
        };
        let outcome = service
            .command_permission(
                "s1",
                " danger-full-access ",
                &log,
                &FixedMode(SandboxMode::WorkspaceWrite),
                &approval,
            )
            .expect("command runs");
        assert_eq!(
            outcome,
            CommandOutcome::success("preset danger-full-access".to_owned())
        );
        assert_eq!(
            log.recorded(),
            vec![
                (
                    "s1".to_owned(),
                    "permission/preset".to_owned(),
                    json!({ "preset": "danger-full-access" })
                ),
                (
                    "s1".to_owned(),
                    "sandbox/mode".to_owned(),
                    json!({ "mode": "danger-full-access" })
                ),
            ],
            "the approval knob left the log: the notice seam carries it"
        );
        assert_eq!(
            *approval.calls.lock().expect("lock"),
            vec![("s1".to_owned(), ApprovalPolicy::Never)]
        );
    }

    #[test]
    fn the_command_settles_current_and_unknown_names() {
        let service = service();
        let log = FakeLog::new(Vec::new());
        let approval = RecordingApproval {
            calls: Mutex::new(Vec::new()),
            fail: false,
        };
        let outcome = service
            .command_permission(
                "s1",
                "  ",
                &log,
                &FixedMode(SandboxMode::ReadOnly),
                &approval,
            )
            .expect("empty input");
        assert_eq!(
            outcome,
            CommandOutcome::success(
                "current preset read-only (available: read-only, workspace-write, danger-full-access)"
                    .to_owned()
            )
        );
        let outcome = service
            .command_permission(
                "s1",
                "nope",
                &log,
                &FixedMode(SandboxMode::ReadOnly),
                &approval,
            )
            .expect("unknown name");
        assert_eq!(
            outcome,
            CommandOutcome::error(
                "unknown preset \"nope\" (available: read-only, workspace-write, danger-full-access)"
                    .to_owned()
            )
        );
    }

    // pin: the three dsh branches.

    #[test]
    fn pin_seeds_a_fresh_session_with_the_default_three_events() {
        let service = service();
        let log = FakeLog::new(Vec::new());
        service
            .pin_initial_permission("s1", &log, &FixedMode(SandboxMode::WorkspaceWrite))
            .expect("pin");
        assert_eq!(
            log.recorded(),
            vec![
                (
                    "s1".to_owned(),
                    "permission/preset".to_owned(),
                    json!({ "preset": "workspace-write" })
                ),
                (
                    "s1".to_owned(),
                    "sandbox/mode".to_owned(),
                    json!({ "mode": "workspace-write" })
                ),
                (
                    "s1".to_owned(),
                    "approval/policy".to_owned(),
                    json!({ "policy": "ask" })
                ),
            ]
        );
    }

    #[test]
    fn pin_fills_only_the_missing_facts_of_a_partial_session() {
        let service = service();
        // Only the sandbox knob exists and its bundle matches the
        // composition default approval: the effective preset backfills
        // the name, the live mode backfills nothing, the approval
        // default backfills the policy.
        let log = FakeLog::new(vec![event("sandbox/mode", &json!({ "mode": "read-only" }))]);
        service
            .pin_initial_permission("s1", &log, &FixedMode(SandboxMode::WorkspaceWrite))
            .expect("pin");
        assert_eq!(
            log.recorded(),
            vec![
                (
                    "s1".to_owned(),
                    "permission/preset".to_owned(),
                    json!({ "preset": "read-only" })
                ),
                (
                    "s1".to_owned(),
                    "approval/policy".to_owned(),
                    json!({ "policy": "ask" })
                ),
            ]
        );

        // A sandbox override that matches no bundle with the composed
        // approval stays unnamed (dsh: effective custom backfills
        // nothing); only the missing approval leg lands.
        let log = FakeLog::new(vec![event(
            "sandbox/mode",
            &json!({ "mode": "danger-full-access" }),
        )]);
        service
            .pin_initial_permission("s1", &log, &FixedMode(SandboxMode::WorkspaceWrite))
            .expect("pin");
        assert_eq!(
            log.recorded(),
            vec![(
                "s1".to_owned(),
                "approval/policy".to_owned(),
                json!({ "policy": "ask" })
            ),]
        );

        // A seeded session with every knob present gains nothing.
        let log = FakeLog::new(vec![
            event("permission/preset", &json!({ "preset": "read-only" })),
            event("sandbox/mode", &json!({ "mode": "read-only" })),
            event("approval/policy", &json!({ "policy": "ask" })),
            event("session/end-seed", &json!({})),
        ]);
        service
            .pin_initial_permission("s1", &log, &FixedMode(SandboxMode::WorkspaceWrite))
            .expect("pin");
        assert_eq!(log.recorded(), vec![]);
    }

    #[test]
    fn pin_refuses_a_stored_auto_identity() {
        let service = service();
        let log = FakeLog::new(vec![event(
            "permission/preset",
            &json!({ "preset": AUTO_PRESET }),
        )]);
        let error = service
            .pin_initial_permission("s1", &log, &FixedMode(SandboxMode::WorkspaceWrite))
            .expect_err("auto refuses");
        assert_eq!(
            error,
            "permission: cannot restore preset \"auto\" without its active integration"
        );
    }

    #[test]
    fn current_folds_the_log_and_derives() {
        let service = service();
        let log = FakeLog::new(vec![
            event("sandbox/mode", &json!({ "mode": "danger-full-access" })),
            event("approval/policy", &json!({ "policy": "never" })),
        ]);
        assert_eq!(
            service
                .current("s1", &log, &FixedMode(SandboxMode::WorkspaceWrite))
                .expect("current"),
            "danger-full-access"
        );
    }

    #[test]
    fn the_baked_default_policy_follows_the_bundle_expression() {
        assert_eq!(
            baked_default_approval_policy_from(None),
            ApprovalPolicy::Ask
        );
        assert_eq!(
            baked_default_approval_policy_from(Some("workspace-write")),
            ApprovalPolicy::Ask
        );
        assert_eq!(
            baked_default_approval_policy_from(Some("danger-full-access")),
            ApprovalPolicy::Never
        );
    }
}

// Rust guideline compliant 2026-10-29
