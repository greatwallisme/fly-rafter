// Rust guideline compliant 2026-11-01
//! Model and UI rendering for persistent terminal tool results (the dsh
//! `experimental/tool-terminal` `render.ts`, one module per PRD 4.5).
//!
//! The bounding vocabulary rides the dsh shapes verbatim: the
//! `\n[output truncated]` marker, the prefix/suffix fit rules, and the
//! per-result render templates. Byte bounds use the shared UTF-8
//! boundary-safe retains ([`crate::job_tools::retain_head`] /
//! [`crate::job_tools::retain_tail`], the dsh `TextRetainer` parity).

use serde_json::Value;

use crate::job_tools::{retain_head, retain_tail};

/// The truncation marker (the dsh `TRUNCATED`).
pub const TRUNCATED: &str = "\n[output truncated]";

/// The rendered session status (the dsh `RenderedSessionStatus`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderedSessionStatus {
    /// The process is alive.
    Running,
    /// The process exited (the exit code is absent under a signal).
    Exited {
        /// The exit code, absent under a signal.
        exit_code: Option<i64>,
        /// The POSIX signal name, absent for clean exits.
        signal: Option<String>,
    },
}

impl RenderedSessionStatus {
    /// Builds the status from the wire shape (the dsh sessionStatus
    /// object); an unknown or absent `kind` word yields `None`.
    #[must_use]
    pub fn from_wire(value: &Value) -> Option<Self> {
        match value.get("kind").and_then(Value::as_str) {
            Some("running") => Some(Self::Running),
            Some("exited") => Some(Self::Exited {
                exit_code: value.get("exitCode").and_then(Value::as_i64),
                signal: value
                    .get("signal")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            }),
            _ => None,
        }
    }

    /// The render word (the dsh `'running'` / the exited composition).
    #[must_use]
    pub fn word(&self) -> String {
        match self {
            Self::Running => "running".to_owned(),
            Self::Exited { exit_code, signal } => format!(
                "exited code={} signal={}",
                exit_code.map_or_else(|| "null".to_owned(), |code| code.to_string()),
                signal.clone().unwrap_or_else(|| "null".to_owned()),
            ),
        }
    }
}

/// One rendered session snapshot (the dsh `RenderedSessionSnapshot`).
#[derive(Debug, Clone)]
pub struct RenderedSessionSnapshot {
    /// The registry-minted session id.
    pub session_id: String,
    /// The optional owner-local name.
    pub name: Option<String>,
    /// The backend type.
    pub backend_type: String,
    /// The provider process id.
    pub pid: Option<u32>,
    /// The top-level status.
    pub status: RenderedSessionStatus,
}

/// Builds one snapshot from the wire shape.
#[must_use]
pub fn snapshot_from_wire(value: &Value) -> Option<RenderedSessionSnapshot> {
    Some(RenderedSessionSnapshot {
        session_id: value.get("sessionId")?.as_str()?.to_owned(),
        name: value.get("name").and_then(Value::as_str).map(str::to_owned),
        backend_type: value.get("type")?.as_str()?.to_owned(),
        // The wire pid rides the registry's u32 domain; the wire number
        // is f64 and the registry never publishes beyond u32.
        pid: value
            .get("pid")
            .and_then(Value::as_u64)
            .map(u32::try_from)
            .transpose()
            .ok()
            .flatten(),
        status: RenderedSessionStatus::from_wire(value.get("status")?)?,
    })
}

/// Binds one complete terminal acknowledgement while preserving UTF-8
/// cuts (the dsh `boundTerminalText`).
#[must_use]
pub fn bound_terminal_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let marker_bytes = TRUNCATED.len();
    if marker_bytes >= max_bytes {
        return retain_tail(TRUNCATED, max_bytes);
    }
    format!("{}{TRUNCATED}", retain_head(text, max_bytes - marker_bytes))
}

/// Keeps the suffix whole and tail-retains the body (the dsh
/// `fitWithSuffix`).
#[must_use]
fn fit_with_suffix(content: &str, suffix: &str, max_bytes: usize) -> String {
    let fixed_bytes = suffix.len();
    if fixed_bytes >= max_bytes {
        return retain_tail(suffix, max_bytes);
    }
    format!("{}{suffix}", retain_tail(content, max_bytes - fixed_bytes))
}

/// Keeps the prefix whole and tail-retains the body (the dsh
/// `fitWithPrefix`).
#[must_use]
fn fit_with_prefix(prefix: &str, content: &str, max_bytes: usize) -> String {
    let fixed = format!("{prefix}{TRUNCATED}");
    let fixed_bytes = fixed.len();
    if fixed_bytes >= max_bytes {
        return retain_head(&fixed, max_bytes);
    }
    format!(
        "{prefix}{}{TRUNCATED}",
        retain_tail(content, max_bytes - fixed_bytes)
    )
}

/// Binds the body plus its metadata suffix, folding the upstream
/// truncation marker into the suffix when the complete text overflows
/// (the dsh `boundBodyWithSuffix`).
#[must_use]
fn bound_body_with_suffix(
    content: &str,
    metadata: &str,
    upstream_truncated: bool,
    max_bytes: usize,
) -> String {
    let suffix = if upstream_truncated {
        format!("{metadata}{TRUNCATED}")
    } else {
        metadata.to_owned()
    };
    let complete = format!("{content}{suffix}");
    if complete.len() <= max_bytes {
        return complete;
    }
    fit_with_suffix(content, &format!("{metadata}{TRUNCATED}"), max_bytes)
}

/// Renders one created session and its bounded MOTD (the dsh
/// `renderSpawn`).
#[must_use]
pub fn render_spawn(result: &RenderedSessionSnapshot, motd: &str, max_bytes: usize) -> String {
    let label = result.name.as_ref().map_or_else(
        || result.session_id.clone(),
        |name| format!("{} ({name})", result.session_id),
    );
    let prefix = format!(
        "started terminal session {label} [type: {}]\n",
        result.backend_type
    );
    let body = if motd.is_empty() {
        "(no startup output)"
    } else {
        motd
    };
    let complete = format!("{prefix}{body}");
    if complete.len() <= max_bytes {
        complete
    } else {
        fit_with_prefix(&prefix, body, max_bytes)
    }
}

/// Renders one settled interactive send (the dsh `renderSend`).
#[must_use]
pub fn render_send(
    viewport: &str,
    wait_reason: &str,
    status: &RenderedSessionStatus,
    truncated: bool,
    max_bytes: usize,
) -> String {
    let output = if viewport.is_empty() {
        "(no new output)"
    } else {
        viewport
    };
    bound_body_with_suffix(
        output,
        &format!("\n[wait: {wait_reason}]\n[session: {}]", status.word()),
        truncated,
        max_bytes,
    )
}

/// Renders one incremental background operation read (the dsh
/// `renderSendRead`).
#[must_use]
pub fn render_send_read(delta: &str, truncated: bool) -> String {
    let separator = if delta.ends_with('\n') || delta.is_empty() {
        ""
    } else {
        "\n"
    };
    if truncated {
        format!("{delta}{separator}[output truncated]")
    } else {
        delta.to_owned()
    }
}

/// Renders one bounded historical page (the dsh `renderRead`).
#[must_use]
pub fn render_read(
    text: &str,
    total_lines: u64,
    line_begin: u64,
    line_end: u64,
    truncated: bool,
    max_bytes: usize,
) -> String {
    let output = if text.is_empty() {
        "(no retained output)"
    } else {
        text
    };
    bound_body_with_suffix(
        output,
        &format!("\n[lines: {line_begin}-{line_end} of {total_lines}]"),
        truncated,
        max_bytes,
    )
}

/// Renders owner-visible live sessions (the dsh `renderList`).
#[must_use]
pub fn render_list(sessions: &[RenderedSessionSnapshot], max_bytes: usize) -> String {
    if sessions.is_empty() {
        return "(no terminal sessions)".to_owned();
    }
    let lines: Vec<String> = sessions
        .iter()
        .map(|session| {
            let name = session
                .name
                .as_ref()
                .map(|name| format!(" ({name})"))
                .unwrap_or_default();
            let pid = session
                .pid
                .map(|pid| format!(" pid={pid}"))
                .unwrap_or_default();
            format!(
                "{}{name} [{}] {}{pid}",
                session.session_id,
                session.backend_type,
                session.status.word()
            )
        })
        .collect();
    bound_body_with_suffix(&lines.join("\n"), "", false, max_bytes)
}

#[cfg(test)]
mod tests {
    use super::{
        RenderedSessionSnapshot, RenderedSessionStatus, TRUNCATED, bound_terminal_text,
        render_list, render_read, render_send, render_send_read, render_spawn,
    };

    fn running() -> RenderedSessionStatus {
        RenderedSessionStatus::Running
    }

    fn exited(code: Option<i64>) -> RenderedSessionStatus {
        RenderedSessionStatus::Exited {
            exit_code: code,
            signal: None,
        }
    }

    fn snapshot(
        name: Option<&str>,
        pid: Option<u32>,
        status: RenderedSessionStatus,
    ) -> RenderedSessionSnapshot {
        RenderedSessionSnapshot {
            session_id: "pty-1".to_owned(),
            name: name.map(str::to_owned),
            backend_type: "shell".to_owned(),
            pid,
            status,
        }
    }

    #[test]
    fn bound_terminal_text_preserves_short_text_and_heads_long_text() {
        assert_eq!(bound_terminal_text("short", 64), "short");
        let long = "x".repeat(100);
        let bound = bound_terminal_text(&long, 50);
        assert_eq!(bound.len(), 50);
        assert!(bound.ends_with(TRUNCATED));
        assert!(bound.starts_with('x'));
    }

    #[test]
    fn render_spawn_renders_the_label_and_motd_markers() {
        let result = snapshot(Some("main"), None, running());
        assert_eq!(
            render_spawn(&result, "", 4096),
            "started terminal session pty-1 (main) [type: shell]\n(no startup output)"
        );
        assert_eq!(
            render_spawn(&result, "ready", 4096),
            "started terminal session pty-1 (main) [type: shell]\nready"
        );
        let unnamed = snapshot(None, Some(42_u32), running());
        assert_eq!(
            render_spawn(&unnamed, "", 4096),
            "started terminal session pty-1 [type: shell]\n(no startup output)"
        );
    }

    #[test]
    fn render_send_appends_wait_and_session_markers() {
        assert_eq!(
            render_send("out\n", "stdin_read", &running(), false, 4096),
            "out\n\n[wait: stdin_read]\n[session: running]"
        );
        let rendered = render_send("", "inferred_idle", &exited(Some(3)), true, 4096);
        assert!(rendered.starts_with("(no new output)"));
        assert!(rendered.contains("[session: exited code=3 signal=null]"));
        assert!(rendered.ends_with(TRUNCATED));
    }

    #[test]
    fn render_send_read_separates_only_on_missing_newline() {
        assert_eq!(render_send_read("delta", false), "delta");
        assert_eq!(render_send_read("delta", true), "delta\n[output truncated]");
        assert_eq!(
            render_send_read("delta\n", true),
            "delta\n[output truncated]"
        );
        assert_eq!(render_send_read("", true), "[output truncated]");
    }

    #[test]
    fn render_read_and_list_match_the_templates() {
        assert_eq!(
            render_read("page", 5, 1, 3, false, 4096),
            "page\n[lines: 1-3 of 5]"
        );
        assert_eq!(
            render_read("", 5, 1, 3, false, 4096),
            "(no retained output)\n[lines: 1-3 of 5]"
        );
        assert_eq!(render_list(&[], 4096), "(no terminal sessions)");
        let line = render_list(&[snapshot(Some("main"), Some(7), running())], 4096);
        assert_eq!(line, "pty-1 (main) [shell] running pid=7");
        let exited_line = render_list(&[snapshot(None, None, exited(None))], 4096);
        assert_eq!(exited_line, "pty-1 [shell] exited code=null signal=null");
    }

    #[test]
    fn list_bounding_keeps_the_whole_lines_under_the_cap() {
        let sessions: Vec<_> = (0..50)
            .map(|index| RenderedSessionSnapshot {
                session_id: format!("pty-{index}"),
                name: None,
                backend_type: "shell".to_owned(),
                pid: None,
                status: running(),
            })
            .collect();
        let rendered = render_list(&sessions, 64);
        assert!(rendered.len() <= 64);
        assert!(rendered.ends_with(TRUNCATED));
    }
}
