// Rust guideline compliant 2026-11-01
//! The six model-facing terminal tools (dsh
//! `experimental/tool-terminal/index.ts`, one module per PRD 4.5).
//!
//! Owner identity comes from the executing call's agent; every PTY face
//! rides the [`crate::tools::TerminalSeams`] band. The background send
//! lowers onto the absorbed `sendBackground` arm (the closure cannot
//! cross the wire, so the job registration lives in the fr-shell
//! registry — the `shell.start` judgment). The dsh `exec.signal` abort
//! face has no wire carrier (the tool-abort channel ends at the
//! approval seam), so the foreground loop has no upstream-cancel arm —
//! the structured-deviation note rides the module contract.

use serde_json::{Value, json};

use crate::terminal_render::{
    RenderedSessionSnapshot, RenderedSessionStatus, bound_terminal_text, render_list, render_read,
    render_send, render_spawn,
};
use crate::tools::{BodyError, Seams, TerminalSeams};

/// The baked opt-in knob (the `tool-terminal` config row; the M1
/// composition leaves it off — the dsh base bundle mounts no terminal
/// plugin, the `BAKED_STR_REPLACE_EDITOR` judgment).
const BAKED_TERMINAL_TOOLS: bool = false;

/// The registration gate for the terminal family (the six tools, the
/// persistent pair, their section, and the finalize listener register
/// only when this answers true). The bake-time cap validation folds
/// the dsh apply throw (a cap below the minimum would drop
/// registry-issued ids from the creation acknowledgements).
pub(crate) const fn terminal_tools_registered() -> bool {
    BAKED_TERMINAL_TOOLS && BAKED_MAX_RESULT_BYTES >= MIN_MAX_RESULT_BYTES
}

/// The baked `enableRunInBackground` (the composition always advertises
/// the background variant; the dsh default).
pub(crate) const BAKED_ENABLE_RUN_IN_BACKGROUND: bool = true;

/// The baked `maxResultBytes` cap (the dsh default; one complete
/// terminal or task-output result).
pub(crate) const BAKED_MAX_RESULT_BYTES: usize = 256 * 1024;

/// The smallest cap that preserves registry-issued PTY and job ids in
/// creation acknowledgements (the dsh `MIN_MAX_RESULT_BYTES`).
pub(crate) const MIN_MAX_RESULT_BYTES: usize = 64;

/// One bounded slice of the send wait (the band discipline: the
/// consumer drives the slices; the terminal wait settles itself at the
/// session's own timeout, so the slice only bounds each wire hop).
pub(crate) const SEND_WAIT_SLICE_MS: u64 = 30_000;

/// The six tool names the finalize listener bounds (the shared
/// `finalizeContent` const in the dsh apply).
const TERMINAL_TOOL_NAMES: [&str; 6] = [
    "terminal_open",
    "terminal_send",
    "terminal_read",
    "terminal_signal",
    "terminal_close",
    "terminal_list",
];

/// The finalize listener's CPS pairing stash (the enter leg parks the
/// call and result; the exit leg consumes them — the jobs finalize
/// stash judgment).
///
/// The table stays separate so the two listener families cannot
/// cross-match tokens.
#[derive(Default)]
pub struct FinalizeParks {
    parks: std::sync::Mutex<std::collections::HashMap<u64, (Value, Value)>>,
}

impl std::fmt::Debug for FinalizeParks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FinalizeParks").finish_non_exhaustive()
    }
}

impl FinalizeParks {
    /// Parks one enter leg (the token identifies the pair).
    pub fn insert(&self, token: u64, call: Value, result: Value) {
        self.parks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(token, (call, result));
    }

    /// Takes one parked pair (the exit leg).
    pub fn remove(&self, token: u64) -> Option<(Value, Value)> {
        self.parks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&token)
    }
}

/// The non-empty session id (dsh `sessionId`).
fn session_id(args: &Value) -> Result<String, BodyError> {
    let id = args
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if id.is_empty() {
        return Err(BodyError::plain("sessionId must be a non-empty string"));
    }
    Ok(id.to_owned())
}

/// The terminals band (fail-closed when the composition mounts none —
/// the attachment-store `None` arm; the baked composition declares the
/// injection, so production cannot reach this).
fn terminal_seams(seams: &dyn Seams) -> Result<&dyn TerminalSeams, BodyError> {
    seams
        .terminals()
        .ok_or_else(|| BodyError::plain("terminal tools require the terminals service"))
}

/// Resolves the open request's cwd against the session directory (the
/// node `path.resolve(cwd, arg)` shape: an absolute argument wins, a
/// relative one joins).
fn resolve_cwd(cwd: &str, arg: &str) -> String {
    if std::path::Path::new(arg).is_absolute() {
        arg.to_owned()
    } else {
        crate::tools::normalize_join(cwd, arg)
    }
}

/// The `terminal_open` body (dsh `execute`): type validation, the
/// working-directory ensure, and the resolved cwd join.
///
/// # Errors
///
/// The empty-type refusal, the missing-agent refusal, or the ensure /
/// spawn refusal family.
pub fn execute_open(seams: &dyn Seams, call: &Value) -> Result<Value, BodyError> {
    let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
    let agent = call.get("agent").and_then(Value::as_str);
    let backend_type = arguments
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if backend_type.is_empty() {
        return Err(BodyError::plain("type must be a non-empty string"));
    }
    let owner = agent
        .map(str::to_owned)
        .ok_or_else(|| BodyError::plain("terminal tools require an initiating agent"))?;
    let ensured = seams
        .working_directory_ensure(&owner)
        .map_err(BodyError::plain)?;
    let cwd = arguments
        .get("cwd")
        .and_then(Value::as_str)
        .map_or_else(|| ensured.clone(), |arg| resolve_cwd(&ensured, arg));
    let mut spec = json!({ "type": backend_type, "cwd": cwd });
    if let Some(name) = arguments.get("name").and_then(Value::as_str) {
        spec["name"] = json!(name);
    }
    terminal_seams(seams)?
        .spawn(&owner, &spec)
        .map_err(BodyError::plain)
}

/// The `terminal_send` body (dsh `execute`): the background arm lowers
/// onto the absorbed send job; the foreground arm drives the wait
/// slices to settlement.
///
/// # Errors
///
/// The missing-agent or empty-session refusal, the disabled-background
/// refusal, or the terminals-band failure family.
pub fn execute_send(seams: &dyn Seams, call: &Value) -> Result<Value, BodyError> {
    let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
    let agent = call.get("agent").and_then(Value::as_str);
    let owner = agent
        .map(str::to_owned)
        .ok_or_else(|| BodyError::plain("terminal tools require an initiating agent"))?;
    let id = session_id(&arguments)?;
    let text = arguments
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let submit = arguments
        .get("submit")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let terminals = terminal_seams(seams)?;
    if arguments.get("run_in_background").and_then(Value::as_bool) == Some(true) {
        if !BAKED_ENABLE_RUN_IN_BACKGROUND {
            return Err(BodyError::plain(
                "background terminal sends are disabled by tool-terminal configuration",
            ));
        }
        // The dsh `ctx.get('jobs')` absence arm is structural: the
        // absorbed registration lives inside the terminals band (the
        // `shell.start` judgment), so the jobs face is always there.
        let label = format!("{id}: {}", if text.is_empty() { "(input)" } else { text });
        let reply = terminals
            .send_background(&json!({
                "owner": owner,
                "sessionId": id,
                "text": text,
                "submit": submit,
                "label": label,
                "callId": call.get("callId").cloned().unwrap_or(Value::Null),
                "rootCallId": call.get("rootCallId").cloned().unwrap_or(Value::Null),
                "outputLimitBytes": BAKED_MAX_RESULT_BYTES,
            }))
            .map_err(BodyError::plain)?;
        let job_id = reply
            .get("jobId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        return Ok(json!({ "kind": "background", "jobId": job_id }));
    }
    terminals
        .start_send(&owner, &id, text, submit)
        .map_err(BodyError::plain)?;
    loop {
        let reply = terminals
            .send_wait(&owner, &id, SEND_WAIT_SLICE_MS)
            .map_err(BodyError::plain)?;
        if reply.get("settled").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        return Ok(json!({
            "kind": "foreground",
            "viewport": reply.get("viewport").cloned().unwrap_or(Value::Null),
            "waitReason": reply.get("waitReason").cloned().unwrap_or(Value::Null),
            "sessionStatus": reply.get("sessionStatus").cloned().unwrap_or(Value::Null),
            "truncated": reply.get("truncated").cloned().unwrap_or(Value::Null),
        }));
    }
}

/// The `terminal_read` body (dsh `execute`): the bounded page.
///
/// # Errors
///
/// The empty-session refusal or the read refusal family.
pub fn execute_read(seams: &dyn Seams, call: &Value) -> Result<Value, BodyError> {
    let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
    let agent = call.get("agent").and_then(Value::as_str);
    let owner = agent
        .map(str::to_owned)
        .ok_or_else(|| BodyError::plain("terminal tools require an initiating agent"))?;
    let id = session_id(&arguments)?;
    let offset = arguments.get("offset").and_then(Value::as_u64);
    let count = arguments.get("count").and_then(Value::as_u64);
    terminal_seams(seams)?
        .read(&owner, &id, offset, count)
        .map_err(BodyError::plain)
}

/// The `terminal_signal` body (dsh `execute`).
///
/// # Errors
///
/// The empty-session refusal or the signal refusal family.
pub fn execute_signal(seams: &dyn Seams, call: &Value) -> Result<Value, BodyError> {
    let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
    let agent = call.get("agent").and_then(Value::as_str);
    let owner = agent
        .map(str::to_owned)
        .ok_or_else(|| BodyError::plain("terminal tools require an initiating agent"))?;
    let id = session_id(&arguments)?;
    let signal = arguments
        .get("signal")
        .and_then(Value::as_str)
        .unwrap_or_default();
    terminal_seams(seams)?
        .signal(&owner, &id, signal)
        .map_err(BodyError::plain)
}

/// The `terminal_close` body (dsh `execute`): the kill reply word
/// lowers onto the outcome vocabulary.
///
/// # Errors
///
/// The empty-session refusal or the kill refusal family.
pub fn execute_close(seams: &dyn Seams, call: &Value) -> Result<Value, BodyError> {
    let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
    let agent = call.get("agent").and_then(Value::as_str);
    let owner = agent
        .map(str::to_owned)
        .ok_or_else(|| BodyError::plain("terminal tools require an initiating agent"))?;
    let id = session_id(&arguments)?;
    let reply = terminal_seams(seams)?
        .kill(&owner, &id, "model request")
        .map_err(BodyError::plain)?;
    let closed = reply
        .get("closed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(json!({
        "sessionId": id,
        "outcome": if closed { "closed" } else { "already-closing" },
    }))
}

/// The `terminal_list` body (dsh `execute`): the owner-filtered
/// snapshots array.
///
/// # Errors
///
/// The missing-agent refusal or the transport family.
pub fn execute_list(seams: &dyn Seams, call: &Value) -> Result<Value, BodyError> {
    let agent = call.get("agent").and_then(Value::as_str);
    let owner = agent
        .map(str::to_owned)
        .ok_or_else(|| BodyError::plain("terminal tools require an initiating agent"))?;
    let reply = terminal_seams(seams)?
        .list(&owner)
        .map_err(BodyError::plain)?;
    Ok(reply.get("sessions").cloned().unwrap_or_else(|| json!([])))
}

/// The shared finalize content (dsh `finalizeContent`): one complete
/// text block bounds against the result cap; anything else passes
/// through.
#[must_use]
pub fn finalize_terminal_content(call: &Value, result: &Value) -> Option<Vec<Value>> {
    let name = call.get("name").and_then(Value::as_str)?;
    if !TERMINAL_TOOL_NAMES.contains(&name) {
        return None;
    }
    let content = result.get("content")?.as_array()?;
    if content.len() != 1 {
        return None;
    }
    let block = &content[0];
    if block.get("type").and_then(Value::as_str) != Some("text") {
        return None;
    }
    let raw = block.get("text")?.as_str()?;
    Some(vec![json!({
        "type": "text",
        "text": bound_terminal_text(raw, BAKED_MAX_RESULT_BYTES),
    })])
}

/// Builds one snapshot projection from the wire shape (the render face
/// runs after the output schema validated; the fallbacks are the
/// degraded-shape hosts).
fn snapshot_fields(value: &Value) -> RenderedSessionSnapshot {
    let fallback_status =
        RenderedSessionStatus::from_wire(value.get("status").unwrap_or(&Value::Null))
            .unwrap_or(RenderedSessionStatus::Running);
    value
        .get("sessionId")
        .and_then(Value::as_str)
        .zip(value.get("type").and_then(Value::as_str))
        .map_or_else(
            || RenderedSessionSnapshot {
                session_id: String::new(),
                name: None,
                backend_type: String::new(),
                pid: None,
                status: fallback_status.clone(),
            },
            |(session_id, backend_type)| RenderedSessionSnapshot {
                session_id: session_id.to_owned(),
                name: value.get("name").and_then(Value::as_str).map(str::to_owned),
                backend_type: backend_type.to_owned(),
                pid: value
                    .get("pid")
                    .and_then(Value::as_u64)
                    .and_then(|pid| u32::try_from(pid).ok()),
                status: fallback_status.clone(),
            },
        )
}

/// The `terminal_open` render (dsh `renderSpawn`).
#[must_use]
pub fn render_open(_args: &Value, value: &Value) -> Vec<Value> {
    let motd = value
        .get("motd")
        .and_then(Value::as_str)
        .unwrap_or_default();
    vec![json!({
        "type": "text",
        "text": render_spawn(&snapshot_fields(value), motd, BAKED_MAX_RESULT_BYTES),
    })]
}

/// The `terminal_send` render: the background acknowledgement or the
/// settled foreground template (dsh `output.render`).
#[must_use]
pub fn render_send_value(_args: &Value, value: &Value) -> Vec<Value> {
    let text = if value.get("kind").and_then(Value::as_str) == Some("background") {
        format!(
            "started background job {}",
            value
                .get("jobId")
                .and_then(Value::as_str)
                .unwrap_or_default()
        )
    } else {
        let status =
            RenderedSessionStatus::from_wire(value.get("sessionStatus").unwrap_or(&Value::Null))
                .unwrap_or(RenderedSessionStatus::Running);
        render_send(
            value
                .get("viewport")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            value
                .get("waitReason")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            &status,
            value
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or_default(),
            BAKED_MAX_RESULT_BYTES,
        )
    };
    vec![json!({ "type": "text", "text": text })]
}

/// The `terminal_read` render (dsh `renderRead`).
#[must_use]
pub fn render_read_value(_args: &Value, value: &Value) -> Vec<Value> {
    vec![json!({
        "type": "text",
        "text": render_read(
            value.get("text").and_then(Value::as_str).unwrap_or_default(),
            value.get("totalLines").and_then(Value::as_u64).unwrap_or_default(),
            value.get("lineBegin").and_then(Value::as_u64).unwrap_or_default(),
            value.get("lineEnd").and_then(Value::as_u64).unwrap_or_default(),
            value
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or_default(),
            BAKED_MAX_RESULT_BYTES,
        ),
    })]
}

/// The `terminal_signal` render (the delivered fact line).
#[must_use]
pub fn render_signal(args: &Value, value: &Value) -> Vec<Value> {
    vec![json!({
        "type": "text",
        "text": format!(
            "delivered {} to foreground process group {}",
            args.get("signal").and_then(Value::as_str).unwrap_or_default(),
            value
                .get("targetPgid")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
        ),
    })]
}

/// The `terminal_close` render (the outcome words).
#[must_use]
pub fn render_close(_args: &Value, value: &Value) -> Vec<Value> {
    let session_id = value
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let text = if value.get("outcome").and_then(Value::as_str) == Some("closed") {
        format!("closed terminal session {session_id}")
    } else {
        format!("terminal session {session_id} was already closing")
    };
    vec![json!({ "type": "text", "text": text })]
}

/// The `terminal_list` render (dsh `renderList`).
#[must_use]
pub fn render_list_value(_args: &Value, value: &Value) -> Vec<Value> {
    let sessions: Vec<RenderedSessionSnapshot> = value
        .as_array()
        .map(|rows| rows.iter().map(&|row| snapshot_fields(row)).collect())
        .unwrap_or_default();
    vec![json!({
        "type": "text",
        "text": render_list(&sessions, BAKED_MAX_RESULT_BYTES),
    })]
}

/// The `terminal_open` pending-call presentation (dsh `presentCall`).
#[must_use]
pub fn present_call_open(args: &Value) -> Value {
    let name = args.get("name").and_then(Value::as_str);
    let backend_type = args.get("type").and_then(Value::as_str).unwrap_or_default();
    json!({
        "card": "generic",
        "title": format!("Open terminal {}", name.unwrap_or(backend_type)),
        "kind": "execute",
    })
}

/// The `terminal_send` pending-call presentation: the background
/// generic card or the terminal card (dsh `presentCall`).
#[must_use]
pub fn present_call_send(args: &Value) -> Value {
    let session_id = args
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let text = args.get("text").and_then(Value::as_str).unwrap_or_default();
    if args.get("run_in_background").and_then(Value::as_bool) == Some(true) {
        return json!({
            "card": "generic",
            "title": format!("Send to terminal {session_id} in background"),
            "kind": "execute",
            "rawInput": text,
        });
    }
    json!({
        "card": "terminal",
        "title": if text.is_empty() { "(send input)" } else { text },
        "description": format!("Terminal {session_id}"),
    })
}

/// The `terminal_read` pending-call presentation.
#[must_use]
pub fn present_call_read(args: &Value) -> Value {
    json!({
        "card": "generic",
        "title": format!(
            "Read terminal {}",
            args.get("sessionId").and_then(Value::as_str).unwrap_or_default()
        ),
        "kind": "read",
        "rawInput": args,
    })
}

/// The `terminal_signal` pending-call presentation.
#[must_use]
pub fn present_call_signal(args: &Value) -> Value {
    json!({
        "card": "generic",
        "title": format!(
            "Signal terminal {}",
            args.get("sessionId").and_then(Value::as_str).unwrap_or_default()
        ),
        "kind": "execute",
        "rawInput": args,
    })
}

/// The `terminal_close` pending-call presentation.
#[must_use]
pub fn present_call_close(args: &Value) -> Value {
    json!({
        "card": "generic",
        "title": format!(
            "Close terminal {}",
            args.get("sessionId").and_then(Value::as_str).unwrap_or_default()
        ),
        "kind": "delete",
    })
}

/// The `terminal_list` pending-call presentation.
#[must_use]
pub fn present_call_list() -> Value {
    json!({
        "card": "generic",
        "title": "List terminal sessions",
        "kind": "read",
    })
}

/// The `terminal_send` result presentation (dsh `presentResult`): the
/// background arm and errors stay unpresented; the complete text rides
/// the terminal card.
#[must_use]
pub fn present_result_send(args: &Value, result: &Value) -> Option<Value> {
    if args.get("run_in_background").and_then(Value::as_bool) == Some(true)
        || result.get("isError").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    let content = result.get("content")?.as_array()?;
    if content.len() != 1 {
        return None;
    }
    let block = &content[0];
    if block.get("type").and_then(Value::as_str) != Some("text") {
        return None;
    }
    Some(json!({
        "card": "terminal",
        "output": block.get("text")?,
    }))
}

/// The `terminal_send` presentation metadata (dsh
/// `presentationMeta`): the foreground facts, `null` in the background
/// arm.
#[must_use]
pub fn presentation_meta_send(_args: &Value, value: &Value) -> Value {
    if value.get("kind").and_then(Value::as_str) != Some("foreground") {
        return Value::Null;
    }
    json!({
        "viewport": value.get("viewport").cloned().unwrap_or(Value::Null),
        "waitReason": value.get("waitReason").cloned().unwrap_or(Value::Null),
        "sessionStatus": value.get("sessionStatus").cloned().unwrap_or(Value::Null),
        "truncated": value.get("truncated").cloned().unwrap_or(Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::{BAKED_ENABLE_RUN_IN_BACKGROUND, BAKED_MAX_RESULT_BYTES, MIN_MAX_RESULT_BYTES};

    /// The baked composition carries the dsh defaults: the background
    /// variant rides and the cap preserves registry-issued ids.
    #[test]
    fn baked_terminal_config_carries_the_dsh_defaults() {
        // The bake facts ride one table compare: the background
        // variant rides, and the cap preserves registry-issued ids.
        let facts = [
            BAKED_ENABLE_RUN_IN_BACKGROUND,
            BAKED_MAX_RESULT_BYTES >= MIN_MAX_RESULT_BYTES,
            BAKED_MAX_RESULT_BYTES == 256 * 1024,
            MIN_MAX_RESULT_BYTES == 64,
        ];
        assert_eq!(facts, [true, true, true, true]);
    }
}

#[cfg(test)]
mod exec_tests {
    use super::{
        BAKED_MAX_RESULT_BYTES, execute_close, execute_list, execute_open, execute_read,
        execute_send, execute_signal, finalize_terminal_content, present_call_close,
        present_call_list, present_call_open, present_call_read, present_call_send,
        present_call_signal, present_result_send, presentation_meta_send, render_close,
        render_list_value, render_open, render_read_value, render_send_value, render_signal,
    };
    use crate::tools::Seams;
    use serde_json::{Value, json};
    use std::sync::Mutex;

    /// The scripted terminals band: recorded calls plus canned replies.
    #[derive(Default)]
    struct FakeTerminals {
        calls: Mutex<Vec<String>>,
        unsettled_waits: std::sync::atomic::AtomicUsize,
    }

    impl FakeTerminals {
        fn record(&self, word: &str) {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(word.to_owned());
        }

        fn call_words(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    impl crate::tools::TerminalSeams for FakeTerminals {
        fn spawn(&self, owner: &str, spec: &Value) -> Result<Value, String> {
            self.record("spawn");
            Ok(json!({
                "sessionId": "pty-1",
                "type": spec.get("type").cloned().unwrap_or(Value::Null),
                "status": { "kind": "running" },
                "motd": "welcome",
                "owner": owner,
                "cwd": spec.get("cwd").cloned().unwrap_or(Value::Null),
            }))
        }

        fn start_send(
            &self,
            _owner: &str,
            _id: &str,
            _text: &str,
            _submit: bool,
        ) -> Result<Value, String> {
            self.record("startSend");
            Ok(json!({}))
        }

        fn send_wait(&self, _owner: &str, _id: &str, _timeout_ms: u64) -> Result<Value, String> {
            let remaining = self
                .unsettled_waits
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            if remaining <= 1 {
                Ok(json!({
                    "settled": true,
                    "viewport": "out\n",
                    "waitReason": "inferred_idle",
                    "sessionStatus": { "kind": "running" },
                    "truncated": false,
                }))
            } else {
                Ok(json!({ "settled": false }))
            }
        }

        fn send_cancel(&self, _owner: &str, _id: &str) -> Result<Value, String> {
            self.record("sendCancel");
            Ok(json!({ "cancelled": true }))
        }

        fn send_read(&self, _owner: &str, _id: &str) -> Result<Value, String> {
            self.record("sendRead");
            Ok(json!({ "delta": "delta-text", "truncated": false }))
        }

        fn read(
            &self,
            _owner: &str,
            _id: &str,
            _offset: Option<u64>,
            _count: Option<u64>,
        ) -> Result<Value, String> {
            self.record("read");
            Ok(json!({
                "text": "page",
                "totalLines": 1,
                "lineBegin": 0,
                "lineEnd": 1,
                "truncated": false,
            }))
        }

        fn signal(&self, _owner: &str, _id: &str, signal: &str) -> Result<Value, String> {
            self.record("signal");
            Ok(json!({ "delivered": true, "targetPgid": 4242, "signal": signal }))
        }

        fn kill(&self, _owner: &str, _id: &str, reason: &str) -> Result<Value, String> {
            self.record("kill");
            Ok(json!({ "closed": true, "reason": reason }))
        }

        fn list(&self, owner: &str) -> Result<Value, String> {
            self.record("list");
            Ok(json!({ "sessions": [{
                "sessionId": "pty-1",
                "type": "shell",
                "status": { "kind": "running" },
                "owner": owner,
            }] }))
        }

        fn send_background(&self, payload: &Value) -> Result<Value, String> {
            self.record("sendBackground");
            let label = payload
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Ok(json!({ "jobId": format!("job-{label}") }))
        }
    }

    /// The scripted seam set: the terminals face rides; everything
    /// else is the not-reached stub set.
    #[derive(Default)]
    struct FakeSeams {
        terminals: FakeTerminals,
    }

    impl crate::tools::PolicySeam for FakeSeams {
        fn resolve_policy(&self, _session: Option<&str>) -> Result<Option<Value>, String> {
            Ok(None)
        }
    }

    impl crate::tools::ApprovalSeam for FakeSeams {
        fn request(&self, _ask: &fr_sandbox::escalation::EscalationAsk) -> Result<String, String> {
            Err("not reached".to_owned())
        }
    }

    impl Seams for FakeSeams {
        fn terminals(&self) -> Option<&dyn crate::tools::TerminalSeams> {
            Some(&self.terminals)
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

    fn call(arguments: &Value) -> Value {
        json!({
            "agent": "agent-1",
            "arguments": arguments,
            "callId": "call-1",
            "rootCallId": "root-1",
        })
    }

    /// The open body: the type gate, the ensure+resolve cwd join, and
    /// the spawned snapshot passthrough.
    #[test]
    fn open_body_resolves_the_cwd_and_spawns() {
        let seams = FakeSeams::default();
        let failure = execute_open(&seams, &call(&json!({ "type": "" }))).unwrap_err();
        assert_eq!(failure.message, "type must be a non-empty string");
        let no_agent =
            execute_open(&seams, &json!({ "arguments": { "type": "shell" } })).unwrap_err();
        assert_eq!(
            no_agent.message,
            "terminal tools require an initiating agent"
        );
        let value = execute_open(
            &seams,
            &call(&json!({ "type": "shell", "name": "main", "cwd": "sub" })),
        )
        .expect("the spawn lands");
        assert_eq!(value["sessionId"], json!("pty-1"));
        assert_eq!(
            value["cwd"],
            json!("/ws/agent-1/sub"),
            "the relative cwd joins"
        );
        let absolute = execute_open(&seams, &call(&json!({ "type": "shell", "cwd": "/abs" })))
            .expect("the absolute spawn lands");
        assert_eq!(absolute["cwd"], json!("/abs"), "the absolute cwd wins");
        assert!(seams.terminals.call_words().contains(&"spawn".to_owned()));
    }

    /// The send body: the empty-session gate, the background job
    /// acknowledgement, and the sliced foreground wait.
    #[test]
    fn send_body_runs_both_arms() {
        let seams = FakeSeams::default();
        let failure =
            execute_send(&seams, &call(&json!({ "sessionId": "", "text": "x" }))).unwrap_err();
        assert_eq!(failure.message, "sessionId must be a non-empty string");
        let value = execute_send(
            &seams,
            &call(&json!({
                "sessionId": "pty-1",
                "text": "ls",
                "run_in_background": true,
            })),
        )
        .expect("the background send lands");
        assert_eq!(
            value,
            json!({ "kind": "background", "jobId": "job-pty-1: ls" })
        );
        assert!(
            seams
                .terminals
                .call_words()
                .contains(&"sendBackground".to_owned())
        );
        // The foreground arm: one unsettled slice then the settled one.
        seams
            .terminals
            .unsettled_waits
            .store(2, std::sync::atomic::Ordering::SeqCst);
        let value = execute_send(&seams, &call(&json!({ "sessionId": "pty-1", "text": "ls" })))
            .expect("the foreground send settles");
        assert_eq!(value["kind"], json!("foreground"));
        assert_eq!(value["viewport"], json!("out\n"));
        assert_eq!(value["waitReason"], json!("inferred_idle"));
    }

    /// The read, signal, close, and list bodies pass the wire reply
    /// through with the outcome vocabulary.
    #[test]
    fn read_signal_close_list_bodies_project_the_replies() {
        let seams = FakeSeams::default();
        let value = execute_read(&seams, &call(&json!({ "sessionId": "pty-1", "count": 5 })))
            .expect("the read lands");
        assert_eq!(value["text"], json!("page"));
        let value = execute_signal(
            &seams,
            &call(&json!({ "sessionId": "pty-1", "signal": "SIGINT" })),
        )
        .expect("the signal lands");
        assert_eq!(value["delivered"], json!(true));
        let value =
            execute_close(&seams, &call(&json!({ "sessionId": "pty-1" }))).expect("the close lands");
        assert_eq!(value, json!({ "sessionId": "pty-1", "outcome": "closed" }));
        let value = execute_list(&seams, &call(&json!({}))).expect("the list lands");
        assert_eq!(value.as_array().map(Vec::len), Some(1));
        assert!(seams.terminals.call_words().contains(&"kill".to_owned()));
    }

    /// The pending-call cards ride the dsh shapes (all six).
    #[test]
    fn present_call_cards_match_the_shapes() {
        assert_eq!(
            present_call_open(&json!({ "type": "shell", "name": "main" })),
            json!({ "card": "generic", "title": "Open terminal main", "kind": "execute" })
        );
        assert_eq!(
            present_call_open(&json!({ "type": "shell" })),
            json!({ "card": "generic", "title": "Open terminal shell", "kind": "execute" })
        );
        assert_eq!(
            present_call_send(&json!({
                "sessionId": "pty-1",
                "text": "hi",
                "run_in_background": true,
            })),
            json!({
                "card": "generic",
                "title": "Send to terminal pty-1 in background",
                "kind": "execute",
                "rawInput": "hi",
            })
        );
        assert_eq!(
            present_call_send(&json!({ "sessionId": "pty-1", "text": "" })),
            json!({
                "card": "terminal",
                "title": "(send input)",
                "description": "Terminal pty-1",
            })
        );
        assert_eq!(
            present_call_read(&json!({ "sessionId": "pty-1" }))["title"],
            json!("Read terminal pty-1")
        );
        assert_eq!(
            present_call_signal(&json!({ "sessionId": "pty-1" }))["title"],
            json!("Signal terminal pty-1")
        );
        assert_eq!(
            present_call_close(&json!({ "sessionId": "pty-1" }))["kind"],
            json!("delete")
        );
        assert_eq!(
            present_call_list()["title"],
            json!("List terminal sessions")
        );
    }

    /// The send result presentation: the background arm and errors
    /// stay unpresented; the text rides the terminal card.
    #[test]
    fn send_present_result_and_meta_dual_arms() {
        let result = json!({
            "content": [{ "type": "text", "text": "rendered" }],
        });
        assert_eq!(
            present_result_send(&json!({ "run_in_background": true }), &result),
            None
        );
        assert_eq!(
            present_result_send(&json!({}), &json!({ "isError": true })),
            None
        );
        assert_eq!(
            present_result_send(&json!({}), &result),
            Some(json!({ "card": "terminal", "output": "rendered" }))
        );
        let foreground = json!({
            "kind": "foreground",
            "viewport": "v",
            "waitReason": "timeout",
            "sessionStatus": { "kind": "running" },
            "truncated": false,
        });
        assert_eq!(
            presentation_meta_send(&json!({}), &foreground),
            json!({
                "viewport": "v",
                "waitReason": "timeout",
                "sessionStatus": { "kind": "running" },
                "truncated": false,
            })
        );
        assert_eq!(
            presentation_meta_send(&json!({}), &json!({ "kind": "background", "jobId": "j" })),
            Value::Null
        );
    }

    /// The renders compose over the shared bounding vocabulary.
    #[test]
    fn renders_compose_the_dsh_templates() {
        let spawn = json!({
            "sessionId": "pty-1",
            "type": "shell",
            "status": { "kind": "running" },
            "motd": "hi",
        });
        let blocks = render_open(&json!({}), &spawn);
        assert!(
            blocks[0]["text"]
                .as_str()
                .expect("text")
                .starts_with("started terminal session pty-1 [type: shell]\nhi")
        );
        let foreground = json!({
            "kind": "foreground",
            "viewport": "",
            "waitReason": "stdin_read",
            "sessionStatus": { "kind": "running" },
            "truncated": false,
        });
        let blocks = render_send_value(&json!({}), &foreground);
        assert_eq!(
            blocks[0]["text"],
            json!("(no new output)\n[wait: stdin_read]\n[session: running]")
        );
        let background = json!({ "kind": "background", "jobId": "job-1" });
        let blocks = render_send_value(&json!({}), &background);
        assert_eq!(blocks[0]["text"], json!("started background job job-1"));
        let page = json!({
            "text": "body",
            "totalLines": 3,
            "lineBegin": 1,
            "lineEnd": 2,
            "truncated": false,
        });
        let blocks = render_read_value(&json!({}), &page);
        assert_eq!(blocks[0]["text"], json!("body\n[lines: 1-2 of 3]"));
        let blocks = render_signal(&json!({ "signal": "SIGINT" }), &json!({ "targetPgid": 9 }));
        assert_eq!(
            blocks[0]["text"],
            json!("delivered SIGINT to foreground process group 9")
        );
        let blocks = render_close(
            &json!({}),
            &json!({ "sessionId": "pty-1", "outcome": "closed" }),
        );
        assert_eq!(blocks[0]["text"], json!("closed terminal session pty-1"));
        let blocks = render_close(
            &json!({}),
            &json!({ "sessionId": "pty-1", "outcome": "already-closing" }),
        );
        assert_eq!(
            blocks[0]["text"],
            json!("terminal session pty-1 was already closing")
        );
        let blocks = render_list_value(
            &json!({}),
            &json!([{
                "sessionId": "pty-1",
                "type": "shell",
                "status": { "kind": "running" },
            }]),
        );
        assert!(
            blocks[0]["text"]
                .as_str()
                .expect("text")
                .starts_with("pty-1 [shell] running")
        );
    }

    /// The shared finalize bounds one complete text block for the six
    /// names and passes everything else through.
    #[test]
    fn finalize_bounds_only_the_family() {
        let call = json!({ "name": "terminal_read" });
        let result = json!({
            "content": [{ "type": "text", "text": "x".repeat(BAKED_MAX_RESULT_BYTES + 10) }],
        });
        let content = finalize_terminal_content(&call, &result).expect("the family bounds");
        let text = content[0]["text"].as_str().expect("text");
        assert_eq!(text.len(), BAKED_MAX_RESULT_BYTES);
        assert!(text.ends_with("\n[output truncated]"));
        assert_eq!(
            finalize_terminal_content(&json!({ "name": "bash" }), &result),
            None,
            "the plain shell tool rides its own cap family"
        );
        let multi = json!({
            "content": [
                { "type": "text", "text": "a" },
                { "type": "text", "text": "b" },
            ],
        });
        assert_eq!(finalize_terminal_content(&call, &multi), None);
    }
}
