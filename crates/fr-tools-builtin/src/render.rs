// Rust guideline compliant 2026-10-29
//! The model-facing result rendering for the shell tools (dsh
//! `tool-bash/src/render.ts` + `tool-pwsh/src/render.ts` + the shared
//! `dsh-shell` marker contract, verbatim texts).
//!
//! One finished run renders as: stdout, then a marked `[stderr]`
//! section, then the marker stack (sandbox denial, the escalation hint
//! this composition advertises, timeout, external stop, and the
//! exit-status marker anchored last). Non-zero exits are reported, not
//! errored — only infrastructure failures surface as error results.

use serde_json::Value;

/// Appends the truncation notice (with the full-output spill path) to a
/// stream's text (dsh `streamText`).
#[must_use]
pub fn stream_text(text: &str, truncated: bool, spill_path: Option<&str>) -> String {
    if !truncated {
        return text.to_owned();
    }
    format!(
        "{text}\n[output truncated; full output: {}]",
        spill_path.unwrap_or("(unavailable)")
    )
}

/// The marker set the exit pill parses back out (dsh `parseExitStatus`
/// return shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedExitStatus {
    /// The output body and the recovered exit code (a clean exit when
    /// no marker matched).
    ExitCode {
        /// The marker-free output body.
        body: String,
        /// The recovered exit code.
        code: i64,
    },
    /// The output body and the recovered signal.
    Signal {
        /// The marker-free output body.
        body: String,
        /// The recovered signal name.
        signal: String,
    },
}

/// Splits a rendered result into the output body and the structured
/// exit status (dsh `parseExitStatus`, the regex anchors verbatim).
///
/// A signal marker wins over an exit marker; neither means a clean
/// exit 0; the consumed marker leaves the body.
#[must_use]
pub fn parse_exit_status(text: &str) -> ParsedExitStatus {
    let Some(signal) = find_tail_marker(text, "[killed by signal: ", "]") else {
        let Some(exit) = find_tail_exit(text) else {
            return ParsedExitStatus::ExitCode {
                body: text.to_owned(),
                code: 0,
            };
        };
        return ParsedExitStatus::ExitCode {
            body: text[..exit.0].to_owned(),
            code: exit.1,
        };
    };
    ParsedExitStatus::Signal {
        body: text[..signal.0].to_owned(),
        signal: signal.1,
    }
}

/// Finds the `\n[exit code: N]$` tail marker; returns the body end and
/// the code.
fn find_tail_exit(text: &str) -> Option<(usize, i64)> {
    let marker_end = text.strip_suffix(']')?;
    let open = marker_end.rfind('\n')?;
    let inner = &marker_end[open + 1..];
    let digits = inner.strip_prefix("[exit code: ")?;
    let code: i64 = digits.parse().ok()?;
    Some((open, code))
}

/// Finds the `\n[killed by signal: NAME]$` tail marker.
fn find_tail_marker(text: &str, prefix: &str, suffix: &str) -> Option<(usize, String)> {
    let inner = text.strip_suffix(suffix)?;
    let open = inner.rfind('\n')?;
    let line = &inner[open + 1..];
    let name = line.strip_prefix(prefix)?;
    if name.contains('\n') || name.contains(']') {
        return None;
    }
    Some((open, name.to_owned()))
}

/// Renders one finished run (dsh `renderResult`): the output body, the
/// `[stderr]` section, then the marker stack in dsh order.
///
/// `result` is the canonical foreground value (the wire's camelCase
/// form: `exitCode`, `signal`, `timedOut`, `aborted`, `timeoutMs`,
/// `stdout`, `stderr`, `sandbox?`, `stopped?`).
#[must_use]
pub fn render_result(result: &Value, escalation_modes: &[&str]) -> String {
    let stdout = stream_value(result.get("stdout"));
    let stderr = stream_value(result.get("stderr"));
    let mut body = stdout;
    if !stderr.is_empty() {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("[stderr]\n");
        body.push_str(&stderr);
    }
    if body.is_empty() {
        "(no output)".clone_into(&mut body);
    }
    let mut markers: Vec<String> = Vec::new();
    if let Some(sandbox) = result.get("sandbox")
        && sandbox.get("denied").and_then(Value::as_bool) == Some(true)
    {
        let mode = sandbox
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or_default();
        markers.push(format!("[sandbox: file access denied under {mode} mode]"));
        if !escalation_modes.is_empty() {
            markers.push(escalation_hint());
        }
    }
    if result.get("timedOut").and_then(Value::as_bool) == Some(true) {
        let timeout = js_number_value(result.get("timeoutMs").unwrap_or(&Value::Null));
        markers.push(format!("[timed out after {timeout}ms]"));
    }
    if let Some(stopped) = result.get("stopped").and_then(Value::as_str) {
        markers.push(format!("[stopped: {stopped}]"));
    }
    match result.get("signal").and_then(Value::as_str) {
        Some(signal) => markers.push(format!("[killed by signal: {signal}]")),
        None => {
            if result.get("exitCode").and_then(Value::as_i64) != Some(0) {
                let code = js_number_value(result.get("exitCode").unwrap_or(&Value::Null));
                markers.push(format!("[exit code: {code}]"));
            }
        }
    }
    if markers.is_empty() {
        return body;
    }
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(&markers.join("\n"));
    body
}

/// Renders one JSON number the way dsh template interpolation does
/// (integers without a decimal tail, `null` verbatim).
#[must_use]
pub fn js_number_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Number(number) => {
            let integral = number.as_i64().map_or_else(
                || {
                    // The dsh interpolation renders integral Numbers
                    // without a decimal tail; serde_json keeps the f64
                    // form.
                    number.as_f64().and_then(|f| {
                        #[expect(
                            clippy::cast_possible_truncation,
                            reason = "the branch bounds the value under 1e21 and integral, so the cast keeps every digit"
                        )]
                        let integral = (f.fract() == 0.0 && f.abs() < 1e21).then_some(f as i64);
                        integral.map(|code| code.to_string())
                    })
                },
                |code| Some(code.to_string()),
            );
            integral.unwrap_or_else(|| number.to_string())
        }
        other => other.to_string(),
    }
}

/// Renders the promoted hand-off text (dsh `renderPromoted`).
#[must_use]
pub fn render_promoted(job_id: &str, timeout_ms: f64, output: &str) -> String {
    let body = if output.is_empty() {
        String::new()
    } else if output.ends_with('\n') {
        output.to_owned()
    } else {
        format!("{output}\n")
    };
    format!(
        "{body}[still running after {}ms; moved to background job {job_id}]\n\
The command keeps running in the background. You will be notified when it finishes; \
read newer output with job_output, stop it with job_kill.",
        js_number_value(&serde_json::json!(timeout_ms))
    )
}

/// Renders the one consuming registry read a promoted foreground call
/// embeds (dsh `renderJobRead`): the delta text, then the loss notice,
/// then the sandbox notices.
#[must_use]
pub fn render_job_read(
    delta: &str,
    lossy: bool,
    spill_paths: &[String],
    sandbox: Option<&Value>,
    escalation_modes: &[&str],
) -> String {
    let mut notices: Vec<String> = Vec::new();
    if lossy {
        let paths = if spill_paths.is_empty() {
            "(unavailable)".to_owned()
        } else {
            spill_paths.join(", ")
        };
        notices.push(format!(
            "[some output was dropped from memory; full output: {paths}]"
        ));
    }
    if let Some(sandbox) = sandbox {
        if sandbox.get("runnerFailed").and_then(Value::as_bool) == Some(true) {
            let mode = sandbox
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or_default();
            notices.push(format!(
                "[sandbox: the sandbox runner itself failed under {mode} mode — the command \
did not run; this is a sandbox problem, not a command failure]"
            ));
        } else if sandbox.get("denied").and_then(Value::as_bool) == Some(true) {
            let mode = sandbox
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or_default();
            notices.push(format!("[sandbox: file access denied under {mode} mode]"));
            if !escalation_modes.is_empty() {
                notices.push(escalation_hint());
            }
        }
    }
    if notices.is_empty() {
        return delta.to_owned();
    }
    let separator = if !delta.is_empty() && !delta.ends_with('\n') {
        "\n"
    } else {
        ""
    };
    format!("{delta}{separator}{}", notices.join("\n"))
}

/// The same-turn escalation hint (dsh `escalationHintMarker('command')`,
/// verbatim).
#[must_use]
fn escalation_hint() -> String {
    "[sandbox: escalation available — retry this exact command once with sandbox_permissions \
(the narrowest wider mode that suffices) + justification; the approval prompt asks the user]"
        .to_owned()
}

/// Projects one captured stream value onto its text (the canonical
/// `stdout`/`stderr` object: `text`, `truncated`, `spillPath?`).
fn stream_value(stream: Option<&Value>) -> String {
    let Some(stream) = stream else {
        return String::new();
    };
    stream_text(
        stream
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        stream.get("truncated").and_then(Value::as_bool) == Some(true),
        stream.get("spillPath").and_then(Value::as_str),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The marker stack renders in dsh order with the exit marker last;
    /// a clean run renders the bare body.
    #[test]
    fn render_result_stacks_the_markers_in_dsh_order() {
        let clean = json!({
            "exitCode": 0, "signal": null, "timedOut": false, "aborted": false,
            "timeoutMs": 100.0,
            "stdout": { "text": "out\n", "truncated": false },
            "stderr": { "text": "", "truncated": false },
        });
        assert_eq!(render_result(&clean, &[]), "out\n");
        let failing = json!({
            "exitCode": 3, "signal": null, "timedOut": true, "aborted": false,
            "timeoutMs": 5000.0,
            "stdout": { "text": "out", "truncated": false },
            "stderr": { "text": "boom", "truncated": false },
            "stopped": "the human stopped it",
        });
        assert_eq!(
            render_result(&failing, &[]),
            "out\n[stderr]\nboom\n[timed out after 5000ms]\n[stopped: the human stopped it]\n[exit code: 3]"
        );
        let denied = json!({
            "exitCode": 1, "signal": null, "timedOut": false, "aborted": false,
            "timeoutMs": 100.0,
            "stdout": { "text": "", "truncated": false },
            "stderr": { "text": "", "truncated": false },
            "sandbox": { "mode": "read-only", "denied": true },
        });
        assert_eq!(
            render_result(&denied, &["workspace-write"]),
            "(no output)\n[sandbox: file access denied under read-only mode]\n\
[sandbox: escalation available — retry this exact command once with sandbox_permissions \
(the narrowest wider mode that suffices) + justification; the approval prompt asks the user]\n[exit code: 1]"
        );
    }

    /// The promoted and job-read texts carry the dsh hand-off guidance
    /// and the loss/sandbox notices.
    #[test]
    fn promoted_and_job_read_carry_the_notices() {
        assert_eq!(
            render_promoted("bash-x", 1_000.0, ""),
            "[still running after 1000ms; moved to background job bash-x]\n\
The command keeps running in the background. You will be notified when it finishes; \
read newer output with job_output, stop it with job_kill."
        );
        assert_eq!(
            render_promoted("bash-x", 1_000.0, "part"),
            "part\n[still running after 1000ms; moved to background job bash-x]\n\
The command keeps running in the background. You will be notified when it finishes; \
read newer output with job_output, stop it with job_kill."
        );
        let sandbox = json!({ "mode": "read-only", "denied": true });
        assert_eq!(
            render_job_read("delta", true, &[], Some(&sandbox), &["workspace-write"]),
            "delta\n[some output was dropped from memory; full output: (unavailable)]\n\
[sandbox: file access denied under read-only mode]\n\
[sandbox: escalation available — retry this exact command once with sandbox_permissions \
(the narrowest wider mode that suffices) + justification; the approval prompt asks the user]"
        );
    }

    /// The exit-status parse splits the terminal marker from the body
    /// and defaults to a clean exit 0.
    #[test]
    fn parse_exit_status_splits_the_terminal_marker() {
        let signal = parse_exit_status("body\n[killed by signal: SIGTERM]");
        assert_eq!(
            signal,
            ParsedExitStatus::Signal {
                body: "body".to_owned(),
                signal: "SIGTERM".to_owned()
            }
        );
        let exit = parse_exit_status("body\n[exit code: 3]");
        assert_eq!(
            exit,
            ParsedExitStatus::ExitCode {
                body: "body".to_owned(),
                code: 3
            }
        );
        let clean = parse_exit_status("plain output\n");
        assert_eq!(
            clean,
            ParsedExitStatus::ExitCode {
                body: "plain output\n".to_owned(),
                code: 0
            }
        );
        // Marker-like text that does not anchor at the tail stays in the
        // body.
        let embedded = parse_exit_status("[exit code: 9] tail");
        assert_eq!(
            embedded,
            ParsedExitStatus::ExitCode {
                body: "[exit code: 9] tail".to_owned(),
                code: 0
            }
        );
    }
}
