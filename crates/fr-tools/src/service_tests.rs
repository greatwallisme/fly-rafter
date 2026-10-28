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
    METHOD_REGISTER_LISTENER, METHOD_SETTLED, METHOD_UNREGISTER_LISTENER, METHOD_WIRE_SCHEMAS,
    PLUGIN_NAME, PostEnter, PostExit, PostReply, PreEnter, PreExit, PreReply,
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
