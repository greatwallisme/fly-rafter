// Rust guideline compliant 2026-10-29
//! The dylib artifact shell: descriptor, activation wiring, and the
//! export table (the fr-shell judgment shape).
//!
//! The activation sequence declares the durable change event type,
//! grants the two consumers, reads the mounted shell's default mode,
//! registers the two prompt sections, the `workingDirectory`
//! projection, and the two tools — any refusal declines the activation
//! fail-closed (a half-wired tools fabric must not exist).

use std::ffi::c_void;
use std::sync::Arc;

use dougong_abi::ABI_VERSION;
use dougong_abi::HostExports;
use dougong_abi::PluginExports;
use dougong_abi::STATUS_CAPACITY;
use dougong_abi::STATUS_FAILED;
use dougong_abi::STATUS_OK;
use dougong_abi::write_out;
use serde_json::Value;
use serde_json::json;

use crate::service::BuiltinTools;
use crate::shell_env::ShellEnvRegistry;
use crate::tools::Seams;
use crate::working_directory::WorkingDirectories;

/// The plugin name the host registers the artifact under.
pub const PLUGIN_NAME: &str = "fr-tools-builtin";

/// The kernel-event ids the descriptor's listen map binds (the
/// fr-agent-loop `EV_CHUNK_READY` judgment: the map value rides the
/// `on_event` dispatch).
pub const EV_JOB_SETTLED: u32 = 1;
/// The claimed-event id (the wake-budget reset leg).
pub const EV_INBOX_CLAIMED: u32 = 2;

/// The kernel event names this artifact listens for (the fr-shell owner
/// crate keeps the same words; the string stays verbatim).
pub const EVENT_JOB_SETTLED: &str = "job/settled";
/// The claimed event (the fr-agent-loop owner crate's word).
pub const EVENT_INBOX_CLAIMED: &str = "agent/inbox/claimed";

/// The static descriptor: one provided service, thirteen injected
/// seams (the terminals band rides the baked-gated terminal family)
/// (the search tools' subprocess face rides the injection), and the two
/// completion-notice events — the spill listeners register at activation
/// instead (the descriptor `listen` map stays a dylib event face, not a
/// tools-pipeline registration).
const DESCRIPTOR: &str = "{\"name\":\"fr-tools-builtin\",\"provide\":[\"builtin-tools\",\"fs\",\"user-questions\",\"plan-mode\",\"session-query\",\"skills\"],\"inject\":[\"tools\",\"shell\",\"jobs\",\"sandbox-policy\",\"approval\",\"session-persistence\",\"system-prompt\",\"subprocess\",\"attachments\",\"llm\",\"session-projections\",\"storage-domain\",\"terminals\"],\"listen\":{\"job/settled\":1,\"agent/inbox/claimed\":2}}";

/// The maximum host reply the require loop grows its buffer toward (the
/// approval judgment).
const MAX_REPLY_CAPACITY: usize = 16 * 1024 * 1024;

/// One copied host vtable with the require+grant arms (the approval
/// `HostLink` judgment shape widened by the v4 grant entry).
#[derive(Clone, Copy)]
pub struct HostLink {
    host: HostExports,
}

impl std::fmt::Debug for HostLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostLink").finish_non_exhaustive()
    }
}

// SAFETY: the copied vtable's raw host handle is opaque to this plugin
// and the export-table contract serializes host calls per activation;
// the approval/session shell judgments carry the same declaration.
unsafe impl Send for HostLink {}
// SAFETY: see `Send` — every require goes through the same serialized
// host entry point.
unsafe impl Sync for HostLink {}

impl HostLink {
    /// Copies one vtable.
    #[must_use]
    pub(crate) const fn new(host: HostExports) -> Self {
        Self { host }
    }

    /// Performs one synchronous cross-plugin call (growable buffer
    /// protocol).
    pub(crate) fn require(
        &self,
        service: &str,
        method: u32,
        payload: &[u8],
    ) -> Result<Vec<u8>, String> {
        let mut cap = 4096_usize;
        loop {
            let mut buf = vec![0_u8; cap];
            let mut len = 0_usize;
            // SAFETY: buffers are caller-owned per the export-table
            // contract.
            let status = unsafe {
                (self.host.require)(
                    self.host.host_handle,
                    service.as_ptr(),
                    service.len(),
                    method,
                    payload.as_ptr(),
                    payload.len(),
                    buf.as_mut_ptr(),
                    buf.len(),
                    &raw mut len,
                )
            };
            if status == dougong_abi::STATUS_CAPACITY {
                if cap >= MAX_REPLY_CAPACITY {
                    return Err(format!("reply still exceeds {MAX_REPLY_CAPACITY} bytes"));
                }
                cap = cap.saturating_mul(2);
                continue;
            }
            buf.truncate(len);
            if status == STATUS_OK {
                return Ok(buf);
            }
            return Err(format!(
                "require {service}.{method} failed with host status {status}: {}",
                String::from_utf8_lossy(&buf)
            ));
        }
    }

    /// Publishes one kernel event with a JSON payload (the
    /// `schedule/changed` notifier; the fr-storage emit shape).
    pub(crate) fn emit(&self, event: &str, payload: &str) {
        // SAFETY: the vtable contract guarantees validity for this call;
        // byte-string arguments are read-only for the call's duration.
        let _status = unsafe {
            (self.host.emit)(
                self.host.host_handle,
                event.as_ptr(),
                event.len(),
                payload.as_ptr(),
                payload.len(),
            )
        };
    }

    /// Grants one committed target activation access to a provided
    /// service (the D31 registration primitive; no call, no buffer).
    pub(crate) fn grant(&self, target_service: &str, granted_service: &str) -> Result<(), String> {
        // SAFETY: the copied vtable is live until this activation's
        // `teardown` returns; both names are read-only byte strings for
        // the call.
        let status = unsafe {
            (self.host.grant)(
                self.host.host_handle,
                target_service.as_ptr(),
                target_service.len(),
                granted_service.as_ptr(),
                granted_service.len(),
            )
        };
        if status == STATUS_OK {
            Ok(())
        } else {
            Err(format!(
                "grant {granted_service} to {target_service} failed with host status {status}"
            ))
        }
    }
}

/// The wire implementation of the tool bodies' cross-plugin seam
/// (dsh `ctx.*` consumption over the host vtable).
struct WireSeams {
    link: HostLink,
    directories: Arc<WorkingDirectories>,
    env: Arc<ShellEnvRegistry>,
}

impl crate::tools::PolicySeam for WireSeams {
    fn resolve_policy(&self, session: Option<&str>) -> Result<Option<Value>, String> {
        let payload = session.map_or_else(|| json!({}), |session| json!({ "session": session }));
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("policy encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::sandbox_policy::SERVICE_NAME,
            fr_wire::services::sandbox_policy::METHOD_RESOLVE,
            &bytes,
        )?;
        let policy: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("policy decode failed: {error}"))?;
        Ok(Some(policy))
    }
}

impl crate::tools::AttachmentSeam for WireSeams {
    fn image_limits(&self) -> Result<Value, String> {
        let reply = self.link.require(
            fr_wire::services::attachments::SERVICE_NAME,
            fr_wire::services::attachments::METHOD_IMAGE_LIMITS,
            b"{}",
        )?;
        serde_json::from_slice(&reply)
            .map_err(|error| format!("image-limits decode failed: {error}"))
    }

    fn save_image(&self, input: &Value) -> Result<Value, String> {
        let bytes = serde_json::to_vec(input)
            .map_err(|error| format!("save-image encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::attachments::SERVICE_NAME,
            fr_wire::services::attachments::METHOD_SAVE_IMAGE,
            &bytes,
        )?;
        serde_json::from_slice(&reply).map_err(|error| format!("save-image decode failed: {error}"))
    }
}

impl crate::tools::LlmSeam for WireSeams {
    fn resolve_model_info(&self, provider: &str, model: &str) -> Result<Value, String> {
        let payload = json!({ "provider": provider, "model": model });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("resolve-model-info encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::llm::SERVICE_NAME,
            fr_wire::services::llm::METHOD_RESOLVE_MODEL_INFO,
            &bytes,
        )?;
        serde_json::from_slice(&reply)
            .map_err(|error| format!("resolve-model-info decode failed: {error}"))
    }
}

impl crate::tools::RouteHeaderSeam for WireSeams {
    fn request_header(&self, session: &str) -> Result<Value, String> {
        let payload = json!({ "id": session });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("request-header encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_REQUEST_HEADER,
            &bytes,
        )?;
        serde_json::from_slice(&reply)
            .map_err(|error| format!("request-header decode failed: {error}"))
    }
}

impl crate::tools::ApprovalSeam for WireSeams {
    fn request(&self, ask: &fr_sandbox::escalation::EscalationAsk) -> Result<String, String> {
        let payload = json!({
            "agent": ask.agent,
            "tool_name": ask.tool_name,
            "call_id": ask.call_id,
            "reason": ask.reason,
            "display_reason": { "en": ask.display_reason.en, "zh": ask.display_reason.zh },
        });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("approval encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::approval::SERVICE_NAME,
            fr_wire::services::approval::METHOD_REQUEST,
            &bytes,
        )?;
        let parsed: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("approval decode failed: {error}"))?;
        parsed
            .get("outcome")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "approval reply lacks an outcome".to_owned())
    }
}

impl Seams for WireSeams {
    fn policy(&self) -> &dyn crate::tools::PolicySeam {
        self
    }

    fn approval(&self) -> &dyn crate::tools::ApprovalSeam {
        self
    }

    fn attachments(&self) -> Option<&dyn crate::tools::AttachmentSeam> {
        Some(self)
    }

    fn llm(&self) -> Option<&dyn crate::tools::LlmSeam> {
        Some(self)
    }

    fn route_header(&self) -> Option<&dyn crate::tools::RouteHeaderSeam> {
        Some(self)
    }

    fn shell_resolve(&self, shell: &str, request: &Value) -> Result<Value, String> {
        let payload = json!({ "shell": shell, "request": request });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("resolve encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::shell::SERVICE_NAME,
            fr_wire::services::shell::METHOD_RESOLVE,
            &bytes,
        )?;
        serde_json::from_slice(&reply).map_err(|error| format!("resolve decode failed: {error}"))
    }

    fn shell_start(
        &self,
        shell: &str,
        spec: &Value,
        command: &str,
        owner: Option<&str>,
        call_id: &str,
        root_call_id: &str,
        escalation_modes: &[&str],
    ) -> Result<(String, String), String> {
        let payload = json!({
            "shell": shell,
            "spec": spec,
            "args": { "command": command },
            "owner": owner,
            "tool_call": { "call_id": call_id, "root_call_id": root_call_id },
            "escalation_modes": escalation_modes,
        });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("start encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::shell::SERVICE_NAME,
            fr_wire::services::shell::METHOD_START,
            &bytes,
        )?;
        let parsed: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("start decode failed: {error}"))?;
        let job_id = parsed
            .get("job_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "shell start reply lacks job_id".to_owned())?
            .to_owned();
        let workdir = parsed
            .get("workdir")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        Ok((job_id, workdir))
    }

    fn jobs_wait(&self, id: &str, timeout_ms: f64, caller: Option<&str>) -> Result<Value, String> {
        let payload = json!({ "id": id, "timeout_ms": timeout_ms, "caller": caller });
        let bytes = serde_json::to_vec(&payload).map_err(|e| format!("wait encode failed: {e}"))?;
        let reply = self.link.require(
            fr_wire::services::jobs::SERVICE_NAME,
            fr_wire::services::jobs::METHOD_WAIT,
            &bytes,
        )?;
        serde_json::from_slice(&reply).map_err(|e| format!("wait decode failed: {e}"))
    }

    fn jobs_read(&self, id: &str, caller: Option<&str>) -> Result<Value, String> {
        let payload = json!({ "id": id, "caller": caller });
        let bytes = serde_json::to_vec(&payload).map_err(|e| format!("read encode failed: {e}"))?;
        let reply = self.link.require(
            fr_wire::services::jobs::SERVICE_NAME,
            fr_wire::services::jobs::METHOD_READ,
            &bytes,
        )?;
        serde_json::from_slice(&reply).map_err(|e| format!("read decode failed: {e}"))
    }

    fn jobs_list(&self, caller: Option<&str>) -> Result<Value, String> {
        let payload = json!({ "caller": caller });
        let bytes = serde_json::to_vec(&payload).map_err(|e| format!("list encode failed: {e}"))?;
        let reply = self.link.require(
            fr_wire::services::jobs::SERVICE_NAME,
            fr_wire::services::jobs::METHOD_LIST,
            &bytes,
        )?;
        serde_json::from_slice(&reply).map_err(|e| format!("list decode failed: {e}"))
    }

    fn jobs_get(&self, id: &str, caller: Option<&str>) -> Result<Value, String> {
        let payload = json!({ "id": id, "caller": caller });
        let bytes = serde_json::to_vec(&payload).map_err(|e| format!("get encode failed: {e}"))?;
        let reply = self.link.require(
            fr_wire::services::jobs::SERVICE_NAME,
            fr_wire::services::jobs::METHOD_GET,
            &bytes,
        )?;
        serde_json::from_slice(&reply).map_err(|e| format!("get decode failed: {e}"))
    }

    fn jobs_kill(
        &self,
        id: &str,
        caller: Option<&str>,
        reason: Option<&str>,
    ) -> Result<String, String> {
        let payload = json!({ "id": id, "caller": caller, "reason": reason });
        let bytes = serde_json::to_vec(&payload).map_err(|e| format!("kill encode failed: {e}"))?;
        let reply = self.link.require(
            fr_wire::services::jobs::SERVICE_NAME,
            fr_wire::services::jobs::METHOD_KILL,
            &bytes,
        )?;
        let parsed: Value =
            serde_json::from_slice(&reply).map_err(|e| format!("kill decode failed: {e}"))?;
        parsed
            .get("result")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "jobs kill reply lacks result".to_owned())
    }

    fn jobs_remove(&self, id: &str, caller: Option<&str>) -> Result<(), String> {
        let payload = json!({ "id": id, "caller": caller });
        let bytes =
            serde_json::to_vec(&payload).map_err(|e| format!("remove encode failed: {e}"))?;
        self.link.require(
            fr_wire::services::jobs::SERVICE_NAME,
            fr_wire::services::jobs::METHOD_REMOVE,
            &bytes,
        )?;
        Ok(())
    }

    fn shell_result(&self, job_id: &str) -> Result<Value, String> {
        let payload = json!({ "job_id": job_id });
        let bytes =
            serde_json::to_vec(&payload).map_err(|e| format!("result encode failed: {e}"))?;
        let reply = self.link.require(
            fr_wire::services::shell::SERVICE_NAME,
            fr_wire::services::shell::METHOD_RESULT,
            &bytes,
        )?;
        serde_json::from_slice(&reply).map_err(|e| format!("result decode failed: {e}"))
    }

    fn working_directory_ensure(&self, agent: &str) -> Result<String, String> {
        self.directories.ensure(agent)
    }

    fn shell_env_collect(
        &self,
        agent: Option<&str>,
    ) -> Result<crate::shell_env::FrozenEnv, String> {
        self.env.collect(&crate::shell_env::Execution {
            agent: agent.map(str::to_owned),
        })
    }

    fn subprocess_spawn(&self, spec: &Value) -> Result<String, String> {
        let bytes =
            serde_json::to_vec(spec).map_err(|error| format!("spawn encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::subprocess::SERVICE_NAME,
            fr_wire::services::subprocess::METHOD_SPAWN,
            &bytes,
        )?;
        let parsed: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("spawn decode failed: {error}"))?;
        parsed
            .get("handle")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "subprocess spawn reply lacks handle".to_owned())
    }

    fn subprocess_read(&self, handle: &str, stream: &str, from: u64) -> Result<Value, String> {
        let payload = json!({ "handle": handle, "stream": stream, "from": from });
        let bytes =
            serde_json::to_vec(&payload).map_err(|error| format!("read encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::subprocess::SERVICE_NAME,
            fr_wire::services::subprocess::METHOD_READ,
            &bytes,
        )?;
        serde_json::from_slice(&reply).map_err(|error| format!("read decode failed: {error}"))
    }

    fn subprocess_wait(&self, handle: &str, timeout_ms: f64) -> Result<Value, String> {
        let payload = json!({ "handle": handle, "timeout_ms": timeout_ms });
        let bytes =
            serde_json::to_vec(&payload).map_err(|error| format!("wait encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::subprocess::SERVICE_NAME,
            fr_wire::services::subprocess::METHOD_WAIT,
            &bytes,
        )?;
        serde_json::from_slice(&reply).map_err(|error| format!("wait decode failed: {error}"))
    }

    fn subprocess_resolve_executable(&self, command: &str) -> Result<String, String> {
        let payload = json!({ "command": command });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("resolveExecutable encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::subprocess::SERVICE_NAME,
            fr_wire::services::subprocess::METHOD_RESOLVE_EXECUTABLE,
            &bytes,
        )?;
        let parsed: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("resolveExecutable decode failed: {error}"))?;
        parsed
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "subprocess resolve reply lacks path".to_owned())
    }

    fn spill_store_save_text(&self, save: &Value) -> Result<Option<Value>, String> {
        let bytes =
            serde_json::to_vec(save).map_err(|error| format!("saveText encode failed: {error}"))?;
        match self.link.require(
            fr_wire::services::spill_store::SERVICE_NAME,
            fr_wire::services::spill_store::METHOD_SAVE_TEXT,
            &bytes,
        ) {
            Ok(reply) => {
                let parsed: Value = serde_json::from_slice(&reply)
                    .map_err(|error| format!("saveText decode failed: {error}"))?;
                Ok(Some(parsed))
            }
            // The opportunistic consumption mirrors the dsh `ctx.get`:
            // a service no provider committed (undeclared or inactive)
            // is the absent backend, not a storage failure.
            Err(text) if text.contains("status -3") || text.contains("status -4") => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn terminals(&self) -> Option<&dyn crate::tools::TerminalSeams> {
        Some(self)
    }
}

impl crate::tools::TerminalSeams for WireSeams {
    fn spawn(&self, owner: &str, spec: &Value) -> Result<Value, String> {
        let mut payload = spec.clone();
        payload["owner"] = Value::String(owner.to_owned());
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("terminals spawn encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::terminals::SERVICE_NAME,
            fr_wire::services::terminals::METHOD_SPAWN,
            &bytes,
        )?;
        serde_json::from_slice(&reply).map_err(|error| format!("spawn decode failed: {error}"))
    }

    fn start_send(
        &self,
        owner: &str,
        session_id: &str,
        text: &str,
        submit: bool,
    ) -> Result<Value, String> {
        let payload = json!({
            "owner": owner,
            "sessionId": session_id,
            "text": text,
            "submit": submit,
        });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("start-send encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::terminals::SERVICE_NAME,
            fr_wire::services::terminals::METHOD_START_SEND,
            &bytes,
        )?;
        serde_json::from_slice(&reply).map_err(|error| format!("start-send decode failed: {error}"))
    }

    fn send_wait(&self, owner: &str, session_id: &str, timeout_ms: u64) -> Result<Value, String> {
        let payload = json!({
            "owner": owner,
            "sessionId": session_id,
            "timeout_ms": timeout_ms,
        });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("send-wait encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::terminals::SERVICE_NAME,
            fr_wire::services::terminals::METHOD_SEND_WAIT,
            &bytes,
        )?;
        serde_json::from_slice(&reply).map_err(|error| format!("send-wait decode failed: {error}"))
    }

    fn send_cancel(&self, owner: &str, session_id: &str) -> Result<Value, String> {
        let payload = json!({
            "owner": owner,
            "sessionId": session_id,
        });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("send-cancel encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::terminals::SERVICE_NAME,
            fr_wire::services::terminals::METHOD_SEND_CANCEL,
            &bytes,
        )?;
        serde_json::from_slice(&reply)
            .map_err(|error| format!("send-cancel decode failed: {error}"))
    }

    fn send_read(&self, owner: &str, session_id: &str) -> Result<Value, String> {
        let payload = json!({ "owner": owner, "sessionId": session_id });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("send-read encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::terminals::SERVICE_NAME,
            fr_wire::services::terminals::METHOD_SEND_READ,
            &bytes,
        )?;
        serde_json::from_slice(&reply).map_err(|error| format!("send-read decode failed: {error}"))
    }

    fn read(
        &self,
        owner: &str,
        session_id: &str,
        offset: Option<u64>,
        count: Option<u64>,
    ) -> Result<Value, String> {
        let mut payload = json!({ "owner": owner, "sessionId": session_id });
        if let Some(offset) = offset {
            payload["offset"] = json!(offset);
        }
        if let Some(count) = count {
            payload["count"] = json!(count);
        }
        let bytes =
            serde_json::to_vec(&payload).map_err(|error| format!("read encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::terminals::SERVICE_NAME,
            fr_wire::services::terminals::METHOD_READ,
            &bytes,
        )?;
        serde_json::from_slice(&reply).map_err(|error| format!("read decode failed: {error}"))
    }

    fn signal(&self, owner: &str, session_id: &str, signal: &str) -> Result<Value, String> {
        let payload = json!({
            "owner": owner,
            "sessionId": session_id,
            "signal": signal,
        });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("signal encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::terminals::SERVICE_NAME,
            fr_wire::services::terminals::METHOD_SIGNAL,
            &bytes,
        )?;
        serde_json::from_slice(&reply).map_err(|error| format!("signal decode failed: {error}"))
    }

    fn kill(&self, owner: &str, session_id: &str, reason: &str) -> Result<Value, String> {
        let payload = json!({
            "owner": owner,
            "sessionId": session_id,
            "reason": reason,
        });
        let bytes =
            serde_json::to_vec(&payload).map_err(|error| format!("kill encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::terminals::SERVICE_NAME,
            fr_wire::services::terminals::METHOD_KILL,
            &bytes,
        )?;
        serde_json::from_slice(&reply).map_err(|error| format!("kill decode failed: {error}"))
    }

    fn list(&self, owner: &str) -> Result<Value, String> {
        let bytes = serde_json::to_vec(&json!({ "owner": owner }))
            .map_err(|error| format!("list encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::terminals::SERVICE_NAME,
            fr_wire::services::terminals::METHOD_LIST,
            &bytes,
        )?;
        serde_json::from_slice(&reply).map_err(|error| format!("list decode failed: {error}"))
    }

    fn send_background(&self, payload: &Value) -> Result<Value, String> {
        let bytes = serde_json::to_vec(payload)
            .map_err(|error| format!("send-background encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::terminals::SERVICE_NAME,
            fr_wire::services::terminals::METHOD_SEND_BACKGROUND,
            &bytes,
        )?;
        serde_json::from_slice(&reply)
            .map_err(|error| format!("send-background decode failed: {error}"))
    }
}

/// One activation-sequence step: one require, `None` on any refusal
/// (the fail-closed decline).
fn step(link: &HostLink, service: &str, method: u32, payload: &[u8]) -> Option<()> {
    let payload = normalize_register_payload(service, method, payload)?;
    link.require(service, method, &payload).ok().map(|_| ())
}

/// Normalizes one registration payload onto the standard JSON Schema
/// dialect before it crosses the wire (the 2026-11-04 first-co-load
/// ruling: the crate's authoring dialect carries `required: true` inside
/// property definitions — compact for pinned tables — while the tools
/// registry validates the standard object-level `required` array form
/// the dsh catalog emits; the boundary conversion is the single point
/// where the two dialects meet).
fn normalize_register_payload(service: &str, method: u32, payload: &[u8]) -> Option<Vec<u8>> {
    if service != fr_wire::services::tools::SERVICE_NAME
        || method != fr_wire::services::tools::METHOD_REGISTER
    {
        return Some(payload.to_vec());
    }
    let mut value: serde_json::Value = serde_json::from_slice(payload).ok()?;
    crate::schema::standardize_required(&mut value);
    serde_json::to_vec(&value).ok()
}

/// The escalation modes the registration advertises, resolved from the
/// mounted shell's default mode (dsh: `undefined` mode advertises
/// nothing).
fn escalation_modes_from(mode: Option<&str>) -> Vec<&'static str> {
    match mode {
        Some(_) => crate::service::ESCALATION_MODES.to_vec(),
        None => Vec::new(),
    }
}

/// The tools fabric state: the composed runtime over one copied
/// vtable.
pub struct BuiltinState {
    tools: BuiltinTools,
    fs: Arc<crate::fs_domain::FsDomain>,
    gate: crate::observation::ObservedStateGate,
    /// The activation's outbound seam copy the `'skills'` serve face
    /// requires its session reads through (the session-query engine
    /// holds its own copy; `Copy`).
    link: HostLink,
    /// The spill listeners' CPS pairing stash (the enter leg parks the
    /// call and result; the exit leg consumes them).
    pending: std::sync::Mutex<std::collections::HashMap<u64, (Value, Value)>>,
    /// The completion-notice machine's queue sender; dropping it ends
    /// the worker at teardown.
    tasks: Option<std::sync::mpsc::Sender<crate::job_tools::NoticeTask>>,
    /// The notice worker's join handle (joined before the state drops).
    worker: Option<std::thread::JoinHandle<()>>,
    /// The schedule runtime's join handle (joined before the state
    /// drops; the paired signal shut it down first).
    schedule_worker: Option<std::thread::JoinHandle<()>>,
    /// The schedule runtime's wake signal (shutdown arm for the teardown
    /// quiescence).
    schedule_signal: Arc<crate::schedule::RuntimeSignal>,
}

impl std::fmt::Debug for BuiltinState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuiltinState").finish_non_exhaustive()
    }
}

/// Declares the durable change vocabulary (a duplicate fails closed at
/// the session service). The artifact owns `working-directory/change` and
/// `todo/write` (the goal/schedule change events join with their
/// batches).
fn declare_change_event(link: &HostLink) -> Option<()> {
    step(
        link,
        fr_wire::services::session_persistence::SERVICE_NAME,
        fr_wire::services::session_persistence::METHOD_DECLARE_EVENT_TYPES,
        serde_json::to_vec(&json!({
            "types": [
                crate::working_directory::CHANGE_EVENT,
                crate::todo::CHANGE_EVENT,
                crate::goal::CHANGE_EVENT,
                crate::plan_mode::CHANGE_EVENT,
            ]
        }))
        .ok()?
        .as_slice(),
    )
}

/// Reads the mounted shell's default mode (the escalation
/// advertisement and the pwsh description appendix gate).
fn read_default_mode(link: &HostLink) -> Option<Vec<&'static str>> {
    let mode_reply = link
        .require(
            fr_wire::services::shell::SERVICE_NAME,
            fr_wire::services::shell::METHOD_SANDBOX_MODE,
            b"{\"shell\": \"bash\"}",
        )
        .ok()?;
    let mode: Value = serde_json::from_slice(&mode_reply).ok()?;
    let mode_word = mode.get("mode").and_then(Value::as_str);
    Some(escalation_modes_from(mode_word))
}

/// The environment lookup for the provider option resolution (the dsh
/// `process.env` read; the credential layer resolves once at activation
/// because the fr provider options are construction-time data — the
/// per-operation env re-read is the one seam-route deviation, recorded
/// in the module notes).
fn web_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// The baked web runtime over the dsh base composition's provider set:
/// the four base rows' providers register over the baked group
/// defaults, and the exa/perplexity implementations register too (the
/// `available` gate absorbs the missing credentials; the baked
/// selection pins deepseek-official/http, so the ambiguity arm stays
/// unreachable).
fn build_web_runtime() -> Option<crate::web_seam::WebRuntime> {
    let deepseek_group = json!({ "api-key-env": "DEEPSEEK_API_KEY" });
    let env = |name: &str| web_env(name);
    let mut runtime = crate::web_seam::WebRuntime::new();
    let official: std::sync::Arc<dyn crate::web_seam::WebSearchProvider> =
        std::sync::Arc::new(crate::web_search_deepseek::DeepSeekSearchProvider::new(
            crate::web_search_deepseek::resolve_options(&deepseek_group, &env),
        ));
    runtime.register_search_provider(official).ok()?;
    let api_search: std::sync::Arc<dyn crate::web_seam::WebSearchProvider> =
        std::sync::Arc::new(crate::web_deepseek_api::DeepSeekSearchApiProvider::new(
            crate::web_deepseek_api::resolve_search_options(&deepseek_group, &env),
        ));
    runtime.register_search_provider(api_search).ok()?;
    let api_fetch: std::sync::Arc<dyn crate::web_seam::WebFetchProvider> =
        std::sync::Arc::new(crate::web_deepseek_api::DeepSeekFetchApiProvider::new(
            crate::web_deepseek_api::resolve_fetch_options(&deepseek_group, &env),
        ));
    runtime.register_fetch_provider(api_fetch).ok()?;
    let http = crate::web_fetch_http::HttpFetchProvider::new(
        crate::web_fetch_http::resolve_limits(&json!({})).ok()?,
    );
    runtime
        .register_fetch_provider(std::sync::Arc::new(http))
        .ok()?;
    let exa: std::sync::Arc<dyn crate::web_seam::WebSearchProvider> =
        std::sync::Arc::new(crate::web_search_exa::ExaSearchProvider::new(
            crate::web_search_exa::resolve_options(&json!({}), &env),
        ));
    runtime.register_search_provider(exa).ok()?;
    let perplexity: std::sync::Arc<dyn crate::web_seam::WebSearchProvider> =
        std::sync::Arc::new(crate::web_search_perplexity::PerplexitySearchProvider::new(
            crate::web_search_perplexity::resolve_options(&json!({}), &env),
        ));
    runtime.register_search_provider(perplexity).ok()?;
    Some(runtime)
}

/// The eight prompt sections (dsh `ctx.systemPrompt.section`): the names,
/// the dsh orders, and the script-extracted verbatim texts (the catalog
/// generator; the PRD 22 rule — the texts live in the dsh tool sources,
/// not the catalog). The write text composes with the source's
/// conditional (the edit tail rides while edit registers, which the M1
/// composition always does); the grep text composes the same way (the
/// read tail); the goal text composes over the extracted threshold
/// template. The `str_replace_editor` registers no section (the dsh
/// source injects no systemPrompt; the Q16b-4A ruling).
pub(crate) fn prompt_sections() -> Vec<(String, f64, String)> {
    let web = match (
        crate::web_tools::BAKED_WEB_SEARCH_ENABLED,
        crate::web_tools::BAKED_WEB_FETCH_ENABLED,
    ) {
        (true, true) => WebSection::Both,
        (true, false) => WebSection::SearchOnly,
        _ => WebSection::None,
    };
    prompt_sections_with(SectionGates {
        terminals: crate::terminal_tools::terminal_tools_registered(),
        web,
        lsp: crate::lsp_tools::BAKED_LSP,
    })
}

/// The web pair's section mounts (the dsh `tool:web_search` row rides
/// both arms with a variant text; the `tool:web_fetch` row rides the
/// both arm only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WebSection {
    /// Neither web row.
    None,
    /// The search row only (the fallback search text).
    SearchOnly,
    /// Both rows (the fetch-variant search text plus the fetch row).
    Both,
}

/// The baked section gates (the parameterized table's mounts: the
/// pty/web/lsp rows gate while the base ten always ride).
#[derive(Clone, Copy, Debug)]
pub(crate) struct SectionGates {
    /// The terminal-family mount (the pty row).
    pub terminals: bool,
    /// The web pair's mount.
    pub web: WebSection,
    /// The lsp mount.
    pub lsp: bool,
}

/// The parameterized section table (the gates drive the `tool:pty`
/// and `tool:lsp` rows: the dsh section calls live inside the
/// tool-terminal and tool-lsp applies, so the unmounted compositions
/// register nothing).
pub(crate) fn prompt_sections_with(gates: SectionGates) -> Vec<(String, f64, String)> {
    let SectionGates {
        terminals,
        web,
        lsp,
    } = gates;
    let web_search = web != WebSection::None;
    let web_fetch = web == WebSection::Both;
    let mut sections = vec![
        (
            "tool:bash".to_owned(),
            1000.0,
            crate::catalog_gen::BASH_SECTION_TEXT.to_owned(),
        ),
        (
            "tool:pwsh".to_owned(),
            1010.0,
            crate::catalog_gen::PWSH_SECTION_TEXT.to_owned(),
        ),
        (
            "tool:read".to_owned(),
            1100.0,
            crate::catalog_gen::READ_SECTION_TEXT.to_owned(),
        ),
        (
            "tool:write".to_owned(),
            1200.0,
            crate::fs_tools::write_section_text(true),
        ),
        (
            "tool:edit".to_owned(),
            1300.0,
            crate::catalog_gen::EDIT_SECTION_TEXT.to_owned(),
        ),
        (
            "tool:glob".to_owned(),
            1400.0,
            crate::catalog_gen::GLOB_SECTION_TEXT.to_owned(),
        ),
        (
            "tool:grep".to_owned(),
            1500.0,
            crate::search_tools::grep_section_text(true),
        ),
        (
            "tool:goal".to_owned(),
            2400.0,
            crate::schema::goal_section_text(crate::goal_tools::BAKED_BLOCKED_AFTER),
        ),
        (
            "tool:jobs".to_owned(),
            1600.0,
            crate::catalog_gen::JOBS_SECTION_TEXT.to_owned(),
        ),
        (
            "tool:session-query".to_owned(),
            crate::session_query_tools::SECTION_ORDER,
            crate::catalog_gen::SESSION_QUERY_SECTION_TEXT.to_owned(),
        ),
    ];
    if web_search {
        sections.push((
            "tool:web_search".to_owned(),
            2000.0,
            crate::web_tools::web_search_section_text(web_fetch),
        ));
    }
    if web_fetch {
        sections.push((
            "tool:web_fetch".to_owned(),
            2100.0,
            crate::catalog_gen::WEB_FETCH_SECTION_TEXT.to_owned(),
        ));
    }
    if terminals {
        sections.push((
            "tool:pty".to_owned(),
            1700.0,
            crate::catalog_gen::PTY_SECTION_TEXT.to_owned(),
        ));
    }
    if lsp {
        sections.push((
            "tool:lsp".to_owned(),
            2200.0,
            crate::catalog_gen::LSP_SECTION_TEXT.to_owned(),
        ));
    }
    sections
}

/// Registers the dynamic `plan:policy` section (the dsh
/// `ctx.systemPrompt.section` text fn over the wire textProvider
/// endpoint; the deployment guidance renders only while plan mode is
/// active or pending for the assembling session).
fn register_plan_policy_section(link: &HostLink) -> Option<()> {
    step(
        link,
        fr_wire::services::system_prompt::SERVICE_NAME,
        fr_wire::services::system_prompt::METHOD_REGISTER_SECTION,
        serde_json::to_vec(&json!({
            "name": crate::plan_mode::POLICY_SECTION,
            "order": 500.0,
            "text": {
                "textProvider": {
                    "service": crate::service::SERVICE_NAME,
                    "method": crate::service::METHOD_PLAN_POLICY_SECTION,
                },
            },
        }))
        .ok()?
        .as_slice(),
    )
}

/// Registers the working-directory runtime context (dsh
/// `ctx.systemPrompt.context` with the assemble prepend listener's
/// ensure-and-text resolve; order 100 `WORKING_DIRECTORY`, required,
/// non-interpolated).
fn register_working_directory_context_spec() -> Value {
    json!({
        "name": "working-directory:current",
        "order": 100.0,
        "text": {
            "textProvider": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_WORKING_DIRECTORY_CONTEXT_TEXT,
            },
        },
        "interpolate": false,
        "required": true,
    })
}

fn register_working_directory_context(link: &HostLink) -> Option<()> {
    step(
        link,
        fr_wire::services::system_prompt::SERVICE_NAME,
        fr_wire::services::system_prompt::METHOD_REGISTER_CONTEXT,
        serde_json::to_vec(&register_working_directory_context_spec())
            .ok()?
            .as_slice(),
    )
}

/// Registers the two prompt sections (dsh `ctx.systemPrompt.section`).
fn register_sections(link: &HostLink) -> Option<()> {
    for (name, order, text) in prompt_sections() {
        step(
            link,
            fr_wire::services::system_prompt::SERVICE_NAME,
            fr_wire::services::system_prompt::METHOD_REGISTER_SECTION,
            serde_json::to_vec(&json!({
                "name": name,
                "order": order,
                "text": text,
            }))
            .ok()?
            .as_slice(),
        )?;
    }
    Some(())
}

/// Registers the `workingDirectory` projection (the relay's third-party
/// read face; the fold endpoint is the 900 selector), the `todos`
/// projection (stateVersion 2; the fold endpoint is the 901 selector),
/// and the `goal` projection (stateVersion 6; the fold endpoint is the
/// 902 selector).
fn register_projection(link: &HostLink) -> Option<()> {
    for (key, version, fold_method) in [
        (
            crate::working_directory::PROJECTION_KEY,
            crate::working_directory::STATE_VERSION,
            crate::service::METHOD_WORKING_DIRECTORY_FOLD,
        ),
        (
            crate::todo::PROJECTION_KEY,
            crate::todo::STATE_VERSION,
            crate::service::METHOD_TODOS_FOLD,
        ),
        (
            crate::goal::PROJECTION_KEY,
            crate::goal::STATE_VERSION,
            crate::service::METHOD_GOAL_FOLD,
        ),
        // The timed-question projection (the dsh `userQuestions` key,
        // stateVersion 2; the fold endpoint is the 903 selector).
        (
            "userQuestions",
            2,
            crate::service::METHOD_USER_QUESTIONS_FOLD,
        ),
        // The plan projection (stateVersion 3; the fold endpoint is
        // the 904 selector).
        (
            crate::plan_mode::PROJECTION_KEY,
            crate::plan_mode::STATE_VERSION,
            crate::service::METHOD_PLAN_FOLD,
        ),
    ] {
        step(
            link,
            fr_wire::services::session_projections::SERVICE_NAME,
            fr_wire::services::session_projections::METHOD_REGISTER,
            serde_json::to_vec(&json!({
                "key": key,
                "stateVersion": version,
                "fold": {
                    "service": crate::service::SERVICE_NAME,
                    "method": fold_method,
                },
            }))
            .ok()?
            .as_slice(),
        )?;
    }
    Some(())
}

/// One registration-table row: the name, the composed facts, the five
/// endpoint selectors, the sibling-overlap flag, and the timeout.
type RegistrationRow = (
    &'static str,
    String,
    Value,
    Value,
    bool,
    u32,
    u32,
    u32,
    u32,
    u32,
    Option<f64>,
);

/// Registers the tools (the fr-tools register face; the endpoints point
/// back at this artifact's own service). The read tool opts into sibling
/// overlap (`isConcurrencySafe`); the exclusives stay absent. The
/// glob/grep declarations carry the cooperative timeout budget; the
/// opt-in editor registers only when the composition bakes the knob on
/// (the M1 bundle leaves it off).
#[expect(
    clippy::too_many_lines,
    reason = "the tuple table mirrors the dsh registrations one arm per tool; splitting it would hide the correspondence"
)]
fn register_tools(
    link: &HostLink,
    advertise_escalation: bool,
    attachments_mounted: bool,
) -> Option<()> {
    let tools: Vec<RegistrationRow> = vec![
        (
            "bash",
            crate::schema::bash_description(),
            crate::schema::bash_parameters(true, advertise_escalation, true),
            crate::schema::shell_output_schema(),
            false,
            crate::service::METHOD_BASH_EXECUTE,
            crate::service::METHOD_BASH_RENDER,
            crate::service::METHOD_BASH_PRESENT_CALL,
            crate::service::METHOD_BASH_PRESENT_RESULT,
            crate::service::METHOD_PRESENTATION_META,
            None,
        ),
        (
            "pwsh",
            crate::schema::pwsh_description(advertise_escalation),
            crate::schema::pwsh_parameters(true, advertise_escalation, true),
            crate::schema::shell_output_schema(),
            false,
            crate::service::METHOD_PWSH_EXECUTE,
            crate::service::METHOD_PWSH_RENDER,
            crate::service::METHOD_PWSH_PRESENT_CALL,
            crate::service::METHOD_PWSH_PRESENT_RESULT,
            crate::service::METHOD_PRESENTATION_META,
            None,
        ),
        (
            "read",
            crate::catalog_gen::READ_DESCRIPTION.to_owned(),
            crate::schema::read_parameters(),
            crate::schema::read_output_schema(),
            true,
            crate::service::METHOD_READ_EXECUTE,
            crate::service::METHOD_READ_RENDER,
            crate::service::METHOD_READ_PRESENT_CALL,
            crate::service::METHOD_READ_PRESENT_RESULT,
            crate::service::METHOD_READ_PRESENTATION_META,
            None,
        ),
        (
            "write",
            crate::catalog_gen::WRITE_DESCRIPTION.to_owned(),
            crate::schema::write_parameters(advertise_escalation),
            crate::schema::write_output_schema(),
            false,
            crate::service::METHOD_WRITE_EXECUTE,
            crate::service::METHOD_WRITE_RENDER,
            crate::service::METHOD_WRITE_PRESENT_CALL,
            crate::service::METHOD_WRITE_PRESENT_RESULT,
            crate::service::METHOD_WRITE_PRESENTATION_META,
            None,
        ),
        (
            "edit",
            crate::catalog_gen::EDIT_DESCRIPTION.to_owned(),
            crate::schema::edit_parameters(advertise_escalation),
            crate::schema::edit_output_schema(),
            false,
            crate::service::METHOD_EDIT_EXECUTE,
            crate::service::METHOD_EDIT_RENDER,
            crate::service::METHOD_EDIT_PRESENT_CALL,
            crate::service::METHOD_EDIT_PRESENT_RESULT,
            crate::service::METHOD_EDIT_PRESENTATION_META,
            None,
        ),
        (
            "glob",
            crate::schema::glob_description(
                crate::search_core::baked_caps().sample_over_cap_glob_results,
            ),
            crate::schema::glob_parameters(),
            crate::schema::glob_output_schema(),
            false,
            crate::service::METHOD_GLOB_EXECUTE,
            crate::service::METHOD_GLOB_RENDER,
            crate::service::METHOD_GLOB_PRESENT_CALL,
            crate::service::METHOD_GLOB_PRESENT_RESULT,
            crate::service::METHOD_GLOB_PRESENTATION_META,
            Some(crate::search_core::baked_caps().timeout_ms),
        ),
        (
            "grep",
            crate::schema::grep_description(),
            crate::schema::grep_parameters(),
            crate::schema::grep_output_schema(),
            false,
            crate::service::METHOD_GREP_EXECUTE,
            crate::service::METHOD_GREP_RENDER,
            crate::service::METHOD_GREP_PRESENT_CALL,
            crate::service::METHOD_GREP_PRESENT_RESULT,
            crate::service::METHOD_GREP_PRESENTATION_META,
            Some(crate::search_core::baked_caps().timeout_ms),
        ),
    ];
    for (
        name,
        description,
        parameters,
        output_schema,
        concurrency_safe,
        execute,
        render_m,
        present_call,
        present_result,
        presentation_meta,
        timeout_ms,
    ) in tools
    {
        let mut registration = json!({
            "name": name,
            "description": description,
            "parameters": parameters,
            "output": {
                "schema": output_schema,
                "render": {
                    "service": crate::service::SERVICE_NAME,
                    "method": render_m,
                },
                "presentationMeta": {
                    "service": crate::service::SERVICE_NAME,
                    "method": presentation_meta,
                },
            },
            "execute": { "service": crate::service::SERVICE_NAME, "method": execute },
            "presentCall": { "service": crate::service::SERVICE_NAME, "method": present_call },
            "presentResult": { "service": crate::service::SERVICE_NAME, "method": present_result },
        });
        if concurrency_safe {
            registration["isConcurrencySafe"] = serde_json::Value::Bool(true);
        }
        if let Some(timeout_ms) = timeout_ms {
            registration["timeoutMs"] = json!(timeout_ms);
        }
        step(
            link,
            fr_wire::services::tools::SERVICE_NAME,
            fr_wire::services::tools::METHOD_REGISTER,
            serde_json::to_vec(&registration).ok()?.as_slice(),
        )?;
    }
    // The composition-conditional image tool rides the read family:
    // content-addressed attachment writes are idempotent, so concurrent
    // reads of the same file cannot conflict (the dsh
    // `isConcurrencySafe` judgment).
    if attachments_mounted {
        let registration = json!({
            "name": "read_image",
            "description": crate::catalog_gen::READ_IMAGE_DESCRIPTION.to_owned(),
            "parameters": crate::schema::read_image_parameters(),
            "output": {
                "schema": crate::schema::read_image_output_schema(),
                "render": {
                    "service": crate::service::SERVICE_NAME,
                    "method": crate::service::METHOD_READ_IMAGE_RENDER,
                },
                "presentationMeta": {
                    "service": crate::service::SERVICE_NAME,
                    "method": crate::service::METHOD_READ_IMAGE_PRESENTATION_META,
                },
            },
            "execute": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_READ_IMAGE_EXECUTE,
            },
            "presentCall": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_READ_IMAGE_PRESENT_CALL,
            },
            "isConcurrencySafe": true,
        });
        step(
            link,
            fr_wire::services::tools::SERVICE_NAME,
            fr_wire::services::tools::METHOD_REGISTER,
            serde_json::to_vec(&registration).ok()?.as_slice(),
        )?;
    }
    if crate::sre_tools::str_replace_editor_registered() {
        let registration = json!({
            "name": "str_replace_editor",
            "description": crate::schema::str_replace_editor_description(),
            "parameters": crate::schema::str_replace_editor_parameters(),
            "output": {
                "schema": crate::schema::str_replace_editor_output_schema(),
                "render": {
                    "service": crate::service::SERVICE_NAME,
                    "method": crate::service::METHOD_STR_REPLACE_EDITOR_RENDER,
                },
            },
            "execute": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_STR_REPLACE_EDITOR_EXECUTE,
            },
            "presentCall": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_STR_REPLACE_EDITOR_PRESENT_CALL,
            },
        });
        step(
            link,
            fr_wire::services::tools::SERVICE_NAME,
            fr_wire::services::tools::METHOD_REGISTER,
            serde_json::to_vec(&registration).ok()?.as_slice(),
        )?;
    }
    // The whole-list snapshot tool (dsh `tool-todo`; no opt-in gate, no
    // presentResult/presentationMeta endpoints — the partial-face
    // precedent). The baked `allow-parallel-in-progress` judgment mirrors
    // the headless bundle row (`true`).
    step(
        link,
        fr_wire::services::tools::SERVICE_NAME,
        fr_wire::services::tools::METHOD_REGISTER,
        serde_json::to_vec(&todo_write_registration())
            .ok()?
            .as_slice(),
    )?;
    // The three goal controls (dsh `tool-goal`; the same partial-face
    // shape: exclusive scheduling, no presentResult/presentationMeta,
    // the shared oneOf output contract).
    for registration in [
        get_goal_registration(),
        create_goal_registration(),
        update_goal_registration(),
    ] {
        step(
            link,
            fr_wire::services::tools::SERVICE_NAME,
            fr_wire::services::tools::METHOD_REGISTER,
            serde_json::to_vec(&registration).ok()?.as_slice(),
        )?;
    }
    // The three job controls (dsh `tool-jobs`; the same partial-face
    // shape: exclusive scheduling, no presentResult/presentationMeta;
    // the finalize listeners register separately below — the dsh
    // finalizeContent hook rides the post-execute CPS face).
    for registration in [
        job_output_registration(),
        job_list_registration(),
        job_kill_registration(),
    ] {
        step(
            link,
            fr_wire::services::tools::SERVICE_NAME,
            fr_wire::services::tools::METHOD_REGISTER,
            serde_json::to_vec(&registration).ok()?.as_slice(),
        )?;
    }
    // The four reminder controls (dsh `tool-schedule`; the same
    // partial-face shape: exclusive scheduling, no
    // presentResult/presentationMeta, the shared oneOf view/error
    // contract).
    for registration in [
        schedule_create_registration(),
        schedule_list_registration(),
        schedule_delete_registration(),
        schedule_update_registration(),
    ] {
        step(
            link,
            fr_wire::services::tools::SERVICE_NAME,
            fr_wire::services::tools::METHOD_REGISTER,
            serde_json::to_vec(&registration).ok()?.as_slice(),
        )?;
    }
    // The two interaction tools (dsh `tool-ask-user` legacy default +
    // the plan-mode exit tool; the Q16d-1A unconditional registration).
    // The five session-query tools ride the same unconditional
    // registration (Q16d-1A; the two search bodies carry the resolved
    // deadline, the three read bodies the concurrency-safe fact).
    for registration in [
        ask_user_question_registration(),
        exit_plan_mode_registration(),
        session_search_registration(),
        session_event_search_registration(),
        session_trace_registration(),
        session_event_trace_registration(),
        session_event_read_registration(),
    ] {
        step(
            link,
            fr_wire::services::tools::SERVICE_NAME,
            fr_wire::services::tools::METHOD_REGISTER,
            serde_json::to_vec(&registration).ok()?.as_slice(),
        )?;
    }
    // The two web tools (dsh `tool-web`; the baked gates mirror the
    // base patch row's `fetch: true` and the `search` default. An
    // enabled tool stays visible while its provider is unavailable —
    // the execution-time structured error is the dsh judgment).
    if crate::web_tools::BAKED_WEB_SEARCH_ENABLED {
        step(
            link,
            fr_wire::services::tools::SERVICE_NAME,
            fr_wire::services::tools::METHOD_REGISTER,
            serde_json::to_vec(&web_search_registration())
                .ok()?
                .as_slice(),
        )?;
    }
    if crate::web_tools::BAKED_WEB_FETCH_ENABLED {
        step(
            link,
            fr_wire::services::tools::SERVICE_NAME,
            fr_wire::services::tools::METHOD_REGISTER,
            serde_json::to_vec(&web_fetch_registration())
                .ok()?
                .as_slice(),
        )?;
    }
    // The working-directory tool (dsh `tool-working-directory`; the
    // dsh base bundle mounts it, so the registration is unconditional
    // — the Q16f-F baked mirror judgment).
    step(
        link,
        fr_wire::services::tools::SERVICE_NAME,
        fr_wire::services::tools::METHOD_REGISTER,
        serde_json::to_vec(&working_directory_registration())
            .ok()?
            .as_slice(),
    )?;
    // The skill loader (dsh `tool-skill`; the dsh base bundle mounts
    // the row unconditionally — the `BAKED_SKILL_TOOLS` toggle carries
    // the same judgment, and the toolVisible gate flips right after
    // the row lands: dsh `ctx.tools.get('skill', agent) === skillTool`).
    if crate::skill_tools::BAKED_SKILL_TOOLS {
        step(
            link,
            fr_wire::services::tools::SERVICE_NAME,
            fr_wire::services::tools::METHOD_REGISTER,
            serde_json::to_vec(&skill_registration()).ok()?.as_slice(),
        )?;
    }
    // The cordis-inspect pair (dsh `tool-cordis`; the dsh base bundle
    // mounts neither tool — web-app/desktop compositions only — so the
    // baked gate stays off; the Q16f-B ruling).
    if crate::inspect_tools::BAKED_CORDIS_INSPECT {
        for registration in [
            cordis_inspect_list_registration(),
            cordis_inspect_query_registration(),
        ] {
            step(
                link,
                fr_wire::services::tools::SERVICE_NAME,
                fr_wire::services::tools::METHOD_REGISTER,
                serde_json::to_vec(&registration).ok()?.as_slice(),
            )?;
        }
    }
    // The lsp tool (dsh `tool-lsp`; the dsh base bundle mounts no row
    // — web-app/desktop compositions only — so the baked gate stays
    // off; the Q16f-F mirror).
    if crate::lsp_tools::BAKED_LSP {
        step(
            link,
            fr_wire::services::tools::SERVICE_NAME,
            fr_wire::services::tools::METHOD_REGISTER,
            serde_json::to_vec(&lsp_registration()).ok()?.as_slice(),
        )?;
    }
    // The workspace-dependencies tool (dsh `tool-workspace-
    // dependencies`; the Q16f-E ruling — the baked composition mounts
    // no row and ships no payload; the tool mounts only over a
    // deployment config with a required payload source).
    if crate::wsd_tools::BAKED_WORKSPACE_DEPENDENCIES {
        step(
            link,
            fr_wire::services::tools::SERVICE_NAME,
            fr_wire::services::tools::METHOD_REGISTER,
            serde_json::to_vec(&load_workspace_dependencies_registration())
                .ok()?
                .as_slice(),
        )?;
    }
    // The plugin-management tool (dsh `tool-plugin-manager`; the dsh
    // base bundle mounts the row `disabled: true` — the Q16f-A opt-in
    // gate — so the baked gate stays off and the profile patch face
    // stays opt-in).
    if crate::plugin_manager_tools::BAKED_PLUGIN_MANAGER {
        step(
            link,
            fr_wire::services::tools::SERVICE_NAME,
            fr_wire::services::tools::METHOD_REGISTER,
            serde_json::to_vec(&plugin_manager_registration())
                .ok()?
                .as_slice(),
        )?;
    }
    // The terminal family (dsh `tool-terminal` + the two persistent
    // twins; one bake gate — the dsh base bundle mounts none, the
    // `BAKED_STR_REPLACE_EDITOR` judgment). The registered tool names
    // of the persistent twins collide with the plain shell tools by
    // design (the dsh packages never co-load).
    if crate::terminal_tools::terminal_tools_registered() {
        for registration in [
            terminal_open_registration(),
            terminal_send_registration(),
            terminal_read_registration(),
            terminal_signal_registration(),
            terminal_close_registration(),
            terminal_list_registration(),
            persistent_bash_registration(),
            persistent_pwsh_registration(),
        ] {
            step(
                link,
                fr_wire::services::tools::SERVICE_NAME,
                fr_wire::services::tools::METHOD_REGISTER,
                serde_json::to_vec(&registration).ok()?.as_slice(),
            )?;
        }
    }
    register_spill_listeners(link);
    register_finalize_listeners(link);
    Some(())
}

/// The `web_search` registration row (dsh `ctx.tools.register`,
/// call-for-call; the full face with the baked search budget and the
/// concurrency-safe provider read).
#[must_use]
pub fn web_search_registration() -> Value {
    json!({
        "name": "web_search",
        "description": crate::catalog_gen::WEB_SEARCH_DESCRIPTION,
        "parameters": crate::schema::web_search_parameters(),
        "output": {
            "schema": crate::schema::web_search_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_WEB_SEARCH_RENDER,
            },
            "presentationMeta": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_WEB_SEARCH_PRESENTATION_META,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_WEB_SEARCH_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_WEB_SEARCH_PRESENT_CALL,
        },
        "presentResult": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_WEB_SEARCH_PRESENT_RESULT,
        },
        "isConcurrencySafe": true,
        "timeoutMs": crate::web_tools::BAKED_SEARCH_TIMEOUT_MS,
    })
}

/// The `web_fetch` registration row (dsh `ctx.tools.register`,
/// call-for-call; the full face with the baked fetch budget and the
/// concurrency-safe provider read).
#[must_use]
pub fn web_fetch_registration() -> Value {
    json!({
        "name": "web_fetch",
        "description": crate::catalog_gen::WEB_FETCH_DESCRIPTION,
        "parameters": crate::schema::web_fetch_parameters(),
        "output": {
            "schema": crate::schema::web_fetch_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_WEB_FETCH_RENDER,
            },
            "presentationMeta": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_WEB_FETCH_PRESENTATION_META,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_WEB_FETCH_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_WEB_FETCH_PRESENT_CALL,
        },
        "presentResult": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_WEB_FETCH_PRESENT_RESULT,
        },
        "isConcurrencySafe": true,
        "timeoutMs": crate::web_tools::BAKED_FETCH_TIMEOUT_MS,
    })
}

/// The `working_directory` registration row (dsh `ctx.tools.register`).
///
/// Call-for-call: the dsh defineTool carries no custom present faces
/// and no concurrency flag, so the partial face keeps exactly the
/// execute/render output pair — the exclusive, generic-presentation
/// default.
#[must_use]
pub fn working_directory_registration() -> Value {
    json!({
        "name": "working_directory",
        "description": crate::catalog_gen::WORKING_DIRECTORY_DESCRIPTION,
        "parameters": crate::schema::working_directory_parameters(),
        "output": {
            "schema": crate::schema::working_directory_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_WORKING_DIRECTORY_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_WORKING_DIRECTORY_EXECUTE,
        },
    })
}

/// The `skill` registration row (dsh `ctx.tools.register(skillTool)`, 1:1).
///
/// The catalog-verbatim description, the single `name` parameter, the
/// output schema with the render projection, and the three endpoints
/// (the fr service's own selector band; tool-skill/src/index.ts:106-165).
#[must_use]
pub fn skill_registration() -> Value {
    json!({
        "name": "skill",
        "description": crate::catalog_gen::SKILL_DESCRIPTION,
        "parameters": crate::schema::skill_parameters(),
        "output": {
            "schema": crate::schema::skill_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_SKILL_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_SKILL_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_SKILL_PRESENT_CALL,
        },
    })
}

/// The `cordis_inspect_list` registration row (dsh `ctx.tools.
/// register`; the baked gate stays off per the Q16f-B ruling).
#[must_use]
pub fn cordis_inspect_list_registration() -> Value {
    json!({
        "name": "cordis_inspect_list",
        "description": crate::catalog_gen::CORDIS_INSPECT_LIST_DESCRIPTION,
        "parameters": crate::schema::cordis_inspect_list_parameters(),
        "output": {
            "schema": crate::schema::inspect_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_INSPECT_LIST_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_INSPECT_LIST_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_INSPECT_LIST_PRESENT_CALL,
        },
    })
}

/// The `cordis_inspect_query` registration row (dsh `ctx.tools.
/// register`; the baked gate stays off per the Q16f-B ruling).
#[must_use]
pub fn cordis_inspect_query_registration() -> Value {
    json!({
        "name": "cordis_inspect_query",
        "description": crate::catalog_gen::CORDIS_INSPECT_QUERY_DESCRIPTION,
        "parameters": crate::schema::cordis_inspect_query_parameters(),
        "output": {
            "schema": crate::schema::inspect_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_INSPECT_QUERY_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_INSPECT_QUERY_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_INSPECT_QUERY_PRESENT_CALL,
        },
    })
}

/// The `lsp` registration row (dsh `ctx.tools.register`; the baked
/// gate stays off per the base-bundle mirror — the row serves the
/// web-app/desktop composed faces).
#[must_use]
pub fn lsp_registration() -> Value {
    json!({
        "name": "lsp",
        "description": crate::catalog_gen::LSP_DESCRIPTION,
        "parameters": crate::schema::lsp_parameters(),
        "output": {
            "schema": crate::schema::lsp_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_LSP_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_LSP_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_LSP_PRESENT_CALL,
        },
        "timeoutMs": crate::lsp_tools::BAKED_LSP_TIMEOUT_MS,
    })
}

/// The `load_workspace_dependencies` registration row (dsh
/// `ctx.tools.register`; the baked gate stays off per the Q16f-E
/// ruling — the row serves opt-in deployments carrying a payload).
#[must_use]
pub fn load_workspace_dependencies_registration() -> Value {
    json!({
        "name": "load_workspace_dependencies",
        "description": crate::catalog_gen::LOAD_WORKSPACE_DEPENDENCIES_DESCRIPTION,
        "parameters": crate::schema::load_workspace_dependencies_parameters(),
        "output": {
            "schema": crate::schema::load_workspace_dependencies_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_WSD_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_WSD_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_WSD_PRESENT_CALL,
        },
    })
}

/// The `plugin_manager` registration row (dsh `ctx.tools.register`,
/// call-for-call; the partial face kept out of the baked composition
/// by the Q16f-A opt-in gate).
///
/// The face omits the presentResult and presentationMeta keys, rides
/// exclusive scheduling, and carries no timeout.
#[must_use]
pub fn plugin_manager_registration() -> Value {
    json!({
        "name": "plugin_manager",
        "description": crate::catalog_gen::PLUGIN_MANAGER_DESCRIPTION.to_owned(),
        "parameters": crate::schema::plugin_manager_parameters(),
        "output": {
            "schema": crate::schema::plugin_manager_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_PLUGIN_MANAGER_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_PLUGIN_MANAGER_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_PLUGIN_MANAGER_PRESENT_CALL,
        },
    })
}

/// The `terminal_open` registration row (dsh `ctx.tools.register`,
/// call-for-call; the partial face carries presentCall only).
#[must_use]
pub fn terminal_open_registration() -> Value {
    json!({
        "name": "terminal_open",
        "description": crate::catalog_gen::TERMINAL_OPEN_DESCRIPTION,
        "parameters": crate::schema::terminal_open_parameters(),
        "output": {
            "schema": crate::schema::terminal_open_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_TERMINAL_OPEN_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_TERMINAL_OPEN_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_TERMINAL_OPEN_PRESENT_CALL,
        },
    })
}

/// The `terminal_send` registration row (dsh `ctx.tools.register`;
/// the full face: presentResult and presentationMeta ride).
#[must_use]
pub fn terminal_send_registration() -> Value {
    json!({
        "name": "terminal_send",
        "description": crate::schema::terminal_send_description(
            crate::terminal_tools::BAKED_ENABLE_RUN_IN_BACKGROUND,
        ),
        "parameters": crate::schema::terminal_send_parameters(
            crate::terminal_tools::BAKED_ENABLE_RUN_IN_BACKGROUND,
        ),
        "output": {
            "schema": crate::schema::terminal_send_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_TERMINAL_SEND_RENDER,
            },
            "presentationMeta": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_TERMINAL_SEND_PRESENTATION_META,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_TERMINAL_SEND_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_TERMINAL_SEND_PRESENT_CALL,
        },
        "presentResult": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_TERMINAL_SEND_PRESENT_RESULT,
        },
    })
}

/// The `terminal_read` registration row (dsh `ctx.tools.register`,
/// call-for-call).
#[must_use]
pub fn terminal_read_registration() -> Value {
    json!({
        "name": "terminal_read",
        "description": crate::catalog_gen::TERMINAL_READ_DESCRIPTION,
        "parameters": crate::schema::terminal_read_parameters(),
        "output": {
            "schema": crate::schema::terminal_read_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_TERMINAL_READ_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_TERMINAL_READ_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_TERMINAL_READ_PRESENT_CALL,
        },
    })
}

/// The `terminal_signal` registration row (dsh `ctx.tools.register`,
/// call-for-call).
#[must_use]
pub fn terminal_signal_registration() -> Value {
    json!({
        "name": "terminal_signal",
        "description": crate::catalog_gen::TERMINAL_SIGNAL_DESCRIPTION,
        "parameters": crate::schema::terminal_signal_parameters(),
        "output": {
            "schema": crate::schema::terminal_signal_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_TERMINAL_SIGNAL_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_TERMINAL_SIGNAL_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_TERMINAL_SIGNAL_PRESENT_CALL,
        },
    })
}

/// The `terminal_close` registration row (dsh `ctx.tools.register`,
/// call-for-call).
#[must_use]
pub fn terminal_close_registration() -> Value {
    json!({
        "name": "terminal_close",
        "description": crate::catalog_gen::TERMINAL_CLOSE_DESCRIPTION,
        "parameters": crate::schema::terminal_close_parameters(),
        "output": {
            "schema": crate::schema::terminal_close_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_TERMINAL_CLOSE_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_TERMINAL_CLOSE_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_TERMINAL_CLOSE_PRESENT_CALL,
        },
    })
}

/// The `terminal_list` registration row (dsh `ctx.tools.register`,
/// call-for-call).
#[must_use]
pub fn terminal_list_registration() -> Value {
    json!({
        "name": "terminal_list",
        "description": crate::catalog_gen::TERMINAL_LIST_DESCRIPTION,
        "parameters": crate::schema::terminal_list_parameters(),
        "output": {
            "schema": crate::schema::terminal_list_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_TERMINAL_LIST_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_TERMINAL_LIST_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_TERMINAL_LIST_PRESENT_CALL,
        },
    })
}

/// The persistent `bash` registration row (dsh
/// `tool-bash-persistent`: the plain-string output contract, the
/// terminal present-call card, no finalizeContent).
#[must_use]
pub fn persistent_bash_registration() -> Value {
    json!({
        "name": "bash",
        "description": crate::catalog_gen::BASH_PERSISTENT_DESCRIPTION,
        "parameters": crate::schema::persistent_bash_parameters(),
        "output": {
            "schema": crate::schema::persistent_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_PERSISTENT_BASH_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_PERSISTENT_BASH_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_PERSISTENT_BASH_PRESENT_CALL,
        },
    })
}

/// The persistent `pwsh` registration row (the bash twin over the
/// PowerShell texts).
#[must_use]
pub fn persistent_pwsh_registration() -> Value {
    json!({
        "name": "pwsh",
        "description": crate::catalog_gen::PWSH_PERSISTENT_DESCRIPTION,
        "parameters": crate::schema::persistent_pwsh_parameters(),
        "output": {
            "schema": crate::schema::persistent_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_PERSISTENT_PWSH_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_PERSISTENT_PWSH_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_PERSISTENT_PWSH_PRESENT_CALL,
        },
    })
}

/// The `job_output` registration row (dsh `ctx.tools.register`,
/// call-for-call).
#[must_use]
pub fn job_output_registration() -> Value {
    json!({
        "name": "job_output",
        "description": crate::catalog_gen::JOB_OUTPUT_DESCRIPTION,
        "parameters": crate::schema::job_output_parameters(),
        "output": {
            "schema": crate::schema::job_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_JOB_OUTPUT_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_JOB_OUTPUT_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_JOB_OUTPUT_PRESENT_CALL,
        },
    })
}

/// The `job_list` registration row (dsh `ctx.tools.register`,
/// call-for-call).
#[must_use]
pub fn job_list_registration() -> Value {
    json!({
        "name": "job_list",
        "description": crate::catalog_gen::JOB_LIST_DESCRIPTION,
        "parameters": crate::schema::job_list_parameters(),
        "output": {
            "schema": crate::schema::job_list_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_JOB_LIST_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_JOB_LIST_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_JOB_LIST_PRESENT_CALL,
        },
    })
}

/// The `job_kill` registration row (dsh `ctx.tools.register`,
/// call-for-call).
#[must_use]
pub fn job_kill_registration() -> Value {
    json!({
        "name": "job_kill",
        "description": crate::catalog_gen::JOB_KILL_DESCRIPTION,
        "parameters": crate::schema::job_kill_parameters(),
        "output": {
            "schema": crate::schema::job_kill_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_JOB_KILL_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_JOB_KILL_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_JOB_KILL_PRESENT_CALL,
        },
    })
}

/// The `schedule_create` registration row (dsh `ctx.tools.register`,
/// call-for-call).
#[must_use]
pub fn schedule_create_registration() -> Value {
    json!({
        "name": "schedule_create",
        "description": crate::catalog_gen::SCHEDULE_CREATE_DESCRIPTION,
        "parameters": crate::schema::schedule_create_parameters(),
        "output": {
            "schema": crate::schema::schedule_create_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_SCHEDULE_CREATE_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_SCHEDULE_CREATE_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_SCHEDULE_CREATE_PRESENT_CALL,
        },
    })
}

/// The `schedule_list` registration row (dsh `ctx.tools.register`,
/// call-for-call).
#[must_use]
pub fn schedule_list_registration() -> Value {
    json!({
        "name": "schedule_list",
        "description": crate::catalog_gen::SCHEDULE_LIST_DESCRIPTION,
        "parameters": crate::schema::schedule_list_parameters(),
        "output": {
            "schema": crate::schema::schedule_list_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_SCHEDULE_LIST_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_SCHEDULE_LIST_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_SCHEDULE_LIST_PRESENT_CALL,
        },
    })
}

/// The `schedule_delete` registration row (dsh `ctx.tools.register`,
/// call-for-call).
#[must_use]
pub fn schedule_delete_registration() -> Value {
    json!({
        "name": "schedule_delete",
        "description": crate::catalog_gen::SCHEDULE_DELETE_DESCRIPTION,
        "parameters": crate::schema::schedule_delete_parameters(),
        "output": {
            "schema": crate::schema::schedule_delete_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_SCHEDULE_DELETE_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_SCHEDULE_DELETE_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_SCHEDULE_DELETE_PRESENT_CALL,
        },
    })
}

/// The `ask_user_question` registration row (dsh `ctx.tools.register`,
/// the blocking legacy default: no presentCall, no presentationMeta,
/// and the timed schema variant stays the Q16d-3A pure arm).
#[must_use]
pub fn ask_user_question_registration() -> Value {
    json!({
        "name": "ask_user_question",
        "description": crate::catalog_gen::ASK_USER_QUESTION_DESCRIPTION,
        "parameters": crate::schema::ask_user_question_parameters(),
        "output": {
            "schema": crate::schema::ask_user_question_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_ASK_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_ASK_EXECUTE,
        },
    })
}

/// The `exit_plan_mode` registration row (dsh plan-mode
/// `ctx.tools.register`: the catalog-stable schema that stays mounted
/// while plan mode is inactive).
#[must_use]
pub fn exit_plan_mode_registration() -> Value {
    json!({
        "name": "exit_plan_mode",
        "description": crate::catalog_gen::EXIT_PLAN_MODE_DESCRIPTION,
        "parameters": crate::schema::exit_plan_mode_parameters(),
        "output": {
            "schema": crate::schema::exit_plan_mode_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_EXIT_PLAN_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_EXIT_PLAN_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_EXIT_PLAN_PRESENT_CALL,
        },
        "presentResult": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_EXIT_PLAN_PRESENT_RESULT,
        },
    })
}

/// The `schedule_update` registration row (dsh `ctx.tools.register`,
/// call-for-call).
#[must_use]
pub fn schedule_update_registration() -> Value {
    json!({
        "name": "schedule_update",
        "description": crate::catalog_gen::SCHEDULE_UPDATE_DESCRIPTION,
        "parameters": crate::schema::schedule_update_parameters(),
        "output": {
            "schema": crate::schema::schedule_update_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_SCHEDULE_UPDATE_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_SCHEDULE_UPDATE_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_SCHEDULE_UPDATE_PRESENT_CALL,
        },
    })
}

/// Registers the two finalize listeners (the dsh `finalizeContent` of
/// `job_output`/`job_kill`; the spill-listener registration shape over
/// the post-execute CPS face).
fn register_finalize_listeners(link: &HostLink) -> Option<()> {
    for method in [
        crate::service::METHOD_JOB_OUTPUT_FINALIZE,
        crate::service::METHOD_JOB_KILL_FINALIZE,
    ] {
        step(
            link,
            fr_wire::services::tools::SERVICE_NAME,
            fr_wire::services::tools::METHOD_REGISTER_LISTENER,
            serde_json::to_vec(&json!({
                "stage": "post-execute",
                "endpoint": {
                    "service": crate::service::SERVICE_NAME,
                    "method": method,
                },
            }))
            .ok()?
            .as_slice(),
        )?;
    }
    // The six terminal tools share one finalizeContent (the dsh
    // shared const); the listener registers with the family.
    if crate::terminal_tools::terminal_tools_registered() {
        step(
            link,
            fr_wire::services::tools::SERVICE_NAME,
            fr_wire::services::tools::METHOD_REGISTER_LISTENER,
            serde_json::to_vec(&json!({
                "stage": "post-execute",
                "endpoint": {
                    "service": crate::service::SERVICE_NAME,
                    "method": crate::service::METHOD_TERMINAL_FINALIZE,
                },
            }))
            .ok()?
            .as_slice(),
        )?;
    }
    Some(())
}

/// The `get_goal` registration row (dsh `ctx.tools.register`,
/// call-for-call).
#[must_use]
pub fn get_goal_registration() -> Value {
    json!({
        "name": "get_goal",
        "description": crate::catalog_gen::GET_GOAL_DESCRIPTION,
        "parameters": crate::schema::get_goal_parameters(),
        "output": {
            "schema": crate::schema::goal_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_GET_GOAL_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_GET_GOAL_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_GET_GOAL_PRESENT_CALL,
        },
    })
}

/// The `create_goal` registration row (dsh `ctx.tools.register`,
/// call-for-call).
#[must_use]
pub fn create_goal_registration() -> Value {
    json!({
        "name": "create_goal",
        "description": crate::catalog_gen::CREATE_GOAL_DESCRIPTION,
        "parameters": crate::schema::create_goal_parameters(),
        "output": {
            "schema": crate::schema::goal_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_CREATE_GOAL_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_CREATE_GOAL_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_CREATE_GOAL_PRESENT_CALL,
        },
    })
}

/// The `update_goal` registration row (dsh `ctx.tools.register`,
/// call-for-call).
#[must_use]
pub fn update_goal_registration() -> Value {
    json!({
        "name": "update_goal",
        "description": crate::catalog_gen::UPDATE_GOAL_DESCRIPTION,
        "parameters": crate::schema::update_goal_parameters(),
        "output": {
            "schema": crate::schema::goal_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_UPDATE_GOAL_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_UPDATE_GOAL_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_UPDATE_GOAL_PRESENT_CALL,
        },
    })
}

/// The `todo_write` registration row (dsh `ctx.tools.register(defineTool)`,
/// call-for-call).
///
/// No `isConcurrencySafe` key (the fail-closed exclusive default), no
/// presentResult/presentationMeta endpoints (the dsh source declares
/// none), and the `other` presentation family riding presentCall.
#[must_use]
pub fn todo_write_registration() -> Value {
    json!({
        "name": "todo_write",
        "description": crate::schema::todo_write_description(true),
        "parameters": crate::schema::todo_write_parameters(),
        "output": {
            "schema": crate::schema::todo_write_output_schema(),
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_TODO_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_TODO_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_TODO_PRESENT_CALL,
        },
    })
}

/// Registers the two capped-result spill listeners (dsh
/// `ctx.on('tools/post-execute')` in the apply functions; the fr
/// tools-pipeline registration face).
fn register_spill_listeners(link: &HostLink) -> Option<()> {
    for (method, tool_name) in [
        (crate::service::METHOD_GLOB_SPILL, "glob"),
        (crate::service::METHOD_GREP_SPILL, "grep"),
    ] {
        let _ = tool_name;
        step(
            link,
            fr_wire::services::tools::SERVICE_NAME,
            fr_wire::services::tools::METHOD_REGISTER_LISTENER,
            serde_json::to_vec(&json!({
                "stage": "post-execute",
                "endpoint": {
                    "service": crate::service::SERVICE_NAME,
                    "method": method,
                },
            }))
            .ok()?
            .as_slice(),
        )?;
    }
    Some(())
}

/// The schedule domain's wire seams (dsh `ctx.agents`/`ctx.sessions`
/// delivery faces plus the fr-storage task rows and the kernel emit).
struct WireScheduleSeams {
    link: HostLink,
}

impl std::fmt::Debug for WireScheduleSeams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WireScheduleSeams").finish_non_exhaustive()
    }
}

impl crate::schedule::ScheduleSeams for WireScheduleSeams {
    fn table_entries(&self) -> Result<Vec<(String, Value)>, String> {
        let reply = self.link.require(
            fr_wire::services::storage_domain::SERVICE_NAME,
            fr_wire::services::storage_domain::METHOD_TABLE_ENTRIES,
            &serde_json::to_vec(&json!({
                "domain": crate::schedule::DOMAIN_NAME,
                "table": crate::schedule::DOMAIN_TABLE,
            }))
            .map_err(|error| format!("entries encode failed: {error}"))?,
        )?;
        let parsed: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("entries decode failed: {error}"))?;
        let mut rows = Vec::new();
        for entry in parsed
            .get("entries")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let key = entry
                .get("key")
                .and_then(Value::as_str)
                .ok_or_else(|| "entries reply carries a non-string key".to_owned())?
                .to_owned();
            let value = entry.get("value").cloned().unwrap_or(Value::Null);
            rows.push((key, value));
        }
        Ok(rows)
    }

    fn put_task(&self, id: &str, task: &Value) -> Result<(), String> {
        self.link.require(
            fr_wire::services::storage_domain::SERVICE_NAME,
            fr_wire::services::storage_domain::METHOD_PUT_RECORD,
            &serde_json::to_vec(&json!({
                "domain": crate::schedule::DOMAIN_NAME,
                "table": crate::schedule::DOMAIN_TABLE,
                "key": id,
                "value": task,
            }))
            .map_err(|error| format!("put encode failed: {error}"))?,
        )?;
        Ok(())
    }

    fn delete_task(&self, id: &str) -> Result<(), String> {
        self.link.require(
            fr_wire::services::storage_domain::SERVICE_NAME,
            fr_wire::services::storage_domain::METHOD_DELETE_RECORD,
            &serde_json::to_vec(&json!({
                "domain": crate::schedule::DOMAIN_NAME,
                "table": crate::schedule::DOMAIN_TABLE,
                "key": id,
            }))
            .map_err(|error| format!("delete encode failed: {error}"))?,
        )?;
        Ok(())
    }

    fn emit_changed(&self) {
        self.link.emit("schedule/changed", "{}");
    }

    fn delegation_depth(&self, _session: &str) -> u32 {
        // The M1 composition has no delegation registry yet (the batch-0
        // refusal-arm-in-place ruling): every session reads as a
        // top-level caller until the Phase-18 wiring lands.
        0
    }

    fn followup(&self, session: &str, message: &Value) -> Result<(), String> {
        self.link.require(
            fr_wire::services::agent::SERVICE_NAME,
            fr_wire::services::agent::METHOD_FOLLOWUP,
            &serde_json::to_vec(&json!({ "sessionId": session, "message": message }))
                .map_err(|error| format!("followup encode failed: {error}"))?,
        )?;
        Ok(())
    }

    fn flush(&self, session: &str) -> Result<(), String> {
        self.link.require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_SESSION_FLUSH,
            &serde_json::to_vec(&json!({ "id": session }))
                .map_err(|error| format!("flush encode failed: {error}"))?,
        )?;
        Ok(())
    }
}

/// Opens the schedule storage domain over the two-phase protocol: the
/// prepared snapshot's rows decode against the task schema, a stored key
/// that differs from its record id closes the domain before the refusal,
/// and the validated open commits (the dsh `ready` arm; a refusal
/// declines the activation fail-closed).
fn open_schedule_domain(link: &HostLink) -> Option<()> {
    let payload = serde_json::to_vec(&json!({
        "name": crate::schedule::DOMAIN_NAME,
        "version": crate::schedule::DOMAIN_VERSION,
        "tables": [crate::schedule::DOMAIN_TABLE],
        "hasGlobal": false,
    }))
    .ok()?;
    let reply = link
        .require(
            fr_wire::services::storage_domain::SERVICE_NAME,
            fr_wire::services::storage_domain::METHOD_OPEN,
            &payload,
        )
        .ok()?;
    let snapshot: Value = serde_json::from_slice(&reply).ok()?;
    let rows = snapshot.pointer("/records/tasks")?.as_object()?.clone();
    let mut valid = true;
    for (key, row) in &rows {
        let task = crate::schedule::validate_task_row(row).is_ok();
        let binding = row
            .get("record")
            .and_then(|record| record.get("id"))
            .and_then(Value::as_str)
            .is_some_and(|id| id == key);
        if !task || !binding {
            eprintln!(
                "schedule: stored task key \"{key}\" is not a valid row bound to its record id"
            );
            valid = false;
            break;
        }
    }
    let method = if valid {
        fr_wire::services::storage_domain::METHOD_COMMIT
    } else {
        fr_wire::services::storage_domain::METHOD_REFUSE
    };
    let settle = serde_json::to_vec(&json!({ "name": crate::schedule::DOMAIN_NAME })).ok()?;
    if link
        .require(
            fr_wire::services::storage_domain::SERVICE_NAME,
            method,
            &settle,
        )
        .is_err()
    {
        return None;
    }
    valid.then_some(())
}

impl BuiltinState {
    /// Assembles the state over one copied vtable, wiring the whole
    /// registration sequence; `None` declines the activation
    /// fail-closed.
    #[expect(
        clippy::too_many_lines,
        reason = "the assembly mirrors the dsh plugin composition one arm per domain; splitting it would hide the correspondence"
    )]
    #[must_use]
    pub fn try_new(link: Option<HostLink>) -> Option<Self> {
        let link = link?;

        declare_change_event(&link)?;
        // The D31 grants: the tools fabric consumes the execute/render
        // endpoints; the projection relay consumes the fold endpoint.
        link.grant("tools", "builtin-tools").ok()?;
        link.grant("session-projections", "builtin-tools").ok()?;
        // The 2026-11-04 cycle-cut ruling: the loop reads the skill
        // catalog lazily (the dsh soft read), so the capability rides a
        // grant instead of a descriptor inject — a declared inject would
        // gate the loop's activation on this crate while this crate gates
        // on shell, which gates on the loop (the first true co-load caught
        // the declared cycle). The timing holds: the loop's remaining
        // injects are acyclic, so it commits before shell, shell before
        // this crate, and the grant's target is committed here.
        link.grant("agent", "skills").ok()?;
        let escalation = read_default_mode(&link)?;
        // The composition gate (the dsh `ctx.inject(['attachments'], …)`
        // conditional registration): without a committed durable store
        // the deployment cannot durably commit image bytes, so the tool
        // never registers; the execute body keeps a defensive re-check
        // for direct callers.
        let attachments_mounted = link
            .require(
                fr_wire::services::attachments::SERVICE_NAME,
                fr_wire::services::attachments::METHOD_DESCRIBE,
                b"{}",
            )
            .is_ok();
        // The completion-notice machine: the queue and the plugin-owned
        // worker thread (joined at teardown; the pump discipline). The
        // worker rides a copy of the shared HostLink — its requires
        // serialize through the host contract like every other consumer
        // thread.
        let (tasks, receiver) = std::sync::mpsc::channel::<crate::job_tools::NoticeTask>();
        let notices = Arc::new(crate::job_tools::NoticeState::new(
            tasks.clone(),
            crate::job_tools::BAKED_DELIVERY,
            crate::job_tools::BAKED_WAKE_BUDGET,
        ));
        let worker = crate::job_tools::spawn_notice_worker(Arc::clone(&notices), receiver, link);
        register_sections(&link)?;
        register_plan_policy_section(&link)?;
        register_working_directory_context(&link)?;
        register_projection(&link)?;
        register_tools(&link, !escalation.is_empty(), attachments_mounted)?;

        // The managed-environment registry over the resolved home.
        let env = ShellEnvRegistry::new(None).ok()?;

        // The working-directory domain over the wire log/inject seams
        // and the in-crate fs domain (the dsh ctx.fs isomorph; built
        // first so the directory legs resolve through it).
        let config = crate::fs_domain::FsDomainConfig::new().ok()?;
        let policy = WireStandingPolicy { link };
        let fs = Arc::new(crate::fs_domain::FsDomain::new(
            config,
            Box::new(policy),
            escalation.first().map(|mode| (*mode).to_owned()),
        ));
        let directories = WorkingDirectories::new(
            Arc::new(WireSessionLog { link }),
            Arc::clone(&fs),
            Arc::new(WireAgentInject { link }),
            std::env::current_dir().ok()?.to_string_lossy().to_string(),
        );

        // The todo domain over the wire append seam (the baked
        // `allow-parallel-in-progress: true` bundle judgment).
        let lists = crate::todo::TodoLists::new(true, Box::new(WireSessionAppend { link }));

        // The goal domain over the wire read/append/relay seams (the
        // baked `default-max-goal-rounds: 256` and
        // `blocked-after-consecutive-rounds: 3` defaults; the bundle
        // adds no goal config rows — the dsh base judgment).
        let goals = crate::goal_service::Goals::new(Box::new(WireGoalSeams { link }));

        // The schedule domain over the fr-storage seam (the Q16c-3B
        // wiring): the two-phase open validates every stored row before
        // the commit, so a corrupt task refuses the activation
        // fail-closed (the dsh constructor throw, carried by the
        // activation decline).
        let schedules = Arc::new(crate::schedule::Schedules::new(Box::new(
            WireScheduleSeams { link },
        )));
        open_schedule_domain(&link)?;
        let signal = crate::schedule::runtime_handle();
        let state_signal = Arc::clone(&signal);
        schedules.bind_signal(Arc::clone(&signal));
        let schedule_worker = crate::schedule::spawn_runtime(Arc::clone(&schedules), signal);

        let env_arc = Arc::new(env);
        let directories_arc = Arc::new(directories);
        let seams = WireSeams {
            link,
            directories: Arc::clone(&directories_arc),
            env: Arc::clone(&env_arc),
        };
        let interactions = Arc::new(crate::interaction_tools::InteractionState {
            questions: crate::user_questions::TimedWaits::default(),
            plans: Box::new(WireGoalSeams { link }),
            ask: crate::interaction_tools::InteractionState::production_ask(),
            disposed: false,
            user_questions_present: true,
        });
        let session_query = Arc::new(crate::session_query_tools::SessionQueryRuntime {
            engine: Arc::new(crate::session_query::SessionQueryEngine::new(
                Arc::new(WireSessionQuery { link }),
                crate::session_query::EngineConfig::default(),
            )),
            max_search_results: crate::session_query_tools::BAKED_MAX_SEARCH_RESULTS,
        });
        // The baked web runtime (the dsh base composition: the four
        // base rows' providers register over the baked group defaults;
        // the exa/perplexity implementations ride in with their
        // availability gates absorbing the missing credentials, and the
        // baked selection keeps the ambiguity arm unreachable).
        let web = build_web_runtime()?;
        // The skill runtime over the single-layer registry (Q93-A): the
        // baked collect-cache cap (the dsh `DEFAULT_COLLECT_CACHE_ENTRIES`
        // judgment), the filesystem provider registers first (the dsh
        // base composition's only skill provider), and the toolVisible
        // gate flips right after the registration row landed in
        // `register_tools` above.
        let registry = std::sync::Arc::new(
            crate::skill_runtime::SkillRegistry::new(
                crate::skill_runtime::DEFAULT_COLLECT_CACHE_ENTRIES,
            )
            .ok()?,
        );
        // The filesystem provider over the in-crate fs domain and the
        // process environment (the dsh `$DSH_HOME` resolution chain; the
        // baked config carries no overrides, so the defaults resolve the
        // real roots).
        let skills_provider = crate::skill_filesystem::FileSystemSkillProvider::new(
            std::sync::Arc::downgrade(&registry),
            std::sync::Arc::clone(&fs),
            &crate::skill_filesystem::FileSystemProviderConfig::default(),
            &|key: &str| std::env::var(key).ok(),
        )
        .ok()?;
        // The registration disposer never fires: the teardown drops the
        // whole registry and the provider's Drop stops its watch thread
        // (the kernel reverses the activation; the dsh provider
        // registration is likewise never unregistered).
        drop(
            registry
                .register_provider(|_| Ok(Box::new(skills_provider)))
                .ok()?,
        );
        let skills = std::sync::Arc::new(crate::skill_tools::SkillRuntime {
            catalog_description_max_length:
                crate::skill_tools::resolve_catalog_description_max_length(
                    crate::skill_tools::DEFAULT_CATALOG_DESCRIPTION_MAX_LENGTH,
                )
                .ok()?,
            registered: std::sync::atomic::AtomicBool::new(false),
            next_id: std::sync::Arc::new(|| {
                fr_wire::ids::MessageId::new(uuid::Uuid::new_v4().to_string())
            }),
            registry,
        });
        // The registration row landed in `register_tools` above (the
        // dsh `ctx.tools.get('skill', agent) === skillTool` gate); the
        // gate flip is the row's liveness fact.
        if crate::skill_tools::BAKED_SKILL_TOOLS {
            skills.mark_registered();
        }
        let tools = BuiltinTools::new(
            Box::new(seams),
            env_arc,
            directories_arc,
            Arc::new(lists),
            Arc::new(goals),
            notices,
            Arc::clone(&schedules),
            Arc::clone(&interactions),
            Arc::clone(&session_query),
            std::sync::Arc::new(crate::terminal_tools::FinalizeParks::default()),
            std::sync::Arc::new(crate::persistent::PersistentShells::default()),
            std::sync::Arc::new(crate::inspect_tools::InspectRegistry::new()),
            std::sync::Arc::new(crate::lsp_tools::LspRegistry::new()),
            None,
            std::sync::Arc::new(web),
            Some(crate::web_tools::BAKED_SEARCH_PROVIDER.to_owned()),
            Some(crate::web_tools::BAKED_FETCH_PROVIDER.to_owned()),
            Some(std::sync::Arc::clone(&skills)),
        )
        .ok()?;

        Some(Self {
            tools,
            fs,
            gate: crate::observation::ObservedStateGate::new(),
            link,
            pending: std::sync::Mutex::new(std::collections::HashMap::new()),
            tasks: Some(tasks),
            worker: Some(worker),
            schedule_worker: Some(schedule_worker),
            schedule_signal: state_signal,
        })
    }

    /// The hosted runtime (tests drive the wire face).
    #[must_use]
    pub const fn tools(&self) -> &BuiltinTools {
        &self.tools
    }

    /// The hosted fs domain (tests drive the wire face).
    #[must_use]
    pub const fn fs(&self) -> &Arc<crate::fs_domain::FsDomain> {
        &self.fs
    }

    /// The hosted observation gate (the fs tool bodies' policy slot).
    #[must_use]
    pub const fn gate(&self) -> &crate::observation::ObservedStateGate {
        &self.gate
    }
}

/// The standing sandbox-policy resolution over the wire (the dsh
/// `ctx.sandboxPolicy.resolve()` consumption).
struct WireStandingPolicy {
    link: HostLink,
}

impl crate::fs_domain::StandingPolicy for WireStandingPolicy {
    fn resolve(&self, session: Option<&str>) -> Result<Value, String> {
        let payload = session.map_or_else(|| json!({}), |session| json!({ "session": session }));
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("policy encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::sandbox_policy::SERVICE_NAME,
            fr_wire::services::sandbox_policy::METHOD_RESOLVE,
            &bytes,
        )?;
        serde_json::from_slice(&reply).map_err(|error| format!("policy decode failed: {error}"))
    }
}

/// The durable working-directory change records over the
/// `'session-persistence'` faces (the dsh `agent.session.append`/read
/// pair).
struct WireSessionLog {
    link: HostLink,
}

/// The model notices over the `'agent'` inject face (the dsh
/// `agent.inject(notice)` pair).
struct WireAgentInject {
    link: HostLink,
}

impl crate::working_directory::AgentInject for WireAgentInject {
    fn inject(&self, agent: &str, message: &Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(&json!({
            "sessionId": agent,
            "message": message,
        }))
        .map_err(|error| format!("notice encode failed: {error}"))?;
        self.link
            .require(
                fr_wire::services::agent::SERVICE_NAME,
                fr_wire::services::agent::METHOD_INJECT,
                &bytes,
            )
            .map(|_| ())
    }
}

impl crate::working_directory::SessionLog for WireSessionLog {
    fn read(&self, id: &str) -> Result<crate::working_directory::ColdLog, String> {
        let bytes = serde_json::to_vec(&json!({ "id": id }))
            .map_err(|error| format!("session read encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_SESSION_READ,
            &bytes,
        )?;
        let parsed: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("session read decode failed: {error}"))?;
        Ok(crate::working_directory::ColdLog {
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
        let bytes = serde_json::to_vec(&json!({
            "id": id,
            "type": crate::working_directory::CHANGE_EVENT,
            "data": { "cwd": cwd },
        }))
        .map_err(|error| format!("session append encode failed: {error}"))?;
        self.link.require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_SESSION_APPEND,
            &bytes,
        )?;
        Ok(())
    }
}

/// The durable todo snapshots over the `'session-persistence'` append
/// face (the dsh `agent.session.append` consumption for the whole-list
/// tool).
struct WireSessionAppend {
    link: HostLink,
}

impl crate::todo::SessionAppend for WireSessionAppend {
    fn append(&self, session: &str, event_type: &str, data: &Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(&json!({
            "id": session,
            "type": event_type,
            "data": data,
        }))
        .map_err(|error| format!("session append encode failed: {error}"))?;
        self.link.require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_SESSION_APPEND,
            &bytes,
        )?;
        Ok(())
    }
}

/// The goal domain's wire seams: the session-persistence read/append
/// faces and the session-projections relay (the dsh
/// `snapshotEvents`/`session.append`/`stateOf` consumption).
/// The interaction bodies ride the same wire seam (the delegated
/// projection read; one seam type serves both faces).
impl crate::interaction_tools::InteractionSeam for WireGoalSeams {
    fn projection_state(&self, session: &str, key: &str) -> Result<Option<Value>, String> {
        crate::goal_service::GoalSeams::projection_state(self, session, key)
    }
}

struct WireGoalSeams {
    link: HostLink,
}

impl std::fmt::Debug for WireGoalSeams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WireGoalSeams").finish_non_exhaustive()
    }
}

impl crate::goal_service::GoalSeams for WireGoalSeams {
    fn read_log(&self, session: &str) -> Result<Option<crate::goal_service::ColdGoalLog>, String> {
        let bytes = serde_json::to_vec(&json!({ "id": session }))
            .map_err(|error| format!("session read encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_SESSION_READ,
            &bytes,
        )?;
        let parsed: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("session read decode failed: {error}"))?;
        if parsed.get("header").is_none() {
            // The read face answers nothing for an absent session.
            return Ok(None);
        }
        Ok(Some(crate::goal_service::ColdGoalLog {
            inherited_event_count: parsed
                .get("inheritedEventCount")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            events: parsed
                .get("events")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        }))
    }

    fn append_change(&self, session: &str, change: &Value) -> Result<i64, String> {
        let bytes = serde_json::to_vec(&json!({
            "id": session,
            "type": crate::goal::CHANGE_EVENT,
            "data": change,
        }))
        .map_err(|error| format!("session append encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_SESSION_APPEND,
            &bytes,
        )?;
        let parsed: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("session append decode failed: {error}"))?;
        Ok(parsed
            .pointer("/event/seq")
            .and_then(Value::as_i64)
            .unwrap_or_default())
    }

    fn projection_state(&self, session: &str, key: &str) -> Result<Option<Value>, String> {
        let bytes = serde_json::to_vec(&json!({ "id": session, "key": key }))
            .map_err(|error| format!("projection encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::session_projections::SERVICE_NAME,
            fr_wire::services::session_projections::METHOD_STATE_OF,
            &bytes,
        )?;
        let parsed: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("projection decode failed: {error}"))?;
        Ok(parsed.get("state").cloned())
    }
}

/// The session-query corpus's wire source (the live read, the
/// persisted faces, and the projection relay over one copied vtable;
/// the `WireGoalSeams` judgment shape).
struct WireSessionQuery {
    link: HostLink,
}

impl std::fmt::Debug for WireSessionQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WireSessionQuery").finish_non_exhaustive()
    }
}

impl crate::session_query::QueryWire for WireSessionQuery {
    fn session_read(&self, id: &str) -> Result<Option<crate::session_query::QueryLog>, String> {
        let bytes = serde_json::to_vec(&json!({ "id": id }))
            .map_err(|error| format!("session read encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_SESSION_READ,
            &bytes,
        )?;
        let parsed: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("session read decode failed: {error}"))?;
        if parsed.get("header").is_none() {
            // The read face answers nothing for an absent session.
            return Ok(None);
        }
        Ok(Some(crate::session_query::QueryLog {
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
        }))
    }

    fn persisted_list(&self) -> Result<Vec<Value>, String> {
        let reply = self.link.require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_PERSISTED_LIST,
            b"{}",
        )?;
        let parsed: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("persisted list decode failed: {error}"))?;
        Ok(parsed
            .get("sessions")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| row.get("header").cloned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default())
    }

    fn persisted_cold_read(
        &self,
        id: &str,
    ) -> Result<Option<crate::session_query::QueryLog>, String> {
        let bytes = serde_json::to_vec(&json!({ "id": id }))
            .map_err(|error| format!("cold read encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::session_persistence::SERVICE_NAME,
            fr_wire::services::session_persistence::METHOD_PERSISTED_COLD_READ,
            &bytes,
        )?;
        let parsed: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("cold read decode failed: {error}"))?;
        let Some(log) = parsed.get("log") else {
            // The absent-log reply.
            return Ok(None);
        };
        Ok(Some(crate::session_query::QueryLog {
            header: log.get("header").cloned().unwrap_or(Value::Null),
            inherited_event_count: log
                .get("inheritedEventCount")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            events: log
                .get("events")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        }))
    }

    fn projection_state(&self, session: &str, key: &str) -> Result<Option<Value>, String> {
        let bytes = serde_json::to_vec(&json!({ "id": session, "key": key }))
            .map_err(|error| format!("projection encode failed: {error}"))?;
        let reply = self.link.require(
            fr_wire::services::session_projections::SERVICE_NAME,
            fr_wire::services::session_projections::METHOD_STATE_OF,
            &bytes,
        )?;
        let parsed: Value = serde_json::from_slice(&reply)
            .map_err(|error| format!("projection decode failed: {error}"))?;
        Ok(parsed.get("state").cloned())
    }
}

unsafe extern "C" fn plugin_name(out: *mut u8, cap: usize, len: *mut usize) -> i32 {
    // SAFETY: the host guarantees a `cap`-sized out-buffer and a
    // writable `len` slot (export-table contract).
    if unsafe { write_out(PLUGIN_NAME.as_bytes(), out, cap, len) } {
        STATUS_OK
    } else {
        STATUS_CAPACITY
    }
}

unsafe extern "C" fn descriptor(out: *mut u8, cap: usize, len: *mut usize) -> i32 {
    // SAFETY: same host buffer contract as `plugin_name`.
    if unsafe { write_out(DESCRIPTOR.as_bytes(), out, cap, len) } {
        STATUS_OK
    } else {
        STATUS_CAPACITY
    }
}

unsafe extern "C" fn setup(host: *const HostExports) -> *mut c_void {
    let link = if host.is_null() {
        None
    } else {
        // SAFETY: the vtable contract guarantees validity for this call;
        // copying by value is the sanctioned pattern (dylib-plugin.md).
        Some(HostLink::new(unsafe { *host }))
    };
    let Some(state) = BuiltinState::try_new(link) else {
        // Decline activation (null handle): a half-wired tools fabric
        // must not exist.
        return std::ptr::null_mut();
    };
    Box::into_raw(Box::new(state)).cast::<c_void>()
}

unsafe extern "C" fn call(
    handle: *mut c_void,
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
    // The host contract delivers `in_data` valid for `in_len` readable
    // bytes; an empty payload arrives as null/0.
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
    let state = unsafe { &*(handle.cast::<BuiltinState>()) };
    // The fs domain and the tool bands ride the same export table (the
    // per-artifact method-space judgment; the fr-shell three-band
    // precedent).
    let reply = if (fr_wire::services::fs::METHOD_DESCRIBE..=fr_wire::services::fs::METHOD_WATCH)
        .contains(&method)
    {
        state.fs().serve(method, payload)
    } else if (crate::service::METHOD_READ_EXECUTE..=crate::service::METHOD_EDIT_PRESENTATION_META)
        .contains(&method)
    {
        let deps = crate::fs_tools::FsToolDeps {
            fs: state.fs(),
            gate: state.gate(),
            seams: state.tools().seams(),
            advertised: state.fs().sandbox_mode().is_some(),
        };
        crate::fs_tools::serve(&deps, method, payload)
    } else if (crate::service::METHOD_READ_IMAGE_EXECUTE
        ..=crate::service::METHOD_READ_IMAGE_PRESENTATION_META)
        .contains(&method)
    {
        let deps = crate::fs_read_image::ImageToolDeps {
            fs: state.fs(),
            gate: state.gate(),
            seams: state.tools().seams(),
        };
        crate::fs_read_image::serve(&deps, method, payload)
    } else if (crate::service::METHOD_GLOB_EXECUTE..=crate::service::METHOD_GREP_SPILL)
        .contains(&method)
    {
        let deps = crate::search_tools::SearchToolDeps {
            seams: state.tools().seams(),
            pending: &state.pending,
        };
        crate::search_tools::serve(&deps, method, payload)
    } else if (crate::service::METHOD_STR_REPLACE_EDITOR_EXECUTE
        ..=crate::service::METHOD_STR_REPLACE_EDITOR_PRESENT_CALL)
        .contains(&method)
    {
        let deps = crate::fs_tools::FsToolDeps {
            fs: state.fs(),
            gate: state.gate(),
            seams: state.tools().seams(),
            advertised: state.fs().sandbox_mode().is_some(),
        };
        crate::sre_tools::serve(&deps, method, payload)
    } else if method == fr_wire::services::skills::METHOD_PRE_STEP {
        // The `'skills'` service's own selector band (the single
        // pre-step method): the loop-side transform over the wire
        // session reads (the dsh `agent/pre-step` listeners' service
        // carrier; the per-artifact method-space judgment).
        let reads = WireSessionQuery { link: state.link };
        let deps = crate::skill_tools::SkillServiceDeps {
            runtime: state.tools().skills(),
            seams: state.tools().seams(),
            reads: &reads,
        };
        crate::skill_tools::serve(&deps, method, payload)
    } else {
        state.tools().serve(method, payload)
    };
    let reply = match reply {
        Ok(reply) => reply,
        Err(text) => {
            let body = json!({ "error": text }).to_string();
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

// The two listened events (emit dispatch): the settlement handler runs
// the notice machine's judge-and-enqueue fast path (no require, no
// blocking — the AGENT-LOOP spec invariant); the claimed handler
// resets the wake budget. Both write `{}` and answer OK.
unsafe extern "C" fn on_event(
    handle: *mut c_void,
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
    // SAFETY: live handle per the export-table contract.
    let state = unsafe { &*(handle.cast::<BuiltinState>()) };
    // Same empty-payload shape as `call` (null/0 pairs).
    let payload: &[u8] = if in_len == 0 || in_data.is_null() {
        &[]
    } else {
        // SAFETY: `in_len` is nonzero and `in_data` non-null here, valid
        // for `in_len` readable bytes per the host contract.
        unsafe { std::slice::from_raw_parts(in_data, in_len) }
    };
    let outcome = match event {
        EV_JOB_SETTLED => handle_job_settled(state, payload),
        EV_INBOX_CLAIMED => handle_inbox_claimed(state, payload),
        _ => Err(format!("unknown event id {event}")),
    };
    match outcome {
        Ok(()) => {
            // SAFETY: host buffer contract; an empty JSON object reply.
            if unsafe { write_out(b"{}", out, cap, out_len) } {
                STATUS_OK
            } else {
                STATUS_CAPACITY
            }
        }
        Err(message) => {
            let body = json!({ "error": message }).to_string();
            // SAFETY: host buffer contract.
            let _ = unsafe { write_out(body.as_bytes(), out, cap, out_len) };
            STATUS_FAILED
        }
    }
}

const unsafe extern "C" fn on_next(
    _handle: *mut c_void,
    _event: u32,
    _chain_status: i32,
    _data: *const u8,
    _len: usize,
    _out: *mut u8,
    _cap: usize,
    _out_len: *mut usize,
) -> i32 {
    STATUS_FAILED
}

unsafe extern "C" fn teardown(handle: *mut c_void) {
    if handle.is_null() {
        return;
    }
    // SAFETY: the handle was produced by `setup` and torn down exactly
    // once; the notice worker joins before the state drops (the sender
    // drops first, so the worker's recv unwinds and the join waits for
    // the in-flight delivery to contain).
    let mut state = unsafe { Box::from_raw(handle.cast::<BuiltinState>()) };
    drop(state.tasks.take());
    // Close the shared-arc sender copy too: the worker holds an arc of the
    // notice state, so only an explicit close lets its `recv` return before
    // this join (the 2026-11-04 in-process shutdown hang).
    state.tools().notices().close_tasks();
    if let Some(worker) = state.worker.take() {
        let _ignored = worker.join();
    }
    // Shut the schedule runtime BEFORE its join: the worker parks on the
    // signal condvar, which only an explicit shutdown wakes (the
    // 2026-11-04 shutdown hang).
    state.schedule_signal.shutdown();
    if let Some(worker) = state.schedule_worker.take() {
        let _ignored = worker.join();
    }
    // The persistent shells close before the vtable goes dark (the
    // dsh shell-cleanup effect; best-effort — a dead terminals band
    // cannot block the teardown).
    if let Some(terminals) = state.tools.seams().terminals() {
        state.tools.persistent_shells().dispose(terminals);
    }
    drop(state);
}

/// Handles one `job/settled` notification: the notice machine's
/// judge-and-enqueue fast path (no require, no blocking — the
/// AGENT-LOOP spec invariant). The dsh skip arms ride in order: a
/// model-requested kill (`killedByModel.delete`), a released wait
/// (`awaited`), a teardown settlement, and an unowned job.
fn handle_job_settled(state: &BuiltinState, payload: &[u8]) -> Result<(), String> {
    let event: Value = serde_json::from_slice(payload)
        .map_err(|error| format!("{EVENT_JOB_SETTLED} payload is invalid: {error}"))?;
    if event.get("type").and_then(Value::as_str) != Some("settled") {
        return Ok(());
    }
    let job = event.get("job").cloned().unwrap_or(Value::Null);
    let Some(id) = job.get("id").and_then(Value::as_str) else {
        return Ok(());
    };
    let delivered = state.tools().notices().consumes_model_kill(id)
        || event
            .get("awaited")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    if delivered
        || event.get("cause").and_then(Value::as_str) == Some("teardown")
        || job.get("owner").and_then(Value::as_str).is_none()
    {
        return Ok(());
    }
    let Some(session_id) = job.get("owner").and_then(Value::as_str) else {
        return Ok(());
    };
    state
        .tools()
        .notices()
        .enqueue(crate::job_tools::NoticeTask {
            session_id: session_id.to_owned(),
            message: crate::job_tools::notice_message(&job),
        });
    Ok(())
}

/// Handles one `agent/inbox/claimed` notification: the wake-budget
/// reset when the claimed message carries a user source (the dsh
/// claimed filter; a no-op while no budget is configured — the dsh
/// listener would not even register).
fn handle_inbox_claimed(state: &BuiltinState, payload: &[u8]) -> Result<(), String> {
    let event: Value = serde_json::from_slice(payload)
        .map_err(|error| format!("{EVENT_INBOX_CLAIMED} payload is invalid: {error}"))?;
    let Some(session_id) = event.get("sessionId").and_then(Value::as_str) else {
        return Ok(());
    };
    let is_user = matches!(
        event.pointer("/message/source/kind").cloned().unwrap_or(Value::Null),
        Value::String(kind) if kind == "user"
    );
    if is_user {
        state.tools().notices().reset_wake_budget(session_id);
    }
    Ok(())
}

/// Export table for the fr-tools-builtin dylib artifact.
#[unsafe(no_mangle)]
pub static DOUGONG_PLUGIN_EXPORTS: PluginExports = PluginExports {
    abi_version: ABI_VERSION,
    name: plugin_name,
    descriptor,
    setup,
    call,
    on_event,
    on_next,
    teardown,
};

/// The `session_search` registration row (dsh `ctx.tools.register`:
/// the resolved deadline rides the row; the exclusive default holds).
#[must_use]
pub fn session_search_registration() -> Value {
    json!({
        "name": "session_search",
        "description": crate::catalog_gen::SESSION_SEARCH_DESCRIPTION,
        "parameters": crate::schema::session_search_parameters(),
        "output": {
            "schema": { "type": "string" },
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_SESSION_SEARCH_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_SESSION_SEARCH_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_SESSION_SEARCH_PRESENT_CALL,
        },
        "timeoutMs": json!(crate::session_query_tools::BAKED_SEARCH_TIMEOUT_MS),
    })
}

/// The `session_event_search` registration row (the same shape; the
/// current-session step exclusion rides the body).
#[must_use]
pub fn session_event_search_registration() -> Value {
    json!({
        "name": "session_event_search",
        "description": crate::catalog_gen::SESSION_EVENT_SEARCH_DESCRIPTION,
        "parameters": crate::schema::session_event_search_parameters(),
        "output": {
            "schema": { "type": "string" },
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_SESSION_EVENT_SEARCH_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_SESSION_EVENT_SEARCH_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_SESSION_EVENT_SEARCH_PRESENT_CALL,
        },
        "timeoutMs": json!(crate::session_query_tools::BAKED_SEARCH_TIMEOUT_MS),
    })
}

/// The `session_trace` registration row (the concurrency-safe read).
#[must_use]
pub fn session_trace_registration() -> Value {
    json!({
        "name": "session_trace",
        "description": crate::catalog_gen::SESSION_TRACE_DESCRIPTION,
        "parameters": crate::schema::session_trace_parameters(),
        "output": {
            "schema": { "type": "string" },
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_SESSION_TRACE_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_SESSION_TRACE_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_SESSION_TRACE_PRESENT_CALL,
        },
        "isConcurrencySafe": true,
    })
}

/// The `session_event_trace` registration row (the concurrency-safe
/// read).
#[must_use]
pub fn session_event_trace_registration() -> Value {
    json!({
        "name": "session_event_trace",
        "description": crate::catalog_gen::SESSION_EVENT_TRACE_DESCRIPTION,
        "parameters": crate::schema::session_event_trace_parameters(),
        "output": {
            "schema": { "type": "string" },
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_SESSION_EVENT_TRACE_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_SESSION_EVENT_TRACE_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_SESSION_EVENT_TRACE_PRESENT_CALL,
        },
        "isConcurrencySafe": true,
    })
}

/// The `session_event_read` registration row (the concurrency-safe
/// read).
#[must_use]
pub fn session_event_read_registration() -> Value {
    json!({
        "name": "session_event_read",
        "description": crate::catalog_gen::SESSION_EVENT_READ_DESCRIPTION,
        "parameters": crate::schema::session_event_read_parameters(),
        "output": {
            "schema": { "type": "string" },
            "render": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_SESSION_EVENT_READ_RENDER,
            },
        },
        "execute": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_SESSION_EVENT_READ_EXECUTE,
        },
        "presentCall": {
            "service": crate::service::SERVICE_NAME,
            "method": crate::service::METHOD_SESSION_EVENT_READ_PRESENT_CALL,
        },
        "isConcurrencySafe": true,
    })
}

#[cfg(test)]
mod tests {
    /// The working-directory registration row keeps the partial face
    /// (the dsh `tool-working-directory` defineTool: no concurrency
    /// flag — the exclusive default, no custom present faces — the
    /// generic presentation family, no timeout): exactly the
    /// execute/render output pair plus the catalog identity.
    #[test]
    fn working_directory_registration_pins_the_partial_face() {
        let row = super::working_directory_registration();
        assert_eq!(row["name"], serde_json::json!("working_directory"));
        assert_eq!(
            row["description"],
            serde_json::json!(crate::catalog_gen::WORKING_DIRECTORY_DESCRIPTION)
        );
        assert_eq!(
            row["parameters"],
            crate::schema::working_directory_parameters()
        );
        assert_eq!(
            row["output"]["schema"],
            crate::schema::working_directory_output_schema()
        );
        assert_eq!(
            row["output"]["render"]["method"],
            serde_json::json!(crate::service::METHOD_WORKING_DIRECTORY_RENDER)
        );
        assert_eq!(
            row["execute"]["method"],
            serde_json::json!(crate::service::METHOD_WORKING_DIRECTORY_EXECUTE)
        );
        for absent in [
            "isConcurrencySafe",
            "presentCall",
            "presentResult",
            "presentationMeta",
            "timeoutMs",
        ] {
            assert!(row.get(absent).is_none(), "{absent} stays absent");
        }
    }

    /// The working-directory runtime context registers with the dsh
    /// name, the order-100 `WORKING_DIRECTORY` slot, the required
    /// flag, the non-interpolated flag, and the endpoint route (the
    /// PROMPT contract's context registration face).
    #[test]
    fn working_directory_context_pins_the_registration() {
        let spec = super::register_working_directory_context_spec();
        assert_eq!(spec["name"], serde_json::json!("working-directory:current"));
        assert_eq!(spec["order"], serde_json::json!(100.0));
        assert_eq!(spec["interpolate"], serde_json::json!(false));
        assert_eq!(spec["required"], serde_json::json!(true));
        assert_eq!(
            spec["text"]["textProvider"]["service"],
            serde_json::json!(crate::service::SERVICE_NAME)
        );
        assert_eq!(
            spec["text"]["textProvider"]["method"],
            serde_json::json!(crate::service::METHOD_WORKING_DIRECTORY_CONTEXT_TEXT)
        );
    }

    /// The inspect pair keeps its registration rows reachable with
    /// the baked gate off (the Q16f-B ruling): the rows exist for the
    /// composed web-app/desktop faces while the dsh base bundle mounts
    /// neither tool. The list row rides the generic read call card and
    /// the query row echoes its three-name identity.
    #[test]
    fn cordis_inspect_registrations_pin_the_rows_with_the_gate_off() {
        // The compile-time gate: the const pin rides the baked-off
        // ruling (the dsh base bundle mounts neither tool).
        const _: () = assert!(!crate::inspect_tools::BAKED_CORDIS_INSPECT);
        let list = super::cordis_inspect_list_registration();
        assert_eq!(list["name"], serde_json::json!("cordis_inspect_list"));
        assert_eq!(
            list["description"],
            serde_json::json!(crate::catalog_gen::CORDIS_INSPECT_LIST_DESCRIPTION)
        );
        assert_eq!(
            list["presentCall"]["method"],
            serde_json::json!(crate::service::METHOD_INSPECT_LIST_PRESENT_CALL)
        );
        assert_eq!(
            crate::inspect_tools::present_call_list(),
            serde_json::json!({
                "card": "generic",
                "kind": "read",
                "title": "List Cordis Inspect Providers",
            })
        );
        for absent in [
            "isConcurrencySafe",
            "presentResult",
            "presentationMeta",
            "timeoutMs",
        ] {
            assert!(list.get(absent).is_none(), "{absent} stays absent");
        }
        let query = super::cordis_inspect_query_registration();
        assert_eq!(query["name"], serde_json::json!("cordis_inspect_query"));
        assert_eq!(
            query["parameters"],
            crate::schema::cordis_inspect_query_parameters()
        );
        assert_eq!(
            query["presentCall"]["method"],
            serde_json::json!(crate::service::METHOD_INSPECT_QUERY_PRESENT_CALL)
        );
        assert_eq!(
            crate::inspect_tools::present_call_query(&serde_json::json!({
                "platform": "host",
                "provider": "tools",
                "method": "schema",
            }))["title"],
            serde_json::json!("Query Cordis host tools.schema")
        );
    }

    /// The lsp row keeps its registration reachable with the baked
    /// gate off (the base-bundle mirror): the row carries the catalog
    /// identity, the four-row table, the timeout budget, and the
    /// generic search call card; the section rides the same gate.
    #[test]
    fn lsp_registration_pins_the_row_with_the_gate_off() {
        const _: () = assert!(!crate::lsp_tools::BAKED_LSP);
        let row = super::lsp_registration();
        assert_eq!(row["name"], serde_json::json!("lsp"));
        assert_eq!(
            row["description"],
            serde_json::json!(crate::catalog_gen::LSP_DESCRIPTION)
        );
        assert_eq!(row["parameters"], crate::schema::lsp_parameters());
        assert_eq!(row["output"]["schema"], crate::schema::lsp_output_schema());
        assert_eq!(
            row["presentCall"]["method"],
            serde_json::json!(crate::service::METHOD_LSP_PRESENT_CALL)
        );
        assert_eq!(
            row["timeoutMs"],
            serde_json::json!(crate::lsp_tools::BAKED_LSP_TIMEOUT_MS)
        );
        for absent in ["isConcurrencySafe", "presentResult", "presentationMeta"] {
            assert!(row.get(absent).is_none(), "{absent} stays absent");
        }
        // The gated section arm: on with the gate, off in the baked
        // composition (twelve sections hold).
        let with_lsp = super::prompt_sections_with(super::SectionGates {
            terminals: false,
            web: super::WebSection::None,
            lsp: true,
        });
        let lsp_rows: Vec<_> = with_lsp
            .iter()
            .filter(|(name, _, _)| name == "tool:lsp")
            .collect();
        assert_eq!(lsp_rows.len(), 1);
        assert_eq!(lsp_rows[0].1, 2200.0);
        assert_eq!(lsp_rows[0].2, crate::catalog_gen::LSP_SECTION_TEXT);
        let without = super::prompt_sections_with(super::SectionGates {
            terminals: false,
            web: super::WebSection::None,
            lsp: false,
        });
        assert!(without.iter().all(|(name, _, _)| name != "tool:lsp"));
    }

    /// The plugin-management row keeps its registration reachable
    /// with the baked gate off (the Q16f-A ruling): the partial face
    /// carries the catalog identity, the string output schema, and
    /// the generic management card.
    #[test]
    fn plugin_manager_registration_pins_the_row() {
        const _: () = assert!(!crate::plugin_manager_tools::BAKED_PLUGIN_MANAGER);
        let row = super::plugin_manager_registration();
        assert_eq!(row["name"], serde_json::json!("plugin_manager"));
        assert_eq!(
            row["description"],
            serde_json::json!(crate::catalog_gen::PLUGIN_MANAGER_DESCRIPTION)
        );
        assert_eq!(
            row["parameters"],
            crate::schema::plugin_manager_parameters()
        );
        assert_eq!(
            row["output"]["schema"],
            serde_json::json!({ "type": "string" })
        );
        assert!(row.get("presentResult").is_none());
        assert!(row.get("presentationMeta").is_none());
        assert!(row.get("isConcurrencySafe").is_none());
        assert!(row.get("timeoutMs").is_none());
        assert_eq!(
            row["presentCall"]["method"],
            serde_json::json!(crate::service::METHOD_PLUGIN_MANAGER_PRESENT_CALL)
        );
        assert_eq!(
            crate::plugin_manager_tools::present_call(&serde_json::json!({
                "action": "list_plugins",
            })),
            serde_json::json!({
                "card": "generic",
                "title": "Manage profile plugins",
                "kind": "read",
                "rawInput": { "action": "list_plugins" },
            })
        );
    }

    /// The workspace-dependencies row keeps its registration
    /// reachable with the baked gate off (the Q16f-E ruling): the
    /// row carries the catalog identity and the generic read card,
    /// and the tool config gate refuses relative paths.
    #[test]
    fn load_workspace_dependencies_registration_pins_the_row() {
        const _: () = assert!(!crate::wsd_tools::BAKED_WORKSPACE_DEPENDENCIES);
        let row = super::load_workspace_dependencies_registration();
        assert_eq!(
            row["name"],
            serde_json::json!("load_workspace_dependencies")
        );
        assert_eq!(
            row["description"],
            serde_json::json!(crate::catalog_gen::LOAD_WORKSPACE_DEPENDENCIES_DESCRIPTION)
        );
        assert_eq!(
            row["parameters"],
            crate::schema::load_workspace_dependencies_parameters()
        );
        assert_eq!(
            row["presentCall"]["method"],
            serde_json::json!(crate::service::METHOD_WSD_PRESENT_CALL)
        );
        assert_eq!(
            crate::wsd_tools::WorkspaceDependenciesTool::present_call(),
            serde_json::json!({
                "card": "generic",
                "title": "Load workspace dependencies",
                "kind": "read",
            })
        );
        assert_eq!(
            crate::wsd_tools::WorkspaceDependenciesTool::new("relative", None).unwrap_err(),
            "workspace dependencies: source and root must be absolute paths"
        );
        for absent in [
            "isConcurrencySafe",
            "presentResult",
            "presentationMeta",
            "timeoutMs",
        ] {
            assert!(row.get(absent).is_none(), "{absent} stays absent");
        }
    }

    /// The two web registration rows carry the full faces (the dsh
    /// `tool-web` registration: parallel scheduling, the catalog
    /// descriptions, the service-route faces, and the baked budgets).
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the pin mirrors the two registrations face by face; splitting it would hide the face correspondence"
    )]
    fn web_registrations_pin_the_full_faces() {
        let search = super::web_search_registration();
        assert_eq!(search["name"], serde_json::json!("web_search"));
        assert_eq!(
            search["description"],
            serde_json::json!(crate::catalog_gen::WEB_SEARCH_DESCRIPTION)
        );
        assert_eq!(search["parameters"], crate::schema::web_search_parameters());
        assert_eq!(
            search["output"]["schema"],
            crate::schema::web_search_output_schema()
        );
        assert_eq!(
            search["output"]["render"]["method"],
            serde_json::json!(crate::service::METHOD_WEB_SEARCH_RENDER)
        );
        assert_eq!(
            search["output"]["presentationMeta"]["method"],
            serde_json::json!(crate::service::METHOD_WEB_SEARCH_PRESENTATION_META)
        );
        assert_eq!(
            search["execute"]["method"],
            serde_json::json!(crate::service::METHOD_WEB_SEARCH_EXECUTE)
        );
        assert_eq!(
            search["presentCall"]["method"],
            serde_json::json!(crate::service::METHOD_WEB_SEARCH_PRESENT_CALL)
        );
        assert_eq!(
            search["presentResult"]["method"],
            serde_json::json!(crate::service::METHOD_WEB_SEARCH_PRESENT_RESULT)
        );
        assert_eq!(search["isConcurrencySafe"], serde_json::json!(true));
        assert_eq!(
            search["timeoutMs"],
            serde_json::json!(crate::web_tools::BAKED_SEARCH_TIMEOUT_MS)
        );
        // The presentation family (the pending call's search card and
        // the completed call's web card).
        assert_eq!(
            crate::web_tools::present_search_call(&serde_json::json!({
                "queries": ["a"],
            }))["kind"],
            serde_json::json!("search")
        );
        assert_eq!(
            crate::web_tools::present_search_result(
                &serde_json::json!({ "queries": ["a"] }),
                &serde_json::json!({
                    "isError": false,
                    "meta": { "sources": [], "truncated": false },
                }),
            )
            .expect("the search card")["card"],
            serde_json::json!("web")
        );
        let fetch = super::web_fetch_registration();
        assert_eq!(fetch["name"], serde_json::json!("web_fetch"));
        assert_eq!(
            fetch["description"],
            serde_json::json!(crate::catalog_gen::WEB_FETCH_DESCRIPTION)
        );
        assert_eq!(fetch["parameters"], crate::schema::web_fetch_parameters());
        assert_eq!(
            fetch["output"]["schema"],
            crate::schema::web_fetch_output_schema()
        );
        assert_eq!(
            fetch["output"]["render"]["method"],
            serde_json::json!(crate::service::METHOD_WEB_FETCH_RENDER)
        );
        assert_eq!(
            fetch["output"]["presentationMeta"]["method"],
            serde_json::json!(crate::service::METHOD_WEB_FETCH_PRESENTATION_META)
        );
        assert_eq!(
            fetch["execute"]["method"],
            serde_json::json!(crate::service::METHOD_WEB_FETCH_EXECUTE)
        );
        assert_eq!(
            fetch["presentCall"]["method"],
            serde_json::json!(crate::service::METHOD_WEB_FETCH_PRESENT_CALL)
        );
        assert_eq!(
            fetch["presentResult"]["method"],
            serde_json::json!(crate::service::METHOD_WEB_FETCH_PRESENT_RESULT)
        );
        assert_eq!(fetch["isConcurrencySafe"], serde_json::json!(true));
        assert_eq!(
            fetch["timeoutMs"],
            serde_json::json!(crate::web_tools::BAKED_FETCH_TIMEOUT_MS)
        );
        assert_eq!(
            crate::web_tools::present_fetch_call(&serde_json::json!({ "url": "https://a.test" }))["kind"],
            serde_json::json!("fetch")
        );
        assert_eq!(
            crate::web_tools::present_fetch_result(
                &serde_json::json!({ "url": "https://a.test" }),
                &serde_json::json!({
                    "isError": false,
                    "meta": { "url": "https://a.test", "statusCode": 200, "truncated": false },
                }),
            )
            .expect("the fetch card")["card"],
            serde_json::json!("web")
        );
    }

    /// The twelve tool sections ride the dsh orders and their verbatim
    /// texts: the bash order opens the 1000-3000 tool band, the pwsh
    /// twin sits ten above it, the fs trio rides the dsh
    /// read/write/edit orders, the discovery pair rides the glob/grep
    /// orders, the jobs guidance rides the `TOOL_JOBS` order (one
    /// section serving the three controls), the goal guidance rides
    /// the `TOOL_GOAL` order with its threshold-composed text, and the
    /// session-query guidance rides the `TOOL_SESSION_QUERY` order (one
    /// section serving the five tools; the builtin-tools contract's
    /// section clause; the write and grep texts carry the composed
    /// conditional tails), and the web pair rides the `TOOL_WEB_SEARCH`/
    /// `TOOL_WEB_FETCH` orders with the fetch-variant search text. The
    /// count pins the absences too: the image
    /// tool, the opt-in editor, and the whole-list snapshot tool
    /// register no section (the dsh sources inject no systemPrompt; the
    /// Q16b-4A, Q16b-5A, and Q16c batch-2 rulings).
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the pin mirrors the twelve dsh sections one anchor per section; splitting it would hide the band correspondence"
    )]
    fn prompt_sections_pin_the_twelve_tools() {
        let sections = super::prompt_sections();
        assert_eq!(sections.len(), 12);
        assert!(
            sections.iter().all(|(name, _, _)| !name.contains("todo")
                && !name.contains("image")
                && !name.contains("schedule")
                && !name.contains("ask-user")
                && !name.contains("exit-plan")
                && !name.contains("skill")),
            "the section-free tools stay absent (the dsh truth; the Q16b-4A, Q16b-5A, Q16c batch-2, Q16c batch-0 schedule, Q16d-4A ask/exit, and Q16f batch-2 skill rulings)"
        );
        let jobs = sections
            .iter()
            .find(|(name, _, _)| name == "tool:jobs")
            .expect("the jobs section registers");
        assert_eq!(jobs.1, 1600.0);
        assert!(
            jobs.2
                .starts_with("Track every background job id you start."),
            "{}",
            jobs.2
        );
        assert!(
            jobs.2.contains("do not busy-poll or sleep on one"),
            "{}",
            jobs.2
        );
        assert!(
            jobs.2
                .ends_with("and job_kill jobs that stopped mattering."),
            "{}",
            jobs.2
        );
        let goal = sections
            .iter()
            .find(|(name, _, _)| name == "tool:goal")
            .expect("the goal section registers");
        assert_eq!(goal.1, 2400.0);
        assert!(
            goal.2
                .starts_with("create_goal may infer goal intent from a direct human request"),
            "{}",
            goal.2
        );
        assert!(
            goal.2.contains("at least 3 consecutive goal rounds"),
            "{}",
            goal.2
        );
        assert!(
            goal.2.ends_with("or useful remaining work is not blocked."),
            "{}",
            goal.2
        );
        // The threshold interpolation rides the extracted template: a
        // synthetic threshold swaps exactly the number.
        let tuned = crate::schema::goal_section_text(7);
        assert!(
            tuned.contains("at least 7 consecutive goal rounds")
                && !tuned.contains("at least 3 consecutive goal rounds"),
            "{tuned}"
        );
        let session_query = sections
            .iter()
            .find(|(name, _, _)| name == "tool:session-query")
            .expect("the session-query section registers");
        assert_eq!(session_query.1, 2300.0);
        assert!(
            session_query
                .2
                .starts_with("Use session_search to find relevant work from prior sessions,"),
            "{}",
            session_query.2
        );
        assert!(
            session_query
                .2
                .contains("Search results are cursor-free and workspace-scoped."),
            "{}",
            session_query.2
        );
        assert!(
            session_query
                .2
                .ends_with("when you need lineage, relationships, or exact data."),
            "{}",
            session_query.2
        );
        assert_eq!(sections[0].0, "tool:bash");
        assert_eq!(sections[0].1, 1000.0);
        assert!(
            sections[0].2.starts_with("Check the [exit code: N] marker"),
            "{}",
            sections[0].2
        );
        assert_eq!(sections[1].0, "tool:pwsh");
        assert_eq!(sections[1].1, 1010.0);
        assert!(
            sections[1]
                .2
                .starts_with("Non-zero exits are reported as `[exit code: N]` markers"),
            "{}",
            sections[1].2
        );
        assert_eq!(sections[2].0, "tool:read");
        assert_eq!(sections[2].1, 1100.0);
        assert!(
            sections[2]
                .2
                .starts_with("Use the read tool — not shell commands like cat"),
            "{}",
            sections[2].2
        );
        assert_eq!(sections[3].0, "tool:write");
        assert_eq!(sections[3].1, 1200.0);
        assert!(
            sections[3]
                .2
                .ends_with("and prefer edit for targeted changes."),
            "{}",
            sections[3].2
        );
        assert_eq!(sections[4].0, "tool:edit");
        assert_eq!(sections[4].1, 1300.0);
        assert!(
            sections[4].2.starts_with("Read a file before editing it"),
            "{}",
            sections[4].2
        );
        assert_eq!(sections[5].0, "tool:glob");
        assert_eq!(sections[5].1, 1400.0);
        assert!(
            sections[5]
                .2
                .starts_with("Use the glob tool — not shell find"),
            "{}",
            sections[5].2
        );
        assert_eq!(sections[6].0, "tool:grep");
        assert_eq!(sections[6].1, 1500.0);
        assert!(
            sections[6]
                .2
                .ends_with("Use read on a matched file when you need surrounding context."),
            "{}",
            sections[6].2
        );
        for (name, order, _) in sections {
            assert!(name.starts_with("tool:"));
            assert!((1000.0..=3000.0).contains(&order));
        }
    }

    /// The two web sections ride their dsh orders with the variant
    /// texts: the search text recommends `web_fetch` while the fetch
    /// tool shares the composition and falls back to the snippets
    /// variant otherwise; the fetch text is the one verbatim guidance.
    #[test]
    fn web_sections_carry_the_variant_texts() {
        let both = super::prompt_sections_with(super::SectionGates {
            terminals: true,
            web: super::WebSection::Both,
            lsp: false,
        });
        let search = both
            .iter()
            .find(|(name, _, _)| name == "tool:web_search")
            .expect("the search section registers");
        assert_eq!(search.1, 2000.0);
        assert_eq!(search.2, crate::catalog_gen::WEB_SEARCH_SECTION_TEXTS[0]);
        let fetch = both
            .iter()
            .find(|(name, _, _)| name == "tool:web_fetch")
            .expect("the fetch section registers");
        assert_eq!(fetch.1, 2100.0);
        assert_eq!(fetch.2, crate::catalog_gen::WEB_FETCH_SECTION_TEXT);
        let search_only = super::prompt_sections_with(super::SectionGates {
            terminals: true,
            web: super::WebSection::SearchOnly,
            lsp: false,
        });
        let fallback = search_only
            .iter()
            .find(|(name, _, _)| name == "tool:web_search")
            .expect("the search section registers without fetch");
        assert_eq!(fallback.2, crate::catalog_gen::WEB_SEARCH_SECTION_TEXTS[1]);
        assert!(
            search_only
                .iter()
                .all(|(name, _, _)| name != "tool:web_fetch"),
            "the fetch section stays absent while disabled"
        );
    }

    /// The `tool:pty` section joins only when the terminal family is
    /// enabled: the baked default stays at the web-pair rows (the
    /// opt-in judgment) while the enabled face pins the dsh `TOOL_PTY`
    /// order 1700 and the extracted text anchors.
    #[test]
    fn pty_section_registers_only_when_enabled() {
        assert!(
            super::prompt_sections()
                .iter()
                .all(|(name, _, _)| name != "tool:pty"),
            "the baked default carries no pty section"
        );
        let enabled = super::prompt_sections_with(super::SectionGates {
            terminals: true,
            web: super::WebSection::Both,
            lsp: false,
        });
        assert_eq!(enabled.len(), 13);
        let pty = enabled
            .iter()
            .find(|(name, _, _)| name == "tool:pty")
            .expect("the pty section registers when the family is enabled");
        assert_eq!(pty.1, 1700.0);
        assert!(
            pty.2.starts_with(
                "Use a terminal session only when work needs persistent terminal state"
            ),
            "{}",
            pty.2
        );
        assert!(
            pty.2.ends_with(
                "An inferred_idle or timeout result does not prove the foreground command exited."
            ),
            "{}",
            pty.2
        );
    }

    /// The descriptor declares the two completion-notice events with
    /// their dispatch ids (the fr-agent-loop listen-map judgment), and
    /// the event ids stay paired with the on-event dispatch constants.
    #[test]
    fn descriptor_declares_the_notice_events() {
        let parsed: serde_json::Value =
            serde_json::from_str(super::DESCRIPTOR).expect("descriptor json");
        assert_eq!(
            parsed["listen"],
            serde_json::json!({ "job/settled": 1, "agent/inbox/claimed": 2 })
        );
        assert_eq!(super::EV_JOB_SETTLED, 1);
        assert_eq!(super::EV_INBOX_CLAIMED, 2);
        assert_eq!(super::EVENT_JOB_SETTLED, "job/settled");
        assert_eq!(super::EVENT_INBOX_CLAIMED, "agent/inbox/claimed");
        // The provided-service face: the skill pre-step seam is the
        // sixth provide row (the 16f batch-2 service band).
        assert_eq!(
            parsed["provide"],
            serde_json::json!([
                "builtin-tools",
                "fs",
                "user-questions",
                "plan-mode",
                "session-query",
                "skills"
            ])
        );
    }

    /// The `skill` registration row pins the catalog description and
    /// the three fr endpoints (the Q97-A full-shape judgment).
    #[test]
    fn skill_registration_pins_the_row() {
        let row = super::skill_registration();
        assert_eq!(row["name"], serde_json::json!("skill"));
        assert_eq!(
            row["description"],
            serde_json::json!(crate::catalog_gen::SKILL_DESCRIPTION)
        );
        assert_eq!(row["parameters"], crate::schema::skill_parameters());
        assert_eq!(
            row["output"]["schema"],
            crate::schema::skill_output_schema()
        );
        assert_eq!(
            row["output"]["render"],
            serde_json::json!({
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_SKILL_RENDER,
            })
        );
        assert_eq!(
            row["execute"],
            serde_json::json!({
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_SKILL_EXECUTE,
            })
        );
        assert_eq!(
            row["presentCall"],
            serde_json::json!({
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_SKILL_PRESENT_CALL,
            })
        );
    }
    /// The `skill` row carries no concurrency flag (the fr-tools
    /// fail-closed exclusive default applies, the Q94-A tool-pipeline
    /// carrier) and the presentation is the read family (dsh
    /// `presentCall`).
    #[test]
    fn skill_row_defaults_to_exclusive_with_read_presentation() {
        let row = super::skill_registration();
        assert!(
            row.get("isConcurrencySafe").is_none(),
            "no concurrency flag: the fail-closed exclusive applies"
        );
        let view = crate::skill_tools::present_call(&serde_json::json!({ "name": "pdf" }));
        assert_eq!(view["card"], serde_json::json!("generic"));
        assert_eq!(view["kind"], serde_json::json!("read"));
    }
}
