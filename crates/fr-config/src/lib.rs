//! Layered assembly configuration for Fly Rafter (PRD 6).
//!
//! dougong's loader accepts only whole JSON documents; the layered patch
//! semantics (ordered stacks, per-key override by `id`, root-level `insert`)
//! are rebuilt here — aligned with dsh's `applyEntryPatches` — and compiled
//! down to a loader document for `dougong_loader::Reconciler` convergence.

#![forbid(unsafe_code)]

pub mod cond;
pub mod error;
pub mod patch;
pub mod source;
pub mod stack;

pub use cond::{EvalContext, eval, host_platform};
pub use error::Error;
pub use patch::{
    CompileFailure, CompileOutcome, ComposeOutcome, FlatRow, PatchLine, compile_to_loader_doc,
    compose, parse_patch_lines,
};
pub use source::{PathResolver, SourceKind, SourceSpec, check_trust_boundary};
pub use stack::{
    EntryKind, LoadedProfile, SkippedBundle, StackOutcome, builtin_bundle_yaml, init_profile,
    load_optional_patches, load_profile, load_required_overlay, load_stack,
    profile_template_bundles, read_profile_manifest, validate_profile_name,
};
