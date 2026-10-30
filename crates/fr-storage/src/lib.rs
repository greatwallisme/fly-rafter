// Rust guideline compliant 2026-10-30
//! The storage-domain seam (dsh `storage` + `storage-domain` +
//! `storage-json`, one crate per the Q16c-3B ruling): declarative unit
//! descriptors, one FIFO write chain per domain, a single-layout JSON unit
//! backend with the atomic temp-rename-fsync publish protocol, and the
//! facility that enforces single-open per domain name.
//!
//! Record schemas live with the owning consumer package (the dsh zod
//! schemas live in the schedule package's own `storage.ts`), so the
//! consumer validates records after `load_all` and refuses the open on the
//! first invalid record — the dsh facility's `invalid-record` rejection,
//! carried by the consumer's open protocol instead of the facility.
//!
//! The domain vocabulary and the runtime land module-for-module
//! ([`error`], [`mod@format`], [`atomic`], [`mod@unit`], [`domain`],
//! [`facility`], [`service`], [`plugin_shell`]).

pub mod atomic;
pub mod domain;
pub mod error;
pub mod facility;
pub mod format;
pub mod plugin_shell;
pub mod service;
pub mod unit;
