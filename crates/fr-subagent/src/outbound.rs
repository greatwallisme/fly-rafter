// Rust guideline compliant 2026-11-06
//! The plugin's outbound faces: the copied `HostExports` require seam
//! (the fr-llm-retry `Outbound` twin) and the [`crate::manager::Seams`]
//! wire implementation over the consumed services.
//!
//! Every require is the synchronous dynamic-channel call the kernel
//! documents; the error carrier locates the `{"error": <text>}` body
//! the fr-approval spelling established.

use dougong_abi::{HostExports, STATUS_CAPACITY, STATUS_OK};
use fr_wire::services::{
    agent, approval, session_persistence, session_projections, session_query, system_prompt, tools,
};
use fr_working_directory::wire as working_directory;
use serde_json::{Value, json};

use crate::manager::{ChildCreate, ChildRoute, Observation, Seams};
use crate::wire;

/// The host protocol's documented out-buffer ceiling.
const MAX_REPLY_CAPACITY: usize = 16 * 1024 * 1024;

/// The copied vtable wrapper (per-activation lifetime).
#[derive(Debug, Clone, Copy)]
pub struct Outbound(HostExports);

impl Outbound {
    /// Wraps the vtable copied at `setup`.
    #[must_use]
    pub const fn new(host: HostExports) -> Self {
        Self(host)
    }

    /// Performs one synchronous cross-plugin service call with the
    /// caller-owned growable buffer protocol.
    ///
    /// # Errors
    ///
    /// The host status plus the JSON detail document the host delivered.
    pub fn require(
        &self,
        service: &str,
        method: u32,
        body: &[u8],
    ) -> Result<Vec<u8>, (i32, String)> {
        let mut buffer = vec![0_u8; 4096];
        loop {
            let mut written: usize = 0;
            // SAFETY: the copied vtable is live for the activation; the
            // call runs on a host-invoked foreign entry's thread with an
            // empty foreign stack (no reentrancy).
            let status = unsafe {
                (self.0.require)(
                    self.0.host_handle,
                    service.as_ptr(),
                    service.len(),
                    method,
                    body.as_ptr(),
                    body.len(),
                    buffer.as_mut_ptr(),
                    buffer.len(),
                    &raw mut written,
                )
            };
            match status {
                STATUS_OK => {
                    buffer.truncate(written);
                    return Ok(buffer);
                }
                STATUS_CAPACITY => {
                    if buffer.len() >= MAX_REPLY_CAPACITY {
                        return Err((
                            status,
                            "out-buffer exhausted at the documented ceiling".to_owned(),
                        ));
                    }
                    buffer.resize(buffer.len() * 2, 0);
                }
                failed => {
                    let detail =
                        String::from_utf8_lossy(&buffer[..written.min(buffer.len())]).to_string();
                    return Err((failed, detail));
                }
            }
        }
    }

    /// Publishes one fire-and-forget lifecycle edge.
    pub fn emit(&self, event: &str, payload: &Value) {
        let bytes = serde_json::to_vec(payload).unwrap_or_default();
        // SAFETY: the copied vtable is live for the activation; both
        // byte strings are read-only for the call's duration.
        unsafe {
            let _ = (self.0.emit)(
                self.0.host_handle,
                event.as_ptr(),
                event.len(),
                bytes.as_ptr(),
                bytes.len(),
            );
        }
    }

    /// The refusal detail's message body (the fr-approval carrier).
    #[must_use]
    pub fn message_of(detail: &str) -> String {
        let start = detail.find("{\"error\":").unwrap_or(0);
        serde_json::from_str::<Value>(&detail[start..])
            .ok()
            .and_then(|parsed| {
                parsed
                    .get("error")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| detail.to_owned())
    }
}

// SAFETY: the ABI vtable contract allows require/emit from any plugin
// thread, concurrently; the copy stays valid until this activation's
// `teardown` returns (the fr-approval `HostLink` twin judgment).
unsafe impl Send for Outbound {}

// SAFETY: same contract: sharing one copy only issues concurrent vtable
// calls, which the same contract explicitly allows.
unsafe impl Sync for Outbound {}

/// The manager's seams over the wire.
#[derive(Debug, Clone, Copy)]
pub struct WireSeams {
    outbound: Outbound,
}

impl WireSeams {
    /// One seam set over one outbound vtable copy.
    #[must_use]
    pub const fn new(outbound: Outbound) -> Self {
        Self { outbound }
    }

    /// One JSON require with the error body located.
    fn require_json(&self, service: &str, method: u32, body: &Value) -> Result<Value, String> {
        let bytes =
            serde_json::to_vec(body).map_err(|error| format!("request encode failed: {error}"))?;
        let reply = self
            .outbound
            .require(service, method, &bytes)
            .map_err(|(_, detail)| Outbound::message_of(&detail))?;
        serde_json::from_slice(&reply).map_err(|error| format!("reply decode failed: {error}"))
    }

    /// The observation cut decoded from a read or observe reply.
    fn observation_of(parsed: Value) -> Observation {
        Observation {
            source: parsed
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("live")
                .to_owned(),
            header: parsed.get("header").cloned().unwrap_or(Value::Null),
            inherited_event_count: parsed
                .get("inheritedEventCount")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            events: parsed
                .get("events")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        }
    }
}

impl Seams for WireSeams {
    fn session_read(&self, id: &str) -> Result<Observation, String> {
        let parsed = self.require_json(
            session_persistence::SERVICE_NAME,
            session_persistence::METHOD_SESSION_READ,
            &json!({ "id": id }),
        )?;
        let mut observation = Self::observation_of(parsed);
        observation.source = "live".to_owned();
        Ok(observation)
    }

    fn request_header(&self, id: &str) -> Result<Option<Value>, String> {
        let parsed = self.require_json(
            session_persistence::SERVICE_NAME,
            session_persistence::METHOD_REQUEST_HEADER,
            &json!({ "id": id }),
        )?;
        Ok(parsed.get("header").cloned())
    }

    fn session_append(&self, id: &str, event_type: &str, data: &Value) -> Result<(), String> {
        self.require_json(
            session_persistence::SERVICE_NAME,
            session_persistence::METHOD_SESSION_APPEND,
            &json!({ "id": id, "type": event_type, "data": data }),
        )?;
        Ok(())
    }

    fn agent_live(&self, id: &str) -> Result<bool, String> {
        let parsed = self.require_json(
            agent::SERVICE_NAME,
            agent::METHOD_LIVE,
            &json!({ "sessionId": id }),
        )?;
        Ok(parsed.get("live").and_then(Value::as_bool).unwrap_or(false))
    }

    fn agent_status(&self, id: &str) -> Result<&'static str, String> {
        let parsed = self.require_json(
            agent::SERVICE_NAME,
            agent::METHOD_STATUS,
            &json!({ "sessionId": id }),
        )?;
        match parsed.get("status").and_then(Value::as_str) {
            Some("running") => Ok("running"),
            _ => Ok("idle"),
        }
    }

    fn agent_create(&self, create: &ChildCreate) -> Result<(), String> {
        let meta = create.meta.clone();
        let mut body = json!({
            "sessionId": create.session_id,
            "cwd": create.cwd,
            "meta": meta,
            "inheritedEventCount": create.inherited_event_count,
        });
        if let Some(seed) = &create.seed {
            body["seed"] = json!(seed);
        }
        if let Some(provider) = &create.route.provider {
            body["provider"] = json!(provider);
        }
        if let Some(model) = &create.route.model {
            body["model"] = json!(model);
        }
        if let Some(effort) = &create.route.reasoning_effort {
            body["reasoningEffort"] = effort.clone();
        }
        if let Some(max_tokens) = create.route.max_tokens {
            body["maxTokens"] = json!(max_tokens);
        }
        self.require_json(agent::SERVICE_NAME, agent::METHOD_CREATE, &body)?;
        Ok(())
    }

    fn agent_resume(
        &self,
        session_id: &str,
        route: &ChildRoute,
        _meta_patch: Option<Value>,
    ) -> Result<(), String> {
        let mut body = json!({ "sessionId": session_id });
        if let Some(provider) = &route.provider {
            body["provider"] = json!(provider);
        }
        if let Some(model) = &route.model {
            body["model"] = json!(model);
        }
        if let Some(effort) = &route.reasoning_effort {
            body["reasoningEffort"] = effort.clone();
        }
        if let Some(max_tokens) = route.max_tokens {
            body["maxTokens"] = json!(max_tokens);
        }
        self.require_json(agent::SERVICE_NAME, agent::METHOD_RESUME, &body)?;
        Ok(())
    }

    fn agent_followup(&self, id: &str, message: &Value) -> Result<(), String> {
        self.require_json(
            agent::SERVICE_NAME,
            agent::METHOD_FOLLOWUP,
            &json!({ "sessionId": id, "message": message }),
        )?;
        Ok(())
    }

    fn agent_steer(&self, id: &str, message: &Value) -> Result<(), String> {
        self.require_json(
            agent::SERVICE_NAME,
            agent::METHOD_STEER,
            &json!({ "sessionId": id, "message": message }),
        )?;
        Ok(())
    }

    fn agent_inject(&self, id: &str, message: &Value) -> Result<(), String> {
        self.require_json(
            agent::SERVICE_NAME,
            agent::METHOD_INJECT,
            &json!({ "sessionId": id, "message": message }),
        )?;
        Ok(())
    }

    fn agent_cancel(&self, id: &str, cause: &Value, keep_inbox: bool) -> Result<(), String> {
        self.require_json(
            agent::SERVICE_NAME,
            agent::METHOD_CANCEL,
            &json!({ "sessionId": id, "cause": cause, "keepInbox": keep_inbox }),
        )?;
        Ok(())
    }

    fn session_flush(&self, id: &str) -> Result<(), String> {
        self.require_json(
            session_persistence::SERVICE_NAME,
            session_persistence::METHOD_SESSION_FLUSH,
            &json!({ "id": id }),
        )?;
        Ok(())
    }

    fn persisted_exists(&self, id: &str) -> Result<bool, String> {
        let parsed = self.require_json(
            session_persistence::SERVICE_NAME,
            session_persistence::METHOD_PERSISTED_COLD_READ,
            &json!({ "id": id }),
        )?;
        Ok(!parsed
            .get("absent")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    fn observe(&self, id: &str) -> Result<Observation, String> {
        let parsed = self.require_json(
            session_query::SERVICE_NAME,
            session_query::METHOD_OBSERVE,
            &json!({ "id": id }),
        )?;
        Ok(Self::observation_of(parsed))
    }

    fn projection_state(&self, id: &str, key: &str) -> Result<Option<Value>, String> {
        let parsed = self.require_json(
            session_projections::SERVICE_NAME,
            session_projections::METHOD_STATE_OF,
            &json!({ "id": id, "key": key }),
        )?;
        Ok(parsed.get("state").cloned())
    }

    fn working_directory_ensure(&self, agent_id: &str) -> Result<String, String> {
        let parsed = self.require_json(
            working_directory::SERVICE_NAME,
            working_directory::METHOD_ENSURE,
            &json!({ "agent": agent_id }),
        )?;
        parsed
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "working-directory ensure reply carries no cwd".to_owned())
    }

    fn working_directory_set(&self, agent_id: &str, path: &str) -> Result<(), String> {
        self.require_json(
            working_directory::SERVICE_NAME,
            working_directory::METHOD_SET,
            &json!({ "agent": agent_id, "path": path }),
        )?;
        Ok(())
    }

    fn approval_available(&self) -> Result<bool, String> {
        // The approval capability composes when its policy face answers;
        // a refused require reads as absent (the dsh optional-face read).
        let parsed = self.require_json(
            approval::SERVICE_NAME,
            approval::METHOD_DESCRIBE,
            &json!({}),
        );
        Ok(parsed.is_ok())
    }

    fn emit_lifecycle(&self, event: &str, payload: &Value) {
        self.outbound.emit(event, payload);
    }
}

/// The activation-time registration set: the durable vocabulary, the
/// four projection folds, the two context providers, the structured
/// section, the scoped structured tool provider, the six tool
/// registrations, and the lifecycle listener.
///
/// # Errors
///
/// The first refused registration (the activation declines fail-closed).
pub fn register_all(outbound: &Outbound, manager: &crate::manager::Manager) -> Result<(), String> {
    declare_vocabulary(outbound)?;
    register_projections(outbound)?;
    register_contexts(outbound)?;
    register_tools(outbound, manager)?;
    Ok(())
}

/// Declares the three owned durable event types.
fn declare_vocabulary(outbound: &Outbound) -> Result<(), String> {
    let bytes = serde_json::to_vec(&json!({
        "types": [
            wire::CATALOG_EVENT,
            wire::DESCRIPTOR_EVENT,
            wire::PRESET_SELECTED_EVENT,
        ],
    }))
    .map_err(|error| format!("declare encode failed: {error}"))?;
    // The realm child and the parent may both run this setup; the
    // session service accepts an already-declared type set (18f).
    match outbound.require(
        session_persistence::SERVICE_NAME,
        session_persistence::METHOD_DECLARE_EVENT_TYPES,
        &bytes,
    ) {
        Ok(_) => Ok(()),
        Err((_, detail)) if Outbound::message_of(&detail).contains("already declared") => Ok(()),
        Err((_, detail)) => Err(format!("declare: {}", Outbound::message_of(&detail))),
    }
}

/// Registers the four projection folds.
fn register_projections(outbound: &Outbound) -> Result<(), String> {
    let folds = [
        ("subagentCatalog", 6_u32, wire::METHOD_FOLD_CATALOG),
        ("subagentTiming", 3, wire::METHOD_FOLD_TIMING),
        ("subagent", 2, wire::METHOD_FOLD_IDENTITY),
        ("agentPreset", 1, wire::METHOD_FOLD_AGENT_PRESET),
    ];
    for (key, state_version, method) in folds {
        let bytes = serde_json::to_vec(&json!({
            "key": key,
            "stateVersion": state_version,
            "fold": { "service": wire::SERVICE_NAME, "method": method },
        }))
        .map_err(|error| format!("projection encode failed: {error}"))?;
        outbound
            .require(
                session_projections::SERVICE_NAME,
                session_projections::METHOD_REGISTER,
                &bytes,
            )
            .map_err(|(_, detail)| {
                format!("projection {key}: {}", Outbound::message_of(&detail))
            })?;
    }
    Ok(())
}

/// Registers the depth and delegation context text providers.
fn register_contexts(outbound: &Outbound) -> Result<(), String> {
    let contexts = [
        (
            "tool:subagent:depth",
            120.0,
            wire::METHOD_CONTEXT_DEPTH_TEXT,
        ),
        (
            "subagent:delegation",
            120.0,
            wire::METHOD_CONTEXT_DELEGATION_TEXT,
        ),
    ];
    for (name, order, method) in contexts {
        let bytes = serde_json::to_vec(&json!({
            "name": name,
            "order": order,
            "text": {
                "textProvider": { "service": wire::SERVICE_NAME, "method": method },
            },
            "interpolate": false,
            "required": false,
        }))
        .map_err(|error| format!("context encode failed: {error}"))?;
        outbound
            .require(
                system_prompt::SERVICE_NAME,
                system_prompt::METHOD_REGISTER_CONTEXT,
                &bytes,
            )
            .map_err(|(_, detail)| format!("context {name}: {}", Outbound::message_of(&detail)))?;
    }
    let section = serde_json::to_vec(&json!({
        "name": "tool:structured_output",
        "order": 9900.0,
        "text": {
            "textProvider": { "service": wire::SERVICE_NAME, "method": wire::METHOD_SECTION_STRUCTURED_TEXT },
        },
    }))
    .map_err(|error| format!("section encode failed: {error}"))?;
    outbound
        .require(
            system_prompt::SERVICE_NAME,
            system_prompt::METHOD_REGISTER_SECTION,
            &section,
        )
        .map_err(|(_, detail)| format!("section: {}", Outbound::message_of(&detail)))?;
    Ok(())
}

/// Registers the five global tool arms and the scoped structured tool
/// provider with the tools service and the system-prompt tool provider.
fn register_tools(outbound: &Outbound, _manager: &crate::manager::Manager) -> Result<(), String> {
    for spec in crate::tools::tool_registration_specs() {
        let bytes = serde_json::to_vec(&spec)
            .map_err(|error| format!("tool registration encode failed: {error}"))?;
        outbound
            .require(tools::SERVICE_NAME, tools::METHOD_REGISTER, &bytes)
            .map_err(|(_, detail)| format!("tool register: {}", Outbound::message_of(&detail)))?;
    }
    // The tool-schema provider the prompt assembly consults: the
    // scoped structured_output schema rides this endpoint.
    let provider = serde_json::to_vec(&json!({
        "provider": { "service": wire::SERVICE_NAME, "method": wire::METHOD_STRUCTURED_TOOLS },
    }))
    .map_err(|error| format!("tool provider encode failed: {error}"))?;
    outbound
        .require(
            system_prompt::SERVICE_NAME,
            system_prompt::METHOD_REGISTER_TOOLS,
            &provider,
        )
        .map_err(|(_, detail)| format!("tool provider: {}", Outbound::message_of(&detail)))?;
    Ok(())
}
