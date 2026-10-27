// Rust guideline compliant 2026-10-28
//! The workspace-instruction domain (dsh `agent-instructions`).
//!
//! AGENTS.md discovery, bounded reads, deduplication, budgeted rendering,
//! and reconciliation state. Filesystem access rides a seam (Q88-A): the
//! production implementation reads the host filesystem and an absent seam
//! composes to a no-op, mirroring dsh's providerless no-op mount.

pub mod config;
pub mod digest;
pub mod files;
pub mod render;
pub mod state;
