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

/// The static descriptor: one provided service, eight injected seams
/// (the search tools' subprocess face rides the injection), and the two
/// completion-notice events — the spill listeners register at activation
/// instead (the descriptor `listen` map stays a dylib event face, not a
/// tools-pipeline registration).
const DESCRIPTOR: &str = "{\"name\":\"fr-tools-builtin\",\"provide\":[\"builtin-tools\",\"fs\"],\"inject\":[\"tools\",\"shell\",\"jobs\",\"sandbox-policy\",\"approval\",\"session-persistence\",\"system-prompt\",\"subprocess\",\"attachments\",\"llm\",\"session-projections\",\"storage-domain\"],\"listen\":{\"job/settled\":1,\"agent/inbox/claimed\":2}}";

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
}
/// One activation-sequence step: one require, `None` on any refusal
/// (the fail-closed decline).
fn step(link: &HostLink, service: &str, method: u32, payload: &[u8]) -> Option<()> {
    link.require(service, method, payload).ok().map(|_| ())
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
    /// The spill listeners' CPS pairing stash (the enter leg parks the
    /// call and result; the exit leg consumes them).
    pending: std::sync::Mutex<std::collections::HashMap<u64, (Value, Value)>>,
    /// The completion-notice machine's queue sender; dropping it ends
    /// the worker at teardown.
    tasks: Option<std::sync::mpsc::Sender<crate::job_tools::NoticeTask>>,
    /// The notice worker's join handle (joined before the state drops).
    worker: Option<std::thread::JoinHandle<()>>,
    /// The schedule runtime's join handle (joined before the state
    /// drops).
    schedule_worker: Option<std::thread::JoinHandle<()>>,
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
    vec![
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
    ]
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
    register_spill_listeners(link);
    register_finalize_listeners(link);
    Some(())
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
    #[must_use]
    pub fn try_new(link: Option<HostLink>) -> Option<Self> {
        let link = link?;

        declare_change_event(&link)?;
        // The D31 grants: the tools fabric consumes the execute/render
        // endpoints; the projection relay consumes the fold endpoint.
        link.grant("tools", "builtin-tools").ok()?;
        link.grant("session-projections", "builtin-tools").ok()?;
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
        register_projection(&link)?;
        register_tools(&link, !escalation.is_empty(), attachments_mounted)?;

        // The managed-environment registry over the resolved home.
        let env = ShellEnvRegistry::new(None).ok()?;

        // The working-directory domain over the wire log seam and the
        // in-crate fsio probe.
        let directories = WorkingDirectories::new(
            Arc::new(WireSessionLog { link }),
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
        schedules.bind_signal(Arc::clone(&signal));
        let schedule_worker = crate::schedule::spawn_runtime(Arc::clone(&schedules), signal);

        let env_arc = Arc::new(env);
        let directories_arc = Arc::new(directories);
        let seams = WireSeams {
            link,
            directories: Arc::clone(&directories_arc),
            env: Arc::clone(&env_arc),
        };
        let tools = BuiltinTools::new(
            Box::new(seams),
            env_arc,
            directories_arc,
            Arc::new(lists),
            Arc::new(goals),
            notices,
            Arc::clone(&schedules),
        )
        .ok()?;

        // The fs domain over the standing-policy wire seam (the
        // composed single-arm backend; the Q16b-3B seam).
        let config = crate::fs_domain::FsDomainConfig::new().ok()?;
        let policy = WireStandingPolicy { link };
        let fs = Arc::new(crate::fs_domain::FsDomain::new(
            config,
            Box::new(policy),
            escalation.first().map(|mode| (*mode).to_owned()),
        ));
        Some(Self {
            tools,
            fs,
            gate: crate::observation::ObservedStateGate::new(),
            pending: std::sync::Mutex::new(std::collections::HashMap::new()),
            tasks: Some(tasks),
            worker: Some(worker),
            schedule_worker: Some(schedule_worker),
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
    if let Some(worker) = state.worker.take() {
        let _ignored = worker.join();
    }
    if let Some(worker) = state.schedule_worker.take() {
        let _ignored = worker.join();
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

#[cfg(test)]
mod tests {
    /// The nine tool sections ride the dsh orders and their verbatim
    /// texts: the bash order opens the 1000-3000 tool band, the pwsh
    /// twin sits ten above it, the fs trio rides the dsh
    /// read/write/edit orders, the discovery pair rides the glob/grep
    /// orders, the jobs guidance rides the `TOOL_JOBS` order (one
    /// section serving the three controls), and the goal guidance rides
    /// the `TOOL_GOAL` order with its threshold-composed text (the
    /// builtin-tools contract's section clause; the write and grep texts
    /// carry the composed conditional tails). The count pins the
    /// absences too: the image tool, the opt-in editor, and the
    /// whole-list snapshot tool register no section (the dsh sources
    /// inject no systemPrompt; the Q16b-4A, Q16b-5A, and Q16c batch-2
    /// rulings).
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the pin mirrors the nine dsh sections one anchor per section; splitting it would hide the band correspondence"
    )]
    fn prompt_sections_pin_the_nine_tools() {
        let sections = super::prompt_sections();
        assert_eq!(sections.len(), 9);
        assert!(
            sections.iter().all(|(name, _, _)| !name.contains("todo")
                && !name.contains("image")
                && !name.contains("schedule")),
            "the section-free tools stay absent (the dsh truth; the Q16b-4A, Q16b-5A, Q16c batch-2, and Q16c batch-0 schedule rulings)"
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
    }
}
