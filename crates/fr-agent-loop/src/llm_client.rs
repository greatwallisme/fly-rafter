// Rust guideline compliant 2026-10-28
//! The consumer-side `llm`-service wrappers (Q84-B2, Q85-C): the loop-side
//! [`LlmFace`] over the dougong dynamic channel.
//!
//! One thin client over the activation's outbound seam: encode the
//! fr-llm service payloads, perform a synchronous `require`, decode the
//! reply. Method numbers and payload shapes are the provider contract in
//! `crates/fr-llm/src/generation.rs`; the service-name wiring lands with
//! the Phase 18 assembly (the inject pin follows PRD 8.1 `llm`).
//!
//! Failure mapping: a `STATUS_FAILED` reply carrying the
//! `{"llmError": ...}` document rebuilds the [`LlmError`] with its code
//! and retry facts intact (the `NO_ADAPTER` tolerance and the request-error
//! waterfall route on the code); any other host or reply failure maps to
//! the transport arm of the taxonomy.

use std::fmt;

use fr_llm::adapter::{GenerateOptions, LlmCallConfig, ResolvedModelInfo};
use fr_llm::error::LlmError;
use fr_llm::error::TRANSPORT_CODE;
use fr_llm::generation::{
    AbortRequest, BeginReply, BeginRequest, DrainReply, DrainRequest, METHOD_ABORT, METHOD_BEGIN,
    METHOD_DRAIN, METHOD_PREPARE_CALL, PrepareCallReply, PrepareCallRequest, SERVICE_NAME,
    WireGenerateOptions, from_wire_retry_policy, to_wire_call_config,
};

use crate::driver::{DrainedGeneration, LlmFace, PreparedCall};
use crate::outbound::{Outbound, RequireError};

/// Maps one host rejection onto the taxonomy: the `llmError` document
/// preserves the code (rebuilt through the driver's failure
/// reconstruction); everything else is transport.
fn map_require_error(error: &RequireError) -> LlmError {
    if let Ok(document) = serde_json::from_slice::<serde_json::Value>(&error.detail)
        && let Some(failure) = document.get("llmError")
        && let Ok(failure) = serde_json::from_value::<fr_wire::Failure>(failure.clone())
    {
        return crate::driver::llm_error(&failure);
    }
    LlmError::try_new(&error.to_string(), TRANSPORT_CODE, None, None, None, None)
        .expect("the static transport rejection is impossible")
}

/// One failed `'llm'` call whose reply missed the method contract.
fn reply_error(context: &str, error: impl fmt::Display) -> LlmError {
    LlmError::try_new(
        &format!("llm service reply is invalid: {context}: {error}"),
        TRANSPORT_CODE,
        None,
        None,
        None,
        None,
    )
    .expect("the static transport rejection is impossible")
}

/// The loop-side LLM face over the outbound require seam: every method
/// is one synchronous call; the notification wake that precedes a drain
/// arrives through the shell's event listener.
#[derive(Debug, Clone)]
pub struct LlmClient {
    outbound: Outbound,
}

impl LlmClient {
    /// Builds the client over the activation's outbound seam.
    #[must_use]
    pub const fn new(outbound: Outbound) -> Self {
        Self { outbound }
    }

    fn require(&self, method: u32, payload: &impl serde::Serialize) -> Result<Vec<u8>, LlmError> {
        let body =
            serde_json::to_vec(payload).map_err(|error| reply_error("request encoding", error))?;
        self.outbound
            .require(SERVICE_NAME, method, &body)
            .map_err(|error| map_require_error(&error))
    }
}

impl LlmFace for LlmClient {
    fn prepare_call(&mut self, config: &LlmCallConfig) -> Result<PreparedCall, LlmError> {
        let request = PrepareCallRequest {
            config: to_wire_call_config(config),
        };
        let reply = self.require(METHOD_PREPARE_CALL, &request)?;
        let reply: PrepareCallReply =
            serde_json::from_slice(&reply).map_err(|error| reply_error("prepare_call", error))?;
        Ok(PreparedCall {
            prepared_id: Some(reply.prepared_id),
            config: fr_llm::generation::from_wire_call_config(&reply.config),
            model: ResolvedModelInfo::from(reply.model),
            adapter_defaults: reply.adapter_defaults,
            retry_policy: reply.retry_policy.as_ref().map(from_wire_retry_policy),
        })
    }

    fn begin(
        &mut self,
        request: &GenerateOptions,
        prepared: Option<&PreparedCall>,
    ) -> Result<String, LlmError> {
        let request = BeginRequest {
            request: WireGenerateOptions::from(request.clone()),
            prepared_id: prepared.and_then(|call| call.prepared_id.clone()),
        };
        let reply = self.require(METHOD_BEGIN, &request)?;
        let reply: BeginReply =
            serde_json::from_slice(&reply).map_err(|error| reply_error("begin", error))?;
        Ok(reply.generation_id)
    }

    fn drain(&mut self, generation: &str) -> Result<DrainedGeneration, LlmError> {
        let request = DrainRequest {
            generation_id: generation.to_owned(),
        };
        let reply = self.require(METHOD_DRAIN, &request)?;
        let reply: DrainReply =
            serde_json::from_slice(&reply).map_err(|error| reply_error("drain", error))?;
        Ok(DrainedGeneration {
            chunks: reply.chunks,
            done: reply.done,
        })
    }

    fn abort(&mut self, generation: &str) -> Result<(), LlmError> {
        let request = AbortRequest {
            generation_id: generation.to_owned(),
        };
        self.require(METHOD_ABORT, &request)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::LlmClient;
    use crate::driver::{DrainedGeneration, LlmFace, PreparedCall};
    use crate::outbound::Outbound;
    use dougong_abi::{HostExports, STATUS_FAILED, STATUS_OK};
    use fr_llm::adapter::{GenerateOptions, LlmCallConfig, ModelContext, ModelReasoningInfo};
    use fr_llm::error::LlmError;
    use fr_llm::generation::{GenerationService, GenerationSource, PreparedRecord, ServiceError};
    use fr_wire::call_config::{FinishReason, FinishReasonKind};
    use fr_wire::stream::KnownStreamChunk;
    use fr_wire::{CallControls, StreamChunk};
    use futures_core::Stream;
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{Context, Poll};
    use std::thread::sleep;
    use std::time::Duration;

    /// A stream that yields its chunks immediately.
    struct LoadedStream {
        chunks: VecDeque<StreamChunk>,
    }

    impl Stream for LoadedStream {
        type Item = StreamChunk;

        fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<StreamChunk>> {
            Poll::Ready(self.chunks.pop_front())
        }
    }

    /// A route source serving model defaults and one recorded run.
    struct TestSource;

    impl GenerationSource for TestSource {
        fn prepare(
            &self,
            config: &LlmCallConfig,
        ) -> Result<(PreparedRecord, LlmCallConfig), LlmError> {
            let model = fr_llm::adapter::ResolvedModelInfo {
                provider: "p".to_owned(),
                id: "m".to_owned(),
                name: "m".to_owned(),
                context: Some(ModelContext {
                    context_window: 128,
                }),
                default_max_tokens: Some(4096),
                reasoning: Some(ModelReasoningInfo {
                    efforts: Vec::new(),
                    default_effort: Some(fr_wire::ReasoningEffortId::new("high")),
                }),
                ..fr_llm::adapter::ResolvedModelInfo::default()
            };
            let mut controls = config.controls.clone();
            let mut defaults = fr_wire::AdapterDefaults::default();
            if controls.reasoning_effort.is_none() {
                controls.reasoning_effort = model
                    .reasoning
                    .as_ref()
                    .and_then(|r| r.default_effort.clone());
                defaults.reasoning_effort = Some(fr_wire::content::TrueOnly);
            }
            if controls.max_tokens.is_none() {
                controls.max_tokens = model.default_max_tokens;
                defaults.max_tokens = Some(fr_wire::content::TrueOnly);
            }
            Ok((
                PreparedRecord {
                    model,
                    adapter_defaults: Some(defaults),
                    retry_policy: None,
                },
                LlmCallConfig {
                    provider: config.provider.clone(),
                    model: config.model.clone(),
                    controls,
                    agent_loop: false,
                },
            ))
        }

        fn stream(
            &self,
            _options: GenerateOptions,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            Box::pin(LoadedStream {
                chunks: vec![
                    text("hello"),
                    StreamChunk::Known(KnownStreamChunk::Finish {
                        reason: FinishReason::Known(FinishReasonKind::Stop),
                        replay_state: None,
                    }),
                ]
                .into(),
            })
        }
    }

    fn text(value: &str) -> StreamChunk {
        StreamChunk::Known(KnownStreamChunk::TextDelta {
            index: 0,
            text: value.to_owned(),
        })
    }

    fn options() -> GenerateOptions {
        GenerateOptions {
            provider: "p".to_owned(),
            model: "m".to_owned(),
            reasoning_effort: None,
            messages: Vec::new(),
            system: None,
            tools: None,
            tool_history: None,
            temperature: None,
            max_tokens: None,
            stop: None,
            signal: None,
            session_id: Some(fr_wire::SessionId::new("s1")),
            purpose: None,
        }
    }

    unsafe extern "C" fn quiet_emit(
        _host: *mut std::ffi::c_void,
        _event: *const u8,
        _event_len: usize,
        _payload: *const u8,
        _payload_len: usize,
    ) -> i32 {
        STATUS_OK
    }

    /// Routes one require into the real fr-llm service dispatch. The
    /// shell's own dispatch wraps this same `serve` with the FFI status
    /// mapping (pinned fr-llm-side); the export tables of two shells
    /// cannot rlib-link into one test binary, so the client test drives
    /// the service layer and mirrors the shell's failure wrapping — the
    /// `{"llmError": ...}` shape both sides pin.
    unsafe extern "C" fn real_dispatch_require(
        host: *mut std::ffi::c_void,
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
        let served = unsafe { &*(host.cast::<GenerationService>()) };
        // SAFETY: the name slice is valid for this call.
        let called = unsafe {
            String::from_utf8_lossy(std::slice::from_raw_parts(service, service_len)).into_owned()
        };
        if called != fr_llm::generation::SERVICE_NAME {
            return STATUS_FAILED;
        }
        // SAFETY: the payload slice is valid for this call.
        let payload = unsafe { std::slice::from_raw_parts(in_data, in_len) };
        match fr_llm::generation::serve(served, method, payload) {
            Ok(reply) => {
                // SAFETY: the vtable contract guarantees the caller-owned
                // buffer.
                let written = unsafe { dougong_abi::write_out(&reply, out_buf, cap, out_len) };
                if written {
                    STATUS_OK
                } else {
                    dougong_abi::STATUS_CAPACITY
                }
            }
            Err(ServiceError::Llm(failure)) => {
                let body = serde_json::json!({ "llmError": failure.failure() }).to_string();
                // SAFETY: same caller-owned buffer contract.
                let _ = unsafe { dougong_abi::write_out(body.as_bytes(), out_buf, cap, out_len) };
                STATUS_FAILED
            }
            Err(ServiceError::Text(message)) => {
                let body = serde_json::json!({ "error": message }).to_string();
                // SAFETY: same caller-owned buffer contract.
                let _ = unsafe { dougong_abi::write_out(body.as_bytes(), out_buf, cap, out_len) };
                STATUS_FAILED
            }
        }
    }

    fn real_client() -> (LlmClient, *mut fr_llm::generation::GenerationService) {
        let service = Box::into_raw(Box::new(fr_llm::generation::GenerationService::new(
            fr_llm::generation::quiet_notify(),
        )));
        // SAFETY: the box pointer is dereferenced for the state's lifetime
        // (reclaimed exactly once at the test's end).
        unsafe {
            (*service).register_source("p", Arc::new(TestSource));
        }
        let outbound = Outbound::new(HostExports {
            host_handle: service.cast::<std::ffi::c_void>(),
            require: real_dispatch_require,
            emit: quiet_emit,
        });
        (LlmClient::new(outbound), service)
    }

    fn release(service: *mut fr_llm::generation::GenerationService) {
        // SAFETY: the require path returned synchronously; the box is
        // reclaimed exactly once. The hub join happens first: a loaded
        // stream's worker has settled by the time the drains observed it.
        let service = unsafe { Box::from_raw(service) };
        service.shutdown();
    }

    /// The full consumer round trip against the real fr-llm dispatch:
    /// `prepare_call` materializes the route defaults over the wire, `begin`
    /// carries the minted binding, drain settles the recorded run.
    #[test]
    fn face_round_trips_against_the_real_llm_dispatch() {
        let (mut client, state) = real_client();
        let config = LlmCallConfig {
            provider: "p".to_owned(),
            model: "m".to_owned(),
            controls: CallControls::default(),
            agent_loop: false,
        };
        let prepared: PreparedCall = client.prepare_call(&config).expect("prepare over wire");
        assert!(
            prepared
                .prepared_id
                .as_deref()
                .is_some_and(|id| id.starts_with("prep-"))
        );
        assert_eq!(
            prepared
                .config
                .controls
                .reasoning_effort
                .as_ref()
                .map(fr_wire::ReasoningEffortId::as_str),
            Some("high"),
            "adapter default effort materialized over the wire"
        );
        assert_eq!(
            prepared.config.controls.max_tokens,
            Some(4096),
            "adapter default cap materialized over the wire"
        );
        assert!(
            prepared
                .adapter_defaults
                .as_ref()
                .is_some_and(|d| d.reasoning_effort.is_some() && d.max_tokens.is_some())
        );

        // The loop derives the request from the prepared (materialized)
        // config — dsh `config = preparedCall.config`.
        let mut request = options();
        request.reasoning_effort = prepared.config.controls.reasoning_effort.clone();
        request.max_tokens = prepared.config.controls.max_tokens;
        let id = client
            .begin(&request, Some(&prepared))
            .expect("begin over wire");
        let mut drained = DrainedGeneration::default();
        for _ in 0..500 {
            let batch = client.drain(&id).expect("drain over wire");
            drained.chunks.extend(batch.chunks);
            if batch.done {
                drained.done = true;
                break;
            }
            sleep(Duration::from_millis(2));
        }
        assert!(drained.done);
        assert_eq!(drained.chunks.len(), 2);
        drop(client.abort(&id));
        release(state);
    }

    /// The `llmError` document preserves the code (the `NO_ADAPTER`
    /// tolerance routes on it).
    #[test]
    fn host_failures_carry_the_taxonomy_code() {
        let (mut client, state) = real_client();
        let mut options = options();
        options.provider = "missing".to_owned();
        let error = client
            .begin(&options, None)
            .expect_err("unregistered route");
        assert_eq!(error.code(), "NO_ADAPTER");
        assert_eq!(
            error.message(),
            "no adapter registered for provider \"missing\""
        );
        release(state);
    }

    /// A raw host rejection without the llmError document maps to the
    /// transport arm.
    unsafe extern "C" fn failing_require(
        _host: *mut std::ffi::c_void,
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
        // SAFETY: the vtable contract guarantees the caller-owned buffer.
        let _ = unsafe { dougong_abi::write_out(b"{}", out_buf, cap, out_len) };
        STATUS_FAILED
    }

    #[test]
    fn raw_host_failures_map_to_transport() {
        let outbound = Outbound::new(HostExports {
            host_handle: std::ptr::null_mut(),
            require: failing_require,
            emit: quiet_emit,
        });
        let mut client = LlmClient::new(outbound);
        let error = client
            .prepare_call(&LlmCallConfig {
                provider: "p".to_owned(),
                model: "m".to_owned(),
                controls: CallControls::default(),
                agent_loop: false,
            })
            .expect_err("host rejected");
        assert_eq!(error.code(), "TRANSPORT");
    }

    /// A reply outside the method contract fails the decode with the
    /// transport arm and names the method.
    unsafe extern "C" fn broken_require(
        _host: *mut std::ffi::c_void,
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
        let written = unsafe { dougong_abi::write_out(b"{\"nope\":true}", out_buf, cap, out_len) };
        if written {
            STATUS_OK
        } else {
            dougong_abi::STATUS_CAPACITY
        }
    }

    #[test]
    fn malformed_replies_map_to_transport() {
        let outbound = Outbound::new(HostExports {
            host_handle: std::ptr::null_mut(),
            require: broken_require,
            emit: quiet_emit,
        });
        let mut client = LlmClient::new(outbound);
        let error = client.drain("gen-0").expect_err("reply outside contract");
        assert_eq!(error.code(), "TRANSPORT");
        assert!(error.message().contains("drain"));
    }
}
