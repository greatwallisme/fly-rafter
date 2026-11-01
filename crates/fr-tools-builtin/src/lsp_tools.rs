// Rust guideline compliant 2026-11-01
//! The `lsp` tool (dsh `tool-lsp`): one read-only tool with four
//! operations over the `'lsp'` route table.
//!
//! The fr route table is the Q16f-B honest face carried to the lsp
//! seam: no provider registers in M1, so every query refuses through
//! the structured `LSP_UNAVAILABLE` code without changing the schema.
//! The render face stays complete (the tool's registered face), and
//! the working-directory ensure rides the in-crate domain.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::tools::BodyError;
use crate::working_directory::WorkingDirectories;

/// The baked gate: the dsh base bundle mounts no `tool-lsp` row
/// (web-app/desktop compositions only — the Q16f-F baked mirror).
pub const BAKED_LSP: bool = false;

/// The baked result cap (dsh `DEFAULT_MAX_LOCATIONS`).
pub const BAKED_MAX_LOCATIONS: u64 = 100;
/// The baked rendered-text cap (dsh `DEFAULT_MAX_RESULT_CHARS`).
pub const BAKED_MAX_RESULT_CHARS: u64 = 16_000;
/// The baked tool-call timeout budget (dsh
/// `DEFAULT_LSP_TOOL_TIMEOUT_MS`).
pub const BAKED_LSP_TIMEOUT_MS: u64 = 60_000;

/// The four operations (dsh `LSP_OPERATIONS`, the runtime tuple
/// behind the schema enum and the parse gate).
pub const LSP_OPERATIONS: [&str; 4] = [
    "goToDefinition",
    "findReferences",
    "goToImplementation",
    "hover",
];

/// The `LspError` carrier name (the dsh `HarnessError` family).
pub const LSP_ERROR_NAME: &str = "LspError";

/// Builds a coded lsp failure (the dsh `LspError` pair: the stable
/// code rides the name so callers route without parsing the message).
#[must_use]
pub fn lsp_error(code: &str, message: impl Into<String>) -> BodyError {
    BodyError {
        name: LSP_ERROR_NAME.to_owned(),
        message: message.into(),
        code: Some(code.to_owned()),
    }
}

/// The `'lsp'` route table (dsh `LspRegistry`).
///
/// Extension routes resolve to providers per query; the fr
/// composition registers no provider in M1, so the table stays empty
/// and every lookup refuses through the structured
/// `LSP_UNAVAILABLE` code.
#[derive(Debug, Default)]
pub struct LspRegistry {
    routes: HashMap<String, String>,
}

impl LspRegistry {
    /// The empty route table (the M1 honest face).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The route key (dsh `finalExtension`): the lowercased path
    /// extension behind the last dot.
    #[must_use]
    pub fn extension_of(&self, file_path: &str) -> String {
        std::path::Path::new(file_path)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
    }
}

/// Validates the model arguments (dsh `parseLspArgs`) and returns the
/// file path. The coordinate conversion legs run their one-based
/// validation; the converted seam positions carry no consumer in the
/// honest composition (no provider face exists to receive them).
///
/// # Errors
///
/// The four refusals (operation, blank path, the two coordinates).
fn parse_args(arguments: &Value) -> Result<String, BodyError> {
    let operation = arguments
        .get("operation")
        .and_then(Value::as_str)
        .ok_or_else(|| lsp_error("LSP_BAD_REQUEST", "operation must be a string"))?;
    if !LSP_OPERATIONS.contains(&operation) {
        return Err(lsp_error(
            "LSP_BAD_REQUEST",
            format!("operation must be one of {}", LSP_OPERATIONS.join(", ")),
        ));
    }
    let file_path = arguments
        .get("file_path")
        .and_then(Value::as_str)
        .ok_or_else(|| lsp_error("LSP_BAD_REQUEST", "file_path must be a string"))?;
    if file_path.trim().is_empty() {
        return Err(lsp_error(
            "LSP_BAD_REQUEST",
            "file_path must be a non-empty string",
        ));
    }
    // The one-based validation legs run; the dsh zero-based
    // conversion result carries no consumer in the honest composition
    // (no provider face exists to receive it).
    let line = one_based(arguments.get("line"), "line")?;
    let character = one_based(arguments.get("character"), "character")?;
    let (line, character) = (line - 1, character - 1);
    let _ = (line, character);
    Ok(file_path.to_owned())
}

/// Validates a one-based coordinate (dsh `oneBased`).
fn one_based(value: Option<&Value>, name: &str) -> Result<i64, BodyError> {
    let Some(value) = value.and_then(Value::as_i64) else {
        return Err(lsp_error(
            "LSP_BAD_REQUEST",
            format!("{name} must be a positive integer (one-based)"),
        ));
    };
    if value < 1 {
        return Err(lsp_error(
            "LSP_BAD_REQUEST",
            format!("{name} must be a positive integer (one-based)"),
        ));
    }
    Ok(value)
}

/// Executes one call (dsh `execute`): the workspace ensure, then the
/// route lookup — the empty table refuses through the structured
/// `LSP_UNAVAILABLE` code before any provider face is reachable.
///
/// # Errors
///
/// The argument refusals, the workspace refusal, or the structured
/// unavailable refusal.
pub fn execute(
    registry: &LspRegistry,
    directories: &WorkingDirectories,
    arguments: &Value,
    agent: Option<&str>,
) -> Result<Value, BodyError> {
    let file_path = parse_args(arguments)?;
    let Some(agent) = agent else {
        return Err(lsp_error(
            "LSP_WORKSPACE_REQUIRED",
            "the lsp tool requires a session workspace cwd",
        ));
    };
    let _workspace_root = directories
        .ensure(agent)
        .map_err(|error| lsp_error("LSP_WORKSPACE_REQUIRED", error))?;
    // The dsh route lookup (the extension-to-provider table): the
    // fr composition registers no provider, so the lookup misses and
    // the structured unavailable refusal carries the path.
    let extension = registry.extension_of(&file_path);
    if registry.routes.contains_key(&extension) {
        return Err(lsp_error(
            "LSP_UNAVAILABLE",
            "the registered provider face is unreachable in this composition",
        ));
    }
    Err(lsp_error(
        "LSP_UNAVAILABLE",
        format!("no LSP provider handles \"{file_path}\""),
    ))
}

/// Renders one output value (dsh `output.render`: the two closed
/// union arms over the shared bounds).
///
/// # Errors
///
/// Never: unknown kinds render the empty text (the schema's closed
/// union makes the arm unreachable for wire values).
pub fn render(_args: &Value, value: &Value, max_locations: u64, max_result_chars: u64) -> Value {
    let text = match value.get("kind").and_then(Value::as_str) {
        Some("locations") => format_locations(
            value
                .get("locations")
                .cloned()
                .unwrap_or(Value::Array(Vec::new())),
            value
                .get("resolvedWorkspaceUri")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            max_locations,
            max_result_chars,
        ),
        _ => format_hover(value.get("hover"), max_result_chars),
    };
    json!([{ "type": "text", "text": text }])
}

/// Renders a locations result (dsh `formatLocations`): file-grouped
/// `path:line:character` entries, the omission marker, then the
/// complete-result bound.
#[must_use]
pub fn format_locations(
    locations: Value,
    workspace_uri: &str,
    max_locations: u64,
    max_result_chars: u64,
) -> String {
    let Value::Array(items) = locations else {
        return bound_result("No results.", max_result_chars, "locations");
    };
    if items.is_empty() {
        return bound_result("No results.", max_result_chars, "locations");
    }
    let shown: Vec<Value> = items
        .iter()
        .take(usize::try_from(max_locations).unwrap_or(usize::MAX))
        .cloned()
        .collect();
    let omitted = items.len() - shown.len();
    let mut lines: Vec<String> = Vec::new();
    let mut grouped: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for location in &shown {
        let uri = location
            .get("uri")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let range = location
            .pointer("/range/start")
            .cloned()
            .unwrap_or(Value::Null);
        let line = range.get("line").and_then(Value::as_i64).unwrap_or(0) + 1;
        let character = range.get("character").and_then(Value::as_i64).unwrap_or(0) + 1;
        let path = render_uri(uri, workspace_uri);
        let entry = format!("{path}:{line}:{character}");
        grouped.entry(path).or_default().push(entry);
    }
    for entries in grouped.values() {
        lines.extend(entries.iter().cloned());
    }
    if omitted > 0 {
        lines.push(format!(
            "… {omitted} more location{} omitted (limit {max_locations}).",
            if omitted == 1 { "" } else { "s" }
        ));
    }
    bound_result(&lines.join("\n"), max_result_chars, "locations")
}

/// Renders a hover result (dsh `formatHover`).
#[must_use]
pub fn format_hover(hover: Option<&Value>, max_result_chars: u64) -> String {
    let text = hover
        .and_then(|hover| hover.get("contents"))
        .and_then(Value::as_str)
        .unwrap_or("No hover information.")
        .to_owned();
    bound_result(&text, max_result_chars, "hover")
}

/// Bounds a complete rendered result, keeping the notice inside the
/// cap (dsh `boundResult`).
#[must_use]
pub fn bound_result(text: &str, max_chars: u64, label: &str) -> String {
    let max = usize::try_from(max_chars).unwrap_or(usize::MAX);
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let notice = format!("\n… {label} truncated (limit {max_chars} characters).");
    if notice.chars().count() >= max {
        return notice.chars().take(max).collect();
    }
    let keep = max - notice.chars().count();
    format!("{}{notice}", text.chars().take(keep).collect::<String>())
}

/// Resolves a location URI for display (dsh `renderUri`): a `file:`
/// URI under the workspace renders workspace-relative, outside paths
/// render absolute, malformed and non-`file:` URIs stay verbatim.
#[must_use]
pub fn render_uri(uri: &str, workspace_uri: &str) -> String {
    if !uri.starts_with("file:") {
        return uri.to_owned();
    }
    let target = url::Url::parse(uri);
    let workspace = url::Url::parse(workspace_uri);
    let (Ok(target), Ok(workspace)) = (target, workspace) else {
        return uri.to_owned();
    };
    if workspace.scheme() != "file" {
        return uri.to_owned();
    }
    let workspace_path = url_to_path(&workspace);
    let target_path = url_to_path(&target);
    let (Some(workspace_path), Some(target_path)) = (workspace_path, target_path) else {
        return uri.to_owned();
    };
    let Ok(relative) = std::path::Path::new(&target_path)
        .strip_prefix(std::path::Path::new(&workspace_path))
        .map(|rest| rest.to_string_lossy().into_owned())
    else {
        return target_path;
    };
    if relative.is_empty() {
        return ".".to_owned();
    }
    relative
}

/// Decodes a `file:` URL into its host path (the dsh
/// `fileURLToPath` containment: malformed inputs fail contained).
fn url_to_path(url: &url::Url) -> Option<String> {
    if url.scheme() != "file" {
        return None;
    }
    let path = percent_encoding::percent_decode_str(url.path())
        .decode_utf8()
        .ok()?
        .into_owned();
    if path.contains('\0') {
        return None;
    }
    Some(path)
}

/// The pending-call presentation (dsh `presentLspCall`): the generic
/// search card over the operation and the one-based cursor, focused
/// on the queried line.
#[must_use]
pub fn present_call(arguments: &Value) -> Value {
    let operation = arguments
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let file_path = arguments
        .get("file_path")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let line = arguments.get("line").cloned().unwrap_or(Value::Null);
    let character = arguments.get("character").cloned().unwrap_or(Value::Null);
    json!({
        "card": "generic",
        "kind": "search",
        "title": format!("LSP {operation} {file_path}:{line}:{character}"),
        "locations": [{ "path": file_path, "line": line }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The empty route table refuses through the structured
    /// `LSP_UNAVAILABLE` code with the dsh text, after the workspace
    /// ensure leg runs.
    #[test]
    fn execute_refuses_unavailable_after_the_workspace_leg() {
        struct OpenPolicy;
        impl crate::fs_domain::StandingPolicy for OpenPolicy {
            fn resolve(&self, _session: Option<&str>) -> Result<Value, String> {
                Ok(json!({}))
            }
        }
        struct NoLog;
        impl crate::working_directory::SessionLog for NoLog {
            fn read(&self, _id: &str) -> Result<crate::working_directory::ColdLog, String> {
                Ok(crate::working_directory::ColdLog {
                    header: json!({}),
                    inherited_event_count: 0,
                    events: Value::Array(Vec::new()),
                })
            }
            fn append(&self, _id: &str, _cwd: &str) -> Result<(), String> {
                Ok(())
            }
        }
        struct NoInject;
        impl crate::working_directory::AgentInject for NoInject {
            fn inject(&self, _agent: &str, _message: &Value) -> Result<(), String> {
                Ok(())
            }
        }
        let registry = LspRegistry::new();
        let config = crate::fs_domain::FsDomainConfig {
            cwd: std::env::temp_dir().to_string_lossy().into_owned(),
            diff_basis_max_bytes: crate::fsio::DEFAULT_DIFF_BASIS_MAX_BYTES,
        };
        let directories = WorkingDirectories::new(
            std::sync::Arc::new(NoLog),
            std::sync::Arc::new(crate::fs_domain::FsDomain::new(
                config,
                Box::new(OpenPolicy),
                None,
            )),
            std::sync::Arc::new(NoInject),
            std::env::temp_dir().to_string_lossy().into_owned(),
        );
        let arguments = json!({
            "operation": "hover",
            "file_path": "src/lib.rs",
            "line": 3,
            "character": 7,
        });
        let error = execute(&registry, &directories, &arguments, Some("agent-1")).unwrap_err();
        assert_eq!(error.code.as_deref(), Some("LSP_UNAVAILABLE"));
        assert_eq!(error.name, "LspError");
        assert_eq!(error.message, "no LSP provider handles \"src/lib.rs\"");
        // The missing-agent refusal rides the workspace code.
        let error = execute(&registry, &directories, &arguments, None).unwrap_err();
        assert_eq!(error.code.as_deref(), Some("LSP_WORKSPACE_REQUIRED"));
    }

    /// The parse legs pin the four refusals and the coordinate
    /// conversion (one-based model coordinates into the seam).
    #[test]
    fn lsp_parse_args_pins_the_refusals() {
        for broken in [
            json!({ "operation": "jump", "file_path": "a.rs", "line": 1, "character": 1 }),
            json!({ "operation": "hover", "file_path": "  ", "line": 1, "character": 1 }),
            json!({ "operation": "hover", "file_path": "a.rs", "line": 0, "character": 1 }),
            json!({ "operation": "hover", "file_path": "a.rs", "line": 1, "character": 0 }),
            json!({ "operation": "hover", "file_path": "a.rs", "line": 1 }),
        ] {
            assert!(parse_args(&broken).is_err(), "refused: {broken}");
        }
        assert_eq!(
            parse_args(
                &json!({ "operation": "jump", "file_path": "a.rs", "line": 1, "character": 1 })
            )
            .unwrap_err()
            .message,
            "operation must be one of goToDefinition, findReferences, goToImplementation, hover"
        );
    }

    /// The render family pins the grouping, the omission marker, the
    /// hover arms, and the bound notice (dsh formatLocations/formatHover/
    /// boundResult verbatim texts).
    #[test]
    fn render_family_pins_the_texts() {
        assert_eq!(
            format_locations(json!([]), "file:///w", 100, 16000),
            "No results."
        );
        let locations = json!([
            { "uri": "file:///w/src/a.rs", "range": { "start": { "line": 2, "character": 4 } } },
            { "uri": "file:///w/src/a.rs", "range": { "start": { "line": 5, "character": 0 } } },
            { "uri": "file:///other/b.rs", "range": { "start": { "line": 0, "character": 0 } } },
        ]);
        let rendered = format_locations(locations, "file:///w", 100, 16000);
        // Workspace-relative for in-workspace URIs, absolute outside
        // (the dsh renderUri judgment).
        assert!(rendered.contains("src/a.rs:3:5"));
        assert!(rendered.contains("src/a.rs:6:1"));
        assert!(rendered.contains("/other/b.rs:1:1"));
        assert_eq!(format_hover(None, 16000), "No hover information.");
        assert_eq!(
            format_hover(Some(&json!({ "contents": "text here" })), 16000),
            "text here"
        );
        // The notice fits inside the cap: head + notice within 60.
        let bound = bound_result(&"x".repeat(80), 60, "hover");
        assert!(bound.contains("hover truncated (limit 60 characters)."));
        assert!(bound.chars().count() <= 60);
        // The notice itself overflows the cap: the dsh slice arm keeps
        // the first max chars of the notice.
        let overflow = bound_result(&"x".repeat(50), 10, "hover");
        assert_eq!(overflow.chars().count(), 10);
        assert!(overflow.starts_with('\n'));
    }

    /// The pending-call card carries the operation, the cursor, and
    /// the focused line.
    #[test]
    fn present_call_pins_the_card() {
        let card = present_call(&json!({
            "operation": "findReferences",
            "file_path": "src/lib.rs",
            "line": 4,
            "character": 9,
        }));
        assert_eq!(card["kind"], serde_json::json!("search"));
        assert_eq!(
            card["title"],
            serde_json::json!("LSP findReferences src/lib.rs:4:9")
        );
        assert_eq!(card["locations"][0]["line"], serde_json::json!(4));
    }
}
