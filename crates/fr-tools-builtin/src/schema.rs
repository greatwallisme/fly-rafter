// Rust guideline compliant 2026-10-29
//! The bash and pwsh tool schemas (dsh `tool-bash` + `tool-pwsh`
//! definition surfaces, verbatim texts under the Q16a-1A FR_ prefix
//! ruling).
//!
//! The model-facing parameter table is the enforced JSON Schema
//! subset; the output contract is the dsh value-schema DSL (per-property
//! `required`, `oneOf` unions, explicit `additionalProperties`).
//! Q97-A pins each tool's complete table; the variants here are the
//! registration-time facts dsh bakes in: the job-backed composition
//! always advertises `run_in_background`, and a confining executor
//! always advertises the escalation pair.

use serde_json::{Value, json};

use crate::tools::timeout_ms_description;

/// The bash tool description (dsh `bashDescription`, nine sentences;
/// the managed-variable sentence carries the FR_ prefix).
#[must_use]
pub fn bash_description() -> String {
    concat!(
        "Execute a bash command (`bash -c`) and return its stdout/stderr. ",
        "Each call runs in a fresh shell; pass `workdir` instead of using `cd`. ",
        "Managed `$FR_*` variables expose current harness environment facts. ",
        "Long output is truncated to its tail; the full output is saved to a file whose path is reported when available. ",
        "Provide `description` before `command` in the arguments. ",
        "Before any delete or move, verify that the resolved absolute target path is the intended one; never run it against a computed path you have not checked. ",
        "An unset variable expands to an empty string, so guard variables in such paths with `${VAR:?}`. ",
        "Commands may run under a file sandbox; a blocked file operation is reported as ",
        "`[sandbox: file access denied under <mode> mode]`, a policy denial: do not retry another way."
    )
    .to_owned()
}

/// The pwsh tool description (dsh `pwshDescription`).
///
/// The base sentences ride every registration; the Windows-sandbox
/// appendix rides when a confining executor is mounted (every fr
/// composition with confinement; `$env:DSH_*` reads `$env:FR_*` under
/// the prefix ruling).
#[must_use]
pub fn pwsh_description(windows_sandbox: bool) -> String {
    let base = concat!(
        "Execute a PowerShell command (`pwsh -Command`) and return its stdout/stderr. ",
        "Each call runs in a fresh pwsh process; pass `workdir` instead of using `cd`. ",
        "Paths use native Windows form (`C:\\...`); read environment variables with `$env:NAME`. ",
        "Managed `$env:FR_*` variables expose current harness environment facts. ",
        "Long output is truncated to its tail; the full output is saved to a file whose path is reported when available. ",
        "On Windows a force-killed command settles as `[exit code: 1]` without a signal marker — treat it as an interruption, not a command failure. ",
        "Provide `description` before `command` in the arguments. ",
        "Before any delete or move, verify that the resolved absolute target path is the intended one; never run it against a computed path you have not checked. ",
        "Do not assign to automatic variables such as `$HOME`; variable names are case-insensitive, so `$home` is the same read-only variable. ",
        "Commands may run under a file sandbox; a blocked file operation is reported as ",
        "`[sandbox: file access denied under <mode> mode]`, a policy denial: do not retry another way."
    );
    if !windows_sandbox {
        return base.to_owned();
    }
    concat!(
        " Under the Windows sandbox, read-only pwsh runs in PowerShell ConstrainedLanguage mode, while ",
        "workspace-write stays in FullLanguage unless host policy says otherwise. In read-only, prefer cmdlets and core types (`[string]`, `[datetime]`, `[regex]`, `[guid]`); ",
        ".NET static calls (`[System.IO.*]::`, `[math]::`), `Add-Type`, COM objects, and reflection fail ",
        "with \"only core types\" errors. `-f` formatting, property access, and core cmdlets work. ",
        "In both confined modes, programs cannot open named pipes, so a command that captures another ",
        "program's output through piped stdio (Node.js `child_process.spawn`/`exec` with the default ",
        "`stdio: 'pipe'`) fails with EPERM, while `stdio: 'inherit'` and `stdio: 'ignore'` spawns ",
        "work and PowerShell's own pipelines are unaffected. That EPERM is the documented boundary: ",
        "do not retry the command another way — escalate the exact command once or restructure it to ",
        "avoid capturing output."
    )
    .to_owned()
}

/// Builds one tool's parameter table (dsh `parameters`): the shared
/// shell core, plus the background key while a job registry backs the
/// tool, plus the escalation pair while the composition confines.
#[must_use]
pub fn shell_parameters(
    background: bool,
    escalation: bool,
    promote: bool,
    command_word: &str,
) -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "description".to_owned(),
        json!({
            "type": "string",
            "description": "Clear, concise description of what this command does in active voice, \
5-10 words (shown in the UI). Examples: \"ls\" → \"List files in current directory\"; \
\"git status\" → \"Show working tree status\"; \"npm install\" → \"Install package dependencies\"."
        }),
    );
    properties.insert(
        "command".to_owned(),
        json!({ "type": "string", "description": format!("The {command_word} command to execute.") }),
    );
    properties.insert(
        "timeoutMs".to_owned(),
        json!({ "type": "number", "description": timeout_ms_description(promote) }),
    );
    properties.insert(
        "workdir".to_owned(),
        json!({
            "type": "string",
            "description": "Working directory for this command. Defaults to the session workspace; \
        a relative path is resolved against it."
        }),
    );
    if background {
        properties.insert(
            "run_in_background".to_owned(),
            json!({
                "type": "boolean",
                "description": "Run in the background and return a job id immediately \
            (collect with job_output, stop with job_kill). No timeout applies."
            }),
        );
    }
    if escalation {
        properties.insert(
            "sandbox_permissions".to_owned(),
            json!({
                "type": "string",
                "enum": ["workspace-write", "danger-full-access"],
                "description": fr_sandbox::escalation::sandbox_permissions_description("command"),
            }),
        );
        properties.insert(
            "justification".to_owned(),
            json!({
                "type": "string",
                "description": "Required with sandbox_permissions: one sentence for the user \
            explaining why this exact command needs the wider access. \
            Use the language of the user’s current request."
            }),
        );
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": ["description", "command"],
    })
}

/// The shared shell-tool output contract (dsh `output.schema`, the one
/// `oneOf` over the background handle, the promoted hand-off, and the
/// canonical foreground split).
#[must_use]
pub fn shell_output_schema() -> Value {
    let stream = json!({
        "type": "object",
        "additionalProperties": false,
        "required": true,
        "properties": {
            "text": { "type": "string", "required": true },
            "truncated": { "type": "boolean", "required": true },
            "spillPath": { "type": "string" },
        },
    });
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind": { "type": "string", "required": true, "const": "background" },
                    "jobId": { "type": "string", "required": true },
                    "cwd": { "type": "string", "required": true },
                },
            },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind": { "type": "string", "required": true, "const": "promoted" },
                    "cwd": { "type": "string", "required": true },
                    "jobId": { "type": "string", "required": true },
                    "timeoutMs": { "type": "number", "required": true },
                    "output": { "type": "string", "required": true },
                },
            },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind": { "type": "string", "required": true, "const": "foreground" },
                    "cwd": { "type": "string", "required": true },
                    "exitCode": { "required": true, "oneOf": [{ "type": "integer" }, { "type": "null" }] },
                    "signal": { "required": true, "oneOf": [{ "type": "string" }, { "type": "null" }] },
                    "timedOut": { "type": "boolean", "required": true },
                    "aborted": { "type": "boolean", "required": true },
                    "stopped": { "type": "string" },
                    "timeoutMs": { "type": "number", "required": true },
                    "stdout": stream,
                    "stderr": stream,
                    "sandbox": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "mode": { "type": "string", "required": true },
                            "denied": { "type": "boolean", "required": true },
                            "enforcement": { "type": "string" },
                            "runnerFailed": { "type": "boolean" },
                        },
                    },
                },
            },
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bash table pins the nine-sentence description and the
    /// full-properties foreground composition.
    #[test]
    fn bash_table_pins_the_description_and_full_properties() {
        let description = bash_description();
        assert!(description.starts_with("Execute a bash command (`bash -c`)"));
        assert!(description.contains("Managed `$FR_*` variables"));
        assert!(description.ends_with("do not retry another way."));
        let parameters = shell_parameters(true, true, true, "bash");
        let keys: Vec<&str> = parameters["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            vec![
                "description",
                "command",
                "timeoutMs",
                "workdir",
                "run_in_background",
                "sandbox_permissions",
                "justification"
            ]
        );
        assert_eq!(
            parameters["properties"]["sandbox_permissions"]["enum"],
            json!(["workspace-write", "danger-full-access"])
        );
        assert_eq!(parameters["required"], json!(["description", "command"]));
    }

    /// The pwsh description carries the Windows appendix only when a
    /// confining executor is mounted.
    #[test]
    fn pwsh_description_gates_the_windows_appendix() {
        let base = pwsh_description(false);
        assert!(base.starts_with("Execute a PowerShell command (`pwsh -Command`)"));
        assert!(base.contains("Managed `$env:FR_*` variables"));
        assert!(!base.contains("ConstrainedLanguage"));
        let full = pwsh_description(true);
        assert!(full.contains("ConstrainedLanguage"));
        assert!(full.ends_with("avoid capturing output."));
    }

    /// The output contract's three union arms carry the exact key sets.
    #[test]
    fn output_schema_pins_the_three_union_arms() {
        let schema = shell_output_schema();
        let arms = schema["oneOf"].as_array().expect("oneOf");
        assert_eq!(arms.len(), 3);
        let background = arms[0]["properties"].as_object().expect("background");
        assert_eq!(
            background.keys().collect::<Vec<_>>(),
            vec!["kind", "jobId", "cwd"]
        );
        let promoted = arms[1]["properties"].as_object().expect("promoted");
        assert_eq!(
            promoted.keys().collect::<Vec<_>>(),
            vec!["kind", "cwd", "jobId", "timeoutMs", "output"]
        );
        let foreground = arms[2]["properties"].as_object().expect("foreground");
        assert_eq!(foreground.len(), 11);
        assert_eq!(
            arms[2]["properties"]["stdout"]["properties"]["text"]["required"],
            true
        );
        assert_eq!(arms[0]["additionalProperties"], false);
    }
}
