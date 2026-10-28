// Rust guideline compliant 2026-10-29
//! The sandbox policy home and the process-confinement seam (dsh
//! `sandbox-policy` + `sandbox` + `sandbox-local`, one crate per PRD
//! 4.5).
//!
//! The policy home owns the deployment's sandbox fallbacks plus
//! per-session resolution: the file-effect mode, the `workspace-write`
//! root, and the override kit (the log-only `sandbox/mode` event, its
//! fold, and its write meaning — the last logged mode is the session's
//! override). Before each agent request the home contributes the
//! resolved policy to the cache-safe runtime-context snapshot, so
//! replay reconstructs the same mode and root the enforcing consumers
//! resolve without rewriting the stable system prompt.
//!
//! The confinement seam wraps exact subprocess argv under a host-path
//! file policy: the local backend selects the platform runner chain
//! (Linux bwrap then Landlock, macOS Seatbelt, Windows the ACL
//! restricted-token runner), functionally probes competing candidates
//! once, and reports each wrap's enforcement and stderr classification
//! facts. Missing or unusable confinement fails closed
//! (`SANDBOX_UNAVAILABLE`); silent unconfined passthrough is
//! forbidden.
//!
//! Wire carriers live in [`service`] (the `'sandbox-policy'` method
//! face plus the `'sandbox'` seam face) and [`plugin_shell`] (the
//! dylib artifact); the escalation vocabulary ([`escalation`]) and the
//! shared writable-root derivation ([`roots`]) serve every enforcing
//! tool family.

pub mod diagnostics;
pub mod escalation;
pub mod path_boundary;
pub mod plugin_shell;
pub mod policy;
pub mod profiles;
pub mod provider;
pub mod roots;
pub mod seam;
pub mod service;
pub mod workspace_sid;

#[cfg(windows)]
pub mod win;

pub use policy::SANDBOX_MODE_EVENT;
pub use policy::fold_sandbox_mode;
pub use policy::render_policy_context;
pub use seam::ConfinedMode;
pub use seam::SANDBOX_UNAVAILABLE;
pub use seam::SandboxEnforcement;
pub use seam::SandboxExecutionPolicy;
pub use seam::SandboxMode;
pub use seam::SandboxPolicy;
