// Rust guideline compliant 2026-11-06
//! The dylib plugin shell: the descriptor, the activation-time
//! registrations, the service dispatch, and the settlement watcher's
//! event entries.
//!
//! The descriptor provides `subagents`, injects the seven consumed
//! faces, and listens for the two loop events the settlement watcher
//! joins (`agent/status` flips and `agent/error` boundaries). The
//! activation declines fail-closed when any registration refuses (the
//! fr-llm-retry judgment): a half-wired subagent owner must not exist.
//!
//! This module is the crate's only `dougong-abi` consumer.

use std::sync::Arc;

use dougong_abi::{HostExports, STATUS_CAPACITY, STATUS_FAILED, STATUS_OK, write_out};
use serde_json::{Value, json};

use crate::manager::{Capabilities, Config, Manager, Provider, ProviderKind, Seams};
use crate::outbound::{Outbound, WireSeams, register_all};
use crate::runtime::Runtime;
use crate::wire;

/// The descriptor the loader consumes.
const DESCRIPTOR: &[u8] = concat!(
    "{\"name\":\"fr-subagent\",\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\",\"provide\":[\"subagents\"],\"inject\":[",
    "\"session-persistence\",\"session-query\",\"agent\",\"working-directory\",",
    "\"system-prompt\",\"tools\",\"session-projections\"],\"listen\":{",
    "\"agent/status\":1,\"agent/error\":2}}"
)
.as_bytes();

/// Local listen-table id: one `agent/status` flip (the settlement
/// watcher's idle entry).
const EVENT_STATUS_ID: u32 = 1;
/// Local listen-table id: one `agent/error` boundary failure.
const EVENT_ERROR_ID: u32 = 2;

/// The shell state: the runtime service face over the continuation
/// manager plus the copied outbound seam for the lifecycle emits.
pub struct ShellState {
    runtime: Runtime,
}

impl std::fmt::Debug for ShellState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellState").finish_non_exhaustive()
    }
}

impl ShellState {
    /// Serves one `subagents` method invocation.
    ///
    /// # Errors
    ///
    /// The payload decode refusal, an unknown method, or the coded
    /// domain refusal (the host surfaces it as the `STATUS_FAILED`
    /// `{"error": <text>}` detail).
    pub fn serve(&self, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
        let request: Value = if payload.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(payload)
                .map_err(|error| format!("subagents payload must be JSON: {error}"))?
        };
        let reply = match method {
            wire::METHOD_DESCRIBE => json!({
                "name": wire::PLUGIN_NAME,
                "service": wire::SERVICE_NAME,
                "abiVersion": dougong_abi::ABI_VERSION,
            }),
            wire::METHOD_START => self.runtime.start(&request).map_err(error_text)?,
            wire::METHOD_INTERRUPT => self.runtime.interrupt(&request).map_err(error_text)?,
            wire::METHOD_SEND => self.runtime.send(&request).map_err(error_text)?,
            wire::METHOD_WAIT_FOR_CHILDREN => self.runtime.wait_for_children(&request),
            wire::METHOD_LIST_CHILDREN => {
                self.runtime.list_children(&request).map_err(error_text)?
            }
            wire::METHOD_LIST_DESCENDANTS => self
                .runtime
                .list_descendants(&request)
                .map_err(error_text)?,
            wire::METHOD_SUBAGENT_TOOL_EXECUTE | wire::METHOD_SUBAGENT_FORK_TOOL_EXECUTE => {
                let provider = if method == wire::METHOD_SUBAGENT_TOOL_EXECUTE {
                    "spawn"
                } else {
                    "fork"
                };
                let context = request.get("context").cloned().unwrap_or_else(|| json!({}));
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                let caller = context
                    .get("agent")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "subagent tool requires a calling agent".to_owned())?;
                let prompt = args
                    .get("prompt")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "subagent payload requires prompt".to_owned())?;
                let description = args
                    .get("description")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "subagent payload requires description".to_owned())?;
                let body = json!({
                    "provider": provider,
                    "label": description,
                    "request": {
                        "parentId": caller,
                        "prompt": [{ "type": "text", "text": prompt }],
                        "cwd": args.get("cwd").and_then(Value::as_str),
                    },
                });
                self.runtime.start(&body).map_err(error_text)?
            }
            wire::METHOD_SEND_MESSAGE_TOOL_EXECUTE => {
                let caller = request
                    .get("agent")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "send_message requires a calling agent".to_owned())?;
                let target = request
                    .get("args")
                    .and_then(|args| args.get("agent_id"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| "send_message payload requires agent_id".to_owned())?;
                let message = request
                    .get("args")
                    .and_then(|args| args.get("message"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| "send_message payload requires message".to_owned())?;
                self.runtime
                    .send(&json!({
                        "sender": caller,
                        "targetId": target,
                        "content": [{ "type": "text", "text": message }],
                    }))
                    .map_err(error_text)?
            }
            wire::METHOD_INTERRUPT_TOOL_EXECUTE => {
                let caller = request
                    .get("agent")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "interrupt_agent requires a calling agent".to_owned())?;
                let target = request
                    .get("args")
                    .and_then(|args| args.get("agent_id"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| "interrupt_agent payload requires agent_id".to_owned())?;
                self.runtime
                    .interrupt(&json!({
                        "targetSessionId": target,
                        "authority": { "kind": "ancestor", "agent": caller },
                    }))
                    .map_err(error_text)?
            }
            wire::METHOD_LIST_AGENTS_TOOL_EXECUTE => self
                .runtime
                .list_agents_execute(&request)
                .map_err(error_text)?,
            wire::METHOD_LIST_SUBAGENT_MODELS_TOOL_EXECUTE => json!({
                "error": "list_subagent_models requires the model-selection setting",
            }),
            wire::METHOD_SUBAGENT_TOOL_RENDER | wire::METHOD_SUBAGENT_FORK_TOOL_RENDER => {
                let value = request.get("value").cloned().unwrap_or_else(|| json!({}));
                let subagent_id = value
                    .get("subagentId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                json!(crate::tools::render_activation(subagent_id))
            }
            wire::METHOD_SEND_MESSAGE_TOOL_RENDER => {
                let args = request.get("args").cloned().unwrap_or_else(|| json!({}));
                let agent_id = args
                    .get("agent_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                json!(crate::tools::render_delivered(agent_id))
            }
            wire::METHOD_INTERRUPT_TOOL_RENDER => {
                let args = request.get("args").cloned().unwrap_or_else(|| json!({}));
                let agent_id = args
                    .get("agent_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                json!(crate::tools::render_interrupt(agent_id))
            }
            wire::METHOD_LIST_AGENTS_TOOL_RENDER => {
                let value = request.get("value").cloned().unwrap_or_else(|| json!([]));
                let text = value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string());
                json!([{ "type": "text", "text": text }])
            }
            wire::METHOD_FOLD_CATALOG => fold_catalog(&request),
            wire::METHOD_FOLD_TIMING => fold_projection(&request, crate::projection::fold_timing),
            wire::METHOD_FOLD_IDENTITY => {
                fold_projection(&request, crate::projection::fold_identity)
            }
            wire::METHOD_FOLD_AGENT_PRESET => {
                fold_projection(&request, crate::projection::fold_agent_preset)
            }
            wire::METHOD_CONTEXT_DEPTH_TEXT => {
                let context = request.get("context").cloned().unwrap_or_else(|| json!({}));
                let agent = context.get("agent");
                let session_id = context
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| {
                        agent
                            .and_then(|agent| agent.get("sessionId"))
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    });
                let depth = session_id
                    .as_deref()
                    .map(|id| self.runtime.manager.header_depth(id))
                    .unwrap_or(Ok(0))
                    .unwrap_or(0);
                let max_depth = self.runtime.resolve_max_depth().unwrap_or(0);
                Value::String(crate::tools::depth_context_text(
                    "subagent", depth, max_depth,
                ))
            }
            wire::METHOD_CONTEXT_DELEGATION_TEXT => {
                let session_id = request
                    .get("context")
                    .and_then(|context| context.get("sessionId"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let delegated = session_id
                    .as_deref()
                    .is_some_and(|id| self.runtime.manager.is_delegated_child(id));
                Value::String(if delegated {
                    crate::domain::SUBAGENT_DELEGATION_CONTEXT.to_owned()
                } else {
                    String::new()
                })
            }
            wire::METHOD_SECTION_STRUCTURED_TEXT => {
                let session_id = request
                    .get("context")
                    .and_then(|context| context.get("sessionId"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let structured = session_id
                    .as_deref()
                    .and_then(|id| self.runtime.manager.structured_runtime(id));
                Value::String(match structured {
                    Some(_) => crate::structured::STRUCTURED_OUTPUT_INSTRUCTION.to_owned(),
                    None => String::new(),
                })
            }
            wire::METHOD_STRUCTURED_TOOLS => {
                let session_id = request
                    .get("context")
                    .and_then(|context| context.get("sessionId"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let schemas = session_id
                    .as_deref()
                    .and_then(|id| self.runtime.manager.structured_runtime(id))
                    .map(|runtime| vec![runtime.lock().expect("structured lock").tool_schema()])
                    .unwrap_or_default();
                json!({ "tools": schemas })
            }
            other => {
                return Err(format!("fr-subagent provides no service method {other}"));
            }
        };
        serde_json::to_vec(&reply).map_err(|error| format!("reply encode failed: {error}"))
    }

    /// The settlement watcher's event entries (the listen map's two
    /// ids): a status flip to idle settles a resident child; an error
    /// boundary marks the failure the result fold reads.
    pub fn on_status(&self, payload: &[u8]) {
        let Ok(payload) = serde_json::from_slice::<Value>(payload) else {
            return;
        };
        let Some(session_id) = payload
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return;
        };
        if payload.get("status").and_then(Value::as_str) == Some("idle") {
            self.runtime.manager.settle_child(&session_id);
        }
    }

    /// The error-boundary entry: marks the live failure (dsh
    /// `observeLocalFailure`).
    pub fn on_error(&self, payload: &[u8]) {
        let Ok(payload) = serde_json::from_slice::<Value>(payload) else {
            return;
        };
        let Some(session_id) = payload
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return;
        };
        self.runtime.manager.observe_error(&session_id);
    }
}

/// The coded refusal's message text (the dynamic-channel carrier).
fn error_text(error: crate::domain::SubagentError) -> String {
    format!("{}: {}", error.code, error.message)
}

/// Folds the catalog projection state (the relay's read face).
fn fold_catalog(request: &Value) -> Value {
    let inherited = request
        .get("inheritedEventCount")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let mut state: Option<Value> = Some(json!({ "inheritedEventCount": inherited }));
    if let Some(events) = request.get("events").and_then(Value::as_array) {
        for event in events {
            state = Some(crate::catalog::fold(state, event, inherited));
        }
    }
    json!({ "state": state.unwrap_or(Value::Null) })
}

/// Folds one pure projection over the relay's cold-log request.
fn fold_projection(request: &Value, fold: fn(Option<Value>, &Value) -> Value) -> Value {
    let mut state: Option<Value> = request.get("state").cloned();
    if let Some(events) = request.get("events").and_then(Value::as_array) {
        for event in events {
            state = Some(fold(state, event));
        }
    }
    json!({ "state": state.unwrap_or(Value::Null) })
}

pub(crate) unsafe extern "C" fn plugin_name(out: *mut u8, cap: usize, len: *mut usize) -> i32 {
    // SAFETY: the host guarantees a cap-sized out-buffer and a writable
    // len slot (export-table contract).
    if unsafe { write_out(wire::PLUGIN_NAME.as_bytes(), out, cap, len) } {
        STATUS_OK
    } else {
        STATUS_CAPACITY
    }
}

pub(crate) unsafe extern "C" fn descriptor(out: *mut u8, cap: usize, len: *mut usize) -> i32 {
    // SAFETY: same host buffer contract as plugin_name.
    if unsafe { write_out(DESCRIPTOR, out, cap, len) } {
        STATUS_OK
    } else {
        STATUS_CAPACITY
    }
}

pub(crate) unsafe extern "C" fn setup(host: *const HostExports) -> *mut std::os::raw::c_void {
    if host.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the host exports pointer is valid for the duration of
    // setup; the vtable is copied by value (v3 contract).
    let host = unsafe { *host };
    let outbound = Outbound::new(host);
    let seams: Arc<dyn Seams> = Arc::new(WireSeams::new(outbound));
    let manager = Arc::new(Manager::new(
        Arc::clone(&seams),
        Config {
            max_depth: 1,
            max_active: 8,
        },
    ));
    if let Err(error) = register_all(&outbound, &manager) {
        eprintln!("fr: subagent: {error}");
        return std::ptr::null_mut();
    }
    if let Err(error) = register_builtin_providers(&manager) {
        eprintln!("fr: subagent: {error}");
        return std::ptr::null_mut();
    }
    // The runtime owns the manager by value; the shell reaches the
    // manager's watch-face methods through the runtime.
    let runtime = Runtime::new(
        match Arc::try_unwrap(manager) {
            Ok(manager) => manager,
            Err(_) => {
                eprintln!("fr: subagent: manager Arc held during setup");
                return std::ptr::null_mut();
            }
        },
        crate::manager::ConfiguredDepth::HostDefault,
        false,
    );
    Box::into_raw(Box::new(ShellState { runtime })).cast::<std::ffi::c_void>()
}

/// The spawn provider's detached contribution: a fresh child starts
/// with no seed (dsh `subagent-spawn-in-process`).
fn spawn_seed(
    _seams: &dyn Seams,
    _parent_id: &str,
    _cwd: &str,
) -> Result<Option<Vec<Value>>, crate::domain::SubagentError> {
    Ok(None)
}

/// The fork provider's detached contribution: the parent's balanced
/// completed-turn prefix (dsh `subagent-fork-in-process`).
fn fork_seed(
    seams: &dyn Seams,
    parent_id: &str,
    _cwd: &str,
) -> Result<Option<Vec<Value>>, crate::domain::SubagentError> {
    let observation = seams
        .observe(parent_id)
        .map_err(|detail| crate::domain::SubagentError::new("PARENT_UNAVAILABLE", detail))?;
    let last_end = observation
        .events
        .iter()
        .rposition(|event| event.get("type").and_then(Value::as_str) == Some("turn/end"));
    let Some(last_end) = last_end else {
        return Ok(None);
    };
    Ok(Some(observation.events[..=last_end].to_vec()))
}

/// Registers the two in-process providers (dsh base bundle's
/// `providerName` rows).
fn register_builtin_providers(manager: &Manager) -> Result<(), String> {
    let spawn_seed: crate::manager::SeedFn = Arc::new(spawn_seed);
    let fork_seed: crate::manager::SeedFn = Arc::new(fork_seed);
    let providers = [
        Provider {
            name: "spawn".to_owned(),
            capabilities: Capabilities {
                agent_options: true,
                agent_preset: true,
                output_schema: true,
                depth_limit: true,
            },
            inherits_parent_context: false,
            kind: ProviderKind::Local { seed: spawn_seed },
        },
        Provider {
            name: "fork".to_owned(),
            capabilities: Capabilities {
                agent_options: true,
                agent_preset: false,
                output_schema: true,
                depth_limit: true,
            },
            inherits_parent_context: true,
            kind: ProviderKind::Local { seed: fork_seed },
        },
    ];
    for provider in providers {
        manager
            .register_provider(provider)
            .map_err(|error| format!("{}: {}", error.code, error.message))?;
    }
    Ok(())
}

pub(crate) unsafe extern "C" fn call(
    handle: *mut std::os::raw::c_void,
    method: u32,
    in_data: *const u8,
    in_len: usize,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    if handle.is_null() {
        return STATUS_FAILED;
    }
    let payload: &[u8] = if in_len == 0 || in_data.is_null() {
        &[]
    } else {
        // SAFETY: `in_len` is nonzero and `in_data` non-null here, valid
        // for `in_len` readable bytes per the host contract.
        unsafe { std::slice::from_raw_parts(in_data, in_len) }
    };
    // SAFETY: the handle was produced by `setup` and is valid until the
    // matching `teardown`; the host serializes calls per the
    // export-table contract.
    let state = unsafe { &*(handle.cast::<ShellState>()) };
    let reply = match state.serve(method, payload) {
        Ok(reply) => reply,
        Err(text) => {
            let body = serde_json::json!({ "error": text }).to_string();
            // SAFETY: host buffer contract.
            let _ = unsafe { write_out(body.as_bytes(), out, cap, out_len) };
            return STATUS_FAILED;
        }
    };
    // SAFETY: host buffer contract.
    if unsafe { write_out(&reply, out, cap, out_len) } {
        STATUS_OK
    } else {
        STATUS_CAPACITY
    }
}

pub(crate) unsafe extern "C" fn on_event(
    handle: *mut std::os::raw::c_void,
    event: u32,
    _mode: u32,
    in_data: *const u8,
    in_len: usize,
    _next_available: u32,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    if handle.is_null() {
        return STATUS_FAILED;
    }
    let payload: &[u8] = if in_len == 0 || in_data.is_null() {
        &[]
    } else {
        // SAFETY: same host contract as `call`.
        unsafe { std::slice::from_raw_parts(in_data, in_len) }
    };
    // SAFETY: same handle contract as `call`.
    let state = unsafe { &*(handle.cast::<ShellState>()) };
    match event {
        EVENT_STATUS_ID => state.on_status(payload),
        EVENT_ERROR_ID => state.on_error(payload),
        _ => {}
    }
    let body = b"{}";
    // SAFETY: host buffer contract.
    if unsafe { write_out(body, out, cap, out_len) } {
        STATUS_OK
    } else {
        STATUS_CAPACITY
    }
}

pub(crate) const unsafe extern "C" fn on_next(
    _handle: *mut std::os::raw::c_void,
    _event: u32,
    _chain_status: i32,
    _data: *const u8,
    _len: usize,
    _out: *mut u8,
    _cap: usize,
    _out_len: *mut usize,
) -> i32 {
    // No waterfall listeners are declared, so the host never defers
    // into this continuation.
    STATUS_FAILED
}

pub(crate) unsafe extern "C" fn teardown(handle: *mut std::os::raw::c_void) {
    if handle.is_null() {
        return;
    }
    // SAFETY: the handle was produced by `setup` and is torn down
    // exactly once. No plugin-owned threads exist to join.
    drop(unsafe { Box::from_raw(handle.cast::<ShellState>()) });
}
