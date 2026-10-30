// Rust guideline compliant 2026-10-30
//! The model-facing `str_replace_editor` tool (dsh
//! `tool-str-replace-editor/src/index.ts`): standalone view/create/
//! unique-literal-replace/line-insert over the in-crate fs domain.
//!
//! The tool rides the same observation gate as read/write/edit (the
//! dsh fs-observation-policy waterfalls dispatched directly, the
//! Q101′A same-crate judgment) and the simple-form mutation policy: a
//! standing-policy resolve with a bare denial marker (no escalation
//! hint, no escalation arguments — the dsh `MutationPolicy` form).
//! Registration is opt-in and the M1 composition bakes the knob off
//! (the builtin-tools contract's default-off ruling); the body ships
//! complete and serves from the editor band when a composition turns it
//! on.

use serde_json::Value;
use serde_json::json;

use crate::fs_tools::FsToolDeps;
use crate::fs_types::FsError;
use crate::fs_types::FsErrorCode;
use crate::fs_types::FsKind;
use crate::fs_types::FsObservation;
use crate::fs_types::FsTarget;
use crate::fs_types::FsWriteIntent;
use crate::service::METHOD_STR_REPLACE_EDITOR_EXECUTE;
use crate::service::METHOD_STR_REPLACE_EDITOR_PRESENT_CALL;
use crate::service::METHOD_STR_REPLACE_EDITOR_RENDER;
use crate::tools::BodyError;

/// The clipped-output marker (dsh `TRUNCATED_MESSAGE`).
const TRUNCATED_MESSAGE: &str = "<response clipped><NOTE>To save on context only part of this \
file has been shown to you. You should retry this tool after you have searched inside the file \
with `grep -n` in order to find the line numbers of what you are looking for.</NOTE>";

/// The default output budget before clipping (the `max-output-chars`
/// config; the dsh default).
pub const DEFAULT_MAX_OUTPUT_CHARS: usize = 16_000;

/// The baked opt-in knob (the `str-replace-editor` config key; the M1
/// composition leaves it off — the builtin-tools contract's
/// 「替代编辑器默认不装配」 ruling).
const BAKED_STR_REPLACE_EDITOR: bool = false;

/// The registration gate for the opt-in editor: the composition
/// registers `str_replace_editor` only when this answers true.
pub(crate) const fn str_replace_editor_registered() -> bool {
    BAKED_STR_REPLACE_EDITOR
}

/// The word a string argument arrives as (absent keys stay empty).
fn word<'a>(arguments: &'a Value, key: &str) -> &'a str {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// Maps an fs-domain error onto the body failure (the `FsError` name
/// rides: dsh `FsError` extends `HarnessError`).
fn fs_error(error: FsError) -> BodyError {
    BodyError {
        code: Some(error.code.as_str().to_owned()),
        name: "FsError".to_owned(),
        message: error.message,
    }
}

/// Maps a failed mutation error for the model (the dsh `MutationPolicy.
/// mapError` simple form): only `FS_SANDBOX_DENIED` becomes the shared
/// `[sandbox: …]` denial marker — no escalation hint — keeping the
/// structured code.
fn map_error(error: FsError, policy: Option<&Value>) -> FsError {
    if error.code != FsErrorCode::SandboxDenied {
        return error;
    }
    let mode = policy
        .and_then(|policy| policy.get("mode"))
        .and_then(Value::as_str)
        .and_then(fr_sandbox::seam::SandboxMode::parse)
        .unwrap_or(fr_sandbox::seam::SandboxMode::ReadOnly);
    FsError::new(
        FsErrorCode::SandboxDenied,
        fr_sandbox::escalation::sandbox_denial_marker(mode),
    )
}

/// Clips one view body past the character budget (dsh
/// `maybeTruncate`).
///
/// The budget counts UTF-16 code units — the JS `.length` fact — and
/// the cut never splits a surrogate pair (the dsh
/// `truncateWithoutSplittingSurrogatePair`).
#[must_use]
pub fn maybe_truncate(content: &str, max_output_chars: usize) -> String {
    if content.encode_utf16().count() <= max_output_chars {
        return content.to_owned();
    }
    let mut units = 0_usize;
    let mut cut = content.len();
    for (index, char) in content.char_indices() {
        let width = char.len_utf16();
        if units + width > max_output_chars {
            cut = index;
            break;
        }
        units += width;
    }
    format!("{}{TRUNCATED_MESSAGE}", &content[..cut])
}

/// The codepoint order the directory rows sort under (the dsh
/// `codepointCompare` judgment: a plain scalar comparison).
fn codepoint_compare(left: &str, right: &str) -> std::cmp::Ordering {
    left.cmp(right)
}

/// Finds every occurrence offset of `search` inside `content` (dsh
/// `matchOffsets`).
fn match_offsets(content: &str, search: &str) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut rest = content;
    let mut base = 0_usize;
    while let Some(found) = rest.find(search) {
        let at = base + found;
        offsets.push(at);
        base = at + search.len();
        rest = &content[base..];
    }
    offsets
}

/// Maps occurrence offsets to 1-based line numbers (dsh
/// `lineNumbersAt`).
fn line_numbers_at(content: &str, offsets: &[usize]) -> Vec<usize> {
    let mut line = 1_usize;
    let mut cursor = 0_usize;
    offsets
        .iter()
        .map(|offset| {
            while cursor < *offset {
                if content.as_bytes()[cursor] == b'\n' {
                    line += 1;
                }
                cursor += 1;
            }
            line
        })
        .collect()
}

/// Resolves the target (dsh `resolveTarget`): a non-blank ABSOLUTE
/// path — the editor resolves with no session directory.
fn resolve_target(deps: &FsToolDeps<'_>, path: &str) -> Result<FsTarget, BodyError> {
    if path.trim().is_empty() {
        return Err(BodyError::plain("path must be a non-empty string"));
    }
    if !std::path::Path::new(path).is_absolute() {
        return Err(BodyError::plain(format!(
            "The path {path} is not an absolute path, it should start with `/`. Maybe you \
meant /{path}?"
        )));
    }
    deps.fs.resolve(path, None).map_err(fs_error)
}

/// Statis an existing target for the non-view commands (dsh
/// `statExisting`): absence records the observation and refuses;
/// directories refuse for every non-view command.
fn stat_existing(
    deps: &FsToolDeps<'_>,
    target: &FsTarget,
    command: &str,
    agent: Option<&str>,
) -> Result<crate::fs_types::FsInfo, BodyError> {
    let Some(info) = deps.fs.stat(target).map_err(fs_error)? else {
        deps.gate.observe(target, FsObservation::Absent, agent);
        return Err(fs_error(FsError::new(
            FsErrorCode::NotFound,
            format!(
                "The path {} does not exist. Please provide a valid path.",
                target.display_path
            ),
        )));
    };
    if info.kind == FsKind::Directory && command != "view" {
        return Err(fs_error(FsError::new(
            FsErrorCode::NotRegularFile,
            format!(
                "The path {} is a directory and only the `view` command can be used on \
directories",
                target.display_path
            ),
        )));
    }
    Ok(info)
}

/// The required-parameter refusal (dsh `requiredForCommand`); the empty
/// guard rides only when the command rejects empty values.
fn required_for_command(
    value: Option<&str>,
    parameter: &str,
    command: &str,
    allow_empty: bool,
) -> Result<String, BodyError> {
    let Some(value) = value else {
        return Err(BodyError::plain(format!(
            "Parameter `{parameter}` is required for command: {command}"
        )));
    };
    if !allow_empty && value.is_empty() {
        return Err(BodyError::plain(format!(
            "Parameter `{parameter}` is empty for command: {command}"
        )));
    }
    Ok(value.to_owned())
}

/// Formats the numbered file view (dsh `formatFileView`): the optional
/// 1-based window, the `%6d` two-space gutter, and the clip. The window
/// arithmetic stays in `f64` end to end (the dsh number semantics, the
/// `-1` sentinel included) and lands on one validated `usize` slice
/// bound.
///
/// # Errors
///
/// The dsh `view_range` validation refusals verbatim.
fn format_file_view(
    path: &str,
    content: &str,
    max_output_chars: usize,
    view_range: Option<&[Value]>,
) -> Result<String, BodyError> {
    let all_lines: Vec<&str> = content.split('\n').collect();
    let total = all_lines.len();
    #[expect(
        clippy::cast_precision_loss,
        reason = "a line count is far below the f64 mantissa, so the widening is exact"
    )]
    let total_f64 = total as f64;
    let mut prompt = format!(
        "Here's the content of {path} with line numbers (which has a total of {total} lines)"
    );
    let window = match view_range {
        Some(view_range) => {
            let (initial, final_) = resolve_view_range(view_range, total_f64, total)?;
            prompt.push_str(" with view_range=[");
            prompt.push_str(&crate::service::js_number(initial));
            prompt.push_str(", ");
            prompt.push_str(&crate::service::js_number(final_));
            prompt.push(']');
            Some((initial, Some(final_)))
        }
        None => None,
    };
    let initial_line = window.map_or(1.0, |(initial, _)| initial);
    let final_line = window.and_then(|(_, final_)| final_);
    #[expect(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "the validated initial line is an integral value of at least 1"
    )]
    let initial_index = initial_line as usize - 1;
    let window_end: Option<usize> = match final_line {
        // The dsh `-1` sentinel views to the end of the file.
        None | Some(-1.0) => None,
        Some(requested_final) =>
        {
            #[expect(
                clippy::cast_sign_loss,
                clippy::cast_possible_truncation,
                reason = "the validated final line is an integral value at or past the window start"
            )]
            Some(requested_final as usize)
        }
    };
    let lines: &[&str] = window_end.map_or(&all_lines[initial_index..], |window_end| {
        &all_lines[initial_index..window_end]
    });
    let numbered: Vec<String> = lines
        .iter()
        .enumerate()
        .map(|(index, line)| format!("{:>6}  {}", initial_index + index + 1, line))
        .collect();
    Ok(maybe_truncate(
        &format!("{}:\n{}\n", prompt, numbered.join("\n")),
        max_output_chars,
    ))
}

/// Validates one dsh `view_range` window against the file line count
/// (the four refusal texts verbatim) and returns the 1-based integral
/// pair.
///
/// # Errors
///
/// The dsh `view_range` validation refusals.
fn resolve_view_range(
    view_range: &[Value],
    total_f64: f64,
    total: usize,
) -> Result<(f64, f64), BodyError> {
    let integral = |value: &Value| {
        value
            .as_f64()
            .filter(|number| number.fract() == 0.0 && number.is_finite())
    };
    let first = view_range.first().and_then(integral);
    let second = view_range.get(1).and_then(integral);
    if view_range.len() != 2 || first.is_none() || second.is_none() {
        return Err(BodyError::plain(
            "Invalid `view_range`. It should be a list of two integers.".to_owned(),
        ));
    }
    let requested_initial = first.expect("checked above");
    let requested_final = second.expect("checked above");
    let first_text = crate::service::js_number(view_range[0].as_f64().unwrap_or_default());
    let second_text = crate::service::js_number(view_range[1].as_f64().unwrap_or_default());
    if requested_initial < 1.0 || requested_initial > total_f64 {
        return Err(BodyError::plain(format!(
            "Invalid `view_range`: [{first_text}, {second_text}]. Its first element `{}` \
should be within the range of lines of the file: [1, {total}]",
            crate::service::js_number(requested_initial)
        )));
    }
    if requested_final > total_f64 {
        return Err(BodyError::plain(format!(
            "Invalid `view_range`: [{first_text}, {second_text}]. Its second element `{}` \
should be smaller than the number of lines in the file: `{total}`",
            crate::service::js_number(requested_final)
        )));
    }
    #[expect(
        clippy::float_cmp,
        reason = "the validated integral value compares against the integral -1 sentinel"
    )]
    let sentinel = requested_final != -1.0;
    if sentinel && requested_final < requested_initial {
        return Err(BodyError::plain(format!(
            "Invalid `view_range`: [{first_text}, {second_text}]. Its second element `{}` \
should be larger or equal than its first `{}`",
            crate::service::js_number(requested_final),
            crate::service::js_number(requested_initial)
        )));
    }
    Ok((requested_initial, requested_final))
}

/// Lists a directory two levels deep (dsh `listDirectory`): non-hidden
/// names only, `node_modules` and Python cache directories excluded,
/// the `d\t` root header row, codepoint-sorted paths.
fn list_directory(
    deps: &FsToolDeps<'_>,
    target: &FsTarget,
    max_output_chars: usize,
) -> Result<String, BodyError> {
    fn visit(
        deps: &FsToolDeps<'_>,
        dir: &FsTarget,
        depth: usize,
        rows: &mut Vec<String>,
    ) -> Result<(), BodyError> {
        for entry in deps.fs.list_dir(dir).map_err(fs_error)? {
            if entry.name.starts_with('.')
                || entry.name == "node_modules"
                || entry.name == "__pycache__"
            {
                continue;
            }
            let type_word = match entry.kind {
                FsKind::Directory => "d",
                FsKind::File => "f",
                FsKind::Other => "?",
            };
            rows.push(format!("{}\t{}", type_word, entry.target.display_path));
            if entry.kind == FsKind::Directory && depth < 2 {
                visit(deps, &entry.target, depth + 1, rows)?;
            }
        }
        Ok(())
    }
    let mut rows = vec![format!("d\t{}", target.display_path)];
    visit(deps, target, 1, &mut rows)?;
    rows.sort_by(|left, right| {
        let left_path = &left[left.find('\t').map_or(0, |at| at + 1)..];
        let right_path = &right[right.find('\t').map_or(0, |at| at + 1)..];
        codepoint_compare(left_path, right_path)
    });
    let listing = maybe_truncate(&format!("{}\n", rows.join("\n")), max_output_chars);
    Ok(format!(
        "Here're the files and directories up to 2 levels deep in {}, excluding hidden \
items, node_modules, and Python cache directories:\n{listing}\n",
        target.display_path
    ))
}

/// Resolves the standing policy (the dsh `MutationPolicy.resolve`):
/// the session-scoped resolve, refusing a confining mount whose policy
/// face went missing.
fn resolve_standing(
    deps: &FsToolDeps<'_>,
    agent: Option<&str>,
) -> Result<Option<Value>, BodyError> {
    let standing = deps
        .seams
        .policy()
        .resolve_policy(agent)
        .map_err(BodyError::plain)?;
    if standing.is_none() && deps.fs.sandbox_mode().is_some() {
        return Err(BodyError::plain(
            "tool-str-replace-editor: the mounted filesystem confines but ctx.sandboxPolicy \
is missing",
        ));
    }
    Ok(standing)
}

/// The `view` command (dsh `viewPath`).
fn view_path(
    deps: &FsToolDeps<'_>,
    arguments: &Value,
    agent: Option<&str>,
    max_output_chars: usize,
) -> Result<String, BodyError> {
    let path = word(arguments, "path").to_owned();
    let view_range = arguments
        .get("view_range")
        .filter(|value| !value.is_null())
        .and_then(Value::as_array)
        .cloned();
    let target = resolve_target(deps, &path)?;
    let info = stat_existing(deps, &target, "view", agent)?;
    if info.kind == FsKind::Directory {
        if view_range.is_some() {
            return Err(BodyError::plain(
                "The `view_range` parameter is not allowed when `path` points to a directory."
                    .to_owned(),
            ));
        }
        return list_directory(deps, &target, max_output_chars);
    }
    if info.kind != FsKind::File {
        return Err(fs_error(FsError::new(
            FsErrorCode::NotRegularFile,
            format!(
                "cannot view \"{}\": not a regular file or directory",
                target.display_path
            ),
        )));
    }
    let content = deps.fs.read_text(&target).map_err(fs_error)?;
    deps.gate.observe(
        &target,
        FsObservation::Present {
            version: info.version,
        },
        agent,
    );
    format_file_view(
        &target.display_path,
        &content,
        max_output_chars,
        view_range.as_deref(),
    )
}

/// The `create` command (dsh `createFile`).
fn create_file(
    deps: &FsToolDeps<'_>,
    arguments: &Value,
    agent: Option<&str>,
) -> Result<String, BodyError> {
    let path = word(arguments, "path").to_owned();
    let file_text = arguments
        .get("file_text")
        .filter(|value| !value.is_null())
        .and_then(Value::as_str);
    let content = required_for_command(file_text, "file_text", "create", true)?;
    let policy = resolve_standing(deps, agent)?;
    let target = resolve_target(deps, &path)?;
    if deps.fs.stat(&target).map_err(fs_error)?.is_some() {
        return Err(BodyError::plain(format!(
            "File already exists at: {}. Cannot overwrite files using command `create`.",
            target.display_path
        )));
    }
    let intent = deps.gate.write_intent(&target, agent);
    let outcome = deps
        .fs
        .write_text(&target, &content, Some(&intent), policy.as_ref())
        .map_err(|error| fs_error(map_error(error, policy.as_ref())))?;
    deps.gate.observe(
        &target,
        FsObservation::Present {
            version: outcome.version,
        },
        agent,
    );
    Ok(format!(
        "New file created successfully at: {}",
        target.display_path
    ))
}

/// The `str_replace` command (dsh `replaceInFile`).
fn replace_in_file(
    deps: &FsToolDeps<'_>,
    arguments: &Value,
    agent: Option<&str>,
) -> Result<String, BodyError> {
    let path = word(arguments, "path").to_owned();
    let new_str = arguments.get("new_str");
    if new_str.is_some_and(Value::is_null) {
        return Err(BodyError::plain(
            "Parameter `new_str` must be omitted or contain a string for command: \
str_replace",
        ));
    }
    let policy = resolve_standing(deps, agent)?;
    let target = resolve_target(deps, &path)?;
    let version = deps.gate.edit_intent(&target, agent).map_err(fs_error)?;
    let old_value = required_for_command(
        arguments
            .get("old_str")
            .filter(|value| !value.is_null())
            .and_then(Value::as_str),
        "old_str",
        "str_replace",
        false,
    )?;
    let new_value = new_str
        .and_then(Value::as_str)
        .map_or_else(String::new, str::to_owned);
    let info = stat_existing(deps, &target, "str_replace", agent)?;
    if info.kind != FsKind::File {
        return Err(fs_error(FsError::new(
            FsErrorCode::NotRegularFile,
            format!(
                "cannot edit \"{}\": not a regular file",
                target.display_path
            ),
        )));
    }
    let before = deps.fs.read_text(&target).map_err(fs_error)?;
    let offsets = match_offsets(&before, &old_value);
    let Some(offset) = offsets.first() else {
        return Err(fs_error(FsError::new(
            FsErrorCode::EditNotFound,
            format!(
                "No replacement was performed, old_str `{old_value}` did not appear verbatim \
in {}.",
                target.display_path
            ),
        )));
    };
    if offsets.len() > 1 {
        let lines = line_numbers_at(&before, &offsets);
        let line_list = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(fs_error(FsError::new(
            FsErrorCode::AmbiguousEdit,
            format!(
                "No replacement was performed. Multiple occurrences of old_str `{old_value}` \
in lines [{line_list}]. Please ensure it is unique"
            ),
        )));
    }
    let after = format!(
        "{}{}{}",
        &before[..*offset],
        new_value,
        &before[offset + old_value.len()..]
    );
    let intent = FsWriteIntent::ReplaceIfVersion { version };
    let outcome = deps
        .fs
        .write_text(&target, &after, Some(&intent), policy.as_ref())
        .map_err(|error| fs_error(map_error(error, policy.as_ref())))?;
    deps.gate.observe(
        &target,
        FsObservation::Present {
            version: outcome.version,
        },
        agent,
    );
    Ok(format!(
        "The file {} has been edited successfully.",
        target.display_path
    ))
}

/// The `insert` command (dsh `insertInFile`).
fn insert_in_file(
    deps: &FsToolDeps<'_>,
    arguments: &Value,
    agent: Option<&str>,
) -> Result<String, BodyError> {
    let path = word(arguments, "path").to_owned();
    let insert_line = arguments
        .get("insert_line")
        .filter(|value| !value.is_null())
        .and_then(Value::as_f64);
    let Some(insert_line) = insert_line else {
        return Err(BodyError::plain(
            "Parameter `insert_line` is required for command: insert",
        ));
    };
    let new_value = required_for_command(
        arguments
            .get("new_str")
            .filter(|value| !value.is_null())
            .and_then(Value::as_str),
        "new_str",
        "insert",
        true,
    )?;
    let policy = resolve_standing(deps, agent)?;
    let target = resolve_target(deps, &path)?;
    let version = deps.gate.edit_intent(&target, agent).map_err(fs_error)?;
    let info = stat_existing(deps, &target, "insert", agent)?;
    if info.kind != FsKind::File {
        return Err(fs_error(FsError::new(
            FsErrorCode::NotRegularFile,
            format!(
                "cannot insert into \"{}\": not a regular file",
                target.display_path
            ),
        )));
    }
    let before = deps.fs.read_text(&target).map_err(fs_error)?;
    let lines: Vec<&str> = before.split('\n').collect();
    let total = lines.len();
    #[expect(
        clippy::cast_precision_loss,
        reason = "a line count is far below the f64 mantissa, so the widening is exact"
    )]
    let total_f64 = total as f64;
    if insert_line.fract() != 0.0 || !(0.0..=total_f64).contains(&insert_line) {
        return Err(BodyError::plain(format!(
            "Invalid `insert_line` parameter: {}. It should be within the range of lines of \
the file: [0, {total}]",
            crate::service::js_number(insert_line)
        )));
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "validated integral within the line range above"
    )]
    let at = insert_line as usize;
    let mut after_lines: Vec<String> = lines[..at].iter().map(|line| (*line).to_owned()).collect();
    after_lines.extend(new_value.split('\n').map(str::to_owned));
    after_lines.extend(lines[at..].iter().map(|line| (*line).to_owned()));
    let intent = FsWriteIntent::ReplaceIfVersion { version };
    let outcome = deps
        .fs
        .write_text(
            &target,
            &after_lines.join("\n"),
            Some(&intent),
            policy.as_ref(),
        )
        .map_err(|error| fs_error(map_error(error, policy.as_ref())))?;
    deps.gate.observe(
        &target,
        FsObservation::Present {
            version: outcome.version,
        },
        agent,
    );
    Ok(format!(
        "The file {} has been edited successfully.",
        target.display_path
    ))
}

/// Presents the pending call (dsh `presentEditorCall`): the four arms
/// — a read card for view, diff cards for create/`str_replace`, and an
/// edit card with the line anchor for insert.
#[must_use]
pub fn present_call(arguments: &Value) -> Value {
    let path = word(arguments, "path");
    match word(arguments, "command") {
        "view" => json!({
            "card": "generic",
            "title": format!("view {path}"),
            "kind": "read",
            "locations": [{ "path": path }],
        }),
        "create" => json!({
            "card": "diff",
            "title": format!("create {path}"),
            "diffs": [{ "path": path, "oldText": null, "newText": word(arguments, "file_text") }],
            "locations": [{ "path": path }],
        }),
        "str_replace" => json!({
            "card": "diff",
            "title": format!("str_replace {path}"),
            "diffs": [{
                "path": path,
                "oldText": arguments
                    .get("old_str")
                    .filter(|value| !value.is_null())
                    .and_then(Value::as_str)
                    .map_or(Value::Null, |text| json!(text)),
                "newText": arguments
                    .get("new_str")
                    .filter(|value| !value.is_null())
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            }],
            "locations": [{ "path": path }],
        }),
        _ => {
            let line = arguments
                .get("insert_line")
                .filter(|value| !value.is_null())
                .and_then(Value::as_f64)
                .map_or_else(
                    || json!(1_u64),
                    |line| {
                        #[expect(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            reason = "the schema pins integral insert_line; a fractional value is invalid upstream"
                        )]
                        let anchored = (line + 1.0).max(1.0) as u64;
                        json!(anchored)
                    },
                );
            json!({
                "card": "generic",
                "title": format!("insert {path}"),
                "kind": "edit",
                "locations": [{ "path": path, "line": line }],
            })
        }
    }
}

/// Renders the output (dsh `output.render`): the string value passes
/// through as the single text block.
///
/// # Errors
///
/// The canonical-value shape refusal (a non-string value).
pub fn render(_arguments: &Value, value: &Value) -> Result<Vec<Value>, String> {
    let text = value
        .as_str()
        .ok_or_else(|| "str_replace_editor render requires a string".to_owned())?;
    Ok(vec![json!({ "type": "text", "text": text })])
}

/// Executes one call (the dsh `execute` four-arm dispatch).
fn execute(
    deps: &FsToolDeps<'_>,
    arguments: &Value,
    agent: Option<&str>,
) -> Result<String, BodyError> {
    let max_output_chars = DEFAULT_MAX_OUTPUT_CHARS;
    match word(arguments, "command") {
        "view" => view_path(deps, arguments, agent, max_output_chars),
        "create" => create_file(deps, arguments, agent),
        "str_replace" => replace_in_file(deps, arguments, agent),
        "insert" => insert_in_file(deps, arguments, agent),
        other => Err(BodyError::plain(format!(
            "Parameter `command` must be one of view, create, str_replace, insert, got \
{other}"
        ))),
    }
}

/// Serves one tool body: decodes the execution frame, runs the dsh
/// pipeline, and maps the outcome onto the wire body protocol.
fn serve_execute(deps: &FsToolDeps<'_>, request: &Value) -> Result<Vec<u8>, String> {
    let call = request
        .get("call")
        .ok_or_else(|| "builtin-tools execute payload requires call".to_string())?;
    let agent = call.get("agent").and_then(Value::as_str);
    let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
    match execute(deps, &arguments, agent) {
        Ok(value) => serde_json::to_vec(&json!({ "value": value }))
            .map_err(|error| format!("body reply encode failed: {error}")),
        Err(error) => {
            let body = match error.code {
                Some(code) => json!({
                    "error": { "code": code, "message": error.message, "name": error.name }
                }),
                None => json!({ "error": { "message": error.message, "name": error.name } }),
            };
            serde_json::to_vec(&body).map_err(|error| format!("body error encode failed: {error}"))
        }
    }
}

/// Serves one editor method invocation (the editor band).
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
        METHOD_STR_REPLACE_EDITOR_EXECUTE => {
            return serve_execute(deps, &request);
        }
        METHOD_STR_REPLACE_EDITOR_RENDER => {
            json!(render(&request["args"], &request["value"])?)
        }
        METHOD_STR_REPLACE_EDITOR_PRESENT_CALL => present_call(&request["args"]),
        other => return Err(format!("unknown fr-tools-builtin editor method {other}")),
    };
    serde_json::to_vec(&reply)
        .map_err(|error| format!("builtin-tools reply encode failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::fs_domain::FsDomain;
    use crate::observation::ObservedStateGate;

    /// One unique scratch directory per test (cleaned on drop).
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fr-sre-{tag}-{}-{}",
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

    /// One standing policy over one fixed value.
    struct FixedPolicy(Value);

    impl crate::fs_domain::StandingPolicy for FixedPolicy {
        fn resolve(&self, _session: Option<&str>) -> Result<Value, String> {
            Ok(self.0.clone())
        }
    }

    /// The scripted cross-plugin seam.
    struct ScriptSeams {
        policy: Option<Value>,
        workdir: String,
    }

    impl crate::tools::PolicySeam for ScriptSeams {
        fn resolve_policy(&self, _session: Option<&str>) -> Result<Option<Value>, String> {
            Ok(self.policy.clone())
        }
    }

    impl crate::tools::ApprovalSeam for ScriptSeams {
        fn request(&self, _ask: &fr_sandbox::escalation::EscalationAsk) -> Result<String, String> {
            Err("unused".to_owned())
        }
    }

    impl crate::tools::Seams for ScriptSeams {
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
            let fs = FsDomain::new(
                crate::fs_domain::FsDomainConfig {
                    cwd: root.clone(),
                    diff_basis_max_bytes: crate::fsio::DEFAULT_DIFF_BASIS_MAX_BYTES,
                },
                Box::new(FixedPolicy(json!({
                    "mode": mode,
                    "workspace_root": root,
                }))),
                Some(mode.to_owned()),
            );
            let workdir = root.clone();
            Self {
                scratch,
                fs,
                gate: ObservedStateGate::new(),
                seams: ScriptSeams {
                    policy: Some(json!({
                        "mode": mode,
                        "workspace_root": root,
                    })),
                    workdir,
                },
            }
        }

        fn deps(&self) -> FsToolDeps<'_> {
            FsToolDeps {
                fs: &self.fs,
                gate: &self.gate,
                seams: &self.seams,
                advertised: false,
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
                serve(&self.deps(), method, &bytes).expect("the error body rides the transport");
            serde_json::from_slice(&reply).expect("error body parses")
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

    /// The M1 bundle ships the opt-in editor off: the registration
    /// gate answers false, so the default assembly never registers
    /// `str_replace_editor` (the 「替代编辑器默认不装配」 ruling).
    #[test]
    fn baked_bundle_leaves_the_editor_unregistered() {
        assert!(!str_replace_editor_registered());
    }

    /// The view family: the numbered file view, the window arm, the
    /// two-level directory listing, and the refusal texts.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the family pins six arms end to end over one shared fixture"
    )]
    fn view_family_pins_the_arms() {
        let fixture = Fixture::new("view", "danger-full-access");
        std::fs::write(fixture.scratch.path("a.txt"), "one\ntwo\nthree\n").expect("seed");
        let file_path = fixture.scratch.path("a.txt");
        let value = fixture.serve_ok(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({ "command": "view", "path": file_path })),
        )["value"]
            .clone();
        let text = value.as_str().expect("string");
        assert!(
            text.starts_with(&format!(
                "Here's the content of {file_path} with line numbers (which has a total of 4 \
lines):\n"
            )),
            "{text}"
        );
        assert!(
            text.contains("     1  one\n     2  two\n     3  three\n"),
            "{text}"
        );

        let windowed = fixture.serve_ok(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({
                "command": "view",
                "path": file_path,
                "view_range": [2, -1],
            })),
        )["value"]
            .clone();
        let text = windowed.as_str().expect("string");
        assert!(
            text.contains("with view_range=[2, -1]:\n     2  two\n     3  three"),
            "{text}"
        );

        // The directory listing: two levels, exclusions, sorted rows.
        std::fs::create_dir(fixture.scratch.path("sub")).expect("dir");
        std::fs::create_dir(fixture.scratch.path("sub/.hidden")).expect("hidden");
        std::fs::write(fixture.scratch.path("sub/z.txt"), "z\n").expect("seed");
        std::fs::create_dir(fixture.scratch.path("node_modules")).expect("nm");
        // The display path resolves without the trailing separator.
        let dir_path = fixture.scratch.path("").trim_end_matches('/').to_owned();
        let value = fixture.serve_ok(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({ "command": "view", "path": dir_path })),
        )["value"]
            .clone();
        let text = value.as_str().expect("string");
        assert!(
            text.starts_with(&format!(
                "Here're the files and directories up to 2 levels deep in {dir_path}, excluding \
hidden items, node_modules, and Python cache directories:\n"
            )),
            "{text}"
        );
        assert!(
            text.contains(&format!("d\t{}", fixture.scratch.path("sub"))),
            "{text}"
        );
        assert!(
            text.contains(&format!("f\t{}", fixture.scratch.path("sub/z.txt"))),
            "{text}"
        );
        // The header sentence itself names the exclusions, so the
        // assertions target the ROWS.
        assert!(
            !text.contains(&format!("d\t{}", fixture.scratch.path("node_modules"))),
            "{text}"
        );
        assert!(
            !text.contains(&format!("d\t{}", fixture.scratch.path("sub/.hidden"))),
            "{text}"
        );
        assert!(text.contains(&format!("d\t{dir_path}")), "{text}");

        // The refusal family: absent, directory view_range, non-file.
        let absent = fixture.serve_err(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({ "command": "view", "path": fixture.scratch.path("nope.txt") })),
        );
        assert_eq!(
            absent["error"]["message"],
            format!(
                "The path {} does not exist. Please provide a valid path.",
                fixture.scratch.path("nope.txt")
            )
        );
        assert_eq!(absent["error"]["code"], "FS_NOT_FOUND");
        let dir_range = fixture.serve_err(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({
                "command": "view",
                "path": fixture.scratch.path("sub"),
                "view_range": [1, 2],
            })),
        );
        assert_eq!(
            dir_range["error"]["message"],
            "The `view_range` parameter is not allowed when `path` points to a directory."
        );
        let relative = fixture.serve_err(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({ "command": "view", "path": "a.txt" })),
        );
        assert_eq!(
            relative["error"]["message"],
            "The path a.txt is not an absolute path, it should start with `/`. Maybe you meant \
/a.txt?"
        );
    }

    /// The create/`str_replace`/insert family: the success texts, the
    /// uniqueness refusal, the insert boundary, and the create-over
    /// refusal.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the family pins eight arms end to end over one shared fixture"
    )]
    fn mutations_pin_the_family() {
        let fixture = Fixture::new("mutate", "danger-full-access");
        let file_path = fixture.scratch.path("a.txt");

        // create over an existing path refuses before anything writes.
        std::fs::write(&file_path, "seed\n").expect("seed");
        let exists = fixture.serve_err(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({
                "command": "create",
                "path": file_path,
                "file_text": "x",
            })),
        );
        assert_eq!(
            exists["error"]["message"],
            format!(
                "File already exists at: {file_path}. Cannot overwrite files using command \
`create`."
            )
        );

        // str_replace requires a prior view (the shared gate).
        let unread = fixture.serve_err(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({
                "command": "str_replace",
                "path": file_path,
                "old_str": "seed",
                "new_str": "grown",
            })),
        );
        assert_eq!(
            unread["error"]["message"],
            format!("edit requires reading \"{file_path}\" first")
        );

        // The view unlocks the editor; the unique replace succeeds.
        let _ = fixture.serve_ok(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({ "command": "view", "path": file_path })),
        );
        let value = fixture.serve_ok(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({
                "command": "str_replace",
                "path": file_path,
                "old_str": "seed",
                "new_str": "grown",
            })),
        )["value"]
            .clone();
        assert_eq!(
            value,
            format!("The file {file_path} has been edited successfully.")
        );
        assert_eq!(
            std::fs::read_to_string(&file_path).expect("read"),
            "grown\n"
        );

        // A missing match refuses with the verbatim text.
        let _ = fixture.serve_ok(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({ "command": "view", "path": file_path })),
        );
        let missing = fixture.serve_err(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({
                "command": "str_replace",
                "path": file_path,
                "old_str": "nope",
                "new_str": "x",
            })),
        );
        assert_eq!(
            missing["error"]["message"],
            format!(
                "No replacement was performed, old_str `nope` did not appear verbatim in \
{file_path}."
            )
        );
        assert_eq!(missing["error"]["code"], "FS_EDIT_NOT_FOUND");

        // An ambiguous match carries the line table.
        std::fs::write(&file_path, "dup\ndup\n").expect("reseed");
        let _ = fixture.serve_ok(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({ "command": "view", "path": file_path })),
        );
        let ambiguous = fixture.serve_err(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({
                "command": "str_replace",
                "path": file_path,
                "old_str": "dup",
                "new_str": "x",
            })),
        );
        assert_eq!(
            ambiguous["error"]["message"],
            "No replacement was performed. Multiple occurrences of old_str `dup` in lines \
[1, 2]. Please ensure it is unique"
        );
        assert_eq!(ambiguous["error"]["code"], "FS_AMBIGUOUS_EDIT");

        // insert at line 1 splices after the first line.
        std::fs::write(&file_path, "one\ntwo\n").expect("reseed");
        let _ = fixture.serve_ok(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({ "command": "view", "path": file_path })),
        );
        let value = fixture.serve_ok(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({
                "command": "insert",
                "path": file_path,
                "insert_line": 1,
                "new_str": "middle",
            })),
        )["value"]
            .clone();
        assert_eq!(
            value,
            format!("The file {file_path} has been edited successfully.")
        );
        assert_eq!(
            std::fs::read_to_string(&file_path).expect("read"),
            "one\nmiddle\ntwo\n"
        );

        // The insert boundary refuses past the file end.
        let _ = fixture.serve_ok(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({ "command": "view", "path": file_path })),
        );
        let over = fixture.serve_err(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({
                "command": "insert",
                "path": file_path,
                "insert_line": 9,
                "new_str": "x",
            })),
        );
        assert_eq!(
            over["error"]["message"],
            "Invalid `insert_line` parameter: 9. It should be within the range of lines of \
the file: [0, 4]"
        );

        // create on a fresh path writes through the gate's create
        // intent.
        let fresh_path = fixture.scratch.path("fresh.txt");
        let value = fixture.serve_ok(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({
                "command": "create",
                "path": fresh_path,
                "file_text": "body",
            })),
        )["value"]
            .clone();
        assert_eq!(
            value,
            format!("New file created successfully at: {fresh_path}")
        );
        assert_eq!(std::fs::read_to_string(&fresh_path).expect("read"), "body");
    }

    /// The clipped view: the budget counts UTF-16 units and the cut
    /// never splits a surrogate pair; the marker rides verbatim.
    #[test]
    fn maybe_truncate_pins_the_budget() {
        assert_eq!(maybe_truncate("short", 100), "short");
        let clipped = maybe_truncate("abcdef", 3);
        assert!(clipped.starts_with("abc"), "{clipped}");
        assert!(clipped.contains(TRUNCATED_MESSAGE), "{clipped}");
        // The astral char (two UTF-16 units) never splits.
        let astral = "a\u{1F600}b";
        let clipped = maybe_truncate(astral, 2);
        assert!(clipped.starts_with('a'), "{clipped}");
    }

    /// The pending-call cards: the four arms (view read card, the two
    /// diff cards, the insert edit card with the line anchor).
    #[test]
    fn present_call_pins_the_four_arms() {
        let view = present_call(&json!({ "command": "view", "path": "/a.txt" }));
        assert_eq!(view["card"], "generic");
        assert_eq!(view["title"], "view /a.txt");
        assert_eq!(view["kind"], "read");
        let create = present_call(&json!({
            "command": "create",
            "path": "/a.txt",
            "file_text": "body",
        }));
        assert_eq!(create["card"], "diff");
        assert_eq!(create["title"], "create /a.txt");
        assert_eq!(create["diffs"][0]["oldText"], Value::Null);
        assert_eq!(create["diffs"][0]["newText"], "body");
        let replace = present_call(&json!({
            "command": "str_replace",
            "path": "/a.txt",
            "old_str": "old",
            "new_str": "new",
        }));
        assert_eq!(replace["card"], "diff");
        assert_eq!(replace["diffs"][0]["oldText"], "old");
        let replace_null = present_call(&json!({
            "command": "str_replace",
            "path": "/a.txt",
            "old_str": null,
            "new_str": null,
        }));
        assert_eq!(replace_null["diffs"][0]["oldText"], Value::Null);
        assert_eq!(replace_null["diffs"][0]["newText"], "");
        let insert = present_call(&json!({
            "command": "insert",
            "path": "/a.txt",
            "insert_line": 4,
        }));
        assert_eq!(insert["card"], "generic");
        assert_eq!(insert["kind"], "edit");
        assert_eq!(insert["title"], "insert /a.txt");
        assert_eq!(insert["locations"][0]["line"], 5);
        let insert_zero = present_call(&json!({
            "command": "insert",
            "path": "/a.txt",
            "insert_line": 0,
        }));
        assert_eq!(insert_zero["locations"][0]["line"], 1);
    }

    /// The required-parameter refusals ride the dsh texts.
    #[test]
    fn required_parameter_refusals_pin_the_texts() {
        let fixture = Fixture::new("required", "danger-full-access");
        let file_path = fixture.scratch.path("a.txt");
        std::fs::write(&file_path, "x\n").expect("seed");
        let _ = fixture.serve_ok(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({ "command": "view", "path": file_path })),
        );
        let no_old = fixture.serve_err(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({
                "command": "str_replace",
                "path": file_path,
                "new_str": "y",
            })),
        );
        assert_eq!(
            no_old["error"]["message"],
            "Parameter `old_str` is required for command: str_replace"
        );
        let empty_old = fixture.serve_err(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({
                "command": "str_replace",
                "path": file_path,
                "old_str": "",
                "new_str": "y",
            })),
        );
        assert_eq!(
            empty_old["error"]["message"],
            "Parameter `old_str` is empty for command: str_replace"
        );
        let explicit_null = fixture.serve_err(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({
                "command": "str_replace",
                "path": file_path,
                "old_str": "x",
                "new_str": null,
            })),
        );
        assert_eq!(
            explicit_null["error"]["message"],
            "Parameter `new_str` must be omitted or contain a string for command: str_replace"
        );
        let no_insert_line = fixture.serve_err(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({
                "command": "insert",
                "path": file_path,
                "new_str": "y",
            })),
        );
        assert_eq!(
            no_insert_line["error"]["message"],
            "Parameter `insert_line` is required for command: insert"
        );
    }

    /// The read-only fence denies a mutation with the bare denial
    /// marker (no escalation hint — the `MutationPolicy` simple form).
    #[test]
    fn read_only_fence_denies_with_the_bare_marker() {
        let fixture = Fixture::new("denied", "read-only");
        let fresh_path = fixture.scratch.path("fresh.txt");
        let denied = fixture.serve_err(
            METHOD_STR_REPLACE_EDITOR_EXECUTE,
            &call_frame(&json!({
                "command": "create",
                "path": fresh_path,
                "file_text": "x",
            })),
        );
        let message = denied["error"]["message"].as_str().expect("message");
        assert_eq!(
            message, "[sandbox: file access denied under read-only mode]",
            "{message}"
        );
        assert_eq!(denied["error"]["code"], "FS_SANDBOX_DENIED");
    }
}
