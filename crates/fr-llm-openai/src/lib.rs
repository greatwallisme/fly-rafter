//! Fly Rafter OpenAI-compatible provider (PRD 13.2): the `fr-llm-openai`
//! artifact.
//!
//! Behavior and protocol parity with dsh `llm-pi-ai` serving the two
//! hand-declared OpenAI-family wire protocols (`openai-completions` and
//! `openai-responses`; PRD decision 42 Q74-B/D5). Provider routes form a
//! dict, every model is hand-declared (no built-in catalog, PRD 19.2 item
//! 20), and tool-update capability is absent for both protocols (baseline
//! folding semantics). The cdylib face is the plugin shell; the rlib face
//! is the behavior library the network slices wire to real HTTP transport.

pub mod compat;
pub mod config;
pub mod convert;
pub mod estimate;
pub mod hash;
pub mod history_transform;
pub mod json_parse;
pub mod models;
pub mod params;
pub mod plugin_shell;
pub mod prompt_cache;
pub mod responses;
pub mod responses_stream;
pub mod text;
pub mod transcript;
pub mod translate;
pub mod usage;
