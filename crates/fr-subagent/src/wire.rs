// Rust guideline compliant 2026-11-06
//! The fr-subagent wire vocabulary, single-sourced in the owner crate.
//!
//! The service face constants mirror the shared fr-wire protocol table
//! selector-for-selector (the batch-2a placement judgment), keeping the
//! owner crate the spelling authority for its own registrations.

use fr_wire::services::subagents as shared;

/// The dylib artifact name (the descriptor's `name`; the manifest row
/// resolves the artifact by the same stem).
pub const PLUGIN_NAME: &str = "fr-subagent";

/// The provided service name consumers inject.
pub const SERVICE_NAME: &str = shared::SERVICE_NAME;

/// Face metadata echo (`{plugin, service, methods}`).
pub const METHOD_DESCRIBE: u32 = shared::METHOD_DESCRIBE;
/// Start one managed child (the dsh `startActivation`).
pub const METHOD_START: u32 = shared::METHOD_START;
/// Interrupt one live child (the dsh `interrupt`).
pub const METHOD_INTERRUPT: u32 = shared::METHOD_INTERRUPT;
/// Steer one adjacent-Agent message (the dsh `sendMessage`).
pub const METHOD_SEND: u32 = shared::METHOD_SEND;
/// The headless runner's non-blocking descendant join probe.
pub const METHOD_WAIT_FOR_CHILDREN: u32 = shared::METHOD_WAIT_FOR_CHILDREN;
/// Read the durable direct-child catalog (the dsh `listChildren`).
pub const METHOD_LIST_CHILDREN: u32 = shared::METHOD_LIST_CHILDREN;
/// Recursively list reachable catalogs (the dsh `listDescendants`).
pub const METHOD_LIST_DESCENDANTS: u32 = shared::METHOD_LIST_DESCENDANTS;
/// The `subagent` tool body.
pub const METHOD_SUBAGENT_TOOL_EXECUTE: u32 = shared::METHOD_SUBAGENT_TOOL_EXECUTE;
/// The `subagent` tool render.
pub const METHOD_SUBAGENT_TOOL_RENDER: u32 = shared::METHOD_SUBAGENT_TOOL_RENDER;
/// The `subagent_fork` tool body.
pub const METHOD_SUBAGENT_FORK_TOOL_EXECUTE: u32 = shared::METHOD_SUBAGENT_FORK_TOOL_EXECUTE;
/// The `subagent_fork` tool render.
pub const METHOD_SUBAGENT_FORK_TOOL_RENDER: u32 = shared::METHOD_SUBAGENT_FORK_TOOL_RENDER;
/// The `send_message` tool body.
pub const METHOD_SEND_MESSAGE_TOOL_EXECUTE: u32 = shared::METHOD_SEND_MESSAGE_TOOL_EXECUTE;
/// The `send_message` tool render.
pub const METHOD_SEND_MESSAGE_TOOL_RENDER: u32 = shared::METHOD_SEND_MESSAGE_TOOL_RENDER;
/// The `interrupt_agent` tool body.
pub const METHOD_INTERRUPT_TOOL_EXECUTE: u32 = shared::METHOD_INTERRUPT_TOOL_EXECUTE;
/// The `interrupt_agent` tool render.
pub const METHOD_INTERRUPT_TOOL_RENDER: u32 = shared::METHOD_INTERRUPT_TOOL_RENDER;
/// The `list_agents` tool body.
pub const METHOD_LIST_AGENTS_TOOL_EXECUTE: u32 = shared::METHOD_LIST_AGENTS_TOOL_EXECUTE;
/// The `list_agents` tool render.
pub const METHOD_LIST_AGENTS_TOOL_RENDER: u32 = shared::METHOD_LIST_AGENTS_TOOL_RENDER;
/// The `list_subagent_models` tool body.
pub const METHOD_LIST_SUBAGENT_MODELS_TOOL_EXECUTE: u32 =
    shared::METHOD_LIST_SUBAGENT_MODELS_TOOL_EXECUTE;
/// The `list_subagent_models` tool render.
pub const METHOD_LIST_SUBAGENT_MODELS_TOOL_RENDER: u32 =
    shared::METHOD_LIST_SUBAGENT_MODELS_TOOL_RENDER;
/// The `subagentCatalog` projection fold relay.
pub const METHOD_FOLD_CATALOG: u32 = shared::METHOD_FOLD_CATALOG;
/// The `subagentTiming` projection fold relay.
pub const METHOD_FOLD_TIMING: u32 = shared::METHOD_FOLD_TIMING;
/// The `subagent` identity projection fold relay.
pub const METHOD_FOLD_IDENTITY: u32 = shared::METHOD_FOLD_IDENTITY;
/// The `agentPreset` projection fold relay.
pub const METHOD_FOLD_AGENT_PRESET: u32 = shared::METHOD_FOLD_AGENT_PRESET;
/// The `tool:subagent:depth` context text provider.
pub const METHOD_CONTEXT_DEPTH_TEXT: u32 = shared::METHOD_CONTEXT_DEPTH_TEXT;
/// The `subagent:delegation` context text provider.
pub const METHOD_CONTEXT_DELEGATION_TEXT: u32 = shared::METHOD_CONTEXT_DELEGATION_TEXT;
/// The `tool:structured_output` section text provider.
pub const METHOD_SECTION_STRUCTURED_TEXT: u32 = shared::METHOD_SECTION_STRUCTURED_TEXT;
/// The scoped structured-output tool-schema provider.
pub const METHOD_STRUCTURED_TOOLS: u32 = shared::METHOD_STRUCTURED_TOOLS;

/// The parent's durable direct-child record (dsh `subagent/catalog`).
pub const CATALOG_EVENT: &str = "subagent/catalog";
/// The child's durable identity and continuation composition (dsh
/// `subagent/descriptor`).
pub const DESCRIPTOR_EVENT: &str = "subagent/descriptor";
/// The independent preset selection record (dsh `agent-preset/selected`).
pub const PRESET_SELECTED_EVENT: &str = "agent-preset/selected";
/// The delegated sandbox-mode override (dsh `sandbox/mode`;
/// fr-sandbox declares the type, the manager appends the event).
pub const SANDBOX_MODE_EVENT: &str = "sandbox/mode";
/// The pinned never-approval delegation record (dsh `approval/policy`;
/// fr-approval declares the type).
pub const APPROVAL_POLICY_EVENT: &str = "approval/policy";
/// The inherited permission preset record (dsh `permission/preset`;
/// fr-permission-presets declares the type).
pub const PERMISSION_PRESET_EVENT: &str = "permission/preset";

/// The lifecycle start edge (dsh `subagent/start`).
pub const EVENT_START: &str = "subagent/start";
/// The lifecycle terminal edge (dsh `subagent/end`).
pub const EVENT_END: &str = "subagent/end";
/// The loop status flip this manager's settlement watcher joins (the
/// decision-46 carrier face).
pub const EVENT_AGENT_STATUS: &str = "agent/status";
/// The loop's failure boundary the result fold joins.
pub const EVENT_AGENT_ERROR: &str = "agent/error";

/// The durable `subagent-settled` message source kind (dsh
/// `continuation-messages.ts`).
pub const SOURCE_SUBAGENT_SETTLED: &str = "subagent-settled";
/// The durable `agent-message` relay source kind.
pub const SOURCE_AGENT_MESSAGE: &str = "agent-message";
