// Rust guideline compliant 2026-10-30
//! The fs tool bodies (dsh `tool-fs` `read`/`write`/`edit` `execute`,
//! `render`, `presentationMeta`, `presentCall`, and `presentResult`).
//!
//! The bodies consume the in-crate fs domain, the in-crate observation
//! gate (the dsh fs-observation-policy waterfalls dispatched directly —
//! the Q101′A same-crate judgment), and the cross-plugin seam for the
//! standing policy, the approval channel, and the session working
//! directory. The read caps are the dsh defaults baked at build time
//! (the fr-shell `ExecutorConfigs::bundle()` judgment; the
//! `read-limit`/`read-max-line-length`/`read-max-bytes`/
//! `read-stream-min-size` config keys carry these values and the
//! `read`/`write`/`edit`/`read-image` switches default on, the M1
//! composition registering all three tools).

use serde_json::Value;
use serde_json::json;

use crate::catalog_gen;
use crate::fs_diff;
use crate::fs_domain::FsDomain;
use crate::fs_read_render;
use crate::fs_read_render::FileReadOutcome;
use crate::fs_read_render::FileTextLine;
use crate::fs_read_render::ReadWindow;
use crate::fs_sandbox;
use crate::fs_types::FsEditOutcome;
use crate::fs_types::FsEditRequest;
use crate::fs_types::FsError;
use crate::fs_types::FsKind;
use crate::fs_types::FsObservation;
use crate::fs_types::FsWriteOutcome;
use crate::fs_types::WriteOperation;
use crate::observation::ObservedStateGate;
use crate::service::METHOD_EDIT_PRESENT_CALL;
use crate::service::METHOD_EDIT_PRESENT_RESULT;
use crate::service::METHOD_EDIT_PRESENTATION_META;
use crate::service::METHOD_EDIT_RENDER;
use crate::service::METHOD_READ_PRESENT_CALL;
use crate::service::METHOD_READ_PRESENT_RESULT;
use crate::service::METHOD_READ_PRESENTATION_META;
use crate::service::METHOD_READ_RENDER;
use crate::service::METHOD_WRITE_PRESENT_CALL;
use crate::service::METHOD_WRITE_PRESENT_RESULT;
use crate::service::METHOD_WRITE_PRESENTATION_META;
use crate::service::METHOD_WRITE_RENDER;
use crate::tools::BodyError;
use crate::tools::Seams;

/// Default and maximum number of lines returned by one `read` call
/// (the dsh `READ_LIMIT` and the `read-limit` config).
pub const BAKED_READ_LIMIT: u64 = 2000;

/// Default streaming threshold (the dsh `STREAM_MIN_SIZE` and the
/// `read-stream-min-size` config): files at or above this size stream;
/// smaller files read whole into memory.
pub const BAKED_READ_STREAM_MIN_SIZE: u64 = 10 * 1024 * 1024;

/// The read-tool caps (dsh `ReadToolCaps`; the baked dsh defaults).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadCaps {
    /// Default and maximum number of lines returned by one call.
    pub limit: u64,
    /// Maximum characters returned for a single line.
    pub max_line_length: u64,
    /// Maximum bytes returned for selected file lines.
    pub max_bytes: u64,
    /// Files at or above this size stream; smaller files read whole.
    pub stream_min_size: u64,
}

/// The baked caps (the dsh defaults; the build-time bundle judgment).
#[must_use]
pub const fn baked_caps() -> ReadCaps {
    ReadCaps {
        limit: BAKED_READ_LIMIT,
        max_line_length: fs_read_render::READ_MAX_LINE_LENGTH as u64,
        max_bytes: fs_read_render::READ_MAX_BYTES as u64,
        stream_min_size: BAKED_READ_STREAM_MIN_SIZE,
    }
}

/// The composed cross-plugin state the fs tool bodies run through.
pub struct FsToolDeps<'a> {
    /// The fs domain (the single sandboxed local backend).
    pub fs: &'a FsDomain,
    /// The observed-state gate (the in-crate observation policy).
    pub gate: &'a ObservedStateGate,
    /// The cross-plugin seam (standing policy, approval, session cwd).
    pub seams: &'a dyn Seams,
    /// Whether the mounted backend confines (the escalation
    /// advertisement).
    pub advertised: bool,
}

impl std::fmt::Debug for FsToolDeps<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsToolDeps")
            .field("advertised", &self.advertised)
            .finish_non_exhaustive()
    }
}

/// The fs tool variant one serve method dispatches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolKind {
    Read,
    Write,
    Edit,
}

/// The word a string argument arrives as (absent keys stay empty).
fn word<'a>(arguments: &'a Value, key: &str) -> &'a str {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// Maps an fs-domain error onto the body failure (dsh `FsError`
/// extends `HarnessError`, so the code stays routable and the class
/// name rides as `FsError`).
fn fs_error(error: FsError) -> BodyError {
    BodyError {
        code: Some(error.code.as_str().to_owned()),
        name: "FsError".to_owned(),
        message: error.message,
    }
}

/// Validates one `number` argument as a positive integer (the dsh
/// `parsePositiveInteger`).
fn positive_integer(value: &Value, name: &str) -> Result<u64, String> {
    let number = value
        .as_f64()
        .ok_or_else(|| format!("{name} must be a positive integer"))?;
    if !number.is_finite() || number.fract() != 0.0 || number < 1.0 {
        return Err(format!("{name} must be a positive integer"));
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "validated finite, integral, and at least 1 before the conversion"
    )]
    Ok(number as u64)
}

/// The validated `read` arguments after defaulting (dsh `ReadInput`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadInput {
    /// The model-supplied path.
    pub file_path: String,
    /// 1-based first line, defaulted to 1.
    pub offset: u64,
    /// Line cap, defaulted to the deployment cap.
    pub limit: u64,
}

/// Validates value constraints the schema cannot express (dsh
/// `parseReadArgs`).
///
/// # Errors
///
/// The dsh refusal texts verbatim.
pub fn parse_read_args(arguments: &Value, max_limit: u64) -> Result<ReadInput, String> {
    let file_path = word(arguments, "file_path");
    if file_path.trim().is_empty() {
        return Err("file_path must be a non-empty string".to_owned());
    }
    let offset = match arguments.get("offset") {
        None => 1,
        Some(value) => positive_integer(value, "offset")?,
    };
    let limit = match arguments.get("limit") {
        None => max_limit,
        Some(value) => positive_integer(value, "limit")?,
    };
    if limit > max_limit {
        return Err(format!("limit must be less than or equal to {max_limit}"));
    }
    Ok(ReadInput {
        file_path: file_path.to_owned(),
        offset,
        limit,
    })
}

/// Validates the write arguments (dsh `parseWriteArgs`): only a
/// non-blank `file_path` — an empty `content` is legitimate.
///
/// # Errors
///
/// The dsh refusal text verbatim.
pub fn parse_write_args(arguments: &Value) -> Result<(String, String), String> {
    let file_path = word(arguments, "file_path");
    if file_path.trim().is_empty() {
        return Err("file_path must be a non-empty string".to_owned());
    }
    Ok((file_path.to_owned(), word(arguments, "content").to_owned()))
}

/// The validated `edit` arguments after defaulting (dsh `EditInput`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditInput {
    /// The model-supplied path.
    pub file_path: String,
    /// Literal text to replace.
    pub old_string: String,
    /// Literal replacement text.
    pub new_string: String,
    /// Replace every match instead of requiring exactly one.
    pub replace_all: bool,
}

/// Validates value constraints the schema cannot express (dsh
/// `parseEditArgs`): a non-blank path, a non-empty `old_string`, and an
/// unequal pair.
///
/// # Errors
///
/// The dsh refusal texts verbatim.
pub fn parse_edit_args(arguments: &Value) -> Result<EditInput, String> {
    let file_path = word(arguments, "file_path");
    if file_path.trim().is_empty() {
        return Err("file_path must be a non-empty string".to_owned());
    }
    let old_string = word(arguments, "old_string");
    if old_string.is_empty() {
        return Err("old_string must be a non-empty string".to_owned());
    }
    let new_string = word(arguments, "new_string");
    if old_string == new_string {
        return Err("old_string and new_string must differ".to_owned());
    }
    Ok(EditInput {
        file_path: file_path.to_owned(),
        old_string: old_string.to_owned(),
        new_string: new_string.to_owned(),
        replace_all: arguments.get("replace_all").and_then(Value::as_bool) == Some(true),
    })
}

/// Resolves the session working directory for one call (dsh
/// `sessionResolveOptions`: agent calls ensure, agent-less calls leave
/// the directory default).
pub(crate) fn session_cwd(
    seams: &dyn Seams,
    agent: Option<&str>,
) -> Result<Option<String>, BodyError> {
    match agent {
        None => Ok(None),
        Some(agent) => Ok(Some(
            seams
                .working_directory_ensure(agent)
                .map_err(BodyError::plain)?,
        )),
    }
}

/// Formats an edit success (dsh `formatEditOutput`).
#[must_use]
pub fn format_edit_output(display_path: &str, replace_all: bool) -> String {
    if replace_all {
        format!(
            "The file {display_path} has been updated. All occurrences were successfully \
replaced."
        )
    } else {
        format!("The file {display_path} has been updated successfully.")
    }
}

/// Formats a write outcome (dsh `formatWriteOutput`; no file content is
/// echoed back).
#[must_use]
pub fn format_write_output(display_path: &str, operation: WriteOperation) -> String {
    let verb = match operation {
        WriteOperation::Create => "Created",
        WriteOperation::Update => "Updated",
    };
    format!("<path>{display_path}</path>\n<type>file</type>\n<content>\n{verb} file\n</content>")
}

/// Records the new file version and exposes the mutation result with
/// its canonical path (dsh `mutationResult`: the observation event, the
/// version stripped, the path projected).
fn write_mutation_result(
    gate: &ObservedStateGate,
    target: &crate::fs_types::FsTarget,
    outcome: &FsWriteOutcome,
    actor: Option<&str>,
) -> Value {
    gate.observe(
        target,
        FsObservation::Present {
            version: outcome.version.clone(),
        },
        actor,
    );
    json!({
        "path": FsDomain::process_path(target),
        "operation": outcome.operation,
        "before": outcome.before,
        "after": outcome.after,
    })
}

/// The edit-arm [`write_mutation_result`] (the outcome carries no
/// operation word).
fn edit_mutation_result(
    gate: &ObservedStateGate,
    target: &crate::fs_types::FsTarget,
    outcome: &FsEditOutcome,
    actor: Option<&str>,
) -> Value {
    gate.observe(
        target,
        FsObservation::Present {
            version: outcome.version.clone(),
        },
        actor,
    );
    json!({
        "path": FsDomain::process_path(target),
        "before": outcome.before,
        "after": outcome.after,
    })
}

/// The shared mutation failure chain (dsh `remediateFsError(sandbox.
/// mapError(error, sandboxPolicy), target.displayPath)`): the denial
/// marker first, the guarded-mutation remedy second.
fn mutation_failure(error: FsError, policy: Option<&Value>, display_path: &str) -> BodyError {
    let mapped = fs_sandbox::map_error(error, policy);
    fs_error(fs_sandbox::remediate_fs_error(mapped, display_path))
}

/// Executes one `read` call (dsh `applyReadTool.execute`): one stat for
/// absence, type, size routing, and the observed version, then the
/// bounded window and the observation record.
fn read_execute(
    deps: &FsToolDeps<'_>,
    arguments: &Value,
    agent: Option<&str>,
) -> Result<Value, BodyError> {
    let caps = baked_caps();
    let input = parse_read_args(arguments, caps.limit).map_err(BodyError::plain)?;
    let cwd = session_cwd(deps.seams, agent)?;
    let target = deps
        .fs
        .resolve(&input.file_path, cwd.as_deref())
        .map_err(fs_error)?;
    let info = deps.fs.stat(&target).map_err(fs_error)?;
    let Some(info) = info else {
        deps.gate.observe(&target, FsObservation::Absent, agent);
        return Err(BodyError::plain(format!(
            "cannot read \"{}\": not found",
            target.display_path
        )));
    };
    if info.kind != FsKind::File {
        return Err(BodyError::plain(format!(
            "cannot read \"{}\": not a regular file",
            target.display_path
        )));
    }
    // Stream when the file is large OR size is unknown, so a size-less
    // backend never buffers an arbitrarily large file.
    let window = match info.size {
        Some(size) if size < caps.stream_min_size => {
            let text = deps.fs.read_text(&target).map_err(fs_error)?;
            fs_read_render::build_window(
                std::iter::once(Ok(text)),
                &ReadWindow {
                    offset: input.offset,
                    limit: input.limit,
                    max_line_length: caps.max_line_length,
                    max_bytes: caps.max_bytes,
                },
                &target.display_path,
            )
        }
        _ => {
            let stream = deps
                .fs
                .stream_text(&target, Box::new(|| false))
                .map_err(fs_error)?;
            fs_read_render::build_window(
                stream,
                &ReadWindow {
                    offset: input.offset,
                    limit: input.limit,
                    max_line_length: caps.max_line_length,
                    max_bytes: caps.max_bytes,
                },
                &target.display_path,
            )
        }
    }
    .map_err(fs_error)?;
    let outcome = json!({
        "path": FsDomain::process_path(&target),
        "offset": input.offset,
        "lines": window
            .lines
            .iter()
            .map(|line| json!({ "number": line.number, "text": line.text }))
            .collect::<Vec<_>>(),
        "totalLines": window.total_lines,
    });
    deps.gate.observe(
        &target,
        FsObservation::Present {
            version: info.version,
        },
        agent,
    );
    Ok(outcome)
}

/// Executes one `write` call (dsh `applyWriteTool.execute`): the
/// per-call policy resolves BEFORE anything executes, the single-slot
/// intent is unconditional-or-guarded, and the guarded failure chain
/// remaps the error.
fn write_execute(
    deps: &FsToolDeps<'_>,
    arguments: &Value,
    call_id: &str,
    agent: Option<&str>,
) -> Result<Value, BodyError> {
    let (file_path, content) = parse_write_args(arguments).map_err(BodyError::plain)?;
    let policy = fs_sandbox::resolve_policy(
        deps.seams,
        arguments,
        agent,
        call_id,
        "write",
        deps.advertised,
    )?;
    let cwd = session_cwd(deps.seams, agent)?;
    let target = deps
        .fs
        .resolve(&file_path, cwd.as_deref())
        .map_err(fs_error)?;
    // Single-slot decision: the gate produces createIfAbsent/
    // replaceIfVersion; a call with no owner defaults to
    // createIfAbsent (the bare unconditional write never runs — the
    // gate always answers).
    let intent = deps.gate.write_intent(&target, agent);
    let outcome = deps
        .fs
        .write_text(&target, &content, Some(&intent), policy.as_ref())
        .map_err(|error| mutation_failure(error, policy.as_ref(), &target.display_path))?;
    Ok(write_mutation_result(deps.gate, &target, &outcome, agent))
}

/// Executes one `edit` call (dsh `applyEditTool.execute`): the intent
/// slot itself can refuse for an unread target, so it sits inside the
/// remapped failure chain.
fn edit_execute(
    deps: &FsToolDeps<'_>,
    arguments: &Value,
    call_id: &str,
    agent: Option<&str>,
) -> Result<Value, BodyError> {
    let input = parse_edit_args(arguments).map_err(BodyError::plain)?;
    let policy = fs_sandbox::resolve_policy(
        deps.seams,
        arguments,
        agent,
        call_id,
        "edit",
        deps.advertised,
    )?;
    let cwd = session_cwd(deps.seams, agent)?;
    let target = deps
        .fs
        .resolve(&input.file_path, cwd.as_deref())
        .map_err(fs_error)?;
    let request = FsEditRequest {
        old_string: input.old_string,
        new_string: input.new_string,
        replace_all: input.replace_all,
    };
    let run = || -> Result<FsEditOutcome, FsError> {
        let version = deps.gate.edit_intent(&target, agent)?;
        deps.fs
            .edit_text(&target, &request, Some(&version), policy.as_ref())
    };
    match run() {
        Ok(outcome) => Ok(edit_mutation_result(deps.gate, &target, &outcome, agent)),
        Err(error) => Err(mutation_failure(
            error,
            policy.as_ref(),
            &target.display_path,
        )),
    }
}

/// Renders the read output (dsh `output.render`): the envelope recomputed
/// from the canonical value with the byte-truncation flag re-derived.
pub(crate) fn render_read(arguments: &Value, value: &Value) -> Result<Vec<Value>, String> {
    let caps = baked_caps();
    let input = parse_read_args(arguments, caps.limit)?;
    let lines = value
        .get("lines")
        .and_then(Value::as_array)
        .ok_or_else(|| "read render requires lines".to_owned())?
        .iter()
        .map(|line| {
            Ok(FileTextLine {
                number: line
                    .get("number")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "read render requires line numbers".to_owned())?,
                text: line
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "read render requires line text".to_owned())?
                    .to_owned(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let offset = value
        .get("offset")
        .and_then(Value::as_u64)
        .ok_or_else(|| "read render requires offset".to_owned())?;
    let total_lines = value
        .get("totalLines")
        .and_then(Value::as_u64)
        .ok_or_else(|| "read render requires totalLines".to_owned())?;
    let end_line = lines
        .last()
        .map_or_else(|| offset.saturating_sub(1), |line| line.number);
    let truncated_by_bytes = (lines.len() as u64) < input.limit && end_line < total_lines;
    let path = value
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "read render requires path".to_owned())?;
    let text = fs_read_render::format_read_output(
        path,
        &FileReadOutcome {
            offset,
            lines,
            total_lines,
            truncated_by_bytes,
        },
    );
    Ok(vec![json!({ "type": "text", "text": text })])
}

/// Renders the write output (dsh `output.render`: the Created/Updated
/// envelope from the canonical value).
pub(crate) fn render_write(_arguments: &Value, value: &Value) -> Result<Vec<Value>, String> {
    let path = value
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "write render requires path".to_owned())?;
    let operation = match value.get("operation").and_then(Value::as_str) {
        Some("create") => WriteOperation::Create,
        Some("update") => WriteOperation::Update,
        other => {
            return Err(format!(
                "write render requires an operation word, got {other:?}"
            ));
        }
    };
    Ok(vec![json!({
        "type": "text",
        "text": format_write_output(path, operation),
    })])
}

/// Renders the edit output (dsh `output.render`: the confirmation
/// sentence, worded by the raw `replace_all` argument).
pub(crate) fn render_edit(arguments: &Value, value: &Value) -> Result<Vec<Value>, String> {
    let path = value
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "edit render requires path".to_owned())?;
    let replace_all = arguments.get("replace_all").and_then(Value::as_bool) == Some(true);
    Ok(vec![json!({
        "type": "text",
        "text": format_edit_output(path, replace_all),
    })])
}

/// Projects the read meta (dsh `output.presentationMeta`): the
/// structured window with the derived language hint, so a UI's read
/// card survives replay.
pub(crate) fn presentation_meta_read(_arguments: &Value, value: &Value) -> Result<Value, String> {
    let path = value
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "read meta requires path".to_owned())?;
    let mut meta = serde_json::Map::new();
    meta.insert("path".to_owned(), json!(path));
    meta.insert(
        "offset".to_owned(),
        value
            .get("offset")
            .cloned()
            .ok_or_else(|| "read meta requires offset".to_owned())?,
    );
    meta.insert(
        "lines".to_owned(),
        value
            .get("lines")
            .cloned()
            .ok_or_else(|| "read meta requires lines".to_owned())?,
    );
    meta.insert(
        "totalLines".to_owned(),
        value
            .get("totalLines")
            .cloned()
            .ok_or_else(|| "read meta requires totalLines".to_owned())?,
    );
    if let Some(lang) = fs_read_render::lang_from_path(path) {
        meta.insert("lang".to_owned(), json!(lang));
    }
    Ok(Value::Object(meta))
}

/// Projects the write meta (dsh `output.presentationMeta`): the applied
/// contextual-diff hunks plus the operation word.
pub(crate) fn presentation_meta_write(_arguments: &Value, value: &Value) -> Result<Value, String> {
    let path = value
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "write meta requires path".to_owned())?;
    let before = value.get("before");
    let after = value
        .get("after")
        .and_then(Value::as_str)
        .ok_or_else(|| "write meta requires after".to_owned())?;
    let diffs = match before {
        None | Some(Value::Null) => Vec::new(),
        Some(before) => fs_diff::compute_hunk_diffs(
            path,
            before.as_str().ok_or("write meta before must be text")?,
            after,
        ),
    };
    Ok(json!({
        "operation": value.get("operation").cloned().unwrap_or(Value::Null),
        "path": path,
        "diffs": diffs
            .iter()
            .map(|hunk| json!({
                "path": hunk.path,
                "oldText": hunk.old_text,
                "newText": hunk.new_text,
            }))
            .collect::<Vec<_>>(),
    }))
}

/// Projects the edit meta (dsh `output.presentationMeta`): the applied
/// contextual-diff hunks.
pub(crate) fn presentation_meta_edit(_arguments: &Value, value: &Value) -> Result<Value, String> {
    let path = value
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "edit meta requires path".to_owned())?;
    let before = value
        .get("before")
        .and_then(Value::as_str)
        .ok_or_else(|| "edit meta requires before".to_owned())?;
    let after = value
        .get("after")
        .and_then(Value::as_str)
        .ok_or_else(|| "edit meta requires after".to_owned())?;
    let diffs = fs_diff::compute_hunk_diffs(path, before, after);
    Ok(json!({
        "path": path,
        "diffs": diffs
            .iter()
            .map(|hunk| json!({
                "path": hunk.path,
                "oldText": hunk.old_text,
                "newText": hunk.new_text,
            }))
            .collect::<Vec<_>>(),
    }))
}

/// Presents the pending read call (dsh `presentCall`): a generic card
/// titled by the file with the raw-args window appended.
#[must_use]
pub fn present_call_read(arguments: &Value) -> Value {
    let file_path = word(arguments, "file_path");
    let offset = arguments.get("offset").and_then(Value::as_f64);
    let limit = arguments.get("limit").and_then(Value::as_f64);
    let window_note = if limit.is_some_and(|limit| limit > 0.0) {
        let first = offset.unwrap_or(1.0);
        let last = first + limit.unwrap_or_default() - 1.0;
        format!(
            " ({} - {})",
            crate::service::js_number(first),
            crate::service::js_number(last)
        )
    } else if let Some(offset) = offset {
        format!(" (from line {})", crate::service::js_number(offset))
    } else {
        String::new()
    };
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the schema pins integral offsets; a fractional value is invalid upstream"
    )]
    let line = offset.map_or(1, |offset| offset as u64);
    json!({
        "card": "generic",
        "title": format!("Read {file_path}{window_note}"),
        "kind": "read",
        "locations": [{ "path": file_path, "line": line }],
    })
}

/// Presents the pending write call (dsh `presentCall`): a diff card
/// with `oldText: null` (a call-time presenter has no prior content).
#[must_use]
pub fn present_call_write(arguments: &Value) -> Value {
    let file_path = word(arguments, "file_path");
    let content = word(arguments, "content");
    json!({
        "card": "diff",
        "title": format!("Write {file_path}"),
        "diffs": [{ "path": file_path, "oldText": null, "newText": content }],
        "locations": [{ "path": file_path }],
    })
}

/// Presents the pending edit call (dsh `presentCall`): a diff card of
/// the literal replacement; an empty `old_string` maps to `null`.
#[must_use]
pub fn present_call_edit(arguments: &Value) -> Value {
    let file_path = word(arguments, "file_path");
    let old_string = word(arguments, "old_string");
    let new_string = word(arguments, "new_string");
    let old_text = if old_string.is_empty() {
        Value::Null
    } else {
        Value::String(old_string.to_owned())
    };
    json!({
        "card": "diff",
        "title": format!("Edit {file_path}"),
        "diffs": [{ "path": file_path, "oldText": old_text, "newText": new_string }],
        "locations": [{ "path": file_path }],
    })
}

/// The result frame's fields the presenters consume.
struct ResultFrame<'a> {
    is_error: bool,
    meta: Option<&'a Value>,
    text: Option<&'a str>,
}

fn result_frame(result: &Value) -> Option<ResultFrame<'_>> {
    let is_error = result.get("isError").and_then(Value::as_bool) == Some(true);
    if is_error {
        return Some(ResultFrame {
            is_error: true,
            meta: None,
            text: None,
        });
    }
    let meta = result.get("meta").filter(|meta| !meta.is_null());
    let content = result.get("content")?.as_array()?;
    let text = if content.len() == 1 {
        content.first()?.get("text").and_then(Value::as_str)
    } else {
        None
    };
    Some(ResultFrame {
        is_error: false,
        meta,
        text,
    })
}

/// Presents the completed read (dsh `presentResult`): the structured
/// card rebuilt from the persisted meta with the envelope-stripped body,
/// declining on any malformed or obsolete replay shape.
pub(crate) fn present_result_read(_arguments: &Value, result: &Value) -> Option<Value> {
    let frame = result_frame(result)?;
    if frame.is_error {
        return None;
    }
    let meta = fs_read_render::read_meta_from_meta(frame.meta?)?;
    let body = fs_read_render::envelope_body(frame.text?)?;
    let mut view = serde_json::Map::new();
    view.insert("card".to_owned(), json!("read"));
    view.insert("path".to_owned(), json!(meta.path));
    view.insert("offset".to_owned(), json!(meta.offset));
    view.insert(
        "lines".to_owned(),
        Value::Array(
            meta.lines
                .iter()
                .map(|line| json!({ "number": line.number, "text": line.text }))
                .collect(),
        ),
    );
    view.insert("totalLines".to_owned(), json!(meta.total_lines));
    if let Some(lang) = &meta.lang {
        view.insert("lang".to_owned(), json!(lang));
    }
    view.insert(
        "content".to_owned(),
        json!([{ "type": "text", "text": body }]),
    );
    Some(Value::Object(view))
}

/// Presents the completed write (dsh `presentResult`): applied metadata
/// replaces the call-time snippet; creates and identical overwrites use
/// the replay-safe args fallback.
pub(crate) fn present_result_write(arguments: &Value, result: &Value) -> Option<Value> {
    let frame = result_frame(result)?;
    if frame.is_error {
        return None;
    }
    let file_path = word(arguments, "file_path");
    let diffs = fs_diff::diffs_from_meta(frame.meta).unwrap_or_else(|| {
        vec![fs_diff::FileDiffHunk {
            path: fs_diff::path_from_meta(frame.meta).unwrap_or_else(|| file_path.to_owned()),
            old_text: None,
            new_text: word(arguments, "content").to_owned(),
        }]
    });
    Some(json!({
        "card": "diff",
        "title": format!("Write {file_path}"),
        "diffs": diffs
            .iter()
            .map(|hunk| json!({
                "path": hunk.path,
                "oldText": hunk.old_text,
                "newText": hunk.new_text,
            }))
            .collect::<Vec<_>>(),
    }))
}

/// Presents the completed edit (dsh `presentResult`): applied metadata
/// only; errors or malformed replay metadata decline to the generic
/// rendering.
pub(crate) fn present_result_edit(arguments: &Value, result: &Value) -> Option<Value> {
    let frame = result_frame(result)?;
    if frame.is_error {
        return None;
    }
    let diffs = fs_diff::diffs_from_meta(frame.meta)?;
    let file_path = word(arguments, "file_path");
    Some(json!({
        "card": "diff",
        "title": format!("Edit {file_path}"),
        "diffs": diffs
            .iter()
            .map(|hunk| json!({
                "path": hunk.path,
                "oldText": hunk.old_text,
                "newText": hunk.new_text,
            }))
            .collect::<Vec<_>>(),
    }))
}

/// Composes the `tool:write` section text the way the dsh source does
/// (the base sentence, the edit tail only while `edit` registers, and
/// the closing period — the extracted segment rule).
#[must_use]
pub fn write_section_text(edit_registered: bool) -> String {
    let segments = catalog_gen::WRITE_SECTION_TEXTS;
    let mut text = String::from(segments[0]);
    if edit_registered {
        text.push_str(segments[1]);
    }
    text.push_str(segments[2]);
    text
}

/// Serves one fs-tool method invocation (the third export-table band:
/// dsh tool objects endpoint-ized, the Q101′A seam-host judgment).
///
/// # Errors
///
/// The payload decode refusal, an unknown method, or the tool body's
/// own failure (the wire body-error shape).
pub fn serve(deps: &FsToolDeps<'_>, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
    let request: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(payload)
            .map_err(|error| format!("builtin-tools payload must be JSON: {error}"))?
    };
    let reply = match method {
        crate::service::METHOD_READ_EXECUTE => {
            return serve_execute(deps, ToolKind::Read, &request);
        }
        crate::service::METHOD_WRITE_EXECUTE => {
            return serve_execute(deps, ToolKind::Write, &request);
        }
        crate::service::METHOD_EDIT_EXECUTE => {
            return serve_execute(deps, ToolKind::Edit, &request);
        }
        METHOD_READ_RENDER => json!(render_read(&request["args"], &request["value"])?),
        METHOD_WRITE_RENDER => json!(render_write(&request["args"], &request["value"])?),
        METHOD_EDIT_RENDER => json!(render_edit(&request["args"], &request["value"])?),
        METHOD_READ_PRESENTATION_META => {
            json!(presentation_meta_read(&request["args"], &request["value"])?)
        }
        METHOD_WRITE_PRESENTATION_META => {
            json!(presentation_meta_write(
                &request["args"],
                &request["value"]
            )?)
        }
        METHOD_EDIT_PRESENTATION_META => {
            json!(presentation_meta_edit(&request["args"], &request["value"])?)
        }
        METHOD_READ_PRESENT_CALL => present_call_read(&request["args"]),
        METHOD_WRITE_PRESENT_CALL => present_call_write(&request["args"]),
        METHOD_EDIT_PRESENT_CALL => present_call_edit(&request["args"]),
        METHOD_READ_PRESENT_RESULT => {
            present_result_read(&request["args"], &request["result"]).unwrap_or(Value::Null)
        }
        METHOD_WRITE_PRESENT_RESULT => {
            present_result_write(&request["args"], &request["result"]).unwrap_or(Value::Null)
        }
        METHOD_EDIT_PRESENT_RESULT => {
            present_result_edit(&request["args"], &request["result"]).unwrap_or(Value::Null)
        }
        other => return Err(format!("unknown fr-tools-builtin fs-tool method {other}")),
    };
    serde_json::to_vec(&reply)
        .map_err(|error| format!("builtin-tools reply encode failed: {error}"))
}

/// Serves one tool body: decodes the execution frame, runs the dsh
/// pipeline, and maps the outcome onto the wire body protocol.
fn serve_execute(
    deps: &FsToolDeps<'_>,
    kind: ToolKind,
    request: &Value,
) -> Result<Vec<u8>, String> {
    let call = request
        .get("call")
        .ok_or_else(|| "builtin-tools execute payload requires call".to_string())?;
    let call_id = call
        .get("call_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let agent = call.get("agent").and_then(Value::as_str);
    let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
    let outcome = match kind {
        ToolKind::Read => read_execute(deps, &arguments, agent),
        ToolKind::Write => write_execute(deps, &arguments, call_id, agent),
        ToolKind::Edit => edit_execute(deps, &arguments, call_id, agent),
    };
    match outcome {
        Ok(value) => serde_json::to_vec(&json!({ "value": value }))
            .map_err(|error| format!("body reply encode failed: {error}")),
        Err(error) => {
            let body = match error.code {
                Some(code) => {
                    json!({ "error": { "code": code, "message": error.message, "name": error.name } })
                }
                None => json!({ "error": { "message": error.message, "name": error.name } }),
            };
            serde_json::to_vec(&body).map_err(|error| format!("body error encode failed: {error}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::service::METHOD_EDIT_EXECUTE;
    use crate::service::METHOD_READ_EXECUTE;
    use crate::service::METHOD_WRITE_EXECUTE;

    /// One unique scratch directory per test (cleaned on drop).
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fr-fs-tools-{tag}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&dir).expect("scratch dir creates");
            Self(dir)
        }

        fn path(&self, name: &str) -> String {
            self.0.join(name).to_string_lossy().into_owned()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The standing policy over one fixed value.
    struct FixedPolicy(Value);

    impl crate::fs_domain::StandingPolicy for FixedPolicy {
        fn resolve(&self, _session: Option<&str>) -> Result<Value, String> {
            Ok(self.0.clone())
        }
    }

    /// The scripted cross-plugin seam: fixed standing policy,
    /// approval word, and session directory.
    struct ScriptSeams {
        policy: Option<Value>,
        approval_word: String,
        workdir: String,
    }

    impl crate::tools::PolicySeam for ScriptSeams {
        fn resolve_policy(&self, _session: Option<&str>) -> Result<Option<Value>, String> {
            Ok(self.policy.clone())
        }
    }

    impl crate::tools::ApprovalSeam for ScriptSeams {
        fn request(&self, _ask: &fr_sandbox::escalation::EscalationAsk) -> Result<String, String> {
            Ok(self.approval_word.clone())
        }
    }

    impl Seams for ScriptSeams {
        fn attachments(&self) -> Option<&dyn crate::tools::AttachmentSeam> {
            None
        }

        fn llm(&self) -> Option<&dyn crate::tools::LlmSeam> {
            None
        }

        fn route_header(&self) -> Option<&dyn crate::tools::RouteHeaderSeam> {
            None
        }

        fn policy(&self) -> &dyn crate::tools::PolicySeam {
            self
        }

        fn approval(&self) -> &dyn crate::tools::ApprovalSeam {
            self
        }

        fn shell_resolve(&self, _shell: &str, _request: &Value) -> Result<Value, String> {
            Err("unused".to_owned())
        }

        fn shell_start(
            &self,
            _shell: &str,
            _spec: &Value,
            _command: &str,
            _owner: Option<&str>,
            _call_id: &str,
            _root_call_id: &str,
            _escalation_modes: &[&str],
        ) -> Result<(String, String), String> {
            Err("unused".to_owned())
        }

        fn shell_result(&self, _job_id: &str) -> Result<Value, String> {
            Err("unused".to_owned())
        }

        fn jobs_wait(
            &self,
            _id: &str,
            _timeout_ms: f64,
            _caller: Option<&str>,
        ) -> Result<Value, String> {
            Err("unused".to_owned())
        }

        fn jobs_list(&self, _caller: Option<&str>) -> Result<Value, String> {
            Ok(json!([]))
        }

        fn jobs_get(&self, _id: &str, _caller: Option<&str>) -> Result<Value, String> {
            Err("unknown job".to_owned())
        }

        fn jobs_read(&self, _id: &str, _caller: Option<&str>) -> Result<Value, String> {
            Err("unused".to_owned())
        }

        fn jobs_kill(
            &self,
            _id: &str,
            _caller: Option<&str>,
            _reason: Option<&str>,
        ) -> Result<String, String> {
            Err("unused".to_owned())
        }

        fn jobs_remove(&self, _id: &str, _caller: Option<&str>) -> Result<(), String> {
            Err("unused".to_owned())
        }

        fn working_directory_ensure(&self, _agent: &str) -> Result<String, String> {
            Ok(self.workdir.clone())
        }

        fn shell_env_collect(
            &self,
            _agent: Option<&str>,
        ) -> Result<crate::shell_env::FrozenEnv, String> {
            Err("unused".to_owned())
        }

        fn subprocess_spawn(&self, _spec: &Value) -> Result<String, String> {
            Err("unused".to_owned())
        }

        fn subprocess_read(
            &self,
            _handle: &str,
            _stream: &str,
            _from: u64,
        ) -> Result<Value, String> {
            Err("unused".to_owned())
        }

        fn subprocess_wait(&self, _handle: &str, _timeout_ms: f64) -> Result<Value, String> {
            Err("unused".to_owned())
        }

        fn subprocess_resolve_executable(&self, _command: &str) -> Result<String, String> {
            Err("unused".to_owned())
        }

        fn spill_store_save_text(&self, _save: &Value) -> Result<Option<Value>, String> {
            Err("unused".to_owned())
        }
    }

    /// The execute frame one serve call rides in.
    fn call_frame(arguments: &Value) -> Value {
        json!({
            "call": {
                "call_id": "c1",
                "root_call_id": "c1",
                "agent": "s1",
                "arguments": arguments,
            }
        })
    }

    /// One composed fixture: scratch dir, fs domain, gate, and seams.
    struct Fixture {
        scratch: Scratch,
        fs: FsDomain,
        gate: ObservedStateGate,
        seams: ScriptSeams,
    }

    impl Fixture {
        fn new(tag: &str, mode: &str) -> Self {
            let scratch = Scratch::new(tag);
            let root = scratch.path("");
            let config = crate::fs_domain::FsDomainConfig {
                cwd: root.clone(),
                diff_basis_max_bytes: crate::fsio::DEFAULT_DIFF_BASIS_MAX_BYTES,
            };
            let fs = FsDomain::new(
                config,
                Box::new(FixedPolicy(json!({
                    "mode": mode,
                    "workspace_root": root,
                }))),
                Some(mode.to_owned()),
            );
            let seam_root = scratch.path("");
            let workdir = scratch.path("");
            Self {
                scratch,
                fs,
                gate: ObservedStateGate::new(),
                seams: ScriptSeams {
                    policy: Some(json!({
                        "mode": mode,
                        "workspace_root": seam_root,
                    })),
                    approval_word: "allowed-once".to_owned(),
                    workdir,
                },
            }
        }

        fn deps(&self) -> FsToolDeps<'_> {
            FsToolDeps {
                fs: &self.fs,
                gate: &self.gate,
                seams: &self.seams,
                advertised: self.fs.sandbox_mode().is_some()
                    && self.fs.sandbox_mode() != Some("danger-full-access"),
            }
        }

        fn serve_ok(&self, method: u32, payload: &Value) -> Value {
            let bytes = serde_json::to_vec(payload).expect("encode");
            let reply = serve(&self.deps(), method, &bytes).expect("serve ok");
            serde_json::from_slice(&reply).expect("reply parses")
        }

        fn serve_err(&self, method: u32, payload: &Value) -> Value {
            let bytes = serde_json::to_vec(payload).expect("encode");
            let reply =
                serve(&self.deps(), method, &bytes).expect("the error body rides the ok transport");
            serde_json::from_slice(&reply).expect("error body parses")
        }
    }

    /// The read round trip: canonical value, envelope render, window
    /// meta with the language hint, and the replayed read card.
    #[test]
    fn read_round_trip_pins_the_shapes() {
        let fixture = Fixture::new("read-ok", "danger-full-access");
        std::fs::write(fixture.scratch.path("a.rs"), "one\ntwo\n").expect("seed");
        let value = fixture.serve_ok(
            METHOD_READ_EXECUTE,
            &call_frame(&json!({ "file_path": "a.rs" })),
        )["value"]
            .clone();
        assert_eq!(value["offset"], 1);
        assert_eq!(value["totalLines"], 2);
        assert_eq!(value["lines"][1]["text"], "two");
        let canonical = value["path"].as_str().expect("path").to_owned();
        assert!(canonical.ends_with("a.rs"), "{canonical}");

        let rendered = fixture.serve_ok(
            METHOD_READ_RENDER,
            &json!({ "args": { "file_path": "a.rs" }, "value": value }),
        );
        let text = rendered[0]["text"].as_str().expect("text");
        assert!(text.contains("<path>"));
        assert!(text.contains("1: one"));
        assert!(text.contains("(End of file - total 2 lines)"));

        let meta = fixture.serve_ok(
            METHOD_READ_PRESENTATION_META,
            &json!({ "args": {}, "value": value }),
        );
        assert_eq!(meta["lang"], "rs");

        let result = json!({
            "isError": false,
            "content": rendered,
            "meta": meta,
        });
        let view = fixture.serve_ok(
            METHOD_READ_PRESENT_RESULT,
            &json!({ "args": { "file_path": "a.rs" }, "result": result }),
        );
        assert_eq!(view["card"], "read");
        assert_eq!(view["lines"][0]["number"], 1);
        assert_eq!(view["lang"], "rs");
        assert_eq!(
            view["content"][0]["text"],
            "1: one\n2: two\n\n(End of file - total 2 lines)"
        );

        let call_view = fixture.serve_ok(
            METHOD_READ_PRESENT_CALL,
            &json!({ "args": { "file_path": "a.rs", "offset": 5, "limit": 3 } }),
        );
        assert_eq!(call_view["title"], "Read a.rs (5 - 7)");
        assert_eq!(call_view["kind"], "read");
        assert_eq!(call_view["locations"][0]["line"], 5);
    }

    /// The read refusal family: absent targets record the absence and
    /// refuse, directories refuse, offset past EOF refuses, and the
    /// argument validations ride the dsh texts.
    #[test]
    fn read_refusals_pin_the_texts() {
        let fixture = Fixture::new("read-refusals", "danger-full-access");
        std::fs::write(fixture.scratch.path("a.txt"), "x\n").expect("seed");
        let absent = fixture.serve_err(
            METHOD_READ_EXECUTE,
            &call_frame(&json!({ "file_path": "missing.txt" })),
        );
        assert_eq!(
            absent["error"]["message"],
            format!(
                "cannot read \"{}\": not found",
                fixture.scratch.path("missing.txt")
            )
        );
        assert_eq!(absent["error"]["name"], "Error");

        let error = fixture.serve_err(
            METHOD_READ_EXECUTE,
            &call_frame(&json!({ "file_path": "a.txt", "offset": 9 })),
        );
        assert_eq!(
            error["error"]["message"],
            format!(
                "offset 9 is out of range for \"{}\" (1 lines)",
                fixture.scratch.path("a.txt")
            )
        );

        let blank = fixture.serve_err(
            METHOD_READ_EXECUTE,
            &call_frame(&json!({ "file_path": "  " })),
        );
        assert_eq!(
            blank["error"]["message"],
            "file_path must be a non-empty string"
        );
        let zero = fixture.serve_err(
            METHOD_READ_EXECUTE,
            &call_frame(&json!({ "file_path": "a.txt", "limit": 0 })),
        );
        assert_eq!(zero["error"]["message"], "limit must be a positive integer");
        let over = fixture.serve_err(
            METHOD_READ_EXECUTE,
            &call_frame(&json!({ "file_path": "a.txt", "limit": 9999 })),
        );
        assert_eq!(
            over["error"]["message"],
            "limit must be less than or equal to 2000"
        );
    }

    /// The write family: create then guarded replace after a read; the
    /// observation gate drives the intents (create-if-absent first,
    /// replace-at-version after the read records the version).
    #[test]
    fn write_create_then_guarded_update_round_trips() {
        let fixture = Fixture::new("write-ok", "danger-full-access");
        // The validation refusals ride the shared texts before any io.
        let blank = fixture.serve_err(
            METHOD_WRITE_EXECUTE,
            &call_frame(&json!({ "file_path": "  ", "content": "x" })),
        );
        assert_eq!(
            blank["error"]["message"],
            "file_path must be a non-empty string"
        );

        let created = fixture.serve_ok(
            METHOD_WRITE_EXECUTE,
            &call_frame(&json!({ "file_path": "a.txt", "content": "one\n" })),
        )["value"]
            .clone();
        assert_eq!(created["operation"], "create");
        assert_eq!(created["before"], Value::Null);
        assert_eq!(created["after"], "one\n");
        let rendered = fixture.serve_ok(
            METHOD_WRITE_RENDER,
            &json!({ "args": {}, "value": created }),
        );
        assert_eq!(
            rendered[0]["text"],
            format!(
                "<path>{}</path>\n<type>file</type>\n<content>\nCreated file\n</content>",
                fixture.scratch.path("a.txt")
            )
        );

        // The read records the version; the next write guards on it.
        let read_value = fixture.serve_ok(
            METHOD_READ_EXECUTE,
            &call_frame(&json!({ "file_path": "a.txt" })),
        )["value"]
            .clone();
        let _ = read_value;
        let updated = fixture.serve_ok(
            METHOD_WRITE_EXECUTE,
            &call_frame(&json!({ "file_path": "a.txt", "content": "two\n" })),
        )["value"]
            .clone();
        assert_eq!(updated["operation"], "update");
        assert_eq!(updated["before"], "one\n");

        // An external mutation between the read and the write leaves
        // the gate's version stale; the remediated text appends the
        // re-read remedy.
        std::fs::write(fixture.scratch.path("a.txt"), "external\n").expect("external write");
        let stale = fixture.serve_err(
            METHOD_WRITE_EXECUTE,
            &call_frame(&json!({ "file_path": "a.txt", "content": "three\n" })),
        );
        assert_eq!(
            stale["error"]["message"],
            format!(
                "cannot write \"{}\": file changed since it was read — re-read the file, \
then retry",
                fixture.scratch.path("a.txt")
            )
        );
    }

    /// The edit family: unique replace succeeds with applied diffs; an
    /// unread file refuses with the read remedy; a missing match and an
    /// ambiguous match refuse verbatim.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the family pins five arms end to end over one shared fixture"
    )]
    fn edit_family_pins_the_arms() {
        let fixture = Fixture::new("edit-ok", "danger-full-access");
        std::fs::write(fixture.scratch.path("a.txt"), "alpha\nbeta\n").expect("seed");

        // The validation refusal rides before the read gate.
        let blank = fixture.serve_err(
            METHOD_EDIT_EXECUTE,
            &call_frame(&json!({
                "file_path": "  ",
                "old_string": "a",
                "new_string": "b",
            })),
        );
        assert_eq!(
            blank["error"]["message"],
            "file_path must be a non-empty string"
        );

        // Unread: the gate refuses before the provider runs.
        let unread = fixture.serve_err(
            METHOD_EDIT_EXECUTE,
            &call_frame(&json!({
                "file_path": "a.txt",
                "old_string": "alpha",
                "new_string": "ALPHA",
            })),
        );
        assert_eq!(
            unread["error"]["message"],
            format!(
                "cannot modify \"{}\": file has not been read — read the file, then retry",
                fixture.scratch.path("a.txt")
            )
        );

        // The read unlocks the edit.
        let _ = fixture.serve_ok(
            METHOD_READ_EXECUTE,
            &call_frame(&json!({ "file_path": "a.txt" })),
        );
        let value = fixture.serve_ok(
            METHOD_EDIT_EXECUTE,
            &call_frame(&json!({
                "file_path": "a.txt",
                "old_string": "alpha",
                "new_string": "ALPHA",
            })),
        )["value"]
            .clone();
        assert_eq!(value["before"], "alpha\nbeta\n");
        assert_eq!(value["after"], "ALPHA\nbeta\n");

        let meta = fixture.serve_ok(
            METHOD_EDIT_PRESENTATION_META,
            &json!({ "args": {}, "value": value }),
        );
        // The trailing context line rides both sides of the hunk.
        assert_eq!(meta["diffs"][0]["oldText"], "alpha\nbeta");
        assert_eq!(meta["diffs"][0]["newText"], "ALPHA\nbeta");

        let rendered = fixture.serve_ok(
            METHOD_EDIT_RENDER,
            &json!({ "args": { "replace_all": true }, "value": value }),
        );
        assert_eq!(
            rendered[0]["text"],
            format!(
                "The file {} has been updated. All occurrences were successfully replaced.",
                fixture.scratch.path("a.txt")
            )
        );

        // Uniqueness: a second occurrence flips to the ambiguous text.
        let _ = fixture.serve_ok(
            METHOD_READ_EXECUTE,
            &call_frame(&json!({ "file_path": "a.txt" })),
        );
        std::fs::write(fixture.scratch.path("a.txt"), "dup\ndup\n").expect("reseed");
        let _ = fixture.serve_ok(
            METHOD_READ_EXECUTE,
            &call_frame(&json!({ "file_path": "a.txt" })),
        );
        let ambiguous = fixture.serve_err(
            METHOD_EDIT_EXECUTE,
            &call_frame(&json!({
                "file_path": "a.txt",
                "old_string": "dup",
                "new_string": "x",
            })),
        );
        assert_eq!(
            ambiguous["error"]["message"],
            format!(
                "old_string matched 2 times in \"{}\"; provide a more specific old_string or \
set replace_all to true",
                fixture.scratch.path("a.txt")
            )
        );
        assert_eq!(ambiguous["error"]["code"], "FS_AMBIGUOUS_EDIT");

        // The presentation arms pin the pending cards and the fallback.
        let call_view = fixture.serve_ok(
            METHOD_EDIT_PRESENT_CALL,
            &json!({ "args": { "file_path": "a.txt", "old_string": "", "new_string": "x" } }),
        );
        assert_eq!(call_view["diffs"][0]["oldText"], Value::Null);
        let write_view = fixture.serve_ok(
            METHOD_WRITE_PRESENT_CALL,
            &json!({ "args": { "file_path": "a.txt", "content": "body" } }),
        );
        assert_eq!(write_view["title"], "Write a.txt");
        assert_eq!(write_view["diffs"][0]["oldText"], Value::Null);
        assert_eq!(write_view["diffs"][0]["newText"], "body");
        let result = json!({
            "isError": false,
            "content": [{ "type": "text", "text": "The file a.txt has been updated successfully." }],
            "meta": { "path": "a.txt", "diffs": [{ "path": "a.txt", "oldText": "alpha", "newText": "ALPHA" }] },
        });
        let edit_card = fixture.serve_ok(
            METHOD_EDIT_PRESENT_RESULT,
            &json!({ "args": { "file_path": "a.txt" }, "result": result }),
        );
        assert_eq!(edit_card["diffs"][0]["oldText"], "alpha");
        // The write fallback: malformed meta falls back to the args.
        let fallback_result = json!({
            "isError": false,
            "content": [{ "type": "text", "text": "Created file" }],
            "meta": { "operation": "create", "path": "a.txt", "diffs": [] },
        });
        let write_card = fixture.serve_ok(
            METHOD_WRITE_PRESENT_RESULT,
            &json!({ "args": { "file_path": "a.txt", "content": "body" }, "result": fallback_result }),
        );
        assert_eq!(write_card["diffs"][0]["oldText"], Value::Null);
        assert_eq!(write_card["diffs"][0]["newText"], "body");
    }

    /// The read-only fence denies a write with the shared denial marker
    /// plus the operation-subject escalation hint, keeping the
    /// structured code in the message chain.
    #[test]
    fn read_only_fence_denies_with_the_marker() {
        let mut fixture = Fixture::new("write-denied", "read-only");
        fixture.seams.policy = Some(json!({
            "mode": "read-only",
            "workspace_root": fixture.scratch.path(""),
        }));
        let denied = fixture.serve_err(
            METHOD_WRITE_EXECUTE,
            &call_frame(&json!({ "file_path": "a.txt", "content": "x" })),
        );
        let message = denied["error"]["message"].as_str().expect("message");
        assert!(
            message.starts_with(
                "[sandbox: file access denied under read-only mode]\n[sandbox: \
escalation available — retry this exact operation once with sandbox_permissions"
            ),
            "{message}"
        );
    }

    /// The escalation pair rides the approval seam and lands in the
    /// merged policy mode (the strictly-wider grant).
    #[test]
    fn escalation_grant_merges_the_mode() {
        let mut fixture = Fixture::new("write-escalate", "workspace-write");
        fixture.seams.policy = Some(json!({
            "mode": "workspace-write",
            "workspace_root": fixture.scratch.path(""),
        }));
        let value = fixture.serve_ok(
            METHOD_WRITE_EXECUTE,
            &call_frame(&json!({
                "file_path": "a.txt",
                "content": "x",
                "sandbox_permissions": "danger-full-access",
                "justification": "the file lives outside the workspace",
            })),
        )["value"]
            .clone();
        assert_eq!(value["operation"], "create");

        // The pairing refusals ride verbatim before anything executes.
        let orphan = fixture.serve_err(
            METHOD_WRITE_EXECUTE,
            &call_frame(&json!({
                "file_path": "a.txt",
                "content": "x",
                "sandbox_permissions": "danger-full-access",
            })),
        );
        assert_eq!(
            orphan["error"]["message"],
            "invalid escalation: sandbox_permissions requires a justification"
        );
    }

    /// The section composition mirrors the dsh conditional: the edit
    /// tail rides only while edit registers.
    #[test]
    fn write_section_composes_conditionally() {
        assert_eq!(
            write_section_text(true),
            "Read an existing file before overwriting it with write (the default \
fs-observation-policy requires it) and prefer edit for targeted changes."
        );
        assert_eq!(
            write_section_text(false),
            "Read an existing file before overwriting it with write (the default \
fs-observation-policy requires it)."
        );
    }
}
