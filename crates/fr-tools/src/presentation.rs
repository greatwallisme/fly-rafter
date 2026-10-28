// Rust guideline compliant 2026-10-28
//! Tool render-intent vocabulary for UI rendering of tool calls.
//!
//! The provider-neutral views a tool declares via `presentCall`/
//! `presentResult` (dsh `core/tools/src/presentation.ts`, wire-verbatim
//! shapes).
//!
//! Every discriminant and field spelling keeps the dsh wire names (`card`,
//! `shape`, `kind`, `oldText`, `newText`, ...) because these views cross the
//! session log and UI bridges; absent optionals stay absent on the wire. The
//! module is a pure vocabulary: dsh ships no runtime behavior here, and the
//! numeric fields (line numbers, counts, exit codes) narrow to unsigned or
//! signed integers from JS `number` because only first-party presenters
//! produce them.

use fr_wire::ContentBlock;
use serde::{Deserialize, Serialize};

/// Category of a tool call, used by a UI to pick an icon or treatment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolCallKind {
    /// Reading file content.
    Read,
    /// Modifying file content.
    Edit,
    /// Removing files.
    Delete,
    /// Renaming or relocating files.
    Move,
    /// Searching content or paths.
    Search,
    /// Running a command.
    Execute,
    /// Retrieving from the web.
    Fetch,
    /// Anything else (the default).
    Other,
}

/// A file location a tool reads or modifies, so a capable UI can follow
/// along; `line` is an optional 1-based line to focus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileLocation {
    /// The model-facing path the tool operated on.
    pub path: String,
    /// Optional 1-based line to focus.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
}

/// A single-file change a tool is about to make; `old_text` is `None` for a
/// new-file create or a call-time overwrite (no prior content available).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    /// The changed file's path.
    pub path: String,
    /// Prior content, or `None` for a new file / an overwrite.
    pub old_text: Option<String>,
    /// Content after the change.
    pub new_text: String,
}

/// Provider-neutral pending-call presentation (dsh `ToolCallView`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "card", rename_all = "lowercase")]
pub enum ToolCallView {
    /// The default card: a titled tool-call row.
    Generic(GenericCallView),
    /// A call that IS a shell command in a working directory.
    Terminal(TerminalCallView),
    /// A call that creates or modifies files, rendered inline.
    Diff(DiffCallView),
}

/// The default card: a titled tool-call row with an optional category icon, a
/// salient raw input, extra content blocks, and follow-along file locations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenericCallView {
    /// Human-readable, always-visible label describing what this call does.
    pub title: String,
    /// Category for icon/treatment; defaults to `other` when omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ToolCallKind>,
    /// The salient input for a detail/expanded view.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<serde_json::Value>,
    /// UI-facing content blocks to show on the pending call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentBlock>>,
    /// Files this call reads/modifies, for editor follow-along.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locations: Option<Vec<FileLocation>>,
}

/// A call that IS a shell command running in a working directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalCallView {
    /// The command, shown as the terminal card's title / header line.
    pub title: String,
    /// One-line summary rendered above the terminal card.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Working directory the command runs in (absolute used as-is, relative
    /// resolved by the UI bridge against the session workspace).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// A call that creates or modifies files, rendered as an inline diff card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffCallView {
    /// Card header (e.g. `Write foo.txt`).
    pub title: String,
    /// One entry per file the call changes.
    pub diffs: Vec<FileDiff>,
    /// Files this call modifies, for editor follow-along.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locations: Option<Vec<FileLocation>>,
}

/// How a tool wants the COMPLETED call shown (dsh `ToolResultView`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "card", rename_all = "lowercase")]
pub enum ToolResultView {
    /// The default completed card.
    Generic(GenericResultView),
    /// The completed state of a terminal call.
    Terminal(TerminalResultView),
    /// A completed file mutation rendered inline.
    Diff(DiffResultView),
    /// A completed search (matches or paths).
    Search(SearchResultView),
    /// A completed file read.
    Read(ReadResultView),
    /// A completed web retrieval (search or fetch).
    Web(WebResultView),
}

/// The default completed card: an optional replacement title and reformatted
/// content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenericResultView {
    /// Replacement title for the completed call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// UI-facing result content, reformatted from the model-facing result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentBlock>>,
}

/// The completed state of a terminal call: captured output and exit status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalResultView {
    /// Replacement title for the completed call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Captured command output (stdout+stderr as the tool combined them).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Process exit code, when the run ended by exiting (not a signal).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    /// Signal name that killed the process; mutually exclusive with
    /// `exit_code`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
}

/// A completed file mutation rendered as an inline diff card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffResultView {
    /// Replacement title for the completed call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The change to show, in file order.
    pub diffs: Vec<FileDiff>,
}

/// One matched line inside a search-matches group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchLineMatch {
    /// 1-based line number of the match within its file.
    pub line_number: u64,
    /// The matched line text, as the tool surfaced it.
    pub line: String,
}

/// One file's grouped content matches, in first-seen file order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchFileMatches {
    /// The file the matches belong to (the model-facing display path).
    pub path: String,
    /// The file's matched lines, in output order.
    pub matches: Vec<SearchLineMatch>,
}

/// A completed search: one `card: "search"` view with two shape-discriminated
/// variants; the discriminant is `shape` so it never collides with the
/// `ToolCallKind` an icon-picking bridge reads off a call view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "lowercase")]
pub enum SearchResultView {
    /// Matched lines grouped by file.
    Matches(SearchMatchesResultView),
    /// A flat path list.
    Paths(SearchPathsResultView),
}

/// A completed content search (`grep`) with matches grouped by file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchMatchesResultView {
    /// Replacement title for the completed call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Matched lines grouped by file, in first-seen file order.
    pub files: Vec<SearchFileMatches>,
    /// Whether the tool capped the inline result.
    pub truncated: bool,
    /// Total matches found before capping.
    pub total: u64,
}

/// A completed path search (`glob`) as a flat path list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPathsResultView {
    /// Replacement title for the completed call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The discovered paths, in the tool's result order.
    pub paths: Vec<String>,
    /// Whether the tool capped the inline result.
    pub truncated: bool,
    /// Total paths found before capping.
    pub total: u64,
}

/// One numbered line of a file, the unit a read view carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadFileLine {
    /// 1-based line number in the file (a window past `offset` keeps the
    /// file's own numbering).
    pub number: u64,
    /// The line without its trailing newline, already truncated to the read
    /// tool's per-line cap.
    pub text: String,
}

/// A completed file read, projected through `presentationMeta` and narrowed
/// back on live and replay paths alike.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadResultView {
    /// Replacement title for the completed call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The read file's path (the model-facing path).
    pub path: String,
    /// The 1-based first line the window requested, preserved even when
    /// `lines` is empty.
    pub offset: u64,
    /// The returned window's lines, in file order.
    pub lines: Vec<ReadFileLine>,
    /// Exact total line count in the file.
    pub total_lines: u64,
    /// Syntax-highlighting language hint, omitted when the extension maps to
    /// no known language.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// The model-facing result content with its envelope stripped, for a UI
    /// without the read capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentBlock>>,
}

/// One citeable source in a completed web-search view; the faithful
/// projection of the web service's source type (the two are declared
/// separately and must evolve together).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSource {
    /// The source URL.
    pub url: String,
    /// The source title, when the provider returned one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// A short excerpt or summary, when the provider returned one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// Publication/crawl timestamp as a provider-supplied ISO-8601 string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
}

/// A completed web retrieval: one `card: "web"` view with two
/// kind-discriminated variants (this `kind` is the union's own discriminant,
/// deliberately matching the tools' pending `ToolCallKind` values).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum WebResultView {
    /// The completed state of a `web_search` call.
    Search(WebSearchResultView),
    /// The completed state of a `web_fetch` call.
    Fetch(WebFetchResultView),
}

/// The completed state of a `web_search` call: structured sources, an
/// optional provider answer, and the truncation signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchResultView {
    /// Replacement title for the completed call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The faithful, structured sources.
    pub sources: Vec<WebSource>,
    /// The provider-generated answer text, when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// True when the web service cut the source list to the result cap.
    pub truncated: bool,
}

/// The completed state of a `web_fetch` call: the fetched URL, its HTTP
/// status, and whether the content was cut.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebFetchResultView {
    /// Replacement title for the completed call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The final URL after allowed redirects.
    pub url: String,
    /// HTTP status code of the fetched response.
    pub status_code: u16,
    /// True when the provider capped the decoded body or a later stage
    /// trimmed the rendered text.
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn call_views_round_trip_with_wire_shapes() {
        let generic = ToolCallView::Generic(GenericCallView {
            title: "Read config".to_owned(),
            kind: Some(ToolCallKind::Read),
            raw_input: Some(json!({"path": "a.toml"})),
            content: None,
            locations: Some(vec![FileLocation {
                path: "a.toml".to_owned(),
                line: Some(12),
            }]),
        });
        let wire = serde_json::to_value(&generic).unwrap();
        assert_eq!(
            wire,
            json!({
                "card": "generic",
                "title": "Read config",
                "kind": "read",
                "rawInput": {"path": "a.toml"},
                "locations": [{"path": "a.toml", "line": 12}]
            })
        );
        assert_eq!(
            serde_json::from_value::<ToolCallView>(wire).unwrap(),
            generic
        );

        let terminal = ToolCallView::Terminal(TerminalCallView {
            title: "cargo test".to_owned(),
            description: Some("run the suite".to_owned()),
            cwd: Some("/tmp".to_owned()),
        });
        assert_eq!(
            serde_json::to_value(&terminal).unwrap(),
            json!({"card": "terminal", "title": "cargo test", "description": "run the suite", "cwd": "/tmp"})
        );

        let diff = ToolCallView::Diff(DiffCallView {
            title: "Write foo.txt".to_owned(),
            diffs: vec![FileDiff {
                path: "foo.txt".to_owned(),
                old_text: None,
                new_text: "body".to_owned(),
            }],
            locations: None,
        });
        assert_eq!(
            serde_json::to_value(&diff).unwrap(),
            json!({
                "card": "diff",
                "title": "Write foo.txt",
                "diffs": [{"path": "foo.txt", "oldText": null, "newText": "body"}]
            })
        );
        assert_eq!(
            serde_json::from_value::<ToolCallView>(json!({
                "card": "diff",
                "title": "Write foo.txt",
                "diffs": [{"path": "foo.txt", "oldText": null, "newText": "body"}]
            }))
            .unwrap(),
            diff
        );
    }

    #[test]
    fn result_views_round_trip_with_wire_shapes() {
        let search = ToolResultView::Search(SearchResultView::Matches(SearchMatchesResultView {
            title: None,
            files: vec![SearchFileMatches {
                path: "src/a.rs".to_owned(),
                matches: vec![SearchLineMatch {
                    line_number: 7,
                    line: "pub fn f()".to_owned(),
                }],
            }],
            truncated: true,
            total: 42,
        }));
        assert_eq!(
            serde_json::to_value(&search).unwrap(),
            json!({
                "card": "search",
                "shape": "matches",
                "files": [{"path": "src/a.rs", "matches": [{"lineNumber": 7, "line": "pub fn f()"}]}],
                "truncated": true,
                "total": 42
            })
        );
        assert_eq!(
            serde_json::from_value::<ToolResultView>(serde_json::to_value(&search).unwrap())
                .unwrap(),
            search
        );

        let paths = ToolResultView::Search(SearchResultView::Paths(SearchPathsResultView {
            title: Some("glob".to_owned()),
            paths: vec!["a".to_owned(), "b".to_owned()],
            truncated: false,
            total: 2,
        }));
        assert_eq!(
            serde_json::to_value(&paths).unwrap(),
            json!({
                "card": "search",
                "shape": "paths",
                "title": "glob",
                "paths": ["a", "b"],
                "truncated": false,
                "total": 2
            })
        );

        let read = ToolResultView::Read(ReadResultView {
            title: None,
            path: "src/main.rs".to_owned(),
            offset: 10,
            lines: vec![ReadFileLine {
                number: 10,
                text: "fn main()".to_owned(),
            }],
            total_lines: 100,
            lang: Some("rs".to_owned()),
            content: None,
        });
        assert_eq!(
            serde_json::to_value(&read).unwrap(),
            json!({
                "card": "read",
                "path": "src/main.rs",
                "offset": 10,
                "lines": [{"number": 10, "text": "fn main()"}],
                "totalLines": 100,
                "lang": "rs"
            })
        );
    }

    #[test]
    fn terminal_and_web_result_views_round_trip() {
        let terminal = ToolResultView::Terminal(TerminalResultView {
            title: None,
            output: Some("ok".to_owned()),
            exit_code: Some(0),
            signal: None,
        });
        assert_eq!(
            serde_json::to_value(&terminal).unwrap(),
            json!({"card": "terminal", "output": "ok", "exitCode": 0})
        );
        let signaled = ToolResultView::Terminal(TerminalResultView {
            title: None,
            output: None,
            exit_code: None,
            signal: Some("SIGTERM".to_owned()),
        });
        assert_eq!(
            serde_json::to_value(&signaled).unwrap(),
            json!({"card": "terminal", "signal": "SIGTERM"})
        );

        let web_search = ToolResultView::Web(WebResultView::Search(WebSearchResultView {
            title: None,
            sources: vec![WebSource {
                url: "https://example.com".to_owned(),
                title: Some("Example".to_owned()),
                snippet: None,
                published_at: Some("2026-01-01T00:00:00Z".to_owned()),
            }],
            answer: Some("42".to_owned()),
            truncated: false,
        }));
        assert_eq!(
            serde_json::to_value(&web_search).unwrap(),
            json!({
                "card": "web",
                "kind": "search",
                "sources": [{"url": "https://example.com", "title": "Example", "publishedAt": "2026-01-01T00:00:00Z"}],
                "answer": "42",
                "truncated": false
            })
        );
        let web_fetch = ToolResultView::Web(WebResultView::Fetch(WebFetchResultView {
            title: None,
            url: "https://example.com".to_owned(),
            status_code: 200,
            truncated: true,
        }));
        assert_eq!(
            serde_json::to_value(&web_fetch).unwrap(),
            json!({
                "card": "web",
                "kind": "fetch",
                "url": "https://example.com",
                "statusCode": 200,
                "truncated": true
            })
        );
        assert_eq!(
            serde_json::from_value::<ToolResultView>(serde_json::to_value(&web_fetch).unwrap())
                .unwrap(),
            web_fetch
        );
    }

    #[test]
    fn generic_views_keep_content_blocks() {
        let view = ToolCallView::Generic(GenericCallView {
            title: "job".to_owned(),
            kind: None,
            raw_input: None,
            content: Some(vec![ContentBlock::Known(
                fr_wire::KnownContentBlock::Text {
                    text: "running".to_owned(),
                },
            )]),
            locations: None,
        });
        let wire = serde_json::to_value(&view).unwrap();
        assert_eq!(
            wire,
            json!({"card": "generic", "title": "job", "content": [{"type": "text", "text": "running"}]})
        );
        assert_eq!(serde_json::from_value::<ToolCallView>(wire).unwrap(), view);
    }
}
