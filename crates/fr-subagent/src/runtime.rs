// Rust guideline compliant 2026-11-06
//! The service face over the continuation manager (dsh
//! `SubagentRuntime`'s published methods that cross the wire): start,
//! interrupt, send, the join probe, and the two listing faces — each a
//! JSON-in/JSON-out arm the plugin shell dispatches.

use serde_json::{Value, json};

use crate::control::{self, Scope};
use crate::domain::SubagentError;
use crate::manager::{ConfiguredDepth, InterruptAuthority, Manager, StartRequest};
use crate::{catalog, depth, descriptor};

/// The service state: the manager plus the tool-depth configuration.
#[derive(Debug)]
pub struct Runtime {
    /// The continuation manager.
    pub manager: Manager,
    /// The delegation tool's configured depth policy.
    tool_depth: ConfiguredDepth,
    /// The model-selection capability (the `list_subagent_models`
    /// mount decision).
    model_selection: bool,
}

impl Runtime {
    /// One runtime over a built manager.
    #[must_use]
    pub fn new(manager: Manager, tool_depth: ConfiguredDepth, model_selection: bool) -> Self {
        Self {
            manager,
            tool_depth,
            model_selection,
        }
    }

    /// Resolves the tool's depth policy against the host setting (dsh
    /// `resolveMaxDepth`).
    #[must_use]
    pub fn resolve_max_depth(&self) -> Option<u64> {
        match self.tool_depth {
            ConfiguredDepth::ProviderManaged => None,
            ConfiguredDepth::Numeric(value) => Some(value),
            ConfiguredDepth::HostDefault => Some(self.manager.config().max_depth),
        }
    }

    /// Whether the model-selection tool mounts (dsh
    /// `modelSelectionSettings`).
    #[must_use]
    pub const fn model_selection_enabled(&self) -> bool {
        self.model_selection
    }

    /// Serves `startActivation` (`METHOD_START`).
    ///
    /// # Errors
    ///
    /// The coded refusal JSON body.
    pub fn start(&self, request: &Value) -> Result<Value, SubagentError> {
        let provider = string_field(request, "provider")?;
        let label = string_field(request, "label")?;
        let body = request.get("request").cloned().unwrap_or_else(|| json!({}));
        let parent_id = string_field(&body, "parentId")?.to_owned();
        let prompt = body
            .get("prompt")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let receipt = self.manager.start_local(&StartRequest {
            provider: provider.to_owned(),
            label: label.to_owned(),
            child_id: optional_string(request, "childId"),
            parent_id,
            prompt,
            cwd: optional_string(&body, "cwd"),
            agent_preset: optional_string(&body, "agentPreset"),
            route: crate::manager::ChildRoute {
                provider: optional_string(&body, "routeProvider"),
                model: optional_string(&body, "routeModel"),
                reasoning_effort: body.get("routeReasoningEffort").cloned(),
                max_tokens: body.get("routeMaxTokens").and_then(Value::as_u64),
            },
            output_schema: body.get("outputSchema").cloned(),
            max_depth: body.get("maxDepth").and_then(Value::as_u64),
        })?;
        Ok(json!({
            "childId": receipt.child_id,
            "messageId": receipt.message_id,
        }))
    }

    /// Serves `interrupt` (`METHOD_INTERRUPT`).
    ///
    /// # Errors
    ///
    /// The authority refusal JSON body.
    pub fn interrupt(&self, request: &Value) -> Result<Value, SubagentError> {
        let target = string_field(request, "targetSessionId")?.to_owned();
        let authority = request.get("authority").cloned().unwrap_or(Value::Null);
        let authority = match authority.get("kind").and_then(Value::as_str) {
            Some("user") => InterruptAuthority::User {
                parent_session_id: string_field(&authority, "parentSessionId")?.to_owned(),
            },
            _ => InterruptAuthority::Ancestor {
                agent: string_field(&authority, "agent")?.to_owned(),
            },
        };
        self.manager.interrupt(&target, &authority)?;
        Ok(json!({ "accepted": true }))
    }

    /// Serves `sendMessage` (`METHOD_SEND`).
    ///
    /// # Errors
    ///
    /// The adjacency or admission refusal JSON body.
    pub fn send(&self, request: &Value) -> Result<Value, SubagentError> {
        let sender = string_field(request, "sender")?.to_owned();
        let target = string_field(request, "targetId")?.to_owned();
        let content = request
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let message_id = self.manager.send_message(&sender, &target, &content)?;
        Ok(json!({ "messageId": message_id }))
    }

    /// Serves the runner's join probe (`METHOD_WAIT_FOR_CHILDREN`).
    #[must_use]
    pub fn wait_for_children(&self, request: &Value) -> Value {
        let session_id = request
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        json!({ "progressing": self.manager.descendants_progressing(session_id) })
    }

    /// Serves `listChildren` (`METHOD_LIST_CHILDREN`).
    ///
    /// # Errors
    ///
    /// The query refusal JSON body.
    pub fn list_children(&self, request: &Value) -> Result<Value, SubagentError> {
        let parent = string_field(request, "parentSessionId")?.to_owned();
        let entries = self.manager.list_children(&parent)?;
        Ok(json!({ "entries": entries
            .iter()
            .map(catalog_entry_json)
            .collect::<Vec<_>>() }))
    }

    /// Serves `listDescendants` (`METHOD_LIST_DESCENDANTS`).
    ///
    /// # Errors
    ///
    /// The root query refusal JSON body.
    pub fn list_descendants(&self, request: &Value) -> Result<Value, SubagentError> {
        let root = string_field(request, "rootSessionId")?.to_owned();
        let rows = self.manager.list_descendants(&root)?;
        Ok(json!({ "entries": rows.iter().map(control::output_row).collect::<Vec<_>>() }))
    }

    /// Serves the descendant listing tool body (`METHOD_LIST_AGENTS_TOOL_EXECUTE`).
    ///
    /// # Errors
    ///
    /// The listing refusal JSON body.
    pub fn list_agents_execute(&self, request: &Value) -> Result<Value, SubagentError> {
        let parent = string_field(request, "agent")?.to_owned();
        let scope = Scope::resolve(request.get("args").unwrap_or(&Value::Null));
        let text = match scope {
            Scope::Children => {
                let entries = self.manager.list_children(&parent)?;
                let rows: Vec<control::Row> = entries
                    .iter()
                    .filter_map(|entry| {
                        let status = if self.manager.is_running(&entry.id) {
                            "running"
                        } else {
                            "inactive"
                        };
                        control::project_child(entry, status, None)
                    })
                    .collect();
                control::render(&rows, Scope::Children)
            }
            Scope::Descendants => {
                let rows = self.manager.list_descendants(&parent)?;
                control::render(&rows, Scope::Descendants)
            }
        };
        Ok(json!({ "text": text }))
    }
}

/// The catalog entry's tool-wire shape.
fn catalog_entry_json(entry: &catalog::CatalogEntry) -> Value {
    let mut value = json!({
        "id": entry.id,
        "createdAt": entry.created_at,
        "mode": entry.mode,
    });
    if let Some(label) = &entry.label {
        value["label"] = json!(label);
    }
    value
}

/// Reads one required string field.
fn string_field<'a>(request: &'a Value, field: &str) -> Result<&'a str, SubagentError> {
    request
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| SubagentError::new("BAD_REQUEST", format!("payload requires {field}")))
}

/// Reads one optional string field.
fn optional_string(request: &Value, field: &str) -> Option<String> {
    request
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Re-exports the pure helpers the tool arms compose (the depth and
/// descriptor vocabulary the tests pin through the runtime face).
pub use depth::{delegation_depth_of, resolve_child_depth};
pub use descriptor::{DESCRIPTOR_VERSION, fold as fold_descriptor};
