// Rust guideline compliant 2026-10-31
//! One persistent PTY session (the dsh `terminal-bash` `session.ts`
//! `LocalPtySession` port).
//!
//! Bounded output retention, the readiness three-evidence state machine,
//! and terminal-protocol replies through the `alacritty_terminal` emulator
//! (Q16e-2A).
//!
//! The dsh async event loop port: every interleaving the session pins rides
//! one `SessionCore` mutex plus condvars, with the pump (master reader),
//! reply-writer, poller, exit-watcher, and per-send begin threads standing
//! in for the event-loop turns. Observable ordering contracts (inspect
//! before write, settle arms in the dsh order, the close fence, and the
//! cancel-then-interrupt path) keep their dsh shapes.

use std::collections::VecDeque;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

/// The terminal dimensions (a local [`Dimensions`] impl: the crate's own
/// size type sits behind its test-helpers module).
struct SessionSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for SessionSize {
    fn columns(&self) -> usize {
        self.cols
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn total_lines(&self) -> usize {
        self.rows
    }
}

use crate::terminal::{TerminalForeground, TerminalHandle};
use crate::terminal_buffer::BoundedTextBuffer;
use crate::terminal_sanitize::{CONTROLLED_PROMPT, TerminalSanitizer};

/// Why one interactive send returned control to its caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReason {
    /// The shell regained its prompt or a foreground child reads stdin.
    StdinRead,
    /// The output-silence bound elapsed.
    InferredIdle,
    /// The absolute send deadline elapsed.
    Timeout,
    /// The top-level process exited.
    SessionExit,
}

/// The top-level pty process status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    /// The process is alive.
    Running,
    /// The process exited (exit code null under a signal).
    Exited {
        /// The exit code, `None` under a signal.
        exit_code: Option<i64>,
        /// The POSIX signal name, `None` for clean exits.
        signal: Option<String>,
    },
}

/// The settled result for one foreground send (the dsh
/// `TerminalSendResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendResult {
    /// The bounded rendered terminal delta remaining at settlement.
    pub viewport: String,
    /// Why the wait returned.
    pub wait_reason: WaitReason,
    /// The top-level session status observed at settlement.
    pub session_status: SessionStatus,
    /// Whether output was dropped from the operation or retained scrollback.
    pub truncated: bool,
}

/// One live send operation (the dsh `TerminalSendOperation`).
pub struct SendOperation {
    /// The bounded operation output (the viewport source).
    output: Mutex<BoundedTextBuffer>,
    /// The settle slot plus wakeups.
    settled: Mutex<Option<Result<SendResult, String>>>,
    settled_ready: Condvar,
    /// Whether cancellation was requested.
    cancel_requested: AtomicBool,
    /// The spawn-time instant the readiness arms measure from.
    started_at: Instant,
    /// The pre-write foreground observation.
    initial_foreground: Mutex<Option<(i64, bool)>>,
    /// Whether the initial foreground had left its stdin wait.
    initial_left_wait: AtomicBool,
    /// The input write completion fence (the dsh `activeWrite`).
    input_write_done: Mutex<bool>,
    input_write_ready: Condvar,
    /// Whether the input write failed.
    input_write_failed: AtomicBool,
    /// Whether the SIGINT interrupt was already delivered (one interrupt
    /// per send, the dsh abort-listener shape).
    interrupt_delivered: AtomicBool,
}

impl std::fmt::Debug for SendOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendOperation").finish_non_exhaustive()
    }
}

impl SendOperation {
    fn new(max_read_bytes: usize, started_at: Instant) -> Arc<Self> {
        Arc::new(Self {
            output: Mutex::new(BoundedTextBuffer::new(max_read_bytes, None)),
            settled: Mutex::new(None),
            settled_ready: Condvar::new(),
            cancel_requested: AtomicBool::new(false),
            started_at,
            initial_foreground: Mutex::new(None),
            initial_left_wait: AtomicBool::new(true),
            input_write_done: Mutex::new(false),
            input_write_ready: Condvar::new(),
            input_write_failed: AtomicBool::new(false),
            interrupt_delivered: AtomicBool::new(false),
        })
    }

    /// Awaits the settled result.
    ///
    /// # Errors
    ///
    /// Returns the failure text when the send failed before settlement.
    ///
    /// # Panics
    ///
    /// Never in practice: the settle lock's poisoning arm is unreachable
    /// under the plugin recovery contract.
    pub fn done(&self) -> Result<SendResult, String> {
        let mut slot = self.settled.lock().expect("send settle lock");
        loop {
            if let Some(result) = slot.as_ref() {
                return result.clone();
            }
            let (next, _timeout) = self
                .settled_ready
                .wait_timeout(slot, Duration::from_millis(200))
                .expect("send settle lock");
            slot = next;
        }
    }

    /// Consumes the delta produced since the prior call (the dsh
    /// `readOutput`).
    ///
    /// # Panics
    ///
    /// Never in practice: the output lock's poisoning arm is unreachable
    /// under the plugin recovery contract.
    #[must_use]
    pub fn read_output(&self) -> (String, bool) {
        let mut output = self.output.lock().expect("send output lock");
        output.consume()
    }

    fn append(&self, text: &str) {
        if self.settled_now() {
            return;
        }
        let mut output = self.output.lock().expect("send output lock");
        output.append(text);
    }

    fn settle(
        &self,
        wait_reason: WaitReason,
        session_status: SessionStatus,
        inherited_truncation: bool,
    ) {
        let first = {
            let mut slot = self.settled.lock().expect("send settle lock");
            let first = slot.is_none();
            if first {
                let (viewport, truncated) = self.viewport_locked();
                *slot = Some(Ok(SendResult {
                    viewport,
                    wait_reason,
                    session_status,
                    truncated: truncated || inherited_truncation,
                }));
            }
            first
        };
        if first {
            self.settled_ready.notify_all();
        }
    }

    fn viewport_locked(&self) -> (String, bool) {
        let output = self.output.lock().expect("send output lock");
        (output.snapshot_text(), output.truncated())
    }

    fn fail(&self, error: &str) {
        let first = {
            let mut slot = self.settled.lock().expect("send settle lock");
            let first = slot.is_none();
            if first {
                *slot = Some(Err(error.to_owned()));
            }
            first
        };
        if first {
            self.settled_ready.notify_all();
        }
    }

    /// Requests `SIGINT`; returns false after the operation settled.
    ///
    /// # Panics
    ///
    /// Never in practice: the settle lock's poisoning arm is unreachable
    /// under the plugin recovery contract.
    #[must_use]
    pub fn cancel(&self) -> bool {
        !self.settled.lock().expect("send settle lock").is_some()
            && !self.cancel_requested.swap(true, Ordering::AcqRel)
    }

    fn settled_now(&self) -> bool {
        self.settled.lock().expect("send settle lock").is_some()
    }

    fn set_initial_foreground(&self, foreground: Option<(i64, bool)>) {
        let Some((pgid, waiting)) = foreground else {
            return;
        };
        {
            let mut slot = self
                .initial_foreground
                .lock()
                .expect("send foreground lock");
            *slot = Some((pgid, waiting));
        }
        self.initial_left_wait.store(!waiting, Ordering::Release);
    }

    /// The dsh `acceptsStdinWait`: the same group may still expose the
    /// pre-write wait, so a departure before the exact threshold still
    /// makes a later return to that wait post-write evidence.
    fn accepts_stdin_wait(&self, pgid: i64, waiting: bool) -> bool {
        let same_group = {
            let slot = self
                .initial_foreground
                .lock()
                .expect("send foreground lock");
            slot.is_some_and(|(initial_pgid, _)| initial_pgid == pgid)
        };
        if !same_group {
            return waiting;
        }
        if !waiting {
            self.initial_left_wait.store(true, Ordering::Release);
        }
        waiting && self.initial_left_wait.load(Ordering::Acquire)
    }

    fn mark_input_write(&self, failed: bool) {
        {
            let mut done = self.input_write_done.lock().expect("send write lock");
            *done = true;
        }
        if failed {
            self.input_write_failed.store(true, Ordering::Release);
        }
        self.input_write_ready.notify_all();
    }

    /// Whether the input write completed (the dsh `activeWrite` fence).
    fn input_write_settled(&self) -> bool {
        *self.input_write_done.lock().expect("send write lock")
    }

    /// Whether cancellation owns the send slot.
    #[must_use]
    pub fn cancel_requested(&self) -> bool {
        self.cancel_requested.load(Ordering::Acquire)
    }

    fn mark_interrupted(&self) {
        self.interrupt_delivered.store(true, Ordering::Release);
    }

    #[must_use]
    fn interrupt_delivered(&self) -> bool {
        self.interrupt_delivered.load(Ordering::Acquire)
    }
}

/// The emulator reply feed: the listener side pushes generated replies, the
/// reply-writer thread drains them to the master writer.
#[derive(Debug, Default)]
struct ReplyFeed {
    /// The queued reply texts not yet handed to the writer.
    queue: Mutex<VecDeque<String>>,
    /// Wakeups for the writer drain.
    queued_ready: Condvar,
    /// The enqueue generation (the dsh `emulatorWrites` identity).
    enqueued_gen: AtomicU64,
    /// The write-completed generation (the dsh `responseWrites` identity).
    written_gen: AtomicU64,
    /// Reply writes handed to the writer but not completed.
    in_flight: AtomicU64,
}

impl ReplyFeed {
    fn pending(&self) -> bool {
        self.enqueued_gen.load(Ordering::Acquire) != self.written_gen.load(Ordering::Acquire)
            || self.in_flight.load(Ordering::Acquire) > 0
    }
}

/// The alacritty event listener: `PtyWrite` events queue as replies; every
/// other event kind drops.
#[derive(Debug)]
struct EmuListener {
    feed: Arc<ReplyFeed>,
}

impl EventListener for EmuListener {
    fn send_event(&self, event: Event) {
        if let Event::PtyWrite(text) = event {
            {
                let mut queue = self.feed.queue.lock().expect("reply queue lock");
                queue.push_back(text);
            }
            self.feed.enqueued_gen.fetch_add(1, Ordering::AcqRel);
            self.feed.queued_ready.notify_all();
        }
    }
}

/// The validated session configuration (the dsh `terminal-bash`
/// `ResolvedConfig`).
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// The interactive shell dialect.
    pub shell_dialect: ShellDialect,
    /// The interactive shell executable.
    pub shell_path: String,
    /// The shell arguments.
    pub shell_args: Vec<String>,
    /// Terminal rows.
    pub rows: u16,
    /// Terminal columns.
    pub cols: u16,
    /// Maximum retained logical lines.
    pub scrollback_lines: usize,
    /// Maximum retained UTF-8 bytes.
    pub scrollback_max_bytes: usize,
    /// Maximum bytes returned by one read or settled viewport.
    pub max_read_bytes: usize,
    /// Readiness polling interval in milliseconds.
    pub poll_interval_ms: u64,
    /// Delay before the exact syscall probes in milliseconds.
    pub exact_probe_after_ms: u64,
    /// Silence duration that yields `inferred_idle` in milliseconds.
    pub idle_silence_ms: u64,
    /// Extra wait beyond the silence bound once a prompt marker was seen.
    pub handoff_grace_ms: u64,
    /// Extra wait for a pending prompt tail in milliseconds.
    pub prompt_tail_grace_ms: u64,
    /// Absolute bound for one send in milliseconds.
    pub timeout_ms: u64,
    /// Grace before teardown escalates in milliseconds.
    pub dispose_grace_ms: u64,
}

/// One supported interactive shell dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellDialect {
    /// The bash dialect (`PS1` + `PROMPT_COMMAND` marker protocol).
    Bash,
    /// The pwsh dialect (startup bootstrap prompt injection).
    Pwsh,
}

/// The prompt-evidence pair (the dsh promptSeen/promptTextSeen).
struct PromptEvidence {
    /// The OSC 133;D; marker arrived.
    marker_seen: bool,
    /// The controlled prompt text itself arrived.
    text_seen: bool,
}

/// The end-of-stream flags (the dsh output-ended and exit observations).
struct LifecycleFlags {
    /// The pump folded its held tail at end of stream.
    output_ended: bool,
    /// The exit watcher published the exit facts.
    exited_observed: bool,
}

/// The mutex-guarded session state (one dsh event-loop frame).
struct SessionCore {
    active: Option<Arc<SendOperation>>,
    interrupting: Option<Arc<SendOperation>>,
    prompt: PromptEvidence,
    prompt_tail: String,
    last_output_at: Instant,
    shell_pgid: Option<i64>,
    initializing: bool,
    status_value: SessionStatus,
    closing: bool,
    transport_failure: Option<String>,
    deadline_at: Option<Instant>,
    poll_due_at: Instant,
    lifecycle: LifecycleFlags,
    motd: String,
}

/// One persistent PTY session (the dsh `LocalPtySession`).
pub struct LocalPtySession {
    /// The provider terminal handle.
    handle: Arc<TerminalHandle>,
    /// The resolved configuration.
    config: SessionConfig,
    /// The guarded session state.
    core: Mutex<SessionCore>,
    /// Wakeups for the poller and the close path.
    core_ready: Condvar,
    /// The retained scrollback (pump writer, readers).
    scrollback: Mutex<BoundedTextBuffer>,
    /// The emulator reply feed (listener writer, reply-writer reader).
    feed: Arc<ReplyFeed>,
    /// The sanitizer state (pump thread exclusive under its lock).
    sanitizer: Mutex<TerminalSanitizer>,
    /// The scrollback truncation fence (set by the pump, read by settles).
    scrollback_truncated: AtomicBool,
}

impl std::fmt::Debug for LocalPtySession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalPtySession").finish_non_exhaustive()
    }
}

impl LocalPtySession {
    /// Wraps one spawned terminal handle in a running session and starts
    /// the pump, reply-writer, poller, and exit-watcher threads.
    ///
    /// # Panics
    ///
    /// Never in practice: thread builders only fail on OS resource
    /// exhaustion, which the plugin contract treats as unreachable.
    #[must_use]
    pub fn new(handle: TerminalHandle, config: SessionConfig) -> Arc<Self> {
        let handle = Arc::new(handle);
        let feed = Arc::new(ReplyFeed::default());
        let scrollback =
            BoundedTextBuffer::new(config.scrollback_max_bytes, Some(config.scrollback_lines));
        let sanitizer = TerminalSanitizer::new(config.max_read_bytes);
        let session = Arc::new(Self {
            handle: Arc::clone(&handle),
            config,
            core: Mutex::new(SessionCore {
                active: None,
                interrupting: None,
                prompt: PromptEvidence {
                    marker_seen: false,
                    text_seen: false,
                },
                prompt_tail: String::new(),
                last_output_at: Instant::now(),
                shell_pgid: None,
                initializing: false,
                status_value: SessionStatus::Running,
                closing: false,
                transport_failure: None,
                deadline_at: None,
                poll_due_at: Instant::now(),
                lifecycle: LifecycleFlags {
                    output_ended: false,
                    exited_observed: false,
                },
                motd: String::new(),
            }),
            core_ready: Condvar::new(),
            scrollback: Mutex::new(scrollback),
            sanitizer: Mutex::new(sanitizer),
            feed: Arc::clone(&feed),
            scrollback_truncated: AtomicBool::new(false),
        });
        spawn_threads(&session, feed);
        session
    }

    /// The session's current top-level status.
    ///
    /// # Panics
    ///
    /// Never in practice: the core lock's poisoning arm is unreachable
    /// under the plugin recovery contract.
    #[must_use]
    pub fn status(&self) -> SessionStatus {
        self.core
            .lock()
            .expect("session core lock")
            .status_value
            .clone()
    }

    /// The captured startup output (set by [`Self::initialize`]).
    ///
    /// # Panics
    ///
    /// Never in practice: the core lock's poisoning arm is unreachable
    /// under the plugin recovery contract.
    #[must_use]
    pub fn motd(&self) -> String {
        self.core.lock().expect("session core lock").motd.clone()
    }

    /// Whether no output has been retained yet (the dsh
    /// `scrollback.isEmpty` startup gate).
    fn scrollback_is_empty(&self) -> bool {
        self.scrollback
            .lock()
            .expect("session scrollback lock")
            .is_empty()
    }

    /// Awaits the settled result of the active send, if any.
    ///
    /// # Panics
    ///
    /// Never in practice: the core lock's poisoning arm is unreachable
    /// under the plugin recovery contract.
    #[must_use]
    pub fn active(&self) -> Option<Arc<SendOperation>> {
        self.core.lock().expect("session core lock").active.clone()
    }

    /// One bounded scrollback page (the dsh `read`).
    ///
    /// # Errors
    ///
    /// Returns the dsh validation texts for the offset and count bounds.
    ///
    /// # Panics
    ///
    /// Never in practice: the scrollback lock's poisoning arm is
    /// unreachable under the plugin recovery contract.
    pub fn read(&self, offset: Option<usize>, count: Option<usize>) -> Result<ReadPage, String> {
        let (retained, truncated) = {
            let scrollback = self.scrollback.lock().expect("session scrollback lock");
            (scrollback.snapshot_text(), scrollback.truncated())
        };
        let lines: Vec<&str> = if retained.is_empty() {
            Vec::new()
        } else {
            retained.split('\n').collect()
        };
        let total_lines = if retained.is_empty() { 0 } else { lines.len() };
        let offset = offset.unwrap_or(0);
        let count = count.unwrap_or(500);
        if count == 0 {
            return Err("PTY read count must be a positive safe integer".to_owned());
        }
        if offset >= total_lines {
            return Ok(ReadPage {
                text: String::new(),
                total_lines,
                line_begin: offset,
                line_end: offset,
                truncated,
            });
        }
        let end = total_lines - offset;
        let start = end.saturating_sub(count);
        let requested = lines[start..end].join("\n");
        let bounded = utf8_tail(&requested, self.config.max_read_bytes);
        let returned = if bounded.0.is_empty() {
            0
        } else {
            bounded.0.split('\n').count()
        };
        Ok(ReadPage {
            text: bounded.0,
            total_lines,
            line_begin: offset,
            line_end: offset + returned,
            truncated: truncated || bounded.1,
        })
    }

    /// Delivers one allowed signal to the verified foreground group.
    ///
    /// # Errors
    ///
    /// Returns the closing text and the handle's signal errors.
    ///
    /// # Panics
    ///
    /// Never in practice: the core lock's poisoning arm is unreachable
    /// under the plugin recovery contract.
    pub fn signal(&self, signal: &str) -> Result<i64, String> {
        if self.core.lock().expect("session core lock").closing {
            return Err("PTY session is closing".to_owned());
        }
        self.handle.signal_foreground(signal)
    }

    /// Starts one exclusive interactive send (the dsh `startSend`).
    ///
    /// # Errors
    ///
    /// Returns the dsh closing/exited/active texts.
    ///
    /// # Panics
    ///
    /// Never in practice: the core lock's poisoning arm is unreachable
    /// under the plugin recovery contract.
    pub fn start_send(
        self: &Arc<Self>,
        text: &str,
        submit: bool,
    ) -> Result<Arc<SendOperation>, String> {
        let text = text.to_owned();
        let op = {
            let mut core = self.core.lock().expect("session core lock");
            if core.closing {
                return Err("PTY session is closing".to_owned());
            }
            if core.status_value != SessionStatus::Running {
                return Err("PTY session has exited".to_owned());
            }
            if core.active.is_some() {
                let draining = if core
                    .active
                    .as_ref()
                    .is_some_and(|op| !op.input_write_settled())
                {
                    " or draining provider write"
                } else if core.interrupting.is_some() {
                    " or draining foreground interrupt"
                } else {
                    ""
                };
                return Err(format!("PTY session already has an active send{draining}"));
            }
            let op = SendOperation::new(self.config.max_read_bytes, Instant::now());
            core.active = Some(Arc::clone(&op));
            core.prompt.marker_seen = false;
            core.prompt.text_seen = false;
            core.prompt_tail.clear();
            core.last_output_at = Instant::now();
            core.deadline_at = Some(Instant::now() + Duration::from_millis(self.config.timeout_ms));
            core.poll_due_at = Instant::now();
            op
        };
        let session = Arc::clone(self);
        let begin_op = Arc::clone(&op);
        std::thread::Builder::new()
            .name("fr-shell-pty-begin".to_owned())
            .spawn(move || begin_send(&session, &begin_op, &text, submit))
            .expect("spawning the begin thread never fails under the plugin contract");
        self.core_ready.notify_all();
        Ok(op)
    }

    /// The dsh `initialize`: one empty-text send whose viewport becomes the
    /// MOTD (exit and readiness timeout reject the startup).
    ///
    /// # Errors
    ///
    /// Returns the dsh startup failure texts.
    ///
    /// # Panics
    ///
    /// Never in practice: the core lock's poisoning arm is unreachable
    /// under the plugin recovery contract.
    pub fn initialize(self: &Arc<Self>) -> Result<(), String> {
        {
            let mut core = self.core.lock().expect("session core lock");
            core.initializing = true;
        }
        let op = self.start_send("", false)?;
        let result = op.done();
        {
            let mut core = self.core.lock().expect("session core lock");
            core.initializing = false;
        }
        match result {
            Ok(send) => {
                if send.wait_reason == WaitReason::SessionExit {
                    return Err("PTY shell exited during startup".to_owned());
                }
                if send.wait_reason == WaitReason::Timeout {
                    return Err(
                        "PTY shell did not reach readiness before startup timeout".to_owned()
                    );
                }
                {
                    let mut core = self.core.lock().expect("session core lock");
                    core.motd = send.viewport;
                }
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// Idempotent close with the closing fence (the dsh `close`).
    ///
    /// # Panics
    ///
    /// Never in practice: thread builders only fail on OS resource
    /// exhaustion, which the plugin contract treats as unreachable.
    pub fn close(self: &Arc<Self>) -> Arc<CloseSlot> {
        let first = {
            let mut core = self.core.lock().expect("session core lock");
            let first = !core.closing;
            if first {
                core.closing = true;
            }
            first
        };
        let slot = Arc::new(CloseSlot::default());
        if !first {
            slot.result.lock().expect("close lock").replace(Ok(()));
            slot.ready.notify_all();
            return slot;
        }
        self.core_ready.notify_all();
        let session = Arc::clone(self);
        let close_slot = Arc::clone(&slot);
        std::thread::Builder::new()
            .name("fr-shell-pty-close".to_owned())
            .spawn(move || run_close(&session, &close_slot))
            .expect("spawning the close thread never fails under the plugin contract");
        slot
    }
}

/// One bounded scrollback page (the dsh `TerminalReadResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadPage {
    /// Retained text in chronological order.
    pub text: String,
    /// Number of lines currently retained.
    pub total_lines: usize,
    /// Inclusive newest-relative offset of the first returned line.
    pub line_begin: usize,
    /// Exclusive newest-relative offset after the returned page.
    pub line_end: usize,
    /// Whether older retained output or the requested result exceeded a bound.
    pub truncated: bool,
}

/// The settled close fence (the dsh closing promise).
#[derive(Debug, Default)]
pub struct CloseSlot {
    /// The close result once settled.
    pub result: Mutex<Option<Result<(), String>>>,
    /// Wakeups for the close result.
    pub ready: Condvar,
}

/// Awaits the close fence (blocking in-crate form).
///
/// # Panics
///
/// Never in practice: the close lock's poisoning arm is unreachable under
/// the plugin recovery contract.
///
/// # Errors
///
/// Returns the close failure text (the dsh `closeOnce` rejection).
pub fn wait_close(slot: &CloseSlot) -> Result<(), String> {
    let mut guard = slot.result.lock().expect("close lock");
    loop {
        if let Some(result) = guard.as_ref() {
            return result.clone();
        }
        let (next, _timeout) = slot
            .ready
            .wait_timeout(guard, Duration::from_millis(200))
            .expect("close lock");
        guard = next;
    }
}

/// The UTF-8 tail helper (the dsh `utf8Tail`).
#[must_use]
pub fn utf8_tail(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_owned(), false);
    }
    let mut bytes = 0;
    let mut start = text.len();
    for ch in text.chars().rev() {
        let width = ch.len_utf8();
        if bytes + width > max_bytes {
            break;
        }
        bytes += width;
        start -= width;
    }
    (text[start..].to_owned(), true)
}

/// Builds the session threads (pump, reply-writer, poller, exit-watcher).
///
/// # Panics
///
/// Never in practice: thread builders only fail on OS resource exhaustion,
/// which the plugin contract treats as unreachable.
fn spawn_threads(session: &Arc<LocalPtySession>, feed: Arc<ReplyFeed>) {
    // The reply writer drains the feed into the master writer.
    {
        let session = Arc::clone(session);
        let feed = Arc::clone(&feed);
        std::thread::Builder::new()
            .name("fr-shell-pty-reply".to_owned())
            .spawn(move || {
                loop {
                    let reply = feed.queue.lock().expect("reply queue lock").pop_front();
                    if let Some(reply) = reply {
                        feed.in_flight.fetch_add(1, Ordering::AcqRel);
                        let write = session.handle.write(&reply);
                        feed.written_gen.fetch_add(1, Ordering::AcqRel);
                        feed.in_flight.fetch_sub(1, Ordering::AcqRel);
                        if write.is_err() {
                            session.on_transport_failure("provider write failed");
                            return;
                        }
                    } else {
                        if is_closing(&session) {
                            return;
                        }
                        let queue = feed.queue.lock().expect("reply queue lock");
                        if queue.is_empty() {
                            let _ = feed
                                .queued_ready
                                .wait_timeout(queue, Duration::from_millis(50));
                        }
                    }
                }
            })
            .expect("reply writer thread");
    }
    // The pump: the master reader loop with the emulator and sanitizer.
    {
        let session = Arc::clone(session);
        std::thread::Builder::new()
            .name("fr-shell-pty-pump".to_owned())
            .spawn(move || {
                let Some(mut reader) = session.handle.take_reader() else {
                    return;
                };
                run_pump(&session, reader.as_mut(), &feed);
            })
            .expect("pump thread");
    }
    // The poller: the readiness loop while an active send exists.
    {
        let session = Arc::clone(session);
        std::thread::Builder::new()
            .name("fr-shell-pty-poll".to_owned())
            .spawn(move || run_poller(&session))
            .expect("poller thread");
    }
    // The exit watcher: publishes the exit facts and settles session_exit.
    {
        let session = Arc::clone(session);
        std::thread::Builder::new()
            .name("fr-shell-pty-exit".to_owned())
            .spawn(move || run_exit_watcher(&session))
            .expect("exit watcher thread");
    }
}

/// Whether the session is closing.
fn is_closing(session: &LocalPtySession) -> bool {
    session.core.lock().expect("session core lock").closing
}
/// The pump body: incremental UTF-8 decode, the emulator feed, the
/// sanitizer, the retained scrollback, and the prompt evidence chain.
fn run_pump(session: &LocalPtySession, reader: &mut dyn Read, feed: &Arc<ReplyFeed>) {
    let rows = usize::from(session.config.rows);
    let cols = usize::from(session.config.cols);
    let mut term = Term::new(
        TermConfig {
            scrolling_history: 0,
            ..TermConfig::default()
        },
        &SessionSize { cols, rows },
        EmuListener {
            feed: Arc::clone(feed),
        },
    );
    let mut processor = Processor::<StdSyncHandler>::new();
    let mut pending: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let read = match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => count,
        };
        pending.extend_from_slice(&chunk[..read]);
        let mut decoded = String::new();
        loop {
            match std::str::from_utf8(&pending) {
                Ok(text) => {
                    decoded.push_str(text);
                    pending.clear();
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    if valid > 0 {
                        decoded.push_str(&String::from_utf8_lossy(&pending[..valid]));
                        pending.drain(..valid);
                    }
                    match error.error_len() {
                        Some(bad) => {
                            decoded.push('\u{FFFD}');
                            pending.drain(..bad);
                        }
                        None => break,
                    }
                }
            }
        }
        if decoded.is_empty() {
            continue;
        }
        processor.advance(&mut term, decoded.as_bytes());
        session.absorb_sanitized(&decoded);
    }
    let flushed = session.flush_sanitizer();
    if !flushed.is_empty() {
        session.absorb_sanitized(&flushed);
    }
    session.mark_output_ended();
}

impl LocalPtySession {
    /// Pump-exclusive: sanitizes one decoded text and routes the chunk into
    /// the retained scrollback, the active operation, and the prompt
    /// evidence chain.
    fn absorb_sanitized(&self, decoded: &str) {
        let chunk = self
            .sanitizer
            .lock()
            .expect("session sanitizer lock")
            .push(decoded);
        let text = chunk.text().to_owned();
        let prompt = chunk.prompt();
        let tail = chunk.prompt_tail().map(str::to_owned);
        if !text.is_empty() {
            {
                let mut scrollback = self.scrollback.lock().expect("session scrollback lock");
                scrollback.append(&text);
                if scrollback.truncated() {
                    self.scrollback_truncated.store(true, Ordering::Release);
                }
            }
            let active = self.core.lock().expect("session core lock").active.clone();
            if let Some(op) = active {
                op.append(&text);
            }
            {
                let mut core = self.core.lock().expect("session core lock");
                core.last_output_at = Instant::now();
                core.prompt.marker_seen |= prompt;
                if let Some(tail) = tail {
                    core.prompt_tail = tail;
                }
                core.prompt.text_seen |= text.contains(CONTROLLED_PROMPT);
            }
            self.core_ready.notify_all();
        }
    }

    /// Pump-exclusive: folds the sanitizer's held tail at end of stream.
    fn flush_sanitizer(&self) -> String {
        self.sanitizer
            .lock()
            .expect("session sanitizer lock")
            .flush()
    }

    /// Pump-exclusive: publishes end-of-output and wakes the poller.
    fn mark_output_ended(&self) {
        {
            let mut core = self.core.lock().expect("session core lock");
            core.lifecycle.output_ended = true;
        }
        self.core_ready.notify_all();
    }

    /// Fails the transport: every outstanding send fails, the session
    /// reports a null exit, and the shell is torn down.
    fn on_transport_failure(&self, error: &str) {
        {
            let mut core = self.core.lock().expect("session core lock");
            if core.transport_failure.is_none() {
                core.transport_failure = Some(error.to_owned());
            }
            core.status_value = SessionStatus::Exited {
                exit_code: None,
                signal: None,
            };
            if let Some(op) = core.active.take() {
                op.fail(error);
            }
            if let Some(op) = core.interrupting.take() {
                op.fail(error);
            }
        }
        self.core_ready.notify_all();
        self.handle.terminate();
    }
}

/// The per-send begin body: the pre-write foreground observation, the
/// active-slot revalidation, the input write, and the write fence.
fn begin_send(session: &LocalPtySession, op: &Arc<SendOperation>, text: &str, submit: bool) {
    let mut input = String::with_capacity(text.len() + 1);
    input.push_str(text);
    if submit {
        input.push('\r');
    }
    let foreground = session.handle.inspect_foreground().ok().flatten();
    op.set_initial_foreground(foreground.map(|fg| (fg.process_group_id, fg.input_waiting)));
    if op.cancel_requested() {
        op.mark_input_write(false);
        return;
    }
    let write = if input.is_empty() {
        Ok(())
    } else {
        session.handle.write(&input)
    };
    if write.is_ok() {
        op.mark_input_write(false);
    } else {
        op.mark_input_write(true);
        session.on_transport_failure("provider write failed");
    }
    session.core_ready.notify_all();
}

/// Settles one send operation and releases the send slots (a settle keeps
/// the slot only while its write drain or a protocol reply is still
/// outstanding; the deadline arm owns the interrupt-slot retention).
fn settle_and_release(
    session: &LocalPtySession,
    core: &mut SessionCore,
    op: &Arc<SendOperation>,
    reason: WaitReason,
) {
    let retain = !op.input_write_settled() || session.feed.pending();
    op.settle(reason, core.status_value.clone(), false);
    if !retain {
        core.active = None;
        core.interrupting = None;
    }
}

/// One poller pass; returns true once the poller's work is done.
fn poller_tick(session: &LocalPtySession) -> bool {
    let poll_interval = Duration::from_millis(session.config.poll_interval_ms);
    let mut core = session.core.lock().expect("session core lock");

    // Reap a retained deadline settle once its drains completed (the dsh
    // `releaseSettledActive` conditions, observed on the poll cadence).
    if core
        .active
        .as_ref()
        .is_some_and(|op| op.settled_now() && op.input_write_settled())
        && core.interrupting.is_none()
        && !session.feed.pending()
    {
        core.active = None;
    }

    // Transport failure: fail every outstanding operation and stop.
    if let Some(error) = core.transport_failure.clone() {
        if let Some(op) = core.active.take() {
            op.fail(&error);
        }
        if let Some(op) = core.interrupting.take() {
            op.fail(&error);
        }
        return true;
    }

    // Exit observed: settle every outstanding operation with session_exit.
    if core.lifecycle.exited_observed {
        if let Some(op) = core.active.take() {
            op.settle(WaitReason::SessionExit, core.status_value.clone(), false);
        }
        if let Some(op) = core.interrupting.take() {
            op.settle(WaitReason::SessionExit, core.status_value.clone(), false);
        }
        return true;
    }

    // The interrupt fence: while a SIGINT delivery is in flight no
    // readiness arm runs (the dsh poll revalidation).
    if core.interrupting.is_some() {
        return false;
    }

    // The cancel path: one SIGINT to the foreground group, delivered
    // synchronously; the fence is transient and the send resumes as the
    // active operation (the dsh `interruptOnce` shape).
    if let Some(op) = core.active.clone()
        && op.cancel_requested()
        && op.input_write_settled()
        && !op.interrupt_delivered()
    {
        core.active = None;
        core.interrupting = Some(Arc::clone(&op));
        drop(core);
        let interrupted = session.handle.signal_foreground("SIGINT");
        let mut core = session.core.lock().expect("session core lock");
        core.interrupting = None;
        if interrupted.is_ok() {
            op.mark_interrupted();
            if core.active.is_none() && !core.closing {
                core.active = Some(op);
            }
        } else {
            drop(core);
            session.on_transport_failure("foreground interrupt failed");
            return true;
        }
        session.core_ready.notify_all();
        return false;
    }

    // The emulator reply pipeline must drain before foreground evidence is
    // trusted (the dsh `protocolWorkPending` gate).
    if session.feed.pending() {
        core.poll_due_at = Instant::now() + poll_interval;
        return false;
    }

    let Some(op) = core.active.clone() else {
        core.poll_due_at = Instant::now() + poll_interval;
        return core.closing;
    };

    if let Some(done) = deadline_arm(session, &mut core, &op) {
        session.core_ready.notify_all();
        return done;
    }

    // Foreground evidence: the inspect rides the handle; the arms revalidate
    // under the still-held core lock (the dsh serialized event-loop shape).
    if let Ok(Some(foreground)) = session.handle.inspect_foreground() {
        // The dsh adoption: the shell group is learned from the foreground
        // once a prompt marker has arrived.
        if core.prompt.marker_seen && core.shell_pgid.is_none() {
            core.shell_pgid = Some(foreground.process_group_id);
        }
        if let Some(done) = prompt_arm(session, &mut core, &op, foreground.process_group_id) {
            session.core_ready.notify_all();
            return done;
        }
        if let Some(done) = probe_arm(session, &mut core, &op, foreground) {
            session.core_ready.notify_all();
            return done;
        }
        if let Some(done) = idle_arm(session, &mut core, &op) {
            session.core_ready.notify_all();
            return done;
        }
    }

    core.poll_due_at = Instant::now() + poll_interval;
    core.closing && core.active.is_none()
}

/// The deadline arm; `Some(done)` when it settled the send.
fn deadline_arm(
    session: &LocalPtySession,
    core: &mut SessionCore,
    op: &Arc<SendOperation>,
) -> Option<bool> {
    let deadline = core.deadline_at?;
    if Instant::now() < deadline {
        return None;
    }
    let retain = !op.input_write_settled() || session.feed.pending();
    op.settle(WaitReason::Timeout, core.status_value.clone(), false);
    if !retain {
        core.active = None;
    }
    core.poll_due_at = Instant::now() + Duration::from_millis(session.config.poll_interval_ms);
    Some(core.closing && core.active.is_none())
}

/// The prompt-evidence arm; `Some(done)` when it settled the send.
fn prompt_arm(
    session: &LocalPtySession,
    core: &mut SessionCore,
    op: &Arc<SendOperation>,
    foreground_pgid: i64,
) -> Option<bool> {
    if !op.input_write_settled()
        || !core.prompt.marker_seen
        || !core.prompt.text_seen
        || core.last_output_at.elapsed() < Duration::from_millis(session.config.poll_interval_ms)
        || Some(foreground_pgid) != core.shell_pgid
    {
        return None;
    }
    settle_and_release(session, core, op, WaitReason::StdinRead);
    core.poll_due_at = Instant::now() + Duration::from_millis(session.config.poll_interval_ms);
    Some(core.closing && core.active.is_none())
}

/// The exact-probe arm; `Some(done)` when it settled the send.
fn probe_arm(
    session: &LocalPtySession,
    core: &mut SessionCore,
    op: &Arc<SendOperation>,
    foreground: TerminalForeground,
) -> Option<bool> {
    let startup_has_output = !core.initializing || !session.scrollback_is_empty();
    if !startup_has_output
        || !op.input_write_settled()
        || op.started_at.elapsed() < Duration::from_millis(session.config.exact_probe_after_ms)
        || !op.accepts_stdin_wait(foreground.process_group_id, foreground.input_waiting)
    {
        return None;
    }
    settle_and_release(session, core, op, WaitReason::StdinRead);
    core.poll_due_at = Instant::now() + Duration::from_millis(session.config.poll_interval_ms);
    Some(core.closing && core.active.is_none())
}

/// The silence-bound arm; `Some(done)` when it settled the send.
fn idle_arm(
    session: &LocalPtySession,
    core: &mut SessionCore,
    op: &Arc<SendOperation>,
) -> Option<bool> {
    let startup_has_output = !core.initializing || !session.scrollback_is_empty();
    let tail_pending = core.prompt.marker_seen
        && !core.prompt.text_seen
        && CONTROLLED_PROMPT.starts_with(core.prompt_tail.as_str());
    let required = Duration::from_millis(session.config.idle_silence_ms)
        + if core.prompt.marker_seen {
            Duration::from_millis(session.config.handoff_grace_ms)
        } else {
            Duration::ZERO
        }
        + if tail_pending {
            Duration::from_millis(session.config.prompt_tail_grace_ms)
        } else {
            Duration::ZERO
        };
    if !startup_has_output || core.last_output_at.elapsed() < required {
        return None;
    }
    settle_and_release(session, core, op, WaitReason::InferredIdle);
    core.poll_due_at = Instant::now() + Duration::from_millis(session.config.poll_interval_ms);
    Some(core.closing && core.active.is_none())
}
/// The poller body: sleeps until the next due instant, then runs one pass.
///
/// # Panics
///
/// Never in practice: the core lock's poisoning arm is unreachable under
/// the plugin recovery contract.
fn run_poller(session: &LocalPtySession) {
    loop {
        {
            let core = session.core.lock().expect("session core lock");
            let mut due = core.poll_due_at;
            if let Some(deadline) = core.deadline_at
                && deadline < due
            {
                due = deadline;
            }
            let now = Instant::now();
            if due > now {
                let _ = session.core_ready.wait_timeout(core, due - now);
            }
        }
        if poller_tick(session) {
            return;
        }
    }
}

/// The exit watcher body: publishes the exit facts once the output stream
/// ends, then settles any outstanding send with `session_exit`.
///
/// # Panics
///
/// Never in practice: the core lock's poisoning arm is unreachable under
/// the plugin recovery contract.
fn run_exit_watcher(session: &LocalPtySession) {
    let outcome = session.handle.done();
    let stream_deadline = Instant::now() + Duration::from_millis(session.config.dispose_grace_ms);
    {
        let core = session.core.lock().expect("session core lock");
        let _ = session.core_ready.wait_timeout_while(
            core,
            stream_deadline.duration_since(Instant::now()),
            |current: &mut SessionCore| !current.lifecycle.output_ended,
        );
    }
    let status = SessionStatus::Exited {
        exit_code: outcome.exit_code,
        signal: outcome.signal,
    };
    {
        let mut core = session.core.lock().expect("session core lock");
        core.status_value = status.clone();
        core.lifecycle.exited_observed = true;
        if let Some(op) = core.active.take() {
            op.settle(WaitReason::SessionExit, status.clone(), false);
        }
        if let Some(op) = core.interrupting.take() {
            op.settle(WaitReason::SessionExit, status, false);
        }
    }
    session.core_ready.notify_all();
}

/// The close body: the termination ladder, the output-end fence, and the
/// close result.
///
/// # Panics
///
/// Never in practice: the core lock's poisoning arm is unreachable under
/// the plugin recovery contract.
fn run_close(session: &LocalPtySession, slot: &CloseSlot) {
    session.handle.terminate();
    let result = session.handle.wait_termination();
    let stream_deadline = Instant::now() + Duration::from_millis(session.config.dispose_grace_ms);
    {
        let core = session.core.lock().expect("session core lock");
        let _ = session.core_ready.wait_timeout_while(
            core,
            stream_deadline.duration_since(Instant::now()),
            |current: &mut SessionCore| {
                !current.lifecycle.output_ended && !current.lifecycle.exited_observed
            },
        );
    }
    {
        let mut guard = slot.result.lock().expect("close lock");
        *guard = Some(result);
    }
    slot.ready.notify_all();
}

#[cfg(test)]
mod tests {
    use super::{
        CloseSlot, LocalPtySession, SessionConfig, SessionStatus, ShellDialect, WaitReason,
        wait_close,
    };
    use crate::inspect::ProcFs;
    use crate::terminal::{TerminalSpawnSpec, spawn_terminal};
    use std::sync::Arc;

    fn spawn_spec(argv: &[&str]) -> TerminalSpawnSpec {
        TerminalSpawnSpec {
            argv: argv.iter().map(|word| (*word).to_owned()).collect(),
            cwd: None,
            env: vec![
                ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
                ("TERM".to_owned(), "dumb".to_owned()),
            ],
            rows: 24,
            cols: 80,
            grace_ms: 1000.0,
        }
    }

    fn test_config() -> SessionConfig {
        SessionConfig {
            shell_dialect: ShellDialect::Bash,
            shell_path: "/bin/bash".to_owned(),
            shell_args: vec!["--noprofile".to_owned(), "--norc".to_owned()],
            rows: 24,
            cols: 80,
            scrollback_lines: 500,
            scrollback_max_bytes: 64 * 1024,
            max_read_bytes: 16 * 1024,
            poll_interval_ms: 10,
            exact_probe_after_ms: 60,
            idle_silence_ms: 250,
            handoff_grace_ms: 40,
            prompt_tail_grace_ms: 0,
            timeout_ms: 4000,
            dispose_grace_ms: 250,
        }
    }

    fn make_session(argv: &[&str]) -> Arc<LocalPtySession> {
        make_session_with(test_config(), argv)
    }

    fn make_session_with(config: SessionConfig, argv: &[&str]) -> Arc<LocalPtySession> {
        let handle = spawn_terminal(&spawn_spec(argv), Arc::new(ProcFs))
            .expect("spawning the test shell never fails");
        LocalPtySession::new(handle, config)
    }

    fn close_quietly(session: &Arc<LocalPtySession>) {
        let slot: Arc<CloseSlot> = session.close();
        wait_close(&slot).expect("closing the test session never fails");
    }

    /// The bash rcfile that emits startup output (the positive initialize
    /// path needs the startup-output gate open).
    fn rcfile_with_banner() -> String {
        let path = std::env::temp_dir().join("fr-shell-session-test-rc.sh");
        std::fs::write(&path, "echo fr-banner\n").expect("rcfile writable");
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn initialize_settles_after_startup_output() {
        let rcfile = rcfile_with_banner();
        let session = make_session(&[
            "/bin/bash",
            "--noprofile",
            "--rcfile",
            rcfile.as_str(),
            "--norc",
        ]);
        session
            .initialize()
            .expect("the banner output opens the startup gate");
        assert_eq!(session.status(), SessionStatus::Running);
        close_quietly(&session);
    }

    #[test]
    fn initialize_times_out_on_silent_startup() {
        let mut config = test_config();
        config.timeout_ms = 300;
        let session = make_session_with(config, &["/bin/sleep", "1000"]);
        let error = session
            .initialize()
            .expect_err("the silent shell rides to the startup timeout");
        assert_eq!(
            error,
            "PTY shell did not reach readiness before startup timeout"
        );
        close_quietly(&session);
    }

    /// A marker-less quick builtin is unobservable to the exact probe (the
    /// same-group pre-write wait never visibly departs), so the silence
    /// bound settles it; production sessions settle this shape through the
    /// prompt marker arm instead.
    #[test]
    fn send_settles_inferred_idle_after_quick_command() {
        let session = make_session(&["/bin/bash", "--noprofile", "--norc"]);
        let op = session
            .start_send("echo fr-tag", true)
            .expect("idle session");
        let result = op.done().expect("the send settles");
        assert_eq!(result.wait_reason, WaitReason::InferredIdle);
        assert!(result.viewport.contains("fr-tag"));
        let page = session.read(None, None).expect("readable scrollback");
        assert!(page.text.contains("fr-tag"));
        close_quietly(&session);
    }

    /// The exact probe arm: a foreground child group parked in a stdin read
    /// is direct post-write evidence (a foreign group needs no departure).
    #[test]
    fn send_settles_stdin_read_via_exact_probe() {
        let session = make_session(&["/bin/bash", "--noprofile", "--norc"]);
        let op = session
            .start_send("cat > /dev/null", true)
            .expect("idle session");
        let result = op.done().expect("the send settles");
        assert_eq!(result.wait_reason, WaitReason::StdinRead);
        close_quietly(&session);
    }

    #[test]
    fn send_settles_inferred_idle_on_silent_child() {
        let session = make_session(&["/bin/bash", "--noprofile", "--norc"]);
        let op = session
            .start_send("sleep 0.3; echo done-sleep", true)
            .expect("idle session");
        let result = op.done().expect("the send settles");
        assert_eq!(result.wait_reason, WaitReason::InferredIdle);
        assert!(result.viewport.contains("done-sleep"));
        close_quietly(&session);
    }

    /// Steady output keeps resetting the silence bound, so the absolute
    /// deadline is the only arm left.
    #[test]
    fn send_times_out_against_loud_foreground() {
        let mut config = test_config();
        config.timeout_ms = 900;
        let session = make_session_with(config, &["/bin/bash", "--noprofile", "--norc"]);
        let op = session
            .start_send("timeout 5 yes tick-flood", true)
            .expect("idle session");
        let result = op.done().expect("the send settles");
        assert_eq!(result.wait_reason, WaitReason::Timeout);
        assert_eq!(result.session_status, SessionStatus::Running);
        close_quietly(&session);
    }

    /// The cancel path releases the send slot after the SIGINT delivery, so
    /// the session accepts a fresh send immediately.
    #[test]
    fn cancel_releases_the_slot_for_the_next_send() {
        let session = make_session(&["/bin/bash", "--noprofile", "--norc"]);
        let op = session.start_send("sleep 30", true).expect("idle session");
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(op.cancel());
        assert!(!op.cancel());
        let first = op.done().expect("the interrupted send settles");
        assert_eq!(first.wait_reason, WaitReason::InferredIdle);
        let next = session
            .start_send("echo after-cancel", true)
            .expect("the slot cleared for the next send");
        let second = next.done().expect("the next send settles");
        assert!(second.viewport.contains("after-cancel"));
        close_quietly(&session);
    }

    #[test]
    fn exit_settles_session_exit_and_status() {
        let session = make_session(&["/bin/bash", "--noprofile", "--norc"]);
        let op = session.start_send("exit", true).expect("idle session");
        let result = op.done().expect("the send settles");
        assert_eq!(result.wait_reason, WaitReason::SessionExit);
        assert_ne!(session.status(), SessionStatus::Running);
        close_quietly(&session);
    }
}
