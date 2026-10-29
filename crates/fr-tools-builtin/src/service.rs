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
/// seams and the two in-crate domains (the domains ride `Arc` so the
/// activation shares them with the wire seam implementation).
pub struct BuiltinTools {
    seams: Box<dyn tools::Seams>,
    env: std::sync::Arc<ShellEnvRegistry>,
    directories: std::sync::Arc<working_directory::WorkingDirectories>,
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
    pub fn new(
        seams: Box<dyn tools::Seams>,
        env: std::sync::Arc<ShellEnvRegistry>,
        directories: std::sync::Arc<working_directory::WorkingDirectories>,
    ) -> Result<Self, String> {
        Ok(Self {
            seams,
            env,
            directories,
        })
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

    /// Serves one method invocation.
    ///
    /// # Errors
    ///
    /// The payload decode refusal, an unknown method, or the tool
    /// body's own failure (the wire body-error shape).
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
                "methods": describe_methods(),
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
            other => return Err(format!("unknown {PLUGIN_NAME} method {other}")),
        };
        serde_json::to_vec(&reply)
            .map_err(|error| format!("builtin-tools reply encode failed: {error}"))
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
/// two shell tools' bands and the projection fold; excluded surfaces
/// never appear here).
const fn describe_methods() -> [&'static str; 11] {
    [
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
        "workingDirectory.fold",
    ]
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
