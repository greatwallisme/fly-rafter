// Rust guideline compliant 2026-10-28
//! Model-facing workspace instruction rendering within an explicit byte
//! budget (dsh agent-instructions/src/render.ts, verbatim semantics).
//!
//! The renderer owns the complete `<system-reminder>` frame, neutralizes
//! closing delimiters that appear inside instruction content, paths, or
//! budget markers, and walks a deterministic budget ladder: whole text
//! first, then broadest-file omission, then binary-search truncation of
//! the most specific file, then the compact intro, then a bare notice.

use serde::{Deserialize, Serialize};

use crate::instructions::config::{
    AGENTS_HOME_ENV, DEFAULT_AGENTS_HOME_DISPLAY, DEFAULT_FR_HOME_DISPLAY, FR_HOME_ENV,
};
use crate::prompts::{
    ADDITIONAL_HEADING_TEMPLATE, ADDITIONAL_INSTRUCTIONS_GUIDANCE, AGENT_INSTRUCTIONS_INTRO,
    COMPACT_AGENT_INSTRUCTIONS_INTRO, EMPTY_REPLACEMENT_AGENT_INSTRUCTIONS_INTRO,
    ESCAPED_SYSTEM_REMINDER_CLOSE, MARKER_ITEMS_SEPARATOR, MARKER_LINE_TEMPLATE,
    MARKER_OMITTED_TEMPLATE, MARKER_PARTS_SEPARATOR, MARKER_TRUNCATED_LIST_TEMPLATE,
    MARKER_TRUNCATED_TEMPLATE, REMOVED_SECTION_TEMPLATE, REPLACEMENT_AGENT_INSTRUCTIONS_INTRO,
    SCOPE_APPLICABILITY_TEMPLATE, SECTION_TEXT_TEMPLATE, SYSTEM_REMINDER_CLOSE,
    SYSTEM_REMINDER_OPEN, UPDATED_HEADING_TEMPLATE, UPDATED_NOTICE, USER_GLOBAL_APPLICABILITY,
};

/// An instruction candidate identified by absolute and model-facing paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionFile {
    /// The absolute filesystem path.
    pub absolute_path: String,
    /// The model-facing path.
    pub display_path: String,
}

/// An instruction file whose UTF-8 content was read successfully.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedInstructionFile {
    /// The absolute filesystem path.
    pub absolute_path: String,
    /// The model-facing path.
    pub display_path: String,
    /// The exact file text.
    pub content: String,
    /// Provider freshness token, when the read carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl LoadedInstructionFile {
    /// The path-only view of this file.
    #[must_use]
    pub fn as_instruction_file(&self) -> InstructionFile {
        InstructionFile {
            absolute_path: self.absolute_path.clone(),
            display_path: self.display_path.clone(),
        }
    }
}

/// Byte-accounting record for one truncated instruction file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TruncatedInstruction {
    /// The model-facing path.
    pub display_path: String,
    /// The original UTF-8 byte count.
    pub original_bytes: usize,
    /// The retained UTF-8 byte count.
    pub included_bytes: usize,
}

/// Model-facing text plus omitted and truncated source records.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderedAgentInstructions {
    /// The framed instruction text.
    pub text: String,
    /// Files the budget omitted entirely.
    pub omitted: Vec<InstructionFile>,
    /// Files the budget truncated.
    pub truncated: Vec<TruncatedInstruction>,
}

/// The action of one workspace instruction transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstructionChangeAction {
    /// A newly visible instruction file.
    Set,
    /// Changed content under an already-visible path.
    Replace,
    /// A previously visible file that no longer applies.
    Remove,
}

/// Structured dynamic state persisted outside model-visible prompt prose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInstructionChange {
    /// The transition kind.
    pub action: InstructionChangeAction,
    /// The per-candidate logical scope key.
    pub scope: String,
    /// The model-facing path.
    pub path: String,
    /// The content digest, when the file was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

/// One state transition paired with the content used to render it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRenderItem {
    /// The transition.
    pub change: AgentInstructionChange,
    /// The current file content.
    pub file: LoadedInstructionFile,
}

/// Directory component identifying the harness-home user-global scope.
pub const USER_GLOBAL_DIRECTORY: &str = "user-global";

/// Directory component identifying the shared agents-root user-global
/// scope; distinct as a scope, one candidate group with the harness home.
pub const AGENTS_GLOBAL_DIRECTORY: &str = "agents-global";

/// Ordered user-global scope directories: discovery, reconciliation, and
/// deduplication all iterate this order.
pub const USER_GLOBAL_DIRECTORIES: &[&str] = &[USER_GLOBAL_DIRECTORY, AGENTS_GLOBAL_DIRECTORY];

/// File name of the user-global instruction file under each root.
pub const USER_GLOBAL_FILE: &str = "AGENTS.md";

/// The scope separator no directory path or file name can contain.
const SCOPE_SEPARATOR: char = '\u{0}';

/// The path separator for display paths (dsh uses the platform separator;
/// fr targets the POSIX separator).
const SEP: char = '/';

/// `path.dirname` for display paths.
#[must_use]
pub fn dirname_of(path: &str) -> String {
    path.rfind(SEP).map_or_else(
        || ".".to_string(),
        |at| {
            if at == 0 {
                "/".to_string()
            } else {
                path[..at].to_string()
            }
        },
    )
}

/// `path.basename` for display paths.
#[must_use]
pub fn basename_of(path: &str) -> String {
    path.rfind(SEP)
        .map_or_else(|| path.to_string(), |at| path[at + 1..].to_string())
}

/// Derive the logical instruction scope from a model-facing path (dsh
/// `scopeForDisplayPath`).
///
/// The two display spellings of each user-global root map to their scope
/// directory; everything else maps to its containing directory.
#[must_use]
pub fn scope_for_display_path(display_path: &str) -> String {
    if display_path == format!("{DEFAULT_FR_HOME_DISPLAY}/{USER_GLOBAL_FILE}")
        || display_path == format!("${FR_HOME_ENV}/{USER_GLOBAL_FILE}")
    {
        return USER_GLOBAL_DIRECTORY.to_string();
    }
    if display_path == format!("{DEFAULT_AGENTS_HOME_DISPLAY}/{USER_GLOBAL_FILE}")
        || display_path == format!("${AGENTS_HOME_ENV}/{USER_GLOBAL_FILE}")
    {
        return AGENTS_GLOBAL_DIRECTORY.to_string();
    }
    dirname_of(display_path)
}

/// Whether a decoded scope directory is one of the two user-global roots.
#[must_use]
pub fn is_user_global_directory(directory: &str) -> bool {
    directory == USER_GLOBAL_DIRECTORY || directory == AGENTS_GLOBAL_DIRECTORY
}

/// Candidate group of one decoded scope directory: both user-global roots
/// form one group so identical trimmed content renders once; every project
/// directory forms its own group.
#[must_use]
pub fn instruction_candidate_group(directory: &str) -> String {
    if directory == AGENTS_GLOBAL_DIRECTORY {
        USER_GLOBAL_DIRECTORY.to_string()
    } else {
        directory.to_string()
    }
}

/// Keep a project-relative path out of the user-global scope namespace: a
/// first component colliding with a user-global directory gains a leading
/// `./` (dsh `escapeProjectDisplayPath`).
#[must_use]
pub fn escape_project_display_path(relative_path: &str) -> String {
    let first = relative_path
        .find(SEP)
        .map_or(relative_path, |at| &relative_path[..at]);
    if USER_GLOBAL_DIRECTORIES.contains(&first) {
        format!(".{SEP}{relative_path}")
    } else {
        relative_path.to_string()
    }
}

/// Compose the reconciliation key for one instruction candidate: the
/// logical directory and the exact candidate name behind a NUL separator.
#[must_use]
pub fn candidate_scope_key(directory: &str, candidate_name: &str) -> String {
    format!("{directory}{SCOPE_SEPARATOR}{candidate_name}")
}

/// Derive the per-candidate scope key for a loaded instruction file.
#[must_use]
pub fn instruction_scope_key(display_path: &str) -> String {
    candidate_scope_key(
        &scope_for_display_path(display_path),
        &basename_of(display_path),
    )
}

/// Recover the directory and candidate name a scope key encoded.
#[must_use]
pub fn decode_scope_key(scope: &str) -> (String, String) {
    scope.find(SCOPE_SEPARATOR).map_or_else(
        || (scope.to_string(), String::new()),
        |at| {
            (
                scope[..at].to_string(),
                scope[at + SCOPE_SEPARATOR.len_utf8()..].to_string(),
            )
        },
    )
}

/// UTF-8 byte length (dsh `Buffer.byteLength`).
const fn byte_len(value: &str) -> usize {
    value.len()
}

/// Truncate to a byte budget on UTF-8 code-point boundaries: if the cut
/// lands mid code point, back up to its lead byte (dsh `truncateUtf8`).
#[must_use]
pub fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    let bytes = value.as_bytes();
    if bytes.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && (bytes[end] & 0xc0) == 0x80 {
        end -= 1;
    }
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Neutralize a literal closing delimiter inside instruction content (dsh
/// `escapeInstructionFrameBody`).
#[must_use]
pub fn escape_instruction_frame_body(body: &str) -> String {
    body.replace(SYSTEM_REMINDER_CLOSE, ESCAPED_SYSTEM_REMINDER_CLOSE)
}

/// The baseline section body (dsh `sectionText`).
#[must_use]
pub fn section_text(file: &LoadedInstructionFile) -> String {
    format_section(SECTION_TEXT_TEMPLATE, &[&file.display_path, &file.content])
}

/// The dynamic-batch section body (dsh `additionalSectionText`).
#[must_use]
pub fn additional_section_text(file: &LoadedInstructionFile) -> String {
    let scope = scope_for_display_path(&file.display_path);
    // A user-global scope directory is an internal reconciliation key, so
    // it must never reach the model-visible applicability sentence.
    let applicability = if is_user_global_directory(&scope) {
        USER_GLOBAL_APPLICABILITY.to_string()
    } else {
        format_section(SCOPE_APPLICABILITY_TEMPLATE, &[&scope])
    };
    format!(
        "{}\n\n{} {ADDITIONAL_INSTRUCTIONS_GUIDANCE}\n\n{}",
        format_section(ADDITIONAL_HEADING_TEMPLATE, &[&file.display_path]),
        applicability,
        file.content
    )
}

/// The transition section body (dsh `changedSectionText`).
#[must_use]
pub fn changed_section_text(item: &ChangeRenderItem) -> String {
    match item.change.action {
        InstructionChangeAction::Set => additional_section_text(&item.file),
        InstructionChangeAction::Remove => {
            format_section(REMOVED_SECTION_TEMPLATE, &[&item.change.path])
        }
        InstructionChangeAction::Replace => format!(
            "{}\n\n{UPDATED_NOTICE}\n\n{}",
            format_section(UPDATED_HEADING_TEMPLATE, &[&item.change.path]),
            item.file.content
        ),
    }
}

/// Fill one extracted template's `{name}` groups from `args` by position.
/// An unbalanced brace or a missing argument keeps the group verbatim,
/// which can only surface as a visible template token in a frame; every
/// template is a generated constant whose groups the call sites match
/// one-for-one, and the frame tests would catch any drift.
fn format_section(template: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    let mut next = 0;
    while let Some(open) = rest.find('{') {
        let Some(relative) = rest[open..].find('}') else {
            break;
        };
        let close = open + relative;
        out.push_str(&rest[..open]);
        if let Some(arg) = args.get(next) {
            out.push_str(arg);
        } else {
            out.push_str(&rest[open..=close]);
        }
        next += 1;
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// The budget marker line (dsh `markerText`).
#[must_use]
pub fn marker_text(
    max_bytes: usize,
    omitted: &[InstructionFile],
    truncated: &[TruncatedInstruction],
) -> String {
    if omitted.is_empty() && truncated.is_empty() {
        return String::new();
    }
    let mut parts: Vec<String> = Vec::new();
    if !omitted.is_empty() {
        let paths: Vec<&str> = omitted
            .iter()
            .map(|file| file.display_path.as_str())
            .collect();
        parts.push(format_section(
            MARKER_OMITTED_TEMPLATE,
            &[&paths.join(MARKER_ITEMS_SEPARATOR)],
        ));
    }
    if !truncated.is_empty() {
        let items: Vec<String> = truncated
            .iter()
            .map(|item| {
                format_section(
                    MARKER_TRUNCATED_TEMPLATE,
                    &[
                        &item.display_path,
                        &item.original_bytes.to_string(),
                        &item.included_bytes.to_string(),
                    ],
                )
            })
            .collect();
        parts.push(format_section(
            MARKER_TRUNCATED_LIST_TEMPLATE,
            &[&items.join(MARKER_ITEMS_SEPARATOR)],
        ));
    }
    format_section(
        MARKER_LINE_TEMPLATE,
        &[&max_bytes.to_string(), &parts.join(MARKER_PARTS_SEPARATOR)],
    )
}

/// One render pass's section voice (dsh `RenderStyle`).
struct RenderStyle<'a> {
    intro: String,
    section: Box<dyn Fn(&LoadedInstructionFile) -> String + 'a>,
}

/// Assemble the complete `<system-reminder>` frame (dsh
/// `buildInstructionText`): this producer owns the whole frame; the
/// session adds no wrapper.
fn build_instruction_text(
    files: &[LoadedInstructionFile],
    max_bytes: usize,
    omitted: &[InstructionFile],
    truncated: &[TruncatedInstruction],
    style: &RenderStyle<'_>,
) -> String {
    let marker = marker_text(max_bytes, omitted, truncated);
    let mut body: Vec<String> = Vec::new();
    if !marker.is_empty() {
        body.push(marker);
    }
    if !style.intro.is_empty() {
        body.push(style.intro.clone());
    }
    body.extend(files.iter().map(|file| (style.section)(file)));
    body.retain(|block| !block.is_empty());
    format!(
        "{SYSTEM_REMINDER_OPEN}\n{}\n{SYSTEM_REMINDER_CLOSE}",
        escape_instruction_frame_body(&body.join("\n\n"))
    )
}

/// The baseline render voice (dsh `baselineRenderStyle`).
fn baseline_render_style(replace_previous: bool, empty: bool) -> RenderStyle<'static> {
    let intro = if !replace_previous {
        AGENT_INSTRUCTIONS_INTRO.to_string()
    } else if empty {
        EMPTY_REPLACEMENT_AGENT_INSTRUCTIONS_INTRO.to_string()
    } else {
        REPLACEMENT_AGENT_INSTRUCTIONS_INTRO.to_string()
    };
    RenderStyle {
        intro,
        section: Box::new(section_text),
    }
}

/// Binary-search the largest content slice of one file that fits the
/// budget alongside the already-included files (dsh `truncateToFit`).
fn truncate_to_fit(
    file: &LoadedInstructionFile,
    included_files: &[LoadedInstructionFile],
    max_bytes: usize,
    omitted: &[InstructionFile],
    style: &RenderStyle<'_>,
) -> LoadedInstructionFile {
    let original_bytes = byte_len(&file.content);
    let mut best = LoadedInstructionFile {
        content: truncate_utf8(&file.content, 0),
        version: file.version.clone(),
        ..file.clone()
    };
    let mut low = 0;
    let mut high = original_bytes;
    while low <= high {
        let mid = usize::midpoint(low, high);
        let mut candidate = LoadedInstructionFile {
            content: truncate_utf8(&file.content, mid),
            version: file.version.clone(),
            ..file.clone()
        };
        let _ = &mut candidate;
        let truncated = vec![TruncatedInstruction {
            display_path: file.display_path.clone(),
            original_bytes,
            included_bytes: byte_len(&candidate.content),
        }];
        let mut rendered_files = included_files.to_vec();
        rendered_files.push(candidate.clone());
        let text = build_instruction_text(&rendered_files, max_bytes, omitted, &truncated, style);
        if byte_len(&text) <= max_bytes {
            best = candidate;
            low = mid + 1;
        } else {
            high = mid.saturating_sub(1).min(high);
            if mid == 0 {
                break;
            }
        }
    }
    best
}

/// The internal render with `represented` semantics (dsh
/// `renderInstructionContext`).
fn render_instruction_context(
    files: &[LoadedInstructionFile],
    max_bytes: usize,
    style: &RenderStyle<'_>,
) -> (RenderedAgentInstructions, Vec<LoadedInstructionFile>) {
    if max_bytes == 0 {
        return (
            RenderedAgentInstructions {
                text: String::new(),
                omitted: files
                    .iter()
                    .map(LoadedInstructionFile::as_instruction_file)
                    .collect(),
                truncated: Vec::new(),
            },
            Vec::new(),
        );
    }
    let full_text = build_instruction_text(files, max_bytes, &[], &[], style);
    if byte_len(&full_text) <= max_bytes {
        return (
            RenderedAgentInstructions {
                text: full_text,
                omitted: Vec::new(),
                truncated: Vec::new(),
            },
            files.to_vec(),
        );
    }
    // Drop the broadest files one at a time until the suffix fits.
    for start in 1..files.len() {
        let included = &files[start..];
        let omitted: Vec<InstructionFile> = files[..start]
            .iter()
            .map(LoadedInstructionFile::as_instruction_file)
            .collect();
        let suffix_text = build_instruction_text(included, max_bytes, &omitted, &[], style);
        if byte_len(&suffix_text) <= max_bytes {
            return (
                RenderedAgentInstructions {
                    text: suffix_text,
                    omitted,
                    truncated: Vec::new(),
                },
                included.to_vec(),
            );
        }
    }
    let Some((last, prefix)) = files.split_last() else {
        return (RenderedAgentInstructions::default(), Vec::new());
    };
    let omitted: Vec<InstructionFile> = prefix
        .iter()
        .map(LoadedInstructionFile::as_instruction_file)
        .collect();
    truncate_or_notice(last, max_bytes, &omitted, style)
}

/// The last rung of the budget ladder (dsh `renderInstructionContext`
/// tail): binary-search truncation of the most specific file under both
/// intros, then the compact notice, then the notice truncated to the
/// budget.
fn truncate_or_notice(
    last: &LoadedInstructionFile,
    max_bytes: usize,
    omitted: &[InstructionFile],
    style: &RenderStyle<'_>,
) -> (RenderedAgentInstructions, Vec<LoadedInstructionFile>) {
    let original_bytes = byte_len(&last.content);
    let compact_style = RenderStyle {
        intro: COMPACT_AGENT_INSTRUCTIONS_INTRO.to_string(),
        section: Box::new(|file: &LoadedInstructionFile| (style.section)(file)),
    };
    for candidate_style in [
        RenderStyle {
            intro: style.intro.clone(),
            section: Box::new(|file: &LoadedInstructionFile| (style.section)(file)),
        },
        compact_style,
    ] {
        let truncated_file = truncate_to_fit(last, &[], max_bytes, omitted, &candidate_style);
        let included_bytes = byte_len(&truncated_file.content);
        let truncated = vec![TruncatedInstruction {
            display_path: last.display_path.clone(),
            original_bytes,
            included_bytes,
        }];
        let text = build_instruction_text(
            std::slice::from_ref(&truncated_file),
            max_bytes,
            omitted,
            &truncated,
            &candidate_style,
        );
        if byte_len(&text) <= max_bytes {
            let represented = if included_bytes > 0 || original_bytes == 0 {
                vec![last.clone()]
            } else {
                Vec::new()
            };
            return (
                RenderedAgentInstructions {
                    text,
                    omitted: omitted.to_vec(),
                    truncated,
                },
                represented,
            );
        }
    }
    // Even the empty heading cannot fit: notice only, truncated to the
    // budget as the last resort.
    let truncated = vec![TruncatedInstruction {
        display_path: last.display_path.clone(),
        original_bytes,
        included_bytes: 0,
    }];
    let compact_notice =
        escape_instruction_frame_body(&marker_text(max_bytes, omitted, &truncated));
    let heading_only = LoadedInstructionFile {
        content: String::new(),
        version: None,
        ..last.clone()
    };
    let compact_with_heading = escape_instruction_frame_body(&format!(
        "{}\n\n{}",
        compact_notice,
        (style.section)(&heading_only)
    ));
    if byte_len(&compact_with_heading) <= max_bytes {
        return (
            RenderedAgentInstructions {
                text: compact_with_heading,
                omitted: omitted.to_vec(),
                truncated,
            },
            if original_bytes == 0 {
                vec![last.clone()]
            } else {
                Vec::new()
            },
        );
    }
    let text = if byte_len(&compact_notice) <= max_bytes {
        compact_notice
    } else {
        truncate_utf8(&compact_notice, max_bytes)
    };
    (
        RenderedAgentInstructions {
            text,
            omitted: omitted.to_vec(),
            truncated,
        },
        Vec::new(),
    )
}

/// Render a baseline together with the exact files semantically
/// represented in it (dsh `renderAgentInstructionSet`).
#[must_use]
pub fn render_agent_instruction_set(
    files: &[LoadedInstructionFile],
    max_bytes: usize,
    replace_previous_baseline: bool,
) -> (RenderedAgentInstructions, Vec<LoadedInstructionFile>) {
    let style = baseline_render_style(replace_previous_baseline, files.is_empty());
    render_instruction_context(files, max_bytes, &style)
}

/// Render the baseline instruction chain (dsh `renderAgentInstructions`).
#[must_use]
pub fn render_agent_instructions(
    files: &[LoadedInstructionFile],
    max_bytes: usize,
    replace_previous_baseline: bool,
) -> RenderedAgentInstructions {
    render_agent_instruction_set(files, max_bytes, replace_previous_baseline).0
}

/// Render one reconciliation batch and retain only transitions that fit
/// (dsh `renderInstructionChanges`).
#[must_use]
pub fn render_instruction_changes(
    items: &[ChangeRenderItem],
    max_bytes: usize,
) -> (String, Vec<AgentInstructionChange>) {
    let files: Vec<LoadedInstructionFile> = items.iter().map(|item| item.file.clone()).collect();
    let style = RenderStyle {
        intro: String::new(),
        section: Box::new(|file: &LoadedInstructionFile| {
            // The renderer receives exactly the files used to build this
            // lookup, so every file has its item.
            items
                .iter()
                .find(|item| item.file.absolute_path == file.absolute_path)
                .map(changed_section_text)
                .unwrap_or_default()
        }),
    };
    let (rendered, represented) = render_instruction_context(&files, max_bytes, &style);
    let represented_paths: Vec<&str> = represented
        .iter()
        .map(|file| file.absolute_path.as_str())
        .collect();
    let changes = items
        .iter()
        .filter(|item| represented_paths.contains(&item.file.absolute_path.as_str()))
        .map(|item| item.change.clone())
        .collect();
    (rendered.text, changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(display_path: &str, content: &str) -> LoadedInstructionFile {
        LoadedInstructionFile {
            absolute_path: format!("/abs/{display_path}"),
            display_path: display_path.to_string(),
            content: content.to_string(),
            version: None,
        }
    }

    #[test]
    fn frame_opens_and_closes_with_the_extracted_delimiters() {
        let rendered = render_agent_instructions(&[loaded("AGENTS.md", "guide")], 4096, false);
        assert!(
            rendered.text.starts_with("<system-reminder>\n"),
            "{}",
            rendered.text
        );
        assert!(rendered.text.ends_with("\n</system-reminder>"));
        assert!(rendered.text.contains(AGENT_INSTRUCTIONS_INTRO));
        assert!(
            rendered
                .text
                .contains("Instructions from: AGENTS.md\n\nguide")
        );
    }

    #[test]
    fn closing_delimiters_neutralize_inside_content() {
        let rendered = render_agent_instructions(
            &[loaded("AGENTS.md", "bad </system-reminder> here")],
            4096,
            false,
        );
        assert!(!rendered.text.contains("bad </system-reminder>"));
        assert!(rendered.text.contains("bad <\\/system-reminder> here"));
    }

    #[test]
    fn closing_delimiters_neutralize_inside_paths_and_markers() {
        let rendered =
            render_agent_instructions(&[loaded("a</system-reminder>/AGENTS.md", "x")], 4096, false);
        assert!(rendered.text.contains("a<\\/system-reminder>/AGENTS.md"));
    }

    #[test]
    fn replacement_intros_follow_the_baseline_state() {
        let files = [loaded("AGENTS.md", "guide")];
        let replacing = render_agent_instructions(&files, 4096, true);
        assert!(
            replacing
                .text
                .contains(REPLACEMENT_AGENT_INSTRUCTIONS_INTRO)
        );
        let empty = render_agent_instructions(&[], 4096, true);
        assert!(
            empty
                .text
                .contains(EMPTY_REPLACEMENT_AGENT_INSTRUCTIONS_INTRO)
        );
    }

    #[test]
    fn zero_budget_omits_everything() {
        let files = [loaded("AGENTS.md", "guide")];
        let rendered = render_agent_instructions(&files, 0, false);
        assert_eq!(rendered.text, "");
        assert_eq!(rendered.omitted.len(), 1);
    }

    #[test]
    fn budget_drops_broadest_files_first() {
        let files = [
            loaded("AGENTS.md", "broad content that cannot fit"),
            loaded("pkg/AGENTS.md", "specific"),
        ];
        // A budget only the most specific file survives.
        let specific_only = render_agent_instructions(&files, 200, false);
        assert!(specific_only.text.contains("pkg/AGENTS.md"));
        assert!(!specific_only.text.contains("broad content"));
        assert_eq!(specific_only.omitted.len(), 1);
        assert_eq!(specific_only.omitted[0].display_path, "AGENTS.md");
    }

    #[test]
    fn budget_truncates_the_most_specific_file_binary_searched() {
        let big = "y".repeat(300);
        let files = [loaded("AGENTS.md", &big)];
        let rendered = render_agent_instructions(&files, 400, false);
        assert_eq!(rendered.truncated.len(), 1);
        assert_eq!(rendered.truncated[0].original_bytes, 300);
        assert!(rendered.truncated[0].included_bytes > 0);
        assert!(rendered.truncated[0].included_bytes < 300);
        assert!(byte_len(&rendered.text) <= 400);
    }

    #[test]
    fn tight_budget_falls_back_to_the_compact_intro() {
        let big = "y".repeat(600);
        let files = [loaded("AGENTS.md", &big)];
        let rendered = render_agent_instructions(&files, 250, false);
        // The plain intro cannot fit beside any content, so the compact
        // intro carries the frame.
        assert!(rendered.text.contains(COMPACT_AGENT_INSTRUCTIONS_INTRO));
    }

    #[test]
    fn empty_instruction_heading_represents_the_file() {
        let files = [loaded("AGENTS.md", "")];
        let (rendered, included) = render_agent_instruction_set(&files, 4096, false);
        assert_eq!(included.len(), 1, "a genuinely empty file counts");
        assert!(rendered.text.contains("Instructions from: AGENTS.md"));
    }

    #[test]
    fn scope_detection_covers_both_global_spellings() {
        assert_eq!(
            scope_for_display_path("~/.fr/AGENTS.md"),
            USER_GLOBAL_DIRECTORY
        );
        assert_eq!(
            scope_for_display_path("$FR_HOME/AGENTS.md"),
            USER_GLOBAL_DIRECTORY
        );
        assert_eq!(
            scope_for_display_path("~/.agents/AGENTS.md"),
            AGENTS_GLOBAL_DIRECTORY
        );
        assert_eq!(scope_for_display_path("pkg/AGENTS.md"), "pkg");
        assert_eq!(scope_for_display_path("AGENTS.md"), ".");
    }

    #[test]
    fn candidate_groups_merge_the_two_global_roots() {
        assert_eq!(
            instruction_candidate_group(AGENTS_GLOBAL_DIRECTORY),
            USER_GLOBAL_DIRECTORY
        );
        assert_eq!(instruction_candidate_group("pkg"), "pkg");
    }

    #[test]
    fn scope_keys_roundtrip_through_the_separator() {
        let key = candidate_scope_key("pkg", "AGENTS.md");
        assert_eq!(key, "pkg\0AGENTS.md");
        let (directory, name) = decode_scope_key(&key);
        assert_eq!((directory.as_str(), name.as_str()), ("pkg", "AGENTS.md"));
        assert_eq!(instruction_scope_key("pkg/AGENTS.md"), key);
    }

    #[test]
    fn colliding_project_directories_escape_with_a_leading_dot() {
        assert_eq!(
            escape_project_display_path("user-global/AGENTS.md"),
            "./user-global/AGENTS.md"
        );
        assert_eq!(
            escape_project_display_path("pkg/AGENTS.md"),
            "pkg/AGENTS.md"
        );
        assert_eq!(escape_project_display_path("AGENTS.md"), "AGENTS.md");
    }

    #[test]
    fn additional_sections_state_their_applicability() {
        let global = loaded("~/.fr/AGENTS.md", "content");
        let text = additional_section_text(&global);
        assert!(text.contains("Additional instructions from: ~/.fr/AGENTS.md"));
        assert!(text.contains(USER_GLOBAL_APPLICABILITY));
        let scoped = loaded("pkg/CLAUDE.md", "content");
        let text = additional_section_text(&scoped);
        assert!(text.contains("These instructions apply to work under `pkg`."));
        assert!(text.contains(ADDITIONAL_INSTRUCTIONS_GUIDANCE));
    }

    #[test]
    fn changed_sections_render_all_three_actions() {
        let set_item = ChangeRenderItem {
            change: AgentInstructionChange {
                action: InstructionChangeAction::Set,
                scope: candidate_scope_key("pkg", "AGENTS.md"),
                path: "pkg/AGENTS.md".to_string(),
                digest: Some("d".to_string()),
            },
            file: loaded("pkg/AGENTS.md", "new content"),
        };
        assert!(
            changed_section_text(&set_item).contains("Additional instructions from: pkg/AGENTS.md")
        );
        let remove_item = ChangeRenderItem {
            change: AgentInstructionChange {
                action: InstructionChangeAction::Remove,
                scope: candidate_scope_key("pkg", "AGENTS.md"),
                path: "pkg/AGENTS.md".to_string(),
                digest: None,
            },
            file: loaded("pkg/AGENTS.md", ""),
        };
        let removed = changed_section_text(&remove_item);
        assert!(removed.starts_with("Instructions removed: pkg/AGENTS.md"));
        assert!(
            removed.contains(
                REMOVED_SECTION_TEMPLATE
                    .split("\\n\\n")
                    .next()
                    .unwrap_or_default()
            ) || removed.contains("no longer apply")
        );
        let replace_item = ChangeRenderItem {
            change: AgentInstructionChange {
                action: InstructionChangeAction::Replace,
                scope: candidate_scope_key("pkg", "AGENTS.md"),
                path: "pkg/AGENTS.md".to_string(),
                digest: Some("d2".to_string()),
            },
            file: loaded("pkg/AGENTS.md", "updated content"),
        };
        let updated = changed_section_text(&replace_item);
        assert!(updated.starts_with("Updated instructions from: pkg/AGENTS.md"));
        assert!(updated.contains(UPDATED_NOTICE));
        assert!(updated.contains("updated content"));
    }

    #[test]
    fn marker_lists_omitted_and_truncated_entries() {
        let marker = marker_text(
            100,
            &[InstructionFile {
                absolute_path: "/abs/a".to_string(),
                display_path: "a/AGENTS.md".to_string(),
            }],
            &[TruncatedInstruction {
                display_path: "b/AGENTS.md".to_string(),
                original_bytes: 30,
                included_bytes: 10,
            }],
        );
        assert_eq!(
            marker,
            "Workspace instruction budget 100 bytes: omitted a/AGENTS.md; truncated b/AGENTS.md from 30 to 10 bytes"
        );
    }

    #[test]
    fn marker_empty_when_nothing_was_dropped() {
        assert_eq!(marker_text(100, &[], &[]), "");
    }

    #[test]
    fn changes_rendering_keeps_only_represented_transitions() {
        let items = vec![
            ChangeRenderItem {
                change: AgentInstructionChange {
                    action: InstructionChangeAction::Set,
                    scope: candidate_scope_key(".", "AGENTS.md"),
                    path: "AGENTS.md".to_string(),
                    digest: Some("d1".to_string()),
                },
                file: loaded(
                    "AGENTS.md",
                    "root guidance that is long enough to overflow a tiny budget on its own",
                ),
            },
            ChangeRenderItem {
                change: AgentInstructionChange {
                    action: InstructionChangeAction::Set,
                    scope: candidate_scope_key("pkg", "AGENTS.md"),
                    path: "pkg/AGENTS.md".to_string(),
                    digest: Some("d2".to_string()),
                },
                file: loaded("pkg/AGENTS.md", "kept"),
            },
        ];
        let (text, changes) = render_instruction_changes(&items, 420);
        assert!(text.contains("Additional instructions from: pkg/AGENTS.md"));
        assert!(!text.contains("root guidance"));
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "pkg/AGENTS.md");
    }

    #[test]
    fn truncate_utf8_backs_up_to_code_point_boundaries() {
        // "é" is two UTF-8 bytes; the budget cut lands on its continuation.
        let value = "aéb";
        assert_eq!(truncate_utf8(value, 2), "a");
        assert_eq!(truncate_utf8(value, 3), "aé");
        assert_eq!(truncate_utf8(value, 99), value);
    }
}
