// Rust guideline compliant 2026-11-06
//! The six model-facing tool definitions (dsh `tool-subagent`,
//! `tool-subagent-control`, and `list-models.ts` 1:1): the schema
//! shapes, the verbatim descriptions, the render arms, and the
//! execution bodies over the runtime face.
//!
//! The tool schemas ride the dsh `docs/tool-catalog.md` rows the
//! builtin-tools contract freezes; the definitions here are pure data
//! plus thin manager calls so the definition-level pins bind without a
//! loaded composition.

use serde_json::{Value, json};

use crate::manager::ConfiguredDepth;
use crate::wire;

/// The delegation execution mode (the dsh `isConcurrencySafe` faces):
/// `subagent` is parallel-safe; every control and discovery tool is
/// exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Sibling calls may overlap.
    Parallel,
    /// One call at a time, fail-closed.
    Exclusive,
}

/// One registered tool definition (the dsh `defineTool` wire form).
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    /// The model-facing name.
    pub name: &'static str,
    /// The model-facing description.
    pub description: &'static str,
    /// The raw JSON-Schema parameters object.
    pub parameters: Value,
    /// The canonical output contract.
    pub output_schema: Value,
    /// The execution-mode classification.
    pub mode: ExecutionMode,
}

/// The spawn tool's standalone wording (dsh `providerWording(false)`,
/// verbatim).
pub const SUBAGENT_DESCRIPTION: &str = "Delegate a self-contained task to a subagent (a \
separate agent that works in its own context) to offload focused, independent work — research, \
a scoped implementation, an analysis — so it does not consume this conversation's context. The \
subagent returns its result, not its intermediate steps. This tool starts an independently \
managed subagent and immediately returns its id. The runtime notifies you when it finishes. The \
child reports results with `send_message`; use `send_message` to steer it while running or \
continue its conversation after it finishes.";

/// The fork tool's inherited-context wording (dsh `providerWording(true)`,
/// verbatim).
pub const SUBAGENT_FORK_DESCRIPTION: &str = "Delegate a task to a subagent that inherits this \
conversation: a child agent seeded with all completed turns so far (it does not see the current \
in-flight turn). Use this when the subtask builds on this conversation's context — a follow-up \
analysis, a review, a continuation — without consuming this conversation's context for the work \
itself. You receive its result, not its intermediate steps. This tool starts an independently \
managed subagent and immediately returns its id. The runtime notifies you when it finishes. The \
child reports results with `send_message`; use `send_message` to steer it while running or \
continue its conversation after it finishes.";

/// The `subagent` tool parameters (dsh tool-subagent schema; the fork
/// twin omits nothing — the two share the three-parameter shape in the
/// headless composition).
#[must_use]
pub fn delegation_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "cwd": {
                "type": "string",
                "description": "Initial child working directory. Relative paths use your current \
    directory; omitted inherits it. Later directory changes in either agent are independent.",
            },
            "description": {
                "type": "string",
                "description": "A short (3-5 word) description of the delegated task, for display.",
            },
            "prompt": {
                "type": "string",
                "description": "The complete, self-contained task for the subagent. It does not \
    share this conversation's context, so include everything it needs.",
            },
        },
        "required": ["description", "prompt"],
        "additionalProperties": false,
    })
}

/// The fork twin's prompt parameter reuses the inherited wording (dsh
/// `providerWording(true).promptDescription`, verbatim).
pub const FORK_PROMPT_DESCRIPTION: &str = "The task for the subagent. It already sees this \
conversation's completed turns, so build on them freely and state only what is new.";

/// The activation output contract (dsh `subagent` output schema).
#[must_use]
pub fn activation_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "kind": { "type": "string", "const": "activation" },
            "subagentId": { "type": "string" },
        },
        "required": ["kind", "subagentId"],
    })
}

/// The `send_message` definition (dsh tool-subagent-control, verbatim).
#[must_use]
pub fn send_message_definition() -> ToolDefinition {
    ToolDefinition {
        name: "send_message",
        description: "Send a message to an agent. A working agent receives it at its next \
step; an idle agent starts a new turn with it. Returns delivery confirmation, not the agent's \
answer.",
        parameters: json!({
            "type": "object",
            "properties": {
                "agent_id": {
                    "type": "string",
                    "description": "The agent id of your direct continuable child, or your \
        direct parent when you are a resident continuable child.",
                },
                "message": {
                    "type": "string",
                    "description": "The message to deliver to the agent.",
                },
            },
            "required": ["agent_id", "message"],
            "additionalProperties": false,
        }),
        output_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "messageId": { "type": "string" },
            },
            "required": ["messageId"],
        }),
        mode: ExecutionMode::Exclusive,
    }
}

/// The `interrupt_agent` definition (dsh tool-subagent-control, verbatim).
#[must_use]
pub fn interrupt_agent_definition() -> ToolDefinition {
    ToolDefinition {
        name: "interrupt_agent",
        description: "Ask a subagent to stop its current work. This call returns without \
waiting for it to stop. You can continue a local direct child's conversation later with \
send_message. External executions stop permanently and cannot receive follow-ups. Subagents it \
started will keep running.",
        parameters: json!({
            "type": "object",
            "properties": {
                "agent_id": {
                    "type": "string",
                    "description": "The id of an agent created under you: your direct child or \
        a deeper descendant.",
                },
            },
            "required": ["agent_id"],
            "additionalProperties": false,
        }),
        output_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "accepted": { "type": "boolean" },
            },
            "required": ["accepted"],
        }),
        mode: ExecutionMode::Exclusive,
    }
}

/// The `list_agents` definition (dsh list-agents.ts, verbatim).
#[must_use]
pub fn list_agents_definition() -> ToolDefinition {
    ToolDefinition {
        name: "list_agents",
        description: "List subagents you started, with their ids, labels, and status. running \
means it is working; inactive means it is not currently working. You will be notified when a \
subagent finishes; there is no need to keep checking its status. Use send_message to continue \
the conversation.",
        parameters: json!({
            "type": "object",
            "properties": {
                "scope": {
                    "type": "string",
                    "enum": ["children", "descendants"],
                    "description": "children (default) lists direct children, which accept \
        send_message in any status. descendants lists the whole tree below you with each entry's parent \
        session id and depth; entries deeper than 1 accept only interrupt_agent.",
                },
            },
            "additionalProperties": false,
        }),
        output_schema: json!({
            "type": "array",
        }),
        mode: ExecutionMode::Exclusive,
    }
}

/// The `list_subagent_models` definition (dsh list-models.ts, verbatim).
#[must_use]
pub fn list_subagent_models_definition() -> ToolDefinition {
    ToolDefinition {
        name: "list_subagent_models",
        description: "Discover LLM routes for subagents without changing the current Agent. \
Call with no arguments to list registered providers, with `provider` to list its advertised \
models, or with `provider` and `model` to inspect that exact model and its reasoning efforts. \
Catalog membership is advisory: an adapter may accept an unlisted model id. Use the returned \
ids with a delegation tool's `provider`, `model`, and `reasoning_effort` fields.",
        parameters: json!({
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "description": "Registered LLM provider id. Omit to list providers.",
                },
                "model": {
                    "type": "string",
                    "description": "Exact model id to inspect. Requires provider; omit to list \
        that provider's advertised models.",
                },
            },
            "additionalProperties": false,
        }),
        output_schema: json!({ "type": "string" }),
        mode: ExecutionMode::Exclusive,
    }
}

/// The `subagent` tool definition (the spawn provider binding).
#[must_use]
pub fn subagent_definition() -> ToolDefinition {
    ToolDefinition {
        name: "subagent",
        description: SUBAGENT_DESCRIPTION,
        parameters: delegation_parameters(),
        output_schema: activation_output_schema(),
        mode: ExecutionMode::Parallel,
    }
}

/// The `subagent_fork` tool definition (the fork provider binding).
#[must_use]
pub fn subagent_fork_definition() -> ToolDefinition {
    ToolDefinition {
        name: "subagent_fork",
        description: SUBAGENT_FORK_DESCRIPTION,
        parameters: fork_parameters(),
        output_schema: activation_output_schema(),
        mode: ExecutionMode::Parallel,
    }
}

/// The fork twin's parameters (the shared shape with the inherited
/// prompt wording).
#[must_use]
pub fn fork_parameters() -> Value {
    let mut parameters = delegation_parameters();
    parameters["properties"]["prompt"]["description"] = json!(FORK_PROMPT_DESCRIPTION);
    parameters
}

/// The `subagent` render arm (dsh: `started subagent <id>`).
#[must_use]
pub fn render_activation(subagent_id: &str) -> Vec<Value> {
    vec![json!({
        "type": "text",
        "text": format!("started subagent {subagent_id}"),
    })]
}

/// The `send_message` render arm (dsh: `message delivered to agent <id>`).
#[must_use]
pub fn render_delivered(agent_id: &str) -> Vec<Value> {
    vec![json!({
        "type": "text",
        "text": format!("message delivered to agent {agent_id}"),
    })]
}

/// The `interrupt_agent` render arm (dsh: `interrupt requested for agent <id>`).
#[must_use]
pub fn render_interrupt(agent_id: &str) -> Vec<Value> {
    vec![json!({
        "type": "text",
        "text": format!("interrupt requested for agent {agent_id}"),
    })]
}

/// The delegation-batch system-prompt section text (dsh
/// `tool:subagent` section, the M1 visible set verbatim).
#[must_use]
pub fn delegation_section_text() -> &'static str {
    "Start independent delegations with `subagent` or `subagent_fork` together in one assistant \
message and continue useful work while they run."
}

/// The depth-context text at the cap (dsh `tool:subagent:depth` text,
/// verbatim; empty below the cap).
#[must_use]
pub fn depth_context_text(tool_name: &str, depth: u64, max_depth: u64) -> String {
    if depth < max_depth {
        return String::new();
    }
    format!(
        "Delegation depth for `{tool_name}`: current {depth}, maximum {max_depth}, remaining \
         levels 0. Do not call `{tool_name}` at this depth; carry out the work directly."
    )
}

/// The mount-time provider configuration assertion (dsh
/// `assertSubagentProviderConfiguration`): a numeric maxDepth requires
/// the depthLimit capability.
///
/// # Errors
///
/// The verbatim mount refusal.
pub fn assert_provider_configuration(
    provider_name: &str,
    capabilities: crate::manager::Capabilities,
    tool_depth: ConfiguredDepth,
    host_max_depth: u64,
    wants_model_selection: bool,
) -> Result<(), String> {
    let numeric = match tool_depth {
        ConfiguredDepth::ProviderManaged => None,
        ConfiguredDepth::Numeric(value) => Some(value),
        ConfiguredDepth::HostDefault => Some(host_max_depth),
    };
    if numeric.is_some() && !capabilities.depth_limit {
        return Err(format!(
            "tool-subagent: provider \"{provider_name}\" cannot enforce maxDepth (no depthLimit \
             capability) — set maxDepth: 'provider-managed' to leave the recursion budget to the \
             provider"
        ));
    }
    if wants_model_selection && !capabilities.agent_options {
        return Err(format!(
            "tool-subagent: provider \"{provider_name}\" does not support child model selection"
        ));
    }
    Ok(())
}

/// The adjacent-tool mark this plugin's `send_message` carries (dsh
/// `markAdjacentAgentSendMessageTool`): the manager reads the mark to
/// decide the return-guidance arm.
pub const ADJACENT_TOOL_MARK: &str = "dsh.subagent.adjacentAgentSendMessageTool";

/// Lists the six tool wire registrations for the setup arm (the
/// registration documents the tools service consumes; the execute and
/// render endpoints point back at this plugin's service selectors).
#[must_use]
pub fn tool_registration_specs() -> Vec<Value> {
    let endpoint = |method: u32| json!({ "service": wire::SERVICE_NAME, "method": method });
    let spec = |name: &str, definition: &ToolDefinition, execute: u32, render: u32| {
        json!({
            "name": name,
            "description": definition.description,
            "parameters": definition.parameters,
            "output": {
                "schema": definition.output_schema,
                "render": endpoint(render),
            },
            "execute": endpoint(execute),
            "isConcurrencySafe": definition.mode == ExecutionMode::Parallel,
        })
    };
    vec![
        spec(
            "subagent",
            &subagent_definition(),
            wire::METHOD_SUBAGENT_TOOL_EXECUTE,
            wire::METHOD_SUBAGENT_TOOL_RENDER,
        ),
        spec(
            "subagent_fork",
            &subagent_fork_definition(),
            wire::METHOD_SUBAGENT_FORK_TOOL_EXECUTE,
            wire::METHOD_SUBAGENT_FORK_TOOL_RENDER,
        ),
        spec(
            "send_message",
            &send_message_definition(),
            wire::METHOD_SEND_MESSAGE_TOOL_EXECUTE,
            wire::METHOD_SEND_MESSAGE_TOOL_RENDER,
        ),
        spec(
            "interrupt_agent",
            &interrupt_agent_definition(),
            wire::METHOD_INTERRUPT_TOOL_EXECUTE,
            wire::METHOD_INTERRUPT_TOOL_RENDER,
        ),
        spec(
            "list_agents",
            &list_agents_definition(),
            wire::METHOD_LIST_AGENTS_TOOL_EXECUTE,
            wire::METHOD_LIST_AGENTS_TOOL_RENDER,
        ),
    ]
}

/// The structured-output tool registration (mounted per requesting
/// session through the scoped tools provider, not the global registry).
#[must_use]
pub fn structured_tool_schema(runtime: &crate::structured::StructuredRuntime) -> Value {
    runtime.tool_schema()
}

/// The manager seam the tool bodies call (kept as a free function so
/// the definition module stays pure).
pub trait ToolRuntime {
    /// Starts one activation (the `subagent`/`subagent_fork` bodies).
    fn start(&self, request: &Value) -> Result<Value, crate::domain::SubagentError>;
    /// The calling agent id of one execution context.
    fn caller(&self, context: &Value) -> Result<String, String>;
}

/// Executes the `subagent` tool body (dsh tool-subagent `execute`).
///
/// # Errors
///
/// The provider-resolution, capability, or start refusal.
pub fn execute_delegation(
    runtime: &dyn ToolRuntime,
    context: &Value,
    args: &Value,
    provider: &str,
) -> Result<Value, String> {
    let caller = runtime.caller(context)?;
    let prompt = args
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let request = json!({
        "provider": provider,
        "label": args.get("description").and_then(Value::as_str).unwrap_or_default(),
        "request": {
            "parentId": caller,
            "prompt": [{ "type": "text", "text": prompt }],
            "cwd": args.get("cwd").and_then(Value::as_str),
        },
    });
    let reply = runtime.start(&request).map_err(|error| error.message)?;
    let child_id = reply
        .get("childId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Ok(json!({ "kind": "activation", "subagentId": child_id }))
}

/// The `structured_output` guard check the manager's admission arm
/// consults (dsh `tools.guard` composition).
#[must_use]
pub fn structured_guard_text(
    structured: Option<&std::sync::Mutex<crate::structured::StructuredRuntime>>,
) -> Option<String> {
    structured.and_then(|runtime| runtime.lock().expect("structured lock").guard())
}

/// The tool name constant for the adjacent-messaging check.
#[must_use]
pub const fn adjacent_tool_name() -> &'static str {
    "send_message"
}

/// The wire vocabulary re-export the registration specs compose.
#[must_use]
pub const fn service_name() -> &'static str {
    wire::SERVICE_NAME
}
