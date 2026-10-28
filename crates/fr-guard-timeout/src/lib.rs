// Rust guideline compliant 2026-10-28
//! Cooperative tool-call timeout enforcer (dsh `guard/timeout-policy`,
//! verbatim semantics).
//!
//! A tool declares `timeoutMs` and promises to honor its signal; this guard
//! arms that deadline and maps its own expiry to `TOOL_TIMEOUT` without
//! racing or abandoning the tool body: it delegates synchronously (the body
//! settles cooperatively), then replaces the result only when its own timer
//! fired. Where dsh arms a live timer through `dsh-timeout`, this port arms
//! the passive [`fr_timeout::Deadline`] with a one-shot checker thread so
//! the body's signal polls observe the expiry at wall-clock time.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use fr_tools::json_schema::js_number_to_string;
use fr_tools::registry::{ExecuteListener, ToolDispatchView, ToolExecuteOutcome, ToolRegistry};
use fr_tools::signal::ToolSignal;
use fr_tools::types::{
    ToolErrorInfo, ToolExecutionFailure, ToolExecutionResult, ToolFailure, text_content,
};

pub mod plugin_shell;

/// The code owned by this guard, used both as the deadline classification
/// code and as the structured error code on the replacement result (a
/// nested outer deadline reads as an ordinary upstream cancel).
pub const TOOL_TIMEOUT: &str = "TOOL_TIMEOUT";

/// The structured result substituted when this guard's deadline wins (dsh
/// `toolTimeoutResult`).
#[must_use]
pub fn tool_timeout_result(timeout_ms: f64) -> ToolExecutionResult {
    let message = format!(
        "tool call timed out after {}ms",
        serde_json::Number::from_f64(timeout_ms).map_or_else(
            || timeout_ms.to_string(),
            |number: serde_json::Number| js_number_to_string(&number),
        )
    );
    ToolExecutionResult::Failure(ToolExecutionFailure {
        content: text_content(&format!("Error: {message}")),
        error: ToolFailure {
            message,
            info: Some(ToolErrorInfo {
                name: "ToolTimeoutError".to_owned(),
                code: TOOL_TIMEOUT.to_owned(),
                reason: None,
            }),
        },
        meta: None,
        additional_contexts: Vec::new(),
    })
}

/// Builds the `tools/execute` wrapper for one registry.
///
/// It resolves the caller-visible tool, temporarily replaces the dispatch
/// signal with its deadline, delegates, restores the upstream signal, and
/// replaces the result only when this guard's own timer fired.
#[must_use]
pub fn timeout_listener(registry: Arc<ToolRegistry>) -> ExecuteListener {
    Arc::new(
        move |view: &mut ToolDispatchView<'_>,
              next: &mut dyn FnMut(&mut ToolDispatchView<'_>) -> ToolExecutionResult| {
            let timeout_ms = registry
                .get(&view.exec.name)
                .and_then(|tool| tool.timeout_ms());
            let Some(timeout_ms) = timeout_ms else {
                // A tool that declares no budget: no deadline, delegate
                // unchanged.
                return ToolExecuteOutcome::Canonical(next(view));
            };
            // dsh `deadline(exec.signal, timeoutMs, TOOL_TIMEOUT)`: derive the
            // deadline from the current dispatch signal, then swap it in.
            let upstream = Arc::clone(&view.signal);
            // The budget was validated at registration; a malformed repeat
            // degrades to an unbounded delegation.
            let Ok(deadline) = fr_timeout::deadline(None, timeout_ms, TOOL_TIMEOUT) else {
                return ToolExecuteOutcome::Canonical(next(view));
            };
            arm_checker(deadline.clone(), timeout_ms);
            let derived = ToolSignal::plain(deadline.signal().clone());
            view.signal = ToolSignal::fuse(Arc::clone(&upstream), derived);
            let result = next(view);
            view.signal = upstream;
            // Restore happened above (post-execute never sees this guard's
            // possibly-aborted timeout signal); now check whether OUR timer won,
            // scoped by code so a nested outer deadline reads as upstream.
            let _expired = deadline.check();
            let own_timer_fired =
                fr_timeout::timeout_of(deadline.signal().reason(), Some(TOOL_TIMEOUT)).is_some();
            if own_timer_fired {
                return ToolExecuteOutcome::Authored(tool_timeout_result(timeout_ms));
            }
            ToolExecuteOutcome::Canonical(result)
        },
    )
}

/// Arms the one-shot checker: a thread that sleeps the budget and then
/// advances the passive deadline so signal polls observe the expiry. The
/// thread self-terminates right after its check (a transient per timed call,
/// like dsh's timer).
fn arm_checker(deadline: fr_timeout::Deadline, timeout_ms: f64) {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "JS Number parity: the dsh timer consumes the same integer-truncated milliseconds"
    )]
    let millis = timeout_ms.max(0.0) as u64;
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(millis));
        let _expired = deadline.check();
    });
}

#[cfg(test)]
mod tests {
    use fr_tools::definition::{DefineTool, ToolDefinition};
    use fr_tools::registry::ToolsEmitter;
    use fr_tools::types::{ToolExecution, ToolExecutionInput, ToolPresentationMode};
    use serde_json::{Value, json};

    use super::*;

    /// A recording no-op emitter.
    #[derive(Default)]
    struct NullEmitter;
    impl ToolsEmitter for NullEmitter {
        fn tools_change(&self) {}
        fn tools_result(&self, _exec: &ToolExecution, _result: &ToolExecutionResult) {}
    }

    /// A sleeping echo tool with a declared budget.
    fn timed_tool(name: &str, timeout_ms: Option<f64>, sleep_ms: u64) -> Arc<dyn ToolDefinition> {
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
                thread::sleep(Duration::from_millis(sleep_ms));
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

    fn input_for(name: &str) -> ToolExecutionInput {
        ToolExecutionInput {
            call_id: "call-1".to_owned(),
            root_call_id: None,
            name: name.to_owned(),
            schema: None,
            arguments: json!({"value": "x"}),
            agent: None,
            parent: None,
            signal: ToolSignal::plain(fr_timeout::DeadlineSignal::new()),
        }
    }

    #[test]
    fn no_budget_passes_through() {
        let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(NullEmitter));
        let _registration = registry
            .register(timed_tool("plain", None, 0))
            .expect("registers");
        let _listener = registry.on_execute(timeout_listener(Arc::clone(&registry)));
        let result = registry.execute(&input_for("plain"));
        let ToolExecutionResult::Success(success) = &result else {
            panic!("unbudgeted call must succeed")
        };
        assert_eq!(success.value, json!("done"));
    }

    #[test]
    fn own_expiry_replaces_the_result() {
        let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(NullEmitter));
        let _registration = registry
            .register(timed_tool("slow", Some(1.0), 80))
            .expect("registers");
        let _listener = registry.on_execute(timeout_listener(Arc::clone(&registry)));
        let result = registry.execute(&input_for("slow"));
        let ToolExecutionResult::Failure(failure) = &result else {
            panic!("timed-out call must fail")
        };
        assert_eq!(failure.error.message, "tool call timed out after 1ms");
        let info = failure.error.info.as_ref().expect("coded");
        assert_eq!(
            (info.name.as_str(), info.code.as_str()),
            ("ToolTimeoutError", "TOOL_TIMEOUT")
        );
    }

    #[test]
    fn under_budget_keeps_the_result() {
        let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(NullEmitter));
        let _registration = registry
            .register(timed_tool("quick", Some(1000.0), 0))
            .expect("registers");
        let _listener = registry.on_execute(timeout_listener(Arc::clone(&registry)));
        let result = registry.execute(&input_for("quick"));
        assert!(matches!(result, ToolExecutionResult::Success(_)));
    }

    #[test]
    fn wrapper_contract_shape() {
        // The listener hands the view to `next` and restores the upstream
        // signal: assert through a chained probe wrapper.
        let registry = ToolRegistry::new(ToolPresentationMode::Native, Arc::new(NullEmitter));
        let _registration = registry
            .register(timed_tool("probe", Some(5000.0), 0))
            .expect("registers");
        let _outer = registry.on_execute(Arc::new(|view, next| {
            let before = Arc::clone(&view.signal);
            let result = next(view);
            assert!(Arc::ptr_eq(&before, &view.signal));
            ToolExecuteOutcome::Canonical(result)
        }));
        let _timeout = registry.on_execute(timeout_listener(Arc::clone(&registry)));
        let result = registry.execute(&input_for("probe"));
        assert!(matches!(result, ToolExecutionResult::Success(_)));
    }
}
