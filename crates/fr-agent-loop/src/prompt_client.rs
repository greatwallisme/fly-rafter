// Rust guideline compliant 2026-10-28
//! The consumer-side `system-prompt`-service wrappers (Q88-A): the
//! loop-side [`SystemPromptFace`] and [`InstructionsBridge`] over the
//! dougong dynamic channel.
//!
//! Method numbers, payload shapes, and the render vocabulary are the
//! provider contract in `crates/fr-system-prompt` (the `llm_client`
//! judgment: wire DTOs stay single-sourced in the provider crate, the
//! consumer converts). The instructions bridge lowers the composed wire
//! message to a user message and reuses a pending payload-twin's identity
//! so a short-circuited recompose keeps the id dsh preserves
//! (agent-instructions/src/index.ts:131 returns `pending[0]` verbatim).
//!
//! The assembly round trip caches the wire assembly: the driver's
//! [`crate::driver::PromptAssembly`] is the reduced render product and
//! cannot carry the sections and variables `refresh_context` needs, so
//! the client holds the last assembled wire document — dsh's driver keeps
//! the full assembly object the same way (agent-instructions/src/:341
//! compose callers pass what `assemble` returned).

use fr_system_prompt::instructions::state::VisibleMessage;
use fr_system_prompt::interpolate::{
    ContextSnapshotSection as WireContextSection, PromptAssembly as WireAssembly,
    join_context_sections, render_context_sections, render_prompt,
};
use fr_system_prompt::registry::WireAssembleContext;
use fr_system_prompt::service::{
    ComposeReply, ComposeRequest, InstructionMessage, METHOD_ASSEMBLE, METHOD_COMPOSE_INSTRUCTIONS,
    METHOD_REFRESH_CONTEXT, RefreshRequest, SERVICE_NAME,
};

use crate::driver::{InstructionsBridge, PromptAssembly, SystemPromptFace};
use crate::outbound::{Outbound, RequireError};
use fr_wire::ids::MessageId;
use fr_wire::message::Message;

/// One failed service call: the host rejection or the reply that missed
/// the method contract, as the face's text error.
fn call_error(context: &str, error: impl std::fmt::Display) -> String {
    format!("system-prompt service call failed: {context}: {error}")
}

/// Reduces one wire assembly to the driver's render product (dsh the
/// agent's `renderPrompt`/`renderContextSections` calls).
fn reduce(wire: &WireAssembly) -> Result<PromptAssembly, String> {
    let rendered_prompt = render_prompt(wire).map_err(|error| error.0)?;
    let sections = render_context_sections(wire).map_err(|error| error.0)?;
    let joined_sections = join_context_sections(&sections);
    Ok(PromptAssembly {
        rendered_prompt,
        context_sections: sections
            .into_iter()
            .map(
                |WireContextSection { name, text }| fr_wire::message::ContextSnapshotSection {
                    name,
                    text,
                },
            )
            .collect(),
        joined_sections,
        tools: wire.tools.clone(),
    })
}

/// The instruction-relevant view of one message (the wire
/// [`VisibleMessage`] source/content pair).
fn visible(message: &Message) -> Option<VisibleMessage> {
    let value = serde_json::to_value(message).ok()?;
    let object = value.as_object()?;
    Some(VisibleMessage {
        source: object
            .get("source")
            .and_then(|value| value.as_object())
            .cloned()
            .unwrap_or_default(),
        content: object
            .get("content")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    })
}

/// The loop-side system-prompt face over the outbound require seam.
#[derive(Debug, Clone)]
pub struct SystemPromptClient {
    outbound: Outbound,
    /// The last assembled wire document, kept for `refresh_context`
    /// (the reduced product the driver holds cannot round-trip).
    wire: Option<WireAssembly>,
}

impl SystemPromptClient {
    /// Builds the client over the activation's outbound seam.
    #[must_use]
    pub const fn new(outbound: Outbound) -> Self {
        Self {
            outbound,
            wire: None,
        }
    }

    fn require(&self, method: u32, payload: &impl serde::Serialize) -> Result<Vec<u8>, String> {
        let body =
            serde_json::to_vec(payload).map_err(|error| call_error("request encoding", error))?;
        self.outbound
            .require(SERVICE_NAME, method, &body)
            .map_err(|error: RequireError| call_error("transport", error))
    }
}

impl SystemPromptFace for SystemPromptClient {
    fn assemble(&mut self) -> Result<PromptAssembly, String> {
        let context = WireAssembleContext::default();
        let reply = self.require(METHOD_ASSEMBLE, &context)?;
        let wire: WireAssembly =
            serde_json::from_slice(&reply).map_err(|error| call_error("assemble", error))?;
        let product = reduce(&wire)?;
        self.wire = Some(wire);
        Ok(product)
    }

    fn refresh_context(&mut self, _assembly: &PromptAssembly) -> Result<PromptAssembly, String> {
        // The passed product is the one `assemble` returned over this
        // same client; the cached wire document is its full form.
        let assembly = self
            .wire
            .clone()
            .ok_or_else(|| call_error("refresh_context", "no assembly to refresh"))?;
        let request = RefreshRequest {
            assembly,
            context: WireAssembleContext::default(),
        };
        let reply = self.require(METHOD_REFRESH_CONTEXT, &request)?;
        let wire: WireAssembly =
            serde_json::from_slice(&reply).map_err(|error| call_error("refresh_context", error))?;
        let product = reduce(&wire)?;
        self.wire = Some(wire);
        Ok(product)
    }
}

/// The loop-side workspace-instructions bridge over the outbound require
/// seam: one `compose_instructions` call per step boundary.
#[derive(Debug, Clone)]
pub struct InstructionsClient {
    outbound: Outbound,
    session_id: String,
    /// The session working directory (the working-directory service is a
    /// Phase 18 assembly; M1 headless runs from the host's directory).
    cwd: String,
}

impl InstructionsClient {
    /// Builds the bridge over the activation's outbound seam, bound to
    /// one session and its working directory.
    #[must_use]
    pub fn new(outbound: Outbound, session_id: &str, cwd: &str) -> Self {
        Self {
            outbound,
            session_id: session_id.to_owned(),
            cwd: cwd.to_owned(),
        }
    }
}

impl InstructionsBridge for InstructionsClient {
    fn compose(
        &mut self,
        next_id: &dyn Fn() -> MessageId,
        claimed: &[Message],
        pending: &[Message],
        touched: &[String],
        surface: &[Message],
    ) -> Result<Option<Message>, String> {
        let request = ComposeRequest {
            session_id: self.session_id.clone(),
            cwd: self.cwd.clone(),
            claimed: claimed.iter().filter_map(visible).collect(),
            pending: pending.iter().filter_map(visible).collect(),
            touched_paths: touched.to_vec(),
            surface: surface.iter().filter_map(visible).collect(),
        };
        let body =
            serde_json::to_vec(&request).map_err(|error| call_error("request encoding", error))?;
        let reply = self
            .outbound
            .require(SERVICE_NAME, METHOD_COMPOSE_INSTRUCTIONS, &body)
            .map_err(|error: RequireError| call_error("transport", error))?;
        let reply: ComposeReply =
            serde_json::from_slice(&reply).map_err(|error| call_error("compose", error))?;
        let Some(message) = reply.message else {
            return Ok(None);
        };
        // A pending twin of the composed payload keeps its identity — dsh
        // short-circuits the recompose to `pending[0]` verbatim.
        if let Some(twin) = pending
            .iter()
            .find(|candidate| twin_of(candidate, &message))
        {
            return Ok(Some(twin.clone()));
        }
        lower_with(next_id, message).map(Some)
    }
}

/// Whether one pending message carries exactly the composed payload
/// (content and source, dsh `sameContextPayload`).
fn twin_of(candidate: &Message, message: &InstructionMessage) -> bool {
    let Some(view) = visible(candidate) else {
        return false;
    };
    let content: Vec<serde_json::Value> = message
        .content
        .iter()
        .map(|block| serde_json::json!({ "type": block.block_type, "text": block.text }))
        .collect();
    view.content == serde_json::Value::Array(content) && view.source == message.source
}

/// Lowers one composed wire message with a caller-minted identity (the
/// id seam the runtime-context messages already use).
fn lower_with(
    next_id: &dyn Fn() -> MessageId,
    message: InstructionMessage,
) -> Result<Message, String> {
    let content: Vec<serde_json::Value> = message
        .content
        .into_iter()
        .map(|block| serde_json::json!({ "type": block.block_type, "text": block.text }))
        .collect();
    let document = serde_json::json!({
        "role": "user",
        "id": next_id(),
        "content": content,
        "source": serde_json::Value::Object(message.source),
    });
    serde_json::from_value(document)
        .map_err(|error| format!("composed instruction message is invalid: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{InstructionsClient, SystemPromptClient};
    use crate::driver::{InstructionsBridge, SystemPromptFace};
    use crate::outbound::Outbound;
    use dougong_abi::{HostExports, STATUS_CAPACITY, STATUS_FAILED, STATUS_OK, write_out};
    use fr_system_prompt::instructions::config::InstructionConfig;
    use fr_system_prompt::instructions::files::MemoryFs;
    use fr_system_prompt::registry::{PromptSectionSpec, RegistryConfig, SectionText};
    use fr_system_prompt::service::{
        METHOD_REGISTER_SECTION, METHOD_REGISTER_VARIABLE, PromptService, RegisterVariableRequest,
    };
    use fr_wire::ids::MessageId;
    use fr_wire::message::Message;
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicU64, Ordering};

    unsafe extern "C" fn quiet_emit(
        _host: *mut c_void,
        _event: *const u8,
        _event_len: usize,
        _payload: *const u8,
        _payload_len: usize,
    ) -> i32 {
        STATUS_OK
    }

    /// Routes one require into the real fr-system-prompt service dispatch,
    /// mirroring the shell's `{"error": ...}` failure wrapping (the two
    /// shells' export tables cannot rlib-link into one test binary, so the
    /// pin anchors the serve layer — the `llm_client` judgment).
    unsafe extern "C" fn serving_require(
        host: *mut c_void,
        service: *const u8,
        service_len: usize,
        method: u32,
        in_data: *const u8,
        in_len: usize,
        out_buf: *mut u8,
        cap: usize,
        out_len: *mut usize,
    ) -> i32 {
        // SAFETY: the handle aliases the test-owned service box.
        let served = unsafe { &mut *(host.cast::<PromptService>()) };
        // SAFETY: the name slice is valid for this call.
        let called =
            String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(service, service_len) })
                .into_owned();
        if called != fr_system_prompt::service::SERVICE_NAME {
            let body = serde_json::json!({ "error": "wrong service" }).to_string();
            // SAFETY: the vtable contract guarantees the caller-owned buffer.
            let _ = unsafe { write_out(body.as_bytes(), out_buf, cap, out_len) };
            return STATUS_FAILED;
        }
        // SAFETY: the payload slice is valid for this call.
        let payload = unsafe { std::slice::from_raw_parts(in_data, in_len) };
        match fr_system_prompt::service::serve(served, method, payload) {
            Ok(reply) => {
                // SAFETY: same caller-owned buffer contract.
                if unsafe { write_out(&reply, out_buf, cap, out_len) } {
                    STATUS_OK
                } else {
                    STATUS_CAPACITY
                }
            }
            Err(failure) => {
                let body = serde_json::json!({ "error": failure.to_string() }).to_string();
                // SAFETY: same caller-owned buffer contract.
                let _ = unsafe { write_out(body.as_bytes(), out_buf, cap, out_len) };
                STATUS_FAILED
            }
        }
    }

    fn service_with_section() -> *mut PromptService {
        let mut seam = MemoryFs::new();
        seam.write("/proj/AGENTS.md", "workspace guide");
        let service = PromptService::new(
            &RegistryConfig::default(),
            &InstructionConfig::base_patch(),
            Box::new(seam),
        )
        .expect("service builds");
        let service = Box::into_raw(Box::new(service));
        // SAFETY: the box pointer is dereferenced for the state's lifetime
        // (reclaimed exactly once at each test's end).
        unsafe {
            let spec = PromptSectionSpec {
                name: "test:section".to_owned(),
                order: 10.0,
                text: SectionText::Literal("hello {{name}}".to_owned()),
                interpolate: None,
                complete: None,
            };
            fr_system_prompt::service::serve(
                &mut *service,
                METHOD_REGISTER_SECTION,
                &serde_json::to_vec(&spec).expect("spec serializes"),
            )
            .expect("section registers");
            let variable = RegisterVariableRequest::Literal {
                name: "name".to_owned(),
                value: "world".to_owned(),
            };
            fr_system_prompt::service::serve(
                &mut *service,
                METHOD_REGISTER_VARIABLE,
                &serde_json::to_vec(&variable).expect("variable serializes"),
            )
            .expect("variable registers");
        }
        service
    }

    fn release(service: *mut PromptService) {
        // SAFETY: the requires returned synchronously; the box is
        // reclaimed exactly once.
        drop(unsafe { Box::from_raw(service) });
    }

    fn next_id() -> impl Fn() -> MessageId {
        let counter = AtomicU64::new(0);
        move || {
            let value = counter.fetch_add(1, Ordering::Relaxed);
            MessageId::new(format!("mid-{value}"))
        }
    }

    /// The full assembly round trip against the real fr-system-prompt
    /// dispatch: sections and variables cross the wire, the render
    /// product reaches the driver's reduced shape, and `refresh_context`
    /// reuses the cached wire document.
    #[test]
    fn assembly_round_trips_against_the_real_dispatch() {
        let service = service_with_section();
        let outbound = Outbound::new(HostExports {
            grant: crate::outbound::quiet_grant,
            host_handle: service.cast::<c_void>(),
            require: serving_require,
            emit: quiet_emit,
        });
        let mut client = SystemPromptClient::new(outbound);
        let assembly = client.assemble().expect("assemble over wire");
        assert_eq!(
            assembly.rendered_prompt,
            "You are an AI agent powered by Fly Rafter.\n\nhello world"
        );
        assert_eq!(assembly.joined_sections, String::new());
        let refreshed = client
            .refresh_context(&assembly)
            .expect("refresh over wire");
        assert_eq!(refreshed, assembly);
        // Without a prior assemble there is nothing to refresh (the
        // driver always holds one before refreshing).
        let mut fresh = SystemPromptClient::new(Outbound::new(HostExports {
            grant: crate::outbound::quiet_grant,
            host_handle: service.cast::<c_void>(),
            require: serving_require,
            emit: quiet_emit,
        }));
        fresh.refresh_context(&assembly).unwrap_err();
        release(service);
    }

    /// The compose round trip: a seeded AGENTS.md composes the baseline
    /// message over the wire, and a recompose over the same pending
    /// message keeps its identity (dsh returns `pending[0]` verbatim).
    #[test]
    fn compose_round_trips_and_reuses_the_pending_twin() {
        let service = service_with_section();
        let outbound = Outbound::new(HostExports {
            grant: crate::outbound::quiet_grant,
            host_handle: service.cast::<c_void>(),
            require: serving_require,
            emit: quiet_emit,
        });
        let mut bridge = InstructionsClient::new(outbound, "s1", "/proj");
        let id = next_id();
        let desired = bridge
            .compose(&id, &[], &[], &[], &[])
            .expect("compose over wire")
            .expect("the seeded baseline is due");
        assert!(matches!(&desired, Message::Known(known) if known.role_name() == "user"));
        let view = serde_json::to_value(&desired).expect("message serializes");
        assert_eq!(view["source"]["kind"], "agent-instructions");
        assert_eq!(view["source"]["baseline"], true);
        assert!(
            view["content"]
                .as_array()
                .is_some_and(|blocks| !blocks.is_empty())
        );
        let first_id = view["id"].as_str().expect("minted id").to_owned();

        // The recompose short-circuits to the pending head: same payload,
        // same identity.
        let recomposed = bridge
            .compose(&next_id(), &[], std::slice::from_ref(&desired), &[], &[])
            .expect("recompose over wire")
            .expect("the baseline stays due");
        let view = serde_json::to_value(&recomposed).expect("message serializes");
        assert_eq!(view["id"].as_str(), Some(first_id.as_str()));
        release(service);
    }

    /// An empty filesystem composes nothing, and host failures surface as
    /// the face's text error.
    #[test]
    fn compose_without_files_returns_none() {
        let service = PromptService::new(
            &RegistryConfig::default(),
            &InstructionConfig::base_patch(),
            Box::new(MemoryFs::new()),
        )
        .expect("service builds");
        let service = Box::into_raw(Box::new(service));
        let outbound = Outbound::new(HostExports {
            grant: crate::outbound::quiet_grant,
            host_handle: service.cast::<c_void>(),
            require: serving_require,
            emit: quiet_emit,
        });
        let mut bridge = InstructionsClient::new(outbound, "s1", "/nowhere");
        let composed = bridge
            .compose(&next_id(), &[], &[], &[], &[])
            .expect("compose over wire");
        assert!(composed.is_none());
        release(service);
    }

    unsafe extern "C" fn failing_require(
        _host: *mut c_void,
        _service: *const u8,
        _service_len: usize,
        _method: u32,
        _in_data: *const u8,
        _in_len: usize,
        out_buf: *mut u8,
        cap: usize,
        out_len: *mut usize,
    ) -> i32 {
        // SAFETY: the vtable contract guarantees the caller-owned buffer.
        let _ = unsafe { write_out(br#"{"error":"down"}"#, out_buf, cap, out_len) };
        STATUS_FAILED
    }

    #[test]
    fn host_failures_surface_as_face_errors() {
        let outbound = Outbound::new(HostExports {
            grant: crate::outbound::quiet_grant,
            host_handle: std::ptr::null_mut(),
            require: failing_require,
            emit: quiet_emit,
        });
        let mut client = SystemPromptClient::new(outbound);
        let error = client.assemble().expect_err("host rejected");
        assert!(error.contains("down"));
        let mut bridge = InstructionsClient::new(outbound, "s1", "/proj");
        let error = bridge
            .compose(&next_id(), &[], &[], &[], &[])
            .expect_err("host rejected");
        assert!(error.contains("down"));
    }
}
