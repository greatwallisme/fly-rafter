// Rust guideline compliant 2026-11-05
//! The plugin's outbound faces.
//!
//! The copied `HostExports` require seam (the fr-llm-retry `Outbound`
//! twin) plus the three domain seams implemented over the wire: the
//! session-log client (read/append), the agent-inject client, and the
//! directory leg over the `'fs'` service's resolve/stat/processPath
//! selectors.

use dougong_abi::{HostExports, STATUS_CAPACITY, STATUS_OK};
use serde_json::Value;

use crate::core::{AgentInject, ColdLog, DirectoryLeg, SessionLog};
use crate::wire::{CHANGE_EVENT, METHOD_CONTEXT_TEXT, METHOD_FOLD, SERVICE_NAME};
use fr_wire::services::{agent, fs, session_persistence, session_projections, system_prompt};

/// The copied vtable wrapper (per-activation lifetime: callable until
/// `teardown` returns; this plugin spawns no threads, so every call
/// happens on a host-invoked foreign entry's thread).
#[derive(Debug, Clone, Copy)]
pub struct Outbound(HostExports);

impl Outbound {
    /// Wraps the vtable copied at `setup`.
    #[must_use]
    pub const fn new(host: HostExports) -> Self {
        Self(host)
    }

    /// Performs one synchronous cross-plugin service call with the
    /// caller-owned growable buffer protocol (the host's documented
    /// 16 MiB ceiling; a `STATUS_CAPACITY` at that size is a protocol
    /// failure).
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

    /// Grants `target`'s committed provider the right to require this
    /// plugin's `granted` service (D31; registration only, no call).
    ///
    /// # Errors
    ///
    /// The host refusal status.
    pub fn grant(&self, target: &str, granted: &str) -> Result<(), String> {
        // SAFETY: the copied vtable is live for the activation; both
        // names are read-only byte strings for the call's duration.
        let status = unsafe {
            (self.0.grant)(
                self.0.host_handle,
                target.as_ptr(),
                target.len(),
                granted.as_ptr(),
                granted.len(),
            )
        };
        if status == STATUS_OK {
            Ok(())
        } else {
            Err(format!(
                "grant {granted} to {target} failed with host status {status}"
            ))
        }
    }

    /// The refusal detail's message body (the fr-approval carrier
    /// spells refusals as `{"error": <text>}`; the host may prefix the
    /// detail with its require failure line, so the carrier JSON is
    /// located by its opening brace and anything else passes through
    /// verbatim).
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

// SAFETY: the ABI vtable contract allows require from any plugin
// thread, concurrently; the copy stays valid until this activation's
// `teardown` returns, and the wrapper carries no other state (the
// fr-approval `HostLink` twin judgment).
unsafe impl Send for Outbound {}

// SAFETY: same contract: sharing one copy only issues concurrent vtable
// calls, which the same contract explicitly allows.
unsafe impl Sync for Outbound {}

/// The host protocol's documented out-buffer ceiling.
const MAX_REPLY_CAPACITY: usize = 16 * 1024 * 1024;

/// The durable change records over the `'session-persistence'` faces
/// (the dsh `agent.session.append`/read pair).
#[derive(Debug, Clone, Copy)]
pub struct WireSessionLog {
    outbound: Outbound,
}

impl WireSessionLog {
    /// One client over one outbound seam.
    #[must_use]
    pub const fn new(outbound: Outbound) -> Self {
        Self { outbound }
    }
}

impl SessionLog for WireSessionLog {
    fn read(&self, id: &str) -> Result<ColdLog, String> {
        let bytes = serde_json::to_vec(&serde_json::json!({ "id": id }))
            .map_err(|error| format!("session read encode failed: {error}"))?;
        let reply = self
            .outbound
            .require(
                session_persistence::SERVICE_NAME,
                session_persistence::METHOD_SESSION_READ,
                &bytes,
            )
            .map_err(|(_, detail)| Outbound::message_of(&detail))?;
        let parsed: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("session read decode failed: {error}"))?;
        Ok(ColdLog {
            header: parsed.get("header").cloned().unwrap_or(Value::Null),
            inherited_event_count: parsed
                .get("inheritedEventCount")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            events: parsed
                .get("events")
                .cloned()
                .unwrap_or(Value::Array(Vec::new())),
        })
    }

    fn append(&self, id: &str, cwd: &str) -> Result<(), String> {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "id": id,
            "type": CHANGE_EVENT,
            "data": { "cwd": cwd },
        }))
        .map_err(|error| format!("session append encode failed: {error}"))?;
        self.outbound
            .require(
                session_persistence::SERVICE_NAME,
                session_persistence::METHOD_SESSION_APPEND,
                &bytes,
            )
            .map(|_| ())
            .map_err(|(_, detail)| Outbound::message_of(&detail))
    }
}

/// The model notices over the `'agent'` inject face (the dsh
/// `agent.inject(notice)` pair).
#[derive(Debug, Clone, Copy)]
pub struct WireAgentInject {
    outbound: Outbound,
}

impl WireAgentInject {
    /// One client over one outbound seam.
    #[must_use]
    pub const fn new(outbound: Outbound) -> Self {
        Self { outbound }
    }
}

impl AgentInject for WireAgentInject {
    fn inject(&self, agent: &str, message: &Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "sessionId": agent,
            "message": message,
        }))
        .map_err(|error| format!("notice encode failed: {error}"))?;
        self.outbound
            .require(agent::SERVICE_NAME, agent::METHOD_INJECT, &bytes)
            .map(|_| ())
            .map_err(|(_, detail)| Outbound::message_of(&detail))
    }
}

/// The directory leg over the `'fs'` service (the dsh
/// `ctx.fs.resolve/stat/processPath` consumption; the opaque target
/// token is the fs face's target JSON spelled as a string).
#[derive(Debug, Clone, Copy)]
pub struct WireDirectoryLeg {
    outbound: Outbound,
}

impl WireDirectoryLeg {
    /// One leg over one outbound seam.
    #[must_use]
    pub const fn new(outbound: Outbound) -> Self {
        Self { outbound }
    }

    /// The fs face's target document for one token (the round trip
    /// decode).
    fn target_of(token: &str) -> Result<Value, String> {
        serde_json::from_str(token).map_err(|error| format!("fs target decode failed: {error}"))
    }
}

impl DirectoryLeg for WireDirectoryLeg {
    fn resolve(&self, path: &str, cwd: Option<&str>) -> Result<String, String> {
        let mut request = serde_json::json!({ "path": path });
        if let Some(cwd) = cwd {
            request["cwd"] = Value::String(cwd.to_owned());
        }
        let bytes = serde_json::to_vec(&request)
            .map_err(|error| format!("fs resolve encode failed: {error}"))?;
        let reply = self
            .outbound
            .require(fs::SERVICE_NAME, fs::METHOD_RESOLVE, &bytes)
            .map_err(|(_, detail)| Outbound::message_of(&detail))?;
        let parsed: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("fs resolve decode failed: {error}"))?;
        let target = parsed
            .get("target")
            .cloned()
            .ok_or_else(|| "fs resolve reply carries no target".to_owned())?;
        serde_json::to_string(&target).map_err(|error| format!("fs target encode failed: {error}"))
    }

    fn stat_is_directory(&self, target: &str) -> Result<Option<bool>, String> {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "target": Self::target_of(target)?,
        }))
        .map_err(|error| format!("fs stat encode failed: {error}"))?;
        let reply = self
            .outbound
            .require(fs::SERVICE_NAME, fs::METHOD_STAT, &bytes)
            .map_err(|(_, detail)| Outbound::message_of(&detail))?;
        let parsed: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("fs stat decode failed: {error}"))?;
        Ok(parsed
            .get("info")
            .filter(|info| !info.is_null())
            .map(|info| info.get("kind").and_then(Value::as_str) == Some("directory")))
    }

    fn process_path(&self, target: &str) -> String {
        let payload = Self::target_of(target)
            .and_then(|target| {
                serde_json::to_vec(&serde_json::json!({ "target": target }))
                    .map_err(|error| format!("fs process-path encode failed: {error}"))
            })
            .and_then(|bytes| {
                self.outbound
                    .require(fs::SERVICE_NAME, fs::METHOD_PROCESS_PATH, &bytes)
                    .map_err(|(_, detail)| Outbound::message_of(&detail))
            })
            .and_then(|reply| {
                serde_json::from_slice::<Value>(&reply)
                    .map_err(|error| format!("fs process-path decode failed: {error}"))
            });
        // The canonical spelling is best-effort (the dsh processPath
        // runs only after a confirmed directory; a refused spell-out
        // keeps the token's display shape rather than failing the leg).
        payload
            .ok()
            .and_then(|parsed| {
                parsed
                    .get("path")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| target.to_owned())
    }
}

/// Declares the durable change vocabulary (a duplicate fails closed at
/// the session service; the Q34A ownership channel).
///
/// # Errors
///
/// The transport or persistence refusal.
pub fn declare_change_event(outbound: &Outbound) -> Result<(), String> {
    outbound
        .require(
            session_persistence::SERVICE_NAME,
            session_persistence::METHOD_DECLARE_EVENT_TYPES,
            &declared_event_types_bytes(),
        )
        .map(|_| ())
        .map_err(|(_, detail)| format!("declare: {}", Outbound::message_of(&detail)))
}

/// Registers the `workingDirectory` projection (the relay's third-party
/// read face; the fold endpoint is the plugin's own service selector).
///
/// # Errors
///
/// The transport or relay refusal.
pub fn register_projection(outbound: &Outbound) -> Result<(), String> {
    outbound
        .require(
            session_projections::SERVICE_NAME,
            session_projections::METHOD_REGISTER,
            &projection_registration_bytes(),
        )
        .map(|_| ())
        .map_err(|(_, detail)| format!("projection register: {}", Outbound::message_of(&detail)))
}

/// Registers the working-directory runtime context (order 100
/// `WORKING_DIRECTORY`, required, non-interpolated).
///
/// The dsh `ctx.systemPrompt.context` plus its assemble prepend
/// listener reduce to the single textProvider endpoint.
///
/// # Errors
///
/// The transport or registry refusal.
pub fn register_context(outbound: &Outbound) -> Result<(), String> {
    outbound
        .require(
            system_prompt::SERVICE_NAME,
            system_prompt::METHOD_REGISTER_CONTEXT,
            &context_registration_bytes(),
        )
        .map(|_| ())
        .map_err(|(_, detail)| format!("context register: {}", Outbound::message_of(&detail)))
}

/// The declared event types document.
#[must_use]
pub fn declared_event_types_bytes() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "types": [CHANGE_EVENT] })).unwrap_or_default()
}

/// The projection registration document (`{key, stateVersion, fold}`).
#[must_use]
pub fn projection_registration_bytes() -> Vec<u8> {
    serde_json::to_vec(&projection_registration_spec()).unwrap_or_default()
}

/// The context registration document (the dsh `working-directory:current`
/// shape over the fr textProvider carrier).
#[must_use]
pub fn context_registration_bytes() -> Vec<u8> {
    serde_json::to_vec(&context_registration_spec()).unwrap_or_default()
}

/// The projection registration payload (the pinned shape).
#[must_use]
pub fn projection_registration_spec() -> Value {
    serde_json::json!({
        "key": crate::core::PROJECTION_KEY,
        "stateVersion": crate::core::STATE_VERSION,
        "fold": {
            "service": SERVICE_NAME,
            "method": METHOD_FOLD,
        },
    })
}

/// The context registration payload (the pinned shape).
#[must_use]
pub fn context_registration_spec() -> Value {
    serde_json::json!({
        "name": "working-directory:current",
        "order": 100.0,
        "text": {
            "textProvider": {
                "service": SERVICE_NAME,
                "method": METHOD_CONTEXT_TEXT,
            },
        },
        "interpolate": false,
        "required": true,
    })
}
