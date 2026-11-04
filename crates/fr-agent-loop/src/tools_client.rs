// Rust guideline compliant 2026-11-04
//! The consumer-side `'tools'`-service wrappers: the loop-side
//! [`ToolsFace`] over the dougong dynamic channel.
//!
//! One thin client over the activation's outbound seam: encode the
//! fr-tools service payloads, perform a synchronous `require`, decode
//! the reply. The registry mints the execution token at `prepare`; the
//! client keys it by the loop's call id and hands the minted execution
//! back through [`ToolPrepared`], so `dispatch`/`finalize`/`finish`
//! route by token the way the dsh `TOOL_RUNTIME_SCHEDULER` revisits the
//! same execution object. The settle pull carries the client's agent
//! identity (Q8A, 2026-11-04), so one session's drain never consumes
//! another session's settlement; a settled wire result travels back
//! verbatim at `finalize`/`finish` — the loop-local projection has no
//! `value`, and the registry's post-execute waterfall and `tools/result`
//! observers must still see the canonical value the dispatch produced.
//!
//! Failure mapping follows the producer face: `prepare`/`dispatch`/
//! `finalize` surface the scheduler failure text through their
//! `Result` channel; the infallible faces (`execution_mode`,
//! `settled`, `finish`) fall back conservatively or fail loud —
//! `execution_mode` degrades to the exclusive barrier so the next
//! fallible call reports the broken composition through its own
//! channel, while `settled`/`finish` panic the way the session reads
//! do: silently swallowing a settlement would park the loop forever
//! with no error surface.

use std::collections::HashMap;

use fr_tools::service::{
    DispatchRequest, ExecutionModeRequest, FinalizeRequest, FinishRequest, METHOD_DISPATCH,
    METHOD_EXECUTION_MODE, METHOD_FINALIZE, METHOD_FINISH, METHOD_PREPARE, METHOD_SETTLED,
    SERVICE_NAME, WireCall, WireExecution, WirePreparation, WireSettle, WireSettleKind,
};
use fr_tools::types::ToolExecutionMode as WireToolMode;
use fr_tools::types::ToolExecutionResult as WireToolResult;
use fr_wire::event::ToolResultError;
use fr_wire::ids::ToolCallId;

use crate::driver::{
    ToolExecution, ToolExecutionMode, ToolExecutionResult, ToolPrepared, ToolSettlement, ToolsFace,
};
use crate::outbound::Outbound;

/// One prepared call's client-side record: the minted token plus the
/// settled wire result awaiting the finalize/finish round trip. Records
/// of calls whose group was abandoned without a settlement stay until
/// the session driver (and with it the client) ends — the same bounded
/// lifetime the dsh per-loop scheduler's queue has.
#[derive(Debug, Clone)]
struct CallRecord {
    token: u64,
    wire_result: Option<WireToolResult>,
}

/// The loop-side tools face over the outbound require seam: every
/// method is one synchronous call; the settle wake that precedes a
/// pull arrives through the shell's event listener.
#[derive(Debug, Clone)]
pub struct ToolsClient {
    outbound: Outbound,
    agent: String,
    calls: HashMap<String, CallRecord>,
}

impl ToolsClient {
    /// Builds the client over the activation's outbound seam. `agent`
    /// is the initiator identity every call carries (the dsh
    /// `requireInitiator` face) and the one the settle pull filters by.
    #[must_use]
    pub fn new(outbound: Outbound, agent: &str) -> Self {
        Self {
            outbound,
            agent: agent.to_owned(),
            calls: HashMap::new(),
        }
    }

    fn require(&self, method: u32, payload: &impl serde::Serialize) -> Result<Vec<u8>, String> {
        let body = serde_json::to_vec(payload)
            .map_err(|error| format!("tools request encoding failed: {error}"))?;
        self.outbound
            .require(SERVICE_NAME, method, &body)
            .map_err(|error| error.to_string())
    }

    fn record_of(&self, stage: &str, exec: &ToolExecution) -> Result<u64, String> {
        self.calls
            .get(exec.call_id.as_str())
            .map(|record| record.token)
            .ok_or_else(|| format!("tools {stage}: unknown call {}", exec.call_id))
    }
}

/// Maps one wire execution onto the loop's projection (the loop face
/// carries no root/schema/agent/parent chain; the M1 tools face runs
/// root-model calls only).
fn execution_from_wire(exec: WireExecution) -> ToolExecution {
    ToolExecution {
        call_id: ToolCallId::new(exec.call_id),
        name: exec.name,
        arguments: exec.arguments,
    }
}

/// Maps one wire result onto the loop's flat projection: the durable
/// error identity is the failure's `info` (the dsh `result.error?.info`
/// projection); a success never carries one.
fn result_from_wire(wire: WireToolResult) -> ToolExecutionResult {
    match wire {
        WireToolResult::Success(success) => ToolExecutionResult {
            content: success.content,
            is_error: false,
            error: None,
            meta: success.meta,
            additional_contexts: success.additional_contexts,
            concludes_turn: success.concludes_turn,
        },
        WireToolResult::Failure(failure) => ToolExecutionResult {
            content: failure.content,
            is_error: true,
            error: failure.error.info.map(|info| ToolResultError {
                name: info.name,
                code: info.code,
                reason: info.reason,
            }),
            meta: failure.meta,
            additional_contexts: failure.additional_contexts,
            concludes_turn: false,
        },
    }
}

impl ToolsFace for ToolsClient {
    /// One wire classification; a broken composition degrades to the
    /// exclusive barrier and lets the next fallible call (prepare)
    /// surface the transport failure through its own channel.
    fn execution_mode(&mut self, exec: &ToolExecution) -> ToolExecutionMode {
        let request = ExecutionModeRequest {
            name: exec.name.clone(),
            arguments: exec.arguments.clone(),
            nested: false,
        };
        let Ok(reply) = self.require(METHOD_EXECUTION_MODE, &request) else {
            return ToolExecutionMode::Exclusive;
        };
        let Ok(mode) = serde_json::from_slice::<WireToolMode>(&reply) else {
            return ToolExecutionMode::Exclusive;
        };
        match mode {
            WireToolMode::Parallel => ToolExecutionMode::Parallel,
            WireToolMode::Exclusive => ToolExecutionMode::Exclusive,
        }
    }

    fn prepare(&mut self, exec: &ToolExecution) -> Result<ToolPrepared, String> {
        let call = WireCall {
            call_id: exec.call_id.as_str().to_owned(),
            root_call_id: None,
            name: exec.name.clone(),
            schema: None,
            arguments: exec.arguments.clone(),
            agent: Some(self.agent.clone()),
            parent: None,
        };
        let reply = self.require(METHOD_PREPARE, &call)?;
        let wired: WirePreparation = serde_json::from_slice(&reply)
            .map_err(|error| format!("tools prepare reply is invalid: {error}"))?;
        match wired {
            WirePreparation::Dispatch { exec } => {
                let token = exec.token;
                let mapped = execution_from_wire(exec);
                self.calls.insert(
                    mapped.call_id.as_str().to_owned(),
                    CallRecord {
                        token,
                        wire_result: None,
                    },
                );
                Ok(ToolPrepared::Dispatch { exec: mapped })
            }
            WirePreparation::PostResult { exec, result } => {
                let token = exec.token;
                let mapped_exec = execution_from_wire(exec);
                let mapped_result = result_from_wire(result.clone());
                self.calls.insert(
                    mapped_exec.call_id.as_str().to_owned(),
                    CallRecord {
                        token,
                        wire_result: Some(result),
                    },
                );
                Ok(ToolPrepared::PostResult {
                    exec: mapped_exec,
                    result: mapped_result,
                })
            }
            WirePreparation::FinalResult { exec, result } => {
                let token = exec.token;
                let mapped_exec = execution_from_wire(exec);
                let mapped_result = result_from_wire(result.clone());
                self.calls.insert(
                    mapped_exec.call_id.as_str().to_owned(),
                    CallRecord {
                        token,
                        wire_result: Some(result),
                    },
                );
                Ok(ToolPrepared::FinalResult {
                    exec: mapped_exec,
                    result: mapped_result,
                })
            }
        }
    }

    fn dispatch(&mut self, exec: &ToolExecution) -> Result<(), String> {
        let token = self.record_of("dispatch", exec)?;
        self.require(METHOD_DISPATCH, &DispatchRequest { token })?;
        Ok(())
    }

    /// One settle pull scoped to this client's agent (Q8A). Settlements
    /// whose token this client never minted stay unreported (the driver
    /// filters them the same way); a transport or decode failure here
    /// means a broken composition, so the read fails loud — silently
    /// returning an empty pull would park the loop with no error
    /// surface.
    ///
    /// # Panics
    ///
    /// Always, on a require or decode failure: the panic names the
    /// transport or decode error (the dsh throw equivalent, mirroring
    /// the session-reads judgment).
    fn settled(&mut self) -> Vec<(ToolCallId, ToolSettlement)> {
        let reply = self
            .require(METHOD_SETTLED, &serde_json::json!({ "agent": self.agent }))
            .unwrap_or_else(|error| fail_loud("pull", &error));
        let settles: Vec<WireSettle> = serde_json::from_slice(&reply)
            .unwrap_or_else(|error| fail_loud("decode", &error.to_string()));
        let mut out = Vec::new();
        for settle in settles {
            let Some((call_id, record)) = self
                .calls
                .iter_mut()
                .find(|(_, record)| record.token == settle.token)
                .map(|(call_id, record)| (call_id.clone(), record))
            else {
                continue;
            };
            record.wire_result = Some(settle.result.clone());
            let settlement = ToolSettlement {
                result: result_from_wire(settle.result),
                needs_post: matches!(settle.kind, WireSettleKind::PostResult),
            };
            out.push((ToolCallId::new(call_id), settlement));
        }
        out
    }

    /// Drives the registry's post-execute waterfall for one post-result
    /// settlement. The request carries the settled wire result the
    /// client stored at the pull (`_result`, the loop's flat
    /// projection, is superseded: it has no `value`, and the waterfall
    /// and the `tools/result` observers judge the canonical outcome).
    fn finalize(
        &mut self,
        exec: &ToolExecution,
        _result: ToolExecutionResult,
    ) -> Result<ToolExecutionResult, String> {
        let record = self
            .calls
            .remove(exec.call_id.as_str())
            .ok_or_else(|| format!("tools finalize: unknown call {}", exec.call_id))?;
        let Some(wire_result) = record.wire_result else {
            return Err(format!(
                "tools finalize: call {} has no settled wire result",
                exec.call_id
            ));
        };
        let request = FinalizeRequest {
            token: record.token,
            result: wire_result,
        };
        let reply = self.require(METHOD_FINALIZE, &request)?;
        let wired: WireToolResult = serde_json::from_slice(&reply)
            .map_err(|error| format!("tools finalize reply is invalid: {error}"))?;
        Ok(result_from_wire(wired))
    }

    /// Synchronously finishes one settlement; the same stored-wire
    /// round trip as [`ToolsFace::finalize`] applies.
    ///
    /// # Panics
    ///
    /// Always, on an unknown call, a missing wire result, or a require
    /// or decode failure (the trait face has no error channel; the
    /// session-reads fail-loud judgment applies).
    fn finish(
        &mut self,
        exec: &ToolExecution,
        _result: ToolExecutionResult,
    ) -> ToolExecutionResult {
        let record = self
            .calls
            .remove(exec.call_id.as_str())
            .unwrap_or_else(|| fail_finish(exec.call_id.as_str(), "unknown call"));
        let Some(wire_result) = record.wire_result else {
            fail_finish(exec.call_id.as_str(), "the call has no settled wire result");
        };
        let request = FinishRequest {
            token: record.token,
            result: wire_result,
        };
        let reply = self
            .require(METHOD_FINISH, &request)
            .unwrap_or_else(|error| fail_loud("finish pull", &error));
        let wired: WireToolResult = serde_json::from_slice(&reply)
            .unwrap_or_else(|error| fail_loud("finish decode", &error.to_string()));
        result_from_wire(wired)
    }
}

/// Fails loud on an infallible-face failure (the dsh throw equivalent).
///
/// # Panics
///
/// Always: the failure names the stage and the transport or decode
/// error.
fn fail_loud(stage: &str, error: &str) -> ! {
    panic!("tools {stage} failed: {error}");
}

/// Fails loud on a finish-stage bookkeeping failure.
///
/// # Panics
///
/// Always: the failure names the call and the defect.
fn fail_finish(call_id: &str, why: &str) -> ! {
    panic!("tools finish failed for call {call_id}: {why}");
}

#[cfg(test)]
mod tests {
    use super::ToolsClient;
    use crate::driver::{ToolExecution, ToolExecutionMode, ToolPrepared, ToolsFace};
    use crate::outbound::{Outbound, noop_dispatch, quiet_grant};
    use dougong_abi::{HostExports, STATUS_FAILED, STATUS_OK};
    use fr_tools::definition::DefineTool;
    use fr_tools::registry::{ToolRegistry, ToolsEmitter};
    use fr_tools::service::ToolEvents;
    use fr_tools::service::{SERVICE_NAME as WIRE_SERVICE, ToolsService};
    use fr_tools::types::{ToolPresentationMode, text_content};
    use std::sync::Arc;
    use std::time::Duration;

    unsafe extern "C" fn quiet_emit(
        _host: *mut std::ffi::c_void,
        _event: *const u8,
        _event_len: usize,
        _payload: *const u8,
        _payload_len: usize,
    ) -> i32 {
        STATUS_OK
    }

    /// Routes one require into the real fr-tools service dispatch. The
    /// shell's own dispatch wraps this same `serve` with the FFI status
    /// mapping (pinned fr-tools-side); the export tables of two shells
    /// cannot rlib-link into one test binary, so the client test drives
    /// the service layer and mirrors the shell's failure wrapping — the
    /// `{"error": ...}` shape both sides pin.
    unsafe extern "C" fn real_tools_require(
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
        // SAFETY: the handle aliases the test-owned service box; the
        // test drives it from one thread, so the mutable alias is
        // exclusive.
        let served = unsafe { &mut *(host.cast::<ToolsService>()) };
        // SAFETY: the name slice is valid for this call.
        let called = unsafe {
            String::from_utf8_lossy(std::slice::from_raw_parts(service, service_len)).into_owned()
        };
        if called != WIRE_SERVICE {
            return STATUS_FAILED;
        }
        // SAFETY: the payload slice is valid for this call.
        let payload = unsafe { std::slice::from_raw_parts(in_data, in_len) };
        match served.serve(method, payload) {
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
            Err(error) => {
                let body = serde_json::json!({ "error": error.0 }).to_string();
                // SAFETY: same caller-owned buffer contract.
                let _ = unsafe { dougong_abi::write_out(body.as_bytes(), out_buf, cap, out_len) };
                STATUS_FAILED
            }
        }
    }

    fn echo_tool() -> Arc<dyn fr_tools::definition::ToolDefinition> {
        let tool = DefineTool {
            name: "echo".to_owned(),
            description: "echo".to_owned(),
            parameters: serde_json::json!({"value": {"type": "string"}}),
            output_schema: serde_json::json!({"type": "string"}),
            render: Arc::new(|_args: &serde_json::Value, value: &serde_json::Value| {
                text_content(value.as_str().unwrap_or_default())
            }),
            presentation_meta: None,
            execute: Arc::new(|args, _run| Ok(serde_json::json!(args["value"].clone()))),
            defer_loading: false,
            timeout_ms: None,
            concurrency_safe: None,
            project_content: None,
            finalize_content: None,
            present_call: None,
            present_result: None,
        };
        Arc::new(tool.build().expect("builds"))
    }

    /// Builds one client over a real fr-tools service behind a fake
    /// host seam; the raw box is reclaimed by [`release`].
    fn echo_client(agent: &str) -> (ToolsClient, *mut ToolsService) {
        let events = Arc::new(ToolEvents::default());
        let registry = ToolRegistry::new(
            ToolPresentationMode::Native,
            Arc::clone(&events) as Arc<dyn ToolsEmitter>,
        );
        let service = Box::into_raw(Box::new(ToolsService::new(registry, events)));
        // SAFETY: the box is dereferenced for the state's lifetime
        // (reclaimed exactly once at the test's end).
        let registration = unsafe { (*service).registry().register(echo_tool()) };
        std::mem::forget(registration.expect("registers"));
        let outbound = Outbound::new(HostExports {
            grant: quiet_grant,
            dispatch: noop_dispatch,
            host_handle: service.cast::<std::ffi::c_void>(),
            require: real_tools_require,
            emit: quiet_emit,
        });
        (ToolsClient::new(outbound, agent), service)
    }

    fn release(service: *mut ToolsService) {
        // SAFETY: the require path returned synchronously; the box is
        // reclaimed exactly once. Dispatch workers joined first.
        let mut service = unsafe { Box::from_raw(service) };
        service.join_workers();
    }

    fn exec(call_id: &str) -> ToolExecution {
        ToolExecution {
            call_id: fr_wire::ids::ToolCallId::new(call_id),
            name: "echo".to_owned(),
            arguments: serde_json::json!({ "value": "x" }),
        }
    }

    /// The full scheduler round trip against the real fr-tools
    /// dispatch: mode, prepare, dispatch, scoped settle pull, the wire
    /// result traveling back through finalize and finish.
    #[test]
    fn face_round_trips_the_segmented_scheduler_over_the_wire() {
        let (mut client, state) = echo_client("session-1");
        let exec = exec("call-1");
        assert!(matches!(
            client.execution_mode(&exec),
            ToolExecutionMode::Exclusive
        ));
        let ToolPrepared::Dispatch { exec: minted } =
            client.prepare(&exec).expect("prepare over the wire")
        else {
            panic!("an allowed echo call dispatches");
        };
        client.dispatch(&minted).expect("dispatch over the wire");
        let settlements = (0..2000)
            .find_map(|_| {
                let pulled = client.settled();
                if pulled.is_empty() {
                    std::thread::sleep(Duration::from_millis(1));
                    None
                } else {
                    Some(pulled)
                }
            })
            .expect("no settle arrived within the bounded wait");
        assert_eq!(settlements.len(), 1);
        let (call_id, settlement) = &settlements[0];
        assert_eq!(call_id.as_str(), "call-1");
        assert!(
            settlement.needs_post,
            "an allowed call settles for post-execute"
        );
        let finalized = client
            .finalize(&minted, settlement.result.clone())
            .expect("finalize over the wire");
        assert!(!finalized.is_error);
        assert_eq!(
            finalized
                .content
                .iter()
                .filter_map(|block| match block {
                    fr_wire::ContentBlock::Known(fr_wire::KnownContentBlock::Text { text }) => {
                        Some(text.as_str())
                    }
                    _ => None,
                })
                .collect::<String>(),
            "x",
            "the canonical value rendered back over the wire"
        );
        release(state);
    }

    /// One client's pull never consumes another client's settle (Q8A):
    /// both prepare and dispatch over one service, the scoped drain
    /// routes by the agent each client carries.
    #[test]
    fn settled_pulls_scoped_to_the_client_agent() {
        let (mut client_a, state) = echo_client("session-a");
        let outbound_b = Outbound::new(HostExports {
            grant: quiet_grant,
            dispatch: noop_dispatch,
            host_handle: state.cast::<std::ffi::c_void>(),
            require: real_tools_require,
            emit: quiet_emit,
        });
        let mut client_b = ToolsClient::new(outbound_b, "session-b");
        let exec_a = exec("call-a");
        let ToolPrepared::Dispatch { exec: minted_a } =
            client_a.prepare(&exec_a).expect("prepare a")
        else {
            panic!("an allowed echo call dispatches");
        };
        client_a.dispatch(&minted_a).expect("dispatch a");
        assert!(
            client_b.settled().is_empty(),
            "no settle may cross the agent scope before it exists"
        );
        let settlements = (0..2000)
            .find_map(|_| {
                let pulled = client_a.settled();
                if pulled.is_empty() {
                    std::thread::sleep(Duration::from_millis(1));
                    None
                } else {
                    Some(pulled)
                }
            })
            .expect("no settle arrived within the bounded wait");
        assert_eq!(settlements[0].0.as_str(), "call-a");
        assert!(
            client_b.settled().is_empty(),
            "the scoped drain keeps the other agent's queue untouched"
        );
        release(state);
    }

    /// A dispatch failure rides the post-result settlement (policies
    /// still observe it), the failure's `info` becomes the durable
    /// error identity, and the finish arm carries the wire result back
    /// through the registry's finish stage. The driver's
    /// needs-post-versus-finish selection lives in the driver tests.
    #[test]
    fn face_finishes_a_failed_dispatch_through_the_final_settle() {
        let (mut client, state) = echo_client("session-1");
        let missing = ToolExecution {
            call_id: fr_wire::ids::ToolCallId::new("call-x"),
            name: "missing".to_owned(),
            arguments: serde_json::json!({}),
        };
        let ToolPrepared::Dispatch { exec: minted } =
            client.prepare(&missing).expect("prepare over the wire")
        else {
            panic!("an unknown tool still dispatches");
        };
        client.dispatch(&minted).expect("dispatch over the wire");
        let settlements = (0..2000)
            .find_map(|_| {
                let pulled = client.settled();
                if pulled.is_empty() {
                    std::thread::sleep(Duration::from_millis(1));
                    None
                } else {
                    Some(pulled)
                }
            })
            .expect("no settle arrived within the bounded wait");
        assert_eq!(settlements.len(), 1);
        let (call_id, settlement) = &settlements[0];
        assert_eq!(call_id.as_str(), "call-x");
        assert!(
            settlement.needs_post,
            "a materialized failure still passes post-execute"
        );
        assert!(
            settlement.result.is_error,
            "the dispatch failure is an error result"
        );
        let error = settlement
            .result
            .error
            .as_ref()
            .expect("the failure carries its structured identity");
        assert!(!error.code.is_empty(), "the identity names a routing code");
        let finished = client.finish(&minted, settlement.result.clone());
        assert!(finished.is_error);
        release(state);
    }
}
