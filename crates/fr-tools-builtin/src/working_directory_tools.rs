// Rust guideline compliant 2026-11-01
//! The `working_directory` tool body (dsh `tool-working-directory`):
//! the read-or-change tool over the working-directory domain.
//!
//! The registration face stays a partial row (the dsh defineTool has
//! no custom present faces and no concurrency flag — the exclusive,
//! generic-presentation default).

use serde_json::{Value, json};

use crate::tools::{BodyError, Seams};

/// The baked gate: the dsh base bundle mounts `tool-working-directory`
/// (the Q16f-F baked mirror judgment).
pub const BAKED_WORKING_DIRECTORY_TOOL: bool = true;

/// Parses the arguments (the dsh `cd` option; absent reads the
/// current directory). A present non-string refusal stands in for the
/// dsh schema validation, which never reaches the body.
///
/// # Errors
///
/// The non-string `cd` refusal.
pub fn parse_args(arguments: &Value) -> Result<Option<String>, BodyError> {
    match arguments.get("cd") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(cd)) => Ok(Some(cd.clone())),
        Some(_) => Err(BodyError::plain("cd must be a string")),
    }
}

/// Executes one call (the dsh `execute`): the missing-agent refusal
/// and the ensure/set fork over the fr-working-directory service.
///
/// The dsh `tool-working-directory` injects the workingDirectory
/// service; the fr carrier routes the fork through the seams.
///
/// # Errors
///
/// The missing-agent refusal or the domain refusal (the service's dsh
/// texts carried verbatim through the require error chain).
pub fn execute(
    seams: &dyn Seams,
    arguments: &Value,
    agent: Option<&str>,
) -> Result<Value, BodyError> {
    let arguments = parse_args(arguments)?;
    let Some(agent) = agent else {
        return Err(BodyError::plain(
            "working_directory requires an agent Session",
        ));
    };
    let cwd = match &arguments {
        Some(cd) => seams
            .working_directory_set(agent, cd)
            .map_err(BodyError::plain)?,
        None => seams
            .working_directory_ensure(agent)
            .map_err(BodyError::plain)?,
    };
    Ok(json!({ "cwd": cwd }))
}

/// Renders the output value (the dsh `output.render`: one text block
/// carrying the absolute directory).
#[must_use]
pub fn render(_args: &Value, value: &Value) -> Value {
    let cwd = value
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    serde_json::json!([{ "type": "text", "text": cwd }])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parse legs: absent/null read, the string pass-through, and
    /// the non-string refusal (the schema-violation stand-in).
    #[test]
    fn parse_args_pins_the_shapes() {
        assert_eq!(parse_args(&json!({})).expect("read"), None);
        assert_eq!(
            parse_args(&json!({ "cd": "/tmp" })).expect("cd"),
            Some("/tmp".to_owned())
        );
        assert_eq!(
            parse_args(&serde_json::json!({ "cd": null })).expect("null"),
            None
        );
        let error = parse_args(&json!({ "cd": 42 })).unwrap_err();
        assert_eq!(error.message, "cd must be a string");
    }

    /// The render face: one text block over the committed directory.
    #[test]
    fn render_returns_one_text_block() {
        let rendered = render(&json!({}), &json!({ "cwd": "/repo" }));
        assert_eq!(
            rendered,
            serde_json::json!([{ "type": "text", "text": "/repo" }])
        );
    }
}
