//! Fly Rafter `DeepSeek` provider (PRD 13.2): the `fr-llm-deepseek` artifact.
//!
//! Behavior and protocol parity with dsh `llm-deepseek` (Anthropic Messages
//! wire protocol): wire request types, request serialization, SSE frame
//! parsing with heartbeat-comment pulses, event translation, replay state,
//! connection option resolution, and `x-api-key` credentials. The cdylib
//! face is the plugin shell; the rlib face is the behavior library that a
//! future network slice wires to real HTTP transport.

pub mod adapter;
pub mod auth;
pub mod codes;
pub mod config;
pub mod defaults;
pub mod file_id;
pub mod file_store;
pub mod files_api;
pub mod http;
pub mod image_tokens;
pub mod images;
pub mod messages_api;
pub mod model_info;
pub mod models;
pub mod plugin_shell;
pub mod replay;
pub mod request_extensions;
pub mod request_files;
pub mod request_image;
pub mod request_image_pipeline;
pub mod request_pricing;
pub mod serialize;
pub mod sse;
pub mod translate;
pub mod transport;
pub mod types;
pub mod upload_index;
pub mod wire_types;
