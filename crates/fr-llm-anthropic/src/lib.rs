//! Fly Rafter Anthropic provider (PRD 13.2): the `fr-llm-anthropic`
//! artifact.
//!
//! Behavior and protocol parity with dsh `llm-pi-ai` serving the
//! hand-declared `anthropic-messages` wire protocol (PRD decision 45).
//! Provider routes form a dict, every model is hand-declared (no built-in
//! catalog, PRD 19.2 item 20), tool-update capability is absent (baseline
//! folding semantics; the in-history face needs withheld compat switches
//! only an installed catalog sets), and the OAuth stealth face follows
//! the `sk-ant-oat` value detection verbatim (D15-a). The cdylib face is
//! the plugin shell; the rlib face is the behavior library the network
//! slices wire to real HTTP transport.

pub mod adapter;
pub mod codes;
pub mod compat;
pub mod config;
pub mod convert;
pub mod estimate;
pub mod hash;
pub mod history_transform;
pub mod json_parse;
pub mod models;
pub mod overflow;
pub mod params;
pub mod plugin_shell;
pub mod settle;
pub mod sse;
pub mod text;
pub mod transcript;
pub mod translate;
