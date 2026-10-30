// Rust guideline compliant 2026-10-30
//! Shared execution plumbing for the `glob`/`grep` search tools (dsh
//! `tool-fs-search/src/search-core.ts`).
//!
//! The module owns the package `SEARCH_*` error vocabulary, one spawn
//! helper that runs ripgrep with a plain argv vector and returns
//! complete raw stdout, workdir-relative path display, the head
//! retention the renders and the search cards share, and the
//! best-effort formatted-result spill handoff.
//!
//! Both tools execute as ordinary foreground spawns through the fr-shell
//! subprocess face — never a shell job, never a model-visible background
//! task. The ripgrep binary resolves per call (the fr packaging
//! judgment): `FR_RIPGREP` names the binary when set (a set-but-invalid
//! value refuses fail-closed), else a bare PATH resolution; dsh resolves
//! its packaged binary once per process. Raw `rg` stdout is an internal
//! transport detail: the tools request a per-run stdout capture budget,
//! parse only complete in-memory stdout within `rawOutputMaxBytes`, and
//! never read spill files. The model-facing recovery artifact is the
//! formatted result saved through the spill-store seam.
//!
//! The dsh entry abort pre-check (`exec.signal.aborted` before the
//! spawn) has no fr body-side analog: the signal cannot cross the wire
//! (the F3 ruling) and the dispatch gate refuses pre-aborted calls as
//! `ABORTED_BEFORE_DISPATCH`. The cooperative timeout and caller
//! cancellation reach the body through the kill carrier instead: the
//! per-call abort watcher kills the bound subprocess, the wait reply
//! carries the kill cause, and the run maps it to the dsh `SEARCH_ABORTED`
//! text verbatim.

use serde_json::Value;
use serde_json::json;

use crate::tools::BodyError;
use crate::tools::Seams;

/// Default cap on the complete raw `rg` stdout the tools will parse (the
/// `raw-output-max-bytes` config), matching Claude Code's ripgrep raw
/// buffer.
pub const RAW_OUTPUT_MAX_BYTES: usize = 20_000_000;

/// Default cooperative tool-call timeout budget in milliseconds (the
/// `timeout-ms` config), attached to both tool definitions for the
/// cooperative timeout guard to enforce.
pub const SEARCH_TIMEOUT_MS: f64 = 30_000.0;

/// Default cap in bytes on the retained stderr tail of one search run —
/// a diagnostic excerpt only (the tool never reads a stderr spill path,
/// and the collect disposition requests none).
pub const SEARCH_STDERR_MAX_BYTES: usize = 64 * 1024;

/// Default terminate grace period for a search process (ms).
pub const SEARCH_GRACE_MS: f64 = 3_000.0;

/// Default cap in bytes on one search's serialized `presentationMeta`
/// (the `search-meta-max-bytes` config).
pub const SEARCH_META_MAX_BYTES: usize = 65_536;

/// The resolved search caps (the dsh bundle values baked at build time;
/// the `ExecutorConfigs::bundle()` judgment).
///
/// The `sample-over-cap-glob-results`, `glob-max-results`,
/// `grep-max-matches`, `grep-max-line-bytes`, `search-meta-max-bytes`,
/// `raw-output-max-bytes`, `grace-ms`, `stderr-max-bytes`, and
/// `timeout-ms` config keys carry these values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SearchCaps {
    /// Max paths retained inline by one `glob` call.
    pub glob_max_results: usize,
    /// Whether an over-cap `glob` page is sampled across top-level
    /// entries instead of taking the modification-time head.
    pub sample_over_cap_glob_results: bool,
    /// Max flat matches retained inline by one `grep` call.
    pub grep_max_matches: usize,
    /// Max bytes retained per matched-line preview.
    pub grep_max_line_bytes: usize,
    /// Max bytes of serialized `presentationMeta`.
    pub search_meta_max_bytes: usize,
    /// Cap on the complete raw `rg` stdout the tool will parse.
    pub raw_output_max_bytes: usize,
    /// Terminate-escalation grace period (ms).
    pub grace_ms: f64,
    /// Cap on the retained stderr diagnostic tail.
    pub stderr_max_bytes: usize,
    /// Cooperative tool-call budget (ms) on both tool definitions.
    pub timeout_ms: f64,
}

/// The baked caps (the dsh bundle row: `sampleOverCapGlobResults: false`
/// with every remaining field at its package default).
#[must_use]
pub const fn baked_caps() -> SearchCaps {
    SearchCaps {
        glob_max_results: 100,
        sample_over_cap_glob_results: false,
        grep_max_matches: 250,
        grep_max_line_bytes: 2000,
        search_meta_max_bytes: SEARCH_META_MAX_BYTES,
        raw_output_max_bytes: RAW_OUTPUT_MAX_BYTES,
        grace_ms: SEARCH_GRACE_MS,
        stderr_max_bytes: SEARCH_STDERR_MAX_BYTES,
        timeout_ms: SEARCH_TIMEOUT_MS,
    }
}

/// The execution identity one search body runs through (the fr-side
/// shape of the dsh `ToolExecution` fields the bodies consume).
#[derive(Debug, Clone)]
pub struct SearchExec {
    /// The serving tool's registered name (the dsh `exec.name`).
    pub tool_name: String,
    /// Model-issued call identity (the spawn's kill-carrier binding).
    pub call_id: String,
    /// Root model-requested call.
    pub root_call_id: String,
    /// The owning agent (the session workdir and the spill owner); the
    /// dsh agent-less calls run from the process directory and spill
    /// nothing.
    pub agent: Option<String>,
}

/// One completed `rg` acquisition: complete stdout plus the resolved
/// workdir (dsh `RipgrepRun`).
#[derive(Debug)]
pub struct RipgrepRun {
    /// Complete raw stdout retained by the subprocess seam within the
    /// requested cap.
    pub stdout: String,
    /// True when ripgrep exited 1: a successful search with zero results.
    pub no_matches: bool,
    /// The resolved working directory (the display-relativization base).
    pub workdir: String,
}

/// Maps a search failure onto the body failure (dsh `SearchError`
/// extends `HarnessError`, so the code stays routable and the class
/// name rides as `SearchError`).
#[must_use]
pub fn search_error(message: impl Into<String>, code: &str) -> BodyError {
    BodyError {
        code: Some(code.to_owned()),
        name: "SearchError".to_owned(),
        message: message.into(),
    }
}

/// The retained stderr tail as a diagnostic excerpt, with a truncation
/// note when the subprocess seam dropped bytes (dsh `stderrExcerpt`).
#[must_use]
pub fn stderr_excerpt(stderr_text: &str, truncated: bool) -> String {
    let text = stderr_text.trim();
    if text.is_empty() {
        return String::new();
    }
    if truncated {
        format!("{text} [stderr truncated]")
    } else {
        text.to_owned()
    }
}

/// Classifies a nonzero-exit `rg` run into the search error vocabulary
/// (dsh `classifyRunFailure`).
///
/// There is no shell layer, so an exit 127 or shell "command not found"
/// text cannot occur — a launch failure rejects at spawn.
#[must_use]
pub fn classify_run_failure(
    tool_name: &str,
    exit_code: i64,
    stderr_text: &str,
    stderr_truncated: bool,
) -> BodyError {
    let stderr = stderr_excerpt(stderr_text, stderr_truncated);
    let lower = stderr.to_lowercase();
    if lower.contains("regex parse error") || lower.contains("error parsing glob") {
        return search_error(
            format!("{tool_name} pattern rejected by ripgrep: {stderr}"),
            "SEARCH_INVALID_PATTERN",
        );
    }
    if stderr.is_empty() {
        search_error(
            format!("{tool_name} search failed (exit {exit_code})"),
            "SEARCH_FAILED",
        )
    } else {
        search_error(
            format!("{tool_name} search failed (exit {exit_code}): {stderr}"),
            "SEARCH_FAILED",
        )
    }
}

/// Acquires the COMPLETE raw stdout of a finished run, enforcing
/// `rawOutputMaxBytes` on the in-memory transport (dsh
/// `completeStdout`).
///
/// A truncated result means the subprocess seam could not retain
/// complete stdout within the requested budget, so the tool fails
/// clearly instead of parsing a silently-partial stream.
///
/// # Errors
///
/// The two `SEARCH_RAW_OUTPUT_OVERFLOW` refusals verbatim.
pub fn complete_stdout(
    tool_name: &str,
    stdout: &Value,
    raw_output_max_bytes: usize,
) -> Result<String, BodyError> {
    let narrow = "narrow pattern, path, or include and retry";
    let text = stdout
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let lossy = stdout.get("lossy").and_then(Value::as_bool) == Some(true);
    if !lossy {
        let inline_bytes = text.len();
        if inline_bytes > raw_output_max_bytes {
            return Err(search_error(
                format!(
                    "{tool_name} produced {inline_bytes} bytes of raw output, over the \
{raw_output_max_bytes}-byte cap; {narrow}"
                ),
                "SEARCH_RAW_OUTPUT_OVERFLOW",
            ));
        }
        return Ok(text.to_owned());
    }
    Err(search_error(
        format!(
            "{tool_name} produced more raw output than the subprocess seam retained within the \
{raw_output_max_bytes}-byte cap; {narrow}"
        ),
        "SEARCH_RAW_OUTPUT_OVERFLOW",
    ))
}

/// Resolves the ripgrep binary path (the fr packaging judgment).
///
/// The `FR_RIPGREP` pointer wins when set (fail-closed on an invalid
/// value); a bare PATH resolution runs otherwise.
///
/// # Errors
///
/// The resolution failure (the dsh launch-failure classification
/// applies at the caller).
pub fn resolve_rg_path(seams: &dyn Seams) -> Result<String, String> {
    match std::env::var("FR_RIPGREP") {
        Ok(pointer) if !pointer.trim().is_empty() => seams.subprocess_resolve_executable(&pointer),
        _ => seams.subprocess_resolve_executable("rg"),
    }
}

/// Runs ripgrep with a plain argv vector and returns its complete raw
/// stdout (dsh `runRipgrep`).
///
/// The working directory is the calling agent's session current
/// directory when available, else the process directory. The spawn
/// binds the tool call, so the cooperative timeout and caller
/// cancellation terminate the process through the kill carrier; a
/// settlement carrying that cause maps to the dsh `SEARCH_ABORTED` text
/// verbatim.
///
/// # Errors
///
/// The dsh refusal family: the launch failure, the provider failure,
/// the kill-cause abort, the killed-by-signal classification, the exit
/// classification, and the raw-output overflow.
#[expect(
    clippy::too_many_lines,
    reason = "the run mirrors the dsh pipeline one arm per refusal; splitting it would hide the correspondence"
)]
pub fn run_ripgrep(
    seams: &dyn Seams,
    exec: &SearchExec,
    tool_name: &str,
    argv: &[String],
    caps: &SearchCaps,
) -> Result<RipgrepRun, BodyError> {
    let workdir = match &exec.agent {
        None => std::env::current_dir()
            .map_err(|error| BodyError::plain(error.to_string()))?
            .to_string_lossy()
            .into_owned(),
        Some(agent) => seams
            .working_directory_ensure(agent)
            .map_err(BodyError::plain)?,
    };
    let rg_path = resolve_rg_path(seams).map_err(|error| {
        let _ = error;
        search_error(
            format!("{tool_name} could not start its search command (ripgrep launch failed)"),
            "SEARCH_FAILED",
        )
    })?;
    let mut full_argv = vec![rg_path, "--no-config".to_owned()];
    full_argv.extend(argv.iter().cloned());
    let spec = json!({
        "argv": full_argv,
        "cwd": workdir,
        "stdio": {
            "stdin": "ignore",
            "stdout": { "max_bytes": caps.raw_output_max_bytes },
            "stderr": { "max_bytes": caps.stderr_max_bytes },
        },
        "grace_ms": caps.grace_ms,
        "tool_call": {
            "call_id": exec.call_id,
            "root_call_id": exec.root_call_id,
        },
    });
    let handle = seams.subprocess_spawn(&spec).map_err(|error| {
        // The verbatim dsh text omits the cause (the chained invisible
        // field); the transport text stays a local diagnostic.
        let _ = error;
        search_error(
            format!("{tool_name} could not start its search command (ripgrep launch failed)"),
            "SEARCH_FAILED",
        )
    })?;
    // The bounded wait slices: the instance lock frees between slices so
    // the host-side kill carrier lands (the jobs wait judgment).
    let outcome = loop {
        let reply = seams.subprocess_wait(&handle, 200.0).map_err(|error| {
            let _ = error;
            search_error(
                format!(
                    "{tool_name} subprocess failed before reporting an outcome \
(ripgrep provider failure)"
                ),
                "SEARCH_FAILED",
            )
        })?;
        if reply.get("settled").and_then(Value::as_bool) == Some(true) {
            break reply;
        }
    };
    let read_stream = |stream: &str| {
        seams.subprocess_read(&handle, stream, 0).map_err(|error| {
            let _ = error;
            search_error(
                format!("{tool_name} search command produced no collected output streams"),
                "SEARCH_FAILED",
            )
        })
    };
    let stdout = read_stream("stdout")?;
    let stderr = read_stream("stderr")?;
    // The kill cause first: the carrier's TERM lands as a signal
    // settlement, but the cause (not the signal word) distinguishes the
    // dsh abort re-check from an external kill.
    if let Some(reason) = outcome.get("kill_reason").and_then(Value::as_str) {
        let _ = reason;
        return Err(search_error(
            format!(
                "{tool_name} was aborted before completion (tool timeout or caller \
cancellation)"
            ),
            "SEARCH_ABORTED",
        ));
    }
    let signal = outcome.get("signal").and_then(Value::as_str);
    let exit_code = outcome.get("exit_code").and_then(Value::as_i64);
    if signal.is_some() || exit_code.is_none() {
        let word = signal.unwrap_or("(unknown)");
        return Err(search_error(
            format!("{tool_name} search command was killed by signal {word}"),
            "SEARCH_FAILED",
        ));
    }
    let exit_code = exit_code.unwrap_or_default();
    if exit_code != 0 && exit_code != 1 {
        let stderr_text = stderr
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let stderr_lossy = stderr.get("lossy").and_then(Value::as_bool) == Some(true);
        return Err(classify_run_failure(
            tool_name,
            exit_code,
            stderr_text,
            stderr_lossy,
        ));
    }
    let stdout_text = complete_stdout(tool_name, &stdout, caps.raw_output_max_bytes)?;
    Ok(RipgrepRun {
        stdout: stdout_text,
        no_matches: exit_code == 1,
        workdir,
    })
}

/// Maps an `rg` output path to its display form (dsh
/// `toWorkdirRelative`): absolute paths inside the resolved workdir
/// become workdir-relative; everything else passes through unchanged.
#[must_use]
pub fn to_workdir_relative(path: &str, workdir: &str) -> String {
    if !std::path::Path::new(path).is_absolute() {
        return path.to_owned();
    }
    let rel = relative(workdir, path);
    if rel.is_empty() {
        return ".".to_owned();
    }
    let escape = format!("..{}", std::path::MAIN_SEPARATOR);
    if rel == ".." || rel.starts_with(&escape) {
        return path.to_owned();
    }
    rel
}

/// The component-walk `relative(from, to)` (the node path.relative
/// semantics the display rule consumes): both sides resolve lexically
/// first (the node `path.resolve` normalization — `.` segments drop,
/// `..` climbs), then the result joins `..` climbs over the `from`
/// remainder with the `to` remainder.
fn relative(from: &str, to: &str) -> String {
    let from_parts = parts(from);
    let to_parts = parts(to);
    let mut split = 0_usize;
    while split < from_parts.len() && split < to_parts.len() && from_parts[split] == to_parts[split]
    {
        split += 1;
    }
    let mut segments: Vec<String> = from_parts[split..]
        .iter()
        .map(|_| "..".to_owned())
        .collect();
    segments.extend(to_parts[split..].iter().cloned());
    segments.join(std::path::MAIN_SEPARATOR_STR)
}

/// Lexically resolves one path into its normal components (the node
/// `path.resolve` normalization): `.` drops, `..` climbs.
fn parts(path: &str) -> Vec<String> {
    let mut segments: Vec<String> = Vec::new();
    for component in std::path::Path::new(path).components() {
        match component {
            std::path::Component::RootDir => segments.clear(),
            std::path::Component::CurDir | std::path::Component::Prefix(_) => {}
            std::path::Component::ParentDir => {
                segments.pop();
            }
            std::path::Component::Normal(part) => {
                segments.push(part.to_string_lossy().into_owned());
            }
        }
    }
    segments
}

/// One parsed match (dsh `GrepMatch`): the file, the 1-based line
/// number, and the (possibly previewed) line text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepMatch {
    /// The display path as ripgrep printed it.
    pub path: String,
    /// The 1-based line number.
    pub line_number: u64,
    /// The (possibly previewed) line text.
    pub line: String,
}

/// Bounds one matched-line preview to `max_bytes` (UTF-8 boundary
/// preserved) and marks the cut (dsh `previewLine` over the output
/// retention head text).
///
/// The cap is a per-line budget fact; the complete line stays in the
/// searched file for `read`.
#[must_use]
pub fn preview_line(line: &str, max_bytes: usize) -> String {
    let (kept, truncated) = head_text(line, max_bytes);
    if truncated {
        format!("{kept} (line truncated)")
    } else {
        kept
    }
}

/// The head retention over one text (the dsh `TextRetainer` head over
/// `max_bytes`): a byte-exact head cut on a UTF-8 char boundary plus the
/// cut flag.
#[must_use]
pub fn head_text(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_owned(), false);
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    (text[..boundary].to_owned(), true)
}

/// The head retention over one item list (the dsh `ItemRetainer` head
/// over `max_items`).
///
/// Returns the kept page, whether the complete result was capped, and
/// the pre-cap total.
#[must_use]
pub fn head_items<T>(items: impl IntoIterator<Item = T>, max_items: usize) -> (Vec<T>, bool, usize)
where
    T: Send,
{
    let mut seen = 0_usize;
    let mut kept = Vec::new();
    for item in items {
        seen += 1;
        if kept.len() < max_items {
            kept.push(item);
        }
    }
    let truncated = seen > max_items;
    (kept, truncated, seen)
}

/// Best-effort save of one COMPLETE formatted search result through the
/// spill-store seam (dsh `trySaveFormattedResult`) — the model-facing
/// recovery path for a capped result.
///
/// A call with no session owner, a missing backend, or a storage
/// rejection logs a warning and returns `None` — the caller keeps the
/// inline result and reports that the complete result could not be
/// saved; search success never turns into an error because spill
/// storage is unavailable (the three dsh warn arms verbatim; the fr
/// diagnostic channel is the process stderr).
pub fn try_save_formatted_result(
    seams: &dyn Seams,
    exec: &SearchExec,
    suggested_name: &str,
    content: &str,
) -> Option<Value> {
    let Some(session_id) = exec.agent.as_deref() else {
        eprintln!(
            "tool-fs-search: no session owner for {} result; complete result not saved",
            exec.name()
        );
        return None;
    };
    let save = json!({
        "owner": { "sessionId": session_id },
        "source": {
            "kind": "tool",
            "toolName": exec.name(),
            "callId": exec.call_id,
            "label": "result",
        },
        "suggestedName": suggested_name,
        "content": content,
    });
    match seams.spill_store_save_text(&save) {
        Ok(Some(reference)) => Some(reference),
        Ok(None) => {
            eprintln!(
                "tool-fs-search: no ctx.spillStore backend loaded; complete {} result not saved",
                exec.name()
            );
            None
        }
        Err(error) => {
            eprintln!(
                "tool-fs-search: saveText failed for {}: {error}; complete result not saved",
                exec.name()
            );
            None
        }
    }
}

impl SearchExec {
    /// The tool name the dsh warn arms interpolate (the dsh `exec.name`).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.tool_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The tool name rides the frame.
    #[test]
    fn exec_name_carries_the_tool() {
        let exec = SearchExec {
            tool_name: "glob".to_owned(),
            call_id: "c1".to_owned(),
            root_call_id: "c1".to_owned(),
            agent: Some("s1".to_owned()),
        };
        assert_eq!(exec.name(), "glob");
    }

    /// The stderr excerpt trims, and the truncation note rides only on a
    /// dropped-bytes tail.
    #[test]
    fn stderr_excerpt_pins_the_tail_shapes() {
        assert_eq!(stderr_excerpt("  \n", false), "");
        assert_eq!(stderr_excerpt(" boom ", false), "boom");
        assert_eq!(stderr_excerpt("boom", true), "boom [stderr truncated]");
    }

    /// The exit classification splits the invalid-pattern family from
    /// the generic failure, embedding the stderr excerpt verbatim.
    #[test]
    fn classify_run_failure_splits_the_families() {
        let pattern = classify_run_failure("grep", 2, "Regex parse error: unclosed group", false);
        assert_eq!(pattern.code.as_deref(), Some("SEARCH_INVALID_PATTERN"));
        assert_eq!(
            pattern.message,
            "grep pattern rejected by ripgrep: Regex parse error: unclosed group"
        );
        let failed = classify_run_failure("glob", 2, "boom", true);
        assert_eq!(failed.code.as_deref(), Some("SEARCH_FAILED"));
        assert_eq!(
            failed.message,
            "glob search failed (exit 2): boom [stderr truncated]"
        );
        let quiet = classify_run_failure("glob", 2, "", false);
        assert_eq!(quiet.message, "glob search failed (exit 2)");
    }

    /// The raw-output overflow splits the inline-over-cap arm from the
    /// truncated-at-the-seam arm (the two dsh texts).
    #[test]
    fn complete_stdout_pins_the_two_overflow_arms() {
        let ok = complete_stdout("glob", &json!({ "text": "a\nb", "lossy": false }), 100);
        assert_eq!(ok.expect("ok"), "a\nb");
        let over = complete_stdout("glob", &json!({ "text": "abcdef", "lossy": false }), 3)
            .expect_err("inline over the cap overflows");
        assert_eq!(over.code.as_deref(), Some("SEARCH_RAW_OUTPUT_OVERFLOW"));
        assert_eq!(
            over.message,
            "glob produced 6 bytes of raw output, over the 3-byte cap; narrow pattern, path, \
or include and retry"
        );
        let lossy = complete_stdout("grep", &json!({ "text": "tail", "lossy": true }), 100)
            .expect_err("a lossy read overflows");
        assert_eq!(lossy.code.as_deref(), Some("SEARCH_RAW_OUTPUT_OVERFLOW"));
        assert_eq!(
            lossy.message,
            "grep produced more raw output than the subprocess seam retained within the 100-byte \
cap; narrow pattern, path, or include and retry"
        );
    }

    /// The workdir display rule relativizes only absolute paths inside
    /// the workdir; escapes and outside paths pass through unchanged.
    #[test]
    fn workdir_relative_pins_the_display_rule() {
        let workdir = "/repo";
        assert_eq!(to_workdir_relative("src/a.rs", workdir), "src/a.rs");
        assert_eq!(to_workdir_relative("/repo/src/a.rs", workdir), "src/a.rs");
        assert_eq!(to_workdir_relative("/repo", workdir), ".");
        assert_eq!(
            to_workdir_relative("/elsewhere/a.rs", workdir),
            "/elsewhere/a.rs"
        );
        assert_eq!(
            to_workdir_relative("/repo/../above.rs", workdir),
            "/repo/../above.rs"
        );
        let deep = "/repo/sub";
        // A sibling outside the workdir escapes, so the display passes
        // the original through (the dsh `..` escape check).
        assert_eq!(to_workdir_relative("/repo/a.rs", deep), "/repo/a.rs");
        assert_eq!(to_workdir_relative("/repo/sub/a.rs", deep), "a.rs");
    }

    /// The head retention cuts on char boundaries and reports the cut.
    #[test]
    fn head_text_cuts_on_char_boundaries() {
        assert_eq!(head_text("short", 100), ("short".to_owned(), false));
        let (kept, truncated) = head_text("abc", 2);
        assert_eq!((kept.as_str(), truncated), ("ab", true));
        let emoji = "a\u{1F600}b";
        let (kept, truncated) = head_text(emoji, 2);
        // The four-byte emoji never fits in a two-byte budget: the cut
        // backs off to the ASCII prefix.
        assert_eq!((kept.as_str(), truncated), ("a", true));
    }

    /// The line preview carries the truncation marker.
    #[test]
    fn preview_line_marks_the_cut() {
        assert_eq!(preview_line("ok", 10), "ok");
        assert_eq!(preview_line("abcdef", 3), "abc (line truncated)");
    }

    /// The item retention keeps the head, the pre-cap total, and the
    /// cap flag.
    #[test]
    fn head_items_keeps_the_head_and_the_total() {
        let (kept, truncated, seen) = head_items(["a", "b", "c"], 2);
        assert_eq!(kept, vec!["a", "b"]);
        assert!(truncated);
        assert_eq!(seen, 3);
        let (kept, truncated, seen) = head_items(["a"], 2);
        assert_eq!(kept, vec!["a"]);
        assert!(!truncated);
        assert_eq!(seen, 1);
    }
}
