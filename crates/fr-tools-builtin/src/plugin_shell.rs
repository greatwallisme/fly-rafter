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

/// The static descriptor: one provided service, seven injected seams,
/// no listeners.
const DESCRIPTOR: &str = "{\"name\":\"fr-tools-builtin\",\"provide\":[\"builtin-tools\"],\"inject\":[\"tools\",\"shell\",\"jobs\",\"sandbox-policy\",\"approval\",\"session-persistence\",\"system-prompt\"],\"listen\":{}}";

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
}

impl std::fmt::Debug for BuiltinState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuiltinState").finish_non_exhaustive()
    }
}

/// Declares the durable change vocabulary (a duplicate fails closed at
/// the session service).
fn declare_change_event(link: &HostLink) -> Option<()> {
    step(
        link,
        fr_wire::services::session_persistence::SERVICE_NAME,
        fr_wire::services::session_persistence::METHOD_DECLARE_EVENT_TYPES,
        serde_json::to_vec(&json!({ "types": [crate::working_directory::CHANGE_EVENT] }))
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

/// The two prompt sections (dsh `ctx.systemPrompt.section`): the names,
/// the dsh orders, and the script-extracted verbatim texts (the catalog
/// generator; the PRD 22 rule — the texts live in the dsh tool sources,
/// not the catalog).
const fn prompt_sections() -> [(&'static str, f64, &'static str); 2] {
    [
        ("tool:bash", 1000.0, crate::catalog_gen::BASH_SECTION_TEXT),
        ("tool:pwsh", 1010.0, crate::catalog_gen::PWSH_SECTION_TEXT),
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
/// read face; the fold endpoint is the 900 selector).
fn register_projection(link: &HostLink) -> Option<()> {
    step(
        link,
        fr_wire::services::session_projections::SERVICE_NAME,
        fr_wire::services::session_projections::METHOD_REGISTER,
        serde_json::to_vec(&json!({
            "key": crate::working_directory::PROJECTION_KEY,
            "stateVersion": crate::working_directory::STATE_VERSION,
            "fold": {
                "service": crate::service::SERVICE_NAME,
                "method": crate::service::METHOD_WORKING_DIRECTORY_FOLD,
            },
        }))
        .ok()?
        .as_slice(),
    )
}

/// Registers the two tools (the fr-tools register face; the endpoints
/// point back at this artifact's own service).
fn register_tools(link: &HostLink, advertise_escalation: bool) -> Option<()> {
    for (name, description, parameters, execute, render_m, present_call, present_result) in [
        (
            "bash",
            crate::schema::bash_description(),
            crate::schema::bash_parameters(true, advertise_escalation, true),
            crate::service::METHOD_BASH_EXECUTE,
            crate::service::METHOD_BASH_RENDER,
            crate::service::METHOD_BASH_PRESENT_CALL,
            crate::service::METHOD_BASH_PRESENT_RESULT,
        ),
        (
            "pwsh",
            crate::schema::pwsh_description(advertise_escalation),
            crate::schema::pwsh_parameters(true, advertise_escalation, true),
            crate::service::METHOD_PWSH_EXECUTE,
            crate::service::METHOD_PWSH_RENDER,
            crate::service::METHOD_PWSH_PRESENT_CALL,
            crate::service::METHOD_PWSH_PRESENT_RESULT,
        ),
    ] {
        step(
            link,
            fr_wire::services::tools::SERVICE_NAME,
            fr_wire::services::tools::METHOD_REGISTER,
            serde_json::to_vec(&json!({
                "name": name,
                "description": description,
                "parameters": parameters,
                "output": {
                    "schema": crate::schema::shell_output_schema(),
                    "render": {
                        "service": crate::service::SERVICE_NAME,
                        "method": render_m,
                    },
                    "presentationMeta": {
                        "service": crate::service::SERVICE_NAME,
                        "method": crate::service::METHOD_PRESENTATION_META,
                    },
                },
                "execute": { "service": crate::service::SERVICE_NAME, "method": execute },
                "presentCall": { "service": crate::service::SERVICE_NAME, "method": present_call },
                "presentResult": { "service": crate::service::SERVICE_NAME, "method": present_result },
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
        register_sections(&link)?;
        register_projection(&link)?;
        register_tools(&link, !escalation.is_empty())?;

        // The managed-environment registry over the resolved home.
        let env = ShellEnvRegistry::new(None).ok()?;

        // The working-directory domain over the wire log seam and the
        // std filesystem seam.
        let directories = WorkingDirectories::new(
            Arc::new(WireSessionLog { link }),
            Arc::new(crate::working_directory::StdFsStat),
            std::env::current_dir().ok()?.to_string_lossy().to_string(),
        );

        let env_arc = Arc::new(env);
        let directories_arc = Arc::new(directories);
        let seams = WireSeams {
            link,
            directories: Arc::clone(&directories_arc),
            env: Arc::clone(&env_arc),
        };
        let tools = BuiltinTools::new(Box::new(seams), env_arc, directories_arc).ok()?;
        Some(Self { tools })
    }

    /// The hosted runtime (tests drive the wire face).
    #[must_use]
    pub const fn tools(&self) -> &BuiltinTools {
        &self.tools
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
    let reply = match state.tools().serve(method, payload) {
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
    /// The two tool sections ride the dsh orders and their verbatim
    /// texts: the bash order opens the 1000-3000 tool band, the pwsh
    /// twin sits ten above it (the builtin-tools contract's section
    /// clause).
    #[test]
    fn prompt_sections_pin_the_two_tools() {
        let sections = super::prompt_sections();
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
        for (name, order, _) in sections {
            assert!(name.starts_with("tool:"));
            assert!((1000.0..=3000.0).contains(&order));
        }
    }
}
