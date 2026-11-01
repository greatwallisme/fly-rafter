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

/// The `web_search` parameter table (the catalog-frozen base; the query
/// cap inside the description text is the baked dsh default).
#[must_use]
pub fn web_search_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "queries": {
                "type": "array",
                "items": { "type": "string" },
                "description": catalog_gen::WEB_SEARCH_QUERIES_TEXT,
            },
        },
        "required": ["queries"],
    })
}

/// The `web_fetch` parameter table (the catalog-frozen base).
#[must_use]
pub fn web_fetch_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "url": {
                "type": "string",
                "description": catalog_gen::WEB_FETCH_URL_TEXT,
            },
        },
        "required": ["url"],
    })
}

/// The `web_search` output contract (the dsh `defineTool` `output.schema`
/// verbatim shapes).
#[must_use]
pub fn web_search_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "content": { "type": "string" },
            "sources": {
                "type": "array",
                "required": true,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "url": { "type": "string", "required": true },
                        "title": { "type": "string" },
                        "snippet": { "type": "string" },
                        "publishedAt": { "type": "string" },
                    },
                },
            },
            "truncated": { "type": "boolean", "required": true },
        },
    })
}

/// The `web_fetch` output contract (the dsh `defineTool` `output.schema`
/// verbatim shapes; the body union carries the html/text kind arms).
#[must_use]
pub fn web_fetch_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "url": { "type": "string", "required": true },
            "statusCode": { "type": "integer", "required": true },
            "body": {
                "required": true,
                "oneOf": [
                    {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "kind": { "type": "string", "required": true, "const": "html" },
                            "content": { "type": "string", "required": true },
                        },
                    },
                    {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "kind": { "type": "string", "required": true, "const": "text" },
                            "content": { "type": "string", "required": true },
                        },
                    },
                ],
            },
            "truncated": { "type": "boolean", "required": true },
        },
    })
}

/// The `working_directory` parameter table (the catalog-frozen base;
/// the single optional `cd` row).
#[must_use]
pub fn working_directory_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "cd": {
                "type": "string",
                "description": catalog_gen::WORKING_DIRECTORY_CD_TEXT,
            },
        },
    })
}

/// The `working_directory` output contract (the dsh `defineTool`
/// `output.schema` verbatim shape).
#[must_use]
pub fn working_directory_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "cwd": {
                "type": "string",
                "required": true,
                "description": catalog_gen::WORKING_DIRECTORY_CWD_TEXT,
            },
        },
    })
}

/// The `cordis_inspect_list` parameter table (the catalog-frozen base:
/// the empty object).
#[must_use]
pub fn cordis_inspect_list_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {},
    })
}

/// The `cordis_inspect_query` parameter table (the catalog-frozen
/// base: the four rows).
#[must_use]
pub fn cordis_inspect_query_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "platform": {
                "type": "string",
                "required": true,
                "enum": ["host", "client"],
                "description": catalog_gen::CORDIS_INSPECT_QUERY_PLATFORM_TEXT,
            },
            "provider": {
                "type": "string",
                "required": true,
                "description": catalog_gen::CORDIS_INSPECT_QUERY_PROVIDER_TEXT,
            },
            "method": {
                "type": "string",
                "required": true,
                "description": catalog_gen::CORDIS_INSPECT_QUERY_METHOD_TEXT,
            },
            "input": {
                "type": "json",
                "description": catalog_gen::CORDIS_INSPECT_QUERY_INPUT_TEXT,
            },
        },
    })
}

/// The inspect output contract (the dsh `{ type: 'json' }` shape both
/// tools share).
#[must_use]
pub fn inspect_output_schema() -> Value {
    json!({ "type": "json" })
}

/// The `load_workspace_dependencies` parameter table (the
/// catalog-frozen base: the empty object).
#[must_use]
pub fn load_workspace_dependencies_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {},
    })
}

/// The `load_workspace_dependencies` output contract (the dsh
/// `defineTool` `output.schema` verbatim shape).
#[must_use]
pub fn load_workspace_dependencies_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "python": { "type": "string", "required": true },
            "node": { "type": "string", "description": "Absent when the payload ships no Node.js." },
            "pnpm": { "type": "string", "description": "Absent when the payload ships no pnpm." },
            "pythonPackages": { "type": "string", "required": true },
            "nodePackages": { "type": "string", "description": "Absent when the payload ships no Node.js." },
            "pythonDistributions": {
                "type": "object",
                "additionalProperties": true,
                "required": true,
                "description": "Bundled distribution names and versions recorded in runtime.json; excludes user-installed additions.",
            },
        },
    })
}

/// The `plugin_manager` parameter table (the catalog-frozen base:
/// the required action enum over the nine optional rows).
#[must_use]
pub fn plugin_manager_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "required": true,
                "enum": [
                    "list_plugins",
                    "list_bundles",
                    "set_plugin",
                    "set_bundle",
                    "install_bundle",
                    "remove_bundle",
                    "list_version_exemptions",
                    "set_version_exemption",
                ],
                "description": "Management operation.",
            },
            "target": {
                "type": "string",
                "description": "Plugin entry id, bundle package name, or installation spec, according to action. For an Official catalog entry, pass list_bundles installTarget.spec to install_bundle.",
            },
            "enabled": {
                "type": "boolean",
                "description": "Required for set operations; defaults to true for installation. For set_version_exemption, true grants and false revokes.",
            },
            "runtimeVersion": {
                "type": "string",
                "description": "For set_version_exemption: exact DSH version from list_version_exemptions. Target must be the manifest package-name@version, not an alias or version range.",
            },
            "acceptRisk": {
                "type": "boolean",
                "description": "For granting an exemption: true only after warning the user about possible crashes and data loss and receiving explicit permission for this exact plugin/runtime pair. General installation permission is not enough.",
            },
            "approvedBuilds": {
                "type": "array",
                "items": { "type": "string" },
                "description": "For install_bundle: pass names from pendingBuilds only after the user explicitly approves running their install scripts in the conversation. This grants persistent permission for this profile.",
            },
            "registry": {
                "type": "string",
                "description": "For install_bundle: the npm registry URL asked first, when the user names one; otherwise the configured registry is asked, and its configured fallbacks while a registry is unreachable.",
            },
            "offset": {
                "type": "number",
                "description": "Zero-based list offset; defaults to 0.",
            },
            "limit": {
                "type": "number",
                "description": "List page size, from 1 to 100; defaults to 25.",
            },
        },
    })
}

/// The `plugin_manager` output contract (the dsh `defineTool`
/// `output.schema`: the JSON string the execute tail serializes).
#[must_use]
pub fn plugin_manager_output_schema() -> Value {
    json!({ "type": "string" })
}

/// The `lsp` parameter table (the catalog-frozen base: the four rows).
#[must_use]
pub fn lsp_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "operation": {
                "type": "string",
                "required": true,
                "enum": ["goToDefinition", "findReferences", "goToImplementation", "hover"],
                "description": catalog_gen::LSP_OPERATION_TEXT,
            },
            "file_path": {
                "type": "string",
                "required": true,
                "description": catalog_gen::LSP_FILE_PATH_TEXT,
            },
            "line": {
                "type": "number",
                "required": true,
                "description": catalog_gen::LSP_LINE_TEXT,
            },
            "character": {
                "type": "number",
                "required": true,
                "description": catalog_gen::LSP_CHARACTER_TEXT,
            },
        },
    })
}

/// The `lsp` output contract (the dsh `defineTool` `output.schema`
/// verbatim two-arm union over the shared position/range shapes).
#[must_use]
pub fn lsp_output_schema() -> Value {
    let position = |required: bool| {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "line": { "type": "integer", "required": true },
                "character": { "type": "integer", "required": true },
            },
            "required": required,
        })
    };
    let range = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "start": position(true),
            "end": position(true),
        },
    });
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind": { "type": "string", "required": true, "const": "locations" },
                    "locations": {
                        "type": "array",
                        "required": true,
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "uri": { "type": "string", "required": true },
                                "range": {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "properties": {
                                        "start": position(true),
                                        "end": position(true),
                                    },
                                    "required": true,
                                },
                            },
                        },
                    },
                    "resolvedWorkspaceUri": { "type": "string", "required": true },
                },
            },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind": { "type": "string", "required": true, "const": "hover" },
                    "hover": {
                        "required": true,
                        "oneOf": [
                            { "type": "null" },
                            {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "contents": { "type": "string", "required": true },
                                    "range": range,
                                },
                            },
                        ],
                    },
                },
            },
        ],
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

/// The `job_output` parameter table (the catalog-frozen base).
///
/// # Panics
///
/// Panics when the frozen catalog JSON stops parsing — the generation
/// contract, pinned by the verify suite.
#[must_use]
pub fn job_output_parameters() -> Value {
    serde_json::from_str(crate::catalog_gen::JOB_OUTPUT_PARAMETERS_JSON)
        .expect("the frozen job_output table parses")
}

/// The `job_list` parameter table (the catalog-frozen base; no rows).
///
/// # Panics
///
/// Panics when the frozen catalog JSON stops parsing — the generation
/// contract, pinned by the verify suite.
#[must_use]
pub fn job_list_parameters() -> Value {
    serde_json::from_str(crate::catalog_gen::JOB_LIST_PARAMETERS_JSON)
        .expect("the frozen job_list table parses")
}

/// The `job_kill` parameter table (the catalog-frozen base).
///
/// # Panics
///
/// Panics when the frozen catalog JSON stops parsing — the generation
/// contract, pinned by the verify suite.
#[must_use]
pub fn job_kill_parameters() -> Value {
    serde_json::from_str(crate::catalog_gen::JOB_KILL_PARAMETERS_JSON)
        .expect("the frozen job_kill table parses")
}

/// The shared job-control projection row (the dsh `PUBLIC_JOB_SCHEMA`):
/// seven keys, the terminal-status enum, camelCase timestamps.
#[must_use]
pub fn public_job_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "id": { "type": "string", "required": true },
            "kind": { "type": "string", "required": true },
            "label": { "type": "string", "required": true },
            "status": {
                "type": "string",
                "required": true,
                "enum": ["running", "stopping", "completed", "killed", "failed"],
            },
            "detail": { "type": "string" },
            "startedAt": { "type": "integer", "required": true },
            "finishedAt": { "type": "integer" },
        },
    })
}

/// The `job_output` output contract (the dsh `output.schema`).
#[must_use]
pub fn job_output_schema() -> Value {
    let mut row = public_job_schema();
    row["required"] = json!(true);
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "text": { "type": "string", "required": true },
            "job": row,
        },
    })
}

/// The `job_list` output contract (the dsh `output.schema` array).
#[must_use]
pub fn job_list_schema() -> Value {
    json!({ "type": "array", "items": public_job_schema() })
}

/// The `job_kill` output contract (the dsh `output.schema`).
#[must_use]
pub fn job_kill_schema() -> Value {
    let mut row = public_job_schema();
    row["required"] = json!(true);
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "outcome": {
                "type": "string",
                "required": true,
                "enum": ["cancellation-requested", "already-finished"],
            },
            "job": row,
        },
    })
}

// The schedule family (dsh `tool-schedule/index.ts`): the four
// parameter tables freeze the catalog rows outright; the output
// contracts ride the dsh source shapes (the schemastery row form with
// property-level `required`).

/// The `schedule_create` parameter table (the catalog-frozen base).
///
/// # Panics
///
/// Panics when the frozen catalog JSON drifts (the generation gate
/// keeps the constant in sync).
#[must_use]
pub fn schedule_create_parameters() -> Value {
    serde_json::from_str(crate::catalog_gen::SCHEDULE_CREATE_PARAMETERS_JSON)
        .expect("the frozen schedule_create table parses")
}

/// The `schedule_delete` parameter table (the catalog-frozen base).
///
/// # Panics
///
/// Panics when the frozen catalog JSON drifts (the generation gate
/// keeps the constant in sync).
#[must_use]
pub fn schedule_delete_parameters() -> Value {
    serde_json::from_str(crate::catalog_gen::SCHEDULE_DELETE_PARAMETERS_JSON)
        .expect("the frozen schedule_delete table parses")
}

/// The `schedule_list` parameter table (the catalog-frozen base).
///
/// # Panics
///
/// Panics when the frozen catalog JSON drifts (the generation gate
/// keeps the constant in sync).
#[must_use]
pub fn schedule_list_parameters() -> Value {
    serde_json::from_str(crate::catalog_gen::SCHEDULE_LIST_PARAMETERS_JSON)
        .expect("the frozen schedule_list table parses")
}

/// The `schedule_update` parameter table (the catalog-frozen base).
///
/// # Panics
///
/// Panics when the frozen catalog JSON drifts (the generation gate
/// keeps the constant in sync).
#[must_use]
pub fn schedule_update_parameters() -> Value {
    serde_json::from_str(crate::catalog_gen::SCHEDULE_UPDATE_PARAMETERS_JSON)
        .expect("the frozen schedule_update table parses")
}

/// The shared view-variant rows (dsh `SHARED_VIEW_PROPERTIES`).
fn schedule_view_properties() -> serde_json::Map<String, Value> {
    serde_json::from_value(json!({
        "id": { "type": "string", "required": true },
        "title": { "type": "string", "required": true },
        "prompt": { "type": "string", "required": true },
        "scheduledAt": { "type": "string", "required": true },
        "state": { "type": "string", "required": true, "enum": ["scheduled", "overdue"] },
        "deliveryMode": { "type": "string", "required": true, "const": "host" },
    }))
    .expect("the shared view rows parse")
}

/// One view variant with the rule rows appended (dsh `AFTER_VIEW_SCHEMA`
/// and its five siblings).
fn schedule_view_schema(rule_rows: Value) -> Value {
    let mut object = serde_json::Map::new();
    object.insert("type".to_owned(), json!("object"));
    object.insert("additionalProperties".to_owned(), json!(false));
    let mut properties = schedule_view_properties();
    if let Value::Object(rule) = rule_rows {
        for (key, value) in rule {
            properties.insert(key, value);
        }
    }
    object.insert("properties".to_owned(), Value::Object(properties));
    Value::Object(object)
}

/// The full view union (dsh `VIEW_SCHEMA`).
fn schedule_view_union() -> Value {
    json!({
        "oneOf": [
            schedule_view_schema(json!({
                "kind": { "type": "string", "required": true, "const": "after" },
                "afterSeconds": { "type": "integer", "required": true },
            })),
            schedule_view_schema(json!({
                "kind": { "type": "string", "required": true, "const": "at" },
            })),
            schedule_view_schema(json!({
                "kind": { "type": "string", "required": true, "const": "every" },
                "everySeconds": { "type": "integer", "required": true },
            })),
            schedule_view_schema(json!({
                "kind": { "type": "string", "required": true, "const": "daily" },
                "time": { "type": "string", "required": true },
                "timeZone": { "type": "string", "required": true },
            })),
            schedule_view_schema(json!({
                "kind": { "type": "string", "required": true, "const": "weekly" },
                "time": { "type": "string", "required": true },
                "timeZone": { "type": "string", "required": true },
                "weekdays": {
                    "type": "array",
                    "required": true,
                    "items": { "type": "integer" },
                },
            })),
            schedule_view_schema(json!({
                "kind": { "type": "string", "required": true, "const": "cron" },
                "expression": { "type": "string", "required": true },
                "timeZone": { "type": "string", "required": true },
            })),
        ]
    })
}

/// One exact two-field error schema (dsh `basicErrorSchema`).
fn schedule_error_schema(code: &str) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "code": { "type": "string", "required": true, "const": code },
            "message": { "type": "string", "required": true },
        },
    })
}

/// The nine-code error union (dsh `ERROR_SCHEMAS`).
fn schedule_error_union() -> Vec<Value> {
    [
        "invalid_prompt",
        "invalid_selector",
        "invalid_rule",
        "invalid_time_zone",
        "not_future",
        "time_out_of_range",
        "frequency_too_high",
        "subagent_session",
        "internal_error",
    ]
    .iter()
    .map(|code| schedule_error_schema(code))
    .collect()
}

/// The `schedule_create` output contract (dsh `CREATE_OUTPUT_SCHEMA`).
#[must_use]
pub fn schedule_create_output_schema() -> Value {
    let mut one_of = vec![schedule_view_union()];
    one_of.extend(schedule_error_union());
    json!({ "oneOf": one_of })
}

/// The `schedule_list` output contract (dsh `LIST_OUTPUT_SCHEMA`).
#[must_use]
pub fn schedule_list_output_schema() -> Value {
    let mut one_of = vec![json!({ "type": "array", "items": schedule_view_union() })];
    one_of.extend(schedule_error_union());
    json!({ "oneOf": one_of })
}

/// The `schedule_delete` output contract (dsh `DELETE_OUTPUT_SCHEMA`).
#[must_use]
pub fn schedule_delete_output_schema() -> Value {
    let mut one_of = vec![
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "id": { "type": "string", "required": true },
                "deleted": { "type": "boolean", "required": true, "const": true },
            },
        }),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "id": { "type": "string", "required": true },
                "deleted": { "type": "boolean", "required": true, "const": false },
                "code": {
                    "type": "string",
                    "required": true,
                    "const": "schedule_not_found",
                },
            },
        }),
    ];
    one_of.extend(schedule_error_union());
    json!({ "oneOf": one_of })
}

/// The `schedule_update` output contract (dsh `UPDATE_OUTPUT_SCHEMA`).
#[must_use]
pub fn schedule_update_output_schema() -> Value {
    let mut one_of = vec![
        schedule_view_union(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "id": { "type": "string", "required": true },
                "updated": { "type": "boolean", "required": true, "const": false },
                "code": {
                    "type": "string",
                    "required": true,
                    "enum": ["schedule_not_found", "schedule_ended", "schedule_conflict"],
                },
            },
        }),
    ];
    one_of.extend(schedule_error_union());
    json!({ "oneOf": one_of })
}

/// The `ask_user_question` parameter table (the catalog-frozen base).
///
/// # Panics
///
/// Panics when the frozen catalog JSON stops parsing — the generation
/// contract, pinned by the verify suite.
#[must_use]
pub fn ask_user_question_parameters() -> Value {
    serde_json::from_str(crate::catalog_gen::ASK_USER_QUESTION_PARAMETERS_JSON)
        .expect("the frozen ask_user_question table parses")
}

/// The `exit_plan_mode` parameter table (the catalog-frozen base).
///
/// # Panics
///
/// Panics when the frozen catalog JSON stops parsing — the generation
/// contract, pinned by the verify suite.
#[must_use]
pub fn exit_plan_mode_parameters() -> Value {
    serde_json::from_str(crate::catalog_gen::EXIT_PLAN_MODE_PARAMETERS_JSON)
        .expect("the frozen exit_plan_mode table parses")
}

/// The legacy `ask_user_question` output contract (the dsh
/// `defineTool` `output.schema`, verbatim shape).
#[must_use]
pub fn ask_user_question_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "answers": {
                "type": "array",
                "required": true,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "id": { "type": "string", "required": true },
                        "selected": {
                            "type": "array",
                            "required": true,
                            "items": { "type": "string" },
                        },
                        "custom": { "type": "string" },
                    },
                },
            },
        },
    })
}

/// The timed variant's `timeout` parameter (dsh
/// `TIMED_WAIT_PARAMETER`; the default rides the baked wait).
#[must_use]
pub fn timed_timeout_parameter(default_seconds: i64) -> Value {
    json!({
        "type": "integer",
        "description": format!(
            "Wait seconds for the entire batch (default {default_seconds}); omit unless the user specifies a duration. Use -1 only when an answer is required before proceeding."
        ),
    })
}

/// The timed variant's output contract (dsh `registerTimedAskUser`
/// `output.schema`: the oneOf pending/answers pair).
#[must_use]
pub fn timed_ask_user_question_output_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "pending": {
                        "type": "boolean",
                        "enum": [true],
                        "required": true,
                        "description": "True when the foreground wait expired before the user submitted an answer batch. The questions remain answerable; this is not a skipped answer.",
                    },
                    "callId": {
                        "type": "string",
                        "required": true,
                        "description": "Tool call whose unanswered questions remain pending.",
                    },
                    "message": {
                        "type": "string",
                        "required": true,
                        "description": "How to continue while the questions stay answerable.",
                    },
                },
            },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "answers": {
                        "type": "array",
                        "required": true,
                        "description": "Submitted answer batch with one item per question. A skipped question has empty selected and no custom; unlike pending, the user has completed the batch.",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "id": {
                                    "type": "string",
                                    "required": true,
                                    "description": "Stable question id echoed from the request.",
                                },
                                "selected": {
                                    "type": "array",
                                    "required": true,
                                    "items": { "type": "string" },
                                    "description": "Selected option labels. Empty with no custom means the user explicitly skipped this question.",
                                },
                                "custom": {
                                    "type": "string",
                                    "description": "Optional free-form answer; omitted for a skipped question.",
                                },
                            },
                        },
                    },
                },
            },
        ],
    })
}

/// The `exit_plan_mode` output contract (dsh plan-mode `output.schema`).
#[must_use]
pub fn exit_plan_mode_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "approved": { "type": "boolean", "required": true },
        },
    })
}

/// The `session_search` parameter table (the catalog-frozen base).
///
/// # Panics
///
/// Panics when the frozen catalog JSON drifts (the generation gate
/// keeps the constant in sync).
#[must_use]
pub fn session_search_parameters() -> Value {
    serde_json::from_str(crate::catalog_gen::SESSION_SEARCH_PARAMETERS_JSON)
        .expect("the frozen session_search table parses")
}

/// The `session_event_search` parameter table (the catalog-frozen
/// base).
///
/// # Panics
///
/// Panics when the frozen catalog JSON drifts.
#[must_use]
pub fn session_event_search_parameters() -> Value {
    serde_json::from_str(crate::catalog_gen::SESSION_EVENT_SEARCH_PARAMETERS_JSON)
        .expect("the frozen session_event_search table parses")
}

/// The `session_trace` parameter table (the catalog-frozen base).
///
/// # Panics
///
/// Panics when the frozen catalog JSON drifts.
#[must_use]
pub fn session_trace_parameters() -> Value {
    serde_json::from_str(crate::catalog_gen::SESSION_TRACE_PARAMETERS_JSON)
        .expect("the frozen session_trace table parses")
}

/// The `session_event_trace` parameter table (the catalog-frozen
/// base).
///
/// # Panics
///
/// Panics when the frozen catalog JSON drifts.
#[must_use]
pub fn session_event_trace_parameters() -> Value {
    serde_json::from_str(crate::catalog_gen::SESSION_EVENT_TRACE_PARAMETERS_JSON)
        .expect("the frozen session_event_trace table parses")
}

/// The `session_event_read` parameter table (the catalog-frozen base).
///
/// # Panics
///
/// Panics when the frozen catalog JSON drifts.
#[must_use]
pub fn session_event_read_parameters() -> Value {
    serde_json::from_str(crate::catalog_gen::SESSION_EVENT_READ_PARAMETERS_JSON)
        .expect("the frozen session_event_read table parses")
}

// The terminal family (dsh `experimental/tool-terminal/index.ts`): the
// parameter tables freeze the catalog rows outright; the output
// contracts ride the dsh source shapes (the schemastery row form with
// property-level `required`, `oneOf` unions, `const` discriminators).

/// The shared terminal session status union (the dsh
/// `SESSION_STATUS_SCHEMA`): running, or exited with code and signal.
#[must_use]
pub fn terminal_session_status_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind": { "type": "string", "required": true, "const": "running" },
                },
            },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind": { "type": "string", "required": true, "const": "exited" },
                    "exitCode": {
                        "required": true,
                        "oneOf": [{ "type": "integer" }, { "type": "null" }],
                    },
                    "signal": {
                        "required": true,
                        "oneOf": [{ "type": "string" }, { "type": "null" }],
                    },
                },
            },
        ],
    })
}

/// The shared session-snapshot property rows (the dsh
/// `SESSION_SNAPSHOT_PROPERTIES`); the caller folds them into the
/// snapshot object or the array items.
#[must_use]
pub fn terminal_snapshot_properties() -> serde_json::Map<String, Value> {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "sessionId".to_owned(),
        json!({ "type": "string", "required": true }),
    );
    properties.insert("name".to_owned(), json!({ "type": "string" }));
    properties.insert(
        "type".to_owned(),
        json!({ "type": "string", "required": true }),
    );
    properties.insert("pid".to_owned(), json!({ "type": "integer" }));
    properties.insert(
        "status".to_owned(),
        json!({ "oneOf": terminal_session_status_schema()["oneOf"], "required": true }),
    );
    properties
}

/// The `terminal_open` parameter table (the catalog-frozen base).
///
/// # Panics
///
/// Panics when the frozen catalog JSON drifts (the generation gate
/// keeps the constant in sync).
#[must_use]
pub fn terminal_open_parameters() -> Value {
    serde_json::from_str(crate::catalog_gen::TERMINAL_OPEN_PARAMETERS_JSON)
        .expect("the frozen terminal_open table parses")
}

/// The `terminal_open` output contract (the snapshot plus the required
/// motd, the dsh spread shape).
#[must_use]
pub fn terminal_open_output_schema() -> Value {
    let mut properties = terminal_snapshot_properties();
    properties.insert(
        "motd".to_owned(),
        json!({ "type": "string", "required": true }),
    );
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
    })
}

/// The `terminal_send` tool description (the composed source
/// conditional: the background sentence rides while the composition
/// bakes the knob on, which the catalog documents).
#[must_use]
pub fn terminal_send_description(background: bool) -> String {
    let base = "Send text to a persistent terminal. By default Enter is submitted and the call waits for a prompt, stdin wait, output silence, timeout, or session exit.";
    if background {
        format!("{base} Background mode returns a job id for job_output/job_kill.")
    } else {
        base.to_owned()
    }
}

/// The `terminal_send` parameter table (the catalog-frozen base; the
/// background key rides while the composition bakes the knob on).
///
/// # Panics
///
/// Panics when the frozen catalog JSON drifts (the generation gate
/// keeps the constant in sync).
#[must_use]
pub fn terminal_send_parameters(background: bool) -> Value {
    let mut table =
        serde_json::from_str::<Value>(crate::catalog_gen::TERMINAL_SEND_PARAMETERS_JSON)
            .expect("the frozen terminal_send table parses");
    if !background {
        table["properties"]
            .as_object_mut()
            .expect("the frozen table carries properties")
            .remove("run_in_background");
    }
    table
}

/// The `terminal_send` output contract (the dsh oneOf: the background
/// acknowledgement or the foreground result).
#[must_use]
pub fn terminal_send_output_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind": { "type": "string", "required": true, "const": "background" },
                    "jobId": { "type": "string", "required": true },
                },
            },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind": { "type": "string", "required": true, "const": "foreground" },
                    "viewport": { "type": "string", "required": true },
                    "waitReason": {
                        "type": "string",
                        "required": true,
                        "enum": ["stdin_read", "inferred_idle", "timeout", "session_exit"],
                    },
                    "sessionStatus": {
                        "oneOf": terminal_session_status_schema()["oneOf"],
                        "required": true,
                    },
                    "truncated": { "type": "boolean", "required": true },
                },
            },
        ],
    })
}

/// The `terminal_read` parameter table (the catalog-frozen base).
///
/// # Panics
///
/// Panics when the frozen catalog JSON drifts.
#[must_use]
pub fn terminal_read_parameters() -> Value {
    serde_json::from_str(crate::catalog_gen::TERMINAL_READ_PARAMETERS_JSON)
        .expect("the frozen terminal_read table parses")
}

/// The `terminal_read` output contract (the bounded scrollback page).
#[must_use]
pub fn terminal_read_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "text": { "type": "string", "required": true },
            "totalLines": { "type": "integer", "required": true },
            "lineBegin": { "type": "integer", "required": true },
            "lineEnd": { "type": "integer", "required": true },
            "truncated": { "type": "boolean", "required": true },
        },
    })
}

/// The `terminal_signal` parameter table (the catalog-frozen base).
///
/// # Panics
///
/// Panics when the frozen catalog JSON drifts.
#[must_use]
pub fn terminal_signal_parameters() -> Value {
    serde_json::from_str(crate::catalog_gen::TERMINAL_SIGNAL_PARAMETERS_JSON)
        .expect("the frozen terminal_signal table parses")
}

/// The `terminal_signal` output contract (the delivered fact).
#[must_use]
pub fn terminal_signal_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "delivered": { "type": "boolean", "required": true, "const": true },
            "targetPgid": { "type": "integer", "required": true },
        },
    })
}

/// The `terminal_close` parameter table (the catalog-frozen base).
///
/// # Panics
///
/// Panics when the frozen catalog JSON drifts.
#[must_use]
pub fn terminal_close_parameters() -> Value {
    serde_json::from_str(crate::catalog_gen::TERMINAL_CLOSE_PARAMETERS_JSON)
        .expect("the frozen terminal_close table parses")
}

/// The `terminal_close` output contract (the closed outcome word).
#[must_use]
pub fn terminal_close_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "sessionId": { "type": "string", "required": true },
            "outcome": {
                "type": "string",
                "required": true,
                "enum": ["closed", "already-closing"],
            },
        },
    })
}

/// The `terminal_list` parameter table (the catalog-frozen base; no
/// rows).
///
/// # Panics
///
/// Panics when the frozen catalog JSON drifts.
#[must_use]
pub fn terminal_list_parameters() -> Value {
    serde_json::from_str(crate::catalog_gen::TERMINAL_LIST_PARAMETERS_JSON)
        .expect("the frozen terminal_list table parses")
}

/// The `terminal_list` output contract (the snapshot array).
#[must_use]
pub fn terminal_list_output_schema() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "additionalProperties": false,
            "properties": terminal_snapshot_properties(),
        },
    })
}

// The persistent twins (dsh `tool-bash-persistent` +
// `tool-pwsh-persistent`): the parameter tables freeze the catalog
// rows; the output contract is the shared plain-string shape.

/// The shared persistent-tool output contract (the dsh
/// `{ type: 'string' }`).
#[must_use]
pub fn persistent_output_schema() -> Value {
    json!({ "type": "string" })
}

/// The persistent `bash` parameter table (the catalog-frozen base).
///
/// # Panics
///
/// Panics when the frozen catalog JSON drifts.
#[must_use]
pub fn persistent_bash_parameters() -> Value {
    serde_json::from_str(crate::catalog_gen::BASH_PERSISTENT_PARAMETERS_JSON)
        .expect("the frozen persistent bash table parses")
}

/// The persistent `pwsh` parameter table (the catalog-frozen base).
///
/// # Panics
///
/// Panics when the frozen catalog JSON drifts.
#[must_use]
pub fn persistent_pwsh_parameters() -> Value {
    serde_json::from_str(crate::catalog_gen::PWSH_PERSISTENT_PARAMETERS_JSON)
        .expect("the frozen persistent pwsh table parses")
}

#[cfg(test)]
mod tests {
    /// The five session-query tables pin their catalog rows (Q97-A:
    /// one full-schema pin per tool, the key sets, the required rows,
    /// and the enum item lists verbatim).
    #[test]
    fn session_query_pins_the_catalog_tables() {
        // session_search: 13 keys, `query` required, the availability
        // and surface enums verbatim.
        let search = super::session_search_parameters();
        let keys = search
            .pointer("/properties")
            .and_then(Value::as_object)
            .expect("session_search properties");
        assert_eq!(keys.len(), 13);
        assert_eq!(
            search["required"],
            serde_json::json!(["query"]),
            "query is the one required row"
        );
        assert_eq!(
            keys.get("query").and_then(|row| row.get("type")),
            Some(&serde_json::json!("string"))
        );
        assert_eq!(
            keys.get("availability")
                .and_then(|row| row.pointer("/items/enum"))
                .and_then(Value::as_array)
                .map(std::vec::Vec::as_slice),
            Some(&[serde_json::json!("live"), serde_json::json!("persisted")][..]),
        );
        assert_eq!(
            keys.get("event_surfaces")
                .and_then(|row| row.pointer("/items/enum"))
                .and_then(Value::as_array)
                .map(std::vec::Vec::as_slice),
            Some(
                &[
                    serde_json::json!("current"),
                    serde_json::json!("shadowed"),
                    serde_json::json!("log-only"),
                ][..]
            ),
        );
        assert_eq!(
            keys.get("include_root_sessions")
                .and_then(|row| row.get("type")),
            Some(&serde_json::json!("boolean"))
        );

        // session_event_search: 8 keys, `query` required.
        let event_search = super::session_event_search_parameters();
        let event_keys = event_search
            .pointer("/properties")
            .and_then(Value::as_object)
            .expect("session_event_search properties");
        assert_eq!(event_keys.len(), 8);
        assert_eq!(event_search["required"], serde_json::json!(["query"]));
        assert_eq!(
            event_keys
                .get("surfaces")
                .and_then(|row| row.pointer("/items/enum"))
                .and_then(Value::as_array)
                .map(std::vec::Vec::len),
            Some(3)
        );

        // session_trace: the one optional session_id row, no required.
        let trace = super::session_trace_parameters();
        let trace_keys = trace
            .pointer("/properties")
            .and_then(Value::as_object)
            .expect("session_trace properties");
        assert_eq!(trace_keys.len(), 1);
        assert!(trace.get("required").is_none());

        // session_event_trace: session_id + required seq.
        let event_trace = super::session_event_trace_parameters();
        let event_trace_keys = event_trace
            .pointer("/properties")
            .and_then(Value::as_object)
            .expect("session_event_trace properties");
        assert_eq!(event_trace_keys.len(), 2);
        assert_eq!(event_trace["required"], serde_json::json!(["seq"]));
        assert_eq!(
            event_trace_keys.get("seq").and_then(|row| row.get("type")),
            Some(&serde_json::json!("integer"))
        );

        // session_event_read: session_id + required seq + before/after.
        let event_read = super::session_event_read_parameters();
        let event_read_keys = event_read
            .pointer("/properties")
            .and_then(Value::as_object)
            .expect("session_event_read properties");
        assert_eq!(event_read_keys.len(), 4);
        assert_eq!(event_read["required"], serde_json::json!(["seq"]));
        for key in ["before", "after"] {
            assert_eq!(
                event_read_keys.get(key).and_then(|row| row.get("type")),
                Some(&serde_json::json!("integer")),
                "{key} rides the integer row"
            );
        }
    }

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

    /// The two web tables pin their catalog rows (Q97-A: one full-schema
    /// pin per tool, the key sets, the required rows, and the optional
    /// fields verbatim).
    #[test]
    fn web_tools_pin_the_catalog_tables() {
        // web_search: the one `queries` array row, required, string
        // items, the baked cap inside the catalog description.
        let search = super::web_search_parameters();
        let search_keys = search
            .pointer("/properties")
            .and_then(Value::as_object)
            .expect("web_search properties");
        assert_eq!(search_keys.len(), 1);
        assert_eq!(search["required"], serde_json::json!(["queries"]));
        assert_eq!(search_keys["queries"]["type"], serde_json::json!("array"));
        assert_eq!(
            search_keys["queries"]["items"],
            serde_json::json!({ "type": "string" })
        );
        assert_eq!(
            search_keys["queries"]["description"],
            serde_json::json!(catalog_gen::WEB_SEARCH_QUERIES_TEXT)
        );
        let search_out = super::web_search_output_schema();
        assert_eq!(search_out["additionalProperties"], serde_json::json!(false));
        let sources = &search_out["properties"]["sources"];
        assert_eq!(sources["required"], serde_json::json!(true));
        assert_eq!(sources["type"], serde_json::json!("array"));
        let source_keys = sources
            .pointer("/items/properties")
            .and_then(Value::as_object)
            .expect("source item properties");
        assert_eq!(
            source_keys.keys().collect::<Vec<_>>(),
            vec!["url", "title", "snippet", "publishedAt"]
        );
        assert_eq!(source_keys["url"]["required"], serde_json::json!(true));
        for optional in ["publishedAt", "snippet", "title"] {
            assert!(
                source_keys[optional].get("required").is_none(),
                "{optional} stays optional"
            );
        }
        assert_eq!(
            search_out["properties"]["truncated"],
            serde_json::json!({ "type": "boolean", "required": true })
        );
        assert_eq!(
            search_out["properties"]["content"],
            serde_json::json!({ "type": "string" })
        );

        // web_fetch: the one `url` row, required; the body union's two
        // kind arms; the status integer; the truncation flag.
        let fetch = super::web_fetch_parameters();
        let fetch_keys = fetch
            .pointer("/properties")
            .and_then(Value::as_object)
            .expect("web_fetch properties");
        assert_eq!(fetch_keys.len(), 1);
        assert_eq!(fetch["required"], serde_json::json!(["url"]));
        assert_eq!(fetch_keys["url"]["type"], serde_json::json!("string"));
        assert_eq!(
            fetch_keys["url"]["description"],
            serde_json::json!(catalog_gen::WEB_FETCH_URL_TEXT)
        );
        let fetch_out = super::web_fetch_output_schema();
        assert_eq!(fetch_out["additionalProperties"], serde_json::json!(false));
        let fetch_fields = fetch_out["properties"].as_object().expect("fetch fields");
        assert_eq!(
            fetch_fields.keys().collect::<Vec<_>>(),
            vec!["url", "statusCode", "body", "truncated"]
        );
        assert_eq!(fetch_fields["url"]["required"], serde_json::json!(true));
        assert_eq!(
            fetch_fields["statusCode"]["type"],
            serde_json::json!("integer")
        );
        assert_eq!(
            fetch_fields["statusCode"]["required"],
            serde_json::json!(true)
        );
        let arms = fetch_fields["body"]["oneOf"]
            .as_array()
            .expect("body oneOf");
        assert_eq!(arms.len(), 2);
        for (arm, kind) in arms.iter().zip(["html", "text"]) {
            assert_eq!(arm["additionalProperties"], serde_json::json!(false));
            let arm_keys = arm["properties"].as_object().expect("arm properties");
            assert_eq!(arm_keys.keys().collect::<Vec<_>>(), vec!["kind", "content"]);
            assert_eq!(arm_keys["kind"]["const"], serde_json::json!(kind));
            assert_eq!(arm_keys["kind"]["required"], serde_json::json!(true));
            assert_eq!(arm_keys["content"]["required"], serde_json::json!(true));
        }
        assert_eq!(fetch_fields["body"]["required"], serde_json::json!(true));
        assert_eq!(
            fetch_fields["truncated"],
            serde_json::json!({ "type": "boolean", "required": true })
        );
    }

    /// The workspace-dependencies tables pin their catalog rows
    /// (Q97-A: the empty parameter object and the six-field output
    /// union with its absent-arm descriptions).
    #[test]
    fn load_workspace_dependencies_pins_the_catalog_tables() {
        let parameters = super::load_workspace_dependencies_parameters();
        assert_eq!(
            parameters,
            serde_json::json!({ "type": "object", "properties": {} })
        );
        let output = super::load_workspace_dependencies_output_schema();
        let fields = output["properties"].as_object().expect("output fields");
        assert_eq!(
            fields.keys().collect::<Vec<_>>(),
            vec![
                "python",
                "node",
                "pnpm",
                "pythonPackages",
                "nodePackages",
                "pythonDistributions"
            ]
        );
        for required in ["python", "pythonPackages", "pythonDistributions"] {
            assert_eq!(
                fields[required]["required"],
                serde_json::json!(true),
                "{required}"
            );
        }
        for optional in ["node", "pnpm", "nodePackages"] {
            assert!(
                fields[optional].get("required").is_none(),
                "{optional} stays optional"
            );
        }
        assert_eq!(
            fields["pythonDistributions"]["additionalProperties"],
            serde_json::json!(true)
        );
    }

    /// The `plugin_manager` tables pin their catalog rows (Q97-A:
    /// the ten rows, the required action enum, and the string output).
    #[test]
    fn plugin_manager_pins_the_catalog_table() {
        let parameters = super::plugin_manager_parameters();
        let keys = parameters
            .pointer("/properties")
            .and_then(Value::as_object)
            .expect("plugin_manager properties");
        assert_eq!(
            keys.keys().collect::<Vec<_>>(),
            vec![
                "action",
                "target",
                "enabled",
                "runtimeVersion",
                "acceptRisk",
                "approvedBuilds",
                "registry",
                "offset",
                "limit",
            ]
        );
        assert_eq!(
            keys["action"]["enum"],
            serde_json::json!([
                "list_plugins",
                "list_bundles",
                "set_plugin",
                "set_bundle",
                "install_bundle",
                "remove_bundle",
                "list_version_exemptions",
                "set_version_exemption",
            ])
        );
        assert_eq!(keys["action"]["required"], serde_json::json!(true));
        assert_eq!(keys["action"]["type"], serde_json::json!("string"));
        assert_eq!(keys["target"]["type"], serde_json::json!("string"));
        assert!(keys["target"].get("required").is_none());
        assert_eq!(keys["enabled"]["type"], serde_json::json!("boolean"));
        assert_eq!(keys["runtimeVersion"]["type"], serde_json::json!("string"));
        assert_eq!(keys["acceptRisk"]["type"], serde_json::json!("boolean"));
        assert_eq!(keys["approvedBuilds"]["type"], serde_json::json!("array"));
        assert_eq!(
            keys["approvedBuilds"]["items"],
            serde_json::json!({ "type": "string" })
        );
        assert_eq!(keys["registry"]["type"], serde_json::json!("string"));
        assert_eq!(keys["offset"]["type"], serde_json::json!("number"));
        assert_eq!(keys["limit"]["type"], serde_json::json!("number"));
        for row in [
            "target",
            "enabled",
            "runtimeVersion",
            "acceptRisk",
            "approvedBuilds",
            "registry",
            "offset",
            "limit",
        ] {
            assert!(keys[row].get("required").is_none(), "optional: {row}");
            assert_ne!(keys[row]["description"].as_str().expect("text"), "");
        }
        assert_eq!(
            super::plugin_manager_output_schema(),
            serde_json::json!({ "type": "string" })
        );
    }

    /// The lsp tables pin their catalog rows (Q97-A: the four rows,
    /// the enum, and the two-arm output union).
    #[test]
    fn lsp_tools_pin_the_catalog_tables() {
        let parameters = super::lsp_parameters();
        let keys = parameters
            .pointer("/properties")
            .and_then(Value::as_object)
            .expect("lsp properties");
        assert_eq!(
            keys.keys().collect::<Vec<_>>(),
            vec!["operation", "file_path", "line", "character"]
        );
        assert_eq!(
            keys["operation"]["enum"],
            serde_json::json!([
                "goToDefinition",
                "findReferences",
                "goToImplementation",
                "hover"
            ])
        );
        assert_eq!(keys["operation"]["required"], serde_json::json!(true));
        assert_eq!(keys["file_path"]["required"], serde_json::json!(true));
        assert_eq!(keys["line"]["type"], serde_json::json!("number"));
        assert_eq!(keys["character"]["type"], serde_json::json!("number"));
        assert_eq!(
            keys["operation"]["description"],
            serde_json::json!(catalog_gen::LSP_OPERATION_TEXT)
        );
        let output = super::lsp_output_schema();
        let arms = output["oneOf"].as_array().expect("two arms");
        assert_eq!(arms.len(), 2);
        let locations = &arms[0]["properties"];
        assert_eq!(locations["kind"]["const"], serde_json::json!("locations"));
        assert_eq!(
            locations["resolvedWorkspaceUri"]["required"],
            serde_json::json!(true)
        );
        let hover = &arms[1]["properties"];
        assert_eq!(hover["kind"]["const"], serde_json::json!("hover"));
        let hover_arms = hover["hover"]["oneOf"].as_array().expect("hover union");
        assert_eq!(hover_arms.len(), 2);
        assert_eq!(hover_arms[0], serde_json::json!({ "type": "null" }));
    }

    /// The working-directory tables pin their catalog rows (Q97-A: the
    /// full-schema pin — the one optional `cd` row and the one
    /// required `cwd` output row, verbatim texts included).
    #[test]
    fn working_directory_tools_pin_the_catalog_tables() {
        let parameters = super::working_directory_parameters();
        assert_eq!(parameters["type"], serde_json::json!("object"));
        assert!(parameters.get("required").is_none());
        let keys = parameters
            .pointer("/properties")
            .and_then(Value::as_object)
            .expect("working_directory properties");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys["cd"]["type"], serde_json::json!("string"));
        assert_eq!(
            keys["cd"]["description"],
            serde_json::json!(catalog_gen::WORKING_DIRECTORY_CD_TEXT)
        );
        let output = super::working_directory_output_schema();
        assert_eq!(output["additionalProperties"], serde_json::json!(false));
        let fields = output["properties"].as_object().expect("cwd fields");
        assert_eq!(fields.keys().collect::<Vec<_>>(), vec!["cwd"]);
        assert_eq!(fields["cwd"]["type"], serde_json::json!("string"));
        assert_eq!(fields["cwd"]["required"], serde_json::json!(true));
        assert_eq!(
            fields["cwd"]["description"],
            serde_json::json!(catalog_gen::WORKING_DIRECTORY_CWD_TEXT)
        );
    }

    /// The inspect pair pins their catalog rows (Q97-A: the query's
    /// four rows and the list's empty object, verbatim texts
    /// included).
    #[test]
    fn cordis_inspect_tools_pin_the_catalog_tables() {
        let list = super::cordis_inspect_list_parameters();
        assert_eq!(
            list,
            serde_json::json!({ "type": "object", "properties": {} })
        );
        let query = super::cordis_inspect_query_parameters();
        let keys = query
            .pointer("/properties")
            .and_then(Value::as_object)
            .expect("query properties");
        assert_eq!(
            keys.keys().collect::<Vec<_>>(),
            vec!["platform", "provider", "method", "input"]
        );
        assert_eq!(
            keys["platform"]["enum"],
            serde_json::json!(["host", "client"])
        );
        assert_eq!(keys["platform"]["required"], serde_json::json!(true));
        assert_eq!(keys["provider"]["required"], serde_json::json!(true));
        assert_eq!(keys["method"]["required"], serde_json::json!(true));
        assert_eq!(
            keys["platform"]["description"],
            serde_json::json!(catalog_gen::CORDIS_INSPECT_QUERY_PLATFORM_TEXT)
        );
        assert_eq!(keys["input"]["type"], serde_json::json!("json"));
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

    /// The six terminal tables pin their catalog rows (Q97-A) and the
    /// persistent twins pin their single-command shape; the frozen
    /// whole-table comparison and the eight-row scheduling faces ride
    /// the same anchor set (splitting would hide the family
    /// correspondence).
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the pin mirrors the eight dsh rows one anchor per row; splitting it would hide the family correspondence"
    )]
    fn terminal_family_pins_the_catalog_tables() {
        // terminal_open: `type` required; the three property rows.
        let open = super::terminal_open_parameters();
        assert_eq!(
            open["required"],
            serde_json::json!(["type"]),
            "type is the one required row"
        );
        assert_eq!(
            open.pointer("/properties/type/description")
                .and_then(Value::as_str),
            Some("Registered terminal backend type, usually \"shell\".")
        );
        // terminal_send: the catalog true-variant carries the
        // background row; the false variant strips it and the
        // description loses the background sentence.
        let send_true = super::terminal_send_parameters(true);
        assert_eq!(
            send_true
                .pointer("/properties")
                .and_then(Value::as_object)
                .expect("rows")
                .len(),
            4
        );
        assert_eq!(
            super::terminal_send_description(true),
            crate::catalog_gen::TERMINAL_SEND_DESCRIPTION
        );
        let send_false = super::terminal_send_parameters(false);
        assert_eq!(
            send_false
                .pointer("/properties")
                .and_then(Value::as_object)
                .expect("rows")
                .len(),
            3
        );
        assert_eq!(
            super::terminal_send_description(false),
            "Send text to a persistent terminal. By default Enter is submitted and the call waits for a prompt, stdin wait, output silence, timeout, or session exit."
        );
        // terminal_read: offset/count optional numbers.
        let read = super::terminal_read_parameters();
        assert_eq!(read["required"], serde_json::json!(["sessionId"]));
        assert!(read.pointer("/properties/offset").is_some());
        assert!(read.pointer("/properties/count").is_some());
        // terminal_signal: the five-signal enum verbatim.
        let signal = super::terminal_signal_parameters();
        assert_eq!(
            signal.pointer("/properties/signal/enum"),
            Some(&serde_json::json!([
                "SIGINT", "SIGTERM", "SIGKILL", "SIGTSTP", "SIGHUP"
            ]))
        );
        // terminal_close: the single session-id row.
        let close = super::terminal_close_parameters();
        assert_eq!(
            close
                .pointer("/properties")
                .and_then(Value::as_object)
                .expect("rows")
                .len(),
            1
        );
        // terminal_list: the empty table.
        let list = super::terminal_list_parameters();
        assert_eq!(
            list.pointer("/properties")
                .and_then(Value::as_object)
                .expect("rows")
                .len(),
            0
        );
        // The persistent twins: one required command row each.
        for parameters in [
            super::persistent_bash_parameters(),
            super::persistent_pwsh_parameters(),
        ] {
            assert_eq!(
                parameters["required"],
                serde_json::json!(["command"]),
                "command is the one required row"
            );
            assert_eq!(
                parameters
                    .pointer("/properties")
                    .and_then(Value::as_object)
                    .expect("rows")
                    .len(),
                1
            );
        }
        // The whole tables equal the frozen catalog JSON (the frozen
        // comparison clause; the send true variant carries the
        // background row).
        for (composed, frozen) in [
            (
                super::terminal_open_parameters(),
                crate::catalog_gen::TERMINAL_OPEN_PARAMETERS_JSON,
            ),
            (
                super::terminal_send_parameters(true),
                crate::catalog_gen::TERMINAL_SEND_PARAMETERS_JSON,
            ),
            (
                super::terminal_read_parameters(),
                crate::catalog_gen::TERMINAL_READ_PARAMETERS_JSON,
            ),
            (
                super::terminal_signal_parameters(),
                crate::catalog_gen::TERMINAL_SIGNAL_PARAMETERS_JSON,
            ),
            (
                super::terminal_close_parameters(),
                crate::catalog_gen::TERMINAL_CLOSE_PARAMETERS_JSON,
            ),
            (
                super::terminal_list_parameters(),
                crate::catalog_gen::TERMINAL_LIST_PARAMETERS_JSON,
            ),
            (
                super::persistent_bash_parameters(),
                crate::catalog_gen::BASH_PERSISTENT_PARAMETERS_JSON,
            ),
            (
                super::persistent_pwsh_parameters(),
                crate::catalog_gen::PWSH_PERSISTENT_PARAMETERS_JSON,
            ),
        ] {
            let frozen: Value = serde_json::from_str(frozen).expect("the frozen table parses");
            assert_eq!(composed, frozen);
        }
        // The scheduling faces: the eight registrations stay
        // exclusive (no isConcurrencySafe; fail-closed exclusive),
        // execute under the one service, and carry present-call
        // endpoints; the send row alone rides the full face
        // (presentResult and the output presentationMeta).
        for registration in [
            crate::plugin_shell::terminal_open_registration(),
            crate::plugin_shell::terminal_send_registration(),
            crate::plugin_shell::terminal_read_registration(),
            crate::plugin_shell::terminal_signal_registration(),
            crate::plugin_shell::terminal_close_registration(),
            crate::plugin_shell::terminal_list_registration(),
            crate::plugin_shell::persistent_bash_registration(),
            crate::plugin_shell::persistent_pwsh_registration(),
        ] {
            assert!(
                registration.get("isConcurrencySafe").is_none(),
                "the terminal family stays exclusive"
            );
            assert_eq!(
                registration["execute"]["service"],
                crate::service::SERVICE_NAME
            );
            assert!(registration.get("presentCall").is_some());
        }
        for registration in [
            crate::plugin_shell::terminal_open_registration(),
            crate::plugin_shell::terminal_read_registration(),
            crate::plugin_shell::terminal_signal_registration(),
            crate::plugin_shell::terminal_close_registration(),
            crate::plugin_shell::terminal_list_registration(),
            crate::plugin_shell::persistent_bash_registration(),
            crate::plugin_shell::persistent_pwsh_registration(),
        ] {
            assert!(registration.get("presentResult").is_none());
            assert!(
                registration
                    .get("output")
                    .expect("output")
                    .get("presentationMeta")
                    .is_none()
            );
        }
        let send = crate::plugin_shell::terminal_send_registration();
        assert!(send.get("presentResult").is_some());
        assert!(
            send.get("output")
                .expect("output")
                .get("presentationMeta")
                .is_some()
        );
        // The presentation families: the six pending-call card kinds
        // and the persistent terminal card (the presentation faces of
        // the 调度与呈现 clauses).
        assert_eq!(
            crate::terminal_tools::present_call_open(&json!({ "type": "shell" }))["kind"],
            json!("execute")
        );
        let foreground =
            crate::terminal_tools::present_call_send(&json!({ "sessionId": "s1", "text": "ls" }));
        assert_eq!(foreground["card"], json!("terminal"));
        let background = crate::terminal_tools::present_call_send(&json!(
            { "sessionId": "s1", "text": "ls", "run_in_background": true }
        ));
        assert_eq!(background["card"], json!("generic"));
        assert_eq!(background["kind"], json!("execute"));
        assert_eq!(
            crate::terminal_tools::present_call_read(&json!({ "sessionId": "s1" }))["kind"],
            json!("read")
        );
        assert_eq!(
            crate::terminal_tools::present_call_signal(
                &json!({ "sessionId": "s1", "signal": "SIGINT" })
            )["kind"],
            json!("execute")
        );
        assert_eq!(
            crate::terminal_tools::present_call_close(&json!({ "sessionId": "s1" }))["kind"],
            json!("delete")
        );
        assert_eq!(
            crate::terminal_tools::present_call_list()["kind"],
            json!("read")
        );
        let persistent_call =
            crate::persistent::present_call_persistent(&json!({ "command": "cargo test" }));
        assert_eq!(persistent_call["card"], json!("terminal"));
    }

    /// The terminal output contracts pin the dsh shapes: the shared
    /// status union, the snapshot spread with motd, the send oneOf,
    /// and the plain-string persistent contract.
    #[test]
    fn terminal_family_pins_the_output_contracts() {
        // The status union: the two const discriminators.
        let status = super::terminal_session_status_schema();
        assert_eq!(
            status.pointer("/oneOf/0/properties/kind/const"),
            Some(&serde_json::json!("running"))
        );
        assert_eq!(
            status.pointer("/oneOf/1/properties/kind/const"),
            Some(&serde_json::json!("exited"))
        );
        // The open contract: five snapshot rows plus motd.
        let open = super::terminal_open_output_schema();
        let rows = open
            .pointer("/properties")
            .and_then(Value::as_object)
            .expect("rows");
        assert_eq!(rows.len(), 6);
        assert!(rows.contains_key("motd"));
        assert_eq!(
            open.pointer("/properties/sessionId/type"),
            Some(&serde_json::json!("string"))
        );
        // The send contract: the background and foreground arms.
        let send = super::terminal_send_output_schema();
        assert_eq!(
            send.pointer("/oneOf/0/properties/kind/const"),
            Some(&serde_json::json!("background"))
        );
        assert_eq!(
            send.pointer("/oneOf/1/properties/kind/const"),
            Some(&serde_json::json!("foreground"))
        );
        assert_eq!(
            send.pointer("/oneOf/1/properties/waitReason/enum"),
            Some(&serde_json::json!([
                "stdin_read",
                "inferred_idle",
                "timeout",
                "session_exit"
            ]))
        );
        // The read, signal, and close rows.
        let read = super::terminal_read_output_schema();
        assert_eq!(
            read.pointer("/properties")
                .and_then(Value::as_object)
                .expect("rows")
                .len(),
            5
        );
        let signal = super::terminal_signal_output_schema();
        assert_eq!(
            signal.pointer("/properties/delivered/const"),
            Some(&serde_json::json!(true))
        );
        let close = super::terminal_close_output_schema();
        assert_eq!(
            close.pointer("/properties/outcome/enum"),
            Some(&serde_json::json!(["closed", "already-closing"]))
        );
        // The list contract rides the snapshot items.
        let list = super::terminal_list_output_schema();
        assert_eq!(list["type"], serde_json::json!("array"));
        assert!(list.pointer("/items/properties/sessionId").is_some());
        // The persistent pair: the plain string.
        assert_eq!(
            super::persistent_output_schema(),
            serde_json::json!({ "type": "string" })
        );
    }
}
