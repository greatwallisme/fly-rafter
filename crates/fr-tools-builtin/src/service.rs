// Rust guideline compliant 2026-10-29
//! The `'builtin-tools'` wire service: the two shell tools' endpoint
//! faces over one export table (dsh tool objects endpoint-ized through
//! the fr-tools register face, the Q101A-prime seam-host judgment).
//!
//! Method selectors: 0 describe; bash 1 execute / 2 render /
//! 3 presentCall / 4 presentResult; pwsh 11-14; 900 the
//! `workingDirectory` projection fold. The execute frames follow the
//! fr-tools body protocol (`{call}` in, `{value}|{error}` out); the
//! projection frames follow the register-wire projector protocol
//! (`{args, value}` / `{args, result}` in, the view out).

use std::sync::Arc;

use serde_json::{Value, json};

use crate::render;
use crate::shell_env::ShellEnvRegistry;
use crate::tools;
use crate::working_directory;

/// The service name this face provides.
pub const SERVICE_NAME: &str = "builtin-tools";

/// The dylib artifact's plugin name.
pub const PLUGIN_NAME: &str = "fr-tools-builtin";

/// Method selector: the shell metadata.
pub const METHOD_DESCRIBE: u32 = 0;
/// Method selector: the bash tool body.
pub const METHOD_BASH_EXECUTE: u32 = 1;
/// Method selector: the bash render projection.
pub const METHOD_BASH_RENDER: u32 = 2;
/// Method selector: the bash pending-call presentation.
pub const METHOD_BASH_PRESENT_CALL: u32 = 3;
/// Method selector: the bash completed-result presentation.
pub const METHOD_BASH_PRESENT_RESULT: u32 = 4;
/// Method selector: the pwsh tool body.
pub const METHOD_PWSH_EXECUTE: u32 = 11;
/// Method selector: the pwsh render projection.
pub const METHOD_PWSH_RENDER: u32 = 12;
/// Method selector: the pwsh pending-call presentation.
pub const METHOD_PWSH_PRESENT_CALL: u32 = 13;
/// Method selector: the pwsh completed-result presentation.
pub const METHOD_PWSH_PRESENT_RESULT: u32 = 14;
/// Method selector: the shared `{cwd}` presentation metadata (dsh
/// `output.presentationMeta`; both twins project the same shape).
pub const METHOD_PRESENTATION_META: u32 = 5;
/// Method selector: the `workingDirectory` projection fold (the
/// non-tool band; 20+ stays reserved for the later tool groups).
pub const METHOD_WORKING_DIRECTORY_FOLD: u32 = 900;
/// Method selector: the read tool body.
pub const METHOD_READ_EXECUTE: u32 = 20;
/// Method selector: the read render projection.
pub const METHOD_READ_RENDER: u32 = 21;
/// Method selector: the read pending-call presentation.
pub const METHOD_READ_PRESENT_CALL: u32 = 22;
/// Method selector: the read completed-result presentation.
pub const METHOD_READ_PRESENT_RESULT: u32 = 23;
/// Method selector: the read presentation metadata.
pub const METHOD_READ_PRESENTATION_META: u32 = 24;
/// Method selector: the write tool body.
pub const METHOD_WRITE_EXECUTE: u32 = 30;
/// Method selector: the write render projection.
pub const METHOD_WRITE_RENDER: u32 = 31;
/// Method selector: the write pending-call presentation.
pub const METHOD_WRITE_PRESENT_CALL: u32 = 32;
/// Method selector: the write completed-result presentation.
pub const METHOD_WRITE_PRESENT_RESULT: u32 = 33;
/// Method selector: the write presentation metadata.
pub const METHOD_WRITE_PRESENTATION_META: u32 = 34;
/// Method selector: the edit tool body.
pub const METHOD_EDIT_EXECUTE: u32 = 40;
/// Method selector: the edit render projection.
pub const METHOD_EDIT_RENDER: u32 = 41;
/// Method selector: the edit pending-call presentation.
pub const METHOD_EDIT_PRESENT_CALL: u32 = 42;
/// Method selector: the edit completed-result presentation.
pub const METHOD_EDIT_PRESENT_RESULT: u32 = 43;
/// Method selector: the edit presentation metadata.
pub const METHOD_EDIT_PRESENTATION_META: u32 = 44;
/// Method selector: the `read_image` tool body.
pub const METHOD_READ_IMAGE_EXECUTE: u32 = 50;
/// Method selector: the `read_image` content projection (the dsh
/// `output.render`).
pub const METHOD_READ_IMAGE_RENDER: u32 = 51;
/// Method selector: the `read_image` pending-call presentation.
pub const METHOD_READ_IMAGE_PRESENT_CALL: u32 = 52;
/// Method selector: the `read_image` presentation metadata (the
/// resolved path only).
pub const METHOD_READ_IMAGE_PRESENTATION_META: u32 = 53;
/// Method selector: the glob tool body.
pub const METHOD_GLOB_EXECUTE: u32 = 60;
/// Method selector: the glob render projection.
pub const METHOD_GLOB_RENDER: u32 = 61;
/// Method selector: the glob pending-call presentation.
pub const METHOD_GLOB_PRESENT_CALL: u32 = 62;
/// Method selector: the glob completed-result presentation.
pub const METHOD_GLOB_PRESENT_RESULT: u32 = 63;
/// Method selector: the glob presentation metadata.
pub const METHOD_GLOB_PRESENTATION_META: u32 = 64;
/// Method selector: the glob capped-result spill listener (the
/// post-execute CPS arm).
pub const METHOD_GLOB_SPILL: u32 = 65;
/// Method selector: the grep tool body.
pub const METHOD_GREP_EXECUTE: u32 = 70;
/// Method selector: the grep render projection.
pub const METHOD_GREP_RENDER: u32 = 71;
/// Method selector: the grep pending-call presentation.
pub const METHOD_GREP_PRESENT_CALL: u32 = 72;
/// Method selector: the grep completed-result presentation.
pub const METHOD_GREP_PRESENT_RESULT: u32 = 73;
/// Method selector: the grep presentation metadata.
pub const METHOD_GREP_PRESENTATION_META: u32 = 74;
/// Method selector: the grep capped-result spill listener (the
/// post-execute CPS arm).
pub const METHOD_GREP_SPILL: u32 = 75;
/// Method selector: the `str_replace_editor` tool body.
pub const METHOD_STR_REPLACE_EDITOR_EXECUTE: u32 = 80;
/// Method selector: the `str_replace_editor` render projection.
pub const METHOD_STR_REPLACE_EDITOR_RENDER: u32 = 81;
/// Method selector: the `str_replace_editor` pending-call presentation.
pub const METHOD_STR_REPLACE_EDITOR_PRESENT_CALL: u32 = 82;
/// Method selector: the todo tool body.
pub const METHOD_TODO_EXECUTE: u32 = 90;
/// Method selector: the todo render projection.
pub const METHOD_TODO_RENDER: u32 = 91;
/// Method selector: the todo pending-call presentation.
pub const METHOD_TODO_PRESENT_CALL: u32 = 92;
/// Method selector: the `todos` projection fold (the non-tool band; the
/// `workingDirectory` fold rides 900).
pub const METHOD_TODOS_FOLD: u32 = 901;
/// Method selector: the `get_goal` tool body.
pub const METHOD_GET_GOAL_EXECUTE: u32 = 93;
/// Method selector: the `get_goal` render projection.
pub const METHOD_GET_GOAL_RENDER: u32 = 94;
/// Method selector: the `get_goal` pending-call presentation.
pub const METHOD_GET_GOAL_PRESENT_CALL: u32 = 95;
/// Method selector: the `create_goal` tool body.
pub const METHOD_CREATE_GOAL_EXECUTE: u32 = 96;
/// Method selector: the `create_goal` render projection.
pub const METHOD_CREATE_GOAL_RENDER: u32 = 97;
/// Method selector: the `create_goal` pending-call presentation.
pub const METHOD_CREATE_GOAL_PRESENT_CALL: u32 = 98;
/// Method selector: the `update_goal` tool body.
pub const METHOD_UPDATE_GOAL_EXECUTE: u32 = 99;
/// Method selector: the `update_goal` render projection.
pub const METHOD_UPDATE_GOAL_RENDER: u32 = 100;
/// Method selector: the `update_goal` pending-call presentation.
pub const METHOD_UPDATE_GOAL_PRESENT_CALL: u32 = 101;
/// Method selector: the `goal` projection fold (the non-tool band;
/// the `todos` fold rides 901).
pub const METHOD_GOAL_FOLD: u32 = 902;
/// Method selector: the `userQuestions` projection fold.
pub const METHOD_USER_QUESTIONS_FOLD: u32 = 903;
/// Method selector: the `plan` projection fold.
pub const METHOD_PLAN_FOLD: u32 = 904;
/// Method selector: the dynamic `plan:policy` section text endpoint.
pub const METHOD_PLAN_POLICY_SECTION: u32 = 905;
/// Method selector: the `job_output` tool body.
pub const METHOD_JOB_OUTPUT_EXECUTE: u32 = 102;
/// Method selector: the `job_output` render projection.
pub const METHOD_JOB_OUTPUT_RENDER: u32 = 103;
/// Method selector: the `job_output` pending-call presentation.
pub const METHOD_JOB_OUTPUT_PRESENT_CALL: u32 = 104;
/// Method selector: the `job_list` tool body.
pub const METHOD_JOB_LIST_EXECUTE: u32 = 105;
/// Method selector: the `job_list` render projection.
pub const METHOD_JOB_LIST_RENDER: u32 = 106;
/// Method selector: the `job_list` pending-call presentation.
pub const METHOD_JOB_LIST_PRESENT_CALL: u32 = 107;
/// Method selector: the `job_kill` tool body.
pub const METHOD_JOB_KILL_EXECUTE: u32 = 108;
/// Method selector: the `job_kill` render projection.
pub const METHOD_JOB_KILL_RENDER: u32 = 109;
/// Method selector: the `job_kill` pending-call presentation.
pub const METHOD_JOB_KILL_PRESENT_CALL: u32 = 110;
/// Method selector: the `job_output` finalize listener (the post-execute
/// CPS arm; the dsh `finalizeContent` carrier).
pub const METHOD_JOB_OUTPUT_FINALIZE: u32 = 111;
/// Method selector: the `job_kill` finalize listener (the post-execute
/// CPS arm; the dsh `finalizeContent` carrier).
pub const METHOD_JOB_KILL_FINALIZE: u32 = 112;
/// Method selector: the `schedule_create` tool body.
pub const METHOD_SCHEDULE_CREATE_EXECUTE: u32 = 113;
/// Method selector: the `schedule_create` render projection.
pub const METHOD_SCHEDULE_CREATE_RENDER: u32 = 114;
/// Method selector: the `schedule_create` pending-call presentation.
pub const METHOD_SCHEDULE_CREATE_PRESENT_CALL: u32 = 115;
/// Method selector: the `schedule_list` tool body.
pub const METHOD_SCHEDULE_LIST_EXECUTE: u32 = 116;
/// Method selector: the `schedule_list` render projection.
pub const METHOD_SCHEDULE_LIST_RENDER: u32 = 117;
/// Method selector: the `schedule_list` pending-call presentation.
pub const METHOD_SCHEDULE_LIST_PRESENT_CALL: u32 = 118;
/// Method selector: the `schedule_delete` tool body.
pub const METHOD_SCHEDULE_DELETE_EXECUTE: u32 = 119;
/// Method selector: the `schedule_delete` render projection.
pub const METHOD_SCHEDULE_DELETE_RENDER: u32 = 120;
/// Method selector: the `schedule_delete` pending-call presentation.
pub const METHOD_SCHEDULE_DELETE_PRESENT_CALL: u32 = 121;
/// Method selector: the `schedule_update` tool body.
pub const METHOD_SCHEDULE_UPDATE_EXECUTE: u32 = 122;
/// Method selector: the `schedule_update` render projection.
pub const METHOD_SCHEDULE_UPDATE_RENDER: u32 = 123;
/// Method selector: the `schedule_update` pending-call presentation.
pub const METHOD_SCHEDULE_UPDATE_PRESENT_CALL: u32 = 124;

/// Method selector: the `ask_user_question` tool body (dsh
/// `tool-ask-user`; the blocking legacy default).
pub const METHOD_ASK_EXECUTE: u32 = 125;
/// Method selector: the `ask_user_question` render projection.
pub const METHOD_ASK_RENDER: u32 = 126;
/// Method selector: the `exit_plan_mode` tool body (the plan-mode
/// controller's tool).
pub const METHOD_EXIT_PLAN_EXECUTE: u32 = 128;
/// Method selector: the `exit_plan_mode` render projection.
pub const METHOD_EXIT_PLAN_RENDER: u32 = 129;
/// Method selector: the `exit_plan_mode` pending-call presentation.
pub const METHOD_EXIT_PLAN_PRESENT_CALL: u32 = 130;
/// Method selector: the `exit_plan_mode` result presentation.
pub const METHOD_EXIT_PLAN_PRESENT_RESULT: u32 = 131;

/// The `session_search` execute endpoint.
pub const METHOD_SESSION_SEARCH_EXECUTE: u32 = 132;
/// The `session_search` render endpoint.
pub const METHOD_SESSION_SEARCH_RENDER: u32 = 133;
/// The `session_search` present-call endpoint.
pub const METHOD_SESSION_SEARCH_PRESENT_CALL: u32 = 134;
/// The `session_event_search` execute endpoint.
pub const METHOD_SESSION_EVENT_SEARCH_EXECUTE: u32 = 135;
/// The `session_event_search` render endpoint.
pub const METHOD_SESSION_EVENT_SEARCH_RENDER: u32 = 136;
/// The `session_event_search` present-call endpoint.
pub const METHOD_SESSION_EVENT_SEARCH_PRESENT_CALL: u32 = 137;
/// The `session_trace` execute endpoint.
pub const METHOD_SESSION_TRACE_EXECUTE: u32 = 138;
/// The `session_trace` render endpoint.
pub const METHOD_SESSION_TRACE_RENDER: u32 = 139;
/// The `session_trace` present-call endpoint.
pub const METHOD_SESSION_TRACE_PRESENT_CALL: u32 = 140;
/// The `session_event_trace` execute endpoint.
pub const METHOD_SESSION_EVENT_TRACE_EXECUTE: u32 = 141;
/// The `session_event_trace` render endpoint.
pub const METHOD_SESSION_EVENT_TRACE_RENDER: u32 = 142;
/// The `session_event_trace` present-call endpoint.
pub const METHOD_SESSION_EVENT_TRACE_PRESENT_CALL: u32 = 143;
/// The `session_event_read` execute endpoint.
pub const METHOD_SESSION_EVENT_READ_EXECUTE: u32 = 144;
/// The `session_event_read` render endpoint.
pub const METHOD_SESSION_EVENT_READ_RENDER: u32 = 145;
/// The `session_event_read` present-call endpoint.
pub const METHOD_SESSION_EVENT_READ_PRESENT_CALL: u32 = 146;
/// The `'session-query'` service face's observe selector (the fr-wire
/// constant mirrors; the headless adoptability read).
pub const METHOD_SESSION_QUERY_OBSERVE: u32 = fr_wire::services::session_query::METHOD_OBSERVE;

/// The escalate-to modes this composition advertises (the dsh
/// `ESCALATION_TARGETS` words in table order).
pub const ESCALATION_MODES: [&str; 2] = ["workspace-write", "danger-full-access"];

/// Renders one finite number the way dsh `JSON.stringify` does for the
/// error texts (the fr-shell judgment, re-exported for the render
/// texts).
#[must_use]
pub fn js_number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e21 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the branch bounds the value under 1e21 and integral, so the cast keeps every digit"
        )]
        return format!("{}", value as i64);
    }
    value.to_string()
}

/// The composed runtime the wire face dispatches against: the scripted
/// seams and the in-crate domains (the domains ride `Arc` so the
/// activation shares them with the wire seam implementation).
pub struct BuiltinTools {
    seams: Box<dyn tools::Seams>,
    env: std::sync::Arc<ShellEnvRegistry>,
    directories: std::sync::Arc<working_directory::WorkingDirectories>,
    lists: std::sync::Arc<crate::todo::TodoLists>,
    goals: std::sync::Arc<crate::goal_service::Goals>,
    notices: std::sync::Arc<crate::job_tools::NoticeState>,
    schedules: std::sync::Arc<crate::schedule::Schedules>,
    interactions: std::sync::Arc<crate::interaction_tools::InteractionState>,
    session_query: std::sync::Arc<crate::session_query_tools::SessionQueryRuntime>,
}

impl std::fmt::Debug for BuiltinTools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuiltinTools").finish_non_exhaustive()
    }
}

impl BuiltinTools {
    /// Assembles the runtime over the composed seams, the resolved
    /// harness home, and the launch directory.
    ///
    /// # Errors
    ///
    /// The home resolution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "the assembly mirrors the dsh plugin composition one arm per domain; a params struct would hide the correspondence"
    )]
    pub fn new(
        seams: Box<dyn tools::Seams>,
        env: std::sync::Arc<ShellEnvRegistry>,
        directories: std::sync::Arc<working_directory::WorkingDirectories>,
        lists: std::sync::Arc<crate::todo::TodoLists>,
        goals: std::sync::Arc<crate::goal_service::Goals>,
        notices: std::sync::Arc<crate::job_tools::NoticeState>,
        schedules: std::sync::Arc<crate::schedule::Schedules>,
        interactions: std::sync::Arc<crate::interaction_tools::InteractionState>,
        session_query: std::sync::Arc<crate::session_query_tools::SessionQueryRuntime>,
    ) -> Result<Self, String> {
        Ok(Self {
            seams,
            env,
            directories,
            lists,
            goals,
            notices,
            schedules,
            interactions,
            session_query,
        })
    }

    /// The hosted session-query runtime (the five bodies' engine and
    /// search bounds).
    #[must_use]
    pub const fn session_query(
        &self,
    ) -> &std::sync::Arc<crate::session_query_tools::SessionQueryRuntime> {
        &self.session_query
    }

    /// The hosted schedule domain (the tool bodies' owning state).
    #[must_use]
    pub const fn schedules(&self) -> &std::sync::Arc<crate::schedule::Schedules> {
        &self.schedules
    }

    /// The hosted interaction runtime (the ask/exit bodies' owning
    /// state).
    #[must_use]
    pub const fn interactions(
        &self,
    ) -> &std::sync::Arc<crate::interaction_tools::InteractionState> {
        &self.interactions
    }

    /// The hosted notice state (the completion-notice machine's shared
    /// ledger; the shell level owns the worker).
    #[must_use]
    pub const fn notices(&self) -> &std::sync::Arc<crate::job_tools::NoticeState> {
        &self.notices
    }

    /// The hosted todo domain (the tool body's owning-session state).
    #[must_use]
    pub const fn lists(&self) -> &std::sync::Arc<crate::todo::TodoLists> {
        &self.lists
    }

    /// The hosted goal domain (the tool bodies' owning-session state).
    #[must_use]
    pub const fn goals(&self) -> &std::sync::Arc<crate::goal_service::Goals> {
        &self.goals
    }

    /// The hosted environment registry (the plugin contributes facts
    /// through it at activation).
    #[must_use]
    pub const fn env(&self) -> &std::sync::Arc<ShellEnvRegistry> {
        &self.env
    }

    /// The hosted working-directory domain (16b migrates the fs leg
    /// behind the seam).
    #[must_use]
    pub const fn directories(&self) -> &std::sync::Arc<working_directory::WorkingDirectories> {
        &self.directories
    }

    /// The composed cross-plugin seam (the fs tool bodies' standing
    /// policy, approval, and session-cwd consumption).
    #[must_use]
    pub fn seams(&self) -> &dyn tools::Seams {
        self.seams.as_ref()
    }

    /// Serves one method invocation.
    ///
    /// # Errors
    ///
    /// The payload decode refusal, an unknown method, or the tool
    /// body's own failure (the wire body-error shape).
    #[expect(
        clippy::too_many_lines,
        reason = "the match table mirrors the registration bands one arm per endpoint; splitting it would hide the correspondence"
    )]
    pub fn serve(&self, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
        let request: Value = if payload.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(payload)
                .map_err(|error| format!("builtin-tools payload must be JSON: {error}"))?
        };
        let reply = match method {
            METHOD_DESCRIBE => json!({
                "plugin": PLUGIN_NAME,
                "service": SERVICE_NAME,
                "methods": describe_methods().as_slice(),
            }),
            METHOD_BASH_EXECUTE => {
                return self.serve_execute(tools::ShellVariant::Bash, &request);
            }
            METHOD_PWSH_EXECUTE => {
                return self.serve_execute(tools::ShellVariant::Pwsh, &request);
            }
            METHOD_BASH_RENDER | METHOD_PWSH_RENDER => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                let value = request.get("value").cloned().unwrap_or(Value::Null);
                json!(render_value(&args, &value))
            }
            METHOD_PRESENTATION_META => {
                let value = request.get("value").cloned().unwrap_or(Value::Null);
                json!({ "cwd": value.get("cwd").cloned().unwrap_or(Value::Null) })
            }
            METHOD_BASH_PRESENT_CALL | METHOD_PWSH_PRESENT_CALL => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                tools::present_call(&args)
            }
            METHOD_BASH_PRESENT_RESULT | METHOD_PWSH_PRESENT_RESULT => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                let result = request.get("result").cloned().unwrap_or(Value::Null);
                tools::present_result(&args, &result).unwrap_or(Value::Null)
            }
            METHOD_WORKING_DIRECTORY_FOLD => {
                let state = fold_log(&request);
                json!({ "state": state })
            }
            METHOD_TODOS_FOLD => {
                let state = crate::todo::fold(
                    &request
                        .get("events")
                        .cloned()
                        .unwrap_or(Value::Array(Vec::new())),
                );
                json!({ "state": state })
            }
            METHOD_TODO_EXECUTE => {
                let call = request
                    .get("call")
                    .ok_or_else(|| "builtin-tools execute payload requires call".to_string())?;
                let agent = call.get("agent").and_then(Value::as_str);
                let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
                return match self.lists.execute(&arguments, agent) {
                    Ok(value) => serde_json::to_vec(&json!({ "value": value }))
                        .map_err(|error| format!("body reply encode failed: {error}")),
                    Err(error) => serde_json::to_vec(&json!({
                        "error": { "message": error.message, "name": error.name },
                    }))
                    .map_err(|error| format!("body error encode failed: {error}")),
                };
            }
            METHOD_TODO_RENDER => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                let value = request.get("value").cloned().unwrap_or(Value::Null);
                json!(crate::todo::render(&args, &value))
            }
            METHOD_TODO_PRESENT_CALL => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                json!(crate::todo::present_call(&args))
            }
            METHOD_GET_GOAL_EXECUTE => {
                let call = request
                    .get("call")
                    .ok_or_else(|| "builtin-tools execute payload requires call".to_string())?;
                return self.serve_goal_body(
                    |goals, _arguments, agent| crate::goal_tools::execute_get(goals, agent),
                    call,
                );
            }
            METHOD_CREATE_GOAL_EXECUTE => {
                let call = request
                    .get("call")
                    .ok_or_else(|| "builtin-tools execute payload requires call".to_string())?;
                return self.serve_goal_body(
                    |goals, arguments, agent| {
                        crate::goal_tools::execute_create(goals, &arguments, agent)
                    },
                    call,
                );
            }
            METHOD_UPDATE_GOAL_EXECUTE => {
                let call = request
                    .get("call")
                    .ok_or_else(|| "builtin-tools execute payload requires call".to_string())?;
                return self.serve_goal_body(
                    |goals, arguments, agent| {
                        crate::goal_tools::execute_update(
                            goals,
                            &arguments,
                            agent,
                            crate::goal_tools::BAKED_BLOCKED_AFTER,
                        )
                    },
                    call,
                );
            }
            METHOD_GET_GOAL_RENDER | METHOD_CREATE_GOAL_RENDER | METHOD_UPDATE_GOAL_RENDER => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                let value = request.get("value").cloned().unwrap_or(Value::Null);
                json!(crate::goal_tools::render(&args, &value))
            }
            METHOD_GET_GOAL_PRESENT_CALL => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                json!(crate::goal_tools::present_call_get(&args))
            }
            METHOD_CREATE_GOAL_PRESENT_CALL => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                json!(crate::goal_tools::present_call_create(&args))
            }
            METHOD_UPDATE_GOAL_PRESENT_CALL => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                json!(crate::goal_tools::present_call_update(&args))
            }
            METHOD_GOAL_FOLD => {
                let events = request
                    .get("events")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let state = crate::goal::fold_log(&events);
                json!({ "state": state.to_value() })
            }
            METHOD_JOB_OUTPUT_EXECUTE => {
                let call = request
                    .get("call")
                    .ok_or_else(|| "builtin-tools execute payload requires call".to_string())?;
                let agent = call.get("agent").and_then(Value::as_str);
                let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
                return self.serve_job_body(
                    |seams, arguments, agent, _| {
                        crate::job_tools::execute_output(seams, &arguments, agent)
                    },
                    arguments,
                    agent,
                    Arc::clone(&self.notices),
                );
            }
            METHOD_JOB_LIST_EXECUTE => {
                let call = request
                    .get("call")
                    .ok_or_else(|| "builtin-tools execute payload requires call".to_string())?;
                let agent = call.get("agent").and_then(Value::as_str);
                let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
                return self.serve_job_body(
                    |seams, _arguments, agent, _| crate::job_tools::execute_list(seams, agent),
                    arguments,
                    agent,
                    Arc::clone(&self.notices),
                );
            }
            METHOD_JOB_KILL_EXECUTE => {
                let call = request
                    .get("call")
                    .ok_or_else(|| "builtin-tools execute payload requires call".to_string())?;
                let agent = call.get("agent").and_then(Value::as_str);
                let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
                let notices = Arc::clone(&self.notices);
                return self.serve_job_body(
                    |seams, arguments, agent, notices| {
                        crate::job_tools::execute_kill(seams, &notices, &arguments, agent)
                    },
                    arguments,
                    agent,
                    notices,
                );
            }
            METHOD_JOB_OUTPUT_RENDER => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                let value = request.get("value").cloned().unwrap_or(Value::Null);
                json!(crate::job_tools::render_output(&args, &value))
            }
            METHOD_JOB_LIST_RENDER => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                let value = request.get("value").cloned().unwrap_or(Value::Null);
                json!(crate::job_tools::render_list(&args, &value))
            }
            METHOD_JOB_KILL_RENDER => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                let value = request.get("value").cloned().unwrap_or(Value::Null);
                json!(crate::job_tools::render_kill(&args, &value))
            }
            METHOD_JOB_OUTPUT_PRESENT_CALL => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                json!(crate::job_tools::present_call_output(&args))
            }
            METHOD_JOB_LIST_PRESENT_CALL => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                json!(crate::job_tools::present_call_list(&args))
            }
            METHOD_JOB_KILL_PRESENT_CALL => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                json!(crate::job_tools::present_call_kill(&args))
            }
            METHOD_JOB_OUTPUT_FINALIZE | METHOD_JOB_KILL_FINALIZE => {
                return serve_job_finalize(self, &request);
            }
            METHOD_SCHEDULE_CREATE_EXECUTE => {
                let call = request
                    .get("call")
                    .ok_or_else(|| "builtin-tools execute payload requires call".to_string())?;
                let agent = call.get("agent").and_then(Value::as_str);
                let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
                let value = crate::schedule::execute_create(&self.schedules, &arguments, agent);
                return serde_json::to_vec(&json!({ "value": value }))
                    .map_err(|error| format!("body reply encode failed: {error}"));
            }
            METHOD_SCHEDULE_LIST_EXECUTE => {
                let call = request
                    .get("call")
                    .ok_or_else(|| "builtin-tools execute payload requires call".to_string())?;
                let agent = call.get("agent").and_then(Value::as_str);
                let value = crate::schedule::execute_list(&self.schedules, agent);
                return serde_json::to_vec(&json!({ "value": value }))
                    .map_err(|error| format!("body reply encode failed: {error}"));
            }
            METHOD_SCHEDULE_DELETE_EXECUTE => {
                let call = request
                    .get("call")
                    .ok_or_else(|| "builtin-tools execute payload requires call".to_string())?;
                let agent = call.get("agent").and_then(Value::as_str);
                let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
                let value = crate::schedule::execute_delete(&self.schedules, &arguments, agent);
                return serde_json::to_vec(&json!({ "value": value }))
                    .map_err(|error| format!("body reply encode failed: {error}"));
            }
            METHOD_SCHEDULE_UPDATE_EXECUTE => {
                let call = request
                    .get("call")
                    .ok_or_else(|| "builtin-tools execute payload requires call".to_string())?;
                let agent = call.get("agent").and_then(Value::as_str);
                let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
                let value = crate::schedule::execute_update(&self.schedules, &arguments, agent);
                return serde_json::to_vec(&json!({ "value": value }))
                    .map_err(|error| format!("body reply encode failed: {error}"));
            }
            METHOD_SCHEDULE_CREATE_RENDER
            | METHOD_SCHEDULE_LIST_RENDER
            | METHOD_SCHEDULE_DELETE_RENDER
            | METHOD_SCHEDULE_UPDATE_RENDER => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                let value = request.get("value").cloned().unwrap_or(Value::Null);
                json!(crate::schedule::render(&args, &value))
            }
            METHOD_SCHEDULE_CREATE_PRESENT_CALL => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                json!(crate::schedule::present_call_create(&args))
            }
            METHOD_SCHEDULE_LIST_PRESENT_CALL => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                json!(crate::schedule::present_call_list(&args))
            }
            METHOD_SCHEDULE_DELETE_PRESENT_CALL => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                json!(crate::schedule::present_call_delete(&args))
            }
            METHOD_SCHEDULE_UPDATE_PRESENT_CALL => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                json!(crate::schedule::present_call_update(&args))
            }
            METHOD_ASK_EXECUTE => {
                let call = request
                    .get("call")
                    .ok_or_else(|| "builtin-tools execute payload requires call".to_string())?;
                let agent = call.get("agent").and_then(Value::as_str);
                let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
                let value =
                    crate::interaction_tools::execute_ask(&arguments, agent, &self.interactions);
                return match value {
                    Ok(value) => serde_json::to_vec(&json!({ "value": value }))
                        .map_err(|error| format!("body reply encode failed: {error}")),
                    Err(error) => serde_json::to_vec(&json!({
                        "error": {
                            "code": error.code,
                            "message": error.message,
                            "name": error.name,
                        }
                    }))
                    .map_err(|error| format!("body error encode failed: {error}")),
                };
            }
            METHOD_EXIT_PLAN_EXECUTE => {
                let call = request
                    .get("call")
                    .ok_or_else(|| "builtin-tools execute payload requires call".to_string())?;
                let agent = call.get("agent").and_then(Value::as_str);
                let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
                let value =
                    crate::interaction_tools::execute_exit(&arguments, agent, &self.interactions);
                return match value {
                    Ok(value) => serde_json::to_vec(&json!({ "value": value }))
                        .map_err(|error| format!("body reply encode failed: {error}")),
                    Err(error) => serde_json::to_vec(&json!({
                        "error": {
                            "code": error.code,
                            "message": error.message,
                            "name": error.name,
                        }
                    }))
                    .map_err(|error| format!("body error encode failed: {error}")),
                };
            }
            METHOD_ASK_RENDER | METHOD_EXIT_PLAN_RENDER => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                let value = request.get("value").cloned().unwrap_or(Value::Null);
                if method == METHOD_ASK_RENDER {
                    json!(crate::interaction_tools::render_ask(&args, &value))
                } else {
                    json!(crate::interaction_tools::render_exit(&args, &value))
                }
            }
            METHOD_EXIT_PLAN_PRESENT_CALL => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                json!(crate::interaction_tools::present_call_exit(&args))
            }
            METHOD_EXIT_PLAN_PRESENT_RESULT => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                let result = request.get("result").cloned().unwrap_or(Value::Null);
                crate::interaction_tools::present_result_exit(&args, &result).unwrap_or(Value::Null)
            }
            METHOD_USER_QUESTIONS_FOLD => {
                let state = crate::user_questions::fold(
                    &request
                        .get("events")
                        .cloned()
                        .unwrap_or(Value::Array(Vec::new())),
                );
                json!({ "state": state })
            }
            METHOD_PLAN_FOLD => {
                let state = crate::plan_mode::fold(
                    &request
                        .get("events")
                        .cloned()
                        .unwrap_or(Value::Array(Vec::new())),
                );
                json!({ "state": state })
            }
            METHOD_SESSION_SEARCH_EXECUTE => {
                let call_frame = request
                    .get("call")
                    .ok_or_else(|| "builtin-tools execute payload requires call".to_string())?;
                let agent = call_frame.get("agent").and_then(Value::as_str);
                let arguments = call_frame.get("arguments").cloned().unwrap_or(Value::Null);
                return match crate::session_query_tools::execute_session_search(
                    &arguments,
                    agent,
                    &self.session_query,
                ) {
                    Ok(value) => serde_json::to_vec(&json!({ "value": value }))
                        .map_err(|error| format!("body reply encode failed: {error}")),
                    Err(error) => serde_json::to_vec(&json!({
                        "error": { "message": error.message, "name": error.name, "code": error.code },
                    }))
                    .map_err(|error| format!("body error encode failed: {error}")),
                };
            }
            METHOD_SESSION_EVENT_SEARCH_EXECUTE => {
                let call_frame = request
                    .get("call")
                    .ok_or_else(|| "builtin-tools execute payload requires call".to_string())?;
                let agent = call_frame.get("agent").and_then(Value::as_str);
                let arguments = call_frame.get("arguments").cloned().unwrap_or(Value::Null);
                return match crate::session_query_tools::execute_event_search(
                    &arguments,
                    agent,
                    &self.session_query,
                ) {
                    Ok(value) => serde_json::to_vec(&json!({ "value": value }))
                        .map_err(|error| format!("body reply encode failed: {error}")),
                    Err(error) => serde_json::to_vec(&json!({
                        "error": { "message": error.message, "name": error.name, "code": error.code },
                    }))
                    .map_err(|error| format!("body error encode failed: {error}")),
                };
            }
            METHOD_SESSION_TRACE_EXECUTE => {
                let call_frame = request
                    .get("call")
                    .ok_or_else(|| "builtin-tools execute payload requires call".to_string())?;
                let agent = call_frame.get("agent").and_then(Value::as_str);
                let arguments = call_frame.get("arguments").cloned().unwrap_or(Value::Null);
                return match crate::session_query_tools::execute_session_trace(
                    &arguments,
                    agent,
                    &self.session_query,
                ) {
                    Ok(value) => serde_json::to_vec(&json!({ "value": value }))
                        .map_err(|error| format!("body reply encode failed: {error}")),
                    Err(error) => serde_json::to_vec(&json!({
                        "error": { "message": error.message, "name": error.name, "code": error.code },
                    }))
                    .map_err(|error| format!("body error encode failed: {error}")),
                };
            }
            METHOD_SESSION_EVENT_TRACE_EXECUTE => {
                let call_frame = request
                    .get("call")
                    .ok_or_else(|| "builtin-tools execute payload requires call".to_string())?;
                let agent = call_frame.get("agent").and_then(Value::as_str);
                let arguments = call_frame.get("arguments").cloned().unwrap_or(Value::Null);
                return match crate::session_query_tools::execute_event_trace(
                    &arguments,
                    agent,
                    &self.session_query,
                ) {
                    Ok(value) => serde_json::to_vec(&json!({ "value": value }))
                        .map_err(|error| format!("body reply encode failed: {error}")),
                    Err(error) => serde_json::to_vec(&json!({
                        "error": { "message": error.message, "name": error.name, "code": error.code },
                    }))
                    .map_err(|error| format!("body error encode failed: {error}")),
                };
            }
            METHOD_SESSION_EVENT_READ_EXECUTE => {
                let call_frame = request
                    .get("call")
                    .ok_or_else(|| "builtin-tools execute payload requires call".to_string())?;
                let agent = call_frame.get("agent").and_then(Value::as_str);
                let arguments = call_frame.get("arguments").cloned().unwrap_or(Value::Null);
                return match crate::session_query_tools::execute_event_read(
                    &arguments,
                    agent,
                    &self.session_query,
                ) {
                    Ok(value) => serde_json::to_vec(&json!({ "value": value }))
                        .map_err(|error| format!("body reply encode failed: {error}")),
                    Err(error) => serde_json::to_vec(&json!({
                        "error": { "message": error.message, "name": error.name, "code": error.code },
                    }))
                    .map_err(|error| format!("body error encode failed: {error}")),
                };
            }
            METHOD_SESSION_SEARCH_RENDER
            | METHOD_SESSION_EVENT_SEARCH_RENDER
            | METHOD_SESSION_TRACE_RENDER
            | METHOD_SESSION_EVENT_TRACE_RENDER
            | METHOD_SESSION_EVENT_READ_RENDER => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                let value = request.get("value").cloned().unwrap_or(Value::Null);
                json!(crate::session_query_tools::render_text(&args, &value))
            }
            METHOD_SESSION_SEARCH_PRESENT_CALL => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                json!(crate::session_query_tools::present_session_search_call(
                    &args
                ))
            }
            METHOD_SESSION_EVENT_SEARCH_PRESENT_CALL => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                json!(crate::session_query_tools::present_event_search_call(&args))
            }
            METHOD_SESSION_TRACE_PRESENT_CALL => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                json!(crate::session_query_tools::present_session_trace_call(
                    &args
                ))
            }
            METHOD_SESSION_EVENT_TRACE_PRESENT_CALL => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                json!(crate::session_query_tools::present_event_target_call(
                    "Trace event",
                    &args,
                ))
            }
            METHOD_SESSION_EVENT_READ_PRESENT_CALL => {
                let args = request.get("args").cloned().unwrap_or(Value::Null);
                json!(crate::session_query_tools::present_event_target_call(
                    "Read event",
                    &args,
                ))
            }
            METHOD_SESSION_QUERY_OBSERVE => {
                let id = request
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "observe payload requires id".to_string())?;
                let observation = self
                    .session_query
                    .engine
                    .observe(id)
                    .map_err(|error| format!("{}: {}", error.code.as_str(), error.message))?;
                return serde_json::to_vec(&json!({
                    "source": observation.source,
                    "header": observation.header,
                    "inheritedEventCount": observation.inherited_event_count,
                    "events": observation.events,
                    "cursor": observation.cursor,
                }))
                .map_err(|error| format!("observe reply encode failed: {error}"));
            }
            METHOD_PLAN_POLICY_SECTION => {
                // The dsh section text fn: the deployment guidance only
                // renders while plan mode is active or pending for the
                // assembling context's session; without a session the
                // dsh arm returns empty.
                let context = request;
                let session = context.get("sessionId").and_then(Value::as_str);
                let text = session.map_or_else(String::new, |session| {
                    let plans = &self.interactions.plans;
                    plans
                        .projection_state(session, crate::plan_mode::PROJECTION_KEY)
                        .ok()
                        .flatten()
                        .map(|state| {
                            let logged = crate::plan_mode::logged_active(Some(&state));
                            let pending =
                                state.get("wanted").is_some_and(|wanted| !wanted.is_null())
                                    || state
                                        .pointer("/running/wanted")
                                        .is_some_and(|wanted| !wanted.is_null());
                            if logged || pending {
                                crate::catalog_gen::PLAN_MODE_SECTION_TEXT.to_owned()
                            } else {
                                String::new()
                            }
                        })
                        .unwrap_or_default()
                });
                return serde_json::to_vec(&text)
                    .map_err(|error| format!("section text encode failed: {error}"));
            }
            other => return Err(format!("unknown {PLUGIN_NAME} method {other}")),
        };
        serde_json::to_vec(&reply)
            .map_err(|error| format!("builtin-tools reply encode failed: {error}"))
    }

    /// Serves one goal tool body: decodes the execution frame, runs
    /// the goal body, and maps the outcome onto the wire body protocol
    /// (the coded error family keeps its code).
    fn serve_goal_body(
        &self,
        body: impl Fn(
            &crate::goal_service::Goals,
            Value,
            Option<&str>,
        ) -> Result<Value, crate::tools::BodyError>,
        call: &Value,
    ) -> Result<Vec<u8>, String> {
        let agent = call.get("agent").and_then(Value::as_str);
        let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
        match body(&self.goals, arguments, agent) {
            Ok(value) => serde_json::to_vec(&json!({ "value": value }))
                .map_err(|error| format!("body reply encode failed: {error}")),
            Err(error) => {
                let body = match error.code {
                    Some(code) => {
                        json!({ "error": { "code": code, "message": error.message, "name": error.name } })
                    }
                    None => json!({ "error": { "message": error.message, "name": error.name } }),
                };
                serde_json::to_vec(&body)
                    .map_err(|error| format!("body error encode failed: {error}"))
            }
        }
    }

    /// Serves one job tool body: the same wire body protocol over the
    /// seams and the shared notice state (the kill body consumes the
    /// ledger; the read/list bodies ignore it).
    fn serve_job_body(
        &self,
        body: impl Fn(
            &dyn crate::tools::Seams,
            Value,
            Option<&str>,
            Arc<crate::job_tools::NoticeState>,
        ) -> Result<Value, crate::tools::BodyError>,
        arguments: Value,
        agent: Option<&str>,
        notices: Arc<crate::job_tools::NoticeState>,
    ) -> Result<Vec<u8>, String> {
        match body(self.seams.as_ref(), arguments, agent, notices) {
            Ok(value) => serde_json::to_vec(&json!({ "value": value }))
                .map_err(|error| format!("body reply encode failed: {error}")),
            Err(error) => {
                let body = match error.code {
                    Some(code) => {
                        json!({ "error": { "code": code, "message": error.message, "name": error.name } })
                    }
                    None => json!({ "error": { "message": error.message, "name": error.name } }),
                };
                serde_json::to_vec(&body)
                    .map_err(|error| format!("body error encode failed: {error}"))
            }
        }
    }

    /// Serves one tool body: decodes the execution frame, runs the dsh
    /// twin pipeline, and maps the outcome onto the wire body protocol.
    fn serve_execute(
        &self,
        variant: tools::ShellVariant,
        request: &Value,
    ) -> Result<Vec<u8>, String> {
        let call = request
            .get("call")
            .ok_or_else(|| "builtin-tools execute payload requires call".to_string())?;
        let call_id = call
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let root_call_id = call
            .get("root_call_id")
            .and_then(Value::as_str)
            .unwrap_or(call_id);
        let agent = call.get("agent").and_then(Value::as_str);
        let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
        match tools::execute(
            variant,
            self.seams.as_ref(),
            &self.env,
            &arguments,
            call_id,
            root_call_id,
            agent,
        ) {
            Ok(value) => serde_json::to_vec(&json!({ "value": value }))
                .map_err(|error| format!("body reply encode failed: {error}")),
            Err(error) => {
                let body = match error.code {
                    Some(code) => {
                        json!({ "error": { "code": code, "message": error.message, "name": error.name } })
                    }
                    None => json!({ "error": { "message": error.message, "name": error.name } }),
                };
                serde_json::to_vec(&body)
                    .map_err(|error| format!("body error encode failed: {error}"))
            }
        }
    }
}

/// Serves one finalize listener call over the post-execute CPS protocol
/// (the dsh `finalizeContent` carrier; the spill-serve judgment): the
/// enter leg parks the call and result and answers delegate; the exit
/// leg takes the downstream decision and, when the fitting applies,
/// replaces the content with the byte-capped render.
fn serve_job_finalize(tools: &BuiltinTools, request: &Value) -> Result<Vec<u8>, String> {
    let finalizes = tools.notices().finalizes();
    let token = request.get("token").and_then(Value::as_u64);
    let Some(token) = token else {
        return Err("finalize listener frame requires token".to_owned());
    };
    if request.get("leg").and_then(Value::as_str) == Some("enter") {
        let call = request.get("call").cloned().unwrap_or(Value::Null);
        let result = request.get("result").cloned().unwrap_or(Value::Null);
        finalizes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(token, (call, result));
        return serde_json::to_vec(&json!({ "kind": "delegate" }))
            .map_err(|error| format!("finalize reply encode failed: {error}"));
    }
    let Some((call, result)) = finalizes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&token)
    else {
        return Err(format!(
            "finalize exit leg has no parked enter for token {token}"
        ));
    };
    let decision = request.get("decision").cloned().unwrap_or(Value::Null);
    if decision.get("kind").and_then(Value::as_str) != Some("accept") {
        // A downstream replacement or block rides through verbatim: the
        // dsh finalizer only adjusts the default rendering.
        return serde_json::to_vec(&json!({ "kind": "final", "decision": decision }))
            .map_err(|error| format!("finalize reply encode failed: {error}"));
    }
    let Some(content) = finalize_job_content(tools, &call, &result) else {
        return serde_json::to_vec(&json!({ "kind": "final", "decision": decision }))
            .map_err(|error| format!("finalize reply encode failed: {error}"));
    };
    let contexts = decision
        .get("additionalContexts")
        .cloned()
        .unwrap_or_else(|| json!([]));
    serde_json::to_vec(&json!({
        "kind": "final",
        "decision": {
            "kind": "accept",
            "content": content,
            "additionalContexts": contexts,
        },
    }))
    .map_err(|error| format!("finalize reply encode failed: {error}"))
}

/// The dsh `finalizeJobContent`: the producer's cap for the named job
/// bounds the rendered content — the default rendering keeps its
/// output/status split only while intact; anything else gets the
/// single-text tail fit; a multi-block or non-text content passes
/// through.
fn finalize_job_content(tools: &BuiltinTools, call: &Value, result: &Value) -> Option<Vec<Value>> {
    let max_bytes = visible_output_limit(tools.seams(), call)?;
    let is_error = result.get("kind").and_then(Value::as_str) == Some("failure");
    let content_blocks = result.get("content").and_then(Value::as_array)?.clone();
    if call.get("name").and_then(Value::as_str) == Some("job_output") && !is_error {
        let value = result.get("value").cloned().unwrap_or(Value::Null);
        let text = value.get("text").and_then(Value::as_str).unwrap_or("");
        let body = if text.is_empty() {
            "(no new output)".to_owned()
        } else {
            text.to_owned()
        };
        let content = body.strip_suffix('\n').unwrap_or(&body).to_owned();
        let suffix = format!(
            "\n{}",
            crate::job_tools::status_line(&value.get("job").cloned().unwrap_or(Value::Null))
        );
        if raw_single_text(&content_blocks) == Some(format!("{content}{suffix}")) {
            return Some(vec![json!({
                "type": "text",
                "text": crate::job_tools::fit_with_suffix(
                    &content,
                    &suffix,
                    Some(max_bytes),
                    "\n[output truncated]",
                ),
            })]);
        }
    }
    bound_single_text(&content_blocks, max_bytes)
}

/// The producer's cap for the job a `job_output` or `job_kill` call
/// names, when it is visible to the caller (dsh `visibleOutputLimit`).
fn visible_output_limit(seams: &dyn crate::tools::Seams, call: &Value) -> Option<f64> {
    let name = call.get("name").and_then(Value::as_str)?;
    if name != "job_output" && name != "job_kill" {
        return None;
    }
    let job_id = call
        .get("arguments")
        .and_then(|arguments| arguments.get("job_id"))
        .and_then(Value::as_str)?;
    if job_id.is_empty() {
        return None;
    }
    let jobs = seams
        .jobs_list(call.get("agent").and_then(Value::as_str))
        .ok()?;
    jobs.as_array()?.iter().find_map(|job| {
        let matches = job.get("id").and_then(Value::as_str) == Some(job_id);
        matches.then(|| job.get("output_limit_bytes").and_then(Value::as_f64))?
    })
}

/// The exactly-one-text-block shape test (dsh `rawSingleText`).
fn raw_single_text(content: &[Value]) -> Option<String> {
    if content.len() != 1 {
        return None;
    }
    let block = &content[0];
    if block.get("type").and_then(Value::as_str) != Some("text") {
        return None;
    }
    block.get("text").and_then(Value::as_str).map(str::to_owned)
}

/// The single-text tail fit (dsh `boundSingleText`): one text block
/// bounds; anything else passes through untouched.
fn bound_single_text(content: &[Value], max_bytes: f64) -> Option<Vec<Value>> {
    if content.len() != 1 {
        return None;
    }
    let block = &content[0];
    if block.get("type").and_then(Value::as_str) != Some("text") {
        return None;
    }
    let text = block.get("text").and_then(Value::as_str)?;
    Some(vec![json!({
        "type": "text",
        "text": crate::job_tools::fit_with_suffix(text, "", Some(max_bytes), "\n[result truncated]"),
    })])
}

/// Renders one canonical output value onto the model-facing text block
/// (dsh `output.render`): the background acknowledgement, the promoted
/// hand-off, or the finished-run marker stack.
#[must_use]
pub fn render_value(_args: &Value, value: &Value) -> Vec<Value> {
    let text = match value.get("kind").and_then(Value::as_str) {
        Some("background") => format!(
            "started background job {}",
            value
                .get("jobId")
                .and_then(Value::as_str)
                .unwrap_or_default()
        ),
        Some("promoted") => render::render_promoted(
            value
                .get("jobId")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            value
                .get("timeoutMs")
                .and_then(Value::as_f64)
                .unwrap_or_default(),
            value
                .get("output")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ),
        _ => render::render_result(value, &ESCALATION_MODES),
    };
    vec![json!({ "type": "text", "text": text })]
}

/// Projects the cold log onto the committed directory (the dsh
/// `workingDirectory` fold the relay calls).
#[must_use]
pub fn fold_log(request: &Value) -> Value {
    let log = working_directory::ColdLog {
        header: request.get("header").cloned().unwrap_or(Value::Null),
        inherited_event_count: request
            .get("inheritedEventCount")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        events: request
            .get("events")
            .cloned()
            .unwrap_or(Value::Array(Vec::new())),
    };
    match working_directory::fold(&log) {
        working_directory::ProjectionState::At(cwd) => Value::String(cwd),
        working_directory::ProjectionState::Null => Value::Null,
    }
}

/// The describe surface's method names (the registration reality: the
/// two shell tools' bands, the read/write/edit/glob/grep bands, the
/// spill listeners, the editor band, the todo band, the goal band, the
/// three job bands with their finalize listeners, and the two
/// projection folds plus the jobless fold; excluded surfaces never
/// appear here).
/// The pre-16d band rows (the assembly's standing face).
const DESCRIBE_BASE_METHODS: [&str; 91] = [
    "describe",
    "bash.execute",
    "bash.render",
    "bash.presentResult",
    "bash.presentCall",
    "presentationMeta",
    "pwsh.execute",
    "pwsh.render",
    "pwsh.presentCall",
    "pwsh.presentResult",
    "read.execute",
    "read.render",
    "read.presentCall",
    "read.presentResult",
    "read.presentationMeta",
    "read_image.execute",
    "read_image.render",
    "read_image.presentCall",
    "read_image.presentationMeta",
    "write.execute",
    "write.render",
    "write.presentCall",
    "write.presentResult",
    "write.presentationMeta",
    "edit.execute",
    "edit.render",
    "edit.presentCall",
    "edit.presentResult",
    "edit.presentationMeta",
    "glob.execute",
    "glob.render",
    "glob.presentCall",
    "glob.presentResult",
    "glob.presentationMeta",
    "glob.spill",
    "grep.execute",
    "grep.render",
    "grep.presentCall",
    "grep.presentResult",
    "grep.presentationMeta",
    "grep.spill",
    "str_replace_editor.execute",
    "str_replace_editor.render",
    "str_replace_editor.presentCall",
    "todo.execute",
    "todo.render",
    "todo.presentCall",
    "get_goal.execute",
    "get_goal.render",
    "get_goal.presentCall",
    "create_goal.execute",
    "create_goal.render",
    "create_goal.presentCall",
    "update_goal.execute",
    "update_goal.render",
    "update_goal.presentCall",
    "job_output.execute",
    "job_output.render",
    "job_output.presentCall",
    "job_list.execute",
    "job_list.render",
    "job_list.presentCall",
    "job_kill.execute",
    "job_kill.render",
    "job_kill.presentCall",
    "job_output.finalize",
    "job_kill.finalize",
    "schedule_create.execute",
    "schedule_create.render",
    "schedule_create.presentCall",
    "schedule_list.execute",
    "schedule_list.render",
    "schedule_list.presentCall",
    "schedule_delete.execute",
    "schedule_delete.render",
    "schedule_delete.presentCall",
    "schedule_update.execute",
    "schedule_update.render",
    "schedule_update.presentCall",
    "todos.fold",
    "workingDirectory.fold",
    "goal.fold",
    "ask_user_question.execute",
    "ask_user_question.render",
    "exit_plan_mode.execute",
    "exit_plan_mode.render",
    "exit_plan_mode.presentCall",
    "exit_plan_mode.presentResult",
    "userQuestions.fold",
    "plan.fold",
    "plan-policy.text",
];

/// The 16d batch rows: the five session-query tools and the observe
/// face.
const DESCRIBE_SESSION_METHODS: [&str; 16] = [
    "session_search.execute",
    "session_search.render",
    "session_search.presentCall",
    "session_event_search.execute",
    "session_event_search.render",
    "session_event_search.presentCall",
    "session_trace.execute",
    "session_trace.render",
    "session_trace.presentCall",
    "session_event_trace.execute",
    "session_event_trace.render",
    "session_event_trace.presentCall",
    "session_event_read.execute",
    "session_event_read.render",
    "session_event_read.presentCall",
    "session-query.observe",
];

const fn describe_methods() -> [&'static str; 107] {
    let mut index = 0;
    let mut methods: [&'static str; 107] = [""; 107];
    while index < 91 {
        methods[index] = DESCRIBE_BASE_METHODS[index];
        index += 1;
    }
    let mut session_index = 0;
    while session_index < 16 {
        methods[91 + session_index] = DESCRIBE_SESSION_METHODS[session_index];
        session_index += 1;
    }
    methods
}

#[cfg(test)]
mod tests {
    /// The declared surface never names an excluded tool: `plugin_market`
    /// (PRD 19.1) stays absent while the shell group rides its two bands
    /// (the builtin-tools contract's exclusion clause).
    #[test]
    fn describe_surface_excludes_plugin_market() {
        let methods = super::describe_methods();
        assert!(
            methods.iter().all(|name| !name.contains("plugin_market")),
            "{methods:?}"
        );
        assert!(methods.contains(&"bash.execute"));
        assert!(methods.contains(&"pwsh.execute"));
    }
}
