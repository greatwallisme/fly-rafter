// Rust guideline compliant 2026-10-28
//! Registry and pipeline tests: the dsh stage-order, decision, cancellation,
//! and projection families (scenario anchors for the TOOLS contract).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use fr_timeout::DeadlineSignal;
use fr_wire::{KnownContentBlock, Message};
use serde_json::{Value, json};

use crate::definition::{DefineTool, ToolDefinition, ToolOutput};
use crate::presentation::ToolCallView;
use crate::registry::{
    ApprovalOutcome, ApprovalSeam, ToolExecuteOutcome, ToolRegistry, ToolsEmitter,
};
use crate::signal::ToolSignal;
use crate::types::{
    PostToolDecision, PreToolDecision, ToolExecution, ToolExecutionInput, ToolExecutionMode,
    ToolExecutionResult, ToolPresentationMode, ToolResult, text_content,
};

/// Records registry notifications.
#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<String>>,
}

impl ToolsEmitter for Recorder {
    fn tools_change(&self) {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push("change".into());
    }

    fn tools_result(&self, exec: &ToolExecution, result: &ToolExecutionResult) {
        let label = if result.is_error() {
            "result-error"
        } else {
            "result-success"
        };
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(format!("{label}:{}", exec.name));
    }
}

/// One scripted approval seam.
struct ScriptedApproval {
    outcome: ApprovalOutcome,
    requests: Mutex<Vec<String>>,
}

impl ApprovalSeam for ScriptedApproval {
    fn request(
        &self,
        _exec: &ToolExecution,
        _reason: Option<&str>,
        _display_reason: Option<&str>,
    ) -> ApprovalOutcome {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push("asked".into());
        self.outcome
    }
}

/// Builds a first-party echo tool through the `defineTool` counterpart.
fn echo_tool(name: &str) -> Arc<dyn ToolDefinition> {
    let tool = DefineTool {
        name: name.to_owned(),
        description: "echo the value".to_owned(),
        parameters: json!({"value": {"type": "string", "required": true}}),
        output_schema: json!({"type": "string"}),
        render: Arc::new(|_args: &Value, value: &Value| {
            text_content(value.as_str().unwrap_or_default())
        }),
        presentation_meta: None,
        execute: Arc::new(|args: &Value, _run| Ok(args["value"].clone())),
        defer_loading: false,
        timeout_ms: None,
        concurrency_safe: None,
        project_content: None,
        finalize_content: None,
        present_call: None,
        present_result: None,
    };
    Arc::new(tool.build().expect("echo tool builds"))
}

/// Builds one call input.
fn input_for(name: &str, arguments: Value) -> ToolExecutionInput {
    ToolExecutionInput {
        call_id: "call-1".to_owned(),
        root_call_id: None,
        name: name.to_owned(),
        schema: None,
        arguments,
        agent: None,
        parent: None,
        signal: ToolSignal::plain(DeadlineSignal::new()),
    }
}

/// Extracts the text of a text-only content list.
fn text_of(content: &[fr_wire::ContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            fr_wire::ContentBlock::Known(KnownContentBlock::Text { text }) => text.clone(),
            _ => String::new(),
        })
        .collect()
}

/// One user message for context attachment.
fn context_message(text: &str) -> Message {
    let message = json!({"role": "user", "id": "m1", "content": [{"type": "text", "text": text}]});
    serde_json::from_value(message).expect("user message parses")
}

#[test]
fn register_failures_are_verbatim() {
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    assert_eq!(
        registry
            .register(echo_tool("run_code"))
            .unwrap_err()
            .to_string(),
        "tool name \"run_code\" is reserved for the PTC mode presentation transport and cannot be registered or shadowed"
    );
    let bad_output = DefineTool {
        output_schema: json!({"type": "strange"}),
        ..echo_tool_fields("bad")
    };
    assert_eq!(
        bad_output.build().unwrap_err().to_string(),
        "unsupported JSON schema: schema.type must be string/number/integer/boolean/null/array/object/json, or use oneOf"
    );
    let timeout_tool = DefineTool {
        timeout_ms: Some(0.0),
        ..echo_tool_fields("slow")
    };
    assert_eq!(
        timeout_tool.build().unwrap_err().to_string(),
        "defineTool(slow): timeoutMs must be a positive finite number"
    );
    let registered = registry.register(echo_tool("echo")).expect("registers");
    assert_eq!(
        registry
            .register(echo_tool("echo"))
            .unwrap_err()
            .to_string(),
        "tool \"echo\" is already registered (for a per-agent variant, register through that agent's `agent.ctx` instead)"
    );
    drop(registered);
    assert!(registry.get("echo").is_none());
}

/// The raw builder fields of one echo-shaped tool.
fn echo_tool_fields(name: &str) -> DefineTool {
    DefineTool {
        name: name.to_owned(),
        description: "echo the value".to_owned(),
        parameters: json!({"value": {"type": "string", "required": true}}),
        output_schema: json!({"type": "string"}),
        render: Arc::new(|_args: &Value, value: &Value| {
            text_content(value.as_str().unwrap_or_default())
        }),
        presentation_meta: None,
        execute: Arc::new(|args: &Value, _run| Ok(args["value"].clone())),
        defer_loading: false,
        timeout_ms: None,
        concurrency_safe: None,
        project_content: None,
        finalize_content: None,
        present_call: None,
        present_result: None,
    }
}

#[test]
fn restrict_is_scoped_only() {
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    assert_eq!(
        registry.restrict().unwrap_err().to_string(),
        "tools.restrict() requires a scoped context (agent.ctx): a context-global restriction would mask every agent — deny the tool for the intended agent instead"
    );
}

#[test]
fn unknown_tool_is_error_result() {
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let result = registry.execute(&input_for("nope", json!({})));
    let failure = match &result {
        ToolExecutionResult::Failure(failure) => failure,
        ToolExecutionResult::Success(_) => panic!("unknown tool must fail"),
    };
    assert_eq!(text_of(&failure.content), "Error: unknown tool \"nope\"");
    assert_eq!(failure.error.message, "unknown tool \"nope\"");
    let info = failure.error.info.as_ref().expect("coded failure");
    assert_eq!(
        (info.name.as_str(), info.code.as_str()),
        ("ToolNotFoundError", "UNKNOWN_TOOL")
    );
}

#[test]
fn pipeline_runs_stages_in_order() {
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let _registration = registry.register(echo_tool("echo")).expect("registers");
    let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&order);
    let _listener = registry.on_pre_execute(Arc::new(move |_exec, next| {
        seen.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push("pre".into());
        next()
    }));
    let seen = Arc::clone(&order);
    let _listener = registry.on_execute(Arc::new(move |view, next| {
        seen.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push("execute".into());
        let result = next(view);
        ToolExecuteOutcome::Canonical(result)
    }));
    let seen_post = Arc::clone(&order);
    let _listener = registry.on_post_execute(Arc::new(move |_exec, _result, next| {
        seen_post
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push("post".into());
        next()
    }));
    let result = registry.execute(&input_for("echo", json!({"value": "hi"})));
    match &result {
        ToolExecutionResult::Success(success) => {
            assert_eq!(text_of(&success.content), "hi");
            assert_eq!(success.value, json!("hi"));
        }
        ToolExecutionResult::Failure(_) => panic!("echo must succeed"),
    }
    let order = order
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert_eq!(order, vec!["pre", "execute", "post"]);
}

#[test]
fn pre_execute_deny_short_circuits_body() {
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let _registration = registry.register(echo_tool("echo")).expect("registers");
    let body_ran = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&body_ran);
    let _listener = registry.on_execute(Arc::new(move |view, next| {
        counter.fetch_add(1, Ordering::SeqCst);
        let result = next(view);
        ToolExecuteOutcome::Canonical(result)
    }));
    let post_seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&post_seen);
    let _listener = registry.on_post_execute(Arc::new(move |exec, _result, next| {
        seen.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(exec.name.clone());
        next()
    }));
    let _listener = registry.on_pre_execute(Arc::new(|_exec, _next| PreToolDecision::Deny {
        reason: "not on my watch".to_owned(),
        info: None,
    }));
    let result = registry.execute(&input_for("echo", json!({"value": "hi"})));
    let ToolExecutionResult::Failure(failure) = &result else {
        panic!("denied call must fail")
    };
    assert_eq!(text_of(&failure.content), "Error: not on my watch");
    assert_eq!(failure.error.message, "not on my watch");
    assert_eq!(body_ran.load(Ordering::SeqCst), 0);
    // A denied call still passes post-execute.
    assert_eq!(
        post_seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len(),
        1
    );
}

#[test]
fn guards_run_only_on_allow() {
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let _registration = registry.register(echo_tool("echo")).expect("registers");
    let guard_calls = Arc::new(AtomicUsize::new(0));
    let calls = Arc::clone(&guard_calls);
    let _guard = registry.guard(Arc::new(move |_exec| {
        calls.fetch_add(1, Ordering::SeqCst);
        Some("repeated loop detected".to_owned())
    }));
    let result = registry.execute(&input_for("echo", json!({"value": "hi"})));
    let ToolExecutionResult::Failure(failure) = &result else {
        panic!("guard denial must fail")
    };
    assert_eq!(failure.error.message, "repeated loop detected");
    assert_eq!(guard_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn ask_resolves_through_approval_seam() {
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let _registration = registry.register(echo_tool("echo")).expect("registers");
    let _listener = registry.on_pre_execute(Arc::new(|_exec, _next| PreToolDecision::Ask {
        reason: Some("needs a human".to_owned()),
        display_reason: None,
    }));

    // No seam installed: the declared reason denies when present.
    let result = registry.execute(&input_for("echo", json!({"value": "hi"})));
    let ToolExecutionResult::Failure(failure) = &result else {
        panic!("ask without a seam must deny")
    };
    assert_eq!(failure.error.message, "needs a human");

    // A seam without an agent on the call: the no-agent text.
    let seam = Arc::new(ScriptedApproval {
        outcome: ApprovalOutcome::AllowedOnce,
        requests: Mutex::new(Vec::new()),
    });
    registry.set_approval(Some(Arc::clone(&seam) as Arc<dyn ApprovalSeam>));
    let result = registry.execute(&input_for("echo", json!({"value": "hi"})));
    let ToolExecutionResult::Failure(failure) = &result else {
        panic!("ask without an agent must deny")
    };
    assert_eq!(
        failure.error.message,
        "tool \"echo\" requires approval, but the call has no agent to route it through"
    );
    assert_eq!(
        seam.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len(),
        0
    );

    // An agent plus each outcome.
    let cases = [
        ApprovalOutcome::AllowedOnce,
        ApprovalOutcome::Rejected,
        ApprovalOutcome::Cancelled,
        ApprovalOutcome::Unavailable,
    ];
    for outcome in cases {
        registry.set_approval(Some(Arc::new(ScriptedApproval {
            outcome,
            requests: Mutex::new(Vec::new()),
        })));
        let mut input = input_for("echo", json!({"value": "hi"}));
        input.agent = Some("agent-1".to_owned());
        let result = registry.execute(&input);
        match (&result, outcome) {
            (ToolExecutionResult::Success(_), ApprovalOutcome::AllowedOnce) => {}
            (ToolExecutionResult::Failure(failure), _) => {
                let expected_message = match outcome {
                    ApprovalOutcome::Rejected => "the user rejected tool \"echo\"",
                    ApprovalOutcome::Cancelled => "approval for tool \"echo\" was cancelled",
                    _ => "tool \"echo\" requires approval, but no approval channel is available",
                };
                assert_eq!(
                    failure.error.message, expected_message,
                    "outcome {outcome:?}"
                );
            }
            _ => panic!("outcome pairing must match, got {outcome:?}"),
        }
    }
    registry.set_approval(None);
}

#[test]
fn caller_abort_selects_before_dispatch() {
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let _registration = registry.register(echo_tool("echo")).expect("registers");
    let caller = DeadlineSignal::new();
    let input = ToolExecutionInput {
        signal: ToolSignal::plain(caller.clone()),
        ..input_for("echo", json!({"value": "hi"}))
    };
    caller.abort_upstream();
    let result = registry.execute(&input);
    let ToolExecutionResult::Failure(failure) = &result else {
        panic!("aborted call must fail")
    };
    assert_eq!(
        text_of(&failure.content),
        "Error: tool call aborted before dispatch"
    );
    let info = failure.error.info.as_ref().expect("coded");
    assert_eq!(info.code, "ABORTED_BEFORE_DISPATCH");
}

#[test]
fn caller_abort_after_body_supersedes_success() {
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let _registration = registry.register(echo_tool("echo")).expect("registers");
    let caller = DeadlineSignal::new();
    let caller_clone = caller.clone();
    // A wrapper aborts the caller AFTER the body settles, then restores its
    // signal: cancellation supersedes the successful started outcome.
    let _listener = registry.on_execute(Arc::new(move |view, next| {
        let restored = Arc::clone(&view.signal);
        view.signal = ToolSignal::plain(DeadlineSignal::new());
        let result = next(view);
        view.signal = restored;
        caller_clone.abort_upstream();
        ToolExecuteOutcome::Canonical(result)
    }));
    let input = ToolExecutionInput {
        signal: ToolSignal::plain(caller),
        ..input_for("echo", json!({"value": "hi"}))
    };
    let result = registry.execute(&input);
    let ToolExecutionResult::Failure(failure) = &result else {
        panic!("superseded call must fail")
    };
    assert_eq!(text_of(&failure.content), "Error: tool call aborted");
    let info = failure.error.info.as_ref().expect("coded");
    assert_eq!(info.code, "ABORTED");
}

#[test]
fn authored_success_is_re_rendered() {
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let _registration = registry.register(echo_tool("echo")).expect("registers");
    // An authored success with a DIFFERENT value re-renders through the
    // output contract; authored content is replaced by the render.
    let _listener = registry.on_execute(Arc::new(|view, next| {
        let _ = next(view);
        ToolExecuteOutcome::Authored(authored_success("replaced"))
    }));
    let result = registry.execute(&input_for("echo", json!({"value": "hi"})));
    match &result {
        ToolExecutionResult::Success(success) => {
            assert_eq!(success.value, json!("replaced"));
            assert_eq!(text_of(&success.content), "replaced");
        }
        ToolExecutionResult::Failure(_) => panic!("authored success must normalize"),
    }
}

/// One authored success carrying rendered content and a value.
fn authored_success(value: &str) -> ToolExecutionResult {
    ToolExecutionResult::Success(crate::types::ToolExecutionSuccess {
        value: json!(value),
        content: text_content("stale content"),
        meta: None,
        additional_contexts: Vec::new(),
        concludes_turn: false,
    })
}

#[test]
fn post_execute_decisions_apply_verbatim() {
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let _registration = registry.register(echo_tool("echo")).expect("registers");

    // block: feedback becomes the error content and message; deferred
    // contexts are discarded, explicit decision contexts survive.
    let _listener =
        registry.on_post_execute(Arc::new(|_exec, _result, _next| PostToolDecision::Block {
            feedback: text_content("do it differently"),
            additional_contexts: vec![context_message("policy hint")],
        }));
    let result = registry.execute(&input_for("echo", json!({"value": "hi"})));
    let ToolExecutionResult::Failure(failure) = &result else {
        panic!("block must fail")
    };
    assert_eq!(text_of(&failure.content), "do it differently");
    assert_eq!(failure.error.message, "do it differently");
    assert_eq!(failure.additional_contexts.len(), 1);

    // accept with content: the content is replaced.
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let _registration = registry.register(echo_tool("echo")).expect("registers");
    let _listener =
        registry.on_post_execute(Arc::new(|_exec, _result, _next| PostToolDecision::Accept {
            content: Some(text_content("rewritten")),
            value: None,
            additional_contexts: vec![context_message("after")],
        }));
    let result = registry.execute(&input_for("echo", json!({"value": "hi"})));
    let ToolExecutionResult::Success(success) = &result else {
        panic!("accept must keep success")
    };
    assert_eq!(text_of(&success.content), "rewritten");
    assert_eq!(success.additional_contexts.len(), 1);

    // accept with value: re-rendered through the contract.
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let _registration = registry.register(echo_tool("echo")).expect("registers");
    let _listener =
        registry.on_post_execute(Arc::new(|_exec, _result, _next| PostToolDecision::Accept {
            content: None,
            value: Some(json!("policy value")),
            additional_contexts: Vec::new(),
        }));
    let result = registry.execute(&input_for("echo", json!({"value": "hi"})));
    let ToolExecutionResult::Success(success) = &result else {
        panic!("value replace must keep success")
    };
    assert_eq!(success.value, json!("policy value"));
    assert_eq!(text_of(&success.content), "policy value");

    // both replacements: the dsh TypeError.
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let _registration = registry.register(echo_tool("echo")).expect("registers");
    let _listener =
        registry.on_post_execute(Arc::new(|_exec, _result, _next| PostToolDecision::Accept {
            content: Some(text_content("a")),
            value: Some(json!("b")),
            additional_contexts: Vec::new(),
        }));
    let result = registry.execute(&input_for("echo", json!({"value": "hi"})));
    let ToolExecutionResult::Failure(failure) = &result else {
        panic!("both replacements must fail")
    };
    assert_eq!(
        failure.error.message,
        "tools/post-execute accept decision cannot replace both value and content"
    );

    // value replacement on a failed result: the dsh TypeError.
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let _registration = registry.register(echo_tool("echo")).expect("registers");
    let _listener = registry.on_pre_execute(Arc::new(|_exec, _next| PreToolDecision::Deny {
        reason: "no".to_owned(),
        info: None,
    }));
    let _listener =
        registry.on_post_execute(Arc::new(|_exec, _result, _next| PostToolDecision::Accept {
            content: None,
            value: Some(json!("b")),
            additional_contexts: Vec::new(),
        }));
    let result = registry.execute(&input_for("echo", json!({"value": "hi"})));
    let ToolExecutionResult::Failure(failure) = &result else {
        panic!("value-on-failure must fail")
    };
    assert_eq!(
        failure.error.message,
        "tools/post-execute cannot replace the value of a failed result"
    );
}

#[test]
fn block_uses_fallback_message_for_empty_feedback() {
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let _registration = registry.register(echo_tool("echo")).expect("registers");
    let _listener =
        registry.on_post_execute(Arc::new(|_exec, _result, _next| PostToolDecision::Block {
            feedback: Vec::new(),
            additional_contexts: Vec::new(),
        }));
    let result = registry.execute(&input_for("echo", json!({"value": "hi"})));
    let ToolExecutionResult::Failure(failure) = &result else {
        panic!("block must fail")
    };
    assert_eq!(
        failure.error.message,
        "tool result blocked by post-execute policy"
    );
}

#[test]
fn body_errors_and_panics_materialize() {
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let failing = DefineTool {
        execute: Arc::new(|_args, _run| Err(crate::types::ToolError::plain("disk on fire"))),
        ..echo_tool_fields("failing")
    };
    let _registration = registry
        .register(Arc::new(failing.build().expect("builds")))
        .expect("registers");
    let result = registry.execute(&input_for("failing", json!({"value": "x"})));
    let ToolExecutionResult::Failure(failure) = &result else {
        panic!("failing body must fail")
    };
    assert_eq!(text_of(&failure.content), "Error: disk on fire");
    assert!(failure.error.info.is_none());

    let panicking = DefineTool {
        execute: Arc::new(|_args, _run| -> Result<Value, _> {
            panic!("boom must reach the pipeline")
        }),
        ..echo_tool_fields("panicking")
    };
    let _registration = registry
        .register(Arc::new(panicking.build().expect("builds")))
        .expect("registers");
    let result = registry.execute(&input_for("panicking", json!({"value": "x"})));
    let ToolExecutionResult::Failure(failure) = &result else {
        panic!("panicking body must fail contained")
    };
    assert_eq!(
        text_of(&failure.content),
        "Error: boom must reach the pipeline"
    );
}

#[test]
fn invalid_output_fails_the_call() {
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let lying = DefineTool {
        execute: Arc::new(|_args, _run| Ok(json!({"not": "a string"}))),
        ..echo_tool_fields("lying")
    };
    let _registration = registry
        .register(Arc::new(lying.build().expect("builds")))
        .expect("registers");
    let result = registry.execute(&input_for("lying", json!({"value": "x"})));
    let ToolExecutionResult::Failure(failure) = &result else {
        panic!("invalid output must fail")
    };
    assert_eq!(
        failure.error.message,
        "tool \"lying\" returned invalid output: \"value\" must be a string"
    );
    let info = failure.error.info.as_ref().expect("coded");
    assert_eq!(info.code, "INVALID_TOOL_OUTPUT");
}

#[test]
fn invalid_args_fail_before_body() {
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let _registration = registry.register(echo_tool("echo")).expect("registers");
    let result = registry.execute(&input_for("echo", json!({"value": 3})));
    let ToolExecutionResult::Failure(failure) = &result else {
        panic!("invalid args must fail")
    };
    assert_eq!(
        failure.error.message,
        "invalid arguments: \"value\" must be a string"
    );
    let info = failure.error.info.as_ref().expect("coded");
    assert_eq!(info.code, "INVALID_ARGS");
}

#[test]
fn deferred_contexts_and_conclusion_ride_results() {
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let concluding = DefineTool {
        execute: Arc::new(|_args, run| {
            run.defer_context(context_message("nested note"));
            run.conclude_turn();
            Ok(json!("done"))
        }),
        ..echo_tool_fields("concluding")
    };
    let _registration = registry
        .register(Arc::new(concluding.build().expect("builds")))
        .expect("registers");
    let result = registry.execute(&input_for("concluding", json!({"value": "x"})));
    let ToolExecutionResult::Success(success) = &result else {
        panic!("concluding body must succeed")
    };
    assert!(success.concludes_turn);
    assert_eq!(success.additional_contexts.len(), 1);
}

#[test]
fn wire_schemas_follow_mode() {
    let recorder = Arc::new(Recorder::default());
    let registry = ToolRegistry::new(
        ToolPresentationMode::Native,
        Arc::clone(&recorder) as Arc<dyn ToolsEmitter>,
    );
    let deferred = DefineTool {
        defer_loading: true,
        ..echo_tool_fields("lazy")
    };
    let _registration = registry
        .register(Arc::new(deferred.build().expect("builds")))
        .expect("registers");
    let (schemas, known) = registry.wire_schemas().expect("native wire");
    assert_eq!(schemas.len(), 1);
    assert_eq!(schemas[0].name, "lazy");
    assert_eq!(schemas[0].defer_loading, Some(true));
    assert_eq!(known, vec!["lazy".to_owned()]);

    let ptc = ToolRegistry::new(ToolPresentationMode::Ptc, recorder);
    let _registration = ptc.register(echo_tool("echo")).expect("registers");
    assert_eq!(
        ptc.wire_schemas().unwrap_err(),
        "dsh-tools: mode \"ptc\" requires a PTC runtime — load a ctx.ptcRuntime implementation (e.g. @deepseek-ai/dsh-ptc-runtime-node) or set tools mode to \"native\""
    );
    ptc.set_ptc_transport(Some(echo_transport()));
    let (schemas, known) = ptc.wire_schemas().expect("ptc wire");
    assert_eq!(schemas.len(), 1);
    assert_eq!(schemas[0].name, "run_code");
    assert_eq!(known, vec!["run_code".to_owned()]);
    // The collapse denies a model-direct native call with the route text.
    let result = ptc.execute(&input_for("echo", json!({"value": "x"})));
    let ToolExecutionResult::Failure(failure) = &result else {
        panic!("collapsed call must fail")
    };
    assert_eq!(
        failure.error.message,
        "unknown tool \"echo\": only `run_code` is callable directly — call `echo` from inside a `run_code` program instead"
    );
}

/// A transport-shaped definition carrying the reserved name (the Phase 17
/// runtime installs the real one).
fn echo_transport() -> Arc<dyn ToolDefinition> {
    struct Transport {
        parameters: Value,
        output: ToolOutput,
    }
    impl ToolDefinition for Transport {
        fn name(&self) -> &'static str {
            "run_code"
        }
        fn description(&self) -> &'static str {
            "run a program"
        }
        fn parameters(&self) -> &Value {
            &self.parameters
        }
        fn output(&self) -> &ToolOutput {
            &self.output
        }
        fn execute(
            &self,
            _args: &Value,
            _exec: &ToolExecution,
            _signal: Arc<ToolSignal>,
            _run: &mut crate::definition::ToolRunContext<'_>,
        ) -> Result<Value, crate::types::ToolError> {
            Ok(json!("transport"))
        }
    }
    Arc::new(Transport {
        parameters: json!({"program": {"type": "string", "required": true}}),
        output: ToolOutput {
            schema: json!({"type": "string"}),
            render: Arc::new(|_args: &Value, value: &Value| {
                text_content(value.as_str().unwrap_or_default())
            }),
            presentation_meta: None,
        },
    })
}

#[test]
fn execution_mode_classifies_fail_closed() {
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let _registration = registry.register(echo_tool("echo")).expect("registers");
    let parallel = DefineTool {
        concurrency_safe: Some(Arc::new(|args| args.get("value").is_some())),
        ..echo_tool_fields("parallel")
    };
    let _registration = registry
        .register(Arc::new(parallel.build().expect("builds")))
        .expect("registers");
    assert_eq!(
        registry.execution_mode("unknown", &json!({}), false),
        ToolExecutionMode::Exclusive
    );
    assert_eq!(
        registry.execution_mode("echo", &json!({"value": "x"}), false),
        ToolExecutionMode::Exclusive
    );
    assert_eq!(
        registry.execution_mode("parallel", &json!({"value": "x"}), false),
        ToolExecutionMode::Parallel
    );
    assert_eq!(
        registry.execution_mode("parallel", &json!({}), false),
        ToolExecutionMode::Exclusive
    );
}

#[test]
fn presentation_hooks_soft_wrap() {
    let fields = echo_tool_fields("presented");
    let built = fields.build().expect("builds");
    // Invalid args fall back to the generic presentation.
    assert!(built.present_call(&json!({"value": 3})).is_none());
    // A valid call renders the declared view.
    let presenting = DefineTool {
        present_call: Some(Arc::new(|_args| {
            Some(ToolCallView::Generic(
                crate::presentation::GenericCallView {
                    title: "Echoing".to_owned(),
                    kind: Some(crate::presentation::ToolCallKind::Read),
                    raw_input: None,
                    content: None,
                    locations: None,
                },
            ))
        })),
        ..echo_tool_fields("presented")
    };
    let built = presenting.build().expect("builds");
    let view = built.present_call(&json!({"value": "x"})).expect("view");
    assert!(matches!(view, ToolCallView::Generic(_)));
    // presentResult narrows the durable projection.
    let result = ToolResult {
        content: text_content("x"),
        is_error: false,
        meta: None,
    };
    assert!(
        built
            .present_result(&json!({"value": "x"}), &result)
            .is_none()
    );
}

/// The registration desk keeps every declared surface retrievable:
/// identity, description, parameters schema, execution mode, and the
/// presentation hooks (the wire schema projects the model-facing subset;
/// tool-description section ordering 1000-3000 is owned by the prompt
/// assembly and pinned on that side).
#[test]
fn registered_metadata_is_complete() {
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let presenting = DefineTool {
        description: "echo the value twice".to_owned(),
        concurrency_safe: Some(Arc::new(|args| args.get("value").is_some())),
        present_call: Some(Arc::new(|_args| {
            Some(ToolCallView::Generic(
                crate::presentation::GenericCallView {
                    title: "Echoing".to_owned(),
                    kind: Some(crate::presentation::ToolCallKind::Read),
                    raw_input: None,
                    content: None,
                    locations: None,
                },
            ))
        })),
        ..echo_tool_fields("echo")
    };
    let built = Arc::new(presenting.build().expect("builds"));
    assert!(
        built.present_call(&json!({"value": "x"})).is_some(),
        "the presentation hook stays mounted"
    );
    let _registration = registry.register(built).expect("registers");

    let definition = registry.get("echo").expect("registered");
    assert_eq!(definition.name(), "echo");
    assert_eq!(definition.description(), "echo the value twice");
    assert!(definition.parameters().is_object());
    assert_eq!(
        registry.execution_mode("echo", &json!({"value": "x"}), false),
        ToolExecutionMode::Parallel
    );
    let schemas = registry.schemas();
    assert_eq!(schemas.len(), 1);
    assert_eq!(schemas[0].name, "echo");
    assert_eq!(schemas[0].description, "echo the value twice");
    assert!(schemas[0].parameters.is_object());
    assert_eq!(schemas[0].defer_loading, None);
}

/// The arguments a call executes with are the snapshot frozen at
/// createExecution: every stage after materialization (pre-execute, the
/// body) reads one cloned value (dsh freezes the JSON snapshot; the
/// clone at the registry boundary is the same freeze — Rust ownership
/// already rules out the caller-side mutation dsh defends against).
#[test]
fn arguments_snapshot_is_frozen() {
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let pre_seen = Arc::clone(&seen);
    let _listener = registry.on_pre_execute(Arc::new(move |exec, _next| {
        pre_seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(exec.arguments.clone());
        PreToolDecision::Allow
    }));
    let body_seen = Arc::clone(&seen);
    let tool = DefineTool {
        execute: Arc::new(move |args, _run| {
            body_seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(args.clone());
            Ok(Value::String("ok".to_owned()))
        }),
        ..echo_tool_fields("echo")
    };
    let _registration = registry
        .register(Arc::new(tool.build().expect("builds")))
        .expect("registers");
    let result = registry.execute(&input_for("echo", json!({"value": "frozen"})));
    assert!(!result.is_error());
    let seen = seen
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(
        *seen,
        vec![json!({"value": "frozen"}), json!({"value": "frozen"})],
        "pre-execute and the body see the same frozen snapshot"
    );
}

/// Guards evaluate only when the pre-execute gate lets the call proceed:
/// neither a deny nor an ask that resolves short of `allowed-once` ever
/// reaches them, and on the allow path the first denial in registration
/// order settles the call — a later guard is not even consulted (no
/// relaxation back to allow).
#[test]
fn guards_skip_non_allow_and_stay_monotonic() {
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let _registration = registry.register(echo_tool("echo")).expect("registers");
    let denials = Arc::new(AtomicUsize::new(0));
    let deny_count = Arc::clone(&denials);
    let _guard = registry.guard(Arc::new(move |_exec| {
        deny_count.fetch_add(1, Ordering::SeqCst);
        Some("loop detected".to_owned())
    }));
    let silent = Arc::new(AtomicUsize::new(0));
    let silent_count = Arc::clone(&silent);
    let _guard = registry.guard(Arc::new(move |_exec| {
        silent_count.fetch_add(1, Ordering::SeqCst);
        None
    }));

    // A pre-execute deny never reaches the guards.
    {
        let _listener = registry.on_pre_execute(Arc::new(|_exec, _next| PreToolDecision::Deny {
            reason: "not allowed".to_owned(),
            info: None,
        }));
        let result = registry.execute(&input_for("echo", json!({"value": "hi"})));
        assert!(result.is_error());
    }
    assert_eq!(denials.load(Ordering::SeqCst), 0);
    assert_eq!(silent.load(Ordering::SeqCst), 0);

    // An ask that resolves rejected also never reaches the guards.
    {
        let _listener = registry.on_pre_execute(Arc::new(|_exec, _next| PreToolDecision::Ask {
            reason: Some("needs a human".to_owned()),
            display_reason: None,
        }));
        registry.set_approval(Some(Arc::new(ScriptedApproval {
            outcome: ApprovalOutcome::Rejected,
            requests: Mutex::new(Vec::new()),
        })));
        let mut input = input_for("echo", json!({"value": "hi"}));
        input.agent = Some("agent-1".to_owned());
        let result = registry.execute(&input);
        let ToolExecutionResult::Failure(failure) = &result else {
            panic!("a rejected ask must deny")
        };
        assert_eq!(failure.error.message, "the user rejected tool \"echo\"");
        registry.set_approval(None);
    }
    assert_eq!(denials.load(Ordering::SeqCst), 0);
    assert_eq!(silent.load(Ordering::SeqCst), 0);

    // On the allow path the first denial wins and the later guard is
    // never consulted.
    let result = registry.execute(&input_for("echo", json!({"value": "hi"})));
    let ToolExecutionResult::Failure(failure) = &result else {
        panic!("the guard denial must fail the call")
    };
    assert_eq!(failure.error.message, "loop detected");
    assert_eq!(denials.load(Ordering::SeqCst), 1);
    assert_eq!(silent.load(Ordering::SeqCst), 0);
}

/// A guard-denied call still passes through tools/post-execute: the
/// denial result is observable there and the pass-through accept keeps
/// it intact.
#[test]
fn guard_denials_still_run_post_execute() {
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let _registration = registry.register(echo_tool("echo")).expect("registers");
    let _guard = registry.guard(Arc::new(|_exec| Some("loop detected".to_owned())));
    let post_errors = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&post_errors);
    let _listener = registry.on_post_execute(Arc::new(move |_exec, result, _next| {
        seen.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(result.is_error());
        PostToolDecision::Accept {
            content: None,
            value: None,
            additional_contexts: Vec::new(),
        }
    }));
    let result = registry.execute(&input_for("echo", json!({"value": "hi"})));
    let ToolExecutionResult::Failure(failure) = &result else {
        panic!("the guard denial must fail the call")
    };
    assert_eq!(failure.error.message, "loop detected");
    assert_eq!(
        *post_errors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![true],
        "post-execute observed the denial result"
    );
}

/// The whitelist has no supersets and the runtime has no depth cap: an
/// unknown keyword refuses registration verbatim, while a schema nested
/// past the dsh TS-type bound of 16 levels still compiles (that `JsonValue`
/// fallback is a compile-time type bound, not runtime behavior — Q-B).
#[test]
fn superset_keys_reject_and_deep_compiles() {
    let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(Recorder::default()));
    let rogue = DefineTool {
        parameters: json!({"value": {"type": "string", "required": true, "unknownKeyword": 1}}),
        ..echo_tool_fields("rogue")
    };
    let error = rogue.build().expect_err("superset keys refuse");
    assert!(
        error
            .to_string()
            .contains("parameters.value.unknownKeyword is not supported"),
        "{error}"
    );

    let mut deep = json!({"type": "string"});
    for _ in 0..17 {
        deep = json!({"type": "array", "items": deep});
    }
    let mut value_spec = deep.as_object().expect("object").clone();
    value_spec.insert("required".to_owned(), json!(true));
    let mut parameters = serde_json::Map::new();
    parameters.insert("value".to_owned(), Value::Object(value_spec));
    let tool = DefineTool {
        parameters: Value::Object(parameters),
        ..echo_tool_fields("deep")
    };
    let built = tool.build().expect("a 17-deep schema compiles iteratively");
    let deep_handle: Arc<dyn ToolDefinition> = Arc::new(built);
    let _registration = registry.register(deep_handle).expect("registers");
}
