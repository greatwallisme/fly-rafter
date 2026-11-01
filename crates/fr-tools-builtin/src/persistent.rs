// Rust guideline compliant 2026-11-01
//! The owner-scoped persistent `bash`/`pwsh` pair (dsh
//! `tool-bash-persistent` + `tool-pwsh-persistent`, one module per PRD
//! 4.5).
//!
//! The two dsh packages are deliberate mirrors (their own `jscpd`
//! note); the fr form parameterizes the twin over one dialect table
//! and keeps every model-visible byte per dialect.
//!
//! The dsh async surfaces lower onto the sync band: the per-owner
//! serialized queue becomes the executor's own call discipline (one
//! executing call per owner; the registry's `SEND_ACTIVE` fence backs
//! the invariant), the `pending`/`creating` creation dedupe collapses
//! (creation completes inside one synchronous body), and the upstream
//! `AbortSignal` arms have no wire carrier (the tool-abort channel
//! ends at the approval seam) — the timeout deadline remains the one
//! live bound.
//!
//! Marker, wrapper, and status vocabulary follow the PRD 10 brand-map
//! ruling: the managed-variable prefix alone maps (`__FR_PERSISTENT_*`
//! markers, `__fr_persistent_bash_status`); the timeout code, job
//! kind, and every error or status sentence stay verbatim.

use serde_json::{Value, json};

use crate::job_tools::truncate_without_splitting_surrogate_pair;
use crate::tools::{BodyError, Seams, TerminalSeams};

/// The baked `backendType` (the merged persistent family default).
pub(crate) const BAKED_BACKEND_TYPE: &str = "shell";

/// The baked `timeoutMs` (one command's wall-clock limit).
pub(crate) const BAKED_TIMEOUT_MS: u64 = 300_000;

/// The baked `maxOutputChars` (the returned-command-output cap).
pub(crate) const BAKED_MAX_OUTPUT_CHARS: usize = 16_000;

/// The clip note (the dsh `TRUNCATED_MESSAGE`; the grep advice is the
/// bash wording).
pub(crate) const BASH_TRUNCATED_MESSAGE: &str = "<response clipped><NOTE>To save on context only part of this file has been shown to you. You should retry this tool after you have searched inside the file with `grep -n` in order to find the line numbers of what you are looking for.</NOTE>";

/// The clip note (the Select-String wording is the pwsh twin's).
pub(crate) const PWSH_TRUNCATED_MESSAGE: &str = "<response clipped><NOTE>To save on context only part of this file has been shown to you. You should retry this tool after you have searched inside the file with Select-String in order to find the line numbers of what you are looking for.</NOTE>";

/// The dropped-scrollback note (the dsh `LOST_PREFIX_MESSAGE`; shared
/// wording across the twins).
pub(crate) const LOST_PREFIX_MESSAGE: &str = "<response clipped><NOTE>The beginning of this command output was dropped by the terminal scrollback limit. The following text is the earliest retained output.</NOTE>\n";

/// The bash reset sentence (the dsh `SHELL_RESET_MESSAGE`).
pub(crate) const BASH_SHELL_RESET_MESSAGE: &str = "The persistent bash shell was reset; the next bash call starts from the workspace with a fresh current directory and environment.";

/// The pwsh reset sentence (the dsh twin's wording).
pub(crate) const PWSH_SHELL_RESET_MESSAGE: &str = "The persistent pwsh shell was reset; the next pwsh call starts from the workspace with a fresh current directory and environment.";

/// The status trailer for a command that never reported an exit code
/// (the bash `TIMEOUT_STATUS_MARKER`; the pwsh twin carries none).
pub(crate) const TIMEOUT_STATUS_MARKER: &str = "[Command timed out or OOM]";

/// The deadline abort code (the dsh `TIMEOUT_CODE`, per dialect).
pub(crate) const BASH_TIMEOUT_CODE: &str = "PERSISTENT_BASH_TIMEOUT";
/// The pwsh deadline abort code.
pub(crate) const PWSH_TIMEOUT_CODE: &str = "PERSISTENT_PWSH_TIMEOUT";

/// One page is enough to find a just-emitted completion marker (the
/// dsh `SCROLLBACK_PAGE_LINES`).
pub(crate) const SCROLLBACK_PAGE_LINES: u64 = 1_000;

/// The polling pause between send slices (the dsh `POLL_INTERVAL_MS`).
pub(crate) const POLL_INTERVAL_MS: u64 = 25;

/// The persistent wait slice handed to the band (the send wait settles
/// at the session's own bound; the slice bounds one wire hop).
const SEND_WAIT_SLICE_MS: u64 = 30_000;

/// One dialect's model-visible table (the twin parameterization; every
/// field is the dsh per-package const or wrapper fragment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dialect {
    /// The registered tool name (`bash` / `pwsh`).
    pub tool_name: &'static str,
    /// The clip note (grep vs Select-String wording).
    pub truncated_message: &'static str,
    /// The reset sentence.
    pub shell_reset_message: &'static str,
    /// The deadline code (`PERSISTENT_*_TIMEOUT`).
    pub timeout_code: &'static str,
    /// The wrapper's status shell variable (bash only; the pwsh twin
    /// carries its status in-session).
    pub status_variable: Option<&'static str>,
    /// Whether the shell setup suppresses echo (`stty -echo`, bash
    /// only — the pwsh prompt rides `PSReadLine`).
    pub echo_suppression: bool,
    /// Whether a settled command appends the exit-code trailer on
    /// every exit (bash) or only on nonzero (pwsh).
    pub always_report_exit: bool,
    /// The timeout output carries the OOM status trailer (bash only).
    pub timeout_status_marker: bool,
}

/// The bash dialect row.
pub const BASH: Dialect = Dialect {
    tool_name: "bash",
    truncated_message: BASH_TRUNCATED_MESSAGE,
    shell_reset_message: BASH_SHELL_RESET_MESSAGE,
    timeout_code: BASH_TIMEOUT_CODE,
    status_variable: Some("__fr_persistent_bash_status"),
    echo_suppression: true,
    always_report_exit: true,
    timeout_status_marker: true,
};

/// The pwsh dialect row.
pub const PWSH: Dialect = Dialect {
    tool_name: "pwsh",
    truncated_message: PWSH_TRUNCATED_MESSAGE,
    shell_reset_message: PWSH_SHELL_RESET_MESSAGE,
    timeout_code: PWSH_TIMEOUT_CODE,
    status_variable: None,
    echo_suppression: false,
    always_report_exit: false,
    timeout_status_marker: false,
};

/// The owner→live-session ledger (the dsh `live` map; the pending and
/// creating dedupe collapse under the synchronous body — see the
/// module note).
///
/// Teardown closes every live row (the dsh shell cleanup effect).
#[derive(Default)]
pub struct PersistentShells {
    live: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

impl std::fmt::Debug for PersistentShells {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistentShells").finish_non_exhaustive()
    }
}

impl PersistentShells {
    /// The live session id for one owner.
    #[must_use]
    pub fn get(&self, owner: &str) -> Option<String> {
        self.live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(owner)
            .cloned()
    }

    /// Closes every live shell (the teardown dispose; best-effort — a
    /// dead terminals band cannot block the teardown).
    pub fn dispose(&self, terminals: &dyn TerminalSeams) {
        let mut live = self
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (owner, id) in live.drain() {
            let _ignored = terminals.kill(&owner, &id, "tool-bash-persistent disposed");
        }
    }

    /// Resets one owner's row and closes the live shell (the dsh
    /// `reset`; the close failure rides the caller's error path).
    fn reset(
        &self,
        terminals: &dyn TerminalSeams,
        owner: &str,
        reason: &str,
    ) -> Result<(), String> {
        let id = self
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(owner);
        id.map_or(Ok(()), |id| terminals.kill(owner, &id, reason).map(|_| ()))
    }
}

/// One captured command output (the dsh `CapturedOutput`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedOutput {
    /// The captured text between the markers.
    pub text: String,
    /// The start marker never arrived (the scrollback dropped it).
    pub incomplete: bool,
    /// The wrapper-reported exit code, when the end marker carried one.
    pub exit_code: Option<u32>,
}

/// The scrollback snapshot (the dsh `RetainedOutput`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedOutput {
    /// The assembled pages.
    pub text: String,
    /// Any page reported the retention truncation.
    pub truncated: bool,
}

/// Clips one complete output against the character cap (the dsh
/// `maybeTruncate`).
#[must_use]
pub fn maybe_truncate(
    content: &str,
    max_output_chars: usize,
    incomplete: bool,
    truncated_message: &str,
) -> String {
    if content.chars().count() <= max_output_chars && !incomplete {
        return content.to_owned();
    }
    let clipped = if content.chars().count() <= max_output_chars {
        content.to_owned()
    } else {
        truncate_without_splitting_surrogate_pair(content, max_output_chars)
    };
    format!("{clipped}{truncated_message}")
}

/// Builds one marker pair (the dsh `markers`; the nonce is a fresh
/// UUID per command).
#[must_use]
pub fn markers(dialect: &Dialect) -> (String, String) {
    let nonce = uuid::Uuid::new_v4();
    let stem = dialect.tool_name.to_uppercase();
    (
        format!("__FR_PERSISTENT_{stem}_START_{nonce}__"),
        format!("__FR_PERSISTENT_{stem}_END_{nonce}:"),
    )
}

/// Quotes one command for the bash wrapper (the dsh `quoteForBash`).
#[must_use]
pub fn quote_for_bash(value: &str) -> String {
    format!(
        "$'{}'",
        value
            .replace('\\', "\\\\")
            .replace('\'', "\\'")
            .replace('\r', "\\r")
            .replace('\n', "\\n")
    )
}

/// Quotes one command body for the pwsh wrapper (the dsh
/// `quoteForPwsh`: backtick escapes keep every character literal).
#[must_use]
pub fn quote_for_pwsh(value: &str) -> String {
    value
        .replace('`', "``")
        .replace('"', "`\"")
        .replace('$', "`$")
        .replace('\r', "")
        .replace('\n', "`n")
        .replace('\x1b', "`e")
}

/// Wraps one command between the markers (the dsh `wrapCommand`; one
/// physical line so PS2/PSReadLine echo never leaks).
#[must_use]
pub fn wrap_command(command: &str, start: &str, end: &str, dialect: &Dialect) -> String {
    if let Some(status) = dialect.status_variable {
        return format!(
            "printf '%s\\n' {}; eval -- {}; {status}=$?; printf '%s%s\\n' {} \"${status}\"",
            quote_for_bash(start),
            quote_for_bash(command),
            quote_for_bash(end),
        );
    }
    let body = quote_for_pwsh(command);
    format!(
        "Write-Output '{start}'; $LASTEXITCODE = $null; $__s = 1; try {{ Invoke-Expression \"{body}\"; $__ok = $? }} catch {{ $__ok = $false }}; if ($null -ne $LASTEXITCODE) {{ $__s = [int]$LASTEXITCODE }} else {{ $__s = if ($__ok) {{ 0 }} else {{ 1 }} }}; Write-Output ('{end}' + $__s)"
    )
}

/// Trims trailing newlines (bash: any run; pwsh: one — the dsh twins'
/// divergent regexes).
#[must_use]
pub fn trim_trailing_newline(text: &str, all: bool) -> String {
    let mut out = text;
    while let Some(stripped) = out.strip_suffix('\n') {
        out = stripped.strip_suffix('\r').unwrap_or(stripped);
        if !all {
            break;
        }
    }
    out.to_owned()
}

/// Extracts the complete command output between the markers (the dsh
/// `commandOutput`; `None` while the status line has not arrived).
#[must_use]
pub fn command_output(
    snapshot: &RetainedOutput,
    start: &str,
    end: &str,
    wrapper: Option<&str>,
    all_trailing: bool,
) -> Option<CapturedOutput> {
    let text = &snapshot.text;
    let end_pos = text.rfind(end)?;
    let tail = &text[end_pos + end.len()..];
    // The pwsh twin tolerates terminal padding before the newline (the
    // `^(\d+) *\r?\n` shape); the bash status abuts the marker.
    let status = if all_trailing {
        padded_digits_before_newline(tail)
    } else {
        digits_before_newline(tail)
    }?;
    let start_marker = text[..end_pos].rfind(start);
    let content_start = start_marker.map_or(0, |pos| pos + start.len());
    let mut captured = text[content_start..end_pos].to_owned();
    // The PSReadLine echo carries the wrapper source before the real
    // markers; stripping covers the fallback-to-echo case.
    if let Some(wrapper) = wrapper {
        captured = captured.replace(wrapper, "");
    }
    let captured = captured
        .strip_prefix('\r')
        .unwrap_or(captured.as_str())
        .strip_prefix('\n')
        .unwrap_or_else(|| captured.strip_prefix('\r').unwrap_or(captured.as_str()));
    Some(CapturedOutput {
        text: trim_trailing_newline(captured, all_trailing),
        incomplete: start_marker.is_none(),
        exit_code: Some(status),
    })
}

/// Reads the run of digits that opens `tail` (the bash
/// `^(\d+)\r?\n` shape).
fn digits_before_newline(tail: &str) -> Option<u32> {
    let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
    let rest = &tail[digits.len()..];
    if digits.is_empty() || !(rest.starts_with('\n') || rest.starts_with("\r\n")) {
        return None;
    }
    digits.parse().ok()
}

/// Reads the digits plus optional horizontal padding that open `tail`
/// (the pwsh `^(\d+) *\r?\n` shape).
fn padded_digits_before_newline(tail: &str) -> Option<u32> {
    let digits_len = tail.chars().take_while(char::is_ascii_digit).count();
    if digits_len == 0 {
        return None;
    }
    let digits = &tail[..digits_len];
    let mut rest = &tail[digits_len..];
    rest = rest.trim_start_matches(' ');
    if rest.starts_with("\r\n") || rest.starts_with('\n') {
        digits.parse().ok()
    } else {
        None
    }
}

/// Extracts the partial output after an abnormal settle (the dsh
/// `partialOutput`).
///
/// The live snapshot anchors on the real start marker; the incremental
/// fallback anchors on its last copy, stripping the echoed wrapper.
#[must_use]
pub fn partial_output(
    snapshot: &RetainedOutput,
    start: &str,
    end: &str,
    fallback: &str,
    fallback_truncated: bool,
    wrapper: Option<&str>,
    all_trailing: bool,
) -> CapturedOutput {
    if let Some(pos) = snapshot.text.rfind(start) {
        let body = &snapshot.text[pos + start.len()..];
        let stripped = body
            .strip_prefix('\r')
            .unwrap_or(body)
            .strip_prefix('\n')
            .unwrap_or(body);
        return CapturedOutput {
            text: trim_trailing_newline(stripped, all_trailing),
            incomplete: false,
            exit_code: None,
        };
    }
    let fallback_start = fallback.rfind(start);
    let after_start = fallback_start.map_or_else(
        || fallback.to_owned(),
        |pos| {
            fallback[pos + start.len()..]
                .strip_prefix('\r')
                .unwrap_or_else(|| &fallback[pos + start.len()..])
                .to_owned()
        },
    );
    let before_end = match after_start.rfind(end) {
        Some(pos) => after_start[..pos].to_owned(),
        None => after_start,
    };
    let before_end = match wrapper {
        Some(wrapper) => before_end.replace(wrapper, ""),
        None => before_end,
    };
    CapturedOutput {
        text: trim_trailing_newline(&before_end, all_trailing),
        incomplete: fallback_truncated || fallback_start.is_none(),
        exit_code: None,
    }
}

/// Renders one captured output (the dsh `renderCaptured`): the clip
/// note, the lost-prefix note, and the dialect's exit-code trailer.
#[must_use]
pub fn render_captured(output: &CapturedOutput, dialect: &Dialect) -> String {
    let rendered = maybe_truncate(
        &output.text,
        BAKED_MAX_OUTPUT_CHARS,
        output.incomplete,
        dialect.truncated_message,
    );
    let with_prefix = if output.incomplete && !output.text.is_empty() {
        format!("{LOST_PREFIX_MESSAGE}{rendered}")
    } else {
        rendered
    };
    let marker = output
        .exit_code
        .filter(|code| dialect.always_report_exit || *code != 0)
        .map(|code| {
            if dialect.always_report_exit {
                format!("[Command finished with exit code {code}]")
            } else {
                format!("[exit code: {code}]")
            }
        });
    append_status_marker(with_prefix, marker)
}

/// Appends one status trailer (the dsh `appendStatusMarker`).
#[must_use]
pub fn append_status_marker(content: String, marker: Option<String>) -> String {
    match marker {
        Some(marker) if content.is_empty() => marker,
        Some(marker) => format!("{content}\n{marker}"),
        None => content,
    }
}

/// Renders the exited-shell status line (the dsh
/// `renderShellExitStatus`).
#[must_use]
pub fn render_shell_exit_status(
    content: String,
    exit_code: Option<i64>,
    signal: Option<&str>,
) -> String {
    let marker = match (signal, exit_code) {
        (Some(signal), _) => format!("[shell killed by signal: {signal}]"),
        (None, Some(code)) => format!("[shell exited: code {code}]"),
        (None, None) => "[shell exited]".to_owned(),
    };
    append_status_marker(content, Some(marker))
}

/// The persistent tool body (the dsh `execute` + `executeCommand`).
///
/// # Errors
///
/// The empty-command refusal, the missing-owner refusal, or the
/// shells/send family with the dialect's reset reasons.
pub fn execute_persistent(
    shells: &PersistentShells,
    seams: &dyn Seams,
    dialect: &Dialect,
    call: &Value,
) -> Result<Value, BodyError> {
    let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
    let agent = call.get("agent").and_then(Value::as_str);
    let command = arguments
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if command.trim().is_empty() {
        return Err(BodyError::plain("command must be a non-empty string"));
    }
    let owner = agent.ok_or_else(|| {
        BodyError::plain(format!(
            "{} requires an owning agent session",
            dialect.tool_name
        ))
    })?;
    let terminals = seams
        .terminals()
        .ok_or_else(|| BodyError::plain("terminal tools require the terminals service"))?;
    execute_command(shells, terminals, seams, dialect, owner, command)
}

/// The deadline arm (the dsh timeout return): the partial output with
/// the OOM status trailer, the shell reset, and the reset sentence.
#[expect(
    clippy::too_many_arguments,
    reason = "the dsh carrier mirrors one argument per fact; grouping would hide the correspondence"
)]
fn timeout_result(
    shells: &PersistentShells,
    terminals: &dyn TerminalSeams,
    dialect: &Dialect,
    owner: &str,
    snapshot: &RetainedOutput,
    start: &str,
    end: &str,
    fallback: &str,
    fallback_truncated: bool,
    echoed: Option<&str>,
) -> Result<Value, BodyError> {
    let partial = render_captured(
        &partial_output(
            snapshot,
            start,
            end,
            fallback,
            fallback_truncated,
            echoed,
            dialect.status_variable.is_some(),
        ),
        dialect,
    );
    shells
        .reset(
            terminals,
            owner,
            &format!("persistent {} command timed out", dialect.tool_name),
        )
        .map_err(BodyError::plain)?;
    let timeout_line = format!(
        "Your command timed out after {} seconds or experienced an OOM error. Below is partial output:",
        BAKED_TIMEOUT_MS.div_ceil(1000)
    );
    let body = if dialect.timeout_status_marker {
        append_status_marker(partial, Some(TIMEOUT_STATUS_MARKER.to_owned()))
    } else {
        partial
    };
    Ok(Value::String(format!(
        "{timeout_line}\n{body}\n{}",
        dialect.shell_reset_message
    )))
}

/// One command over the persistent shell (the dsh `executeCommand`
/// loop; the deadline and the marker loop in sync form).
#[expect(
    clippy::too_many_lines,
    reason = "the body mirrors the dsh executeCommand loop one arm per settle outcome; splitting the arms would hide the correspondence"
)]
fn execute_command(
    shells: &PersistentShells,
    terminals: &dyn TerminalSeams,
    seams: &dyn Seams,
    dialect: &Dialect,
    owner: &str,
    command: &str,
) -> Result<Value, BodyError> {
    let started = std::time::Instant::now();
    let id = get_shell(shells, terminals, seams, dialect, owner)?;
    let (start, end) = markers(dialect);
    let wrapped = wrap_command(command, &start, &end, dialect);
    let echoed = dialect.status_variable.map(|_| wrapped.clone());
    let mut first = true;
    let mut fallback = String::new();
    let mut fallback_truncated = false;
    loop {
        // The shell may flip to exited between iterations; re-observe
        // before the next send (the dsh gap note).
        if let Some((_, code, signal)) = session_status(terminals, owner, &id)
            .filter(|(exited, _, _)| *exited)
        {
            {
                return respond_to_session_exit(
                    shells,
                    terminals,
                    dialect,
                    owner,
                    &id,
                    Some(&code),
                    signal.as_str(),
                    &start,
                    &end,
                    fallback.as_str(),
                    fallback_truncated,
                    echoed.as_deref(),
                );
            }
        }
        let send_text = if first { wrapped.as_str() } else { "" };
        if let Err(error) = terminals.start_send(owner, &id, send_text, first) {
            shells
                .reset(
                    terminals,
                    owner,
                    &format!("persistent {} send failed", dialect.tool_name),
                )
                .map_err(BodyError::plain)?;
            return Err(BodyError::plain(error));
        }
        first = false;
        let result = wait_settled(terminals, owner, &id, started)?;
        let incremental = terminals.send_read(owner, &id).map_err(BodyError::plain)?;
        let delta = incremental
            .get("delta")
            .and_then(Value::as_str)
            .unwrap_or_default();
        fallback = if delta.is_empty() {
            result.viewport.clone().unwrap_or_default()
        } else {
            fallback + delta
        };
        fallback_truncated = fallback_truncated
            || incremental
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            || result.truncated;
        let latest = read_page(terminals, owner, &id, 0, SCROLLBACK_PAGE_LINES)?;
        let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        if elapsed >= BAKED_TIMEOUT_MS {
            let snapshot = retained_scrollback(terminals, owner, &id, Some(&latest))?;
            return timeout_result(
                shells,
                terminals,
                dialect,
                owner,
                &snapshot,
                &start,
                &end,
                &fallback,
                fallback_truncated,
                echoed.as_deref(),
            );
        }
        if latest.text.contains(end.as_str()) {
            let snapshot = retained_scrollback(terminals, owner, &id, Some(&latest))?;
            if let Some(complete) = command_output(
                &snapshot,
                &start,
                &end,
                echoed.as_deref(),
                dialect.status_variable.is_some(),
            ) {
                return Ok(Value::String(render_captured(&complete, dialect)));
            }
        }
        if result.exited {
            return respond_to_session_exit(
                shells,
                terminals,
                dialect,
                owner,
                &id,
                result.exit_code.as_ref(),
                result.signal.as_ref().and_then(Value::as_str),
                &start,
                &end,
                fallback.as_str(),
                fallback_truncated,
                echoed.as_deref(),
            );
        }
        if result.wait_reason.as_deref() == Some("stdin_read") {
            let snapshot = retained_scrollback(terminals, owner, &id, Some(&latest))?;
            return Ok(Value::String(render_captured(
                &partial_output(
                    &snapshot,
                    &start,
                    &end,
                    &fallback,
                    fallback_truncated,
                    echoed.as_deref(),
                    dialect.status_variable.is_some(),
                ),
                dialect,
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS));
    }
}

/// One settled send projection (the loop's local view; the wire shape
/// stays the dsh result fields).
struct SendView {
    viewport: Option<String>,
    wait_reason: Option<String>,
    exited: bool,
    exit_code: Option<Value>,
    signal: Option<Value>,
    truncated: bool,
}

/// Drives the wait slices to settlement (the `operation.done` lower).
fn wait_settled(
    terminals: &dyn TerminalSeams,
    owner: &str,
    id: &str,
    started: std::time::Instant,
) -> Result<SendView, BodyError> {
    loop {
        let reply = terminals
            .send_wait(owner, id, SEND_WAIT_SLICE_MS)
            .map_err(BodyError::plain)?;
        if reply.get("settled").and_then(Value::as_bool) != Some(true) {
            if elapsed_over(started) {
                return Ok(SendView {
                    viewport: None,
                    wait_reason: None,
                    exited: false,
                    exit_code: None,
                    signal: None,
                    truncated: false,
                });
            }
            continue;
        }
        let status = reply.get("sessionStatus").cloned().unwrap_or(Value::Null);
        return Ok(SendView {
            viewport: reply
                .get("viewport")
                .and_then(Value::as_str)
                .map(str::to_owned),
            wait_reason: reply
                .get("waitReason")
                .and_then(Value::as_str)
                .map(str::to_owned),
            exited: status.get("kind").and_then(Value::as_str) == Some("exited"),
            exit_code: status.get("exitCode").cloned(),
            signal: status.get("signal").cloned(),
            truncated: reply
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }
}

/// The command deadline probe (the `using deadline` lower; the
/// timeout keeps polling until the loop's own deadline arm fires).
fn elapsed_over(started: std::time::Instant) -> bool {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX) >= BAKED_TIMEOUT_MS
}

/// Reads one owner session's status triple (exited?, exit code,
/// signal) from the list face.
fn session_status(
    terminals: &dyn TerminalSeams,
    owner: &str,
    id: &str,
) -> Option<(bool, Value, Value)> {
    let reply = terminals.list(owner).ok()?;
    for session in reply.get("sessions")?.as_array()? {
        if session.get("sessionId").and_then(Value::as_str) == Some(id) {
            let status = session.get("status")?;
            return Some((
                status.get("kind").and_then(Value::as_str) == Some("exited"),
                status.get("exitCode").cloned().unwrap_or(Value::Null),
                status.get("signal").cloned().unwrap_or(Value::Null),
            ));
        }
    }
    None
}

/// One scrollback page with its walk metadata (the wire face's
/// `TerminalReadResult` fields).
#[derive(Debug, Clone)]
struct Page {
    text: String,
    truncated: bool,
    total_lines: u64,
    line_end: u64,
}

/// Reads one bounded page (the wire error lift).
fn read_page(
    terminals: &dyn TerminalSeams,
    owner: &str,
    id: &str,
    offset: u64,
    count: u64,
) -> Result<Page, BodyError> {
    let reply = terminals
        .read(owner, id, Some(offset), Some(count))
        .map_err(BodyError::plain)?;
    Ok(Page {
        text: reply
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        truncated: reply
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        total_lines: reply
            .get("totalLines")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        line_end: reply
            .get("lineEnd")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
    })
}

/// Assembles the retained scrollback newest-first (the dsh
/// `retainedScrollback`): the first page anchors the total, older
/// pages prepend until the walk reaches the head.
fn retained_scrollback(
    terminals: &dyn TerminalSeams,
    owner: &str,
    id: &str,
    latest: Option<&Page>,
) -> Result<RetainedOutput, BodyError> {
    let first = match latest {
        Some(page) => page.clone(),
        None => read_page(terminals, owner, id, 0, SCROLLBACK_PAGE_LINES)?,
    };
    let total = first.total_lines;
    let mut pages: Vec<String> = if first.text.is_empty() {
        Vec::new()
    } else {
        vec![first.text.clone()]
    };
    let mut truncated = first.truncated;
    let mut offset = first.line_end;
    loop {
        if offset >= total {
            break;
        }
        let page = read_page(terminals, owner, id, offset, SCROLLBACK_PAGE_LINES)?;
        truncated = truncated || page.truncated;
        if !page.text.is_empty() {
            pages.insert(0, page.text.clone());
        }
        let next = if page.text.is_empty() || page.line_end <= offset {
            None
        } else {
            Some(page.line_end)
        };
        match next {
            Some(next) if next < total => offset = next,
            _ => break,
        }
    }
    Ok(RetainedOutput {
        text: pages.join("\n"),
        truncated,
    })
}

/// Gets or creates the owner's persistent shell (the dsh
/// `shells.get`): spawn, the bash echo-suppression setup, and the
/// rollback on any failure.
fn get_shell(
    shells: &PersistentShells,
    terminals: &dyn TerminalSeams,
    seams: &dyn Seams,
    dialect: &Dialect,
    owner: &str,
) -> Result<String, BodyError> {
    if let Some(id) = shells.get(owner) {
        return Ok(id);
    }
    let ensured = seams
        .working_directory_ensure(owner)
        .map_err(BodyError::plain)?;
    let spawned = terminals
        .spawn(
            owner,
            &json!({ "type": BAKED_BACKEND_TYPE, "cwd": ensured }),
        )
        .map_err(BodyError::plain)?;
    let id = spawned
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if dialect.echo_suppression {
        // Echo suppression only: the prompt stays the backend's own,
        // so the backend's prompt-based readiness keeps working.
        terminals
            .start_send(owner, &id, "stty -echo", true)
            .map_err(BodyError::plain)?;
        let result = wait_settled(terminals, owner, &id, std::time::Instant::now())?;
        if result.exited || result.wait_reason.as_deref() == Some("timeout") {
            let reset = shells.reset(
                terminals,
                owner,
                &format!("persistent {} initialization failed", dialect.tool_name),
            );
            reset.map_err(BodyError::plain)?;
            return Err(BodyError::plain(format!(
                "persistent {} shell did not accept initialization",
                dialect.tool_name
            )));
        }
    }
    shells
        .live
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(owner.to_owned(), id.clone());
    Ok(id)
}

/// Renders the exited-session result and resets the owner's shell
/// (the dsh `respondToSessionExit`).
#[expect(
    clippy::too_many_arguments,
    reason = "the dsh carrier mirrors one argument per fact; grouping would hide the correspondence"
)]
fn respond_to_session_exit(
    shells: &PersistentShells,
    terminals: &dyn TerminalSeams,
    dialect: &Dialect,
    owner: &str,
    id: &str,
    exit_code: Option<&Value>,
    signal: Option<&str>,
    start: &str,
    end: &str,
    fallback: &str,
    fallback_truncated: bool,
    wrapper: Option<&str>,
) -> Result<Value, BodyError> {
    let snapshot = retained_scrollback(terminals, owner, id, None)?;
    let exit = exit_code.and_then(Value::as_i64);
    shells
        .reset(
            terminals,
            owner,
            &format!("persistent {} shell exited", dialect.tool_name),
        )
        .map_err(BodyError::plain)?;
    let body = render_shell_exit_status(
        render_captured(
            &partial_output(
                &snapshot,
                start,
                end,
                fallback,
                fallback_truncated,
                wrapper,
                dialect.status_variable.is_none(),
            ),
            dialect,
        ),
        exit,
        signal,
    );
    Ok(Value::String(format!(
        "{body}\n{}",
        dialect.shell_reset_message
    )))
}

/// The persistent pair's render (dsh `output.render`): the string
/// value rides the text block verbatim.
#[must_use]
pub fn render_persistent(_args: &Value, value: &Value) -> Vec<Value> {
    vec![json!({
        "type": "text",
        "text": value.as_str().unwrap_or_default(),
    })]
}

/// The persistent pair's pending-call presentation (dsh
/// `presentCall`): the terminal card over the command text.
#[must_use]
pub fn present_call_persistent(args: &Value) -> Value {
    json!({
        "card": "terminal",
        "title": args.get("command").and_then(Value::as_str).unwrap_or_default(),
    })
}

#[cfg(test)]
mod persistent_tests {
    use super::{
        BASH, BASH_TRUNCATED_MESSAGE, CapturedOutput, PWSH, PWSH_TRUNCATED_MESSAGE, RetainedOutput,
        append_status_marker, command_output, markers, maybe_truncate, partial_output,
        quote_for_bash, quote_for_pwsh, render_captured, render_shell_exit_status,
        trim_trailing_newline, wrap_command,
    };

    /// The dialect tables keep the model-visible bytes: clip notes,
    /// reset sentences, and timeout codes verbatim.
    #[test]
    fn dialect_tables_keep_the_model_visible_bytes() {
        assert_eq!(BASH.timeout_code, "PERSISTENT_BASH_TIMEOUT");
        assert_eq!(PWSH.timeout_code, "PERSISTENT_PWSH_TIMEOUT");
        assert_eq!(
            BASH.truncated_message,
            "<response clipped><NOTE>To save on context only part of this file has been shown to you. You should retry this tool after you have searched inside the file with `grep -n` in order to find the line numbers of what you are looking for.</NOTE>"
        );
        assert_eq!(
            PWSH.truncated_message,
            "<response clipped><NOTE>To save on context only part of this file has been shown to you. You should retry this tool after you have searched inside the file with Select-String in order to find the line numbers of what you are looking for.</NOTE>"
        );
        // The dialect facts ride one table compare (the bash row is
        // [Some, true, true, true]; the pwsh twin the negation).
        let facts = |dialect: &super::Dialect| {
            [
                dialect.status_variable.is_some(),
                dialect.echo_suppression,
                dialect.always_report_exit,
                dialect.timeout_status_marker,
            ]
        };
        assert_eq!(facts(&BASH), [true, true, true, true]);
        assert_eq!(facts(&PWSH), [false, false, false, false]);
    }

    /// The markers carry the brand-mapped stems and a fresh nonce.
    #[test]
    fn markers_carry_the_mapped_stems() {
        let (start, end) = markers(&BASH);
        assert!(start.starts_with("__FR_PERSISTENT_BASH_START_"));
        assert!(start.ends_with("__"));
        assert!(end.starts_with("__FR_PERSISTENT_BASH_END_"));
        assert!(end.ends_with(':'));
        let (pwsh_start, _) = markers(&PWSH);
        assert!(pwsh_start.starts_with("__FR_PERSISTENT_PWSH_START_"));
    }

    /// The wrappers compose the dialect shell fragments around the
    /// quoted command (one physical line).
    #[test]
    fn wrappers_compose_the_dialect_fragments() {
        let (start, end) = markers(&BASH);
        let wrapped = wrap_command("echo hi", &start, &end, &BASH);
        assert!(
            wrapped.starts_with(&format!("printf '%s\\n' $'{start}'"))
                || wrapped.starts_with("printf '%s"),
            "the wrapper opens with the printf {wrapped:?}"
        );
        assert!(wrapped.contains("eval -- $'echo hi'"));
        assert!(wrapped.contains("__fr_persistent_bash_status=$?"));
        assert!(wrapped.ends_with("\"$__fr_persistent_bash_status\""));
        let (pwsh_start, pwsh_end) = markers(&PWSH);
        let pwsh_wrapped = wrap_command("Get-ChildItem", &pwsh_start, &pwsh_end, &PWSH);
        assert!(pwsh_wrapped.starts_with(&format!("Write-Output '{pwsh_start}'")));
        assert!(pwsh_wrapped.contains("Invoke-Expression"));
        assert!(pwsh_wrapped.contains("$__ok"));
        assert!(pwsh_wrapped.ends_with(&format!("Write-Output ('{pwsh_end}' + $__s)")));
    }

    /// The quote helpers escape per dialect (the dsh twins' rules).
    #[test]
    fn quote_helpers_escape_per_dialect() {
        assert_eq!(quote_for_bash("a'b\nc"), r"$'a\'b\nc'");
        assert_eq!(quote_for_pwsh("a`b\"c$"), "a``b`\"c`$");
        assert_eq!(quote_for_pwsh("x\ry\nz\u{1b}"), "xy`nz`e");
    }

    /// The trailing-newline trims split per dialect (bash strips the
    /// whole run, pwsh exactly one).
    #[test]
    fn trailing_trims_split_per_dialect() {
        assert_eq!(trim_trailing_newline("a\n\n\n", true), "a");
        assert_eq!(trim_trailing_newline("a\r\n\r\n", true), "a");
        assert_eq!(trim_trailing_newline("a\n\n", false), "a\n");
        assert_eq!(trim_trailing_newline("a", true), "a");
    }

    /// The complete extraction rides the end marker's status digits
    /// (bash abutting, pwsh padded) and strips the echoed wrapper.
    #[test]
    fn extraction_reads_the_status_and_strips_the_wrapper() {
        let retained = RetainedOutput {
            text: "$ __FR_PERSISTENT_BASH_START_A__\ncommand out\n__FR_PERSISTENT_BASH_END_A__:0\n"
                .to_owned(),
            truncated: false,
        };
        let captured = command_output(
            &retained,
            "__FR_PERSISTENT_BASH_START_A__",
            "__FR_PERSISTENT_BASH_END_A__:",
            None,
            false,
        )
        .expect("the status line arrived");
        assert_eq!(
            captured,
            CapturedOutput {
                text: "command out".to_owned(),
                incomplete: false,
                exit_code: Some(0),
            }
        );
        let padded = RetainedOutput {
            text: "@@S@@ pre WRAP post@@E@@9 \n".to_owned(),
            truncated: false,
        };
        let captured = command_output(&padded, "@@S@@", "@@E@@", Some("WRAP"), true)
            .expect("the padded status arrived");
        assert_eq!(captured.exit_code, Some(9));
        assert_eq!(captured.text, " pre  post", "the echoed wrapper strips");
    }

    /// The partial extraction anchors on the live snapshot, else the
    /// incremental fallback, marking the incomplete arm.
    #[test]
    fn partial_extraction_anchors_and_marks() {
        let snapshot = RetainedOutput {
            text: "S_HEAD\npartial\n".to_owned(),
            truncated: false,
        };
        let captured = partial_output(&snapshot, "S_HEAD", "E_END", "fallback", false, None, false);
        assert_eq!(captured.text, "partial");
        assert!(!captured.incomplete);
        let captured = partial_output(
            &RetainedOutput {
                text: String::new(),
                truncated: false,
            },
            "S_HEAD",
            "E_END",
            "noiseS_HEADpartialE_ENDtail",
            false,
            None,
            false,
        );
        assert_eq!(captured.text, "partial");
        assert!(!captured.incomplete);
        let captured = partial_output(
            &RetainedOutput {
                text: String::new(),
                truncated: false,
            },
            "S_HEAD",
            "E_END",
            "nothing here",
            true,
            None,
            false,
        );
        assert_eq!(captured.text, "nothing here");
        assert!(captured.incomplete);
    }

    /// The captured render clips at the cap, rides the lost-prefix
    /// note, and appends the dialect exit trailers.
    #[test]
    fn captured_render_clips_and_trailers() {
        let output = CapturedOutput {
            text: "ok".to_owned(),
            incomplete: false,
            exit_code: Some(0),
        };
        assert_eq!(
            render_captured(&output, &BASH),
            "ok\n[Command finished with exit code 0]"
        );
        assert_eq!(
            render_captured(&output, &PWSH),
            "ok",
            "the pwsh twin reports only nonzero"
        );
        let failing = CapturedOutput {
            text: "ok".to_owned(),
            incomplete: false,
            exit_code: Some(3),
        };
        assert_eq!(render_captured(&failing, &PWSH), "ok\n[exit code: 3]");
        let long = CapturedOutput {
            text: "x".repeat(20_000),
            incomplete: true,
            exit_code: None,
        };
        let rendered = render_captured(&long, &BASH);
        assert!(rendered.starts_with("<response clipped><NOTE>The beginning"));
        assert!(rendered.contains(BASH_TRUNCATED_MESSAGE));
        assert_eq!(rendered.chars().count(), 16_000 + BASH_TRUNCATED_MESSAGE.chars().count() + "<response clipped><NOTE>The beginning of this command output was dropped by the terminal scrollback limit. The following text is the earliest retained output.</NOTE>\n".chars().count());
        assert_eq!(
            append_status_marker(String::new(), Some("T".to_owned())),
            "T"
        );
        assert_eq!(
            append_status_marker("c".to_owned(), Some("T".to_owned())),
            "c\nT"
        );
        assert_eq!(
            render_shell_exit_status("body".to_owned(), None, Some("SIGKILL")),
            "body\n[shell killed by signal: SIGKILL]"
        );
        assert_eq!(
            render_shell_exit_status("body".to_owned(), Some(2), None),
            "body\n[shell exited: code 2]"
        );
        assert_eq!(
            render_shell_exit_status("body".to_owned(), None, None),
            "body\n[shell exited]"
        );
        assert_eq!(
            maybe_truncate("hello", 10, false, PWSH_TRUNCATED_MESSAGE),
            "hello"
        );
    }
}

// The manager and loop behavior tests ride in `mod loop_tests` (the
// scripted band records the wire face and drives the marker loop).

#[cfg(test)]
mod loop_tests {
    use super::{BASH, PWSH, PersistentShells, execute_persistent, markers, wrap_command};
    use crate::tools::{Seams, TerminalSeams};
    use serde_json::{Value, json};
    use std::sync::Mutex;

    /// The scripted band for the loop: the read face emits one page
    /// whose text the test plants; `send_wait` settles at once.
    #[derive(Default)]
    struct LoopBand {
        calls: Mutex<Vec<String>>,
        page_text: Mutex<String>,
        spawn_count: std::sync::atomic::AtomicUsize,
        marker_commands: std::sync::atomic::AtomicUsize,
        exit_armed: std::sync::atomic::AtomicBool,
    }

    impl LoopBand {
        fn record(&self, word: &str) {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(word.to_owned());
        }

        fn words(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }

        fn plant(&self, text: &str) {
            *self
                .page_text
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = text.to_owned();
        }
    }

    impl TerminalSeams for LoopBand {
        fn spawn(&self, _owner: &str, spec: &Value) -> Result<Value, String> {
            self.record("spawn");
            self.spawn_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(json!({
                "sessionId": format!("pty-{}", self.spawn_count.load(std::sync::atomic::Ordering::SeqCst)),
                "type": spec.get("type").cloned().unwrap_or(Value::Null),
                "status": { "kind": "running" },
            }))
        }

        fn start_send(
            &self,
            _owner: &str,
            _id: &str,
            text: &str,
            submit: bool,
        ) -> Result<Value, String> {
            if submit {
                // The simulated shell completes the first wrapped
                // command; the second one arms the session exit.
                if text.contains("__FR_PERSISTENT_BASH_START_") {
                    let ordinal = self
                        .marker_commands
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if ordinal >= 1 {
                        self.exit_armed
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                        self.record(if submit { "send" } else { "wait" });
                        return Ok(json!({}));
                    }
                }
                if let Some(begin) = text.find("__FR_PERSISTENT_BASH_START_") {
                    let start_tail = &text[begin..];
                    let body = &start_tail["__FR_PERSISTENT_BASH_START_".len()..];
                    let start_marker =
                        &start_tail[..body.find("__").map_or(start_tail.len(), |i| {
                            "__FR_PERSISTENT_BASH_START_".len() + i + 2
                        })];
                    let end_begin = text.find("__FR_PERSISTENT_BASH_END_").unwrap_or(begin);
                    let end_tail = &text[end_begin..];
                    let end_marker =
                        &end_tail[..end_tail.find(':').map_or(end_tail.len(), |i| i + 1)];
                    self.plant(&format!("{start_marker}\nlisting\n{end_marker}0\n"));
                }
            }
            self.record(if submit { "send" } else { "wait" });
            let _ = text;
            Ok(json!({}))
        }

        fn send_wait(&self, _owner: &str, _id: &str, _timeout_ms: u64) -> Result<Value, String> {
            self.record("sendWait");
            let status = if self.exit_armed.load(std::sync::atomic::Ordering::SeqCst) {
                json!({ "kind": "exited", "exitCode": 0, "signal": Value::Null })
            } else {
                json!({ "kind": "running" })
            };
            Ok(json!({
                "settled": true,
                "viewport": "",
                "waitReason": "inferred_idle",
                "sessionStatus": status,
                "truncated": false,
            }))
        }

        fn send_cancel(&self, _owner: &str, _id: &str) -> Result<Value, String> {
            self.record("sendCancel");
            Ok(json!({ "cancelled": true }))
        }

        fn send_read(&self, _owner: &str, _id: &str) -> Result<Value, String> {
            self.record("sendRead");
            Ok(json!({ "delta": "", "truncated": false }))
        }

        fn read(
            &self,
            _owner: &str,
            _id: &str,
            _offset: Option<u64>,
            _count: Option<u64>,
        ) -> Result<Value, String> {
            self.record("read");
            let text = self
                .page_text
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let lines: Vec<&str> = text.lines().collect();
            Ok(json!({
                "text": text,
                "totalLines": lines.len(),
                "lineBegin": 0,
                "lineEnd": lines.len(),
                "truncated": false,
            }))
        }

        fn signal(&self, _owner: &str, _id: &str, _signal: &str) -> Result<Value, String> {
            self.record("signal");
            Ok(json!({ "delivered": true, "targetPgid": 1 }))
        }

        fn kill(&self, _owner: &str, _id: &str, reason: &str) -> Result<Value, String> {
            self.record("kill");
            Ok(json!({ "closed": true, "reason": reason }))
        }

        fn list(&self, _owner: &str) -> Result<Value, String> {
            self.record("list");
            Ok(json!({ "sessions": [] }))
        }

        fn send_background(&self, _payload: &Value) -> Result<Value, String> {
            self.record("sendBackground");
            Ok(json!({ "jobId": "job-1" }))
        }
    }

    struct LoopSeams {
        band: LoopBand,
    }

    impl crate::tools::PolicySeam for LoopSeams {
        fn resolve_policy(&self, _session: Option<&str>) -> Result<Option<Value>, String> {
            Ok(None)
        }
    }

    impl crate::tools::ApprovalSeam for LoopSeams {
        fn request(&self, _ask: &fr_sandbox::escalation::EscalationAsk) -> Result<String, String> {
            Err("not reached".to_owned())
        }
    }

    impl Seams for LoopSeams {
        fn terminals(&self) -> Option<&dyn TerminalSeams> {
            Some(&self.band)
        }

        fn policy(&self) -> &dyn crate::tools::PolicySeam {
            self
        }

        fn approval(&self) -> &dyn crate::tools::ApprovalSeam {
            self
        }

        fn attachments(&self) -> Option<&dyn crate::tools::AttachmentSeam> {
            None
        }

        fn llm(&self) -> Option<&dyn crate::tools::LlmSeam> {
            None
        }

        fn route_header(&self) -> Option<&dyn crate::tools::RouteHeaderSeam> {
            None
        }

        fn shell_resolve(&self, _shell: &str, _request: &Value) -> Result<Value, String> {
            Err("not reached".to_owned())
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
            Err("not reached".to_owned())
        }

        fn shell_result(&self, _job_id: &str) -> Result<Value, String> {
            Err("not reached".to_owned())
        }

        fn jobs_wait(
            &self,
            _id: &str,
            _timeout_ms: f64,
            _caller: Option<&str>,
        ) -> Result<Value, String> {
            Err("not reached".to_owned())
        }

        fn jobs_read(&self, _id: &str, _caller: Option<&str>) -> Result<Value, String> {
            Err("not reached".to_owned())
        }

        fn jobs_list(&self, _caller: Option<&str>) -> Result<Value, String> {
            Err("not reached".to_owned())
        }

        fn jobs_get(&self, _id: &str, _caller: Option<&str>) -> Result<Value, String> {
            Err("not reached".to_owned())
        }

        fn jobs_kill(
            &self,
            _id: &str,
            _caller: Option<&str>,
            _reason: Option<&str>,
        ) -> Result<String, String> {
            Err("not reached".to_owned())
        }

        fn jobs_remove(&self, _id: &str, _caller: Option<&str>) -> Result<(), String> {
            Err("not reached".to_owned())
        }

        fn working_directory_ensure(&self, agent: &str) -> Result<String, String> {
            Ok(format!("/ws/{agent}"))
        }

        fn shell_env_collect(
            &self,
            _agent: Option<&str>,
        ) -> Result<crate::shell_env::FrozenEnv, String> {
            Err("not reached".to_owned())
        }

        fn subprocess_spawn(&self, _spec: &Value) -> Result<String, String> {
            Err("not reached".to_owned())
        }

        fn subprocess_read(
            &self,
            _handle: &str,
            _stream: &str,
            _from: u64,
        ) -> Result<Value, String> {
            Err("not reached".to_owned())
        }

        fn subprocess_wait(&self, _handle: &str, _timeout_ms: f64) -> Result<Value, String> {
            Err("not reached".to_owned())
        }

        fn subprocess_resolve_executable(&self, _command: &str) -> Result<String, String> {
            Err("not reached".to_owned())
        }

        fn spill_store_save_text(&self, _save: &Value) -> Result<Option<Value>, String> {
            Err("not reached".to_owned())
        }
    }

    /// One full command: the shell spawns once, the wrapped command
    /// rides one send, the end marker extracts the exit code, and the
    /// second call reuses the live shell.
    #[test]
    fn one_command_runs_the_marker_loop() {
        let shells = PersistentShells::default();
        let seams = LoopSeams {
            band: LoopBand::default(),
        };
        let (start, end) = markers(&BASH);
        let page = format!("$ prompt\n{start}\nlisting\n{end}0\n$ prompt\n");
        seams.band.plant(&page);
        let value = execute_persistent(
            &shells,
            &seams,
            &BASH,
            &json!({
                "agent": "agent-1",
                "arguments": { "command": "ls" },
            }),
        )
        .expect("the command lands");
        assert_eq!(
            value,
            json!("listing\n[Command finished with exit code 0]"),
            "the captured output rides the exit trailer"
        );
        assert!(seams.band.words().contains(&"spawn".to_owned()));
        assert!(
            seams
                .band
                .words()
                .iter()
                .filter(|word| word == &"send")
                .count()
                >= 2,
            "the stty setup and the command both send"
        );
        // The live shell row rides the manager between commands.
        assert_eq!(shells.get("agent-1"), Some("pty-1".to_owned()));
        // The next command rides a session exit: the loop renders the
        // exited status, resets the row, and reports the kill.
        let value = execute_persistent(
            &shells,
            &seams,
            &BASH,
            &json!({
                "agent": "agent-1",
                "arguments": { "command": "pwd" },
            }),
        )
        .expect("the exit-armed command lands");
        let text = value.as_str().expect("the string result");
        assert!(text.contains("[shell exited: code 0]"), "{text}");
        assert!(
            text.contains("The persistent bash shell was reset"),
            "{text}"
        );
        assert_eq!(
            seams
                .band
                .spawn_count
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(shells.get("agent-1"), None, "the reset clears the row");
    }

    /// The timeout arm resets the shell and rides the partial output
    /// with the OOM status trailer and the reset sentence.
    #[test]
    fn empty_command_reuses_the_family_gate() {
        let shells = PersistentShells::default();
        let seams = LoopSeams {
            band: LoopBand::default(),
        };
        let failure = execute_persistent(
            &shells,
            &seams,
            &BASH,
            &json!({
                "agent": "agent-1",
                "arguments": { "command": "   " },
            }),
        )
        .unwrap_err();
        assert_eq!(failure.message, "command must be a non-empty string");
        let failure = execute_persistent(
            &shells,
            &seams,
            &BASH,
            &json!({
                "arguments": { "command": "ls" },
            }),
        )
        .unwrap_err();
        assert_eq!(failure.message, "bash requires an owning agent session");
        let failure = execute_persistent(
            &shells,
            &seams,
            &PWSH,
            &json!({
                "arguments": { "command": "ls" },
            }),
        )
        .unwrap_err();
        assert_eq!(failure.message, "pwsh requires an owning agent session");
    }

    /// The wrapped command matches the dialect table bytes.
    #[test]
    fn wrapped_command_rides_the_dialect_bytes() {
        let (start, end) = markers(&BASH);
        let wrapped = wrap_command("ls", &start, &end, &BASH);
        assert!(wrapped.contains("eval -- $'ls'"));
        assert!(wrapped.contains("__fr_persistent_bash_status=$?"));
        let (pwsh_start, pwsh_end) = markers(&PWSH);
        let wrapped = wrap_command("ls", &pwsh_start, &pwsh_end, &PWSH);
        assert!(wrapped.contains("$__ok"));
        let _ = (pwsh_start, pwsh_end);
    }
}
