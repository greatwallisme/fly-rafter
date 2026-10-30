// Rust guideline compliant 2026-10-30
//! Pure read presentation: the dsh `tool-fs/src/read-render.ts` plus
//! the `@deepseek-ai/dsh-util-code-language` table.
//!
//! The module turns provider-decoded text into a bounded,
//! line-numbered window and the model-facing envelope, and derives the
//! persisted `lang` hint.
//!
//! Chunk scanning caps the current line's buffer at one char past the
//! per-line limit, so even one newline-free giant line cannot grow
//! memory without bound. The per-line cap counts Rust `char`s where the
//! dsh counts UTF-16 code units; the divergence reaches only lines
//! holding astral-plane characters (the same divergence class the batch-1
//! codepoint-order note records).

use std::sync::OnceLock;

use regex::Regex;

use crate::fs_types::FsError;
use crate::fs_types::FsErrorCode;

/// Default maximum characters returned for a single line (the dsh
/// `READ_MAX_LINE_LENGTH` and the `read-max-line-length` config).
pub const READ_MAX_LINE_LENGTH: usize = 2000;

/// Default maximum bytes returned for selected file lines (the dsh
/// `READ_MAX_BYTES` and the `read-max-bytes` config).
pub const READ_MAX_BYTES: usize = 50 * 1024;

/// One line returned from a text file (dsh `FileTextLine`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTextLine {
    /// 1-based line number in the file.
    pub number: u64,
    /// Line text without its trailing newline.
    pub text: String,
}

/// The resolved read window (dsh `ReadWindow`); the caller applies its
/// defaults/caps before calling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadWindow {
    /// 1-based first line to return.
    pub offset: u64,
    /// Maximum number of lines to return.
    pub limit: u64,
    /// Maximum characters returned for a single line; overflow is
    /// truncated with a suffix.
    pub max_line_length: u64,
    /// Maximum bytes of selected output; overflow stops the scan and
    /// marks the byte truncation.
    pub max_bytes: u64,
}

/// The windowed result [`build_window`] produces from a file's decoded
/// text (dsh `WindowResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowResult {
    /// Returned lines, already numbered.
    pub lines: Vec<FileTextLine>,
    /// Exact total line count in the file.
    pub total_lines: u64,
    /// Whether selected output hit the byte cap.
    pub truncated_by_bytes: bool,
}

/// Outcome of a bounded text read — what [`format_read_output`]
/// renders (dsh `FileReadOutcome`; the byte-truncation flag rides as a
/// plain bool, the absent/`true` pair of the source).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileReadOutcome {
    /// 1-based first line requested.
    pub offset: u64,
    /// Returned lines, already numbered.
    pub lines: Vec<FileTextLine>,
    /// Exact total line count in the file.
    pub total_lines: u64,
    /// Whether selected output hit the byte cap.
    pub truncated_by_bytes: bool,
}

/// The `read` tool's private `tool/result` `meta` payload (dsh
/// `FsReadMeta`).
///
/// The structured line-numbered window a capable UI renders as a code
/// view; persisted with the session log, so `presentResult` reproduces
/// the read card on replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsReadMeta {
    /// The read file's model-facing path.
    pub path: String,
    /// The 1-based first line the window requested, kept even when
    /// `lines` is empty.
    pub offset: u64,
    /// The returned window's lines, each keeping its file line number.
    pub lines: Vec<FileTextLine>,
    /// Exact total line count in the file.
    pub total_lines: u64,
    /// Syntax-highlighting language hint, `None` for plain text.
    pub lang: Option<String>,
}

impl FsReadMeta {
    /// The persisted meta shape (the dsh object with `lang` omitted
    /// when absent).
    #[must_use]
    pub fn to_value(&self) -> serde_json::Value {
        let lines = self
            .lines
            .iter()
            .map(|line| {
                serde_json::json!({
                    "number": line.number,
                    "text": line.text,
                })
            })
            .collect::<Vec<_>>();
        let mut meta = serde_json::Map::new();
        meta.insert(
            "path".to_owned(),
            serde_json::Value::String(self.path.clone()),
        );
        meta.insert("offset".to_owned(), serde_json::json!(self.offset));
        meta.insert("lines".to_owned(), serde_json::Value::Array(lines));
        meta.insert("totalLines".to_owned(), serde_json::json!(self.total_lines));
        if let Some(lang) = &self.lang {
            meta.insert("lang".to_owned(), serde_json::Value::String(lang.clone()));
        }
        serde_json::Value::Object(meta)
    }
}

struct WindowAccumulator {
    lines: Vec<FileTextLine>,
    total_lines: u64,
    output_bytes: u64,
    truncated_by_bytes: bool,
}

fn truncate_line(line: &str, max_line_length: u64) -> String {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the cap is a config positive integer within usize range"
    )]
    let max = max_line_length as usize;
    if line.chars().count() > max {
        let head = line.chars().take(max).collect::<String>();
        format!("{head}... (line truncated to {max_line_length} chars)")
    } else {
        line.to_owned()
    }
}

/// One UTF-8 line's byte size plus the newline the join spends on every
/// line but the first (dsh `lineByteSize`).
fn line_byte_size(line: &str, current_line_count: usize) -> u64 {
    line.len() as u64 + u64::from(current_line_count > 0)
}

fn consume_line(acc: &mut WindowAccumulator, raw_line: &str, request: &ReadWindow) {
    acc.total_lines += 1;
    if acc.truncated_by_bytes
        || acc.total_lines < request.offset
        || acc.lines.len() as u64 >= request.limit
    {
        return;
    }
    let text = truncate_line(raw_line, request.max_line_length);
    let bytes = line_byte_size(&text, acc.lines.len());
    if acc.output_bytes + bytes > request.max_bytes {
        acc.truncated_by_bytes = true;
        return;
    }
    acc.output_bytes += bytes;
    acc.lines.push(FileTextLine {
        number: acc.total_lines,
        text,
    });
}

fn strip_carriage_return(line: &str) -> &str {
    line.strip_suffix('\r').unwrap_or(line)
}

fn finish(
    acc: WindowAccumulator,
    request: &ReadWindow,
    display_path: &str,
) -> Result<WindowResult, FsError> {
    if !acc.truncated_by_bytes
        && request.offset > acc.total_lines
        && !(acc.total_lines == 0 && request.offset == 1)
    {
        return Err(FsError::new(
            FsErrorCode::NotFound,
            format!(
                "offset {} is out of range for \"{}\" ({} lines)",
                request.offset, display_path, acc.total_lines
            ),
        ));
    }
    Ok(WindowResult {
        lines: acc.lines,
        total_lines: acc.total_lines,
        truncated_by_bytes: acc.truncated_by_bytes,
    })
}

/// Build one window from streamed or whole-file chunks (the dsh
/// `buildWindow`; chunk boundaries carry no meaning).
///
/// Line and byte caps are enforced while still scanning to an exact
/// total line count; the requested offset past EOF refuses with
/// `FS_NOT_FOUND`.
///
/// # Errors
///
/// The offset-out-of-range refusal.
pub fn build_window(
    chunks: impl IntoIterator<Item = Result<String, FsError>>,
    request: &ReadWindow,
    display_path: &str,
) -> Result<WindowResult, FsError> {
    let mut acc = WindowAccumulator {
        lines: Vec::new(),
        total_lines: 0,
        output_bytes: 0,
        truncated_by_bytes: false,
    };
    // One char past the truncation point is enough to prove a line
    // overflows.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the cap is a config positive integer within usize range"
    )]
    let line_buffer_cap = request.max_line_length.saturating_add(1) as usize;
    let mut line_buffer = String::new();
    let append_to_line_buffer = |line_buffer: &mut String, segment: &str| {
        if line_buffer.len() >= line_buffer_cap {
            return;
        }
        line_buffer.push_str(segment);
        // The cap counts the source's UTF-16 units; the buffer keeps
        // whole chars so the truncation cut stays on a char boundary.
        if line_buffer.chars().count() > line_buffer_cap {
            *line_buffer = line_buffer.chars().take(line_buffer_cap).collect();
        }
    };
    for chunk in chunks {
        let chunk = chunk?;
        let mut current = chunk.as_str();
        loop {
            if let Some(newline_pos) = current.find('\n') {
                append_to_line_buffer(&mut line_buffer, &current[..newline_pos]);
                consume_line(&mut acc, strip_carriage_return(&line_buffer), request);
                line_buffer.clear();
                current = &current[newline_pos + 1..];
            } else {
                append_to_line_buffer(&mut line_buffer, current);
                break;
            }
        }
    }
    if !line_buffer.is_empty() {
        consume_line(&mut acc, strip_carriage_return(&line_buffer), request);
    }
    finish(acc, request, display_path)
}

/// Format a read outcome as one OpenCode-style line-numbered text block
/// body (the dsh `formatReadOutput`; the model-facing envelope with the
/// numbered lines and the continuation or end-of-file footer).
#[must_use]
pub fn format_read_output(display_path: &str, outcome: &FileReadOutcome) -> String {
    let end_line = outcome.lines.last().map_or_else(
        // The offset is 1-based, so an empty window ends one line up.
        || outcome.offset.saturating_sub(1),
        |line| line.number,
    );
    let footer = if outcome.truncated_by_bytes {
        format!(
            "(Output capped. Showing lines {}-{}. Use offset={} to continue.)",
            outcome.offset,
            end_line,
            end_line + 1
        )
    } else if end_line < outcome.total_lines {
        format!(
            "(Showing lines {}-{} of {}. Use offset={} to continue.)",
            outcome.offset,
            end_line,
            outcome.total_lines,
            end_line + 1
        )
    } else {
        format!("(End of file - total {} lines)", outcome.total_lines)
    };
    let body = if outcome.lines.is_empty() {
        footer
    } else {
        let rendered = outcome
            .lines
            .iter()
            .map(|line| format!("{}: {}", line.number, line.text))
            .collect::<Vec<_>>()
            .join("\n");
        format!("{rendered}\n\n{footer}")
    };
    format!("<path>{display_path}</path>\n<type>file</type>\n<content>\n{body}\n</content>")
}

/// The extension-to-persisted-`lang` table: the dsh
/// `LANGUAGE_EXTENSIONS` × `SHORT_BY_LANGUAGE` projection with the
/// five `READ_LANG_BY_EXTENSION` suffix overrides folded in.
///
/// A suffix's persisted value is its language's short name except
/// where the suffix names itself better.
#[must_use]
pub fn lang_from_path(path: &str) -> Option<&'static str> {
    let slash = path.rfind(['/', '\\']).map_or(0, |index| index + 1);
    let base = &path[slash..];
    let dot = base.rfind('.')?;
    let extension = base[dot + 1..].to_ascii_lowercase();
    match extension.as_str() {
        "ts" | "mts" | "cts" => Some("ts"),
        "tsx" => Some("tsx"),
        "js" | "mjs" | "cjs" => Some("js"),
        "jsx" => Some("jsx"),
        "sh" | "bash" | "zsh" => Some("sh"),
        "fish" => Some("fish"),
        "json" | "jsonc" | "jsonl" | "ndjson" | "ipynb" => Some("json"),
        "csv" => Some("csv"),
        "py" | "pyw" | "pyi" => Some("py"),
        "rb" | "rake" | "gemspec" => Some("rb"),
        "go" => Some("go"),
        "rs" => Some("rs"),
        "java" => Some("java"),
        "c" | "h" => Some("c"),
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => Some("cpp"),
        "cs" => Some("cs"),
        "kt" | "kts" => Some("kotlin"),
        "swift" => Some("swift"),
        "php" => Some("php"),
        "yaml" | "yml" => Some("yaml"),
        "toml" => Some("toml"),
        "ini" | "conf" | "cfg" | "properties" => Some("ini"),
        "env" => Some("env"),
        "log" => Some("log"),
        "diff" | "patch" => Some("diff"),
        "http" => Some("http"),
        "md" | "markdown" => Some("md"),
        "mdx" => Some("mdx"),
        "rst" => Some("rst"),
        "tex" | "sty" | "cls" => Some("tex"),
        "bib" => Some("bib"),
        "adoc" => Some("adoc"),
        "html" | "htm" | "xhtml" => Some("html"),
        "css" => Some("css"),
        "scss" => Some("scss"),
        "less" => Some("less"),
        "sql" => Some("sql"),
        "xml" | "xsd" | "xsl" | "xslt" | "plist" | "svg" => Some("xml"),
        "lua" => Some("lua"),
        "bat" | "cmd" => Some("bat"),
        "ps1" | "psm1" | "psd1" => Some("ps1"),
        "r" => Some("r"),
        "jl" => Some("jl"),
        "dart" => Some("dart"),
        "scala" => Some("scala"),
        "clj" | "cljs" | "edn" => Some("clj"),
        "erl" | "hrl" => Some("erl"),
        "ex" | "exs" => Some("ex"),
        "hs" => Some("hs"),
        "fs" | "fsi" | "fsx" => Some("fs"),
        "vb" => Some("vb"),
        "pl" | "pm" => Some("pl"),
        "v" => Some("v"),
        "sv" | "svh" => Some("sv"),
        "graphql" | "gql" => Some("graphql"),
        "proto" => Some("proto"),
        "tf" => Some("tf"),
        "tfvars" => Some("tfvars"),
        "hcl" => Some("hcl"),
        "nix" => Some("nix"),
        "vue" => Some("vue"),
        "svelte" => Some("svelte"),
        "makefile" | "mk" => Some("make"),
        "cmake" => Some("cmake"),
        "gradle" => Some("gradle"),
        _ => None,
    }
}

/// Narrow opaque live or replayed result metadata to a structured read
/// window (the dsh `readMetaFromMeta`).
///
/// Malformed metadata returns `None` so presentation falls back to the
/// generic card instead of throwing during replay. Beyond shape, the
/// semantic contract holds:
/// `offset` is a 1-based integer, `totalLines` a non-negative integer,
/// each line number a 1-based integer no less than `offset`, the line
/// numbers strictly increase, and no line number exceeds `totalLines`.
#[must_use]
pub fn read_meta_from_meta(meta: &serde_json::Value) -> Option<FsReadMeta> {
    let object = meta.as_object()?;
    let path = object.get("path")?.as_str()?;
    let offset_value = object.get("offset")?.as_f64()?;
    if offset_value < 1.0 || offset_value.fract() != 0.0 {
        return None;
    }
    let total_value = object.get("totalLines")?.as_f64()?;
    if total_value < 0.0 || total_value.fract() != 0.0 {
        return None;
    }
    let lines_value = object.get("lines")?.as_array()?;
    let mut lines = Vec::with_capacity(lines_value.len());
    let mut previous = offset_value - 1.0;
    for line in lines_value {
        let line_object = line.as_object()?;
        let number = line_object.get("number")?.as_f64()?;
        if number < 1.0 || number.fract() != 0.0 {
            return None;
        }
        let text = line_object.get("text")?.as_str()?;
        if number <= previous || number > total_value {
            return None;
        }
        previous = number;
        // The value is validated integral and at least 1; the wide
        // intermediate cannot lose an in-range magnitude.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "validated integral and positive before the conversion"
        )]
        let number = u64::try_from(number as u128).ok()?;
        lines.push(FileTextLine {
            number,
            text: text.to_owned(),
        });
    }
    let lang = match object.get("lang") {
        None | Some(serde_json::Value::Null) => None,
        Some(lang) => Some(lang.as_str()?.to_owned()),
    };
    Some(FsReadMeta {
        path: path.to_owned(),
        // The value is validated integral and at least 1.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "validated integral and positive before the conversion"
        )]
        offset: u64::try_from(offset_value as u128).ok()?,
        lines,
        // The value is validated integral and non-negative.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "validated integral and non-negative before the conversion"
        )]
        total_lines: u64::try_from(total_value as u128).ok()?,
        lang,
    })
}

/// The read result's envelope regex (the dsh `presentResult` body
/// capture; group 1 always captures when the envelope matches).
pub(crate) fn envelope_body(text: &str) -> Option<&str> {
    static ENVELOPE: OnceLock<Regex> = OnceLock::new();
    let regex = ENVELOPE.get_or_init(|| {
        Regex::new(r"^<path>[^\n]*</path>\n<type>file</type>\n<content>\n([\s\S]*)\n</content>$")
            .expect("the read envelope regex compiles")
    });
    regex
        .captures(text)
        .and_then(|captures| captures.get(1))
        .map(|m| m.as_str())
}

/// The dsh `langFromPath` re-export shape: the read surface's single
/// language-table entry point.
pub use lang_from_path as read_lang_hint;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The read envelope parse fails closed on foreign shapes.
    #[test]
    fn envelope_parse_fails_closed() {
        let good = format_read_output(
            "/w/a.txt",
            &FileReadOutcome {
                offset: 1,
                lines: vec![FileTextLine {
                    number: 1,
                    text: "hi".to_owned(),
                }],
                total_lines: 1,
                truncated_by_bytes: false,
            },
        );
        assert_eq!(
            envelope_body(&good),
            Some("1: hi\n\n(End of file - total 1 lines)")
        );
        assert_eq!(envelope_body("<path>/a</path>\nno content"), None);
        assert_eq!(envelope_body(""), None);
    }

    /// The window caps: line truncation text, byte cap notice, and the
    /// exact total-line scan.
    #[test]
    fn window_caps_lines_bytes_and_totals() {
        // Four 10-char lines (chunk boundaries carry no meaning), cap
        // 12 chars/60 bytes: nothing truncates.
        let lines: Vec<Result<String, FsError>> = vec![Ok(
            "0123456789\n0123456789\n0123456789\n0123456789\n".to_owned(),
        )];
        let window = build_window(
            lines,
            &ReadWindow {
                offset: 1,
                limit: 10,
                max_line_length: 12,
                max_bytes: 60,
            },
            "/w/a.txt",
        )
        .expect("window");
        assert_eq!(window.total_lines, 4);
        assert_eq!(window.lines[0].text, "0123456789");
        // Byte budget: 10 + 11 + 11 + 11 = 43 <= 60, so every line
        // stays and the byte flag stays off.
        assert_eq!(window.lines.len(), 4);
        assert!(!window.truncated_by_bytes);

        // A line past the char cap truncates with the dsh suffix.
        let long = build_window(
            vec![Ok("0123456789ABCDEF\n".to_owned())],
            &ReadWindow {
                offset: 1,
                limit: 10,
                max_line_length: 12,
                max_bytes: 60,
            },
            "/w/a.txt",
        )
        .expect("window");
        assert_eq!(
            long.lines[0].text,
            "0123456789AB... (line truncated to 12 chars)"
        );

        // Offset past EOF refuses with the dsh text.
        let error = build_window(
            vec![Ok("one".to_owned())],
            &ReadWindow {
                offset: 5,
                limit: 10,
                max_line_length: 2000,
                max_bytes: 50 * 1024,
            },
            "/w/a.txt",
        )
        .expect_err("out of range");
        assert_eq!(error.code, FsErrorCode::NotFound);
        assert_eq!(
            error.message,
            "offset 5 is out of range for \"/w/a.txt\" (1 lines)"
        );
    }

    /// The three footers: byte-capped, mid-file continuation, EOF.
    #[test]
    fn footers_pin_the_three_shapes() {
        let lines = |numbers: &[u64]| -> Vec<FileTextLine> {
            numbers
                .iter()
                .map(|n| FileTextLine {
                    number: *n,
                    text: "x".to_owned(),
                })
                .collect()
        };
        let capped = format_read_output(
            "/w/a.txt",
            &FileReadOutcome {
                offset: 1,
                lines: lines(&[1, 2]),
                total_lines: 9,
                truncated_by_bytes: true,
            },
        );
        assert!(capped.contains("(Output capped. Showing lines 1-2. Use offset=3 to continue.)"));
        let mid = format_read_output(
            "/w/a.txt",
            &FileReadOutcome {
                offset: 3,
                lines: lines(&[3, 4]),
                total_lines: 9,
                truncated_by_bytes: false,
            },
        );
        assert!(
            mid.contains("(Showing lines 3-4 of 9. Use offset=5 to continue.)"),
            "{mid}"
        );
        let eof = format_read_output(
            "/w/a.txt",
            &FileReadOutcome {
                offset: 8,
                lines: lines(&[8, 9]),
                total_lines: 9,
                truncated_by_bytes: false,
            },
        );
        assert!(eof.contains("(End of file - total 9 lines)"));
    }

    /// The language table keeps the dsh short-id projection on spot
    /// checks, including the five suffix self-naming overrides.
    #[test]
    fn lang_table_matches_the_dsh_projection() {
        assert_eq!(lang_from_path("/a/b/main.rs"), Some("rs"));
        assert_eq!(lang_from_path("C:\\dir\\main.PS1"), Some("ps1"));
        assert_eq!(lang_from_path("/a/.env"), Some("env"));
        assert_eq!(lang_from_path("/a/.gitignore"), None);
        assert_eq!(lang_from_path("/a/noext"), None);
        assert_eq!(lang_from_path("/a/tf/main.tf"), Some("tf"));
        assert_eq!(lang_from_path("/a/infra/main.hcl"), Some("hcl"));
        assert_eq!(lang_from_path("/a/tf/main.tfvars"), Some("tfvars"));
        assert_eq!(lang_from_path("/a/app.gradle"), Some("gradle"));
        assert_eq!(lang_from_path("/a/app.tsx"), Some("tsx"));
        assert_eq!(lang_from_path("/a/comp.jsx"), Some("jsx"));
        assert_eq!(lang_from_path("/a/notes.markdown"), Some("md"));
        assert_eq!(lang_from_path("/a/style.psm1"), Some("ps1"));
    }

    /// The meta narrowing enforces the semantic contract (strictly
    /// increasing 1-based numbers within `totalLines`).
    #[test]
    fn meta_narrowing_enforces_the_semantics() {
        let good = json!({
            "path": "/a.txt",
            "offset": 2,
            "lines": [
                {"number": 2, "text": "a"},
                {"number": 5, "text": "b"},
            ],
            "totalLines": 9,
            "lang": "rs",
        });
        let meta = read_meta_from_meta(&good).expect("valid meta");
        assert_eq!(meta.offset, 2);
        assert_eq!(meta.total_lines, 9);
        assert_eq!(meta.lang.as_deref(), Some("rs"));

        // Line numbers must strictly increase.
        let mut dup = good.clone();
        dup["lines"][1]["number"] = json!(2);
        assert_eq!(read_meta_from_meta(&dup), None);
        // A line number past totalLines declines.
        let mut over = good.clone();
        over["lines"][1]["number"] = json!(10);
        assert_eq!(read_meta_from_meta(&over), None);
        // A zero or fractional offset declines.
        let mut zero = good.clone();
        zero["offset"] = json!(0);
        assert_eq!(read_meta_from_meta(&zero), None);
        let mut frac = good.clone();
        frac["totalLines"] = json!(9.5);
        assert_eq!(read_meta_from_meta(&frac), None);
        // A non-string lang declines.
        let mut lang = good;
        lang["lang"] = json!(3);
        assert_eq!(read_meta_from_meta(&lang), None);
    }

    /// The byte-cap arms: the scan stops adding lines once the budget
    /// is exceeded, keeping an exact total count.
    #[test]
    fn byte_cap_stops_the_window_and_keeps_the_total() {
        let lines: Vec<Result<String, FsError>> =
            vec![Ok("0123456789\n0123456789\n0123456789\n".to_owned())];
        let window = build_window(
            lines,
            &ReadWindow {
                offset: 1,
                limit: 10,
                max_line_length: 12,
                max_bytes: 25,
            },
            "/w/a.txt",
        )
        .expect("window");
        // 10 + 11 = 21 fits; the third line's 11 more would reach 32.
        assert_eq!(window.lines.len(), 2);
        assert!(window.truncated_by_bytes);
        assert_eq!(window.total_lines, 3);
    }
}
