//! Layered assembly configuration for Fly Rafter (PRD 6).
//!
//! dougong's loader accepts only whole JSON documents; the layered patch
//! semantics (ordered stacks, per-key override by `id`, root-level `insert`)
//! are rebuilt here — aligned with dsh's `applyEntryPatches` — and compiled
//! down to a loader document for `dougong_loader::Reconciler` convergence.

#![forbid(unsafe_code)]

pub mod cond;
pub mod dump;
pub mod env;
pub mod error;
pub mod patch;
pub mod source;
pub mod stack;

pub use cond::{EvalContext, eval, host_platform};
pub use dump::{DumpLayer, collect_dump_layers, render_dump};
pub use env::{
    BOOTSTRAP_NAMES, BOOTSTRAP_PREFIXES, ENV_FILENAME, EnvEntry, EnvLayer, EnvSink, EnvSnapshot,
    EnvSource, FR_HOME_DIR_NAME, FR_HOME_ENV, HOME_LAYER_PROXY_NAMES, is_bootstrap_only,
    load_layered_env_into, parse_env, read_env_layer, resolve_home, resolve_home_from,
};
pub use error::Error;
pub use fr_schema::dump::schema_diagnostic_line;
pub use fr_schema::lookup as schema_lookup;
pub use patch::{
    CompileFailure, CompileOutcome, ComposeOutcome, FlatRow, PatchLine, compile_to_loader_doc,
    compose, parse_patch_lines,
};
pub use source::{PathResolver, SourceKind, SourceSpec, check_trust_boundary};
pub use stack::{
    EntryKind, LoadedProfile, ROOT_CONFIG, ROOT_FILENAME, SkippedBundle, StackOutcome,
    StackRequest, builtin_bundle_yaml, init_profile, initialize_profile_from_default,
    load_optional_patches, load_profile, load_required_overlay, load_stack, prepare_profile,
    profile_template_bundles, read_profile_manifest, validate_profile_name,
};
