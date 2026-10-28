// Rust guideline compliant 2026-10-29
//! The process-confinement capability seam (dsh `sandbox/src/index.ts`,
//! verbatim semantics).
//!
//! One confined execution carries its file-effect policy PER CALL (two
//! consumers may confine under different policies at the same instant),
//! and a provider wraps the caller's exact argv so the confined program
//! runs under the host kernel's enforcement — or the wrap fails closed
//! with [`SANDBOX_UNAVAILABLE`]; silent unconfined passthrough is
//! forbidden.

use serde::{Deserialize, Serialize};

/// Every file-effect mode (dsh `SandboxMode`).
///
/// Local mirror of the vocabulary (the batch 2a link-collision judgment:
/// plugin shells each export one `DOUGONG_PLUGIN_EXPORTS` symbol, so
/// cross-plugin value types are mirrored per crate with comment
/// cross-references — the sibling mirror lives in
/// `fr-permission-presets/src/lib.rs`; within this crate every module
/// imports the vocabulary from here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxMode {
    /// Only required sinks such as `/dev/null` stay writable.
    ReadOnly,
    /// The workspace plus a backend-defined temp area stay writable.
    WorkspaceWrite,
    /// No confinement (the mode a [`SandboxPolicy`] cannot carry).
    DangerFullAccess,
}

impl SandboxMode {
    /// The wire word for this mode (dsh kebab-case vocabulary).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }

    /// Parses the wire vocabulary; `None` outside it.
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

/// A confining (non-`danger-full-access`) mode — the modes a
/// [`SandboxPolicy`] can carry (dsh `ConfinedSandboxMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfinedMode {
    /// Only required sinks such as `/dev/null` stay writable.
    ReadOnly,
    /// The workspace plus a backend-defined temp area stay writable.
    WorkspaceWrite,
}

impl ConfinedMode {
    /// The wire word for this mode.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
        }
    }

    /// Parses the confining subset; `None` outside it.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "read-only" => Some(Self::ReadOnly),
            "workspace-write" => Some(Self::WorkspaceWrite),
            _ => None,
        }
    }
}

impl From<ConfinedMode> for SandboxMode {
    fn from(mode: ConfinedMode) -> Self {
        match mode {
            ConfinedMode::ReadOnly => Self::ReadOnly,
            ConfinedMode::WorkspaceWrite => Self::WorkspaceWrite,
        }
    }
}

/// The complete file-effect policy resolved for one capability call (dsh `SandboxExecutionPolicy`).
///
/// The root is carried even under modes that
/// do not consume it so callers resolve policy once before choosing the
/// enforcement path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxExecutionPolicy {
    /// The file-effect mode this execution runs under.
    pub mode: SandboxMode,
    /// Absolute root directory `workspace-write` may write under.
    pub workspace_root: String,
    /// Opaque identity of the calling session; absent for agentless
    /// calls, which fall back to per-call backend state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Sandbox mode serde over the kebab wire words.
impl Serialize for SandboxMode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SandboxMode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let word = String::deserialize(deserializer)?;
        Self::parse(&word)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown sandbox mode: {word}")))
    }
}

/// Sandbox mode serde over the kebab wire words.
impl Serialize for ConfinedMode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ConfinedMode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let word = String::deserialize(deserializer)?;
        Self::parse(&word)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown confined mode: {word}")))
    }
}

/// Enforcement completeness for this host (dsh `SandboxEnforcement`).
///
/// `Partial` means an active backend or older kernel ABI cannot govern
/// every promised file effect; callers requiring an absolute boundary
/// must not treat it as `Full`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxEnforcement {
    /// Every promised file effect is governed.
    Full,
    /// An older kernel ABI governs only a subset of the promises.
    Partial,
}

impl Serialize for SandboxEnforcement {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SandboxEnforcement {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let word = String::deserialize(deserializer)?;
        Self::parse(&word)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown sandbox enforcement: {word}")))
    }
}

impl SandboxEnforcement {
    /// The wire word for this completeness.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Partial => "partial",
        }
    }

    /// Parses the wire vocabulary; `None` outside it.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "full" => Some(Self::Full),
            "partial" => Some(Self::Partial),
            _ => None,
        }
    }
}

/// What one confined execution is allowed to touch (dsh `SandboxPolicy`):
/// the execution policy narrowed to its confining modes, carried per call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxPolicy {
    /// The file-effect mode this execution runs under (never
    /// `danger-full-access`).
    pub mode: ConfinedMode,
    /// Absolute root directory `workspace-write` may write under.
    pub workspace_root: String,
    /// Opaque identity of the calling session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

impl SandboxPolicy {
    /// Narrows one execution policy to its confining subset; `None`
    /// under `danger-full-access` (that mode bypasses confinement).
    #[must_use]
    pub fn confine_from(execution: &SandboxExecutionPolicy) -> Option<Self> {
        let mode = match execution.mode {
            SandboxMode::DangerFullAccess => return None,
            SandboxMode::ReadOnly => ConfinedMode::ReadOnly,
            SandboxMode::WorkspaceWrite => ConfinedMode::WorkspaceWrite,
        };
        Some(Self {
            mode,
            workspace_root: execution.workspace_root.clone(),
            session_id: execution.session_id.clone(),
        })
    }
}

/// Evidence that identifies a sandbox runner failing before it executes
/// the wrapped command (dsh `RunnerFailureRule`).
///
/// Apply [`RunnerFailureRule::allowed_exit_codes`] when present, remove
/// [`RunnerFailureRule::informational_lines`] by case-insensitive exact
/// line equality, then match [`RunnerFailureRule::fatal_signatures`]
/// case-insensitively within each remaining stderr line. Exit status
/// alone never proves runner failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RunnerFailureRule {
    /// Nonzero process exit codes on which this rule may match; omitted
    /// permits any nonzero exit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_exit_codes: Option<Vec<i32>>,
    /// Non-empty substrings identifying a fatal runner diagnostic on one
    /// stderr line.
    pub fatal_signatures: Vec<String>,
    /// Benign stderr lines excluded by exact full-line equality before
    /// fatal matching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub informational_lines: Option<Vec<String>>,
}

/// A `confine` result (dsh `ConfinedArgv`): the argv to spawn in place of
/// the caller's own, plus the enforcement completeness and the selected
/// backend's classification facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfinedArgv {
    /// The wrapped argv (runner, profile, separator, then the caller's
    /// argv).
    pub argv: Vec<String>,
    /// How completely the selected backend enforces the policy's file
    /// effects.
    pub enforcement: SandboxEnforcement,
    /// The selected backend's denial dialect: the case-insensitive stderr
    /// substrings a file effect denied by THIS backend produces.
    pub denial_signatures: Vec<String>,
    /// Structured runner-failure evidence rules.
    pub runner_failure_rules: Vec<RunnerFailureRule>,
}

/// The error code for a requested confined mode when no backend is usable
/// (dsh `SANDBOX_UNAVAILABLE`); the provider fails closed and the tool
/// layer carries the code through the structured failure channel.
pub const SANDBOX_UNAVAILABLE: &str = "SANDBOX_UNAVAILABLE";

/// Renders the fail-closed refusal (dsh `SandboxUnavailableError`,
/// verbatim — the install-guidance sentence and the optional runner
/// failure detail).
#[must_use]
pub fn sandbox_unavailable_text(mode: ConfinedMode, detail: Option<&str>) -> String {
    let base = format!(
        "sandbox mode \"{}\" is requested but no sandbox backend is usable on this host; \
refusing to run the command unconfined. Install bubblewrap or run a Landlock-enforcing \
kernel (Linux), ensure sandbox-exec is usable (macOS), or ensure the ACL \
restricted-token runner can start (Windows) — otherwise switch the consumer to \
danger-full-access.",
        mode.as_str()
    );
    match detail {
        Some(detail) => format!("{base} Runner failure: {detail}"),
        None => base,
    }
}

/// The fail-closed `confine` refusal (the [`SANDBOX_UNAVAILABLE`] text
/// plus the confining mode that had no usable backend).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxUnavailableError {
    /// The confining mode whose backend chain was empty.
    pub mode: ConfinedMode,
    /// The classified runner-failure detail, when the last probe left one.
    pub detail: Option<String>,
}

impl SandboxUnavailableError {
    /// Builds the refusal for one confining mode.
    #[must_use]
    pub const fn new(mode: ConfinedMode) -> Self {
        Self { mode, detail: None }
    }

    /// Attaches the classified runner-failure detail.
    #[must_use]
    pub fn with_detail(mut self, detail: String) -> Self {
        self.detail = Some(detail);
        self
    }
}

impl std::fmt::Display for SandboxUnavailableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&sandbox_unavailable_text(self.mode, self.detail.as_deref()))
    }
}

impl std::error::Error for SandboxUnavailableError {}
