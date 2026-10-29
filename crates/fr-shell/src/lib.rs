// Rust guideline compliant 2026-10-29
//! The shell execution seam, the managed-range subprocess service, and the
//! background-job registry (dsh `shell` + `bash-local` + `bash-sandbox` +
//! `pwsh-local` + `pwsh-sandbox` + `subprocess` + `subprocess-local` +
//! `jobs` + `jobs-local`, one crate per PRD 4.5 and Q93-A).
//!
//! Three services ride one export table with artifact-unique method
//! bands (the 1000+ precedent): `'subprocess'` keeps the low band,
//! `'jobs'` occupies the 1000+ band, and `'shell'` the 2000+ band.
//! Because the dougong instance lock spans a whole wire call (dougong-
//! dylib `enter_exclusive`), no handler blocks unboundedly: process
//! spawns return immediately, and waits ride bounded slice calls driven
//! by the consumer.
//!
//! The long-running coupling stays in-crate: `shell` start registers
//! the command as a job with the process's output sources (the dsh
//! bash tool's `startJob` absorbed into the seam) and the job pump
//! drains the process streams into the ring. Sandbox consumption goes
//! cross-plugin through the `'sandbox'` confinement endpoint
//! (`fr-sandbox`); session lifecycle records for owned jobs go through
//! `'session-persistence'` as plugin records.
//!
//! The domain vocabulary and the executors land module-for-module
//! ([`jobs_types`], [`ring`], [`pump`], [`jobs_local`], [`subprocess`],
//! [`shell_types`], [`executors`], [`service`], [`plugin_shell`]).

pub mod executors;
pub mod jobs_local;
pub mod jobs_types;
pub mod plugin_shell;
pub mod pump;
pub mod ring;
pub mod service;
pub mod shell_types;
pub mod subprocess;
