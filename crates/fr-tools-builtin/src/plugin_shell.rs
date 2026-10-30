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

/// The static descriptor: one provided service, eight injected seams
/// (the search tools' subprocess face rides the injection), no
/// listeners — the spill listeners register at activation instead (the
/// descriptor `listen` map stays a dylib event face, not a tools-pipeline
/// registration).
const DESCRIPTOR: &str = "{\"name\":\"fr-tools-builtin\",\"provide\":[\"builtin-tools\",\"fs\"],\"inject\":[\"tools\",\"shell\",\"jobs\",\"sandbox-policy\",\"approval\",\"session-persistence\",\"system-prompt\",\"subprocess\",\"attachments\",\"llm\",\"session-projections\"],\"listen\":{}}";

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

/// The seven prompt sections (dsh `ctx.systemPrompt.section`): the names,
/// the dsh orders, and the script-extracted verbatim texts (the catalog
/// generator; the PRD 22 rule — the texts live in the dsh tool sources,
/// not the catalog). The write text composes with the source's
/// conditional (the edit tail rides while edit registers, which the M1
/// composition always does); the grep text composes the same way (the
/// read tail). The `str_replace_editor` registers no section (the dsh
/// source injects no systemPrompt; the Q16b-4A ruling).
fn prompt_sections() -> Vec<(String, f64, String)> {
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
/// read face; the fold endpoint is the 900 selector) and the `todos`
/// projection (stateVersion 2; the fold endpoint is the 901 selector).
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
    register_spill_listeners(link);
    Some(())
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

        let env_arc = Arc::new(env);
        let directories_arc = Arc::new(directories);
        let seams = WireSeams {
            link,
            directories: Arc::clone(&directories_arc),
            env: Arc::clone(&env_arc),
        };
        let tools =
            BuiltinTools::new(Box::new(seams), env_arc, directories_arc, Arc::new(lists)).ok()?;

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

const unsafe extern "C" fn on_event(
    _handle: *mut c_void,
    _event: u32,
    _mode: u32,
    _in_data: *const u8,
    _in_len: usize,
    _next_available: u32,
    _out: *mut u8,
    _cap: usize,
    _out_len: *mut usize,
) -> i32 {
    // No listeners are declared; the host never routes events here.
    STATUS_FAILED
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
    // once; the runtime owns no plugin threads (every wait rides the
    // bounded foreign faces), so the drop is the whole unwinding.
    let _ = unsafe { Box::from_raw(handle.cast::<BuiltinState>()) };
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
    /// The seven tool sections ride the dsh orders and their verbatim
    /// texts: the bash order opens the 1000-3000 tool band, the pwsh
    /// twin sits ten above it, the fs trio rides the dsh
    /// read/write/edit orders, and the discovery pair rides the
    /// glob/grep orders (the builtin-tools contract's section clause;
    /// the write and grep texts carry the composed conditional tails).
    /// The count pins the absences too: the image tool and the opt-in
    /// editor inject no systemPrompt (the dsh sources; the Q16b-4A and
    /// Q16b-5A rulings), and the whole-list snapshot tool registers no
    /// section either (the dsh tool source injects no systemPrompt; the
    /// Q16c batch-2 negative pin).
    #[test]
    fn prompt_sections_pin_the_seven_tools() {
        let sections = super::prompt_sections();
        assert_eq!(sections.len(), 7);
        assert!(
            sections.iter().all(|(name, _, _)| !name.contains("todo")),
            "the tool registers no prompt section (the dsh truth)"
        );
        let sections = super::prompt_sections();
        assert_eq!(sections.len(), 7);
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
}
