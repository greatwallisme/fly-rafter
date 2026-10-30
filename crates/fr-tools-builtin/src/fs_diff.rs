// Rust guideline compliant 2026-10-30
//! Result-time contextual diff presentation for write and edit: the
//! dsh `tool-fs/src/diff.ts` over the locked `diff` 9.0.0 engine.
//!
//! The port carries the engine's line tokenizer, the greedy Myers core
//! of `diff/libesm/diff/base.js`, and the `structuredPatch` hunk
//! builder of `diff/libesm/patch/create.js`. Storage returns
//! before/after text; this model-facing layer derives one
//! three-line-context card per applied hunk. The port keeps the
//! engine's branch decisions verbatim — the diagonal branch preference
//! (`removePath.oldPos < addPath.oldPos` favors an insertion), the
//! diagonal-window pruning, and the overlap rule
//! (`lines.length <= context * 2 && i < diff.length - 2`) — so the
//! hunk boundaries match the dsh output byte for byte on the same
//! inputs.

/// Context lines shown on each side of an applied hunk (the dsh
/// `DIFF_CONTEXT`).
pub const DIFF_CONTEXT: usize = 3;

/// One per-hunk file diff (the dsh `FileDiff` payload shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiffHunk {
    /// The changed file's path (the model-facing `file_path`).
    pub path: String,
    /// Prior content, or `None` for a pure insertion.
    pub old_text: Option<String>,
    /// Content after the change.
    pub new_text: String,
}

/// One unified hunk (the dsh `structuredPatch` hunk object).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchHunk {
    /// 1-based old-file start line (possibly non-positive when the
    /// leading context exceeds the file head, mirroring the engine).
    pub old_start: i64,
    /// Old-file line count the hunk covers.
    pub old_lines: i64,
    /// 1-based new-file start line.
    pub new_start: i64,
    /// New-file line count the hunk covers.
    pub new_lines: i64,
    /// The hunk body: `' '`/`'-'`/`'+'`-prefixed lines with their
    /// trailing newlines stripped, and the engine's
    /// `\ No newline at end of file` markers in place.
    pub lines: Vec<String>,
}

/// One diff component with its already-projected lines (the dsh
/// `buildValues` output entry).
#[derive(Debug, Clone)]
struct DiffPart {
    lines: Vec<String>,
    added: bool,
    removed: bool,
}

/// A best-path node (the dsh `bestPath` entries).
#[derive(Debug, Clone, Copy)]
struct PathNode {
    old_pos: i64,
    last: Option<usize>,
}

/// One linked component in the shared arena (the dsh `lastComponent`
/// chains; tips are never shared, history is).
#[derive(Debug, Clone, Copy)]
struct Component {
    count: u64,
    added: bool,
    removed: bool,
    previous: Option<usize>,
}

/// The dsh `diffLines` tokenizer: split keeping the `\n`/`\r\n`
/// separators, drop the trailing empty split part, merge each separator
/// into its line, then drop the falsy tokens.
fn tokenize(value: &str) -> Vec<String> {
    let mut raw: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' && chars.peek() == Some(&'\n') {
            chars.next();
            raw.push(std::mem::take(&mut current));
            raw.push("\r\n".to_owned());
        } else if ch == '\n' {
            raw.push(std::mem::take(&mut current));
            raw.push("\n".to_owned());
        } else {
            current.push(ch);
        }
    }
    // The trailing content segment rides as the final element (the
    // dsh split keeps it), and the final empty token a trailing
    // newline leaves pops — one pop only.
    raw.push(std::mem::take(&mut current));
    if raw.last().is_some_and(String::is_empty) {
        raw.pop();
    }
    let mut tokens: Vec<String> = Vec::with_capacity(raw.len());
    let mut index = 0_usize;
    while index < raw.len() {
        if index % 2 == 1 {
            if let Some(last) = tokens.last_mut() {
                last.push_str(&raw[index]);
            }
        } else {
            tokens.push(raw[index].clone());
        }
        index += 1;
    }
    tokens.retain(|token| !token.is_empty());
    tokens
}

/// Projects the linked components onto ordered diff parts (the dsh
/// `buildValues`; removed parts draw from the old tokens, everything
/// else from the new).
fn build_values(
    last: Option<usize>,
    arena: &[Component],
    old_tokens: &[String],
    new_tokens: &[String],
) -> Vec<DiffPart> {
    let mut chain: Vec<usize> = Vec::new();
    let mut node = last;
    while let Some(index) = node {
        chain.push(index);
        node = arena[index].previous;
    }
    chain.reverse();
    let mut parts: Vec<DiffPart> = Vec::with_capacity(chain.len());
    let mut new_pos = 0_usize;
    let mut old_pos = 0_usize;
    for index in chain {
        let component = &arena[index];
        // Counts originate from token-slice lengths (usize), so the
        // wide-pointer truncation cannot fire.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "counts originate from token-slice lengths (usize)"
        )]
        let count = component.count as usize;
        if component.removed {
            parts.push(DiffPart {
                lines: old_tokens[old_pos..old_pos + count].to_vec(),
                added: false,
                removed: true,
            });
            old_pos += count;
        } else {
            parts.push(DiffPart {
                lines: new_tokens[new_pos..new_pos + count].to_vec(),
                added: component.added,
                removed: false,
            });
            new_pos += count;
            if !component.added {
                old_pos += count;
            }
        }
    }
    parts
}

/// Extends one path by a single insertion or deletion (the dsh
/// `addToPath`: a same-kind tip folds its count forward).
fn add_to_path(
    node: &PathNode,
    added: bool,
    removed: bool,
    old_pos_inc: i64,
    arena: &mut Vec<Component>,
) -> PathNode {
    let new_old_pos = node.old_pos + old_pos_inc;
    let extended = node
        .last
        .is_some_and(|tip| arena[tip].added == added && arena[tip].removed == removed);
    let component = if extended {
        let tip = node.last.expect("checked above");
        Component {
            count: arena[tip].count + 1,
            added,
            removed,
            previous: arena[tip].previous,
        }
    } else {
        Component {
            count: 1,
            added,
            removed,
            previous: node.last,
        }
    };
    arena.push(component);
    PathNode {
        old_pos: new_old_pos,
        last: Some(arena.len() - 1),
    }
}

/// Consumes the maximal equal run from one path tip (the dsh
/// `extractCommon`), appending one common component when nonzero.
fn extract_common(
    node: &mut PathNode,
    arena: &mut Vec<Component>,
    diagonal_path: i64,
    old_tokens: &[String],
    new_tokens: &[String],
) -> i64 {
    #[expect(
        clippy::cast_possible_wrap,
        reason = "token counts fit an i64 for any real allocation"
    )]
    let old_len = old_tokens.len() as i64;
    #[expect(
        clippy::cast_possible_wrap,
        reason = "token counts fit an i64 for any real allocation"
    )]
    let new_len = new_tokens.len() as i64;
    let mut old_pos = node.old_pos;
    let mut new_pos = old_pos - diagonal_path;
    let mut common_count = 0_u64;
    // The walk keeps both positions inside their token counts, so the
    // index cast never loses sign or magnitude.
    #[expect(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "the walk keeps both positions inside their token counts"
    )]
    while new_pos + 1 < new_len
        && old_pos + 1 < old_len
        && old_tokens[(old_pos + 1) as usize] == new_tokens[(new_pos + 1) as usize]
    {
        new_pos += 1;
        old_pos += 1;
        common_count += 1;
    }
    if common_count > 0 {
        arena.push(Component {
            count: common_count,
            added: false,
            removed: false,
            previous: node.last,
        });
        node.last = Some(arena.len() - 1);
    }
    node.old_pos = old_pos;
    new_pos
}

/// The greedy Myers core (the dsh `diffWithOptionsObj`): per-diagonal
/// best paths, the insertion-favoring branch rule, and the diagonal
/// window pruning once an edge of the edit graph is reached.
///
/// Slots a diagonal into the best-path vector (the dsh `bestPath`
/// array indexing; the diagonal space is offset-bounded).
#[expect(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "diagonals stay within the offset-bounded slot space; the offset is a bounded non-negative count"
)]
const fn slot(offset: usize, k: i64) -> usize {
    (k + offset as i64) as usize
}
fn diff_components(old_tokens: &[String], new_tokens: &[String]) -> Vec<DiffPart> {
    // The engine runs on JS numbers; the token counts fit an i64 for
    // any allocation the process could hold.
    #[expect(
        clippy::cast_possible_wrap,
        reason = "token counts fit an i64 for any real allocation"
    )]
    let old_len = old_tokens.len() as i64;
    #[expect(
        clippy::cast_possible_wrap,
        reason = "token counts fit an i64 for any real allocation"
    )]
    let new_len = new_tokens.len() as i64;
    let max_edit_length = old_len + new_len;
    #[expect(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "the offset is a non-negative count"
    )]
    let offset = (max_edit_length + 1) as usize;
    let mut best_path: Vec<Option<PathNode>> = vec![None; 2 * offset + 1];
    let mut arena: Vec<Component> = Vec::new();
    let mut min_diagonal = i64::MIN;
    let mut max_diagonal = i64::MAX;
    // The seed: d = 0 consumes the common prefix before the first
    // edit-length pass.
    let mut seed = PathNode {
        old_pos: -1,
        last: None,
    };
    let seed_new_pos = extract_common(&mut seed, &mut arena, 0, old_tokens, new_tokens);
    if seed.old_pos + 1 >= old_len && seed_new_pos + 1 >= new_len {
        return build_values(seed.last, &arena, old_tokens, new_tokens);
    }
    best_path[slot(offset, 0)] = Some(seed);

    let mut edit_length: i64 = 1;
    loop {
        let mut diagonal = min_diagonal.max(-edit_length);
        let high = max_diagonal.min(edit_length);
        while diagonal <= high {
            let remove_path = best_path[slot(offset, diagonal - 1)].take();
            let add_path = best_path[slot(offset, diagonal + 1)];
            let can_add = add_path.is_some_and(|path| {
                let add_path_new_pos = path.old_pos - diagonal;
                0 <= add_path_new_pos && add_path_new_pos < new_len
            });
            let can_remove = remove_path.is_some_and(|path| path.old_pos + 1 < old_len);
            if !can_add && !can_remove {
                best_path[slot(offset, diagonal)] = None;
                diagonal += 2;
                continue;
            }
            let branch_add = !can_remove
                || (can_add
                    && remove_path.is_some_and(|removed| {
                        add_path.is_some_and(|added| removed.old_pos < added.old_pos)
                    }));
            let mut base = if branch_add {
                add_to_path(
                    &add_path.expect("can_add holds"),
                    true,
                    false,
                    0,
                    &mut arena,
                )
            } else {
                add_to_path(
                    &remove_path.expect("the remove branch holds"),
                    false,
                    true,
                    1,
                    &mut arena,
                )
            };
            let new_pos = extract_common(&mut base, &mut arena, diagonal, old_tokens, new_tokens);
            if base.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
                return build_values(base.last, &arena, old_tokens, new_tokens);
            }
            best_path[slot(offset, diagonal)] = Some(base);
            if base.old_pos + 1 >= old_len {
                max_diagonal = max_diagonal.min(diagonal - 1);
            }
            if new_pos + 1 >= new_len {
                min_diagonal = min_diagonal.max(diagonal + 1);
            }
            diagonal += 2;
        }
        edit_length += 1;
        if edit_length > max_edit_length {
            // Unreachable: the edit graph needs at most old+new edits.
            return Vec::new();
        }
    }
}

/// The dsh `structuredPatch` over line tokens with the tool's context.
pub(crate) fn structured_patch(before: &str, after: &str, context: usize) -> Vec<PatchHunk> {
    let parts = diff_components(&tokenize(before), &tokenize(after));
    build_patch(context, parts)
}

/// Groups the diff parts into unified hunks (the dsh `buildPatch` with
/// its trailing empty part, the overlap join, and the step-2 newline
/// strip plus missing-newline markers).
#[expect(
    clippy::too_many_lines,
    reason = "the hunk-builder mirrors the dsh loop arm for arm; splitting it would hide the correspondence"
)]
fn build_patch(context: usize, mut parts: Vec<DiffPart>) -> Vec<PatchHunk> {
    parts.push(DiffPart {
        lines: Vec::new(),
        added: false,
        removed: false,
    });
    #[expect(
        clippy::cast_possible_wrap,
        reason = "part counts fit an i64 for any real allocation"
    )]
    let total = parts.len() as i64;
    let mut hunks: Vec<PatchHunk> = Vec::new();
    let mut old_range_start: i64 = 0;
    let mut new_range_start: i64 = 0;
    let mut cur_range: Vec<String> = Vec::new();
    let mut old_line: i64 = 1;
    let mut new_line: i64 = 1;
    for (index, current) in parts.iter().enumerate() {
        let lines = &current.lines;
        if current.added || current.removed {
            if old_range_start == 0 {
                let prev = index
                    .checked_sub(1)
                    .and_then(|previous| parts.get(previous));
                old_range_start = old_line;
                new_range_start = new_line;
                if let Some(prev) = prev {
                    if context > 0 {
                        let start = prev.lines.len().saturating_sub(context);
                        cur_range = prev.lines[start..]
                            .iter()
                            .map(|line| format!(" {line}"))
                            .collect();
                    }
                    #[expect(
                        clippy::cast_possible_wrap,
                        reason = "context-run lengths fit an i64 for any real allocation"
                    )]
                    let context_len = cur_range.len() as i64;
                    old_range_start -= context_len;
                    new_range_start -= context_len;
                }
            }
            for line in lines {
                cur_range.push(format!("{}{}", if current.added { '+' } else { '-' }, line));
            }
            #[expect(
                clippy::cast_possible_wrap,
                reason = "component line counts fit an i64 for any real allocation"
            )]
            let line_count = lines.len() as i64;
            if current.added {
                new_line += line_count;
            } else {
                old_line += line_count;
            }
        } else if old_range_start != 0 {
            #[expect(
                clippy::cast_possible_wrap,
                reason = "the loop index fits an i64 for any real allocation"
            )]
            let ordinal = index as i64;
            if lines.len() <= context * 2 && ordinal < total - 2 {
                for line in lines {
                    cur_range.push(format!(" {line}"));
                }
            } else {
                let context_size = std::cmp::min(lines.len(), context);
                for line in &lines[..context_size] {
                    cur_range.push(format!(" {line}"));
                }
                #[expect(
                    clippy::cast_possible_wrap,
                    reason = "context sizes fit an i64 for any real allocation"
                )]
                let context_width = context_size as i64;
                hunks.push(PatchHunk {
                    old_start: old_range_start,
                    old_lines: old_line - old_range_start + context_width,
                    new_start: new_range_start,
                    new_lines: new_line - new_range_start + context_width,
                    lines: std::mem::take(&mut cur_range),
                });
                old_range_start = 0;
                new_range_start = 0;
            }
            #[expect(
                clippy::cast_possible_wrap,
                reason = "component line counts fit an i64 for any real allocation"
            )]
            let tail_count = lines.len() as i64;
            old_line += tail_count;
            new_line += tail_count;
        } else {
            #[expect(
                clippy::cast_possible_wrap,
                reason = "component line counts fit an i64 for any real allocation"
            )]
            let tail_count = lines.len() as i64;
            old_line += tail_count;
            new_line += tail_count;
        }
    }

    // Step 2: strip each line's trailing newline, inserting the
    // missing-newline marker where a line had none.
    for hunk in &mut hunks {
        let mut index = 0_usize;
        while index < hunk.lines.len() {
            if hunk.lines[index].ends_with('\n') {
                hunk.lines[index].pop();
            } else {
                hunk.lines
                    .insert(index + 1, "\\ No newline at end of file".to_owned());
                index += 1;
            }
            index += 1;
        }
    }
    hunks
}

/// Compute one [`FileDiffHunk`] per hunk between `before` and `after`.
///
/// Each hunk carries the applied change plus [`DIFF_CONTEXT`] context
/// lines (the dsh `computeHunkDiffs`; pure insertions use
/// `old_text: None`, the engine's missing-newline markers are skipped,
/// and scattered replacements remain separate hunks).
#[must_use]
pub fn compute_hunk_diffs(path: &str, before: &str, after: &str) -> Vec<FileDiffHunk> {
    let hunks = structured_patch(before, after, DIFF_CONTEXT);
    let mut diffs: Vec<FileDiffHunk> = Vec::new();
    for hunk in &hunks {
        let mut old_lines: Vec<&str> = Vec::new();
        let mut new_lines: Vec<&str> = Vec::new();
        for line in &hunk.lines {
            // The unified-diff marker for a missing trailing newline
            // annotates the patch, not the content — skip it.
            if line.starts_with('\\') {
                continue;
            }
            let Some(text) = line.get(1..) else {
                continue;
            };
            if let Some(body) = line.strip_prefix('-') {
                old_lines.push(body);
            } else if let Some(body) = line.strip_prefix('+') {
                new_lines.push(body);
            } else {
                old_lines.push(text);
                new_lines.push(text);
            }
        }
        diffs.push(FileDiffHunk {
            path: path.to_owned(),
            old_text: if old_lines.is_empty() {
                None
            } else {
                Some(old_lines.join("\n"))
            },
            new_text: new_lines.join("\n"),
        });
    }
    diffs
}

/// Whether one recorded diff entry is a valid [`FileDiffHunk`] shape
/// (the dsh `isFileDiff` defensive narrowing).
fn is_file_diff(value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let path_ok = object.get("path").is_some_and(serde_json::Value::is_string);
    let old_text_ok = match object.get("oldText") {
        Some(serde_json::Value::Null) => true,
        Some(text) => text.is_string(),
        None => false,
    };
    let new_text_ok = object
        .get("newText")
        .is_some_and(serde_json::Value::is_string);
    path_ok && old_text_ok && new_text_ok
}

/// Narrow opaque live or replayed result metadata to non-empty file
/// diffs (the dsh `diffsFromMeta`; malformed metadata returns `None` so
/// presentation falls back instead of throwing during replay).
#[must_use]
pub fn diffs_from_meta(meta: Option<&serde_json::Value>) -> Option<Vec<FileDiffHunk>> {
    let diffs = meta?.get("diffs")?.as_array()?;
    if diffs.is_empty() || !diffs.iter().all(is_file_diff) {
        return None;
    }
    Some(
        diffs
            .iter()
            .map(|entry| FileDiffHunk {
                path: entry["path"].as_str().unwrap_or_default().to_owned(),
                old_text: entry["oldText"].as_str().map(str::to_owned),
                new_text: entry["newText"].as_str().unwrap_or_default().to_owned(),
            })
            .collect(),
    )
}

/// Read the operation's file path even when the applied diff is empty
/// (the dsh `pathFromMeta`).
#[must_use]
pub fn path_from_meta(meta: Option<&serde_json::Value>) -> Option<String> {
    let path = meta?.get("path")?.as_str()?;
    (!path.is_empty()).then(|| path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A single mid-file replacement: one hunk with three-line-style
    /// context, exact ranges, and the change lines.
    #[test]
    fn single_replacement_produces_one_context_hunk() {
        let diffs = compute_hunk_diffs("/w/a.txt", "a\nb\nc\nd\ne\n", "a\nb\nX\nd\ne\n");
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].path, "/w/a.txt");
        // Both leading lines ride as context (the run is shorter than
        // the three-line window).
        assert_eq!(diffs[0].old_text.as_deref(), Some("a\nb\nc\nd\ne"));
        assert_eq!(diffs[0].new_text, "a\nb\nX\nd\ne");
        let patch = structured_patch("a\nb\nc\nd\ne\n", "a\nb\nX\nd\ne\n", DIFF_CONTEXT);
        assert_eq!(patch.len(), 1);
        assert_eq!(patch[0].old_start, 1);
        assert_eq!(patch[0].old_lines, 5);
        assert_eq!(patch[0].new_start, 1);
        assert_eq!(patch[0].new_lines, 5);
        assert_eq!(patch[0].lines, vec![" a", " b", "-c", "+X", " d", " e"]);

        // A taller file pins the exact three-line window on both sides.
        let tall_before = "l1\nl2\nl3\nl4\nl5\nl6\nl7\n";
        let tall_after = "l1\nl2\nl3\nX\nl5\nl6\nl7\n";
        let tall = compute_hunk_diffs("/w/b.txt", tall_before, tall_after);
        assert_eq!(tall.len(), 1);
        assert_eq!(
            tall[0].old_text.as_deref(),
            Some("l1\nl2\nl3\nl4\nl5\nl6\nl7")
        );
        assert_eq!(tall[0].new_text, "l1\nl2\nl3\nX\nl5\nl6\nl7");
        let tall_patch = structured_patch(tall_before, tall_after, DIFF_CONTEXT);
        assert_eq!(
            tall_patch[0].lines,
            vec![" l1", " l2", " l3", "-l4", "+X", " l5", " l6", " l7"]
        );
    }

    /// Identical texts produce no hunks; an emptied file produces the
    /// pure-deletion arm; a created file produces `oldText: null`.
    #[test]
    fn identity_empty_and_create_arms() {
        assert_eq!(
            compute_hunk_diffs("/w/a", "same\n", "same\n"),
            Vec::<FileDiffHunk>::new()
        );
        let emptied = compute_hunk_diffs("/w/a", "one\ntwo\n", "");
        assert_eq!(emptied.len(), 1);
        assert_eq!(emptied[0].old_text.as_deref(), Some("one\ntwo"));
        assert_eq!(emptied[0].new_text, "");
        let created = compute_hunk_diffs("/w/a", "", "one\ntwo\n");
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].old_text, None);
        assert_eq!(created[0].new_text, "one\ntwo");
    }

    /// Changes separated by more than twice the context stay in
    /// separate hunks.
    #[test]
    fn scattered_changes_stay_separate_hunks() {
        let mut before = String::new();
        let mut after_lines: Vec<String> = Vec::new();
        for n in 1..=12 {
            before.push_str("line");
            before.push_str(&n.to_string());
            before.push('\n');
            after_lines.push(format!("line{n}"));
        }
        after_lines[1] = "X".to_owned();
        after_lines[9] = "Y".to_owned();
        let mut after = String::new();
        for line in &after_lines {
            after.push_str(line);
            after.push('\n');
        }
        let diffs = compute_hunk_diffs("/w/a", &before, &after);
        assert_eq!(diffs.len(), 2);
        let patch = structured_patch(&before, &after, DIFF_CONTEXT);
        assert_eq!(patch.len(), 2);
        assert_eq!(
            patch[0].lines,
            vec![" line1", "-line2", "+X", " line3", " line4", " line5"]
        );
        assert_eq!(
            patch[1].lines,
            vec![
                " line7", " line8", " line9", "-line10", "+Y", " line11", " line12"
            ]
        );
    }

    /// Missing trailing newlines carry the engine's markers, which the
    /// diff projection skips so they never leak into a card.
    #[test]
    fn missing_newline_markers_ride_and_get_skipped() {
        let diffs = compute_hunk_diffs("/w/a", "a\nb", "a\nX");
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].old_text.as_deref(), Some("a\nb"));
        assert_eq!(diffs[0].new_text, "a\nX");
        let patch = structured_patch("a\nb", "a\nX", DIFF_CONTEXT);
        assert_eq!(
            patch[0].lines,
            vec![
                " a",
                "-b",
                "\\ No newline at end of file",
                "+X",
                "\\ No newline at end of file",
            ]
        );
    }

    /// The tokenizer keeps empty lines as newline tokens and merges the
    /// CRLF separator into its line (the dsh `tokenize`).
    #[test]
    fn tokenizer_keeps_empty_lines_and_crlf() {
        assert_eq!(tokenize("a\n\nb\n"), vec!["a\n", "\n", "b\n"]);
        assert_eq!(tokenize("a\r\nb\r\n"), vec!["a\r\n", "b\r\n"]);
        assert_eq!(tokenize("a\nb"), vec!["a\n", "b"]);
        assert_eq!(tokenize(""), Vec::<String>::new());
    }
}
