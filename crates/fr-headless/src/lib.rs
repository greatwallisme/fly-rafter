//! Fly Rafter headless entry plugin (PRD 4.5: the `fr-headless` artifact).
//!
//! 18a shell: the dylib artifact provides the `headless-startup` service
//! and holds the app argv snapshot the host relayed through it during the
//! boot window (dougong dylib activations receive no slot config — the
//! service relay is the sanctioned reading rail; headless spec decision
//! line, 2026-11-04 amendment). The startup parse (task/session-id/json
//! with the dsh `headless/src/startup.ts` wording), the runner, and the
//! `--json` projection arrive with the Phase 18 runner slices on this same
//! crate; the stored snapshot is the raw substrate they consume.
//!
//! Kernel-family edge: this module is the only `dougong-abi` consumer; the
//! rest of the crate stays a plain rlib.

pub mod adopt;
pub mod help_text;
pub mod json_stream;
pub mod plugin_shell;
pub mod reasoning;
pub mod startup;
