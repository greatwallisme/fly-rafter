// Rust guideline compliant 2026-11-02
//! The consumer-side `skills`-service wrapper (16f batch 2): the
//! loop-side [`SkillsClient`] over the dougong dynamic channel.
//!
//! The method number and payload shapes are the provider contract in
//! `crates/fr-tools-builtin` (`skill_tools::serve`; the `llm_client`
//! judgment: wire DTOs stay single-sourced in the provider crate, the
//! consumer converts). One `pre_step` call per step boundary lowers the
//! transformed batch back to the driver's messages.

use fr_wire::services::skills::{METHOD_PRE_STEP, SERVICE_NAME};
use serde_json::Value;

use crate::driver::SkillPreStep;
use crate::outbound::{Outbound, RequireError};
use fr_wire::message::Message;

/// One failed service call: the host rejection or the reply that missed
/// the method contract, as the face's text error.
fn call_error(context: &str, error: impl std::fmt::Display) -> String {
    format!("skills service call failed: {context}: {error}")
}

/// The loop-side skills face over the outbound require seam.
#[derive(Debug, Clone)]
pub struct SkillsClient {
    outbound: Outbound,
}

impl SkillsClient {
    /// Builds the client over the activation's outbound seam.
    #[must_use]
    pub const fn new(outbound: Outbound) -> Self {
        Self { outbound }
    }
}

impl SkillPreStep for SkillsClient {
    fn pre_step(&mut self, session: &str, messages: Vec<Message>) -> Result<Vec<Message>, String> {
        let request = serde_json::json!({ "session": session, "messages": messages });
        let body =
            serde_json::to_vec(&request).map_err(|error| call_error("request encoding", error))?;
        let reply = self
            .outbound
            .require(SERVICE_NAME, METHOD_PRE_STEP, &body)
            .map_err(|error: RequireError| call_error("transport", error))?;
        let reply: Value =
            serde_json::from_slice(&reply).map_err(|error| call_error("reply", error))?;
        let batch = reply
            .get("messages")
            .cloned()
            .ok_or_else(|| call_error("reply", "the reply carries no messages batch"))?;
        serde_json::from_value(batch).map_err(|error| call_error("reply", error))
    }
}

#[cfg(test)]
mod tests {
    use super::{SkillsClient, call_error};
    use crate::driver::SkillPreStep;
    use crate::outbound::{Outbound, quiet_grant};
    use dougong_abi::{HostExports, STATUS_CAPACITY, STATUS_FAILED, STATUS_OK, write_out};
    use fr_wire::content::{ContentBlock, KnownContentBlock, RawObject};
    use fr_wire::ids::MessageId;
    use fr_wire::message::{KnownMessage, KnownMessageSource, Message, MessageSource};
    use std::ffi::c_void;

    /// The scripted host: every require answers with the given reply
    /// bytes, or the given failure (the `prompt_client` stub judgment).
    struct ScriptHost {
        reply: Vec<u8>,
        fail: bool,
    }

    unsafe extern "C" fn script_require(
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
        // SAFETY: the handle aliases the test-owned host box.
        let host = unsafe { &*(host.cast::<ScriptHost>()) };
        let _ = (service, service_len, method, in_data, in_len);
        // SAFETY: the caller-owned buffer contract.
        if unsafe { write_out(&host.reply, out_buf, cap, out_len) } {
            if host.fail { STATUS_FAILED } else { STATUS_OK }
        } else {
            STATUS_CAPACITY
        }
    }

    unsafe extern "C" fn quiet_emit(
        _host: *mut c_void,
        _event: *const u8,
        _event_len: usize,
        _payload: *const u8,
        _payload_len: usize,
    ) -> i32 {
        STATUS_OK
    }

    /// Builds the client over a scripted host; returns the box handle so
    /// the test keeps the storage alive for the require's lifetime.
    fn scripted(reply: Vec<u8>, fail: bool) -> (Box<ScriptHost>, Outbound) {
        let mut host = Box::new(ScriptHost { reply, fail });
        let handle = std::ptr::from_mut(&mut *host).cast::<c_void>();
        let exports = HostExports {
            host_handle: handle,
            require: script_require,
            emit: quiet_emit,
            grant: quiet_grant,
            dispatch: crate::outbound::noop_dispatch,
            realm_spawn: crate::outbound::quiet_realm_spawn,
            realm_retire: crate::outbound::quiet_realm_retire,
            realm_require: crate::outbound::quiet_realm_require,
        };
        (host, Outbound::new(exports))
    }

    /// One plain user message (the `runtime_context` fixture judgment).
    fn user_message() -> Message {
        Message::Known(Box::new(KnownMessage::User {
            id: MessageId::new("m-1"),
            content: vec![ContentBlock::Known(KnownContentBlock::Text {
                text: "hi".to_owned(),
            })],
            source: MessageSource::Known(KnownMessageSource::User {
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        }))
    }

    #[test]
    fn pre_step_round_trips_the_batch_through_the_service() {
        let transformed = serde_json::json!({ "messages": [user_message()] });
        let reply = serde_json::to_vec(&transformed).expect("reply");
        let (host, outbound) = scripted(reply, false);
        let mut client = SkillsClient::new(outbound);
        let batch = client
            .pre_step("agent-1", vec![user_message()])
            .expect("served");
        assert_eq!(batch.len(), 1);
        drop(host);
    }

    #[test]
    fn pre_step_surfaces_the_transport_refusal() {
        let (host, outbound) = scripted(b"boom".to_vec(), true);
        let mut client = SkillsClient::new(outbound);
        let error = client
            .pre_step("agent-1", vec![user_message()])
            .expect_err("refused");
        assert!(
            error.starts_with("skills service call failed: transport"),
            "{error}"
        );
        drop(host);
    }

    #[test]
    fn pre_step_surfaces_the_reply_refusals() {
        let (host, outbound) = scripted(br#"{"messages":"not-a-batch"}"#.to_vec(), false);
        let mut client = SkillsClient::new(outbound);
        let error = client
            .pre_step("agent-1", vec![user_message()])
            .expect_err("refused");
        assert!(
            error.starts_with("skills service call failed: reply"),
            "{error}"
        );
        drop(host);
        let (host, outbound) = scripted(br"{}".to_vec(), false);
        let mut client = SkillsClient::new(outbound);
        let error = client
            .pre_step("agent-1", vec![user_message()])
            .expect_err("refused");
        assert!(error.contains("no messages batch"), "{error}");
        drop(host);
    }

    #[test]
    fn the_error_prefix_is_stable() {
        assert_eq!(
            call_error("error-class", "boom"),
            "skills service call failed: error-class: boom"
        );
    }
}
