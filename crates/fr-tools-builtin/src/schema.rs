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

/// The read parameter table (the catalog-frozen base; the caps are the
/// baked dsh defaults, so the frozen `limit` text carries them).
#[must_use]
pub fn read_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "file_path": {
                "type": "string",
                "description": catalog_gen::READ_FILE_PATH_TEXT,
            },
            "offset": {
                "type": "number",
                "description": catalog_gen::READ_OFFSET_TEXT,
            },
            "limit": {
                "type": "number",
                "description": catalog_gen::READ_LIMIT_TEXT,
            },
        },
        "required": ["file_path"],
    })
}

/// The write parameter table: the catalog-frozen base plus the
/// escalation pair while the composition confines (the dsh
/// `sandbox.escalationModes.length > 0` spread).
#[must_use]
pub fn write_parameters(escalation: bool) -> Value {
    let (permissions, justification) = crate::fs_sandbox::schema_fields();
    let mut properties = serde_json::Map::new();
    properties.insert(
        "file_path".to_owned(),
        json!({
            "type": "string",
            "description": catalog_gen::WRITE_FILE_PATH_TEXT,
        }),
    );
    properties.insert(
        "content".to_owned(),
        json!({
            "type": "string",
            "description": catalog_gen::WRITE_CONTENT_TEXT,
        }),
    );
    if escalation {
        properties.insert("sandbox_permissions".to_owned(), permissions);
        properties.insert("justification".to_owned(), justification);
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": ["file_path", "content"],
    })
}

/// The edit parameter table (the write twin over the edit texts).
#[must_use]
pub fn edit_parameters(escalation: bool) -> Value {
    let (permissions, justification) = crate::fs_sandbox::schema_fields();
    let mut properties = serde_json::Map::new();
    properties.insert(
        "file_path".to_owned(),
        json!({
            "type": "string",
            "description": catalog_gen::EDIT_FILE_PATH_TEXT,
        }),
    );
    properties.insert(
        "old_string".to_owned(),
        json!({
            "type": "string",
            "description": catalog_gen::EDIT_OLD_STRING_TEXT,
        }),
    );
    properties.insert(
        "new_string".to_owned(),
        json!({
            "type": "string",
            "description": catalog_gen::EDIT_NEW_STRING_TEXT,
        }),
    );
    properties.insert(
        "replace_all".to_owned(),
        json!({
            "type": "boolean",
            "description": catalog_gen::EDIT_REPLACE_ALL_TEXT,
        }),
    );
    if escalation {
        properties.insert("sandbox_permissions".to_owned(), permissions);
        properties.insert("justification".to_owned(), justification);
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": ["file_path", "old_string", "new_string"],
    })
}

/// The `read_image` parameter table (the dsh `applyReadImageTool`
/// parameters).
#[must_use]
pub fn read_image_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "file_path": {
                "type": "string",
                "description": catalog_gen::READ_IMAGE_FILE_PATH_TEXT,
            },
        },
        "required": ["file_path"],
    })
}

/// The `read_image` structured outcome (the dsh `IMAGE_VALUE_SCHEMA`).
#[must_use]
pub fn read_image_value_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": true,
        "properties": {
            "attachmentId": { "type": "string", "required": true },
            "mediaType": {
                "type": "string",
                "enum": ["image/png", "image/jpeg", "image/webp", "image/gif"],
                "required": true,
            },
            "bytes": { "type": "integer", "required": true },
            "width": { "type": "integer", "required": true },
            "height": { "type": "integer", "required": true },
            "name": { "type": "string" },
            "originalDimensions": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "width": { "type": "integer", "required": true },
                    "height": { "type": "integer", "required": true },
                },
            },
        },
    })
}

/// The `read_image` output schema (the dsh read-image `output.schema`).
#[must_use]
pub fn read_image_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": {
                "type": "string",
                "required": true,
                "description": "Canonical absolute path in the filesystem execution world.",
            },
            "image": read_image_value_schema(),
        },
    })
}

/// The read output contract (the dsh `defineTool` `output.schema`,
/// verbatim shapes).
#[must_use]
pub fn read_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": {
                "type": "string",
                "required": true,
                "description": "Canonical absolute path in the filesystem execution world.",
            },
            "offset": { "type": "integer", "required": true },
            "lines": {
                "type": "array",
                "required": true,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "number": { "type": "integer", "required": true },
                        "text": { "type": "string", "required": true },
                    },
                },
            },
            "totalLines": { "type": "integer", "required": true },
        },
    })
}

/// The write output contract (the dsh `defineTool` `output.schema`).
#[must_use]
pub fn write_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": {
                "type": "string",
                "required": true,
                "description": "Canonical absolute path in the filesystem execution world.",
            },
            "operation": { "type": "string", "required": true, "enum": ["create", "update"] },
            "before": {
                "required": true,
                "oneOf": [{ "type": "string" }, { "type": "null" }],
            },
            "after": { "type": "string", "required": true },
        },
    })
}

/// The edit output contract (the dsh `defineTool` `output.schema`).
#[must_use]
pub fn edit_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": {
                "type": "string",
                "required": true,
                "description": "Canonical absolute path in the filesystem execution world.",
            },
            "before": { "type": "string", "required": true },
            "after": { "type": "string", "required": true },
        },
    })
}

/// The glob tool description (the composed registration fact).
///
/// The catalog-frozen base carries the sampled over-cap phrase; the
/// baked bundle switch swaps exactly that phrase (the pwsh-appendix
/// composition judgment). The baked cap count equals the catalog's
/// frozen 100, so the counts ride the frozen text.
#[must_use]
pub fn glob_description(sample_over_cap: bool) -> String {
    let base = catalog_gen::GLOB_DESCRIPTION.to_owned();
    if sample_over_cap {
        return base;
    }
    base.replace(
        catalog_gen::GLOB_OVERCAP_SAMPLED_TEXT,
        catalog_gen::GLOB_OVERCAP_HEAD_TEXT,
    )
}

/// The grep tool description (the catalog-frozen constant; the baked
/// match cap equals the catalog's frozen 250).
#[must_use]
pub fn grep_description() -> String {
    catalog_gen::GREP_DESCRIPTION.to_owned()
}

/// The `str_replace_editor` description (the catalog-frozen default).
#[must_use]
pub fn str_replace_editor_description() -> String {
    catalog_gen::STR_REPLACE_EDITOR_DESCRIPTION.to_owned()
}

/// The glob parameter table (the catalog-frozen base).
#[must_use]
pub fn glob_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "pattern": {
                "type": "string",
                "description": catalog_gen::GLOB_PATTERN_TEXT,
            },
            "path": {
                "type": "string",
                "description": catalog_gen::GLOB_PATH_TEXT,
            },
        },
        "required": ["pattern"],
    })
}

/// The grep parameter table (the catalog-frozen base).
#[must_use]
pub fn grep_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "pattern": {
                "type": "string",
                "description": catalog_gen::GREP_PATTERN_TEXT,
            },
            "path": {
                "type": "string",
                "description": catalog_gen::GREP_PATH_TEXT,
            },
            "include": {
                "type": "string",
                "description": catalog_gen::GREP_INCLUDE_TEXT,
            },
        },
        "required": ["pattern"],
    })
}

/// The `str_replace_editor` parameter table (the catalog-frozen base; the
/// five command-scoped keys are the `oneOf` null-tolerant shapes).
#[must_use]
pub fn str_replace_editor_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "command": {
                "type": "string",
                "enum": ["view", "create", "str_replace", "insert"],
                "description": catalog_gen::STR_REPLACE_EDITOR_COMMAND_TEXT,
            },
            "path": {
                "type": "string",
                "description": catalog_gen::STR_REPLACE_EDITOR_PATH_TEXT,
            },
            "file_text": {
                "oneOf": [{ "type": "string" }, { "type": "null" }],
                "description": catalog_gen::STR_REPLACE_EDITOR_FILE_TEXT_TEXT,
            },
            "insert_line": {
                "oneOf": [{ "type": "integer" }, { "type": "null" }],
                "description": catalog_gen::STR_REPLACE_EDITOR_INSERT_LINE_TEXT,
            },
            "new_str": {
                "oneOf": [{ "type": "string" }, { "type": "null" }],
                "description": catalog_gen::STR_REPLACE_EDITOR_NEW_STR_TEXT,
            },
            "old_str": {
                "oneOf": [{ "type": "string" }, { "type": "null" }],
                "description": catalog_gen::STR_REPLACE_EDITOR_OLD_STR_TEXT,
            },
            "view_range": {
                "oneOf": [
                    { "type": "array", "items": { "type": "integer" } },
                    { "type": "null" },
                ],
                "description": catalog_gen::STR_REPLACE_EDITOR_VIEW_RANGE_TEXT,
            },
        },
        "required": ["command", "path"],
    })
}

/// The glob output contract (the dsh `defineTool` `output.schema`).
#[must_use]
pub fn glob_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "root": { "type": "string", "required": true },
            "paths": {
                "type": "array",
                "required": true,
                "items": { "type": "string" },
            },
        },
    })
}

/// The grep output contract (the dsh `defineTool` `output.schema`).
#[must_use]
pub fn grep_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "matches": {
                "type": "array",
                "required": true,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "path": { "type": "string", "required": true },
                        "lineNumber": { "type": "integer", "required": true },
                        "line": { "type": "string", "required": true },
                    },
                },
            },
        },
    })
}

/// The `str_replace_editor` output contract (the dsh `output.schema`):
/// the plain string.
#[must_use]
pub fn str_replace_editor_output_schema() -> Value {
    json!({ "type": "string" })
}

/// The `todo_write` description for one activation (dsh `describe()`).
///
/// The head, the policy's active-status clause, and the tail join in
/// source order; the composed true-variant equals the catalog-verbatim
/// constant (the freeze test).
#[must_use]
pub fn todo_write_description(allow_parallel: bool) -> String {
    let middle = if allow_parallel {
        crate::catalog_gen::TODO_WRITE_DESCRIPTION_PARALLEL
    } else {
        crate::catalog_gen::TODO_WRITE_DESCRIPTION_SINGLE
    };
    format!(
        "{}{}{}",
        crate::catalog_gen::TODO_WRITE_DESCRIPTION_HEAD,
        middle,
        crate::catalog_gen::TODO_WRITE_DESCRIPTION_TAIL
    )
}

/// The `todo_write` parameter table (the catalog-frozen base; the tool
/// carries no escalation pair and no conditional composition).
#[must_use]
pub fn todo_write_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "todos": {
                "type": "array",
                "description": crate::catalog_gen::TODO_WRITE_TODOS_TEXT,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "content": {
                            "type": "string",
                            "description": "What the task is — a short imperative line.",
                        },
                        "status": {
                            "type": "string",
                            "description": "pending (not started) | in_progress (now) | completed (done).",
                            "enum": crate::todo::STATUSES,
                        },
                    },
                    "required": ["content", "status"],
                },
            },
        },
        "required": ["todos"],
    })
}

/// The `todo_write` output contract (the dsh `output.schema`, the
/// schemastery row form): the canonical list plus the status counts; the
/// `inProgress` wire key is the protocol exemption.
#[must_use]
pub fn todo_write_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "todos": {
                "type": "array",
                "required": true,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "content": { "type": "string", "required": true },
                        "status": {
                            "type": "string",
                            "required": true,
                            "enum": crate::todo::STATUSES,
                        },
                    },
                },
            },
            "counts": {
                "type": "object",
                "additionalProperties": false,
                "required": true,
                "properties": {
                    "pending": { "type": "integer", "required": true },
                    "inProgress": { "type": "integer", "required": true },
                    "completed": { "type": "integer", "required": true },
                },
            },
        },
    })
}

/// The `tool:goal` section text for one deployment threshold (the dsh
/// `guidance(blockedAfter)` composition over the extracted template).
#[must_use]
pub fn goal_section_text(blocked_after: i64) -> String {
    format!(
        "{}{blocked_after}{}",
        crate::catalog_gen::GOAL_SECTION_TEXT_HEAD,
        crate::catalog_gen::GOAL_SECTION_TEXT_TAIL
    )
}

/// The `get_goal` parameter table (the catalog base; no parameters).
///
/// # Panics
///
/// Never in practice: the frozen table is the generator's canonical
/// output, so the parse cannot drift.
#[must_use]
pub fn get_goal_parameters() -> Value {
    serde_json::from_str(crate::catalog_gen::GET_GOAL_PARAMETERS_JSON)
        .expect("the frozen get_goal table parses")
}

/// The `create_goal` parameter table (the catalog-frozen base).
#[must_use]
pub fn create_goal_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "objective": {
                "type": "string",
                "description": crate::catalog_gen::CREATE_GOAL_OBJECTIVE_TEXT,
            },
            "max_goal_rounds": {
                "type": "number",
                "description": crate::catalog_gen::CREATE_GOAL_MAX_GOAL_ROUNDS_TEXT,
            },
        },
        "required": ["objective"],
    })
}

/// The `update_goal` parameter table (the catalog-frozen base).
#[must_use]
pub fn update_goal_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "goal_id": {
                "type": "string",
                "description": crate::catalog_gen::UPDATE_GOAL_GOAL_ID_TEXT,
            },
            "revision": {
                "type": "number",
                "description": crate::catalog_gen::UPDATE_GOAL_REVISION_TEXT,
            },
            "action": {
                "type": "string",
                "description": crate::catalog_gen::UPDATE_GOAL_ACTION_TEXT,
                "enum": crate::goal_tools::UPDATE_ACTIONS,
            },
            "objective": {
                "type": "string",
                "description": crate::catalog_gen::UPDATE_GOAL_OBJECTIVE_TEXT,
            },
            "max_goal_rounds": {
                "type": "number",
                "description": crate::catalog_gen::UPDATE_GOAL_MAX_GOAL_ROUNDS_TEXT,
            },
            "blocked_reason": {
                "type": "string",
                "description": crate::catalog_gen::UPDATE_GOAL_BLOCKED_REASON_TEXT,
            },
        },
        "required": ["goal_id", "revision", "action"],
    })
}

/// The canonical goal-tool output contract (the dsh `GOAL_VALUE_SCHEMA`
/// oneOf shape; the schemastery row form with the `required` flags).
#[must_use]
pub fn goal_output_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "goal": { "type": "null", "required": true },
                },
            },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "goal": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": true,
                        "properties": {
                            "id": { "type": "string", "required": true },
                            "revision": { "type": "integer", "required": true },
                            "objective": { "type": "string", "required": true },
                            "phase": {
                                "type": "string",
                                "required": true,
                                "enum": crate::goal::PHASES,
                            },
                            "roundsStarted": { "type": "integer", "required": true },
                            "maxGoalRounds": { "type": "integer", "required": true },
                            "blockedReason": {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "code": { "type": "string", "required": true },
                                    "message": { "type": "string", "required": true },
                                },
                            },
                        },
                    },
                    "activation": {
                        "type": "string",
                        "required": true,
                        "enum": ["armed", "disarmed"],
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
