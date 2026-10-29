// Rust guideline compliant 2026-10-28
//! Service-face tests: the dispatch surface over a real registry, and
//! the endpoint adapters driven through a fake host vtable with the exact
//! FFI shapes (the fr-agent-loop outbound test judgment).

use std::sync::Arc;
use std::time::Duration;

use dougong_abi::{HostExports, STATUS_FAILED, STATUS_INVALID_PAYLOAD, STATUS_OK, write_out};
use serde_json::{Value, json};

use super::service::{
    DeadlineState, ExecEnter, ExecExit, ExecReply, HostSeam, METHOD_CANCEL, METHOD_DESCRIBE,
    METHOD_DISPATCH, METHOD_EXECUTE, METHOD_EXECUTION_MODE, METHOD_FINALIZE, METHOD_PREPARE,
    METHOD_REGISTER_LISTENER, METHOD_SET_APPROVAL, METHOD_SETTLED, METHOD_UNREGISTER_LISTENER,
    METHOD_WIRE_SCHEMAS, PLUGIN_NAME, PostEnter, PostExit, PostReply, PreEnter, PreExit, PreReply,
    RegisterListenerRequest, ToolEvents, ToolsService, WireEndpoint, WireOutcome, WireStage,
};
use crate::definition::{DefineTool, ToolDefinition};
use crate::registry::ToolRegistry;
use crate::types::{ToolExecutionMode, ToolPresentationMode, text_content};

/// The scripted endpoint handler shape (method, payload) -> reply.
type Endpoint = Box<dyn Fn(u32, &[u8]) -> Result<Vec<u8>, String> + Send>;

/// The fake host state behind the test vtable.
struct TestHost {
    /// The scripted endpoint handler.
    endpoint: Endpoint,
    /// Recorded emits (event name, payload).
    emits: std::sync::Mutex<Vec<(String, Vec<u8>)>>,
}

impl TestHost {
    /// Builds the host state and its vtable; `TestVtable` reclaims the
    /// box on drop.
    fn into_vtable(endpoint: Endpoint) -> TestVtable {
        let handle = Box::into_raw(Box::new(Self {
            endpoint,
            emits: std::sync::Mutex::new(Vec::new()),
        }));
        TestVtable {
            handle,
            exports: HostExports {
                host_handle: handle.cast(),
                require: test_require,
                emit: test_emit,
                grant: test_grant,
            },
        }
    }
}

/// Owns one leaked [`TestHost`] and its vtable.
struct TestVtable {
    handle: *mut TestHost,
    exports: HostExports,
}

impl TestVtable {
    /// The recorded emits.
    fn emits(&self) -> Vec<(String, Vec<u8>)> {
        // SAFETY: `handle` is live until `Drop` reclaims it; tests read
        // it before dropping the vtable.
        unsafe { (*self.handle).emits.lock().expect("emits lock").clone() }
    }
}

impl Drop for TestVtable {
    fn drop(&mut self) {
        // SAFETY: the box was leaked exactly once in `TestHost::new` and
        // no vtable call can race this drop (tests join workers first).
        unsafe { drop(Box::from_raw(self.handle)) };
    }
}

/// The require double: routes to the scripted handler with the buffer
/// protocol.
unsafe extern "C" fn test_require(
    host: *mut std::ffi::c_void,
    _service: *const u8,
    _service_len: usize,
    method: u32,
    in_data: *const u8,
    in_len: usize,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    // SAFETY: the vtable contract keeps `host` valid until teardown; the
    // test keeps the vtable alive for the whole call.
    let host = unsafe { &*host.cast::<TestHost>() };
    let payload: &[u8] = if in_len == 0 || in_data.is_null() {
        &[]
    } else {
        // SAFETY: non-null and valid for `in_len` per the contract.
        unsafe { std::slice::from_raw_parts(in_data, in_len) }
    };
    match (host.endpoint)(method, payload) {
        Ok(reply) => {
            // SAFETY: caller-owned buffer contract.
            if unsafe { write_out(&reply, out, cap, out_len) } {
                STATUS_OK
            } else {
                STATUS_INVALID_PAYLOAD
            }
        }
        Err(text) => {
            let body = json!({ "error": text }).to_string();
            // SAFETY: caller-owned buffer contract.
            let _ = unsafe { write_out(body.as_bytes(), out, cap, out_len) };
            STATUS_FAILED
        }
    }
}

/// The emit double: records name and payload.
unsafe extern "C" fn test_emit(
    host: *mut std::ffi::c_void,
    event: *const u8,
    event_len: usize,
    payload: *const u8,
    payload_len: usize,
) -> i32 {
    // SAFETY: same vtable lifetime contract as `test_require`.
    let host = unsafe { &*host.cast::<TestHost>() };
    let event: &[u8] = if event_len == 0 || event.is_null() {
        &[]
    } else {
        // SAFETY: valid for `event_len` per the contract.
        unsafe { std::slice::from_raw_parts(event, event_len) }
    };
    let payload: &[u8] = if payload_len == 0 || payload.is_null() {
        &[]
    } else {
        // SAFETY: valid for `payload_len` per the contract.
        unsafe { std::slice::from_raw_parts(payload, payload_len) }
    };
    let name = String::from_utf8_lossy(event).into_owned();
    host.emits
        .lock()
        .expect("emits lock")
        .push((name, payload.to_vec()));
    STATUS_OK
}

/// The grant double: accepts every grant (the kernel already validated
/// the semantics under its own tests).
unsafe extern "C" fn test_grant(
    _host: *mut std::ffi::c_void,
    _target: *const u8,
    _target_len: usize,
    _granted: *const u8,
    _granted_len: usize,
) -> i32 {
    STATUS_OK
}

/// Builds one service over a registry with an echo tool attached to the
/// scripted host.
fn service_with_host(endpoint: Endpoint) -> (TestVtable, ToolsService) {
    let events = Arc::new(ToolEvents::default());
    let registry = ToolRegistry::new(
        ToolPresentationMode::Native,
        Arc::clone(&events) as Arc<dyn crate::registry::ToolsEmitter>,
    );
    let mut service = ToolsService::new(registry, events);
    let vtable = TestHost::into_vtable(endpoint);
    service.attach(HostSeam::new(vtable.exports));
    (vtable, service)
}

/// An instant echo tool.
fn echo_tool(name: &str) -> Arc<dyn ToolDefinition> {
    let tool = DefineTool {
        name: name.to_owned(),
        description: "echo".to_owned(),
        parameters: json!({"value": {"type": "string"}}),
        output_schema: json!({"type": "string"}),
        render: Arc::new(|_args: &Value, value: &Value| {
            text_content(value.as_str().unwrap_or_default())
        }),
        presentation_meta: None,
        execute: Arc::new(|args, _run| Ok(json!(args["value"].clone()))),
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

/// A sleeping echo tool with a declared budget.
fn sleeping_tool(name: &str, timeout_ms: Option<f64>, sleep_ms: u64) -> Arc<dyn ToolDefinition> {
    let tool = DefineTool {
        name: name.to_owned(),
        description: "slow".to_owned(),
        parameters: json!({"value": {"type": "string"}}),
        output_schema: json!({"type": "string"}),
        render: Arc::new(|_args: &Value, value: &Value| {
            text_content(value.as_str().unwrap_or_default())
        }),
        presentation_meta: None,
        execute: Arc::new(move |_args, _run| {
            std::thread::sleep(Duration::from_millis(sleep_ms));
            Ok(json!("done"))
        }),
        defer_loading: false,
        timeout_ms,
        concurrency_safe: None,
        project_content: None,
        finalize_content: None,
        present_call: None,
        present_result: None,
    };
    Arc::new(tool.build().expect("builds"))
}

/// One prepare request body.
fn prepare_body(name: &str) -> Vec<u8> {
    json!({ "call_id": "call-1", "name": name, "arguments": { "value": "x" } })
        .to_string()
        .into_bytes()
}

/// One prepare request body carrying the agent the ask path routes
/// through.
fn prepare_body_with_agent(name: &str) -> Vec<u8> {
    json!({
        "call_id": "call-1",
        "name": name,
        "arguments": { "value": "x" },
        "agent": "session-1",
    })
    .to_string()
    .into_bytes()
}

/// Registers a tool for the service's lifetime (the registration
/// disposer is intentionally forgotten: the registry outlives the test
/// bodies that share it).
fn register(service: &ToolsService, tool: Arc<dyn ToolDefinition>) {
    let registration = service.registry().register(tool).expect("registers");
    std::mem::forget(registration);
}

/// Polls `settled` until one settle arrives (bounded wait).
fn wait_settled(service: &mut ToolsService) -> super::service::WireSettle {
    let settle = (0..2000).find_map(|_| {
        let reply = service
            .serve(METHOD_SETTLED, b"null")
            .expect("settled call");
        let settles: Vec<super::service::WireSettle> =
            serde_json::from_slice(&reply).expect("settles decode");
        settles.into_iter().next().or_else(|| {
            std::thread::sleep(Duration::from_millis(2));
            None
        })
    });
    settle.expect("no settle arrived within the bounded wait")
}

#[test]
fn describe_reports_metadata() {
    let (_vtable, mut service) = service_with_host(Box::new(|_, _| Err("unused".to_owned())));
    let reply = service.serve(METHOD_DESCRIBE, b"null").expect("describe");
    let parsed: Value = serde_json::from_slice(&reply).expect("json");
    assert_eq!(parsed["name"], PLUGIN_NAME);
    assert_eq!(parsed["service"], "tools");
    assert_eq!(parsed["abiVersion"], 4);
}

#[test]
fn unknown_method_fails_with_text() {
    let (_vtable, mut service) = service_with_host(Box::new(|_, _| Err("unused".to_owned())));
    let error = service.serve(42, b"null").expect_err("unknown");
    assert!(
        error.0.contains("unknown fr-tools method 42"),
        "{}",
        error.0
    );
}

#[test]
fn register_without_seam_fails() {
    let events = Arc::new(ToolEvents::default());
    let registry = ToolRegistry::new(
        ToolPresentationMode::Native,
        Arc::clone(&events) as Arc<dyn crate::registry::ToolsEmitter>,
    );
    let mut service = ToolsService::new(registry, events);
    let request = RegisterListenerRequest {
        stage: WireStage::PreExecute,
        endpoint: WireEndpoint {
            service: "policy".to_owned(),
            method: 7,
        },
    };
    let error = service
        .serve(
            METHOD_REGISTER_LISTENER,
            serde_json::to_vec(&request).unwrap().as_slice(),
        )
        .expect_err("no seam");
    assert!(error.0.contains("no host seam attached"), "{}", error.0);
}

#[test]
fn pre_execute_endpoint_delegates_then_transforms() {
    // The endpoint answers Delegate on enter, then denies on the exit leg
    // with its own reason; a second registered in-process listener
    // proves the delegate ran the remaining chain.
    let (_vtable, mut service) = service_with_host(Box::new(|method, payload| {
        assert_eq!(method, 9, "endpoint method selector");
        let text = String::from_utf8_lossy(payload).into_owned();
        if text.contains("\"enter\"") {
            let frame: PreEnter = serde_json::from_str(&text).expect("enter frame");
            assert_eq!(frame.stage.as_str(), "pre-execute");
            Ok(serde_json::to_vec(&PreReply::Delegate).unwrap())
        } else {
            let frame: PreExit = serde_json::from_str(&text).expect("exit frame");
            let downstream = frame.decision;
            assert!(
                matches!(downstream, crate::types::PreToolDecision::Allow),
                "the remaining chain ran and allowed"
            );
            Ok(serde_json::to_vec(&PreReply::Final {
                decision: crate::types::PreToolDecision::Deny {
                    reason: "policy says no".to_owned(),
                    info: None,
                },
            })
            .unwrap())
        }
    }));
    register(&service, echo_tool("echo"));
    let request = RegisterListenerRequest {
        stage: WireStage::PreExecute,
        endpoint: WireEndpoint {
            service: "policy".to_owned(),
            method: 9,
        },
    };
    service
        .serve(
            METHOD_REGISTER_LISTENER,
            serde_json::to_vec(&request).unwrap().as_slice(),
        )
        .expect("register");
    let reply = service
        .serve(METHOD_PREPARE, &prepare_body("echo"))
        .expect("prepare");
    let parsed: Value = serde_json::from_slice(&reply).expect("json");
    assert_eq!(parsed["kind"], "post-result", "{parsed}");
    assert!(
        parsed["result"]["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("policy says no")),
        "{parsed}"
    );
}

#[test]
fn pre_execute_endpoint_veto_skips_rest() {
    // A final answer on the enter leg must skip the remaining chain; a
    // would-be second listener (an in-process one that panics if run)
    // proves the veto.
    let (_vtable, mut service) = service_with_host(Box::new(|_method, payload| {
        let text = String::from_utf8_lossy(payload).into_owned();
        assert!(text.contains("\"enter\""), "no exit leg may arrive");
        Ok(serde_json::to_vec(&PreReply::Final {
            decision: crate::types::PreToolDecision::Allow,
        })
        .unwrap())
    }));
    register(&service, echo_tool("echo"));
    // The endpoint registers FIRST (chain order = registration order), so
    // its final answer vetoes the would-run listener behind it.
    let request = RegisterListenerRequest {
        stage: WireStage::PreExecute,
        endpoint: WireEndpoint {
            service: "policy".to_owned(),
            method: 9,
        },
    };
    service
        .serve(
            METHOD_REGISTER_LISTENER,
            serde_json::to_vec(&request).unwrap().as_slice(),
        )
        .expect("register");
    let _rest = service.registry().on_pre_execute(Arc::new(|_exec, _next| {
        panic!("a vetoed chain must not run its remaining listeners")
    }));
    let reply = service
        .serve(METHOD_PREPARE, &prepare_body("echo"))
        .expect("prepare");
    let parsed: Value = serde_json::from_slice(&reply).expect("json");
    assert_eq!(parsed["kind"], "dispatch", "{parsed}");
}

#[test]
fn pre_execute_endpoint_failure_denies() {
    let (_vtable, mut service) =
        service_with_host(Box::new(|_, _| Err("policy channel is down".to_owned())));
    register(&service, echo_tool("echo"));
    let request = RegisterListenerRequest {
        stage: WireStage::PreExecute,
        endpoint: WireEndpoint {
            service: "policy".to_owned(),
            method: 9,
        },
    };
    service
        .serve(
            METHOD_REGISTER_LISTENER,
            serde_json::to_vec(&request).unwrap().as_slice(),
        )
        .expect("register");
    let reply = service
        .serve(METHOD_PREPARE, &prepare_body("echo"))
        .expect("prepare");
    let parsed: Value = serde_json::from_slice(&reply).expect("json");
    assert!(
        parsed["result"]["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("policy channel is down")),
        "endpoint failure must fail closed: {parsed}"
    );
}

#[test]
fn execute_endpoint_arms_deadline_and_replaces() {
    // A timeout-shaped policy over the wire: arm {1ms, TEST_TIMEOUT},
    // replace the result when the deadline's own code fired.
    let (_vtable, mut service) = service_with_host(Box::new(|method, payload| {
        assert_eq!(method, 9);
        let text = String::from_utf8_lossy(payload).into_owned();
        if text.contains("\"enter\"") {
            let frame: ExecEnter = serde_json::from_str(&text).expect("enter frame");
            assert_eq!(
                frame.timeout_ms, None,
                "the tool declares no budget; the policy owns the arm"
            );
            Ok(serde_json::to_vec(&ExecReply::Delegate {
                arm_deadline: Some(super::service::ArmDeadline {
                    ms: 1.0,
                    code: "TEST_TIMEOUT".to_owned(),
                }),
            })
            .unwrap())
        } else {
            let frame: ExecExit = serde_json::from_str(&text).expect("exit frame");
            let deadline: DeadlineState = frame.deadline.expect("deadline state reported");
            assert!(deadline.fired, "the 1ms deadline beat the 80ms body");
            assert_eq!(deadline.timeout_code.as_deref(), Some("TEST_TIMEOUT"));
            let replacement =
                crate::types::ToolExecutionResult::Failure(crate::types::ToolExecutionFailure {
                    content: text_content("Error: timed out by policy"),
                    error: crate::types::ToolFailure {
                        message: "timed out by policy".to_owned(),
                        info: None,
                    },
                    meta: None,
                    additional_contexts: Vec::new(),
                });
            Ok(serde_json::to_vec(&ExecReply::Final {
                outcome: WireOutcome::Authored,
                result: replacement,
            })
            .unwrap())
        }
    }));
    register(&service, sleeping_tool("slow", None, 80));
    let request = RegisterListenerRequest {
        stage: WireStage::Execute,
        endpoint: WireEndpoint {
            service: "policy".to_owned(),
            method: 9,
        },
    };
    service
        .serve(
            METHOD_REGISTER_LISTENER,
            serde_json::to_vec(&request).unwrap().as_slice(),
        )
        .expect("register");
    let reply = service
        .serve(METHOD_EXECUTE, &prepare_body("slow"))
        .expect("execute");
    let parsed: Value = serde_json::from_slice(&reply).expect("json");
    assert!(
        parsed["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("timed out by policy")),
        "{parsed}"
    );
}

#[test]
fn execute_endpoint_plain_delegate_passes_through() {
    let (_vtable, mut service) = service_with_host(Box::new(|_method, payload| {
        let text = String::from_utf8_lossy(payload).into_owned();
        if text.contains("\"enter\"") {
            Ok(serde_json::to_vec(&ExecReply::Delegate { arm_deadline: None }).unwrap())
        } else {
            let frame: ExecExit = serde_json::from_str(&text).expect("exit frame");
            Ok(serde_json::to_vec(&ExecReply::Final {
                outcome: WireOutcome::Canonical,
                result: frame.result,
            })
            .unwrap())
        }
    }));
    register(&service, echo_tool("echo"));
    let request = RegisterListenerRequest {
        stage: WireStage::Execute,
        endpoint: WireEndpoint {
            service: "policy".to_owned(),
            method: 9,
        },
    };
    service
        .serve(
            METHOD_REGISTER_LISTENER,
            serde_json::to_vec(&request).unwrap().as_slice(),
        )
        .expect("register");
    let reply = service
        .serve(METHOD_EXECUTE, &prepare_body("echo"))
        .expect("execute");
    let parsed: Value = serde_json::from_slice(&reply).expect("json");
    assert_eq!(parsed["kind"], "success", "{parsed}");
}

#[test]
fn post_execute_endpoint_folds_contexts() {
    let (_vtable, mut service) = service_with_host(Box::new(|method, payload| {
        assert_eq!(method, 9);
        let text = String::from_utf8_lossy(payload).into_owned();
        if text.contains("\"enter\"") {
            let frame: PostEnter = serde_json::from_str(&text).expect("enter frame");
            assert!(matches!(
                frame.result,
                crate::types::ToolExecutionResult::Success(_)
            ));
            Ok(serde_json::to_vec(&PostReply::Delegate).unwrap())
        } else {
            let _frame: PostExit = serde_json::from_str(&text).expect("exit frame");
            Ok(serde_json::to_vec(&PostReply::Final {
                decision: crate::types::PostToolDecision::Accept {
                    content: None,
                    value: None,
                    additional_contexts: vec![fr_wire::Message::Known(Box::new(
                        fr_wire::message::KnownMessage::User {
                            id: fr_wire::MessageId::new("reminder-1"),
                            content: text_content("reminder"),
                            source: fr_wire::message::MessageSource::Other(serde_json::Map::new()),
                            extra: serde_json::Map::new(),
                        },
                    ))],
                },
            })
            .unwrap())
        }
    }));
    register(&service, echo_tool("echo"));
    let request = RegisterListenerRequest {
        stage: WireStage::PostExecute,
        endpoint: WireEndpoint {
            service: "policy".to_owned(),
            method: 9,
        },
    };
    service
        .serve(
            METHOD_REGISTER_LISTENER,
            serde_json::to_vec(&request).unwrap().as_slice(),
        )
        .expect("register");
    let reply = service
        .serve(METHOD_EXECUTE, &prepare_body("echo"))
        .expect("execute");
    let parsed: Value = serde_json::from_slice(&reply).expect("json");
    assert_eq!(
        parsed["additionalContexts"].as_array().map(Vec::len),
        Some(1),
        "{parsed}"
    );
}

#[test]
fn segmented_flow_settles_and_finalizes() {
    let (vtable, mut service) = service_with_host(Box::new(|_, _| Err("unused".to_owned())));
    register(&service, echo_tool("echo"));
    let reply = service
        .serve(METHOD_PREPARE, &prepare_body("echo"))
        .expect("prepare");
    let parsed: Value = serde_json::from_slice(&reply).expect("json");
    assert_eq!(parsed["kind"], "dispatch");
    let token = parsed["exec"]["token"].as_u64().expect("token");
    service
        .serve(
            METHOD_DISPATCH,
            json!({ "token": token }).to_string().as_bytes(),
        )
        .expect("dispatch enqueue");
    let settle = wait_settled(&mut service);
    assert_eq!(settle.token, token);
    assert!(
        matches!(settle.kind, super::service::WireSettleKind::PostResult),
        "an allowed call settles for post-execute"
    );
    let finalize = service
        .serve(
            METHOD_FINALIZE,
            json!({ "token": token, "result": settle.result })
                .to_string()
                .as_bytes(),
        )
        .expect("finalize");
    let parsed: Value = serde_json::from_slice(&finalize).expect("json");
    assert_eq!(parsed["kind"], "success", "{parsed}");
    // The token left the in-flight map with the finished call.
    let again = service.serve(
        METHOD_FINALIZE,
        json!({ "token": token, "result": parsed })
            .to_string()
            .as_bytes(),
    );
    assert!(again.is_err(), "finished tokens are unknown");
    service.join_workers();
    let emits = vtable.emits();
    let names: Vec<&str> = emits.iter().map(|(name, _)| name.as_str()).collect();
    assert!(names.contains(&"tools/settled"), "{names:?}");
    assert!(names.contains(&"tools/result"), "{names:?}");
}

#[test]
fn cancel_overrides_a_pending_call() {
    let (_vtable, mut service) = service_with_host(Box::new(|_, _| Err("unused".to_owned())));
    register(&service, echo_tool("echo"));
    let reply = service
        .serve(METHOD_PREPARE, &prepare_body("echo"))
        .expect("prepare");
    let parsed: Value = serde_json::from_slice(&reply).expect("json");
    let token = parsed["exec"]["token"].as_u64().expect("token");
    let cancel = service
        .serve(
            METHOD_CANCEL,
            json!({ "token": token }).to_string().as_bytes(),
        )
        .expect("cancel");
    let cancel: Value = serde_json::from_slice(&cancel).expect("json");
    assert_eq!(cancel["aborted"], true);
    service
        .serve(
            METHOD_DISPATCH,
            json!({ "token": token }).to_string().as_bytes(),
        )
        .expect("dispatch");
    let settle = wait_settled(&mut service);
    let error = match &settle.result {
        crate::types::ToolExecutionResult::Failure(failure) => failure.error.message.clone(),
        crate::types::ToolExecutionResult::Success(_) => String::new(),
    };
    assert!(
        error.contains("aborted"),
        "a cancelled call must surface the cancellation: {error}"
    );
    service.join_workers();
}

#[test]
fn execution_mode_and_wire_schemas_answer() {
    let (_vtable, mut service) = service_with_host(Box::new(|_, _| Err("unused".to_owned())));
    register(&service, echo_tool("echo"));
    let mode = service
        .serve(
            METHOD_EXECUTION_MODE,
            json!({ "name": "echo", "arguments": { "value": "x" }, "nested": false })
                .to_string()
                .as_bytes(),
        )
        .expect("mode");
    let mode: ToolExecutionMode = serde_json::from_slice(&mode).expect("mode decode");
    assert_eq!(mode, ToolExecutionMode::Exclusive);
    let schemas = service
        .serve(METHOD_WIRE_SCHEMAS, b"null")
        .expect("schemas");
    let schemas: Value = serde_json::from_slice(&schemas).expect("json");
    assert!(
        schemas["schemas"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty())
    );
}

#[test]
fn unregister_listener_detaches() {
    let (_vtable, mut service) = service_with_host(Box::new(|_, _| Err("unused".to_owned())));
    register(&service, echo_tool("echo"));
    let request = RegisterListenerRequest {
        stage: WireStage::PreExecute,
        endpoint: WireEndpoint {
            service: "policy".to_owned(),
            method: 9,
        },
    };
    let reply = service
        .serve(
            METHOD_REGISTER_LISTENER,
            serde_json::to_vec(&request).unwrap().as_slice(),
        )
        .expect("register");
    let reply: Value = serde_json::from_slice(&reply).expect("json");
    let id = reply["id"].as_u64().expect("id");
    service
        .serve(
            METHOD_UNREGISTER_LISTENER,
            json!({ "id": id }).to_string().as_bytes(),
        )
        .expect("unregister");
    // With the endpoint detached, the call dispatches again.
    let reply = service
        .serve(METHOD_PREPARE, &prepare_body("echo"))
        .expect("prepare");
    let parsed: Value = serde_json::from_slice(&reply).expect("json");
    assert_eq!(parsed["kind"], "dispatch", "{parsed}");
}

/// Installs one approval endpoint returning a fixed outcome word.
fn install_approval(service: &mut ToolsService, outcome: &str) {
    let request = json!({
        "endpoint": { "service": "approval", "method": 1 },
    });
    service
        .serve(METHOD_SET_APPROVAL, request.to_string().as_bytes())
        .expect("set-approval");
    let _ = outcome;
}

/// Registers an in-process pre-execute listener asking for approval;
/// the returned registration keeps the listener mounted (its `Drop`
/// deregisters).
fn ask_listener(service: &ToolsService) -> crate::registry::ListenerRegistration {
    service.registry().on_pre_execute(Arc::new(|_exec, _next| {
        crate::types::PreToolDecision::Ask {
            reason: Some("writes outside the workspace".to_owned()),
            display_reason: Some("Write outside the workspace?".to_owned()),
        }
    }))
}

#[test]
fn set_approval_allowed_once_allows_the_ask() {
    let asked = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let recorder = Arc::clone(&asked);
    let (_vtable, mut service) = service_with_host(Box::new(move |method, payload| {
        assert_eq!(method, 1, "approval request selector");
        let ask: Value = serde_json::from_slice(payload).expect("ask decodes");
        recorder.lock().expect("lock").push(ask);
        Ok(serde_json::to_vec(&json!({ "outcome": "allowed-once" })).unwrap())
    }));
    register(&service, echo_tool("echo"));
    install_approval(&mut service, "allowed-once");
    let _ask = ask_listener(&service);
    let reply = service
        .serve(METHOD_PREPARE, &prepare_body_with_agent("echo"))
        .expect("prepare");
    let parsed: Value = serde_json::from_slice(&reply).expect("json");
    assert_eq!(parsed["kind"], "dispatch", "{parsed}");
    let seen_asks = asked.lock().expect("lock").clone();
    assert_eq!(seen_asks.len(), 1);
    assert_eq!(seen_asks[0]["agent"], "session-1");
    assert_eq!(seen_asks[0]["tool_name"], "echo");
    assert_eq!(seen_asks[0]["call_id"], "call-1");
    assert_eq!(seen_asks[0]["reason"], "writes outside the workspace");
    assert_eq!(
        seen_asks[0]["display_reason"]["en"],
        "Write outside the workspace?"
    );
}

#[test]
fn set_approval_rejected_denies_with_the_user_text() {
    let (_vtable, mut service) = service_with_host(Box::new(|_, _| {
        Ok(serde_json::to_vec(&json!({ "outcome": "rejected" })).unwrap())
    }));
    register(&service, echo_tool("echo"));
    install_approval(&mut service, "rejected");
    let _ask = ask_listener(&service);
    let reply = service
        .serve(METHOD_PREPARE, &prepare_body_with_agent("echo"))
        .expect("prepare");
    let parsed: Value = serde_json::from_slice(&reply).expect("json");
    assert_eq!(parsed["kind"], "post-result", "{parsed}");
    assert!(
        parsed["result"]["error"]["message"]
            .as_str()
            .is_some_and(|message| message == "the user rejected tool \"echo\""),
        "{parsed}"
    );
}

#[test]
fn set_approval_transport_failure_fails_closed_unavailable() {
    let (_vtable, mut service) =
        service_with_host(Box::new(|_, _| Err("approval channel is down".to_owned())));
    register(&service, echo_tool("echo"));
    install_approval(&mut service, "unused");
    let _ask = ask_listener(&service);
    let reply = service
        .serve(METHOD_PREPARE, &prepare_body_with_agent("echo"))
        .expect("prepare");
    let parsed: Value = serde_json::from_slice(&reply).expect("json");
    assert_eq!(parsed["kind"], "post-result", "{parsed}");
    assert!(
        parsed["result"]["error"]["message"]
            .as_str()
            .is_some_and(|message| {
                message == "tool \"echo\" requires approval, but no approval channel is available"
            }),
        "{parsed}"
    );
}

#[test]
fn set_approval_rogue_outcome_word_contains_to_unavailable() {
    let (_vtable, mut service) = service_with_host(Box::new(|_, _| {
        Ok(serde_json::to_vec(&json!({ "outcome": "allowed-forever" })).unwrap())
    }));
    register(&service, echo_tool("echo"));
    install_approval(&mut service, "rogue");
    let _ask = ask_listener(&service);
    let reply = service
        .serve(METHOD_PREPARE, &prepare_body_with_agent("echo"))
        .expect("prepare");
    let parsed: Value = serde_json::from_slice(&reply).expect("json");
    assert_eq!(parsed["kind"], "post-result", "{parsed}");
    assert!(
        parsed["result"]["error"]["message"]
            .as_str()
            .is_some_and(|message| {
                message == "tool \"echo\" requires approval, but no approval channel is available"
            }),
        "a rogue word must contain to the fail-closed denial: {parsed}"
    );
}

#[test]
fn set_approval_requires_the_seam_and_absent_clears() {
    // Without an attached host seam the install refuses.
    let events = Arc::new(ToolEvents::default());
    let registry = ToolRegistry::new(
        ToolPresentationMode::Native,
        Arc::clone(&events) as Arc<dyn crate::registry::ToolsEmitter>,
    );
    let mut service = ToolsService::new(registry, events);
    let request = json!({ "endpoint": { "service": "approval", "method": 1 } });
    let error = service
        .serve(METHOD_SET_APPROVAL, request.to_string().as_bytes())
        .expect_err("no seam");
    assert!(
        error.0.contains("requires the attached host seam"),
        "{}",
        error.0
    );

    // An absent endpoint clears an installed seam: the ask degrade
    // denies with the ask's own reason (dsh `serviceAsk`: the
    // not-yet-supported text is the reasonless arm).
    let (_vtable, mut service) = service_with_host(Box::new(|_, _| {
        Ok(serde_json::to_vec(&json!({ "outcome": "allowed-once" })).unwrap())
    }));
    register(&service, echo_tool("echo"));
    install_approval(&mut service, "allowed-once");
    service
        .serve(METHOD_SET_APPROVAL, json!({}).to_string().as_bytes())
        .expect("clear");
    let _ask = ask_listener(&service);
    let reply = service
        .serve(METHOD_PREPARE, &prepare_body_with_agent("echo"))
        .expect("prepare");
    let parsed: Value = serde_json::from_slice(&reply).expect("json");
    assert_eq!(parsed["kind"], "post-result", "{parsed}");
    assert_eq!(
        parsed["result"]["error"]["message"].as_str(),
        Some("writes outside the workspace"),
        "{parsed}"
    );
}

#[test]
fn set_approval_without_an_agent_routes_nowhere() {
    let asked = Arc::new(std::sync::Mutex::new(0_u32));
    let recorder = Arc::clone(&asked);
    let (_vtable, mut service) = service_with_host(Box::new(move |_, _| {
        *recorder.lock().expect("lock") += 1;
        Ok(serde_json::to_vec(&json!({ "outcome": "allowed-once" })).unwrap())
    }));
    register(&service, echo_tool("echo"));
    install_approval(&mut service, "allowed-once");
    let _ask = ask_listener(&service);
    let reply = service
        .serve(METHOD_PREPARE, &prepare_body("echo"))
        .expect("prepare");
    let parsed: Value = serde_json::from_slice(&reply).expect("json");
    assert_eq!(parsed["kind"], "post-result", "{parsed}");
    assert!(
        parsed["result"]["error"]["message"]
            .as_str()
            .is_some_and(|message| {
                message
                    == "tool \"echo\" requires approval, but the call has no agent to route it through"
            }),
        "{parsed}"
    );
    assert_eq!(
        *asked.lock().expect("lock"),
        0,
        "an agent-less ask never reaches the seam"
    );
}

#[test]
fn service_face_constants_match_the_wire_table() {
    use fr_wire::services::tools as wire;
    assert_eq!(super::service::SERVICE_NAME, wire::SERVICE_NAME);
    assert_eq!(super::service::METHOD_DESCRIBE, wire::METHOD_DESCRIBE);
    assert_eq!(
        super::service::METHOD_REGISTER_LISTENER,
        wire::METHOD_REGISTER_LISTENER
    );
    assert_eq!(
        super::service::METHOD_UNREGISTER_LISTENER,
        wire::METHOD_UNREGISTER_LISTENER
    );
    assert_eq!(
        super::service::METHOD_EXECUTION_MODE,
        wire::METHOD_EXECUTION_MODE
    );
    assert_eq!(super::service::METHOD_PREPARE, wire::METHOD_PREPARE);
    assert_eq!(super::service::METHOD_DISPATCH, wire::METHOD_DISPATCH);
    assert_eq!(super::service::METHOD_SETTLED, wire::METHOD_SETTLED);
    assert_eq!(super::service::METHOD_FINALIZE, wire::METHOD_FINALIZE);
    assert_eq!(super::service::METHOD_FINISH, wire::METHOD_FINISH);
    assert_eq!(super::service::METHOD_EXECUTE, wire::METHOD_EXECUTE);
    assert_eq!(
        super::service::METHOD_WIRE_SCHEMAS,
        wire::METHOD_WIRE_SCHEMAS
    );
    assert_eq!(super::service::METHOD_CANCEL, wire::METHOD_CANCEL);
    assert_eq!(
        super::service::METHOD_SET_APPROVAL,
        wire::METHOD_SET_APPROVAL
    );
}

/// The wire register face mounts an endpoint-backed definition (the dsh
/// tool object over the wire): the id echoes the name, a duplicate
/// refuses with the registry text, and unregister removes it.
#[test]
fn wire_register_mounts_and_unregisters() {
    // The scripted endpoint answers the body (9) and the render (10)
    // selectors the registration names.
    let endpoint: Endpoint = Box::new(|method: u32, payload: &[u8]| match method {
        9 => {
            let request: Value =
                serde_json::from_slice(payload).map_err(|error| format!("body frame: {error}"))?;
            let name = request["call"]["name"].as_str().unwrap_or_default();
            Ok(serde_json::to_vec(&json!({ "value": { "echoed": name } })).expect("encode"))
        }
        10 => Ok(
            serde_json::to_vec(&json!([{ "type": "text", "text": "rendered" }])).expect("encode"),
        ),
        other => Err(format!("unexpected endpoint method {other}")),
    });
    let (_vtable, mut service) = service_with_host(endpoint);
    let definition = json!({
        "name": "echo",
        "description": "Echoes the call name.",
        "parameters": { "type": "object", "properties": {} },
        "output": {
            "schema": { "type": "object", "properties": {} },
            "render": { "service": "builtin", "method": 10 },
        },
        "execute": { "service": "builtin", "method": 9 },
        "isConcurrencySafe": true,
    });
    let body = serde_json::to_vec(&definition).expect("encode");
    let reply: Value = serde_json::from_slice(
        &service
            .serve(fr_wire::services::tools::METHOD_REGISTER, &body)
            .expect("register"),
    )
    .expect("decode");
    assert_eq!(reply["id"], "echo");
    assert!(
        service.registry().get("echo").is_some(),
        "the registered tool is visible"
    );
    assert_eq!(
        service
            .serve(fr_wire::services::tools::METHOD_REGISTER, &body)
            .unwrap_err()
            .0,
        "tool \"echo\" is already registered (for a per-agent variant, register through that agent's `agent.ctx` instead)"
    );
    let removed: Value = serde_json::from_slice(
        &service
            .serve(
                fr_wire::services::tools::METHOD_UNREGISTER,
                br#"{"id": "echo"}"#,
            )
            .expect("unregister"),
    )
    .expect("decode");
    assert_eq!(removed["removed"], true);
    assert!(service.registry().get("echo").is_none());
    let again: Value = serde_json::from_slice(
        &service
            .serve(
                fr_wire::services::tools::METHOD_UNREGISTER,
                br#"{"id": "echo"}"#,
            )
            .expect("unregister"),
    )
    .expect("decode");
    assert_eq!(again["removed"], false, "a second unregister is a no-op");
}

/// The wire-tool abort watcher kills through the jobs kill-by-tool-call
/// face mid-dispatch (the Q16a-6 carrier): a blocked body, an aborted
/// caller signal, and the kill request arriving on the scripted jobs
/// endpoint carrying the call id and the dsh stop reason.
#[test]
fn abort_watcher_kills_through_kill_by_tool_call() {
    use fr_wire::services::jobs as wire_jobs;
    let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let kills = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let endpoint_started = Arc::clone(&started);
    let endpoint_released = Arc::clone(&released);
    let endpoint_kills = Arc::clone(&kills);
    let endpoint: Endpoint = Box::new(move |method: u32, payload: &[u8]| match method {
        // The blocked body: hold until the watcher's kill lands.
        9 => {
            endpoint_started.store(true, std::sync::atomic::Ordering::SeqCst);
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !endpoint_released.load(std::sync::atomic::Ordering::SeqCst)
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(serde_json::to_vec(&json!({ "value": "settled" })).expect("encode"))
        }
        // The jobs kill-by-tool-call face: record the frame, release.
        method if method == wire_jobs::METHOD_KILL_BY_TOOL_CALL => {
            let frame: Value = serde_json::from_slice(payload).expect("kill frame");
            endpoint_kills.lock().expect("kills lock").push(frame);
            endpoint_released.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(br#"{"result": "requested"}"#.to_vec())
        }
        other => Err(format!("unexpected endpoint method {other}")),
    });
    let (_vtable, mut service) = service_with_host(endpoint);
    let definition = json!({
        "name": "slow",
        "description": "Blocks until killed.",
        "parameters": { "type": "object", "properties": {} },
        "output": {
            "schema": { "type": "object", "properties": {} },
            "render": { "service": "builtin", "method": 10 },
        },
        "execute": { "service": "builtin", "method": 9 },
    });
    let registered: Value = serde_json::from_slice(
        &service
            .serve(
                fr_wire::services::tools::METHOD_REGISTER,
                &serde_json::to_vec(&definition).expect("encode"),
            )
            .expect("register"),
    )
    .expect("decode");
    assert_eq!(registered["id"], "slow");
    let prepared: Value = serde_json::from_slice(
        &service
            .serve(METHOD_PREPARE, &prepare_body("slow"))
            .expect("prepare"),
    )
    .expect("decode");
    let token = prepared["exec"]["token"].clone();
    service
        .serve(
            METHOD_DISPATCH,
            &json!({ "token": token }).to_string().into_bytes(),
        )
        .expect("dispatch");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !started.load(std::sync::atomic::Ordering::SeqCst) && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        started.load(std::sync::atomic::Ordering::SeqCst),
        "the body started"
    );
    let cancelled: Value = serde_json::from_slice(
        &service
            .serve(
                METHOD_CANCEL,
                &json!({ "token": token }).to_string().into_bytes(),
            )
            .expect("cancel"),
    )
    .expect("decode");
    assert_eq!(cancelled["aborted"], true);
    let _settle = wait_settled(&mut service);
    let recorded = kills.lock().expect("kills lock");
    assert_eq!(recorded.len(), 1, "one kill request: {recorded:?}");
    assert_eq!(recorded[0]["call_id"], "call-1");
    assert_eq!(recorded[0]["reason"], "tool call aborted");
}

/// The wire presentation endpoints ride the registration: presentCall
/// and presentResult decode their views, the null reply presents as
/// none, and invalid arguments soft-fall to the generic rendering.
#[test]
fn wire_presentation_endpoints_ride_the_registration() {
    let endpoint: Endpoint = Box::new(|method: u32, payload: &[u8]| match method {
        11 => {
            let frame: Value =
                serde_json::from_slice(payload).map_err(|error| format!("frame: {error}"))?;
            assert!(frame.get("args").is_some(), "{frame}");
            Ok(serde_json::to_vec(&json!({
                "card": "terminal",
                "title": "ls -la",
                "description": "list",
                "cwd": "/tmp",
            }))
            .expect("encode"))
        }
        12 => {
            let frame: Value =
                serde_json::from_slice(payload).map_err(|error| format!("frame: {error}"))?;
            assert!(
                frame.get("args").is_some() && frame.get("result").is_some(),
                "{frame}"
            );
            if frame["result"]["isError"] == true {
                return Ok(b"null".to_vec());
            }
            Ok(serde_json::to_vec(&json!({ "card": "generic", "title": "done" })).expect("encode"))
        }
        other => Err(format!("unexpected endpoint method {other}")),
    });
    let (_vtable, mut service) = service_with_host(endpoint);
    let definition = json!({
        "name": "presented",
        "description": "Presenting.",
        "parameters": {
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        },
        "output": {
            "schema": { "type": "object", "properties": {} },
            "render": { "service": "builtin", "method": 10 },
        },
        "execute": { "service": "builtin", "method": 9 },
        "presentCall": { "service": "builtin", "method": 11 },
        "presentResult": { "service": "builtin", "method": 12 },
    });
    service
        .serve(
            fr_wire::services::tools::METHOD_REGISTER,
            &serde_json::to_vec(&definition).expect("encode"),
        )
        .expect("register");
    let tool = service
        .registry()
        .get("presented")
        .expect("the wire tool is mounted");
    let call = tool.present_call(&json!({})).expect("the terminal view");
    let crate::presentation::ToolCallView::Terminal(terminal) = call else {
        panic!("expected the terminal card: {call:?}");
    };
    assert_eq!(terminal.title, "ls -la");
    assert_eq!(terminal.cwd.as_deref(), Some("/tmp"));
    let result = crate::types::ToolResult {
        content: Vec::new(),
        is_error: false,
        meta: None,
    };
    let view = tool
        .present_result(&json!({}), &result)
        .expect("the generic view");
    let crate::presentation::ToolResultView::Generic(generic) = view else {
        panic!("expected the generic card: {view:?}");
    };
    assert_eq!(generic.title.as_deref(), Some("done"));
    // An error result presents as the null reply: none.
    let errored = crate::types::ToolResult {
        content: Vec::new(),
        is_error: true,
        meta: None,
    };
    assert!(tool.present_result(&json!({}), &errored).is_none());
    // Invalid arguments soft-fall to none (the BuiltTool judgment).
    assert!(tool.present_call(&json!({ "nope": 1 })).is_none());
}

/// The wire content endpoints install prepared content: projectContent
/// replaces the result content before post-execute and a null
/// finalizeContent leaves it untouched (the pipeline's Option arms).
#[test]
fn wire_content_endpoints_install_prepared_content() {
    let endpoint: Endpoint = Box::new(|method: u32, payload: &[u8]| match method {
        9 => Ok(serde_json::to_vec(&json!({ "value": { "echoed": "body" } })).expect("encode")),
        13 => {
            let frame: Value =
                serde_json::from_slice(payload).map_err(|error| format!("frame: {error}"))?;
            assert_eq!(frame["call"]["name"], "echoed", "{frame}");
            assert!(frame.get("result").is_some(), "{frame}");
            Ok(
                serde_json::to_vec(&json!([{ "type": "text", "text": "projected" }]))
                    .expect("encode"),
            )
        }
        14 => Ok(b"null".to_vec()),
        other => Err(format!("unexpected endpoint method {other}")),
    });
    let (_vtable, mut service) = service_with_host(endpoint);
    let definition = json!({
        "name": "echoed",
        "description": "Echoing.",
        "parameters": { "type": "object", "properties": {} },
        "output": {
            "schema": { "type": "object", "properties": {} },
            "render": { "service": "builtin", "method": 10 },
        },
        "execute": { "service": "builtin", "method": 9 },
        "projectContent": { "service": "builtin", "method": 13 },
        "finalizeContent": { "service": "builtin", "method": 14 },
    });
    service
        .serve(
            fr_wire::services::tools::METHOD_REGISTER,
            &serde_json::to_vec(&definition).expect("encode"),
        )
        .expect("register");
    let result: Value = serde_json::from_slice(
        &service
            .serve(METHOD_EXECUTE, &prepare_body("echoed"))
            .expect("execute"),
    )
    .expect("decode");
    assert_eq!(result["kind"], "success", "{result}");
    let content = result["content"]
        .as_array()
        .unwrap_or_else(|| panic!("the success content: {result}"));
    assert!(
        content
            .iter()
            .any(|block| block["text"].as_str() == Some("projected")),
        "{result}"
    );
}
