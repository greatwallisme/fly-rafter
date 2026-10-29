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
//! 11-14, the shared projection fold at 900.

//! The domain vocabulary lands module-for-module ([`shell_env`],
//! [`working_directory`], [`render`], [`schema`], [`tools`],
//! [`service`], [`plugin_shell`]).

pub mod plugin_shell;
pub mod render;
pub mod schema;
pub mod service;
pub mod shell_env;
pub mod tools;
pub mod working_directory;
