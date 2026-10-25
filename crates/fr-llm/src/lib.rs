//! Fly Rafter LLM seam (PRD 13.1, PRD 4.5).
//!
//! Wire shapes live in `fr-wire`; this crate carries the dsh `dsh-llm`
//! behavior parity surface: stream assembly settlement, the error taxonomy,
//! retry policy resolution, request projection, attribution, and API-key
//! normalization (the rlib face that the provider artifacts link). The
//! cdylib artifact face ([`plugin_shell`]) provides the `llm` service that
//! fr-agent-loop injects (PRD :336).

pub mod adapter;
pub mod api_key;
pub mod assembler;
pub mod assistant_stream;
pub mod attribution;
pub mod error;
pub mod plugin_shell;
pub mod projection;
pub mod retry_policy;

// Rust guideline compliant 2026-10-25
