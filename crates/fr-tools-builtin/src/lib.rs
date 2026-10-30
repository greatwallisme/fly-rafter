// Rust guideline compliant 2026-10-29
//! The built-in model tools' domain layer: dsh `tool-bash` +
//! `tool-pwsh` + `shell-env` + `working-directory`, one crate per PRD
//! 4.5 and Q93-A.
//!
//! The 16a slice lands the shell-group two tools and their two
//! in-crate domain services. The tools consume their executor through
//! the fr-shell wire faces (`'shell'` resolve/start/result, `'jobs'`
//! wait/read/kill/remove); the dougong per-instance lock spans a whole
//! wire call, so the tool bodies never block on their own service and
//! every cross-plugin wait rides the registry's bounded faces.
//!
//! The job-backed registration is the only variant (the M1 composition
//! always has `jobs`): dsh's foreground-only switch arm is structurally
//! dead here and stays documented rather than implemented. Method
//! selectors on the `'builtin-tools'` service: 0 describe, bash
//! 1 execute / 2 render / 3 presentCall / 4 presentResult, pwsh
//! 11-14, the shared projection fold at 900. The schema constants are
//! script-extracted from the dsh docs/tool-catalog.md ([`catalog_gen`];
//! the Q95-A ruling) and frozen by the in-crate verify tests.

//! The 16b file group adds the fs domain, the observation gate, the
//! fence, the read/write/edit tool bodies, and the contextual-diff
//! engine ([`fs_types`], [`fsio`], [`fs_domain`], [`observation`],
//! [`fence`], [`fs_read_render`], [`fs_diff`], [`fs_sandbox`],
//! [`fs_tools`]).
//!
//! The domain vocabulary lands module-for-module ([`shell_env`],
//! [`working_directory`], [`render`], [`schema`], [`tools`],
//! [`service`], [`plugin_shell`], [`catalog_gen`], and the test-only
//! catalog-freeze verifier).

pub mod catalog_gen;
pub mod fence;
pub mod fs_diff;
pub mod fs_domain;
pub mod fs_read_render;
pub mod fs_sandbox;
pub mod fs_tools;
pub mod fs_types;
pub mod fsio;
pub mod observation;
pub mod plugin_shell;
pub mod render;
pub mod schema;
pub mod service;
pub mod shell_env;
pub mod tools;
#[cfg(test)]
mod verify;
pub mod working_directory;
