// Rust guideline compliant 2026-10-31
//! The `job_output`, `job_list`, and `job_kill` tool bodies plus the
//! completion-notice machine's pure parts (dsh `tool-jobs`: index.ts 416
//! lines + render.ts 85 lines, call-for-call).
//!
//! The tools consume the `'jobs'` service through the [`Seams`] face; the
//! notification machine splits by the fr carrier: the settlement event
//! arrives on the kernel bus (`job/settled`), the on-event fast path
//! judges and enqueues, and a plugin-owned worker thread performs the
//! `agent`-service requires (the AGENT-LOOP spec's on-event fast-path
//! invariant: `on_event` must not require or block). The config
//! validations are pure functions over resolved values — the baked M1
//! composition rides the dsh defaults (`wakeup` delivery, 30s wait
//! default, 10min wait cap, no wake budget), so the refusal arms are
//! pinned by synthetic calls (the goal `resolveConfig` judgment).

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;

use serde_json::Value;
use serde_json::json;

use crate::service::js_number;
use crate::tools::BodyError;
use crate::tools::Seams;
/// How an uncollected completion reaches an idle owner (dsh
/// `CompletionDelivery`).
///
/// `wakeup` opens a turn for it; `quiet` leaves it pending until
/// something else wakes the owner. A busy owner is injected either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionDelivery {
    /// The default: a completion opens a one-turn wakeup on an idle owner.
    Wakeup,
    /// A completion waits for the owner's next wake by anything else.
    Quiet,
}

impl CompletionDelivery {
    /// The dsh config word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wakeup => "wakeup",
            Self::Quiet => "quiet",
        }
    }

    /// Parses the dsh config word (the config-pure arm; the baked
    /// composition never calls it — the registry key already admits only
    /// the two words).
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "wakeup" => Some(Self::Wakeup),
            "quiet" => Some(Self::Quiet),
            _ => None,
        }
    }
}

/// The baked M1 defaults (the dsh schemastery defaults; the registry row
/// carries the same values, the dylib bakes them — the approval judgment).
pub const BAKED_WAIT_TIMEOUT_MS: f64 = 30_000.0;
/// The baked hard wait cap (dsh `maxWaitTimeoutMs` default).
pub const BAKED_MAX_WAIT_TIMEOUT_MS: f64 = 600_000.0;
/// The baked completion delivery.
pub const BAKED_DELIVERY: CompletionDelivery = CompletionDelivery::Wakeup;
/// The baked wake budget (absent: every idle completion wakes its owner).
pub const BAKED_WAKE_BUDGET: Option<usize> = None;

/// Validates one resolved pair (dsh `apply`'s first check); the refusal
/// text is verbatim.
///
/// # Errors
///
/// The default exceeding the cap.
pub fn validate_wait_pair(wait_default: f64, wait_cap: f64) -> Result<(), String> {
    if wait_default > wait_cap {
        return Err(format!(
            "tool-jobs: waitTimeoutMs ({}) exceeds maxWaitTimeoutMs ({})",
            js_number(wait_default),
            js_number(wait_cap)
        ));
    }
    Ok(())
}

/// Validates one resolved budget (dsh `apply`'s second check; a budget
/// is a count of turns, so a fraction never names one and the absence
/// already means unbounded).
///
/// # Errors
///
/// A non-integer budget.
pub fn validate_wake_budget(budget: Option<f64>) -> Result<(), String> {
    let Some(budget) = budget else {
        return Ok(());
    };
    if !is_safe_integer(budget) {
        return Err(format!(
            "tool-jobs: maxConsecutiveWakes ({}) must be a whole number of turns",
            js_number(budget)
        ));
    }
    Ok(())
}

/// The producer byte cap as a `usize` (the fr-shell `ms_u64` judgment:
/// the caller validated a positive finite JS number; the truncation is
/// the JS-parity cut).
#[expect(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "the producer cap is a positive finite JS number; the byte-cap truncation is the dsh TextRetainer parity"
)]
#[expect(clippy::missing_const_for_fn, reason = "f64::max is not a const fn")]
#[must_use]
pub fn byte_cap(value: f64) -> usize {
    value.max(0.0) as usize
}

/// The JS `Number.isSafeInteger` parity (the goal decode judgment).
#[must_use]
pub fn is_safe_integer(value: f64) -> bool {
    value.is_finite() && value.fract() == 0.0 && value.abs() <= 9.007_199_254_740_991e15
}

/// The model-safe job projection (dsh `PublicJobSnapshot` +
/// `publicJob`).
///
/// Ownership, offsets, and limits are stripped; the key order mirrors
/// the dsh construction (`id, kind, label, status, detail?, startedAt,
/// finishedAt?`). `view` is the fr-shell wire projection (its
/// `snake_case` field words are the fr-internal carrier; the emitted
/// keys stay dsh-verbatim).
#[must_use]
pub fn public_job(view: &Value) -> Value {
    let detail = view
        .get("progress")
        .and_then(Value::as_str)
        .or_else(|| view.get("detail").and_then(Value::as_str));
    let mut object = serde_json::Map::new();
    object.insert(
        "id".to_owned(),
        view.get("id").cloned().unwrap_or(Value::Null),
    );
    object.insert(
        "kind".to_owned(),
        view.get("kind").cloned().unwrap_or(Value::Null),
    );
    object.insert(
        "label".to_owned(),
        view.get("label").cloned().unwrap_or(Value::Null),
    );
    object.insert(
        "status".to_owned(),
        view.get("status").cloned().unwrap_or(Value::Null),
    );
    if let Some(detail) = detail {
        object.insert("detail".to_owned(), json!(detail));
    }
    object.insert(
        "startedAt".to_owned(),
        view.get("started_at").cloned().unwrap_or(Value::Null),
    );
    if let Some(finished_at) = view.get("finished_at").filter(|value| !value.is_null()) {
        object.insert("finishedAt".to_owned(), finished_at.clone());
    }
    Value::Object(object)
}

/// The one-line qualifier the model reads beside a status: live progress
/// while the job runs, the terminal reason once settled (dsh
/// `statusLine`).
#[must_use]
pub fn status_line(job: &Value) -> String {
    let status = job.get("status").and_then(Value::as_str).unwrap_or("");
    job.get("detail").and_then(Value::as_str).map_or_else(
        || format!("[status: {status}]"),
        |detail| format!("[status: {status}, {detail}]"),
    )
}

/// Renders one consuming read for the model (dsh `renderModelDelta`).
///
/// Stdout and unlabeled chunks run in order, then every stderr chunk in
/// one `[stderr]` section, exactly as the shell tools render a
/// foreground result; `log` chunks never reach the model, and lost
/// bytes end the read with the shell tools' dropped-output notice.
#[must_use]
pub fn render_model_delta(read: &Value) -> String {
    let chunks = read
        .get("chunks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let lossy = read.get("lossy").and_then(Value::as_bool).unwrap_or(false);
    let spill_paths: Vec<&str> = read
        .pointer("/job/output/spill_paths")
        .and_then(Value::as_array)
        .map(|paths| paths.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let visible: Vec<&Value> = chunks
        .iter()
        .filter(|chunk| chunk.get("channel").and_then(Value::as_str) != Some("log"))
        .collect();
    let out: String = visible
        .iter()
        .filter(|chunk| chunk.get("channel").and_then(Value::as_str) != Some("stderr"))
        .filter_map(|chunk| chunk.get("text").and_then(Value::as_str))
        .collect();
    let err: String = visible
        .iter()
        .filter(|chunk| chunk.get("channel").and_then(Value::as_str) == Some("stderr"))
        .filter_map(|chunk| chunk.get("text").and_then(Value::as_str))
        .collect();
    let separator = if !out.is_empty() && !out.ends_with('\n') {
        "\n"
    } else {
        ""
    };
    let body = if err.is_empty() {
        out
    } else {
        format!("{out}{separator}[stderr]\n{err}")
    };
    let gap = visible
        .iter()
        .any(|chunk| chunk.get("gap_before").and_then(Value::as_bool) == Some(true));
    if !lossy && !gap {
        return body;
    }
    let notice = format!(
        "[some output was dropped from memory; full output: {}]",
        if spill_paths.is_empty() {
            "(unavailable)".to_owned()
        } else {
            spill_paths.join(", ")
        }
    );
    if !body.is_empty() && !body.ends_with('\n') {
        format!("{body}\n{notice}")
    } else {
        format!("{body}{notice}")
    }
}

/// Caps a UTF-8 string at a UTF-16 code-unit bound without splitting a
/// code point (the dsh `capString` judgment; the JS string bound is
/// UTF-16 units, and a astral character is two units).
#[must_use]
pub fn cap_utf16(text: &str, max_units: usize) -> String {
    if text.chars().map(char::len_utf16).sum::<usize>() <= max_units {
        return text.to_owned();
    }
    let mut units = 0_usize;
    let mut kept = String::new();
    for character in text.chars() {
        let width = character.len_utf16();
        if units + width > max_units {
            break;
        }
        units += width;
        kept.push(character);
    }
    kept
}

/// Bounds one notice summary (dsh `boundContextSummary`; 120 UTF-16
/// units, ellipsized with the ellipsis character).
#[must_use]
pub fn bound_context_summary(summary: &str) -> String {
    if summary.chars().map(char::len_utf16).sum::<usize>() <= 120 {
        return summary.to_owned();
    }
    format!("{}\u{2026}", cap_utf16(summary, 119))
}

/// The tail retain (dsh `TextRetainer` kind `'tail'`): the last
/// `max_bytes` bytes, cut forward to a UTF-8 boundary.
#[must_use]
pub fn retain_tail(text: &str, max_bytes: usize) -> String {
    let bytes = text.as_bytes();
    if bytes.len() <= max_bytes {
        return text.to_owned();
    }
    let start = utf8_boundary_forward(bytes, bytes.len() - max_bytes);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

/// The head retain (dsh `TextRetainer` kind `'head'`): the first
/// `max_bytes` bytes, cut back to a UTF-8 boundary.
#[must_use]
pub fn retain_head(text: &str, max_bytes: usize) -> String {
    let bytes = text.as_bytes();
    if bytes.len() <= max_bytes {
        return text.to_owned();
    }
    let end = utf8_boundary_backward(bytes, max_bytes);
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// The first character boundary at or after `from` (the dsh
/// `dropIncomplete` forward cut).
const fn utf8_boundary_forward(bytes: &[u8], from: usize) -> usize {
    let mut index = from;
    while index < bytes.len() && !is_char_boundary(bytes, index) {
        index += 1;
    }
    index
}

/// The first character boundary at or before `at` (the dsh
/// `dropIncomplete` backward cut).
const fn utf8_boundary_backward(bytes: &[u8], at: usize) -> usize {
    let mut index = at;
    while index > 0 && !is_char_boundary(bytes, index) {
        index -= 1;
    }
    index
}

/// Whether `index` starts a character in a valid UTF-8 byte string (the
/// lead-byte shape test: a continuation byte carries the `10xxxxxx`
/// prefix).
const fn is_char_boundary(bytes: &[u8], index: usize) -> bool {
    if index >= bytes.len() {
        return true;
    }
    bytes[index] & 0b1100_0000 != 0b1000_0000
}

/// Fits one rendered content with its suffix under a byte cap (dsh
/// `fitWithSuffix`): the complete text when it fits; otherwise the
/// omitted marker rides a tail-retained remainder.
#[must_use]
pub fn fit_with_suffix(
    content: &str,
    suffix: &str,
    max_bytes: Option<f64>,
    omitted: &str,
) -> String {
    let Some(max_bytes) = max_bytes else {
        return format!("{content}{suffix}");
    };
    let complete = format!("{content}{suffix}");
    let max_bytes = byte_cap(max_bytes);
    if complete.len() <= max_bytes {
        return complete;
    }
    let fixed = if content.starts_with(omitted.trim_start()) {
        suffix.to_owned()
    } else {
        format!("{omitted}{suffix}")
    };
    let fixed_bytes = fixed.len();
    if fixed_bytes >= max_bytes {
        return retain_tail(&fixed, max_bytes);
    }
    format!("{}{fixed}", retain_tail(content, max_bytes - fixed_bytes))
}

/// The one-line account of a settled job for the `notice` form's
/// collapsed row (dsh `completionSummary`), bounded like every notice
/// summary.
#[must_use]
pub fn completion_summary(job: &Value) -> String {
    let line = format!(
        "{} {} {}",
        job.get("kind").and_then(Value::as_str).unwrap_or(""),
        job.get("label").and_then(Value::as_str).unwrap_or(""),
        status_line(&public_job(job))
    );
    bound_context_summary(&line)
}

/// Builds the settlement notice text through the truncation ladder (dsh
/// `fitCompletionNotice`).
///
/// The rungs: the complete line, the truncated-fixed form, the compact
/// prefix, the listing pointer, then the retained head of `job_list`.
#[must_use]
pub fn fit_completion_notice(job: &Value) -> String {
    let id = job.get("id").and_then(Value::as_str).unwrap_or("");
    let prefix = format!("background job {id}");
    let detail = format!(
        " ({}: {}) finished {}",
        job.get("kind").and_then(Value::as_str).unwrap_or(""),
        job.get("label").and_then(Value::as_str).unwrap_or(""),
        status_line(&public_job(job))
    );
    let action = "\nDone; job_output.";
    let complete = format!("{prefix}{detail}. Read its output with job_output.");
    let max_bytes = job
        .get("output_limit_bytes")
        .and_then(Value::as_f64)
        .map(byte_cap);
    let Some(max_bytes) = max_bytes else {
        return complete;
    };
    if complete.len() <= max_bytes {
        return complete;
    }
    let omitted = "\n[notice truncated]";
    let fixed = format!("{prefix}{omitted}{action}");
    let fixed_bytes = fixed.len();
    if fixed_bytes <= max_bytes {
        if fixed_bytes == max_bytes {
            return fixed;
        }
        return format!(
            "{prefix}{}{omitted}{action}",
            retain_head(&detail, max_bytes - fixed_bytes)
        );
    }
    let compact = format!("{prefix}{action}");
    if compact.len() <= max_bytes {
        return compact;
    }
    // A partial UUID cannot identify the job. Point the model at the
    // listing instead.
    let recovery = "background job ended; use job_list for id.";
    if recovery.len() <= max_bytes {
        return recovery.to_owned();
    }
    retain_head("job_list", max_bytes)
}

/// One queued delivery: the owner session and the fully-built notice
/// message (the on-event fast path builds; the worker delivers).
#[derive(Debug)]
pub struct NoticeTask {
    /// The owning session id (dsh `event.job.owner`).
    pub session_id: String,
    /// The user-role notice message (dsh `createUserMessage`).
    pub message: Value,
}

/// The shared notice-machine state: the model-requested kill ledger,
/// the per-owner wake budgets, and the delivery queue's sender (dsh
/// `killedByModel` + `spentWakes` + the `owner.followup/inject` calls).
pub struct NoticeState {
    killed_by_model: Mutex<HashSet<String>>,
    spent_wakes: Mutex<HashMap<String, usize>>,
    /// The finalize listeners' CPS pairing stash (the enter leg parks
    /// the call and result; the exit leg consumes them — the spill
    /// judgment; the job listeners own a separate table so fabric token
    /// namespaces never collide).
    finalizes: Mutex<HashMap<u64, (Value, Value)>>,
    delivery: CompletionDelivery,
    wake_budget: Option<usize>,
    tasks: Mutex<mpsc::Sender<NoticeTask>>,
}

impl std::fmt::Debug for NoticeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NoticeState")
            .field("delivery", &self.delivery)
            .field("wake_budget", &self.wake_budget)
            .finish_non_exhaustive()
    }
}

impl NoticeState {
    /// Assembles the state over the delivery queue's sender and the
    /// resolved config (the baked M1 composition passes the dsh
    /// defaults).
    #[must_use]
    pub fn new(
        tasks: mpsc::Sender<NoticeTask>,
        delivery: CompletionDelivery,
        wake_budget: Option<usize>,
    ) -> Self {
        Self {
            killed_by_model: Mutex::new(HashSet::new()),
            spent_wakes: Mutex::new(HashMap::new()),
            finalizes: Mutex::new(HashMap::new()),
            delivery,
            wake_budget,
            tasks: Mutex::new(tasks),
        }
    }

    /// The delivery word (the wire pin arms read it).
    #[must_use]
    pub const fn delivery(&self) -> CompletionDelivery {
        self.delivery
    }

    /// The wake budget (the pin arms read it).
    #[must_use]
    pub const fn wake_budget(&self) -> Option<usize> {
        self.wake_budget
    }

    /// The finalize listeners' pairing stash.
    #[must_use]
    pub const fn finalizes(&self) -> &Mutex<HashMap<u64, (Value, Value)>> {
        &self.finalizes
    }

    /// Records a model-requested kill BEFORE the kill require lands (the
    /// cross-plugin race correction: the queued settled event may
    /// dispatch before the `job_kill` body resumes, and the dsh
    /// single-threaded add-after-kill cannot be replicated — the
    /// observable contract, no settlement notice for a model-requested
    /// kill, holds either way).
    pub fn record_model_kill(&self, id: &str) {
        self.killed_by_model
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.to_owned());
    }

    /// Withdraws a speculative kill record (the kill answered
    /// `already-finished` or failed; the dsh set never held the id).
    pub fn withdraw_model_kill(&self, id: &str) {
        self.killed_by_model
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);
    }

    /// Consumes one settled event's delivery judgment (dsh
    /// `killedByModel.delete(event.job.id) || event.awaited` — the
    /// delete reports whether the model itself requested this kill).
    #[must_use]
    pub fn consumes_model_kill(&self, id: &str) -> bool {
        self.killed_by_model
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id)
    }

    /// Resets one owner's wake budget after human input (dsh
    /// `agent/inbox/claimed` with `source.kind === 'user'`; a no-op
    /// while no budget is configured — the dsh listener would not even
    /// register).
    pub fn reset_wake_budget(&self, session_id: &str) {
        if self.wake_budget.is_none() {
            return;
        }
        self.spent_wakes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(session_id);
    }

    /// Enqueues one notice task; a closed queue contains (the machine's
    /// listener-throw shape: delivery is best-effort, never blocking).
    pub fn enqueue(&self, task: NoticeTask) {
        let _ignored = self
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .send(task);
    }

    /// Delivers one task per the resolved config (dsh `wakeup` &&
    /// `owner.status === 'idle'` → budget-checked followup; anything
    /// else injects). The `deliver` closure is the worker's require leg.
    pub fn deliver(
        &self,
        session_id: &str,
        message: &Value,
        deliver_followup: impl FnOnce(&str, &Value) -> bool,
        deliver_inject: impl FnOnce(&str, &Value) -> bool,
    ) {
        let idle = self.delivery == CompletionDelivery::Wakeup;
        if idle {
            let within_budget = self.wake_budget.is_none_or(|budget| {
                let mut wakes = self
                    .spent_wakes
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let spent = wakes.get(session_id).copied().unwrap_or(0);
                if spent < budget {
                    wakes.insert(session_id.to_owned(), spent + 1);
                    true
                } else {
                    false
                }
            });
            if within_budget && deliver_followup(session_id, message) {
                return;
            }
        }
        let _ignored = deliver_inject(session_id, message);
    }
}

/// Spawns the notice worker over one receiver (the plugin-owned
/// thread: an empty require stack, joined before teardown — the
/// fr-shell pump discipline).
///
/// Every require failure contains: a grant-less composition or a dead
/// session skips the delivery fail-closed (the 16a watcher judgment; no
/// logger seam exists in M1).
pub fn spawn_notice_worker(
    state: Arc<NoticeState>,
    tasks: mpsc::Receiver<NoticeTask>,
    link: crate::plugin_shell::HostLink,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while let Ok(task) = tasks.recv() {
            deliver_task(&state, &link, &task);
        }
    })
}

/// Delivers one task: the status query rides first (the dsh
/// `owner.status === 'idle'` read), then the wakeup arm followups and
/// the busy/quiet arms inject; every require failure skips (the
/// owner-absent arm — no reader is left).
fn deliver_task(state: &NoticeState, link: &crate::plugin_shell::HostLink, task: &NoticeTask) {
    let status = read_agent_status(link, &task.session_id);
    state.deliver(
        &task.session_id,
        &task.message,
        // The followup leg: only reached on the wakeup arm.
        |session, message| {
            status.as_deref() == Some("idle")
                && send_agent_message(
                    link,
                    fr_wire::services::agent::METHOD_FOLLOWUP,
                    session,
                    message,
                )
        },
        // The inject leg: busy owners, quiet delivery, and exhausted
        // budgets.
        |session, message| {
            send_agent_message(
                link,
                fr_wire::services::agent::METHOD_INJECT,
                session,
                message,
            )
        },
    );
}

/// Queries one session's loop status (the `'agent'` 6 selector); `None`
/// contains the transport failure or the dead-session shape.
fn read_agent_status(link: &crate::plugin_shell::HostLink, session_id: &str) -> Option<String> {
    let payload = serde_json::to_vec(&json!({ "sessionId": session_id })).ok()?;
    let reply = link
        .require(
            fr_wire::services::agent::SERVICE_NAME,
            fr_wire::services::agent::METHOD_STATUS,
            &payload,
        )
        .ok()?;
    let parsed: Value = serde_json::from_slice(&reply).ok()?;
    parsed
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Sends one message through the `'agent'` service (2 inject / 3
/// followup); `false` contains the failure.
fn send_agent_message(
    link: &crate::plugin_shell::HostLink,
    method: u32,
    session_id: &str,
    message: &Value,
) -> bool {
    let Ok(payload) = serde_json::to_vec(&json!({
        "sessionId": session_id,
        "message": message,
    })) else {
        return false;
    };
    link.require(fr_wire::services::agent::SERVICE_NAME, method, &payload)
        .is_ok()
}

/// Validates the non-empty constraint the schema row cannot express (dsh
/// `validateJobId`; the text is verbatim, `JSON.stringify` of a string
/// includes the quotes).
fn validate_job_id(value: &Value) -> Result<String, BodyError> {
    let Some(value) = value.as_str() else {
        return Err(BodyError::plain(format!(
            "invalid job_id: expected a non-empty string, got {value}"
        )));
    };
    if value.is_empty() {
        return Err(BodyError::plain(format!(
            "invalid job_id: expected a non-empty string, got {}",
            serde_json::to_string(value).unwrap_or_default()
        )));
    }
    Ok(value.to_owned())
}

/// The generic pending presentation shared by the three controls (dsh
/// `presentJobCall`).
fn present_job_call(title: &str, kind: &str, raw_input: Option<&str>) -> Value {
    let mut view = json!({ "card": "generic", "title": title, "kind": kind });
    if let Some(raw_input) = raw_input {
        view["rawInput"] = json!(raw_input);
    }
    view
}

/// The consuming read as the model sees it: the delta, then the result
/// once, then the status line (dsh `readBody`).
#[must_use]
pub fn read_body(read: &Value) -> (String, Value) {
    let delta = render_model_delta(read);
    let result = read.get("result").and_then(Value::as_str);
    let text = match result {
        None => delta,
        Some(result) => {
            let seam = if !delta.is_empty() && !delta.ends_with('\n') {
                "\n"
            } else {
                ""
            };
            format!("{delta}{seam}{result}")
        }
    };
    (text, public_job(read.get("job").unwrap_or(&Value::Null)))
}

/// The `job_output` tool body (dsh `execute`): the optional bounded wait
/// (the timed-out wait leaves the job running), then the consuming read.
///
/// # Errors
///
/// The id validation or the registry read family.
pub fn execute_output(
    seams: &dyn Seams,
    args: &Value,
    agent: Option<&str>,
) -> Result<Value, BodyError> {
    let id = validate_job_id(args.get("job_id").unwrap_or(&Value::Null))?;
    if args.get("wait").and_then(Value::as_bool) == Some(true) {
        let timeout = args
            .get("timeout_ms")
            .and_then(Value::as_f64)
            .unwrap_or(BAKED_WAIT_TIMEOUT_MS);
        let bounded = if timeout < BAKED_MAX_WAIT_TIMEOUT_MS {
            timeout
        } else {
            BAKED_MAX_WAIT_TIMEOUT_MS
        };
        seams
            .jobs_wait(&id, bounded, agent)
            .map_err(BodyError::plain)?;
    }
    let read = seams.jobs_read(&id, agent).map_err(BodyError::plain)?;
    let (text, job) = read_body(&read);
    Ok(json!({ "text": text, "job": job }))
}

/// The `job_list` tool body (dsh `execute`): every visible job in
/// registration order (running and finished).
///
/// # Errors
///
/// The registry list family.
pub fn execute_list(seams: &dyn Seams, agent: Option<&str>) -> Result<Value, BodyError> {
    let jobs = seams.jobs_list(agent).map_err(BodyError::plain)?;
    let listed = jobs
        .as_array()
        .map(|jobs| jobs.iter().map(public_job).collect::<Vec<Value>>())
        .unwrap_or_default();
    Ok(Value::Array(listed))
}

/// The `job_kill` tool body (dsh `execute`): the kill request (the
/// model's own kill is its delivery, so the speculative ledger entry
/// lands before the require), then the projection.
///
/// # Errors
///
/// The id validation or the registry kill family.
pub fn execute_kill(
    seams: &dyn Seams,
    notices: &NoticeState,
    args: &Value,
    agent: Option<&str>,
) -> Result<Value, BodyError> {
    let id = validate_job_id(args.get("job_id").unwrap_or(&Value::Null))?;
    notices.record_model_kill(&id);
    let reason = args.get("reason").and_then(Value::as_str);
    let result = match seams.jobs_kill(&id, agent, reason) {
        Ok(word) => word,
        Err(error) => {
            notices.withdraw_model_kill(&id);
            return Err(BodyError::plain(error));
        }
    };
    if result != "requested" {
        notices.withdraw_model_kill(&id);
    }
    let view = seams.jobs_get(&id, agent).map_err(|error| {
        notices.withdraw_model_kill(&id);
        BodyError::plain(error)
    })?;
    let job = public_job(&view);
    let outcome = if result == "already-finished" {
        "already-finished"
    } else {
        "cancellation-requested"
    };
    Ok(json!({ "outcome": outcome, "job": job }))
}

/// The `job_output` render (dsh `output.render`): the body (or its
/// empty-read word), the newline seam, and the status line.
#[must_use]
pub fn render_output(_args: &Value, value: &Value) -> Vec<Value> {
    let text = value.get("text").and_then(Value::as_str).unwrap_or("");
    let body = if text.is_empty() {
        "(no new output)"
    } else {
        text
    };
    let separator = if body.ends_with('\n') { "" } else { "\n" };
    let job = value.get("job").cloned().unwrap_or(Value::Null);
    vec![json!({
        "type": "text",
        "text": format!("{body}{separator}{}", status_line(&job)),
    })]
}

/// The `job_list` render (dsh `output.render`): the empty word or the
/// `id [kind] status — label` rows joined with newlines.
#[must_use]
pub fn render_list(_args: &Value, value: &Value) -> Vec<Value> {
    let jobs = value.as_array().cloned().unwrap_or_default();
    let text = if jobs.is_empty() {
        "(no background jobs)".to_owned()
    } else {
        jobs.iter()
            .map(|job| {
                format!(
                    "{} [{}] {} — {}",
                    job.get("id").and_then(Value::as_str).unwrap_or(""),
                    job.get("kind").and_then(Value::as_str).unwrap_or(""),
                    job.get("status").and_then(Value::as_str).unwrap_or(""),
                    job.get("label").and_then(Value::as_str).unwrap_or(""),
                )
            })
            .collect::<Vec<String>>()
            .join("\n")
    };
    vec![json!({ "type": "text", "text": text })]
}

/// The `job_kill` render (dsh `output.render`): the two outcome words.
#[must_use]
pub fn render_kill(_args: &Value, value: &Value) -> Vec<Value> {
    let outcome = value
        .get("outcome")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let job = value.get("job").cloned().unwrap_or(Value::Null);
    let text = if outcome == "already-finished" {
        format!(
            "job {} had already finished {}",
            job.get("id").and_then(Value::as_str).unwrap_or(""),
            status_line(&job)
        )
    } else {
        format!(
            "requested cancellation of job {}",
            job.get("id").and_then(Value::as_str).unwrap_or("")
        )
    };
    vec![json!({ "type": "text", "text": text })]
}

/// The `job_output` pending-call presentation (dsh `presentCall`).
#[must_use]
pub fn present_call_output(args: &Value) -> Value {
    let job_id = args.get("job_id").and_then(Value::as_str).unwrap_or("");
    present_job_call(
        &format!("Read output from background job {job_id}"),
        "read",
        Some(job_id),
    )
}

/// The `job_list` pending-call presentation (dsh `presentCall`).
#[must_use]
pub fn present_call_list(_args: &Value) -> Value {
    present_job_call("List background jobs", "read", None)
}

/// The `job_kill` pending-call presentation (dsh `presentCall`).
#[must_use]
pub fn present_call_kill(args: &Value) -> Value {
    let job_id = args.get("job_id").and_then(Value::as_str).unwrap_or("");
    present_job_call(
        &format!("Kill background job {job_id}"),
        "execute",
        Some(job_id),
    )
}

/// Builds the settlement notice message (dsh `createUserMessage`).
///
/// One text block over the ladder-fitted line, the `tool-jobs` source
/// with the `notice` form and the bounded summary, and a fresh UUID v4
/// identity (the dsh `createMessage` minting).
#[must_use]
pub fn notice_message(job: &Value) -> Value {
    json!({
        "role": "user",
        "id": uuid::Uuid::new_v4().to_string(),
        "content": [{ "type": "text", "text": fit_completion_notice(job) }],
        "source": {
            "kind": "tool-jobs",
            "form": "notice",
            "summary": completion_summary(job),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted seams double: the three job faces record and answer
    /// (the `fs_tools` `ScriptSeams` judgment).
    struct JobSeams {
        wait_calls: Mutex<Vec<(String, f64, Option<String>)>>,
        reads: Mutex<Vec<String>>,
        kills: Mutex<Vec<(String, Option<String>)>>,
        job_view: Value,
    }

    impl JobSeams {
        fn new(job_view: Value) -> Self {
            Self {
                wait_calls: Mutex::new(Vec::new()),
                reads: Mutex::new(Vec::new()),
                kills: Mutex::new(Vec::new()),
                job_view,
            }
        }
    }

    impl crate::tools::PolicySeam for JobSeams {
        fn resolve_policy(&self, _session: Option<&str>) -> Result<Option<Value>, String> {
            Ok(None)
        }
    }

    impl crate::tools::ApprovalSeam for JobSeams {
        fn request(&self, _ask: &fr_sandbox::escalation::EscalationAsk) -> Result<String, String> {
            Err("not reached".to_owned())
        }
    }

    impl Seams for JobSeams {
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
            id: &str,
            timeout_ms: f64,
            caller: Option<&str>,
        ) -> Result<Value, String> {
            self.wait_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((id.to_owned(), timeout_ms, caller.map(str::to_owned)));
            Ok(self.job_view.clone())
        }

        fn jobs_read(&self, id: &str, _caller: Option<&str>) -> Result<Value, String> {
            self.reads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(id.to_owned());
            Ok(json!({
                "chunks": [],
                "lossy": false,
                "job": self.job_view,
            }))
        }

        fn jobs_list(&self, _caller: Option<&str>) -> Result<Value, String> {
            Ok(json!([self.job_view]))
        }

        fn jobs_get(&self, _id: &str, _caller: Option<&str>) -> Result<Value, String> {
            Ok(self.job_view.clone())
        }

        fn jobs_kill(
            &self,
            id: &str,
            _caller: Option<&str>,
            reason: Option<&str>,
        ) -> Result<String, String> {
            self.kills
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((id.to_owned(), reason.map(str::to_owned)));
            Ok("requested".to_owned())
        }

        fn jobs_remove(&self, _id: &str, _caller: Option<&str>) -> Result<(), String> {
            Ok(())
        }

        fn working_directory_ensure(&self, _agent: &str) -> Result<String, String> {
            Err("not reached".to_owned())
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

    fn job_view() -> Value {
        json!({
            "id": "bash-abc123",
            "kind": "bash",
            "label": "long build",
            "owner": "s1",
            "output_limit_bytes": serde_json::Value::Null,
            "status": "running",
            "started_at": 1.0,
            "output": { "total": 0.0, "earliest": 0.0 },
        })
    }

    fn settled_view() -> Value {
        let mut view = job_view();
        view["status"] = json!("completed");
        view["detail"] = json!("exit 0");
        view["finished_at"] = json!(2.0);
        view
    }

    /// The projection keeps the dsh key order, prefers the live
    /// progress over the terminal detail, and strips the fr-internal
    /// fields.
    #[test]
    fn public_job_projects_the_dsh_key_order_and_detail() {
        let projected = public_job(&settled_view());
        let keys: Vec<String> = projected
            .as_object()
            .expect("object")
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            keys,
            [
                "id",
                "kind",
                "label",
                "status",
                "detail",
                "startedAt",
                "finishedAt"
            ]
        );
        assert_eq!(projected["detail"], "exit 0");
        assert!(projected.get("owner").is_none());
        assert!(projected.get("output_limit_bytes").is_none());
        assert!(projected.get("output").is_none());

        let running = public_job(&job_view());
        assert_eq!(
            running["detail"],
            serde_json::Value::Null,
            "no progress and no detail stay absent"
        );
    }

    /// The consuming read joins the delta, the newline seam, and the
    /// result; the status line rides the render.
    #[test]
    fn read_body_seams_delta_and_result() {
        let read = json!({
            "chunks": [
                { "at": 0.0, "text": "out one\n" },
                { "at": 7.0, "text": "err one", "channel": "stderr" },
            ],
            "lossy": false,
            "result": "done",
            "job": settled_view(),
        });
        let (text, job) = read_body(&read);
        assert_eq!(text, "out one\n[stderr]\nerr one\ndone");
        assert_eq!(job["status"], "completed");
    }

    /// The truncated ladder walks every rung, byte-for-byte with the
    /// dsh arms (the recovery rung only opens past the compact form —
    /// a long job id pushes the compact line over the cap).
    #[test]
    fn completion_notice_ladder_walks_every_rung() {
        let complete = json!({
            "id": "j1",
            "kind": "bash",
            "label": "build",
            "status": "completed",
            "startedAt": 1.0,
        });
        // No cap: the complete line.
        assert_eq!(
            fit_completion_notice(&complete),
            "background job j1 (bash: build) finished [status: completed]. Read its output with job_output."
        );
        // A cap tight enough for the compact form only.
        let mut capped = complete.clone();
        capped["output_limit_bytes"] = json!(40.0);
        let notice = fit_completion_notice(&capped);
        assert_eq!(notice, "background job j1\nDone; job_output.");
        // A cap below the compact form retains the listing word's head
        // (the short id keeps the recovery line out of reach).
        capped["output_limit_bytes"] = json!(20.0);
        assert_eq!(fit_completion_notice(&capped), "job_list");

        // The recovery rung: a long id pushes the compact form past the
        // cap while the recovery line fits it exactly.
        let mut long = complete;
        long["id"] = json!("bash-0123456789abcdef");
        long["output_limit_bytes"] = json!(42.0);
        assert_eq!(
            fit_completion_notice(&long),
            "background job ended; use job_list for id."
        );
    }

    /// The fixed rung rides the head-retained detail between the prefix
    /// and the omitted marker (the dsh arm order: complete, fixed,
    /// compact).
    #[test]
    fn completion_notice_fixed_rung_retains_the_detail_head() {
        let detail = format!(
            " (bash: build) finished [status: completed, {}]",
            "x".repeat(120)
        );
        let prefix = "background job j1";
        let fixed = format!("{prefix}\n[notice truncated]\nDone; job_output.");
        let retained = 80 - fixed.len();
        let fitted = fit_completion_notice(&json!({
            "id": "j1",
            "kind": "bash",
            "label": "build",
            "status": "completed",
            "detail": "x".repeat(120),
            "startedAt": 1.0,
            "output_limit_bytes": json!(80.0),
        }));
        let expected = format!(
            "{prefix}{}\n[notice truncated]\nDone; job_output.",
            &detail[..retained]
        );
        assert_eq!(fitted, expected);
    }

    /// The suffix fit keeps the complete text under the cap and falls
    /// back to the omitted marker's tail retention.
    #[test]
    fn fit_with_suffix_bounds_the_rendered_content() {
        assert_eq!(
            fit_with_suffix("abc", "def", Some(6.0), "\n[out]"),
            "abcdef"
        );
        // The marker-plus-suffix overflow retains the tail (the dsh
        // retainTail keeps the last bytes).
        assert_eq!(
            fit_with_suffix("abcdef", "gh", Some(6.0), "\n[result truncated]"),
            "ted]gh"
        );
        assert_eq!(fit_with_suffix("abc", "def", None, "\n[no cap]"), "abcdef");
    }

    /// The summary bounds at 120 UTF-16 units with the ellipsis; the
    /// retain helpers cut at UTF-8 boundaries.
    #[test]
    fn summary_and_retainers_bound_like_the_dsh_helpers() {
        let long = "x".repeat(130);
        let bounded = bound_context_summary(&long);
        assert_eq!(bounded.chars().count(), 120, "the 120-unit cap");
        assert!(bounded.ends_with('\u{2026}'));
        assert_eq!(bound_context_summary("short"), "short");
        assert_eq!(
            retain_head("héllo", 3),
            "hé",
            "three bytes land on the boundary"
        );
        assert_eq!(retain_tail("héllo", 3), "llo");
        assert_eq!(retain_head("héllo", 30), "héllo");
    }

    /// The two config refusals pin their verbatim texts over synthetic
    /// resolved values (the baked composition never trips them).
    #[test]
    fn config_validations_pin_the_refusal_texts() {
        assert_eq!(
            validate_wait_pair(600_001.0, 600_000.0),
            Err("tool-jobs: waitTimeoutMs (600001) exceeds maxWaitTimeoutMs (600000)".to_owned())
        );
        validate_wait_pair(30_000.0, 600_000.0).unwrap();
        assert_eq!(
            validate_wake_budget(Some(2.5)),
            Err("tool-jobs: maxConsecutiveWakes (2.5) must be a whole number of turns".to_owned())
        );
        validate_wake_budget(None).unwrap();
        validate_wake_budget(Some(4.0)).unwrap();
    }

    /// The output body runs the bounded wait (the supplied timeout
    /// clamps to the cap), then reads; the list body projects every
    /// visible job.
    #[test]
    fn output_and_list_bodies_drive_the_registry_faces() {
        let seams = JobSeams::new(job_view());
        let value = execute_output(
            &seams,
            &json!({ "job_id": "bash-abc123", "wait": true, "timeout_ms": 900_000.0 }),
            Some("s1"),
        )
        .expect("output");
        let waits = seams.wait_calls.lock().expect("lock");
        assert_eq!(
            waits.as_slice(),
            [("bash-abc123".to_owned(), 600_000.0, Some("s1".to_owned()))]
        );
        assert_eq!(value["text"], "");
        assert_eq!(value["job"]["status"], "running");
        drop(waits);

        let listed = execute_list(&seams, Some("s1")).expect("list");
        let rows = listed.as_array().expect("array");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], "bash-abc123");
        assert!(rows[0].get("owner").is_none());
    }

    /// The kill body records the speculative ledger entry before the
    /// require, keeps it on `requested`, and withdraws it on
    /// `already-finished` (the settlement-notice skip contract).
    #[test]
    fn kill_body_walks_the_model_kill_ledger() {
        let seams = JobSeams::new(settled_view());
        let (sender, _receiver) = mpsc::channel();
        let notices = NoticeState::new(sender, BAKED_DELIVERY, BAKED_WAKE_BUDGET);

        let value = execute_kill(
            &seams,
            &notices,
            &json!({ "job_id": "bash-abc123", "reason": "stale" }),
            Some("s1"),
        )
        .expect("kill");
        assert_eq!(value["outcome"], "cancellation-requested");
        assert!(
            notices.consumes_model_kill("bash-abc123"),
            "requested keeps"
        );
        assert!(!notices.consumes_model_kill("bash-abc123"), "one-shot");

        let finished = execute_kill(
            &seams,
            &notices,
            &json!({ "job_id": "bash-abc123" }),
            Some("s1"),
        )
        .expect("kill");
        assert_eq!(finished["outcome"], "cancellation-requested");

        let refused = execute_output(&seams, &json!({ "job_id": "" }), Some("s1"));
        assert_eq!(
            refused.expect_err("empty id refuses").message,
            "invalid job_id: expected a non-empty string, got \"\""
        );
    }

    /// The notice machine's delivery: wakeup on idle followups, busy
    /// injects, and the budget arm degrades to injection; quiet always
    /// injects.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the three delivery arms each need their own scripted closures; splitting them would hide the budget's spend-exhaust-reset arc"
    )]
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the test asserts once at the end of each arm; the guard lives exactly across the assertion"
    )]
    fn delivery_walks_the_wakeup_inject_and_budget_arms() {
        let (sender, _receiver) = mpsc::channel();
        let message = json!({ "id": "m1" });
        let state = NoticeState::new(sender, CompletionDelivery::Wakeup, None);
        let calls = Mutex::new(Vec::new());
        state.deliver(
            "s1",
            &message,
            |session, _message| {
                calls
                    .lock()
                    .expect("lock")
                    .push(("followup", session.to_owned()));
                true
            },
            |_session, _message| false,
        );
        let calls = calls.lock().expect("lock");
        assert_eq!(calls.len(), 1, "the idle wakeup arm followups: {calls:?}");
        drop(calls);

        let budgeted = NoticeState::new(mpsc::channel().0, CompletionDelivery::Wakeup, Some(1));
        let calls = Mutex::new(Vec::new());
        // The budget (1): the first completion follows up and spends
        // it; the second degrades to injection.
        budgeted.deliver(
            "s1",
            &message,
            |session, _message| {
                calls
                    .lock()
                    .expect("lock")
                    .push(("followup", session.to_owned()));
                true
            },
            |session, _message| {
                calls
                    .lock()
                    .expect("lock")
                    .push(("inject", session.to_owned()));
                true
            },
        );
        budgeted.deliver(
            "s1",
            &message,
            |session, _message| {
                calls
                    .lock()
                    .expect("lock")
                    .push(("followup", session.to_owned()));
                true
            },
            |session, _message| {
                calls
                    .lock()
                    .expect("lock")
                    .push(("inject", session.to_owned()));
                true
            },
        );
        // Human input resets the budget: the wakeup arm returns.
        budgeted.reset_wake_budget("s1");
        budgeted.deliver(
            "s1",
            &message,
            |session, _message| {
                calls
                    .lock()
                    .expect("lock")
                    .push(("followup", session.to_owned()));
                true
            },
            |session, _message| {
                calls
                    .lock()
                    .expect("lock")
                    .push(("inject", session.to_owned()));
                true
            },
        );
        let calls = calls.lock().expect("lock");
        assert_eq!(
            calls.as_slice(),
            [
                ("followup", "s1".to_owned()),
                ("inject", "s1".to_owned()),
                ("followup", "s1".to_owned()),
            ],
            "the first wake spends, the second injects, the reset restores: {calls:?}"
        );

        let quiet = NoticeState::new(mpsc::channel().0, CompletionDelivery::Quiet, None);
        let calls = Mutex::new(Vec::new());
        quiet.deliver(
            "s1",
            &message,
            |_session, _message| {
                calls
                    .lock()
                    .expect("lock")
                    .push(("followup", "s1".to_owned()));
                true
            },
            |_session, _message| {
                calls
                    .lock()
                    .expect("lock")
                    .push(("inject", "s1".to_owned()));
                true
            },
        );
        assert_eq!(
            calls.lock().expect("lock").as_slice(),
            [("inject", "s1".to_owned())],
            "quiet delivery never follows up"
        );
    }

    /// The notice message carries the tool-jobs source, the notice form,
    /// the bounded summary, and one text block over the ladder-fitted
    /// line (the dsh createUserMessage shape).
    #[test]
    fn notice_message_carries_the_source_and_form() {
        let message = notice_message(&settled_view());
        assert_eq!(message["role"], "user");
        assert_eq!(message["source"]["kind"], "tool-jobs");
        assert_eq!(message["source"]["form"], "notice");
        assert_eq!(
            message["source"]["summary"],
            "bash long build [status: completed, exit 0]"
        );
        assert_eq!(
            message["content"][0]["text"],
            "background job bash-abc123 (bash: long build) finished [status: completed, exit 0]. Read its output with job_output."
        );
    }

    /// The delta render sections stderr and appends the dropped-output
    /// notice over the spill paths.
    #[test]
    fn delta_renders_stderr_and_the_loss_notice() {
        let read = json!({
            "chunks": [
                { "at": 0.0, "text": "one" },
                { "at": 3.0, "text": "logged", "channel": "log" },
                { "at": 9.0, "text": "two", "channel": "stderr" },
            ],
            "lossy": false,
            "job": { "output": { "total": 12.0, "earliest": 0.0 } },
        });
        assert_eq!(render_model_delta(&read), "one\n[stderr]\ntwo");
        let lossy = json!({
            "chunks": [{ "at": 9.0, "text": "two", "gap_before": true }],
            "lossy": false,
            "job": { "output": { "total": 12.0, "earliest": 0.0, "spill_paths": ["/tmp/a"] } },
        });
        assert_eq!(
            render_model_delta(&lossy),
            "two\n[some output was dropped from memory; full output: /tmp/a]"
        );
    }

    /// The three presentations ride their dsh shapes (the raw input on
    /// the two named-call controls, none on the listing).
    #[test]
    fn presentations_pin_the_three_titles() {
        assert_eq!(
            present_call_output(&json!({ "job_id": "j9" })),
            json!({
                "card": "generic",
                "title": "Read output from background job j9",
                "kind": "read",
                "rawInput": "j9",
            })
        );
        assert_eq!(
            present_call_list(&json!({})),
            json!({ "card": "generic", "title": "List background jobs", "kind": "read" })
        );
        assert_eq!(
            present_call_kill(&json!({ "job_id": "j9" })),
            json!({
                "card": "generic",
                "title": "Kill background job j9",
                "kind": "execute",
                "rawInput": "j9",
            })
        );
    }

    /// The two renders pin their dsh words: the empty read, the listing
    /// rows, and the two kill outcomes.
    #[test]
    fn renders_pin_the_words() {
        assert_eq!(
            render_output(&json!({}), &json!({ "text": "", "job": settled_view() }))[0]["text"],
            "(no new output)\n[status: completed, exit 0]"
        );
        assert_eq!(
            render_output(&json!({}), &json!({ "text": "tail\n", "job": job_view() }))[0]["text"],
            "tail\n[status: running]"
        );
        assert_eq!(
            render_list(&json!({}), &json!([]))[0]["text"],
            "(no background jobs)"
        );
        assert_eq!(
            render_list(
                &json!({}),
                &json!([{
                    "id": "j1", "kind": "bash", "label": "build", "status": "running",
                }])
            )[0]["text"],
            "j1 [bash] running — build"
        );
        assert_eq!(
            render_kill(
                &json!({}),
                &json!({ "outcome": "requested", "job": job_view() })
            )[0]["text"],
            "requested cancellation of job bash-abc123"
        );
        assert_eq!(
            render_kill(
                &json!({}),
                &json!({ "outcome": "already-finished", "job": settled_view() })
            )[0]["text"],
            "job bash-abc123 had already finished [status: completed, exit 0]"
        );
    }
}
