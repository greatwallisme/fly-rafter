// Rust guideline compliant 2026-11-05
//! The fr-working-directory wire vocabulary, single-sourced in the owner
//! crate.
//!
//! fr-tools-builtin consumes this crate's rlib for the service face
//! constants its ensure/set seams require instead of carrying local
//! copies (the `tools_client` judgment).

/// The durable change event type (dsh `working-directory/change`).
pub const CHANGE_EVENT: &str = "working-directory/change";

/// The dylib artifact name (the descriptor's `name`; the manifest row
/// resolves the artifact by the same stem).
pub const PLUGIN_NAME: &str = "fr-working-directory";

/// The provided service name consumers inject.
pub const SERVICE_NAME: &str = "working-directory";

/// Face metadata echo (`{plugin, service, methods}`).
pub const METHOD_DESCRIBE: u32 = 0;

/// `{agent}` → `{cwd}` — the committed directory without filesystem
/// I/O (dsh `get`).
pub const METHOD_GET: u32 = 1;

/// `{agent, path}` → `{cwd}` — the changed directory (dsh `set`).
pub const METHOD_SET: u32 = 2;

/// `{agent}` → `{cwd}` — the validated or recovered directory
/// (dsh `ensure`).
pub const METHOD_ENSURE: u32 = 3;

/// The relay's fold selector for the `workingDirectory` projection
/// (the fr carrier numbering carries over from the 16f registration
/// face).
pub const METHOD_FOLD: u32 = 900;

/// The prompt registry's context-text selector (the fr carrier for the
/// dsh `systemPrompt.context` text plus its assemble prepend listener).
pub const METHOD_CONTEXT_TEXT: u32 = 906;
