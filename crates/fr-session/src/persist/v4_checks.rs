//! Native v4 admission and generation-owned validation on raw physical rows
//!
//! (dsh session-format-v3-to-v4: `validation.ts`, `codec.ts` admission,
//! `retired-syntax.ts`, `system-message.ts`, `tool-role.ts`,
//! `fork-result.ts`, `developer.ts`, `message-sources.ts`,
//! `relationships.ts`).
//!
//! Two refusal classes mirror the dsh error split: [`V4CheckError::Format`]
//! (a corrupt current artifact) and [`V4CheckError::UnsupportedMigration`]
//! (a retired shape the harness refuses regardless of recovery mode; the
//! scanner maps it to a format-unsupported refusal).

use serde_json::Value;

use crate::closer_text::INTERRUPTED_NOT_STARTED;
use crate::vocabulary::EventVocabulary;

/// The five surface-carrying event types.
const SURFACE_TYPES: [&str; 5] = [
    "system/message",
    "user/message",
    "developer/message",
    "assistant/message",
    "tool/result",
];

/// The event types that must sit inside an open turn and step.
const STEP_EVENT_TYPES: [&str; 3] = ["system/message", "developer/message", "assistant/attempt"];

/// The event types the lifecycle validator interprets.
const RELATIONSHIP_TYPES: [&str; 26] = [
    "system/message",
    "user/message",
    "developer/message",
    "assistant/message",
    "tool/result",
    "assistant/attempt",
    "turn/start",
    "turn/end",
    "step/start",
    "step/end",
    "tool/call",
    "request/header",
    "request/context",
    "tool/ptc-dispatch-start",
    "tool/ptc-dispatch",
    "llm/retry",
    "llm/retry-started",
    "session/title",
    "session/title-llm-request",
    "command/run",
    "command/done",
    "compaction/start",
    "compaction/summary",
    "compaction/end",
    "compaction/prune",
    "session/end-seed",
];

/// One admission or validation refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V4CheckError {
    /// A corrupt current-format artifact (`SessionFormatError`).
    Format(String),
    /// A retired shape refused before any recovery
    /// (`SessionFormatUnsupportedMigrationError`; the caller renders it as
    /// a format-unsupported refusal).
    UnsupportedMigration(String),
}

impl V4CheckError {
    /// The refusal message text.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Format(message) | Self::UnsupportedMigration(message) => message,
        }
    }
}

fn format_error(message: impl Into<String>) -> V4CheckError {
    V4CheckError::Format(message.into())
}

/// The row's seq as the dsh string interpolation renders it (`undefined`
/// when absent).
fn subject_seq(row: &Value) -> String {
    row.get("seq").map_or_else(
        || "undefined".to_owned(),
        |value| value.to_string().trim_matches('"').to_owned(),
    )
}

fn is_object(value: &Value) -> bool {
    value.is_object()
}

fn count(value: &Value, label: &str) -> Result<u64, V4CheckError> {
    match value.as_u64() {
        Some(value) if value <= fr_wire::ids::SAFE_INTEGER_MAX => Ok(value),
        _ => Err(format_error(format!(
            "{label} must be a non-negative safe integer"
        ))),
    }
}

/// Validates the exact native v4 logical header (dsh
/// `assertReleasedV4Header`).
///
/// # Errors
///
/// Returns the dsh refusal for each structural failure.
///
/// # Panics
///
/// The `.expect("checked object")` cannot fire: the object check at the top
/// of the function refused non-object headers already.
pub fn assert_released_v4_header(header: &Value) -> Result<(), V4CheckError> {
    const REQUIRED: [&str; 5] = ["version", "id", "createdAt", "isSeeded", "delegationDepth"];
    const OPTIONAL: [&str; 4] = ["cwd", "parentSession", "origin", "agentPreset"];
    if !is_object(header) || header.get("version").and_then(Value::as_u64) != Some(4) {
        return Err(format_error("expected format v4 header"));
    }
    let object = header.as_object().expect("checked object");
    if let Some(missing) = REQUIRED.iter().find(|key| !object.contains_key(**key)) {
        return Err(format_error(format!(
            "format v4 header lacks required field {missing}"
        )));
    }
    if let Some(unexpected) = object
        .keys()
        .find(|key| !REQUIRED.contains(&key.as_str()) && !OPTIONAL.contains(&key.as_str()))
    {
        let unexpected = unexpected.clone();
        return Err(format_error(format!(
            "format v4 header has unexpected field {unexpected}"
        )));
    }
    if header.get("id").is_none_or(Value::is_null)
        || header.get("id").and_then(Value::as_str).is_none()
    {
        return Err(format_error("format v4 header id must be a string"));
    }
    count(
        header.get("createdAt").unwrap_or(&Value::Null),
        "format v4 header createdAt",
    )?;
    count(
        header.get("delegationDepth").unwrap_or(&Value::Null),
        "format v4 header delegationDepth",
    )?;
    if header.get("isSeeded").and_then(Value::as_bool).is_none() {
        return Err(format_error("format v4 header isSeeded must be boolean"));
    }
    if let Some(cwd) = header.get("cwd") {
        let absolute = cwd
            .as_str()
            .is_some_and(|cwd| std::path::Path::new(cwd).is_absolute());
        if !absolute {
            return Err(format_error("format v4 header cwd must be absolute"));
        }
    }
    for key in ["parentSession", "agentPreset"] {
        if let Some(value) = header.get(key)
            && value.as_str().is_none()
        {
            return Err(format_error(format!(
                "format v4 header {key} must be a string"
            )));
        }
    }
    if let Some(origin) = header.get("origin")
        && origin.as_str() != Some("subagent")
    {
        return Err(format_error("format v4 header origin must be \"subagent\""));
    }
    Ok(())
}

fn assert_tool_change(block: &Value, developer: bool, subject: &str) -> Result<(), V4CheckError> {
    let Some(kind) = block.get("type").and_then(Value::as_str) else {
        return Ok(());
    };
    if kind != "tool-addition" && kind != "tool-removal" {
        return Ok(());
    }
    if !developer {
        return Err(format_error(format!(
            "format v4 {kind} requires developer role"
        )));
    }
    let name = block.get("toolName").and_then(Value::as_str);
    if name.is_none_or(str::is_empty) {
        return Err(format_error(format!(
            "format v4 {kind} requires a nonempty toolName"
        )));
    }
    if kind == "tool-addition"
        && block
            .as_object()
            .is_some_and(|object| object.contains_key("tool"))
    {
        return Err(format_error(
            "format v4 tool-addition must omit inline tool definitions",
        ));
    }
    let _ = subject;
    Ok(())
}

/// The developer/message data slot: developer-role message with positive
/// turn/step, tool-addition content coupled to headerSeq presence (dsh
/// `assertV4DeveloperData` developer branch).
fn assert_developer_message_row(data: &Value) -> Result<(), V4CheckError> {
    let message = data.get("message").cloned().unwrap_or(Value::Null);
    if !is_object(&message) || message.get("role").and_then(Value::as_str) != Some("developer") {
        return Err(format_error(
            "format v4 developer/message requires turn, step, and a developer message",
        ));
    }
    for field in ["turn", "step"] {
        let value = data.get(field).unwrap_or(&Value::Null);
        if count(value, &format!("developer/message {field}"))? == 0 {
            return Err(format_error(format!(
                "developer/message {field} must be positive"
            )));
        }
    }
    assert_developer_message(&message)?;
    let content = message.get("content").cloned().unwrap_or(Value::Null);
    let has_additions = content.as_array().is_some_and(|blocks| {
        blocks
            .iter()
            .any(|block| block.get("type").and_then(Value::as_str) == Some("tool-addition"))
    });
    if has_additions {
        count(
            data.get("headerSeq").unwrap_or(&Value::Null),
            "developer/message headerSeq",
        )?;
    } else if data
        .as_object()
        .is_some_and(|object| object.contains_key("headerSeq"))
    {
        return Err(format_error(
            "format v4 developer/message must omit headerSeq without tool additions",
        ));
    }
    Ok(())
}

/// Assistant stream chunks keep tool-change blocks developer-only: `block-end`
/// blocks validate against [`assert_tool_change`], and `block-start`
/// `tool-addition`/`tool-removal` blocks refuse outside developer messages
/// (dsh `assertV4DeveloperData` stream walk).
fn assert_stream_developer_role(stream: &[Value]) -> Result<(), V4CheckError> {
    for entry in stream {
        if entry.get("type").and_then(Value::as_str) != Some("chunk") {
            continue;
        }
        let Some(chunk) = entry.get("chunk") else {
            continue;
        };
        if !is_object(chunk) {
            continue;
        }
        if chunk.get("type").and_then(Value::as_str) == Some("block-end")
            && let Some(block) = chunk.get("block")
        {
            assert_tool_change(block, false, "")?;
        }
        if chunk.get("type").and_then(Value::as_str) == Some("block-start") {
            let block_type = chunk
                .get("blockType")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if block_type == "tool-addition" || block_type == "tool-removal" {
                return Err(format_error(
                    "format v4 tool-change blocks require developer role",
                ));
            }
        }
    }
    Ok(())
}

fn assert_retired_content(content: &Value, subject: &str) -> Result<(), V4CheckError> {
    let Some(blocks) = content.as_array() else {
        return Ok(());
    };
    for block in blocks {
        if block.get("type").and_then(Value::as_str) == Some("tool-result") {
            return Err(format_error(format!(
                "{subject} must not contain a released tool-result wrapper"
            )));
        }
    }
    Ok(())
}

fn assert_retired_message_content(message: &Value, subject: &str) -> Result<(), V4CheckError> {
    if is_object(message)
        && let Some(content) = message.get("content")
    {
        assert_retired_content(content, subject)?;
    }
    Ok(())
}

/// Stream chunks stay opaque except for the retired wrapper/block shapes:
/// `block-end` blocks run the retired-content refusal, `block-start`
/// `tool-result` blocks refuse outright (dsh `assertV4RetiredSyntax` stream
/// walk).
fn assert_stream_retired_syntax(stream: &[Value], subject: &str) -> Result<(), V4CheckError> {
    for entry in stream {
        if entry.get("type").and_then(Value::as_str) != Some("chunk") {
            continue;
        }
        let Some(chunk) = entry.get("chunk") else {
            continue;
        };
        if !is_object(chunk) {
            continue;
        }
        if chunk.get("type").and_then(Value::as_str) == Some("block-end")
            && let Some(block) = chunk.get("block")
        {
            assert_retired_content(&Value::Array(vec![block.clone()]), subject)?;
        }
        if chunk.get("type").and_then(Value::as_str) == Some("block-start")
            && chunk.get("blockType").and_then(Value::as_str) == Some("tool-result")
        {
            return Err(format_error(format!(
                "{subject} must not contain a released tool-result wrapper"
            )));
        }
    }
    Ok(())
}

/// Refuses retired headers, PTC tags, and tool-result blocks in interpreted
/// content slots; tool arguments, replay state, schema parameters, and nested
/// extension values remain opaque
///
/// (dsh `assertV4RetiredSyntax`).
///
/// # Errors
///
/// Returns the dsh refusal for each retired-shape failure.
pub fn assert_v4_retired_syntax(row: &Value) -> Result<(), V4CheckError> {
    let Some(object) = row.as_object() else {
        return Ok(());
    };
    let Some(kind) = object.get("type").and_then(Value::as_str) else {
        return Ok(());
    };
    if (kind == "tool/code-dispatch-start" || kind == "tool/code-dispatch")
        && object.get("ignorable").and_then(Value::as_bool) != Some(true)
    {
        return Err(V4CheckError::UnsupportedMigration(format!(
            "format v4 rejects retired event type {kind}"
        )));
    }
    let data = object.get("data").cloned().unwrap_or(Value::Null);
    if kind == "request/header" {
        if !is_object(&data) || !is_object(data.get("header").unwrap_or(&Value::Null)) {
            return Err(format_error(
                "format v4 request/header requires data and header objects",
            ));
        }
        if data
            .get("header")
            .and_then(Value::as_object)
            .is_some_and(|header| header.contains_key("system"))
        {
            return Err(format_error(
                "format v4 request/header rejects retired header.system",
            ));
        }
        return Ok(());
    }
    if !is_object(&data) {
        return Ok(());
    }
    let subject = format!("format v4 {kind} at seq {} content", subject_seq(row));
    match kind {
        "user/message" | "tool/ptc-dispatch" => {
            if let Some(content) = data.get("content") {
                assert_retired_content(content, &subject)?;
            }
        }
        "developer/message" | "assistant/message" | "team/message/queued" => {
            if let Some(message) = data.get("message") {
                assert_retired_message_content(message, &subject)?;
            }
        }
        "agent/inbox/spliced" | "session/title-llm-request" => {
            let key = if kind == "agent/inbox/spliced" {
                "inserted"
            } else {
                "messages"
            };
            if let Some(messages) = data.get(key).and_then(Value::as_array) {
                for message in messages {
                    assert_retired_message_content(message, &subject)?;
                }
            }
        }
        "compaction/summary" => {
            for key in ["summary", "rawOutput"] {
                if let Some(content) = data.get(key) {
                    assert_retired_content(content, &subject)?;
                }
            }
        }
        _ => {}
    }
    if (kind == "assistant/message" || kind == "assistant/attempt")
        && let Some(stream) = data.get("stream").and_then(Value::as_array)
    {
        assert_stream_retired_syntax(stream, &subject)?;
    }
    Ok(())
}

fn require_record<'a>(
    value: &'a Value,
    subject: &str,
) -> Result<&'a serde_json::Map<String, Value>, V4CheckError> {
    value
        .as_object()
        .ok_or_else(|| format_error(format!("format v4 {subject} requires an object")))
}

fn require_string(value: &Value, subject: &str, nonempty: bool) -> Result<(), V4CheckError> {
    let ok = value
        .as_str()
        .is_some_and(|text| !nonempty || !text.is_empty());
    if ok {
        Ok(())
    } else {
        let suffix = if nonempty { " with content" } else { "" };
        Err(format_error(format!(
            "format v4 {subject} requires a string{suffix}"
        )))
    }
}

fn require_positive(value: &Value, subject: &str) -> Result<(), V4CheckError> {
    if count(value, subject)? == 0 {
        return Err(format_error(format!(
            "format v4 {subject} must be positive"
        )));
    }
    Ok(())
}

fn assert_system_image_attachment(value: &Value) -> Result<(), V4CheckError> {
    let attachment = require_record(value, "system image attachment")?;
    require_string(
        attachment.get("attachmentId").unwrap_or(&Value::Null),
        "system image attachmentId",
        true,
    )?;
    let media_ok = attachment
        .get("mediaType")
        .and_then(Value::as_str)
        .is_some_and(|media| {
            matches!(
                media,
                "image/png" | "image/jpeg" | "image/webp" | "image/gif"
            )
        });
    if !media_ok {
        return Err(format_error(
            "format v4 system image requires a supported mediaType",
        ));
    }
    count(
        attachment.get("bytes").unwrap_or(&Value::Null),
        "system image bytes",
    )?;
    require_positive(
        attachment.get("width").unwrap_or(&Value::Null),
        "system image width",
    )?;
    require_positive(
        attachment.get("height").unwrap_or(&Value::Null),
        "system image height",
    )?;
    if let Some(name) = attachment.get("name") {
        require_string(name, "system image name", false)?;
    }
    if let Some(original) = attachment.get("originalDimensions") {
        let original = require_record(original, "system image originalDimensions")?;
        require_positive(
            original.get("width").unwrap_or(&Value::Null),
            "system image original width",
        )?;
        require_positive(
            original.get("height").unwrap_or(&Value::Null),
            "system image original height",
        )?;
    }
    Ok(())
}

/// Validates system-message fields independently of historical message
/// representations; additional JSON fields are preserved (dsh
/// `assertV4SystemMessageFields`).
///
/// # Errors
///
/// Returns the dsh refusal for each malformed system field.
pub fn assert_v4_system_message_fields(row: &Value) -> Result<(), V4CheckError> {
    if row.get("type").and_then(Value::as_str) != Some("system/message") {
        return Ok(());
    }
    let data = require_record(
        row.get("data").unwrap_or(&Value::Null),
        "system/message data",
    )?;
    require_positive(
        data.get("turn").unwrap_or(&Value::Null),
        "system/message turn",
    )?;
    require_positive(
        data.get("step").unwrap_or(&Value::Null),
        "system/message step",
    )?;
    let message = require_record(
        data.get("message").unwrap_or(&Value::Null),
        "system message",
    )?;
    require_string(
        message.get("id").unwrap_or(&Value::Null),
        "system message id",
        true,
    )?;
    if message.get("role").and_then(Value::as_str) != Some("system") {
        return Err(format_error(
            "format v4 system message requires system role",
        ));
    }
    let Some(content) = message.get("content").and_then(Value::as_array) else {
        return Err(format_error(
            "format v4 system message requires content array",
        ));
    };
    for value in content {
        let block = require_record(value, "system content block")?;
        let kind = block.get("type").and_then(Value::as_str).ok_or_else(|| {
            format_error("format v4 system content type requires a string with content")
        })?;
        match kind {
            "text" | "reasoning" => {
                require_string(
                    block.get("text").unwrap_or(&Value::Null),
                    "system content text",
                    false,
                )?;
            }
            "tool-call" => {
                require_string(
                    block.get("id").unwrap_or(&Value::Null),
                    "system tool-call id",
                    true,
                )?;
                require_string(
                    block.get("name").unwrap_or(&Value::Null),
                    "system tool-call name",
                    true,
                )?;
                require_string(
                    block.get("arguments").unwrap_or(&Value::Null),
                    "system tool-call arguments",
                    false,
                )?;
            }
            "image" => {
                assert_system_image_attachment(block.get("attachment").unwrap_or(&Value::Null))?;
            }
            "tool-result" => {
                return Err(format_error(
                    "format v4 system content rejects retired tool-result wrappers",
                ));
            }
            // ContentBlockMap is merge-extensible; only known block fields
            // are interpreted here.
            _ => {}
        }
    }
    Ok(())
}

/// Validates the native v4 first-class tool-role message of one
/// `tool/result` row (dsh `assertV4ToolResultMessage`).
///
/// # Errors
///
/// Returns the dsh refusal for each malformed tool-result message.
pub fn assert_v4_tool_result_message(row: &Value) -> Result<(), V4CheckError> {
    if row.get("type").and_then(Value::as_str) != Some("tool/result") {
        return Ok(());
    }
    let subject = format!("format v4 tool/result at seq {}", subject_seq(row));
    let data = require_record(row.get("data").unwrap_or(&Value::Null), &subject)
        .map_err(|_error| format_error(format!("{subject} data must be an object")))?;
    let message = data
        .get("message")
        .ok_or_else(|| format_error(format!("{subject} message must be an object")))?;
    if !is_object(message) {
        return Err(format_error(format!("{subject} message must be an object")));
    }
    let id = message.get("id").unwrap_or(&Value::Null);
    if id.as_str().is_none_or(str::is_empty) {
        return Err(format_error(format!(
            "{subject} requires a first-class message with a string id"
        )));
    }
    if message.get("role").and_then(Value::as_str) != Some("tool") {
        return Err(format_error(format!(
            "{subject} requires a tool-role message"
        )));
    }
    let tool_call_id = message.get("toolCallId").unwrap_or(&Value::Null);
    let source = message.get("source").unwrap_or(&Value::Null);
    let source_call_id = source.get("callId");
    if tool_call_id.as_str().is_none_or(str::is_empty) || (source_call_id != Some(tool_call_id)) {
        return Err(format_error(format!(
            "{subject} requires toolCallId matching its tool source"
        )));
    }
    if !is_object(source) || source.get("kind").and_then(Value::as_str) != Some("tool") {
        return Err(format_error(format!("{subject} requires a tool source")));
    }
    let Some(content) = message.get("content").and_then(Value::as_array) else {
        return Err(format_error(format!("{subject} requires array content")));
    };
    if content
        .iter()
        .any(|block| block.get("type").and_then(Value::as_str) == Some("tool-result"))
    {
        return Err(format_error(format!(
            "{subject} content must not contain a released tool-result wrapper"
        )));
    }
    if let Some(is_error) = message.get("isError")
        && !is_error.is_boolean()
    {
        return Err(format_error(format!(
            "{subject} isError must be boolean when present"
        )));
    }
    if data.get("error").is_some() {
        let is_error_true = message
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !is_error_true {
            return Err(format_error(format!(
                "{subject} carries error metadata for a non-error tool result"
            )));
        }
    }
    Ok(())
}

/// Validates fork-generated not-started results without changing their
/// identity or text (dsh `assertV4ForkResult`).
///
/// # Errors
///
/// Returns `invalid V4 not-started fork result` on any mismatch.
pub fn assert_v4_fork_result(row: &Value) -> Result<(), V4CheckError> {
    if row.get("type").and_then(Value::as_str) != Some("tool/result") {
        return Ok(());
    }
    let Some(data) = row.get("data").and_then(Value::as_object) else {
        return Ok(());
    };
    let error = data.get("error").cloned().unwrap_or(Value::Null);
    let message = data.get("message").cloned().unwrap_or(Value::Null);
    let code = error.get("code").and_then(Value::as_str);
    if code != Some("TOOL_NOT_STARTED") {
        return Ok(());
    }
    let id = message
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !id.starts_with("forked-tool-result-") {
        return Ok(());
    }
    let source = message.get("source").cloned().unwrap_or(Value::Null);
    let call_id = source.get("callId").cloned().unwrap_or(Value::Null);
    let call_id_text = call_id.as_str().unwrap_or_default();
    let prefix = format!("forked-tool-result-{call_id_text}-");
    let suffix = id.strip_prefix(&prefix).unwrap_or_default();
    let operation = row.get("surfaceOp").cloned().unwrap_or(Value::Null);
    let replacement = operation.get("op").and_then(Value::as_str) == Some("replace");
    let sequence: Option<u64> = suffix
        .parse::<u64>()
        .ok()
        .filter(|parsed| parsed.to_string() == suffix);
    let content = message.get("content").cloned().unwrap_or(Value::Null);
    let text = content
        .as_array()
        .filter(|blocks| blocks.len() == 1)
        .and_then(|blocks| blocks.first().cloned())
        .unwrap_or(Value::Null);
    let source_event_seqs = row.get("sourceEventSeqs").cloned().unwrap_or(Value::Null);
    let row_seq = row.get("seq").cloned().unwrap_or(Value::Null);
    let ok = call_id.as_str().is_some()
        && id.starts_with(&prefix)
        && !suffix.is_empty()
        && !suffix.starts_with('0')
        && suffix.bytes().all(|byte| byte.is_ascii_digit())
        && sequence.is_some()
        && if replacement {
            row_seq.as_u64().is_some()
                && sequence.is_some_and(|sequence| sequence < row_seq.as_u64().unwrap_or(0))
        } else {
            sequence == row_seq.as_u64()
        }
        && error.get("name").and_then(Value::as_str) == Some("ToolNotStartedError")
        && if replacement {
            source_event_seqs
                .as_array()
                .is_some_and(|seqs| seqs.len() == 1 && seqs[0].as_u64() == sequence)
        } else {
            source_event_seqs.is_null() && operation.as_str() == Some("append")
        }
        && message.get("role").and_then(Value::as_str) == Some("tool")
        && message.get("isError").and_then(Value::as_bool) == Some(true)
        && message.get("toolCallId") == Some(&call_id)
        && source.get("kind").and_then(Value::as_str) == Some("tool")
        && text.get("type").and_then(Value::as_str) == Some("text")
        && text.get("text").and_then(Value::as_str).is_some();
    if !ok {
        return Err(format_error("invalid V4 not-started fork result"));
    }
    Ok(())
}

fn assert_developer_content(content: &Value, developer: bool) -> Result<(), V4CheckError> {
    let Some(blocks) = content.as_array() else {
        return Ok(());
    };
    for block in blocks {
        assert_tool_change(block, developer, "")?;
    }
    Ok(())
}

fn assert_developer_message(message: &Value) -> Result<(), V4CheckError> {
    if let Some(content) = message.get("content") {
        assert_developer_content(content, true)?;
    }
    let source = message.get("source").unwrap_or(&Value::Null);
    let id_ok = message
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.is_empty());
    let content_ok = message.get("content").is_some_and(Value::is_array);
    let kind = source.get("kind").and_then(Value::as_str);
    let source_ok =
        is_object(source) && kind.is_some_and(|kind| !kind.is_empty() && kind != "plugin");
    if !id_ok || !content_ok || !source_ok {
        return Err(format_error(
            "format v4 developer message requires id, role, content, and a producer-owned source",
        ));
    }
    Ok(())
}

fn assert_ordinary_message(value: &Value) -> Result<(), V4CheckError> {
    if !is_object(value) {
        return Ok(());
    }
    if value.get("role").and_then(Value::as_str) == Some("developer") {
        return Err(format_error(
            "format v4 developer/message and developer role must occur together",
        ));
    }
    if let Some(content) = value.get("content") {
        assert_developer_content(content, false)?;
    }
    Ok(())
}

/// Validates native v4 developer messages, tool-change blocks, and deferred
/// tool schemas; additional developer JSON fields are preserved (dsh
/// `assertV4DeveloperData`).
///
/// # Errors
///
/// Returns the dsh refusal for each malformed developer or ordinary
/// message slot.
pub fn assert_v4_developer_data(row: &Value) -> Result<(), V4CheckError> {
    let kind = row.get("type").and_then(Value::as_str).unwrap_or_default();
    let data = row.get("data").cloned().unwrap_or(Value::Null);
    if !is_object(&data) {
        if kind == "developer/message" {
            return Err(format_error(
                "format v4 developer/message data must be an object",
            ));
        }
        return Ok(());
    }
    if kind == "developer/message" {
        assert_developer_message_row(&data)?;
    } else {
        match kind {
            "user/message" => assert_ordinary_message(&data)?,
            "system/message" | "assistant/message" | "tool/result" => {
                if let Some(message) = data.get("message") {
                    assert_ordinary_message(message)?;
                }
            }
            "agent/inbox/spliced" | "session/title-llm-request" => {
                let key = if kind == "agent/inbox/spliced" {
                    "inserted"
                } else {
                    "messages"
                };
                if let Some(messages) = data.get(key).and_then(Value::as_array) {
                    for message in messages {
                        assert_ordinary_message(message)?;
                    }
                }
            }
            _ => {}
        }
    }
    if kind == "compaction/summary" {
        for key in ["summary", "rawOutput"] {
            if let Some(content) = data.get(key) {
                assert_developer_content(content, false)?;
            }
        }
    }
    if (kind == "assistant/message" || kind == "assistant/attempt")
        && let Some(stream) = data.get("stream").and_then(Value::as_array)
    {
        assert_stream_developer_role(stream)?;
    }
    if kind == "request/header"
        && let Some(header) = data.get("header")
        && is_object(header)
        && let Some(tools) = header.get("tools").and_then(Value::as_array)
    {
        for tool in tools {
            let Some(tool) = tool.as_object() else {
                continue;
            };
            if !tool.contains_key("deferLoading") {
                continue;
            }
            if tool.get("deferLoading") != Some(&Value::Bool(true)) {
                return Err(format_error(
                    "format v4 tool deferLoading must be true when present",
                ));
            }
        }
    }
    Ok(())
}

/// Validates producer attribution in every declared durable message slot
/// (dsh `assertV4MessageSources` via `mapEventMessages`).
///
/// # Errors
///
/// Returns the dsh refusal for a missing message slot, a non-message
/// member, or a source without a producer-owned kind.
pub fn assert_v4_message_sources(row: &Value) -> Result<(), V4CheckError> {
    let kind = row.get("type").and_then(Value::as_str).unwrap_or_default();
    let Some(data) = row.get("data").filter(|data| is_object(data)) else {
        return Ok(());
    };
    let check_source = |message: &Value| -> Result<(), V4CheckError> {
        let source = message.get("source").cloned().unwrap_or(Value::Null);
        let name = source.get("kind").and_then(Value::as_str);
        let ok =
            is_object(&source) && name.is_some_and(|name| !name.is_empty() && name != "plugin");
        if ok {
            Ok(())
        } else {
            Err(format_error(
                "format v4 message requires a producer-owned source kind",
            ))
        }
    };
    if kind == "user/message" {
        return check_source(data);
    }
    if matches!(
        kind,
        "developer/message" | "system/message" | "assistant/message" | "tool/result"
    ) {
        let message = data
            .get("message")
            .filter(|message| is_object(message))
            .ok_or_else(|| format_error(format!("{kind} requires a message")))?;
        return check_source(message);
    }
    let key = match kind {
        "agent/inbox/spliced" => "inserted",
        "session/title-llm-request" => "messages",
        _ => return Ok(()),
    };
    let messages = data
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format_error(format!("{kind} requires message array")))?;
    for message in messages {
        if !is_object(message) {
            return Err(format_error(format!("{kind} requires message objects")));
        }
        check_source(message)?;
    }
    Ok(())
}

/// Refuses retired source syntax even after a malformed recoverable row;
/// leaves incomplete messages to decoding (dsh
/// `assertV4SourceRowAdmission`): only an exactly-`plugin` kind refuses
/// here.
///
/// # Errors
///
/// Returns the dsh refusal when a declared slot's source kind is `plugin`.
pub fn assert_v4_source_row_admission(row: &Value) -> Result<(), V4CheckError> {
    let Some(data) = row.get("data") else {
        return Ok(());
    };
    if !is_object(data) {
        return Ok(());
    }
    let kind = row.get("type").and_then(Value::as_str).unwrap_or_default();
    let messages: Vec<&Value> = match kind {
        "user/message" => vec![data],
        "system/message" | "assistant/message" | "tool/result" => {
            vec![&data["message"]]
        }
        "agent/inbox/spliced" | "session/title-llm-request" => {
            let key = if kind == "agent/inbox/spliced" {
                "inserted"
            } else {
                "messages"
            };
            match data.get(key).and_then(Value::as_array) {
                Some(found) => found.iter().collect(),
                None => return Ok(()),
            }
        }
        _ => return Ok(()),
    };
    for message in messages {
        let Some(message) = message.as_object() else {
            continue;
        };
        if message
            .get("source")
            .and_then(|source| source.get("kind"))
            .and_then(Value::as_str)
            == Some("plugin")
        {
            return Err(format_error(
                "format v4 message requires a producer-owned source kind",
            ));
        }
    }
    Ok(())
}

/// Applies native v4 admission before a scanner discards a recoverable
/// suffix (dsh `assertV4RowAdmission`). Ignorable developer payloads
/// require reader vocabulary; physical decoding defers them.
///
/// # Errors
///
/// Returns the dsh refusal; [`V4CheckError::UnsupportedMigration`]
/// classifies a format-unsupported refusal, the rest corrupt-artifact
/// refusals.
pub fn assert_v4_row_admission(
    row: &Value,
    known_event_types: &EventVocabulary,
) -> Result<(), V4CheckError> {
    let kind = row.get("type").and_then(Value::as_str).unwrap_or_default();
    let ignorable = row.get("ignorable").and_then(Value::as_bool) == Some(true);
    if kind == "developer/message" && ignorable && !known_event_types.knows(kind) {
        return Ok(());
    }
    assert_v4_developer_data(row)?;
    assert_v4_source_row_admission(row)?;
    assert_v4_retired_syntax(row)?;
    assert_v4_system_message_fields(row)?;
    if kind != "tool/result" {
        return Ok(());
    }
    assert_v4_tool_result_message(row)?;
    assert_v4_fork_result(row)
}

struct ToolState {
    name: Value,
    arguments: Value,
    started: bool,
}

struct CompactionState {
    id: String,
    command: Value,
    turn: Option<u64>,
    seq: u64,
    summarized: bool,
}

struct DispatchEntry {
    data: serde_json::Map<String, Value>,
    settled: bool,
}

/// One native artifact's lifecycle state; no state survives between reads
/// (dsh `Relationships`).
struct Relationships<'a> {
    events: &'a [Value],
    known: &'a EventVocabulary,
    turn: Option<u64>,
    step: Option<u64>,
    next_turn: u64,
    next_step: u64,
    provider: Value,
    surface: Vec<u64>,
    protected_head: Option<u64>,
    compaction: Option<CompactionState>,
    tools: indexmap::IndexMap<String, ToolState>,
    dispatches: std::collections::HashMap<String, DispatchEntry>,
    retries: Vec<serde_json::Map<String, Value>>,
    started_retries: std::collections::HashSet<String>,
    commands: std::collections::HashSet<String>,
    orphan_compactions: std::collections::HashSet<u64>,
}

impl<'a> Relationships<'a> {
    fn new(events: &'a [Value], known: &'a EventVocabulary) -> Self {
        let mut orphan_compactions = std::collections::HashSet::new();
        let mut start: Option<u64> = None;
        for event in events {
            let kind = event
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !known.knows(kind) {
                continue;
            }
            if kind == "compaction/start" {
                start = event.get("seq").and_then(Value::as_u64);
            }
            if kind == "compaction/end" {
                start = None;
            }
            if kind == "session/end-seed"
                && let Some(start) = start.take()
            {
                orphan_compactions.insert(start);
            }
        }
        Self {
            events,
            known,
            turn: None,
            step: None,
            next_turn: 1,
            next_step: 1,
            provider: Value::Null,
            surface: Vec::new(),
            protected_head: None,
            compaction: None,
            tools: indexmap::IndexMap::new(),
            dispatches: std::collections::HashMap::new(),
            retries: Vec::new(),
            started_retries: std::collections::HashSet::new(),
            commands: std::collections::HashSet::new(),
            orphan_compactions,
        }
    }

    fn require_turn(&self, kind: &str) -> Result<(), V4CheckError> {
        if self.turn.is_none() {
            return Err(format_error(format!("{kind} is outside an open turn")));
        }
        Ok(())
    }

    fn require_step(
        &self,
        kind: &str,
        data: &serde_json::Map<String, Value>,
    ) -> Result<(), V4CheckError> {
        let data_turn = data.get("turn").and_then(Value::as_u64);
        let data_step = data.get("step").and_then(Value::as_u64);
        if self.turn.is_none()
            || self.step.is_none()
            || data_turn != self.turn
            || data_step != self.step
        {
            return Err(format_error(format!(
                "{kind} does not match an open turn and step"
            )));
        }
        Ok(())
    }

    fn close_tools(&mut self, kind: &str) -> Result<(), V4CheckError> {
        if let Some((unresolved, _)) = self.tools.pop() {
            return Err(format_error(format!(
                "{kind} leaves unresolved tool call {unresolved}"
            )));
        }
        Ok(())
    }

    fn developer(
        &self,
        seq: u64,
        data: &serde_json::Map<String, Value>,
    ) -> Result<(), V4CheckError> {
        let Some(header_seq) = data.get("headerSeq") else {
            return Ok(());
        };
        let header_seq = earlier(header_seq, seq, "developer/message headerSeq")?;
        let header_event = usize::try_from(header_seq)
            .ok()
            .and_then(|index| self.events.get(index));
        let Some(header_event) = header_event else {
            return Err(format_error(
                "developer/message headerSeq must reference an earlier request/header",
            ));
        };
        if header_event.get("type").and_then(Value::as_str) != Some("request/header") {
            return Err(format_error(
                "developer/message headerSeq must reference an earlier request/header",
            ));
        }
        if !self.known.knows("request/header") {
            return Err(format_error(
                "developer/message headerSeq must reference a known request/header",
            ));
        }
        let tools = header_event
            .get("data")
            .and_then(|data| data.get("header"))
            .and_then(|header| header.get("tools"))
            .cloned()
            .unwrap_or(Value::Null);
        let content = data
            .get("message")
            .and_then(|message| message.get("content"))
            .cloned()
            .unwrap_or(Value::Null);
        let Some(blocks) = content.as_array() else {
            return Err(format_error("developer content requires an array"));
        };
        for value in blocks {
            if value.get("type").and_then(Value::as_str) != Some("tool-addition") {
                continue;
            }
            let tool_name = value
                .get("toolName")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let definitions: Vec<&Value> = tools
                .as_array()
                .map(|tools| {
                    tools
                        .iter()
                        .filter(|tool| {
                            is_object(tool)
                                && tool.get("name").and_then(Value::as_str) == Some(tool_name)
                        })
                        .collect()
                })
                .unwrap_or_default();
            if definitions.len() != 1 {
                return Err(format_error(format!(
                    "developer/message tool-addition \"{tool_name}\" must name exactly one tool in headerSeq {header_seq}"
                )));
            }
            let definition = definitions[0];
            let description_ok = definition
                .get("description")
                .and_then(Value::as_str)
                .is_some();
            let parameters_ok = is_object(definition.get("parameters").unwrap_or(&Value::Null));
            if !description_ok || !parameters_ok {
                return Err(format_error(format!(
                    "developer/message tool-addition \"{tool_name}\" requires a complete tool definition in headerSeq {header_seq}"
                )));
            }
        }
        Ok(())
    }

    fn fold_surface(&mut self, kind: &str, seq: u64, row: &Value) -> Result<(), V4CheckError> {
        if !SURFACE_TYPES.contains(&kind) {
            return Ok(());
        }
        if kind == "system/message" && !self.surface.is_empty() && self.protected_head.is_none() {
            return Err(format_error(
                "system/message requires a protected first surface head",
            ));
        }
        if row.get("surfaceOp").and_then(Value::as_str) == Some("append") {
            if kind == "system/message" && self.surface.is_empty() {
                self.protected_head = Some(seq);
            }
            self.surface.push(seq);
            return Ok(());
        }
        let operation = row.get("surfaceOp").cloned().unwrap_or(Value::Null);
        let start = earlier(
            operation.get("startSeq").unwrap_or(&Value::Null),
            seq,
            "replacement start",
        )?;
        let end = earlier(
            operation.get("endSeq").unwrap_or(&Value::Null),
            seq,
            "replacement end",
        )?;
        let first = self.surface.iter().position(|node| *node == start);
        let last = self.surface.iter().position(|node| *node == end);
        let (Some(first), Some(last)) = (first, last) else {
            return Err(format_error(format!(
                "{kind} replacement range is not on the current surface"
            )));
        };
        if last < first {
            return Err(format_error(format!(
                "{kind} replacement range is not on the current surface"
            )));
        }
        let removed: Vec<u64> = self.surface[first..=last].to_vec();
        let sources = row.get("sourceEventSeqs").cloned().unwrap_or(Value::Null);
        let Some(sources) = sources.as_array() else {
            return Err(format_error(
                "replacement sourceEventSeqs requires an array",
            ));
        };
        let sources: Vec<u64> = sources
            .iter()
            .map(|value| value.as_u64().unwrap_or(u64::MAX))
            .collect();
        if removed.iter().any(|seq| !sources.contains(seq)) {
            return Err(format_error(
                "replacement sourceEventSeqs omit a shadowed surface node",
            ));
        }
        if let Some(head) = self.protected_head
            && removed.contains(&head)
        {
            if kind != "system/message" || removed.len() != 1 {
                return Err(format_error(
                    "surface replacement cannot shadow the protected system head",
                ));
            }
            self.protected_head = Some(seq);
        }
        self.surface.splice(first..=last, std::iter::once(seq));
        Ok(())
    }

    fn tool(
        &mut self,
        kind: &str,
        _seq: u64,
        row: &Value,
        data: &serde_json::Map<String, Value>,
    ) -> Result<(), V4CheckError> {
        if kind == "tool/result" && row.get("surfaceOp").and_then(Value::as_str) != Some("append") {
            self.require_turn(kind)?;
            return Ok(());
        }
        self.require_step(kind, data)?;
        if kind == "assistant/message" {
            let message = data.get("message").cloned().unwrap_or(Value::Null);
            let content = message.get("content").cloned().unwrap_or(Value::Null);
            let Some(blocks) = content.as_array() else {
                return Err(format_error("assistant content requires an array"));
            };
            for value in blocks {
                if value.get("type").and_then(Value::as_str) != Some("tool-call") {
                    continue;
                }
                let id = value
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| format_error("tool call id requires a nonempty string"))?;
                if self.tools.contains_key(id) {
                    return Err(format_error(format!(
                        "assistant/message repeats advertised tool call {id}"
                    )));
                }
                self.tools.insert(
                    id.to_owned(),
                    ToolState {
                        name: value.get("name").cloned().unwrap_or(Value::Null),
                        arguments: value.get("arguments").cloned().unwrap_or(Value::Null),
                        started: false,
                    },
                );
            }
            return Ok(());
        }
        let message =
            (kind == "tool/result").then(|| data.get("message").cloned().unwrap_or(Value::Null));
        let id_text = message.as_ref().map_or_else(
            || {
                data.get("callId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            },
            |message| {
                message
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            },
        );
        if id_text.is_empty() {
            return Err(format_error("tool call id requires a nonempty string"));
        }
        let Some(pending) = self.tools.get(id_text) else {
            return Err(format_error(format!(
                "{kind} {id_text} has no advertised tool lifecycle"
            )));
        };
        let pending_started = pending.started;
        let pending_name = pending.name.clone();
        let pending_arguments = pending.arguments.clone();
        match &message {
            None => {
                if pending_started
                    || pending_name != data.get("name").cloned().unwrap_or(Value::Null)
                    || pending_arguments != data.get("arguments").cloned().unwrap_or(Value::Null)
                {
                    return Err(format_error(format!(
                        "tool/call {id_text} does not match one advertised tool call"
                    )));
                }
                if let Some(pending) = self.tools.get_mut(id_text) {
                    pending.started = true;
                }
            }
            Some(message) => {
                if !pending_started && !not_started_repair(row, data, message, id_text) {
                    return Err(format_error(format!(
                        "tool/result {id_text} is not the exact TOOL_NOT_STARTED repair"
                    )));
                }
                self.tools.shift_remove(id_text);
            }
        }
        Ok(())
    }

    fn dispatch(
        &mut self,
        kind: &str,
        row: &Value,
        data: &serde_json::Map<String, Value>,
    ) -> Result<(), V4CheckError> {
        self.require_turn(kind)?;
        let id = text_field(data, "subCallId", "PTC subCallId")?;
        let root = text_field(data, "rootCallId", "PTC rootCallId")?;
        let parent = text_field(data, "parentCallId", "PTC parentCallId")?;
        if let Some(existing) = self.dispatches.get(id)
            && existing.data.get("rootCallId").and_then(Value::as_str) != Some(root)
        {
            return Err(format_error("PTC dispatch changes its rootCallId"));
        }
        if parent != root
            && self.dispatches.get(parent).is_none_or(|entry| {
                entry.data.get("rootCallId").and_then(Value::as_str) != Some(root)
            })
        {
            return Err(format_error(
                "PTC parentCallId does not belong to rootCallId",
            ));
        }
        if kind == "tool/ptc-dispatch-start" {
            if self.dispatches.contains_key(id) {
                return Err(format_error("PTC dispatch repeats subCallId"));
            }
            self.dispatches.insert(
                id.to_owned(),
                DispatchEntry {
                    data: data.clone(),
                    settled: false,
                },
            );
            return Ok(());
        }
        let Some(existing) = self.dispatches.get_mut(id) else {
            return Err(format_error("PTC dispatch has no unique start"));
        };
        if existing.settled {
            return Err(format_error("PTC dispatch has no unique start"));
        }
        for key in ["rootCallId", "parentCallId", "name", "arguments"] {
            if existing.data.get(key) != data.get(key) {
                return Err(format_error("PTC dispatch does not match its start"));
            }
        }
        existing.settled = true;
        let _ = row;
        Ok(())
    }

    fn retry(
        &mut self,
        kind: &str,
        data: &serde_json::Map<String, Value>,
    ) -> Result<(), V4CheckError> {
        let id = text_field(data, "retryId", "retryId")?;
        let attempt = count(data.get("retry").unwrap_or(&Value::Null), "retry")?;
        if kind == "llm/retry-started" {
            let scheduled = self.retries.iter().find(|item| {
                item.get("retryId").and_then(Value::as_str) == Some(id)
                    && item.get("retry").and_then(Value::as_u64) == Some(attempt)
            });
            let Some(scheduled) = scheduled else {
                return Err(format_error(
                    "llm/retry-started pairs no prior scheduled attempt",
                ));
            };
            if scheduled.get("turn") != data.get("turn")
                || scheduled.get("step") != data.get("step")
            {
                return Err(format_error(
                    "llm/retry-started changes scheduled coordinates",
                ));
            }
            let key = format!("{id}\u{0}{attempt}");
            if !self.started_retries.insert(key) {
                return Err(format_error(
                    "llm/retry-started repeats one scheduled attempt",
                ));
            }
            return Ok(());
        }
        let data_turn = data.get("turn").and_then(Value::as_u64);
        let step_basis = self.step.unwrap_or_else(|| self.next_step - 1);
        if self.turn.is_none()
            || data_turn != self.turn
            || data.get("step").and_then(Value::as_u64) != Some(step_basis)
        {
            return Err(format_error(
                "llm/retry does not match the current turn and step",
            ));
        }
        if data.get("provider").cloned().unwrap_or(Value::Null) != self.provider {
            return Err(format_error(
                "llm/retry provider does not match the open request/header",
            ));
        }
        let chain_keys = ["turn", "step", "provider", "policyKey"];
        let prior = self
            .retries
            .iter()
            .enumerate()
            .rev()
            .find(|(_, item)| {
                chain_keys
                    .iter()
                    .all(|key| item.get(*key) == data.get(*key))
            })
            .map(|(index, _)| index);
        let expected = match prior {
            None => 1,
            Some(index) => self.retries[index]
                .get("retry")
                .and_then(Value::as_u64)
                .map_or(1, |retry| retry + 1),
        };
        if attempt != expected {
            return Err(format_error("llm/retry skips its policy attempt sequence"));
        }
        let keeps_chain = match prior {
            None => !self
                .retries
                .iter()
                .any(|item| item.get("retryId").and_then(Value::as_str) == Some(id)),
            Some(index) => self.retries[index].get("retryId").and_then(Value::as_str) == Some(id),
        };
        if !keeps_chain {
            return Err(format_error(
                "llm/retry must keep one retryId per policy chain",
            ));
        }
        self.retries.push(data.clone());
        Ok(())
    }

    fn compact(
        &mut self,
        kind: &str,
        seq: u64,
        _row: &Value,
        data: &serde_json::Map<String, Value>,
    ) -> Result<(), V4CheckError> {
        if kind == "compaction/prune" || kind == "compaction/summary" {
            self.span(kind, seq, data)?;
        }
        if kind == "compaction/prune" {
            return Ok(());
        }
        if kind == "compaction/start" {
            if self.compaction.is_some() {
                return Err(format_error("compaction/start overlaps an open compaction"));
            }
            let owner = if data.get("turn") == Some(&Value::Null) {
                None
            } else {
                Some(count(
                    data.get("turn").unwrap_or(&Value::Null),
                    "compaction turn",
                )?)
            };
            if owner != self.turn {
                return Err(format_error(
                    "compaction/start does not match the open turn",
                ));
            }
            let id = text_field(data, "compactionId", "compactionId")?;
            self.compaction = Some(CompactionState {
                id: id.to_owned(),
                command: data.get("sourceCommandId").cloned().unwrap_or(Value::Null),
                turn: owner,
                seq,
                summarized: false,
            });
            return Ok(());
        }
        let current = self
            .compaction
            .as_mut()
            .ok_or_else(|| format_error(format!("{kind} has no matching compaction/start")))?;
        if current.id != text_field(data, "compactionId", kind)?
            || current.command != data.get("sourceCommandId").cloned().unwrap_or(Value::Null)
        {
            return Err(format_error(format!(
                "{kind} has no matching compaction/start"
            )));
        }
        if current.turn != self.turn {
            return Err(format_error(format!("{kind} does not match the open turn")));
        }
        if kind == "compaction/summary" {
            if current.summarized {
                return Err(format_error("compaction/summary repeats"));
            }
            current.summarized = true;
        } else {
            let owner_ok = current.turn.map_or_else(
                || data.get("turn") == Some(&Value::Null),
                |turn| data.get("turn").and_then(Value::as_u64) == Some(turn),
            );
            if !owner_ok {
                return Err(format_error("compaction/end changes its owner turn"));
            }
            if data.get("error").is_none() && !current.summarized {
                return Err(format_error(
                    "successful compaction/end requires one summary",
                ));
            }
            self.compaction = None;
        }
        Ok(())
    }

    fn span(
        &self,
        kind: &str,
        seq: u64,
        data: &serde_json::Map<String, Value>,
    ) -> Result<(), V4CheckError> {
        let range = data.get("shadowedRange").cloned().unwrap_or(Value::Null);
        let start = earlier(
            range.get("start").unwrap_or(&Value::Null),
            seq,
            "compaction range start",
        )?;
        let end = earlier(
            range.get("end").unwrap_or(&Value::Null),
            seq,
            "compaction range end",
        )?;
        let first = self.surface.iter().position(|node| *node == start);
        let last = self.surface.iter().position(|node| *node == end);
        let (Some(first), Some(last)) = (first, last) else {
            return Err(format_error(format!(
                "{kind} shadowedSeqs do not name an exact current surface span"
            )));
        };
        if last < first {
            return Err(format_error(format!(
                "{kind} shadowedSeqs do not name an exact current surface span"
            )));
        }
        let seqs = data.get("shadowedSeqs").cloned().unwrap_or(Value::Null);
        let Some(seqs) = seqs.as_array() else {
            return Err(format_error("compaction shadowedSeqs requires an array"));
        };
        let expected: Vec<Value> = self.surface[first..=last]
            .iter()
            .map(|node| Value::from(*node))
            .collect();
        if seqs != &expected {
            return Err(format_error(format!(
                "{kind} shadowedSeqs do not name an exact current surface span"
            )));
        }
        if let Some(head) = self.protected_head
            && seqs.iter().any(|value| value.as_u64() == Some(head))
        {
            return Err(format_error(
                "compaction cannot shadow the protected system head",
            ));
        }
        Ok(())
    }

    fn accept(&mut self, row: &Value) -> Result<(), V4CheckError> {
        let kind = row.get("type").and_then(Value::as_str).unwrap_or_default();
        if !self.known.knows(kind) || !RELATIONSHIP_TYPES.contains(&kind) {
            return Ok(());
        }
        let data = row
            .get("data")
            .and_then(Value::as_object)
            .ok_or_else(|| format_error(format!("{kind} requires an object")))?;
        let seq = row
            .get("seq")
            .and_then(Value::as_u64)
            .ok_or_else(|| format_error(format!("{kind} requires an object seq")))?;
        if STEP_EVENT_TYPES.contains(&kind) {
            self.require_step(kind, data)?;
        }
        if kind.starts_with("turn/")
            && let Some(compaction) = &self.compaction
            && !self.orphan_compactions.contains(&compaction.seq)
        {
            return Err(format_error(format!("{kind} crosses an open compaction")));
        }
        self.fold_surface(kind, seq, row)?;
        match kind {
            "turn/start" => {
                if self.turn.is_some()
                    || data.get("turn").and_then(Value::as_u64) != Some(self.next_turn)
                {
                    return Err(format_error("turn/start does not open the expected turn"));
                }
                self.turn = Some(self.next_turn);
                self.next_step = 1;
                self.tools.clear();
            }
            "turn/end" => {
                if self.turn != data.get("turn").and_then(Value::as_u64)
                    || self.turn.is_none()
                    || self.step.is_some()
                {
                    return Err(format_error(
                        "turn/end does not match the open turn with no open step",
                    ));
                }
                self.close_tools(kind)?;
                self.turn = None;
                self.next_turn += 1;
            }
            "step/start" => {
                if self.turn.is_none()
                    || self.turn != data.get("turn").and_then(Value::as_u64)
                    || self.step.is_some()
                    || data.get("step").and_then(Value::as_u64) != Some(self.next_step)
                {
                    return Err(format_error(
                        "step/start does not match the open turn and next step",
                    ));
                }
                self.step = Some(self.next_step);
            }
            "step/end" => {
                self.require_step(kind, data)?;
                self.close_tools(kind)?;
                self.step = None;
                self.next_step += 1;
            }
            "assistant/message" | "tool/call" | "tool/result" => {
                self.tool(kind, seq, row, data)?;
            }
            "developer/message" => self.developer(seq, data)?,
            "request/header" => {
                self.require_turn(kind)?;
                let config = data
                    .get("header")
                    .and_then(|header| header.get("config"))
                    .cloned()
                    .unwrap_or(Value::Null);
                self.provider = config.get("provider").cloned().unwrap_or(Value::Null);
            }
            "request/context" => self.require_turn(kind)?,
            "tool/ptc-dispatch-start" | "tool/ptc-dispatch" => self.dispatch(kind, row, data)?,
            "llm/retry" | "llm/retry-started" => self.retry(kind, data)?,
            "session/title" | "session/title-llm-request" => {
                title_sources(self.events, kind, seq, row, data, self.known)?;
            }
            "command/run" => {
                let id = text_field(data, "commandId", "commandId")?;
                if !self.commands.insert(id.to_owned()) {
                    return Err(format_error(format!("command/run repeats commandId {id}")));
                }
            }
            "command/done" => self.command_done(seq, data)?,
            "compaction/start" | "compaction/summary" | "compaction/end" | "compaction/prune" => {
                self.compact(kind, seq, row, data)?;
            }
            "user/message" => self.check_user_message_source(row, data)?,
            "session/end-seed" => self.compaction = None,
            _ => {}
        }
        Ok(())
    }

    /// `command/done` must cite a prior `command/run` and, when it carries
    /// `sourceEventSeq`, point at a successful run's event (dsh
    /// relationships `command/done` arm).
    fn command_done(
        &self,
        seq: u64,
        data: &serde_json::Map<String, Value>,
    ) -> Result<(), V4CheckError> {
        let id = text_field(data, "commandId", "commandId")?;
        if !self.commands.contains(id) {
            return Err(format_error("command/done has no prior command/run"));
        }
        if let Some(source_seq) = data.get("sourceEventSeq") {
            let index = usize::try_from(earlier(source_seq, seq, "command sourceEventSeq")?).ok();
            let source = index
                .and_then(|index| self.events.get(index))
                .cloned()
                .unwrap_or(Value::Null);
            let source_type = source
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if data.get("kind").and_then(Value::as_str) != Some("success")
                || source_type == "command/run"
                || source_type == "command/done"
            {
                return Err(format_error("command/done has invalid sourceEventSeq"));
            }
        }
        Ok(())
    }

    /// A non-append `user/message` citing a `compact-checkpoint` source must
    /// match the open compaction (dsh relationships `user/message` arm).
    fn check_user_message_source(
        &self,
        row: &Value,
        data: &serde_json::Map<String, Value>,
    ) -> Result<(), V4CheckError> {
        let source = data.get("source").cloned().unwrap_or(Value::Null);
        let appended = row.get("surfaceOp").and_then(Value::as_str) == Some("append");
        if !appended && source.get("kind").and_then(Value::as_str) == Some("compact-checkpoint") {
            let current = self.compaction.as_ref().ok_or_else(|| {
                format_error("compaction checkpoint has no matching compaction/start")
            })?;
            if current.id
                != source
                    .get("compactionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                || current.command
                    != source
                        .get("sourceCommandId")
                        .cloned()
                        .unwrap_or(Value::Null)
            {
                return Err(format_error(
                    "compaction checkpoint has no matching compaction/start",
                ));
            }
        }
        Ok(())
    }
}

fn text_field<'a>(
    data: &'a serde_json::Map<String, Value>,
    key: &str,
    subject: &str,
) -> Result<&'a str, V4CheckError> {
    data.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format_error(format!("{subject} requires a nonempty string")))
}

/// Whether one tool/result row is the exact not-started repair (dsh
/// `notStartedRepair`); fork-specific identity is checked by
/// [`assert_v4_fork_result`] before relationships run.
fn not_started_repair(
    row: &Value,
    data: &serde_json::Map<String, Value>,
    message: &Value,
    call_id: &str,
) -> bool {
    let Some(error) = data.get("error") else {
        return false;
    };
    if error.get("name").and_then(Value::as_str) != Some("ToolNotStartedError")
        || error.get("code").and_then(Value::as_str) != Some(crate::closer_text::TOOL_NOT_STARTED)
        || message.get("isError").and_then(Value::as_bool) != Some(true)
        || row.get("sourceEventSeqs").is_some()
    {
        return false;
    }
    let Some(id) = message.get("id").and_then(Value::as_str) else {
        return false;
    };
    // Fork-specific fields and identity are checked by assert_v4_fork_result
    // before relationships.
    if id.starts_with(&format!("forked-tool-result-{call_id}-")) {
        return true;
    }
    let prefix = format!("interrupted-tool-result-{call_id}-");
    let Some(suffix) = id.strip_prefix(&prefix) else {
        return false;
    };
    let content = message.get("content").cloned().unwrap_or(Value::Null);
    let Some(blocks) = content.as_array() else {
        return false;
    };
    let block = blocks.first().cloned().unwrap_or(Value::Null);
    let numeric = !suffix.is_empty()
        && !suffix.starts_with('0')
        && suffix.bytes().all(|byte| byte.is_ascii_digit());
    numeric
        && blocks.len() == 1
        && block.get("type").and_then(Value::as_str) == Some("text")
        && block.get("text").and_then(Value::as_str) == Some(INTERRUPTED_NOT_STARTED)
}

fn title_sources(
    events: &[Value],
    kind: &str,
    seq: u64,
    _row: &Value,
    data: &serde_json::Map<String, Value>,
    known: &EventVocabulary,
) -> Result<(), V4CheckError> {
    let references = data.get("messageSeqs").cloned().unwrap_or(Value::Null);
    let Some(references) = references.as_array() else {
        return Err(format_error("title messageSeqs requires an array"));
    };
    if kind == "session/title" {
        let source = data.get("source").cloned().unwrap_or(Value::Null);
        let is_user = source
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|name| name == "user");
        if references.is_empty() != is_user {
            return Err(format_error(
                "session/title messageSeqs must be empty exactly for a user title",
            ));
        }
    }
    let mut seen = std::collections::HashSet::new();
    for value in references {
        let cited = earlier(value, seq, "title messageSeqs")?;
        let source = usize::try_from(cited)
            .ok()
            .and_then(|index| events.get(index))
            .cloned()
            .unwrap_or(Value::Null);
        let source_kind = source
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let human_user = source
            .get("data")
            .and_then(|data| data.get("source"))
            .and_then(|source| source.get("kind"))
            .and_then(Value::as_str)
            .is_some_and(|name| name == "user");
        if source_kind != "user/message"
            || !known.knows(source_kind)
            || !seen.insert(cited)
            || !human_user
        {
            return Err(format_error(
                "session/title messageSeqs must cite distinct earlier human user/message events",
            ));
        }
    }
    if kind != "session/title-llm-request" {
        return Ok(());
    }
    let messages = data.get("messages").cloned().unwrap_or(Value::Null);
    let Some(messages) = messages.as_array() else {
        return Err(format_error("title messages requires an array"));
    };
    let Some(message) = messages.first() else {
        return Err(format_error("title message requires an object"));
    };
    let content = message.get("content").cloned().unwrap_or(Value::Null);
    let Some(content) = content.as_array() else {
        return Err(format_error("title content requires an array"));
    };
    let block = content.first().cloned().unwrap_or(Value::Null);
    let source_kind = message
        .get("source")
        .and_then(|source| source.get("kind"))
        .and_then(Value::as_str);
    let ok = !references.is_empty()
        && messages.len() == 1
        && message.get("role").and_then(Value::as_str) == Some("user")
        && source_kind == Some("dsh-session-title-llm")
        && content.len() == 1
        && block.get("type").and_then(Value::as_str) == Some("text");
    if !ok {
        return Err(format_error(
            "session/title-llm-request messages do not represent messageSeqs",
        ));
    }
    Ok(())
}

fn earlier(value: &Value, seq: u64, subject: &str) -> Result<u64, V4CheckError> {
    let coordinate = count(value, subject)?;
    if coordinate >= seq {
        return Err(format_error(format!(
            "{subject} must name an earlier event"
        )));
    }
    Ok(coordinate)
}

/// Validates native v4 lifecycle and ownership facts without rewriting any
/// event (dsh `assertV4LifecycleRelationships`).
///
/// # Errors
///
/// Returns the first lifecycle refusal, message-verbatim.
pub fn assert_v4_lifecycle_relationships(
    events: &[Value],
    known: &EventVocabulary,
) -> Result<(), V4CheckError> {
    let mut state = Relationships::new(events, known);
    for event in events {
        state.accept(event)?;
    }
    Ok(())
}

/// Validates delivery generation and active-generation coordinates before
/// evaluating ownership (dsh `validateDeliveryAccepted`, verbatim port per
/// PRD decision 29 / Q40B).
///
/// Returns the active delivery's nonempty session id, or `None` for other
/// events and generations.
///
/// # Errors
///
/// Returns the dsh refusal for malformed delivery payload shapes.
pub fn validate_delivery_accepted(
    row: &Value,
    current_version: u64,
) -> Result<Option<String>, V4CheckError> {
    if row.get("type").and_then(Value::as_str) != Some("session-log-deepseek/delivery-accepted") {
        return Ok(None);
    }
    let data = row
        .get("data")
        .cloned()
        .ok_or_else(|| format_error("delivery-accepted data must be an object"))?;
    if !is_object(&data) {
        return Err(format_error("delivery-accepted data must be an object"));
    }
    let raw_version = data.get("sessionFormatVersion").cloned();
    let version = match raw_version {
        Some(value) => count(&value, "delivery sessionFormatVersion")?,
        None => 0,
    };
    if version != current_version {
        return Ok(None);
    }
    let through_seq = count(
        data.get("throughSeq").unwrap_or(&Value::Null),
        "delivery throughSeq",
    )?;
    let seq = count(
        row.get("seq").unwrap_or(&Value::Null),
        "delivery throughSeq",
    )?;
    if through_seq >= seq {
        return Err(format_error("delivery throughSeq must precede its marker"));
    }
    let id = data.get("sessionId").cloned().unwrap_or(Value::Null);
    let id = match id.as_str() {
        Some(id) if !id.is_empty() => id.to_owned(),
        _ => {
            return Err(format_error("delivery requires a nonempty Session id"));
        }
    };
    Ok(Some(id))
}

/// Validates inheritance, vocabulary, native message admission, and the
/// generation-owned relationships of one detached v4 artifact
///
/// (dsh `restoreReleasedV4Artifact`): the composed restore gate the scanner
/// runs at finish.
///
/// # Errors
///
/// Returns the dsh refusal; [`V4CheckError::UnsupportedMigration`] marks
/// the vocabulary refusal the caller renders as format-unsupported.
pub fn restore_released_v4_artifact(
    header: &Value,
    inherited_event_count: u64,
    events: &[Value],
    known: &EventVocabulary,
) -> Result<(), V4CheckError> {
    assert_released_v4_header(header)?;
    if inherited_event_count > events.len() as u64 {
        return Err(format_error(
            "format v4 inherited event count exceeds its events",
        ));
    }
    let is_seeded = header.get("isSeeded").and_then(Value::as_bool);
    let is_seeded =
        is_seeded.ok_or_else(|| format_error("format v4 header isSeeded must be boolean"))?;
    if !is_seeded && inherited_event_count != 0 {
        return Err(format_error(
            "unseeded format v4 Session has inherited events",
        ));
    }
    let mut last_inherited_marker: Option<u64> = None;
    for (index, event) in events.iter().enumerate() {
        let index = index as u64;
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !known.knows(kind) && event.get("ignorable").and_then(Value::as_bool) != Some(true) {
            return Err(V4CheckError::UnsupportedMigration(format!(
                "format v4 contains unknown event type {} at seq {index}",
                serde_json::to_string(kind).unwrap_or_default()
            )));
        }
        if event.get("seq").and_then(Value::as_u64) != Some(index) {
            return Err(format_error(format!(
                "format v4 event {index} is not dense"
            )));
        }
        if !known.knows(kind) {
            continue;
        }
        assert_v4_retired_syntax(event)?;
        assert_v4_system_message_fields(event)?;
        assert_v4_tool_result_message(event)?;
        assert_v4_fork_result(event)?;
        if kind == "session/end-seed"
            && event
                .get("data")
                .and_then(|data| data.get("inherited"))
                .and_then(Value::as_bool)
                == Some(true)
        {
            last_inherited_marker = Some(index);
        }
    }
    if is_seeded && last_inherited_marker != Some(inherited_event_count) {
        return Err(format_error(
            "format v4 seeded header disagrees with its last inherited end-seed marker",
        ));
    }
    if !is_seeded && last_inherited_marker.is_some() {
        return Err(format_error(
            "format v4 unseeded Session contains an inherited end-seed marker",
        ));
    }
    // assertReleasedV4Relationships prologue: developer data, full message
    // sources, delivery ownership, and the child-catalog dedup.
    let header_id = header.get("id").and_then(Value::as_str).unwrap_or_default();
    let parent_session = header.get("parentSession").and_then(Value::as_str);
    let mut catalog_children = std::collections::HashSet::new();
    for (index, event) in events.iter().enumerate() {
        let index = index as u64;
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !known.knows(kind) {
            continue;
        }
        assert_v4_developer_data(event)?;
        assert_v4_message_sources(event)?;
        let delivery_id =
            validate_delivery_accepted(event, crate::persist::SESSION_FORMAT_VERSION)?;
        if let Some(delivery_id) = delivery_id {
            let inherited_marker = parent_session.is_some() && index < inherited_event_count;
            if !inherited_marker && delivery_id != header_id {
                return Err(format_error(
                    "current-generation delivery marker names the wrong Session",
                ));
            }
        }
        if kind == "subagent/catalog" && index >= inherited_event_count {
            let fact = catalog_fact(
                event.get("data").cloned().unwrap_or(Value::Null),
                "subagent/catalog",
            )?;
            let child_id = fact
                .get("childId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if !catalog_children.insert(child_id.clone()) {
                return Err(format_error(format!("duplicate catalog child {child_id}")));
            }
        }
    }
    assert_v4_lifecycle_relationships(events, known)
}

/// Validates the historical fields used for catalog membership without
/// interpreting extensions (dsh `catalogFact`, facts.ts:67-86).
///
/// # Errors
///
/// Returns the dsh refusal for an unsupported catalog fact.
fn catalog_fact(value: Value, subject: &str) -> Result<Value, V4CheckError> {
    let refuse = || {
        Err(format_error(format!(
            "{subject} requires a supported versioned catalog fact"
        )))
    };
    let Some(object) = value.as_object() else {
        return refuse();
    };
    let version = object.get("version").and_then(Value::as_u64);
    let mode = object.get("mode").and_then(Value::as_str);
    let valid = matches!(
        (version, mode),
        (Some(0), Some("continuable" | "one-shot"))
            | (Some(1), Some("continuable" | "one-shot" | "unknown"))
            | (Some(2), Some("external"))
    );
    if !valid || object.get("childId").and_then(Value::as_str).is_none() {
        return refuse();
    }
    // `mode: continuable` requires a string label, and an optional label
    // must be a string whenever present.
    if mode == Some("continuable") && object.get("label").and_then(Value::as_str).is_none() {
        return refuse();
    }
    if let Some(label) = object.get("label")
        && label.as_str().is_none()
    {
        return refuse();
    }
    count(
        object.get("childCreatedAt").unwrap_or(&Value::Null),
        "catalog child creation time",
    )?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        assert_released_v4_header, assert_v4_developer_data, assert_v4_fork_result,
        assert_v4_retired_syntax, assert_v4_row_admission, assert_v4_system_message_fields,
        assert_v4_tool_result_message, restore_released_v4_artifact,
    };
    use crate::persist::v4_checks::V4CheckError;
    use crate::vocabulary::EventVocabulary;
    use fr_wire::event::SessionEvent;

    fn known() -> EventVocabulary {
        EventVocabulary::core()
    }

    fn event_row(seq: u64, kind: &str, data: &Value) -> Value {
        json!({"type": kind, "seq": seq, "time": 1_000, "data": data})
    }

    fn event_row_op(seq: u64, kind: &str, data: &Value, surface_op: &str) -> Value {
        json!({
            "type": kind, "seq": seq, "time": 1_000, "data": data,
            "surfaceOp": surface_op
        })
    }

    fn event_row_ignorable(seq: u64, kind: &str, data: &Value) -> Value {
        json!({
            "type": kind, "seq": seq, "time": 1_000, "data": data,
            "ignorable": true
        })
    }

    fn envelope(seq: u64, kind: &str, data: &Value) -> SessionEvent {
        serde_json::from_value(event_row(seq, kind, data)).expect("envelope")
    }

    fn header(seeded: bool, _cut: u64, events: usize) -> Value {
        let _ = events;
        json!({
            "version": 4, "id": "s-1", "createdAt": 1_000,
            "cwd": "/repo", "isSeeded": seeded, "delegationDepth": 0
        })
    }

    fn turn_start(seq: u64, turn: u64) -> Value {
        event_row(seq, "turn/start", &json!({"turn": turn}))
    }

    fn turn_end(seq: u64, turn: u64) -> Value {
        event_row(
            seq,
            "turn/end",
            &json!({"turn": turn, "reason": {"kind": "completed"}}),
        )
    }

    fn step_start(seq: u64, turn: u64, step: u64) -> Value {
        event_row(seq, "step/start", &json!({"turn": turn, "step": step}))
    }

    fn step_end(seq: u64, turn: u64, step: u64) -> Value {
        event_row(seq, "step/end", &json!({"turn": turn, "step": step}))
    }

    #[test]
    fn v4_header_guard_pins_the_exact_surface() {
        let base = json!({
            "version": 4, "id": "s", "createdAt": 1000, "isSeeded": false, "delegationDepth": 0
        });
        assert_released_v4_header(&base).expect("base");
        assert_released_v4_header(&json!({"version": 4})).expect_err("missing fields");
        let with = |key: &str, value: Value| {
            let mut object = base.as_object().expect("object").clone();
            object.insert(key.to_owned(), value);
            Value::Object(object)
        };
        let error = assert_released_v4_header(&with("bogus", json!(1))).expect_err("unexpected");
        assert!(matches!(error, V4CheckError::Format(_)));
        assert!(error.message().contains("has unexpected field bogus"));
        let error =
            assert_released_v4_header(&with("cwd", json!("rel"))).expect_err("relative cwd");
        assert!(error.message().contains("cwd must be absolute"));
        let error = assert_released_v4_header(&with("origin", json!("main"))).expect_err("origin");
        assert!(error.message().contains("origin must be"));
        assert_released_v4_header(&with("origin", json!("subagent"))).expect("subagent");
    }

    #[test]
    fn retired_syntax_refuses_with_dsh_wording() {
        let dispatch = json!({"type": "tool/code-dispatch", "seq": 0, "time": 0, "data": {}});
        let error = assert_v4_retired_syntax(&dispatch).expect_err("retired type");
        assert!(matches!(error, V4CheckError::UnsupportedMigration(_)));
        assert!(
            error
                .message()
                .contains("format v4 rejects retired event type")
        );

        let header_system = event_row(
            0,
            "request/header",
            &json!({"header": {"system": "s"}, "reason": "initial"}),
        );
        let error = assert_v4_retired_syntax(&header_system).expect_err("header.system");
        assert!(
            error
                .message()
                .contains("format v4 request/header rejects retired header.system")
        );

        let wrapper = event_row(
            0,
            "user/message",
            &json!({"content": [{"type": "tool-result", "toolCallId": "c"}]}),
        );
        let error = assert_v4_retired_syntax(&wrapper).expect_err("wrapper");
        assert!(
            error
                .message()
                .contains("must not contain a released tool-result wrapper")
        );
    }

    #[test]
    fn system_message_fields_and_tool_result_messages_pin() {
        let system = event_row(
            0,
            "system/message",
            &json!({"turn": 0, "step": 1, "message": {"id": "m", "role": "system", "content": []}}),
        );
        let error = assert_v4_system_message_fields(&system).expect_err("turn positive");
        assert!(
            error
                .message()
                .contains("system/message turn must be positive")
        );

        let system = event_row(
            1,
            "system/message",
            &json!({"turn": 1, "step": 1, "message": {"id": "m", "role": "assistant", "content": []}}),
        );
        let error = assert_v4_system_message_fields(&system).expect_err("role");
        assert!(error.message().contains("requires system role"));

        let result = event_row(
            2,
            "tool/result",
            &json!({
                "turn": 1, "step": 1,
                "message": {"id": "r", "role": "tool", "toolCallId": "c",
                             "source": {"kind": "tool", "callId": "other"}, "content": []}
            }),
        );
        let error = assert_v4_tool_result_message(&result).expect_err("callId mismatch");
        assert!(
            error
                .message()
                .contains("toolCallId matching its tool source")
        );

        let carrying_error = event_row(
            2,
            "tool/result",
            &json!({
                "turn": 1, "step": 1, "error": {"name": "E", "code": "C"},
                "message": {"id": "r", "role": "tool", "toolCallId": "c",
                             "source": {"kind": "tool", "callId": "c"}, "content": []}
            }),
        );
        let error = assert_v4_tool_result_message(&carrying_error).expect_err("error flag");
        assert!(
            error
                .message()
                .contains("carries error metadata for a non-error tool result")
        );
    }

    #[test]
    fn fork_result_identity_pins_exact_shape() {
        let valid = event_row_op(
            4,
            "tool/result",
            &json!({
                "turn": 1, "step": 1,
                "error": {"name": "ToolNotStartedError", "code": "TOOL_NOT_STARTED"},
                "message": {"id": "forked-tool-result-call-4", "role": "tool",
                             "toolCallId": "call", "isError": true,
                             "source": {"kind": "tool", "callId": "call"},
                             "content": [{"type": "text", "text": "x"}]}
            }),
            "append",
        );
        assert_v4_fork_result(&valid).expect("valid fork result");
        let mismatched_seq = event_row_op(
            5,
            "tool/result",
            &json!({
                "turn": 1, "step": 1,
                "error": {"name": "ToolNotStartedError", "code": "TOOL_NOT_STARTED"},
                "message": {"id": "forked-tool-result-call-4", "role": "tool",
                             "toolCallId": "call", "isError": true,
                             "source": {"kind": "tool", "callId": "call"},
                             "content": [{"type": "text", "text": "x"}]}
            }),
            "append",
        );
        let error = assert_v4_fork_result(&mismatched_seq).expect_err("seq mismatch");
        assert_eq!(error.message(), "invalid V4 not-started fork result");
        let _ = &valid;
    }

    #[test]
    fn developer_data_pins_tool_change_and_header_seq_rules() {
        let addition = event_row(
            3,
            "developer/message",
            &json!({
                "turn": 1, "step": 1, "headerSeq": 2,
                "message": {"id": "d", "role": "developer", "source": {"kind": "agent"},
                             "content": [{"type": "tool-addition", "toolName": "bash"}]}
            }),
        );
        assert_v4_developer_data(&addition).expect("addition with headerSeq");

        let missing_header = event_row(
            3,
            "developer/message",
            &json!({
                "turn": 1, "step": 1,
                "message": {"id": "d", "role": "developer", "source": {"kind": "agent"},
                             "content": [{"type": "tool-addition", "toolName": "bash"}]}
            }),
        );
        let error = assert_v4_developer_data(&missing_header).expect_err("headerSeq required");
        assert!(error.message().contains("developer/message headerSeq"));

        let inline_tool = event_row(
            3,
            "developer/message",
            &json!({
                "turn": 1, "step": 1, "headerSeq": 2,
                "message": {"id": "d", "role": "developer", "source": {"kind": "agent"},
                             "content": [{"type": "tool-addition", "toolName": "bash", "tool": {}}]}
            }),
        );
        let error = assert_v4_developer_data(&inline_tool).expect_err("inline tool");
        assert!(
            error
                .message()
                .contains("must omit inline tool definitions")
        );

        let assistant_addition = event_row(
            3,
            "assistant/message",
            &json!({
                "turn": 1, "step": 1,
                "message": {"id": "a", "role": "assistant", "source": {"kind": "model", "provider": "p", "model": "m"},
                             "content": [{"type": "tool-addition", "toolName": "bash"}]},
                "stream": []
            }),
        );
        let error = assert_v4_developer_data(&assistant_addition).expect_err("developer role");
        assert!(
            error
                .message()
                .contains("tool-addition requires developer role")
        );
    }

    #[test]
    fn row_admission_defers_ignorable_unknown_developer_rows() {
        let deferred = event_row_ignorable(
            0,
            "developer/message",
            &json!({"turn": 1, "step": 1, "message": {"role": "developer"}}),
        );
        // Core vocabulary knows developer/message: admission applies fully
        // and the malformed slot refuses.
        assert!(assert_v4_row_admission(&deferred, &known()).is_err());
        // Without the declaration, an ignorable developer row defers.
        let mut stripped = EventVocabulary::core();
        stripped.declare(&[]).expect("noop");
        let reduced = {
            let mut reduced = EventVocabulary::core();
            let unknown = EventVocabulary::default();
            let _ = (unknown, &mut reduced);
            EventVocabulary::default()
        };
        assert_v4_row_admission(&deferred, &reduced)
            .expect("ignorable unknown developer row defers");
        let plugin_row = json!({
            "type": "plugin:sample/note", "seq": 0, "time": 1_000, "ignorable": true, "data": {}
        });
        assert_v4_row_admission(&plugin_row, &reduced)
            .expect("ignorable foreign plugin row defers");
    }

    #[test]
    fn restore_gate_refuses_unknown_required_and_enforces_the_cut() {
        // dsh keeps no tail-balance check here: an open tail turn is a
        // legitimate crash-recovery prefix that resume closes.
        let open_tail = vec![turn_start(0, 1)];
        restore_released_v4_artifact(&header(false, 0, 1), 0, &open_tail, &known())
            .expect("open tail stays open");
        let unknown = vec![json!({
            "type": "compaction/start", "seq": 0, "time": 1_000,
            "data": {"turn": null, "compactionId": "c"}
        })];
        let error = restore_released_v4_artifact(&header(false, 0, 1), 0, &unknown, &known())
            .expect_err("unknown required type");
        assert!(matches!(error, V4CheckError::UnsupportedMigration(_)));
        assert!(
            error
                .message()
                .contains("format v4 contains unknown event type \"compaction/start\" at seq 0")
        );
        let ignorable_unknown = vec![json!({
            "type": "compaction/start", "seq": 0, "time": 1_000,
            "ignorable": true, "data": {"turn": null, "compactionId": "c"}
        })];
        restore_released_v4_artifact(&header(false, 0, 1), 0, &ignorable_unknown, &known())
            .expect("ignorable unknown retained");

        let seeded_cut = vec![
            turn_start(0, 1),
            turn_end(1, 1),
            json!({"type": "session/end-seed", "seq": 2, "time": 1_000, "data": {"inherited": true}}),
        ];
        // The cut arrives independently of the marker scan (migration and
        // artifact surfaces in dsh); a mismatch fires the disagreement.
        restore_released_v4_artifact(&header(true, 2, 3), 2, &seeded_cut, &known())
            .expect("cut aligned with the marker");
        let error = restore_released_v4_artifact(&header(true, 2, 3), 1, &seeded_cut, &known())
            .expect_err("cut disagrees with marker");
        assert!(
            error
                .message()
                .contains("disagrees with its last inherited end-seed marker")
        );
        let error = restore_released_v4_artifact(&header(true, 4, 3), 4, &seeded_cut, &known())
            .expect_err("cut beyond the events");
        assert!(
            error
                .message()
                .contains("format v4 inherited event count exceeds its events")
        );

        let unseeded_marker = vec![json!({
            "type": "session/end-seed", "seq": 0, "time": 1_000, "data": {"inherited": true}
        })];
        let error =
            restore_released_v4_artifact(&header(false, 0, 1), 0, &unseeded_marker, &known())
                .expect_err("unseeded marker");
        assert!(
            error
                .message()
                .contains("format v4 unseeded Session contains an inherited end-seed marker"),
            "actual: {}",
            error.message()
        );
    }

    #[test]
    fn lifecycle_happy_chain_and_tool_matching_accept() {
        let envelope_events = |rows: Vec<Value>| -> Vec<Value> { rows };
        let advertised = |seq: u64, call_id: &str| {
            event_row_op(
                seq,
                "assistant/message",
                &json!({
                    "turn": 1, "step": 1,
                    "message": {"id": format!("a-{seq}"), "role": "assistant",
                                 "source": {"kind": "model", "provider": "p", "model": "m"},
                                 "content": [{"type": "tool-call", "id": call_id, "name": "bash", "arguments": "{}"}]},
                    "stream": []
                }),
                "append",
            )
        };
        let call = |seq: u64, call_id: &str| {
            event_row(
                seq,
                "tool/call",
                &json!({"turn": 1, "step": 1, "callId": call_id, "name": "bash", "arguments": "{}"}),
            )
        };
        let result = |seq: u64, call_id: &str| {
            event_row_op(
                seq,
                "tool/result",
                &json!({
                    "turn": 1, "step": 1,
                    "message": {"id": format!("r-{seq}"), "role": "tool", "toolCallId": call_id,
                                 "source": {"kind": "tool", "callId": call_id},
                                 "content": [{"type": "text", "text": "ok"}]}
                }),
                "append",
            )
        };
        let chain = envelope_events(vec![
            turn_start(0, 1),
            step_start(1, 1, 1),
            advertised(2, "call-a"),
            call(3, "call-a"),
            result(4, "call-a"),
            step_end(5, 1, 1),
            turn_end(6, 1),
        ]);
        let header_value = header(false, 0, 7);
        restore_released_v4_artifact(&header_value, 0, &chain, &known()).expect("full chain");

        // An unmatched tool/call refuses.
        let dangling = envelope_events(vec![
            turn_start(0, 1),
            step_start(1, 1, 1),
            call(2, "ghost"),
        ]);
        let error = restore_released_v4_artifact(&header(false, 0, 3), 0, &dangling, &known())
            .expect_err("unadvertised call");
        assert!(
            error
                .message()
                .contains("tool/call ghost has no advertised tool lifecycle")
        );

        // A non-not-started result for a never-started call refuses.
        let early_result = envelope_events(vec![
            turn_start(0, 1),
            step_start(1, 1, 1),
            advertised(2, "call-a"),
            result(3, "call-a"),
        ]);
        let error = restore_released_v4_artifact(&header(false, 0, 4), 0, &early_result, &known())
            .expect_err("unstarted result");
        assert!(
            error
                .message()
                .contains("is not the exact TOOL_NOT_STARTED repair")
        );

        // The exact interrupted not-started repair (verbatim text) passes.
        let interrupted = event_row_op(
            3,
            "tool/result",
            &json!({
                "turn": 1, "step": 1,
                "error": {"name": "ToolNotStartedError", "code": "TOOL_NOT_STARTED"},
                "message": {"id": "interrupted-tool-result-call-a-3", "role": "tool",
                             "toolCallId": "call-a", "isError": true,
                             "source": {"kind": "tool", "callId": "call-a"},
                             "content": [{"type": "text", "text": crate::closer_text::INTERRUPTED_NOT_STARTED}]}
            }),
            "append",
        );
        let repaired = envelope_events(vec![
            turn_start(0, 1),
            step_start(1, 1, 1),
            advertised(2, "call-a"),
            interrupted,
        ]);
        restore_released_v4_artifact(&header(false, 0, 4), 0, &repaired, &known())
            .expect("exact repair accepted");

        // Typed envelopes of the chain re-admit (fixture soundness check).
        let _ = envelope(0, "turn/start", &json!({"turn": 1}));
    }

    #[test]
    fn delivery_ownership_pins_session_binding_and_payload() {
        // Q40B (PRD decision 29): the delivery check is a verbatim port and
        // activates once the owning plugin declares the type.
        let mut delivery_known = known();
        delivery_known
            .declare(&["session-log-deepseek/delivery-accepted".to_owned()])
            .expect("declare delivery type");
        let delivery_row = |seq: u64, session_id: &str, through: u64| {
            json!({
                "type": "session-log-deepseek/delivery-accepted", "seq": seq, "time": 1_000,
                "data": {"sessionId": session_id, "throughSeq": through, "sessionFormatVersion": 4}
            })
        };

        // A current-generation marker naming its own session accepts.
        let own = vec![turn_start(0, 1), delivery_row(1, "s-1", 0)];
        restore_released_v4_artifact(&header(false, 0, 2), 0, &own, &delivery_known)
            .expect("own marker");

        // A current-generation marker naming a foreign session refuses.
        let foreign = vec![turn_start(0, 1), delivery_row(1, "other", 0)];
        let error =
            restore_released_v4_artifact(&header(false, 0, 2), 0, &foreign, &delivery_known)
                .expect_err("foreign marker");
        assert!(
            error
                .message()
                .contains("current-generation delivery marker names the wrong Session")
        );

        // throughSeq at its own seq refuses.
        let backwards = vec![turn_start(0, 1), delivery_row(1, "s-1", 1)];
        let error =
            restore_released_v4_artifact(&header(false, 0, 2), 0, &backwards, &delivery_known)
                .expect_err("throughSeq not earlier");
        assert!(
            error
                .message()
                .contains("delivery throughSeq must precede its marker")
        );

        // An empty session id refuses.
        let empty = vec![turn_start(0, 1), delivery_row(1, "", 0)];
        let error = restore_released_v4_artifact(&header(false, 0, 2), 0, &empty, &delivery_known)
            .expect_err("empty id");
        assert!(
            error
                .message()
                .contains("delivery requires a nonempty Session id")
        );

        // A foreign delivery generation is ignored (sessionFormatVersion
        // defaults to 0 and never matches v4).
        let foreign_generation = vec![
            turn_start(0, 1),
            json!({
                "type": "session-log-deepseek/delivery-accepted", "seq": 1, "time": 1_000,
                "data": {"sessionId": "other", "throughSeq": 0, "sessionFormatVersion": 3}
            }),
        ];
        restore_released_v4_artifact(
            &header(false, 0, 2),
            0,
            &foreign_generation,
            &delivery_known,
        )
        .expect("foreign generation ignored");

        // Without the plugin declaration the type is unknown-and-required:
        // the vocabulary gate refuses before the delivery check applies.
        let undeclared = vec![turn_start(0, 1), delivery_row(1, "other", 0)];
        let error = restore_released_v4_artifact(&header(false, 0, 2), 0, &undeclared, &known())
            .expect_err("undeclared type");
        assert!(
            error
                .message()
                .contains("format v4 contains unknown event type")
        );
    }

    #[test]
    fn lifecycle_pins_step_turn_protected_head_and_dispatch() {
        // A step/end outside a turn refuses.
        let stray = vec![step_end(0, 1, 1)];
        let error = restore_released_v4_artifact(&header(false, 0, 1), 0, &stray, &known())
            .expect_err("stray step/end");
        assert!(
            error
                .message()
                .contains("does not match an open turn and step")
        );

        // A replacement whose range is not on the current surface refuses.
        let system_message = |seq: u64, id: &str, surface_op: Value| {
            let mut row = json!({
                "type": "system/message", "seq": seq, "time": 1_000,
                "data": {"turn": 1, "step": 1,
                          "message": {"id": id, "role": "system", "source": {"kind": "system-prompt"},
                                       "content": [{"type": "text", "text": "prompt"}]}}
            });
            if !surface_op.is_null() {
                row["surfaceOp"] = surface_op;
            }
            row
        };
        let off_surface = vec![
            turn_start(0, 1),
            step_start(1, 1, 1),
            system_message(2, "s0", json!("append")),
            system_message(
                3,
                "s1",
                json!({"op": "replace", "startSeq": 0, "endSeq": 0}),
            ),
        ];
        let error = restore_released_v4_artifact(&header(false, 0, 4), 0, &off_surface, &known())
            .expect_err("range off surface");
        assert!(
            error
                .message()
                .contains("system/message replacement range is not on the current surface")
        );

        // PTC dispatch pairing: settle without start refuses. The ptc types
        // are plugin-owned durable events, so the pin declares them exactly
        // as the fr-ptc plugin will at activation time (PRD 7.1 Q34A).
        let mut ptc_known = known();
        ptc_known
            .declare(&[
                "tool/ptc-dispatch".to_owned(),
                "tool/ptc-dispatch-start".to_owned(),
            ])
            .expect("declare ptc types");
        let ptc_settle = event_row(
            1,
            "tool/ptc-dispatch",
            &json!({"turn": 1, "subCallId": "sub", "rootCallId": "root",
                    "parentCallId": "root", "name": "n", "arguments": "{}"}),
        );
        let without_start = vec![turn_start(0, 1), ptc_settle];
        let error =
            restore_released_v4_artifact(&header(false, 0, 2), 0, &without_start, &ptc_known)
                .expect_err("settle without start");
        assert!(error.message().contains("PTC dispatch has no unique start"));
    }
}
