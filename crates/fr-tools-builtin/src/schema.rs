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

use crate::catalog_gen;
use crate::tools::timeout_ms_description;

/// The bash tool description (the catalog-frozen constant; the
/// managed-variable sentence carries the FR_ prefix).
#[must_use]
pub fn bash_description() -> String {
    catalog_gen::BASH_DESCRIPTION.to_owned()
}

/// The pwsh tool description (the catalog-frozen base constant).
///
/// The base sentences ride every registration; the Windows-sandbox
/// appendix rides when a confining executor is mounted (every fr
/// composition with confinement; the appendix is the composed source
/// text — the catalog documents the base registration alone).
#[must_use]
pub fn pwsh_description(windows_sandbox: bool) -> String {
    let base = catalog_gen::PWSH_DESCRIPTION.to_owned();
    if !windows_sandbox {
        return base;
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

/// The bash parameter table: the catalog-frozen base plus the
/// registration facts (the background key while a job registry backs
/// the tool; the escalation pair while the composition confines).
#[must_use]
pub fn bash_parameters(background: bool, escalation: bool, promote: bool) -> Value {
    shell_parameters(
        background,
        escalation,
        promote,
        catalog_gen::BASH_COMMAND_TEXT,
        catalog_gen::BASH_DESCRIPTION_PROPERTY_TEXT,
    )
}

/// The pwsh parameter table (the bash twin over the PowerShell texts).
#[must_use]
pub fn pwsh_parameters(background: bool, escalation: bool, promote: bool) -> Value {
    shell_parameters(
        background,
        escalation,
        promote,
        catalog_gen::PWSH_COMMAND_TEXT,
        catalog_gen::PWSH_DESCRIPTION_PROPERTY_TEXT,
    )
}

/// Builds one tool's parameter table (dsh `parameters`): the shared
/// shell core over the two catalog-frozen per-tool texts, plus the
/// background key while a job registry backs the tool, plus the
/// escalation pair while the composition confines. The workdir and
/// `run_in_background` texts are byte-identical across the twins, so the
/// shared builder reads the bash constants (the per-tool diff tests pin
/// both spellings against the catalog).
fn shell_parameters(
    background: bool,
    escalation: bool,
    promote: bool,
    command_text: &str,
    description_property_text: &str,
) -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "description".to_owned(),
        json!({
            "type": "string",
            "description": description_property_text,
        }),
    );
    properties.insert(
        "command".to_owned(),
        json!({ "type": "string", "description": command_text }),
    );
    properties.insert(
        "timeoutMs".to_owned(),
        json!({ "type": "number", "description": timeout_ms_description(promote) }),
    );
    properties.insert(
        "workdir".to_owned(),
        json!({
            "type": "string",
            "description": catalog_gen::BASH_WORKDIR_TEXT,
        }),
    );
    if background {
        properties.insert(
            "run_in_background".to_owned(),
            json!({
                "type": "boolean",
                "description": catalog_gen::BASH_RUN_IN_BACKGROUND_TEXT
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
        let parameters = bash_parameters(true, true, true);
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
