// Rust guideline compliant 2026-10-30
//! The `glob` and `grep` search tools (dsh `tool-fs-search/src/glob.ts`
//! and `grep.ts`).
//!
//! The module owns the model-facing schemas' value constraints, argv
//! construction, result parsing, inline sampling, formatting, the
//! pending/completed presentations, and the capped-result spill
//! listener over the post-execute CPS protocol.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::Value;
use serde_json::json;

use crate::catalog_gen;
use crate::search_core;
use crate::search_core::GrepMatch;
use crate::search_core::RipgrepRun;
use crate::search_core::SearchCaps;
use crate::search_core::SearchExec;
use crate::search_core::search_error;
use crate::search_views;
use crate::service::METHOD_GLOB_EXECUTE;
use crate::service::METHOD_GLOB_PRESENT_CALL;
use crate::service::METHOD_GLOB_PRESENT_RESULT;
use crate::service::METHOD_GLOB_PRESENTATION_META;
use crate::service::METHOD_GLOB_RENDER;
use crate::service::METHOD_GLOB_SPILL;
use crate::service::METHOD_GREP_EXECUTE;
use crate::service::METHOD_GREP_PRESENT_CALL;
use crate::service::METHOD_GREP_PRESENT_RESULT;
use crate::service::METHOD_GREP_PRESENTATION_META;
use crate::service::METHOD_GREP_RENDER;
use crate::service::METHOD_GREP_SPILL;
use crate::tools::BodyError;
use crate::tools::Seams;

/// The composed runtime the search bands dispatch against.
///
/// The seams carry the cross-plugin consumption; the per-call stash is
/// what the spill listeners pair across the CPS legs (the enter leg
/// parks the call and the canonical result; the exit leg consumes
/// them).
pub struct SearchToolDeps<'a> {
    /// The cross-plugin seam (subprocess, workdir, spill store).
    pub seams: &'a dyn Seams,
    /// The CPS pairing stash (the enter/exit token legs).
    pub pending: &'a Mutex<HashMap<u64, (Value, Value)>>,
}

impl std::fmt::Debug for SearchToolDeps<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchToolDeps").finish_non_exhaustive()
    }
}

/// The search tool variant one serve method dispatches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolKind {
    Glob,
    Grep,
}

/// The word a string argument arrives as (absent keys stay empty).
fn word<'a>(arguments: &'a Value, key: &str) -> &'a str {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// Validates value constraints the schema cannot express (dsh
/// `parseGlobArgs`): a non-blank `pattern`, and a non-blank `path` when
/// given.
///
/// # Errors
///
/// The dsh refusal texts verbatim.
pub fn parse_glob_args(arguments: &Value) -> Result<(String, Option<String>), String> {
    let pattern = word(arguments, "pattern");
    if pattern.trim().is_empty() {
        return Err("pattern must be a non-empty string".to_owned());
    }
    let path = match arguments.get("path") {
        None => None,
        Some(value) => {
            let path = value.as_str().unwrap_or_default();
            if path.trim().is_empty() {
                return Err("path must be a non-empty string when given".to_owned());
            }
            Some(path.to_owned())
        }
    };
    Ok((pattern.to_owned(), path))
}

/// Validates value constraints the schema cannot express (dsh
/// `parseGrepArgs`): a non-EMPTY `pattern` (whitespace is a legitimate
/// regex), a non-blank `path` when given, and a single positive
/// `include` glob.
///
/// # Errors
///
/// The dsh refusal texts verbatim.
pub fn parse_grep_args(
    arguments: &Value,
) -> Result<(String, Option<String>, Option<String>), String> {
    let pattern = word(arguments, "pattern");
    if pattern.is_empty() {
        return Err("pattern must be a non-empty string".to_owned());
    }
    let path = match arguments.get("path") {
        None => None,
        Some(value) => {
            let path = value.as_str().unwrap_or_default();
            if path.trim().is_empty() {
                return Err("path must be a non-empty string when given".to_owned());
            }
            Some(path.to_owned())
        }
    };
    let include = match arguments.get("include") {
        None => None,
        Some(value) => {
            let include = value.as_str().unwrap_or_default();
            validate_include(include)?;
            Some(include.to_owned())
        }
    };
    Ok((pattern.to_owned(), path, include))
}

/// Rejects an `include` that is not ONE positive glob filter (dsh
/// `validateInclude`): blank strings, negated patterns, and
/// comma-separated lists. A comma inside a brace group is fine.
fn validate_include(include: &str) -> Result<(), String> {
    if include.trim().is_empty() {
        return Err("include must be a non-empty glob when given".to_owned());
    }
    if include.starts_with('!') {
        return Err(
            "include must be a positive glob filter; negated patterns (\"!…\") are not supported"
                .to_owned(),
        );
    }
    let mut brace_depth = 0_i32;
    for char in include.chars() {
        match char {
            '{' => brace_depth += 1,
            '}' => brace_depth = brace_depth.saturating_sub(1),
            ',' if brace_depth == 0 => {
                return Err(
                    "include must be one glob, not a comma-separated list (use {a,b} \
alternation instead)"
                        .to_owned(),
                );
            }
            _ => {}
        }
    }
    Ok(())
}

/// Builds the fixed `rg --files` argv for one `glob` call (dsh
/// `buildGlobCommand`).
///
/// The search root rides behind `--` so a leading-dash path can never
/// be parsed as a flag; the VCS excludes ride TWO negated globs per
/// name.
#[must_use]
pub fn build_glob_command(pattern: &str, path: Option<&str>) -> Vec<String> {
    const GLOB_VCS_EXCLUDES: [&str; 6] = [".git", ".svn", ".hg", ".bzr", ".jj", ".sl"];
    let mut parts = vec![
        "--files".to_owned(),
        format!("--glob={pattern}"),
        "--sort=modified".to_owned(),
        "--no-ignore".to_owned(),
        "--hidden".to_owned(),
    ];
    for name in GLOB_VCS_EXCLUDES {
        parts.push(format!("--glob=!**/{name}"));
        parts.push(format!("--glob=!**/{name}/**"));
    }
    if let Some(path) = path {
        parts.push("--".to_owned());
        parts.push(path.to_owned());
    }
    parts
}

/// Builds the fixed line-oriented `rg --json` argv for one `grep` call
/// (dsh `buildGrepCommand`): the pattern and include ride in
/// `--flag=value` form and the target behind `--`.
#[must_use]
pub fn build_grep_command(pattern: &str, path: Option<&str>, include: Option<&str>) -> Vec<String> {
    let mut parts = vec!["--json".to_owned(), format!("--regexp={pattern}")];
    if let Some(include) = include {
        parts.push(format!("--glob={include}"));
    }
    if let Some(path) = path {
        parts.push("--".to_owned());
        parts.push(path.to_owned());
    }
    parts
}

/// Removes the displayed search-root prefix before choosing a top-level
/// group (dsh `relativeToSearchRoot`).
fn relative_to_search_root(path: &str, root: &str) -> String {
    let sep = std::path::MAIN_SEPARATOR;
    if root == "." {
        let dot_prefix = format!(".{sep}");
        return path
            .strip_prefix(&dot_prefix)
            .map_or_else(|| path.to_owned(), str::to_owned);
    }
    let mut root_end = root.len();
    while root_end > 0 && root.as_bytes()[root_end - 1] == sep as u8 {
        root_end -= 1;
    }
    let trimmed_root = &root[..root_end];
    if trimmed_root.is_empty() {
        return strip_leading_separators(path);
    }
    if path == trimmed_root {
        return String::new();
    }
    let root_prefix = format!("{trimmed_root}{sep}");
    if let Some(rest) = path.strip_prefix(&root_prefix) {
        return rest.to_owned();
    }
    path.to_owned()
}

/// Strips only separators recognized by the execution platform (dsh
/// `stripLeadingSeparators`).
fn strip_leading_separators(path: &str) -> String {
    let sep = std::path::MAIN_SEPARATOR;
    let stripped = path.trim_start_matches(sep);
    stripped.to_owned()
}

/// The leading path segment of one display path — the top-level entry
/// relative to the search root (dsh `topLevelSegment`). A path with no
/// separator is its own top-level entry.
fn top_level_segment(path: &str) -> String {
    let trimmed = strip_leading_separators(path);
    let sep = std::path::MAIN_SEPARATOR;
    match trimmed.find(sep) {
        None => trimmed,
        Some(cut) => trimmed[..cut].to_owned(),
    }
}

/// The inline page of an over-cap result, plus how much of the complete
/// result's top level it reaches (dsh `GlobSample`).
#[derive(Debug)]
pub struct GlobSample {
    /// Paths to show inline, grouped by top-level entry.
    pub items: Vec<String>,
    /// Distinct top-level entries the shown paths reach.
    pub shown: usize,
    /// Distinct top-level entries across the complete result.
    pub total: usize,
}

/// Chooses the inline page of an over-cap result by round-robin across
/// the complete result's top-level entries (dsh
/// `sampleAcrossTopLevel`).
///
/// Every top-level entry receives a slot before any receives a second;
/// exhausted groups drop out. Group order and order within each group
/// follow `paths`, so a flat result reproduces the modification-time
/// head.
#[must_use]
pub fn sample_across_top_level(paths: &[String], max_items: usize, root: &str) -> GlobSample {
    struct ActiveGroup<'a> {
        key: String,
        items: &'a [String],
        index: usize,
        current: String,
    }
    // Phase 1: group by top-level entry in first-seen order.
    let mut groups: HashMap<String, Vec<String>> = HashMap::new();
    let mut group_order: Vec<String> = Vec::new();
    for path in paths {
        let key = top_level_segment(&relative_to_search_root(path, root));
        match groups.get_mut(&key) {
            None => {
                group_order.push(key.clone());
                groups.insert(key, vec![path.clone()]);
            }
            Some(group) => group.push(path.clone()),
        }
    }
    // Phase 2: the round-robin starts from the first-seen groups (the
    // dsh active list starts in insertion order).
    let mut active: Vec<ActiveGroup<'_>> = group_order
        .iter()
        .filter_map(|key| {
            groups.get(key).map(|items| ActiveGroup {
                key: key.clone(),
                items,
                index: 0,
                current: items[0].clone(),
            })
        })
        .collect();
    let mut taken: HashMap<String, Vec<String>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut count = 0_usize;
    while !active.is_empty() && count < max_items {
        let mut next_active: Vec<ActiveGroup<'_>> = Vec::new();
        for group in active {
            if count >= max_items {
                break;
            }
            count += 1;
            match taken.get_mut(&group.key) {
                None => {
                    order.push(group.key.clone());
                    taken.insert(group.key.clone(), vec![group.current.clone()]);
                }
                Some(bucket) => bucket.push(group.current.clone()),
            }
            let next_index = group.index + 1;
            if let Some(next_path) = group.items.get(next_index) {
                next_active.push(ActiveGroup {
                    key: group.key,
                    items: group.items,
                    index: next_index,
                    current: next_path.clone(),
                });
            }
        }
        active = next_active;
    }
    let items = order
        .iter()
        .filter_map(|key| taken.get(key))
        .flatten()
        .cloned()
        .collect();
    GlobSample {
        items,
        shown: taken.len(),
        total: groups.len(),
    }
}

/// Formats a capped sampled page and its complete-result recovery path
/// (dsh `formatGlobOutput`). A flat result keeps the plain footer
/// because its sample is the modification-time head.
#[must_use]
pub fn format_glob_output(sample: &GlobSample, seen: usize, spill_ref: Option<&Value>) -> String {
    let basis = if sample.total == seen {
        ".".to_owned()
    } else {
        let narrow = if sample.shown < sample.total {
            " Narrow path to inspect a specific subtree."
        } else {
            ""
        };
        format!(
            ", sampled across {} of the {} top-level entries this pattern matched instead of \
taken in modification-time order.{narrow}",
            sample.shown, sample.total
        )
    };
    format_glob_page(&sample.items, seen, spill_ref, &basis)
}

/// Formats one bounded page and the recovery path for its complete
/// sorted result (dsh `formatGlobPage`).
#[must_use]
pub fn format_glob_page(
    items: &[String],
    seen: usize,
    spill_ref: Option<&Value>,
    basis: &str,
) -> String {
    let body = items.join("\n");
    let recovery = spill_recovery(spill_ref, "sorted", "narrow pattern or path to see more.");
    format!(
        "{body}\n\n(Showing {} of {seen} paths{basis} {recovery})",
        items.len()
    )
}

/// The recovery footer over one spill reference (the two dsh footer
/// arms: the saved locator line, or the could-not-save sentence).
fn spill_recovery(spill_ref: Option<&Value>, noun: &str, remedy: &str) -> String {
    spill_ref.map_or_else(
        || format!("The complete result could not be saved; {remedy}."),
        |reference| {
            let locator = reference
                .get("locator")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let hint = reference
                .get("retrievalHint")
                .and_then(Value::as_str)
                .unwrap_or_default();
            format!("Full {noun} result stored at: {locator}. {hint}")
        },
    )
}

/// Bounds and formats one canonical path list for the model surface
/// relative to its search root (dsh `renderGlobPaths`).
#[must_use]
pub fn render_glob_paths(
    paths: &[String],
    caps: &SearchCaps,
    root: &str,
    spill_ref: Option<&Value>,
) -> String {
    if paths.is_empty() {
        return "No files found".to_owned();
    }
    // A result that fits is shown whole, untouched: modification-time
    // order is the tool's contract, and over a complete result it is
    // what answers age questions.
    if paths.len() <= caps.glob_max_results {
        return paths.join("\n");
    }
    if !caps.sample_over_cap_glob_results {
        let (head, _, _) = search_core::head_items(paths.iter().cloned(), caps.glob_max_results);
        return format_glob_page(&head, paths.len(), spill_ref, ".");
    }
    format_glob_output(
        &sample_across_top_level(paths, caps.glob_max_results, root),
        paths.len(),
        spill_ref,
    )
}

/// The inline page of paths a completed `glob` card shows, computed the
/// SAME way the model-facing render computes its page (dsh
/// `globCardPage`).
fn glob_card_page(paths: &[String], caps: &SearchCaps, root: &str) -> (Vec<String>, bool) {
    if paths.len() <= caps.glob_max_results {
        return (paths.to_vec(), false);
    }
    if !caps.sample_over_cap_glob_results {
        let (head, _, _) = search_core::head_items(paths.iter().cloned(), caps.glob_max_results);
        return (head, true);
    }
    (
        sample_across_top_level(paths, caps.glob_max_results, root).items,
        true,
    )
}

/// Formats the model-facing `grep` result (dsh `formatGrepOutput`).
///
/// The header carries the found count, the body groups the retained
/// matches by file, and a capped result appends a footer carrying
/// either the formatted-spill recovery locator or the could-not-save
/// explanation.
#[must_use]
pub fn format_grep_output(
    retained: (&[GrepMatch], bool, usize),
    spill_ref: Option<&Value>,
) -> String {
    let (items, truncated, seen) = retained;
    let header = if truncated {
        format!("Found {} of {seen} matches", items.len())
    } else {
        format!("Found {seen} {}", match_noun(seen))
    };
    let body = format_grep_matches(items);
    if !truncated {
        return format!("{header}\n\n{body}");
    }
    let recovery = spill_recovery(
        spill_ref,
        "grep",
        "narrow pattern, path, or include to see more",
    );
    format!("{header}\n\n{body}\n\n({recovery})")
}

/// `match` / `matches` for a count (dsh `matchNoun`).
#[must_use]
pub const fn match_noun(count: usize) -> &'static str {
    if count == 1 { "match" } else { "matches" }
}

/// Groups flat matches by file (first-seen order) into the model-facing
/// body (dsh `formatGrepMatches`): each file's display path, then one
/// `Line N: <text>` row per match.
///
/// # Panics
///
/// Never in practice: the map slots are inserted before their reads in
/// the same single-threaded pass (the dsh loop-shape expectation).
#[must_use]
pub fn format_grep_matches(matches: &[GrepMatch]) -> String {
    let mut order: Vec<String> = Vec::new();
    let mut by_file: HashMap<String, Vec<&GrepMatch>> = HashMap::new();
    for match_ in matches {
        if !by_file.contains_key(&match_.path) {
            order.push(match_.path.clone());
            by_file.insert(match_.path.clone(), Vec::new());
        }
        by_file
            .get_mut(&match_.path)
            .expect("the group was just created")
            .push(match_);
    }
    let sections: Vec<String> = order
        .iter()
        .map(|path| {
            let rows: Vec<String> = by_file
                .get(path)
                .expect("the group was just created")
                .iter()
                .map(|match_| format!("Line {}: {}", match_.line_number, match_.line))
                .collect();
            format!("{path}\n{}", rows.join("\n"))
        })
        .collect();
    sections.join("\n\n")
}

/// Formats one already-retained match list (dsh `formatRetainedGrep`).
#[must_use]
pub fn format_retained_grep(
    retained: (&[GrepMatch], bool, usize),
    spill_ref: Option<&Value>,
) -> String {
    if retained.2 == 0 {
        return "No matches found".to_owned();
    }
    format_grep_output(retained, spill_ref)
}

/// Parses one `rg --json` NDJSON line into a match (dsh `parseRecord`).
///
/// # Errors
///
/// The dsh malformed-output refusals verbatim (a non-JSON line, a
/// non-object record, a match record missing its path, line number, or
/// line content).
pub fn parse_record(line: &str) -> Result<Option<GrepMatch>, BodyError> {
    let malformed = |detail: &str| {
        search_error(
            format!("grep received malformed ripgrep --json output ({detail})"),
            "SEARCH_FAILED",
        )
    };
    let Ok(parsed) = serde_json::from_str::<Value>(line) else {
        return Err(malformed("a line is not JSON"));
    };
    if !parsed.is_object() {
        return Err(malformed("a record is not an object"));
    }
    if parsed.get("type").and_then(Value::as_str) != Some("match") {
        // Non-match record types (begin/end/context/summary — and any
        // future type) are transport framing, not results: skipped.
        return Ok(None);
    }
    let data = parsed.get("data").filter(|data| data.is_object());
    let Some(data) = data else {
        return Err(malformed("a match record has no data"));
    };
    let path_text = data
        .get("path")
        .filter(|path| path.is_object())
        .and_then(|path| path.get("text"))
        .and_then(Value::as_str);
    let Some(path_text) = path_text else {
        return Err(malformed("a match record has no path text"));
    };
    let line_number = data
        .get("line_number")
        .and_then(Value::as_f64)
        .ok_or_else(|| malformed("a match record has no line number"))?;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "ripgrep emits integral 1-based line numbers; a fractional value is malformed upstream"
    )]
    let line_number = line_number as u64;
    let lines = data.get("lines").filter(|lines| lines.is_object());
    let Some(lines) = lines else {
        return Err(malformed("a match record has no line content"));
    };
    if let Some(text) = lines.get("text").and_then(Value::as_str) {
        let stripped = text.strip_suffix('\n').unwrap_or(text);
        let stripped = stripped.strip_suffix('\r').unwrap_or(stripped);
        return Ok(Some(GrepMatch {
            path: path_text.to_owned(),
            line_number,
            line: stripped.to_owned(),
        }));
    }
    if lines.get("bytes").and_then(Value::as_str).is_some() {
        return Ok(Some(GrepMatch {
            path: path_text.to_owned(),
            line_number,
            line: "(line is not valid UTF-8)".to_owned(),
        }));
    }
    Err(malformed("a match record has neither line text nor bytes"))
}

/// Parses complete `rg --json` stdout into flat matches (dsh
/// `parseGrepMatches`), in output order.
///
/// # Errors
///
/// The first malformed record.
pub fn parse_grep_matches(stdout: &str) -> Result<Vec<GrepMatch>, BodyError> {
    let mut matches = Vec::new();
    for line in stdout.split('\n') {
        if line.is_empty() {
            continue;
        }
        if let Some(match_) = parse_record(line)? {
            matches.push(match_);
        }
    }
    Ok(matches)
}

/// Presents the pending glob call (dsh `presentGlobCall`): a search
/// card titled by the pattern and root.
#[must_use]
pub fn present_call_glob(arguments: &Value) -> Value {
    let pattern = word(arguments, "pattern");
    let title = arguments.get("path").and_then(Value::as_str).map_or_else(
        || format!("Glob {pattern}"),
        |path| format!("Glob {pattern} in {path}"),
    );
    json!({
        "card": "generic",
        "title": title,
        "kind": "search",
        "rawInput": pattern,
    })
}

/// Presents the pending grep call (dsh `presentGrepCall`).
#[must_use]
pub fn present_call_grep(arguments: &Value) -> Value {
    let pattern = word(arguments, "pattern");
    let mut title = arguments.get("path").and_then(Value::as_str).map_or_else(
        || format!("Grep {pattern}"),
        |path| format!("Grep {pattern} in {path}"),
    );
    if let Some(include) = arguments.get("include").and_then(Value::as_str) {
        title.push_str(" (");
        title.push_str(include);
        title.push(')');
    }
    json!({
        "card": "generic",
        "title": title,
        "kind": "search",
        "rawInput": pattern,
    })
}

/// Presents the completed glob (dsh `presentGlobResult`): the search
/// card from the persisted meta, declining on any malformed replay
/// shape or a non-paths shape.
pub fn present_result_glob(_arguments: &Value, result: &Value) -> Option<Value> {
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let view = search_views::search_view_from_meta(result.get("meta").filter(|m| !m.is_null()))?;
    if view.get("shape").and_then(Value::as_str) != Some("paths") {
        return None;
    }
    Some(view)
}

/// Presents the completed grep (dsh `presentGrepResult`).
pub fn present_result_grep(_arguments: &Value, result: &Value) -> Option<Value> {
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let view = search_views::search_view_from_meta(result.get("meta").filter(|m| !m.is_null()))?;
    if view.get("shape").and_then(Value::as_str) != Some("matches") {
        return None;
    }
    Some(view)
}

/// Executes one `glob` call (dsh `applyGlobTool.execute`).
fn glob_execute(
    deps: &SearchToolDeps<'_>,
    arguments: &Value,
    exec: &SearchExec,
) -> Result<Value, BodyError> {
    let caps = search_core::baked_caps();
    let (pattern, path) = parse_glob_args(arguments).map_err(BodyError::plain)?;
    let run = run(
        deps,
        exec,
        "glob",
        &build_glob_command(&pattern, path.as_deref()),
        &caps,
    )?;
    let root = path.as_ref().map_or_else(
        || ".".to_owned(),
        |path| search_core::to_workdir_relative(path, &run.workdir),
    );
    if run.no_matches {
        return Ok(json!({ "root": root, "paths": [] }));
    }
    let all: Vec<String> = run
        .stdout
        .split('\n')
        .filter(|line| !line.is_empty())
        .map(|line| search_core::to_workdir_relative(line, &run.workdir))
        .collect();
    Ok(json!({ "root": root, "paths": all }))
}

/// Executes one `grep` call (dsh `applyGrepTool.execute`).
fn grep_execute(
    deps: &SearchToolDeps<'_>,
    arguments: &Value,
    exec: &SearchExec,
) -> Result<Value, BodyError> {
    let caps = search_core::baked_caps();
    let (pattern, path, include) = parse_grep_args(arguments).map_err(BodyError::plain)?;
    let run = run(
        deps,
        exec,
        "grep",
        &build_grep_command(&pattern, path.as_deref(), include.as_deref()),
        &caps,
    )?;
    if run.no_matches {
        return Ok(json!({ "matches": [] }));
    }
    let all: Vec<GrepMatch> = parse_grep_matches(&run.stdout)?
        .into_iter()
        .map(|match_| GrepMatch {
            path: search_core::to_workdir_relative(&match_.path, &run.workdir),
            line_number: match_.line_number,
            line: match_.line,
        })
        .collect();
    Ok(json!({
        "matches": all
            .iter()
            .map(|match_| json!({
                "path": match_.path,
                "lineNumber": match_.line_number,
                "line": match_.line,
            }))
            .collect::<Vec<_>>(),
    }))
}

/// The shared `runRipgrep` lift over the seams.
fn run(
    deps: &SearchToolDeps<'_>,
    exec: &SearchExec,
    tool_name: &str,
    argv: &[String],
    caps: &SearchCaps,
) -> Result<RipgrepRun, BodyError> {
    search_core::run_ripgrep(deps.seams, exec, tool_name, argv, caps)
}

/// Renders the glob output (dsh `output.render`).
///
/// # Errors
///
/// The canonical-value shape refusals (the missing root or paths).
pub fn render_glob(arguments: &Value, value: &Value) -> Result<Vec<Value>, String> {
    let _ = arguments;
    let caps = search_core::baked_caps();
    let root = value
        .get("root")
        .and_then(Value::as_str)
        .ok_or_else(|| "glob render requires root".to_owned())?;
    let paths: Vec<String> = value
        .get("paths")
        .and_then(Value::as_array)
        .ok_or_else(|| "glob render requires paths".to_owned())?
        .iter()
        .map(|path| {
            path.as_str()
                .map(str::to_owned)
                .ok_or_else(|| "glob render requires path strings".to_owned())
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(vec![json!({
        "type": "text",
        "text": render_glob_paths(&paths, &caps, root, None),
    })])
}

/// Renders the grep output (dsh `output.render`): the retained page.
///
/// # Errors
///
/// The canonical-value shape refusals (the missing match fields).
pub fn render_grep(arguments: &Value, value: &Value) -> Result<Vec<Value>, String> {
    let _ = arguments;
    let caps = search_core::baked_caps();
    let matches = parse_value_matches(value)?;
    let retained = search_core::head_items(matches.iter().cloned(), caps.grep_max_matches);
    let previewed = retained_preview(&retained, &caps);
    Ok(vec![json!({
        "type": "text",
        "text": format_retained_grep((&previewed, retained.1, retained.2), None),
    })])
}

/// Previews the retained page's lines (the render's shared retention
/// pass; dsh `retainGrepMatches`).
fn retained_preview(retained: &(Vec<GrepMatch>, bool, usize), caps: &SearchCaps) -> Vec<GrepMatch> {
    retained
        .0
        .iter()
        .map(|match_| GrepMatch {
            path: match_.path.clone(),
            line_number: match_.line_number,
            line: search_core::preview_line(&match_.line, caps.grep_max_line_bytes),
        })
        .collect()
}

/// Parses the canonical value's match array (the render and meta
/// projections' shared narrowing).
fn parse_value_matches(value: &Value) -> Result<Vec<GrepMatch>, String> {
    value
        .get("matches")
        .and_then(Value::as_array)
        .ok_or_else(|| "grep render requires matches".to_owned())?
        .iter()
        .map(|match_| {
            Ok(GrepMatch {
                path: match_
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "grep render requires match paths".to_owned())?
                    .to_owned(),
                line_number: match_
                    .get("lineNumber")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "grep render requires match line numbers".to_owned())?,
                line: match_
                    .get("line")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "grep render requires match lines".to_owned())?
                    .to_owned(),
            })
        })
        .collect()
}

/// Projects the glob meta (dsh `output.presentationMeta`).
///
/// # Errors
///
/// The canonical-value shape refusals (the missing root or paths).
pub fn presentation_meta_glob(_arguments: &Value, value: &Value) -> Result<Value, String> {
    let caps = search_core::baked_caps();
    let root = value
        .get("root")
        .and_then(Value::as_str)
        .ok_or_else(|| "glob meta requires root".to_owned())?;
    let paths: Vec<String> = value
        .get("paths")
        .and_then(Value::as_array)
        .ok_or_else(|| "glob meta requires paths".to_owned())?
        .iter()
        .map(|path| {
            path.as_str()
                .map(str::to_owned)
                .ok_or_else(|| "glob meta requires path strings".to_owned())
        })
        .collect::<Result<Vec<_>, String>>()?;
    let (items, truncated) = glob_card_page(&paths, &caps, root);
    Ok(search_views::glob_search_meta(
        (&items, truncated, paths.len()),
        caps.search_meta_max_bytes,
    ))
}

/// Projects the grep meta (dsh `output.presentationMeta`).
///
/// # Errors
///
/// The canonical-value shape refusals (the missing match fields).
pub fn presentation_meta_grep(_arguments: &Value, value: &Value) -> Result<Value, String> {
    let caps = search_core::baked_caps();
    let matches = parse_value_matches(value)?;
    let retained = search_core::head_items(matches.iter().cloned(), caps.grep_max_matches);
    let previewed = retained_preview(&retained, &caps);
    Ok(search_views::grep_search_meta(
        (&previewed, retained.1, retained.2),
        caps.search_meta_max_bytes,
    ))
}

/// Composes the `tool:grep` section text the way the dsh source does
/// (the base sentence, the read tail only while `read` registers).
#[must_use]
pub fn grep_section_text(read_registered: bool) -> String {
    let segments = catalog_gen::GREP_SECTION_TEXTS;
    let mut text = String::from(segments[0]);
    if read_registered {
        text.push_str(segments[1]);
    }
    text
}

/// Serves one search-tool method invocation (the glob/grep bands).
///
/// # Errors
///
/// The payload decode refusal, an unknown method, or the tool body's
/// own failure (the wire body-error shape).
pub fn serve(deps: &SearchToolDeps<'_>, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
    let request: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(payload)
            .map_err(|error| format!("builtin-tools payload must be JSON: {error}"))?
    };
    let reply = match method {
        METHOD_GLOB_EXECUTE => {
            return serve_execute(deps, ToolKind::Glob, &request);
        }
        METHOD_GREP_EXECUTE => {
            return serve_execute(deps, ToolKind::Grep, &request);
        }
        METHOD_GLOB_RENDER => json!(render_glob(&request["args"], &request["value"])?),
        METHOD_GREP_RENDER => json!(render_grep(&request["args"], &request["value"])?),
        METHOD_GLOB_PRESENTATION_META => {
            json!(presentation_meta_glob(&request["args"], &request["value"])?)
        }
        METHOD_GREP_PRESENTATION_META => {
            json!(presentation_meta_grep(&request["args"], &request["value"])?)
        }
        METHOD_GLOB_PRESENT_CALL => present_call_glob(&request["args"]),
        METHOD_GREP_PRESENT_CALL => present_call_grep(&request["args"]),
        METHOD_GLOB_PRESENT_RESULT => {
            present_result_glob(&request["args"], &request["result"]).unwrap_or(Value::Null)
        }
        METHOD_GREP_PRESENT_RESULT => {
            present_result_grep(&request["args"], &request["result"]).unwrap_or(Value::Null)
        }
        METHOD_GLOB_SPILL => return serve_spill(deps, ToolKind::Glob, &request),
        METHOD_GREP_SPILL => return serve_spill(deps, ToolKind::Grep, &request),
        other => return Err(format!("unknown fr-tools-builtin search method {other}")),
    };
    serde_json::to_vec(&reply)
        .map_err(|error| format!("builtin-tools reply encode failed: {error}"))
}

/// Serves one tool body: decodes the execution frame, runs the dsh
/// pipeline, and maps the outcome onto the wire body protocol.
fn serve_execute(
    deps: &SearchToolDeps<'_>,
    kind: ToolKind,
    request: &Value,
) -> Result<Vec<u8>, String> {
    let call = request
        .get("call")
        .ok_or_else(|| "builtin-tools execute payload requires call".to_string())?;
    let exec = SearchExec {
        tool_name: match kind {
            ToolKind::Glob => "glob".to_owned(),
            ToolKind::Grep => "grep".to_owned(),
        },
        call_id: call
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        root_call_id: call
            .get("root_call_id")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                call.get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            })
            .to_owned(),
        agent: call.get("agent").and_then(Value::as_str).map(str::to_owned),
    };
    let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
    let outcome = match kind {
        ToolKind::Glob => glob_execute(deps, &arguments, &exec),
        ToolKind::Grep => grep_execute(deps, &arguments, &exec),
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

/// Serves one spill listener call over the post-execute CPS protocol:
/// the enter leg parks the call and result and answers delegate; the
/// exit leg takes the downstream decision and, for a matching
/// over-cap direct call, replaces the content with the capped render
/// over the saved complete result (the dsh `tools/post-execute` arms).
fn serve_spill(
    deps: &SearchToolDeps<'_>,
    kind: ToolKind,
    request: &Value,
) -> Result<Vec<u8>, String> {
    let token = request.get("token").and_then(Value::as_u64);
    let Some(token) = token else {
        return Err("spill listener frame requires token".to_owned());
    };
    if request.get("leg").and_then(Value::as_str) == Some("enter") {
        let call = request.get("call").cloned().unwrap_or(Value::Null);
        let result = request.get("result").cloned().unwrap_or(Value::Null);
        deps.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(token, (call, result));
        return serde_json::to_vec(&json!({ "kind": "delegate" }))
            .map_err(|error| format!("spill reply encode failed: {error}"));
    }
    let Some((call, result)) = deps
        .pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&token)
    else {
        return Err(format!(
            "spill exit leg has no parked enter for token {token}"
        ));
    };
    let decision = request.get("decision").cloned().unwrap_or(Value::Null);
    let value = accepted_direct_call_value(kind, &call, &result, &decision);
    let content = match kind {
        ToolKind::Glob => glob_spill_content(deps.seams, &call, value.as_ref()),
        ToolKind::Grep => grep_spill_content(deps.seams, &call, value.as_ref()),
    };
    let Some(content) = content else {
        // The unchanged decision rides through verbatim.
        return serde_json::to_vec(&json!({ "kind": "final", "decision": decision }))
            .map_err(|error| format!("spill reply encode failed: {error}"));
    };
    let contexts = decision
        .get("additionalContexts")
        .cloned()
        .unwrap_or_else(|| json!([]));
    serde_json::to_vec(&json!({
        "kind": "final",
        "decision": {
            "kind": "accept",
            "content": content,
            "additionalContexts": contexts,
        },
    }))
    .map_err(|error| format!("spill reply encode failed: {error}"))
}

/// The acceptedDirectCallValue conditions (dsh `direct-call.ts`): a
/// successful direct top-level call this tool owns, with no downstream
/// replacement of either projection. The dsh live-registry identity
/// check (`ctx.tools.get(exec.name, exec.agent) === tool`) has no fr
/// analog — the registry identity does not cross the wire; the
/// name/shape guards below carry the practical protection (the module
/// veracity note).
fn accepted_direct_call_value(
    kind: ToolKind,
    call: &Value,
    result: &Value,
    decision: &Value,
) -> Option<Value> {
    if decision.get("kind").and_then(Value::as_str) != Some("accept") {
        return None;
    }
    if !decision.get("content").is_none_or(Value::is_null) {
        return None;
    }
    if !decision.get("value").is_none_or(Value::is_null) {
        return None;
    }
    if call.get("parent").is_some_and(|parent| !parent.is_null()) {
        return None;
    }
    let expected = match kind {
        ToolKind::Glob => "glob",
        ToolKind::Grep => "grep",
    };
    if call.get("name").and_then(Value::as_str) != Some(expected) {
        return None;
    }
    if result.get("kind").and_then(Value::as_str) != Some("success") {
        return None;
    }
    result.get("value").cloned()
}

/// The glob spill arm (dsh `applyGlobTool` post-execute): the complete
/// list over the cap saves verbatim and the content replaces with the
/// capped render over the saved reference.
fn glob_spill_content(
    seams: &dyn Seams,
    call: &Value,
    value: Option<&Value>,
) -> Option<Vec<Value>> {
    let value = value?;
    let caps = search_core::baked_caps();
    let root = value.get("root").and_then(Value::as_str)?;
    let paths: Vec<String> = value
        .get("paths")
        .and_then(Value::as_array)?
        .iter()
        .map(|path| path.as_str().unwrap_or_default().to_owned())
        .collect();
    if paths.len() <= caps.glob_max_results {
        return None;
    }
    let exec = spill_exec(call);
    let spill_ref =
        search_core::try_save_formatted_result(seams, &exec, "glob-results.txt", &paths.join("\n"));
    let text = render_glob_paths(&paths, &caps, root, spill_ref.as_ref());
    Some(vec![json!({ "type": "text", "text": text })])
}

/// The grep spill arm (dsh `applyGrepTool` post-execute): the spill
/// artifact holds the COMPLETE result (every match, each line
/// previewed), and the content replaces with the retained render over
/// the saved reference.
fn grep_spill_content(
    seams: &dyn Seams,
    call: &Value,
    value: Option<&Value>,
) -> Option<Vec<Value>> {
    let value = value?;
    let caps = search_core::baked_caps();
    let matches: Vec<GrepMatch> = value
        .get("matches")
        .and_then(Value::as_array)?
        .iter()
        .map(|match_| GrepMatch {
            path: match_
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            line_number: match_
                .get("lineNumber")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            line: match_
                .get("line")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        })
        .collect();
    if matches.len() <= caps.grep_max_matches {
        return None;
    }
    let exec = spill_exec(call);
    let previewed_all: Vec<GrepMatch> = matches
        .iter()
        .map(|match_| GrepMatch {
            path: match_.path.clone(),
            line_number: match_.line_number,
            line: search_core::preview_line(&match_.line, caps.grep_max_line_bytes),
        })
        .collect();
    let spill_ref = search_core::try_save_formatted_result(
        seams,
        &exec,
        "grep-results.txt",
        &format!(
            "Found {} {}\n\n{}",
            matches.len(),
            match_noun(matches.len()),
            format_grep_matches(&previewed_all)
        ),
    );
    let retained = search_core::head_items(matches.iter().cloned(), caps.grep_max_matches);
    let previewed = retained_preview(&retained, &caps);
    let text = format_retained_grep((&previewed, retained.1, retained.2), spill_ref.as_ref());
    Some(vec![json!({ "type": "text", "text": text })])
}

/// Rebuilds the spill execution identity from the parked call (the
/// session owner, the tool name, and the call id).
fn spill_exec(call: &Value) -> SearchExec {
    SearchExec {
        tool_name: call
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        call_id: call
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        root_call_id: call
            .get("root_call_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        agent: call.get("agent").and_then(Value::as_str).map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    /// One unique scratch directory per test (cleaned on drop).
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fr-search-{tag}-{}-{}",
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

    /// The scripted cross-plugin seam over REAL child processes: the
    /// spawn rides `std::process` (the test infrastructure), the workdir
    /// is fixed, the rg resolution walks the real PATH, and the spill
    /// outcomes queue scripts the three warn arms.
    struct SpawnSeams {
        /// Kept alive so the scratch cleanup drops with the fixture.
        #[expect(dead_code, reason = "the field holds the cleanup lifetime")]
        scratch: Scratch,
        workdir: String,
        resolve_fail: bool,
        spills: Mutex<VecDeque<Result<Value, String>>>,
        saved: Mutex<Vec<Value>>,
        children: Mutex<HashMap<String, std::process::Child>>,
        counters: AtomicU64,
        outputs: Mutex<HashMap<String, StreamPair>>,
    }

    /// One child's collected stream pair (the reader-thread buffers).
    type StreamPair = (Arc<Mutex<String>>, Arc<Mutex<String>>);

    impl SpawnSeams {
        fn new(tag: &str) -> Self {
            let scratch = Scratch::new(tag);
            let workdir = scratch.path("");
            Self {
                scratch,
                workdir,
                resolve_fail: false,
                spills: Mutex::new(VecDeque::new()),
                saved: Mutex::new(Vec::new()),
                children: Mutex::new(HashMap::new()),
                counters: AtomicU64::new(1),
                outputs: Mutex::new(HashMap::new()),
            }
        }
    }

    impl Drop for SpawnSeams {
        fn drop(&mut self) {
            let children: Vec<std::process::Child> = self
                .children
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .drain()
                .map(|(_, child)| child)
                .collect();
            for mut child in children {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    impl crate::tools::PolicySeam for SpawnSeams {
        fn resolve_policy(&self, _session: Option<&str>) -> Result<Option<Value>, String> {
            Ok(None)
        }
    }

    impl crate::tools::ApprovalSeam for SpawnSeams {
        fn request(&self, _ask: &fr_sandbox::escalation::EscalationAsk) -> Result<String, String> {
            Err("unused".to_owned())
        }
    }

    impl Seams for SpawnSeams {
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

        fn subprocess_spawn(&self, spec: &Value) -> Result<String, String> {
            let argv: Vec<String> = spec
                .get("argv")
                .and_then(Value::as_array)
                .expect("argv")
                .iter()
                .map(|word| word.as_str().unwrap_or_default().to_owned())
                .collect();
            let cwd = spec.get("cwd").and_then(Value::as_str).unwrap_or_default();
            let mut command = std::process::Command::new(&argv[0]);
            command
                .args(&argv[1..])
                .current_dir(cwd)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            let mut child = command.spawn().map_err(|error| error.to_string())?;
            let stdout_buffer = Arc::new(Mutex::new(String::new()));
            let stderr_buffer = Arc::new(Mutex::new(String::new()));
            if let Some(stdout) = child.stdout.take() {
                let buffer = Arc::clone(&stdout_buffer);
                std::thread::spawn(move || {
                    use std::io::Read;
                    let mut stdout = stdout;
                    let mut text = String::new();
                    let _ = stdout.read_to_string(&mut text);
                    *buffer
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = text;
                });
            }
            if let Some(stderr) = child.stderr.take() {
                let buffer = Arc::clone(&stderr_buffer);
                std::thread::spawn(move || {
                    use std::io::Read;
                    let mut stderr = stderr;
                    let mut text = String::new();
                    let _ = stderr.read_to_string(&mut text);
                    *buffer
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = text;
                });
            }
            let id = self
                .counters
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                .to_string();
            self.outputs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(id.clone(), (stdout_buffer, stderr_buffer));
            self.children
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(id.clone(), child);
            Ok(id)
        }

        fn subprocess_read(&self, handle: &str, stream: &str, from: u64) -> Result<Value, String> {
            let pair = {
                let outputs = self
                    .outputs
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                outputs.get(handle).cloned()
            }
            .ok_or_else(|| format!("unknown subprocess handle {handle}"))?;
            let buffer = match stream {
                "stdout" => pair.0,
                "stderr" => pair.1,
                other => return Err(format!("unknown subprocess stream {other}")),
            };
            let text = buffer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            #[expect(
                clippy::cast_possible_truncation,
                reason = "the test offsets are small constants far below the 32-bit bound"
            )]
            let skip = from as usize;
            let window = text.bytes().skip(skip).collect::<Vec<u8>>();
            let text = String::from_utf8_lossy(&window).into_owned();
            Ok(json!({ "text": text, "lossy": false, "next_offset": 0 }))
        }

        fn subprocess_wait(&self, handle: &str, timeout_ms: f64) -> Result<Value, String> {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "the JS Number parity judgment: the timer consumes integer-truncated milliseconds"
            )]
            let millis = timeout_ms.max(0.0) as u64;
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(millis);
            loop {
                let exited = {
                    let mut children = self
                        .children
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if let Some(child) = children.get_mut(handle) {
                        match child.try_wait() {
                            Ok(Some(status)) => {
                                Some((children.remove(handle).expect("just probed"), status))
                            }
                            Ok(None) => None,
                            Err(error) => return Err(error.to_string()),
                        }
                    } else {
                        None
                    }
                };
                if let Some((mut child, status)) = exited {
                    let _ = child.wait();
                    let exit_code = status.code();
                    let signal = exit_code.is_none().then(|| "SIGKILL".to_owned());
                    let mut reply = json!({ "settled": true });
                    let object = reply.as_object_mut().expect("object");
                    if let Some(exit_code) = exit_code {
                        object.insert("exit_code".to_owned(), json!(exit_code));
                    }
                    if let Some(signal) = signal {
                        object.insert("signal".to_owned(), json!(signal));
                    }
                    return Ok(reply);
                }
                if std::time::Instant::now() >= deadline {
                    return Ok(json!({ "settled": false }));
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }

        fn subprocess_resolve_executable(&self, command: &str) -> Result<String, String> {
            if self.resolve_fail {
                return Err("subprocess-local: no such executable".to_owned());
            }
            if std::path::Path::new(command).is_absolute() {
                return Ok(command.to_owned());
            }
            let search_path = std::env::var_os("PATH").ok_or("no PATH")?;
            for directory in std::env::split_paths(&search_path) {
                let candidate = directory.join(command);
                if candidate.is_file() {
                    return Ok(candidate.to_string_lossy().into_owned());
                }
            }
            Err(format!("subprocess-local: command \"{command}\" not found"))
        }

        fn spill_store_save_text(&self, save: &Value) -> Result<Option<Value>, String> {
            self.saved
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(save.clone());
            let outcome = self
                .spills
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front();
            match outcome {
                None => Ok(None),
                Some(Ok(reference)) => Ok(Some(reference)),
                Some(Err(error)) => Err(error),
            }
        }
    }

    /// The parsed spawn spec's rg argv (the --no-config prefix).
    #[test]
    fn glob_round_trip_pins_the_shapes() {
        let seams = SpawnSeams::new("glob-ok");
        std::fs::write(std::path::Path::new(&seams.workdir).join("a.txt"), "x\n").expect("seed");
        std::fs::write(std::path::Path::new(&seams.workdir).join("b.txt"), "y\n").expect("seed");
        let deps = SearchToolDeps {
            seams: &seams,
            pending: &Mutex::new(HashMap::new()),
        };
        let bytes = serde_json::to_vec(&json!({
            "call": {
                "call_id": "c1",
                "root_call_id": "c1",
                "agent": "s1",
                "arguments": { "pattern": "*.txt" },
            }
        }))
        .expect("encode");
        let reply = serve(&deps, METHOD_GLOB_EXECUTE, &bytes).expect("serve ok");
        let value: Value =
            serde_json::from_slice::<Value>(&reply).expect("decode")["value"].clone();
        assert_eq!(value["root"], ".");
        let paths = value["paths"].as_array().expect("paths");
        assert_eq!(paths.len(), 2);
        // rg prints workdir-relative paths; the mtime order depends on
        // the write sequence, so the set is the pin.
        let mut names: Vec<&str> = paths
            .iter()
            .map(|path| path.as_str().expect("path"))
            .collect();
        names.sort_unstable();
        assert_eq!(names, ["a.txt", "b.txt"]);

        let rendered: Value = serde_json::from_slice(
            &serve(
                &deps,
                METHOD_GLOB_RENDER,
                &serde_json::to_vec(&json!({ "args": {}, "value": value })).expect("encode"),
            )
            .expect("render"),
        )
        .expect("decode");
        let text = rendered[0]["text"].as_str().expect("text");
        assert!(text.contains("a.txt") && text.contains("b.txt"), "{text}");
        assert!(!text.contains("Showing"), "{text}");

        let meta: Value = serde_json::from_slice(
            &serve(
                &deps,
                METHOD_GLOB_PRESENTATION_META,
                &serde_json::to_vec(&json!({ "args": {}, "value": value })).expect("encode"),
            )
            .expect("meta"),
        )
        .expect("decode");
        assert_eq!(meta["shape"], "paths");
        assert_eq!(meta["total"], 2);

        let result = json!({
            "isError": false,
            "content": rendered,
            "meta": meta,
        });
        let view: Value = serde_json::from_slice(
            &serve(
                &deps,
                METHOD_GLOB_PRESENT_RESULT,
                &serde_json::to_vec(&json!({ "args": {}, "result": result })).expect("encode"),
            )
            .expect("present"),
        )
        .expect("decode");
        assert_eq!(view["card"], "search");
        assert_eq!(view["shape"], "paths");

        let call_view: Value = serde_json::from_slice(
            &serve(
                &deps,
                METHOD_GLOB_PRESENT_CALL,
                &serde_json::to_vec(&json!({ "args": { "pattern": "*.ts", "path": "src" } }))
                    .expect("encode"),
            )
            .expect("present call"),
        )
        .expect("decode");
        assert_eq!(call_view["title"], "Glob *.ts in src");
        assert_eq!(call_view["kind"], "search");
        assert_eq!(call_view["rawInput"], "*.ts");
    }

    /// The grep round trip over a real rg --json run: grouped lines,
    /// the search card, and the pending call title.
    #[test]
    fn grep_round_trip_pins_the_shapes() {
        let seams = SpawnSeams::new("grep-ok");
        std::fs::write(
            std::path::Path::new(&seams.workdir).join("a.txt"),
            "alpha\nbeta\n",
        )
        .expect("seed");
        let deps = SearchToolDeps {
            seams: &seams,
            pending: &Mutex::new(HashMap::new()),
        };
        let bytes = serde_json::to_vec(&json!({
            "call": {
                "call_id": "c1",
                "root_call_id": "c1",
                "agent": "s1",
                "arguments": { "pattern": "alpha", "include": "*.txt" },
            }
        }))
        .expect("encode");
        let reply = serve(&deps, METHOD_GREP_EXECUTE, &bytes).expect("serve ok");
        let value: Value =
            serde_json::from_slice::<Value>(&reply).expect("decode")["value"].clone();
        let matches = value["matches"].as_array().expect("matches");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["lineNumber"], 1);
        assert_eq!(matches[0]["line"], "alpha");
        assert!(
            matches[0]["path"]
                .as_str()
                .expect("path")
                .ends_with("a.txt")
        );

        let rendered: Value = serde_json::from_slice(
            &serve(
                &deps,
                METHOD_GREP_RENDER,
                &serde_json::to_vec(&json!({ "args": {}, "value": value })).expect("encode"),
            )
            .expect("render"),
        )
        .expect("decode");
        let text = rendered[0]["text"].as_str().expect("text");
        assert!(text.starts_with("Found 1 match\n"), "{text}");

        let meta: Value = serde_json::from_slice(
            &serve(
                &deps,
                METHOD_GREP_PRESENTATION_META,
                &serde_json::to_vec(&json!({ "args": {}, "value": value })).expect("encode"),
            )
            .expect("meta"),
        )
        .expect("decode");
        assert_eq!(meta["shape"], "matches");

        let result = json!({
            "isError": false,
            "content": rendered,
            "meta": meta,
        });
        let view: Value = serde_json::from_slice(
            &serve(
                &deps,
                METHOD_GREP_PRESENT_RESULT,
                &serde_json::to_vec(&json!({ "args": {}, "result": result })).expect("encode"),
            )
            .expect("present"),
        )
        .expect("decode");
        assert_eq!(view["shape"], "matches");

        let call_view: Value = serde_json::from_slice(
            &serve(
                &deps,
                METHOD_GREP_PRESENT_CALL,
                &serde_json::to_vec(&json!({
                    "args": { "pattern": "x", "include": "*.ts" },
                }))
                .expect("encode"),
            )
            .expect("present call"),
        )
        .expect("decode");
        assert_eq!(call_view["title"], "Grep x (*.ts)");
    }

    /// The zero-result family: the plain empty value arms (the dsh
    /// `No files found` / `No matches found` renders).
    #[test]
    fn zero_results_render_the_plain_texts() {
        let seams = SpawnSeams::new("search-empty");
        let deps = SearchToolDeps {
            seams: &seams,
            pending: &Mutex::new(HashMap::new()),
        };
        let bytes = serde_json::to_vec(&json!({
            "call": {
                "call_id": "c1",
                "root_call_id": "c1",
                "agent": "s1",
                "arguments": { "pattern": "*.nope-ext" },
            }
        }))
        .expect("encode");
        let value: Value = serde_json::from_slice::<Value>(
            &serve(&deps, METHOD_GLOB_EXECUTE, &bytes).expect("ok"),
        )
        .expect("decode")["value"]
            .clone();
        assert_eq!(value["paths"], json!([]));
        let rendered: Value = serde_json::from_slice(
            &serve(
                &deps,
                METHOD_GLOB_RENDER,
                &serde_json::to_vec(&json!({ "args": {}, "value": value })).expect("encode"),
            )
            .expect("render"),
        )
        .expect("decode");
        assert_eq!(rendered[0]["text"], "No files found");

        // A workspace with files but no matches: an EMPTY workspace
        // refuses instead (the rg 14 "no files were searched" exit 2,
        // the dsh SEARCH_FAILED arm).
        std::fs::write(std::path::Path::new(&seams.workdir).join("a.txt"), "x\n").expect("seed");
        let bytes = serde_json::to_vec(&json!({
            "call": {
                "call_id": "c1",
                "root_call_id": "c1",
                "agent": "s1",
                "arguments": { "pattern": "zzz-not-there" },
            }
        }))
        .expect("encode");
        let value: Value = serde_json::from_slice::<Value>(
            &serve(&deps, METHOD_GREP_EXECUTE, &bytes).expect("ok"),
        )
        .expect("decode")["value"]
            .clone();
        assert_eq!(value["matches"], json!([]));
        let rendered: Value = serde_json::from_slice(
            &serve(
                &deps,
                METHOD_GREP_RENDER,
                &serde_json::to_vec(&json!({ "args": {}, "value": value })).expect("encode"),
            )
            .expect("render"),
        )
        .expect("decode");
        assert_eq!(rendered[0]["text"], "No matches found");
    }

    /// The argument refusals ride the dsh texts (glob blank pattern and
    /// path; grep blank pattern, blank path, and the include family).
    #[test]
    fn argument_refusals_pin_the_texts() {
        let seams = SpawnSeams::new("search-refusals");
        let deps = SearchToolDeps {
            seams: &seams,
            pending: &Mutex::new(HashMap::new()),
        };
        let run = |method: u32, arguments: Value| {
            let bytes = serde_json::to_vec(&json!({
                "call": {
                    "call_id": "c1",
                    "root_call_id": "c1",
                    "agent": "s1",
                    "arguments": arguments,
                }
            }))
            .expect("encode");
            let reply = serve(&deps, method, &bytes).expect("error body rides the transport");
            let parsed: Value = serde_json::from_slice(&reply).expect("decode");
            parsed["error"]["message"]
                .as_str()
                .expect("message")
                .to_owned()
        };
        assert_eq!(
            run(METHOD_GLOB_EXECUTE, json!({ "pattern": "  " })),
            "pattern must be a non-empty string"
        );
        assert_eq!(
            run(METHOD_GLOB_EXECUTE, json!({ "pattern": "a", "path": "" })),
            "path must be a non-empty string when given"
        );
        assert_eq!(
            run(METHOD_GREP_EXECUTE, json!({ "pattern": "" })),
            "pattern must be a non-empty string"
        );
        assert_eq!(
            run(METHOD_GREP_EXECUTE, json!({ "pattern": "a", "path": " " })),
            "path must be a non-empty string when given"
        );
        assert_eq!(
            run(
                METHOD_GREP_EXECUTE,
                json!({ "pattern": "a", "include": "  " })
            ),
            "include must be a non-empty glob when given"
        );
        assert_eq!(
            run(
                METHOD_GREP_EXECUTE,
                json!({ "pattern": "a", "include": "!x" })
            ),
            "include must be a positive glob filter; negated patterns (\"!…\") are not supported"
        );
        assert_eq!(
            run(
                METHOD_GREP_EXECUTE,
                json!({ "pattern": "a", "include": "a,b" })
            ),
            "include must be one glob, not a comma-separated list (use {a,b} alternation instead)"
        );
        // A comma inside a brace group is one glob: the spawn runs and
        // finds nothing (the workspace holds a non-matching file, since
        // an EMPTY workspace refuses at the rg 14 exit-2 arm).
        std::fs::write(std::path::Path::new(&seams.workdir).join("a.ts"), "x\n").expect("seed");
        let bytes = serde_json::to_vec(&json!({
            "call": {
                "call_id": "c1",
                "root_call_id": "c1",
                "agent": "s1",
                "arguments": { "pattern": "zzz", "include": "*.{ts,tsx}" },
            }
        }))
        .expect("encode");
        let value: Value = serde_json::from_slice::<Value>(
            &serve(&deps, METHOD_GREP_EXECUTE, &bytes).expect("ok"),
        )
        .expect("decode")["value"]
            .clone();
        assert_eq!(value["matches"], json!([]));
    }

    /// A missing ripgrep binary fails clearly (the spec's explicit-error
    /// arm; the dsh launch-failure text).
    #[test]
    fn missing_rg_fails_with_the_launch_text() {
        let mut seams = SpawnSeams::new("search-norg");
        seams.resolve_fail = true;
        let deps = SearchToolDeps {
            seams: &seams,
            pending: &Mutex::new(HashMap::new()),
        };
        let bytes = serde_json::to_vec(&json!({
            "call": {
                "call_id": "c1",
                "root_call_id": "c1",
                "agent": "s1",
                "arguments": { "pattern": "x" },
            }
        }))
        .expect("encode");
        let reply = serve(&deps, METHOD_GREP_EXECUTE, &bytes).expect("error body");
        let parsed: Value = serde_json::from_slice(&reply).expect("decode");
        assert_eq!(
            parsed["error"]["message"],
            "grep could not start its search command (ripgrep launch failed)"
        );
        assert_eq!(parsed["error"]["code"], "SEARCH_FAILED");
        assert_eq!(parsed["error"]["name"], "SearchError");
    }

    /// The spill listener replaces an over-cap direct call's content
    /// with the capped render over the saved complete result; an
    /// under-cap call, a parented call, a foreign name, and an error
    /// result ride through unchanged.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the family pins seven arms end to end over one shared fixture"
    )]
    fn spill_listener_pins_the_arms() {
        let seams = SpawnSeams::new("spill");
        seams
            .spills
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(Ok(json!({
                "locator": "spills/glob-results-1.txt",
                "retrievalHint": "Read the file to see the complete list.",
            })));
        let pending: Mutex<HashMap<u64, (Value, Value)>> = Mutex::new(HashMap::new());
        let deps = SearchToolDeps {
            seams: &seams,
            pending: &pending,
        };
        // Over 100 paths: the enter leg parks, the exit leg replaces.
        let paths: Vec<String> = (0..120).map(|index| format!("f{index:03}.txt")).collect();
        let call = json!({
            "token": 1,
            "leg": "enter",
            "call": {
                "call_id": "c1",
                "root_call_id": "c1",
                "name": "glob",
                "agent": "s1",
                "parent": null,
            },
            "result": {
                "kind": "success",
                "value": { "root": ".", "paths": paths },
            },
        });
        let enter = serve(
            &deps,
            METHOD_GLOB_SPILL,
            &serde_json::to_vec(&call).expect("encode"),
        )
        .expect("enter");
        assert_eq!(
            serde_json::from_slice::<Value>(&enter).expect("decode")["kind"],
            "delegate"
        );
        let exit = json!({
            "token": 1,
            "leg": "exit",
            "decision": { "kind": "accept", "additionalContexts": [] },
        });
        let reply = serve(
            &deps,
            METHOD_GLOB_SPILL,
            &serde_json::to_vec(&exit).expect("encode"),
        )
        .expect("exit");
        let reply: Value = serde_json::from_slice(&reply).expect("decode");
        assert_eq!(reply["kind"], "final");
        assert_eq!(reply["decision"]["kind"], "accept");
        let text = reply["decision"]["content"][0]["text"]
            .as_str()
            .expect("text");
        assert!(text.contains("(Showing 100 of 120 paths"), "{text}");
        assert!(
            text.contains("Full sorted result stored at: spills/glob-results-1.txt."),
            "{text}"
        );
        let saved = seams
            .saved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0]["suggestedName"], "glob-results.txt");
        assert_eq!(saved[0]["source"]["kind"], "tool");
        assert_eq!(saved[0]["source"]["toolName"], "glob");
        assert_eq!(saved[0]["source"]["label"], "result");
        assert_eq!(
            saved[0]["content"]
                .as_str()
                .expect("content")
                .lines()
                .count(),
            120
        );

        // An under-cap call rides through unchanged (no save).
        let call = json!({
            "token": 2,
            "leg": "enter",
            "call": {
                "call_id": "c2",
                "root_call_id": "c2",
                "name": "glob",
                "agent": "s1",
            },
            "result": {
                "kind": "success",
                "value": { "root": ".", "paths": ["only.txt"] },
            },
        });
        let _ = serve(
            &deps,
            METHOD_GLOB_SPILL,
            &serde_json::to_vec(&call).expect("encode"),
        )
        .expect("enter");
        let reply = serve(
            &deps,
            METHOD_GLOB_SPILL,
            &serde_json::to_vec(&json!({
                "token": 2,
                "leg": "exit",
                "decision": { "kind": "accept", "additionalContexts": [] },
            }))
            .expect("encode"),
        )
        .expect("exit");
        let reply: Value = serde_json::from_slice(&reply).expect("decode");
        assert_eq!(reply["decision"]["kind"], "accept");
        assert!(reply["decision"].get("content").is_none());

        // A parented call and a foreign name never spill.
        for (token, name, parent) in [(3u64, "glob", json!("p1")), (4, "grep", json!(null))] {
            let call = json!({
                "token": token,
                "leg": "enter",
                "call": {
                    "call_id": format!("c{token}"),
                    "root_call_id": format!("c{token}"),
                    "name": name,
                    "agent": "s1",
                    "parent": parent,
                },
                "result": {
                    "kind": "success",
                    "value": { "root": ".", "paths": (0..120).map(|i| format!("f{i}.txt")).collect::<Vec<_>>() },
                },
            });
            let _ = serve(
                &deps,
                METHOD_GLOB_SPILL,
                &serde_json::to_vec(&call).expect("encode"),
            )
            .expect("enter");
            let reply = serve(
                &deps,
                METHOD_GLOB_SPILL,
                &serde_json::to_vec(&json!({
                    "token": token,
                    "leg": "exit",
                    "decision": { "kind": "accept", "additionalContexts": [] },
                }))
                .expect("encode"),
            )
            .expect("exit");
            let reply: Value = serde_json::from_slice(&reply).expect("decode");
            assert!(reply["decision"].get("content").is_none());
        }

        // The three warn arms script through the same face: an error
        // store logs and still replaces with the could-not-save footer.
        let pending: Mutex<HashMap<u64, (Value, Value)>> = Mutex::new(HashMap::new());
        let seams = SpawnSeams::new("spill-fail");
        seams
            .spills
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(Err("disk full".to_owned()));
        let deps = SearchToolDeps {
            seams: &seams,
            pending: &pending,
        };
        let paths: Vec<String> = (0..120).map(|index| format!("f{index:03}.txt")).collect();
        let call = json!({
            "token": 9,
            "leg": "enter",
            "call": {
                "call_id": "c9",
                "root_call_id": "c9",
                "name": "glob",
                "agent": "s1",
            },
            "result": {
                "kind": "success",
                "value": { "root": ".", "paths": paths },
            },
        });
        let _ = serve(
            &deps,
            METHOD_GLOB_SPILL,
            &serde_json::to_vec(&call).expect("encode"),
        )
        .expect("enter");
        let reply = serve(
            &deps,
            METHOD_GLOB_SPILL,
            &serde_json::to_vec(&json!({
                "token": 9,
                "leg": "exit",
                "decision": { "kind": "accept", "additionalContexts": [] },
            }))
            .expect("encode"),
        )
        .expect("exit");
        let reply: Value = serde_json::from_slice(&reply).expect("decode");
        let text = reply["decision"]["content"][0]["text"]
            .as_str()
            .expect("text");
        assert!(
            text.contains(
                "The complete result could not be saved; narrow pattern or path to see more."
            ),
            "{text}"
        );
    }

    /// The sampling choice pins the flat head and the round-robin
    /// groups (the baked switch is the flat head; the sampled arm is
    /// exercised through the direct function).
    #[test]
    fn sampling_reproduces_the_head_when_flat() {
        let paths: Vec<String> = (0..9).map(|index| format!("{index}.txt")).collect();
        let sample = sample_across_top_level(&paths, 4, ".");
        assert_eq!(sample.items, paths[..4].to_vec());
        assert_eq!(sample.shown, 4);
        assert_eq!(sample.total, 9);

        let grouped: Vec<String> = [
            "a/1", "a/2", "b/1", "b/2", "c/1", "c/2", "d/1", "d/2", "e/1",
        ]
        .iter()
        .map(|path| (*path).to_owned())
        .collect();
        let sample = sample_across_top_level(&grouped, 4, ".");
        assert_eq!(sample.items, vec!["a/1", "b/1", "c/1", "d/1"]);
        assert_eq!(sample.shown, 4);
        assert_eq!(sample.total, 5);
    }

    /// The composed argv builders pin the flag shapes and the VCS
    /// double-glob excludes.
    #[test]
    fn argv_builders_pin_the_shapes() {
        let argv = build_glob_command("*.ts", Some("src"));
        assert_eq!(argv[0], "--files");
        assert_eq!(argv[1], "--glob=*.ts");
        assert_eq!(argv[2], "--sort=modified");
        assert!(argv.contains(&"--no-ignore".to_owned()));
        assert!(argv.contains(&"--hidden".to_owned()));
        assert!(argv.contains(&"--glob=!**/.git".to_owned()));
        assert!(argv.contains(&"--glob=!**/.git/**".to_owned()));
        let target = argv.iter().position(|part| part == "--").expect("guard");
        assert_eq!(argv[target + 1], "src");

        let argv = build_grep_command("x", Some("src"), Some("*.ts"));
        assert_eq!(argv[0], "--json");
        assert_eq!(argv[1], "--regexp=x");
        assert_eq!(argv[2], "--glob=*.ts");
        let target = argv.iter().position(|part| part == "--").expect("guard");
        assert_eq!(argv[target + 1], "src");
    }

    /// The NDJSON record parser splits the match family from the
    /// framing types and pins the malformed refusals.
    #[test]
    fn record_parser_pins_the_families() {
        let line = r#"{"type":"match","data":{"path":{"text":"a.rs"},"line_number":3,"lines":{"text":"let x\n"}}}"#;
        let parsed = parse_record(line).expect("match").expect("some");
        assert_eq!(parsed.line, "let x");
        assert_eq!(parsed.line_number, 3);
        let bytes_line = r#"{"type":"match","data":{"path":{"text":"a.rs"},"line_number":4,"lines":{"bytes":"AA=="}}}"#;
        let parsed = parse_record(bytes_line).expect("match").expect("some");
        assert_eq!(parsed.line, "(line is not valid UTF-8)");
        for frame in [
            r#"{"type":"begin"}"#,
            r#"{"type":"end"}"#,
            r#"{"type":"summary"}"#,
        ] {
            assert_eq!(parse_record(frame).expect("skip"), None);
        }
        let malformed = parse_record("not json").expect_err("not json");
        assert_eq!(
            malformed.message,
            "grep received malformed ripgrep --json output (a line is not JSON)"
        );
        let no_path =
            parse_record(r#"{"type":"match","data":{"line_number":1,"lines":{"text":"x"}}}"#)
                .expect_err("no path");
        assert!(
            no_path.message.contains("no path text"),
            "{}",
            no_path.message
        );
        let no_lines = parse_record(
            r#"{"type":"match","data":{"path":{"text":"a"},"line_number":1,"lines":{}}}"#,
        )
        .expect_err("no lines");
        assert!(
            no_lines.message.contains("neither line text nor bytes"),
            "{}",
            no_lines.message
        );
        let no_content =
            parse_record(r#"{"type":"match","data":{"path":{"text":"a"},"line_number":1}}"#)
                .expect_err("no line content");
        assert!(
            no_content.message.contains("no line content"),
            "{}",
            no_content.message
        );
    }
}
