// Rust guideline compliant 2026-10-31
//! The PTY terminal primitive over `portable-pty` (Q16e-1B), the dsh
//! `subprocess-local` `terminal.ts` `LocalTerminalHandle` port.
//!
//! The handle owns one provider terminal process: writes, the cloned master
//! reader (the session pump's exclusive input stream), foreground inspection
//! (the `/proc` tpgid + syscall probe via [`crate::inspect`]), foreground
//! process-group signalling with the shell SIGKILL fence, and the awaited
//! teardown ladder (descendants SIGTERM-then-KILL under start-identity
//! fencing, then the shell SIGTERM-then-KILL race). The dsh `PassThrough`
//! `PassThrough` pause/resume backpressure has no fr counterpart: the fr session pump
//! reads the master reader directly into bounded retention buffers, so the
//! flow-control contract (the pump never buffers unboundedly) holds without
//! a stream pause. The Windows lane (the x03 SIGINT arm and the taskkill
//! ladder) stays archived with the Q99-B runtime-verification gap; the unix
//! arms below are the only compiled paths.

use std::fmt;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::inspect::{
    ProcIntrospection, ProcessIdentity, foreground_pgid, is_stdin_waiting, scan_snapshot,
};
use crate::subprocess::signal_name;

/// The spawn request for one terminal process (the dsh
/// `SubprocessTerminalSpawnSpec`; the `terminalType` field rides the `TERM`
/// environment entry, so the spec carries no separate column).
#[derive(Debug, Clone)]
pub struct TerminalSpawnSpec {
    /// The confined argv (the leading executable plus arguments).
    pub argv: Vec<String>,
    /// The initial working directory.
    pub cwd: Option<String>,
    /// The full child environment (scrubbed base plus overrides).
    pub env: Vec<(String, String)>,
    /// Terminal rows.
    pub rows: u16,
    /// Terminal columns.
    pub cols: u16,
    /// The TERM-to-KILL and exit-wait grace.
    pub grace_ms: f64,
}

/// One foreground observation (the dsh `SubprocessTerminalForeground`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalForeground {
    /// The foreground process group id.
    pub process_group_id: i64,
    /// Whether a group thread sits blocked reading the terminal device.
    pub input_waiting: bool,
}

/// The top-level exit facts (`exitCode` null under a signal, per the dsh
/// outcome mapping).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalOutcome {
    /// The exit code, `None` when a signal killed the process.
    pub exit_code: Option<i64>,
    /// The POSIX signal name, `None` for clean exits.
    pub signal: Option<String>,
}

/// Maps one POSIX signal name to its number (the five model-facing signals).
#[must_use]
pub fn signal_number(name: &str) -> Option<i32> {
    match name {
        "SIGHUP" => Some(1),
        "SIGINT" => Some(2),
        "SIGKILL" => Some(9),
        "SIGTERM" => Some(15),
        "SIGTSTP" => Some(20),
        _ => None,
    }
}

/// The spawn-time root identity capture (the dsh constructor's first read:
/// the tree row for the shell pid, before any exit can race).
fn root_identity(inspector: &dyn ProcIntrospection, pid: u32) -> Option<ProcessIdentity> {
    let pid = i64::from(pid);
    scan_snapshot(inspector)?
        .tree(pid)
        .into_iter()
        .find(|member| member.pid == pid)
}

/// The shared terminal state behind one handle.
struct TerminalState {
    /// The shell pid.
    pid: u32,
    /// Whether the exit event has published.
    exited: AtomicBool,
    /// The exit facts once the waiter publishes them.
    outcome: Mutex<Option<TerminalOutcome>>,
    /// Wakeups for outcome publication.
    outcome_ready: Condvar,
    /// Termination ladder started (idempotent).
    termination_started: AtomicBool,
    /// The termination ladder's settled result.
    termination: Mutex<Option<Result<(), String>>>,
    /// Wakeups for the termination result.
    termination_ready: Condvar,
    /// The shell's spawn-time start identity (adoption fencing).
    root_identity: Option<ProcessIdentity>,
    /// Adopted descendants (start-identity fenced).
    tracked: Mutex<Vec<ProcessIdentity>>,
    /// The teardown grace.
    grace_ms: f64,
    /// The SIGKILL escalation arm.
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    /// The master side (resize plus lifetime pinning: dropping the master
    /// closes the master fd and would hang the child's next read).
    master: Mutex<Box<dyn MasterPty + Send>>,
    /// The `/proc` introspection surface.
    inspector: Arc<dyn ProcIntrospection>,
}

impl fmt::Debug for TerminalState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalState")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

/// One provider-owned terminal process handle (the dsh
/// `SubprocessTerminalHandle`).
pub struct TerminalHandle {
    /// The shell pid.
    pub pid: u32,
    /// The master reader, taken exactly once by the session pump.
    reader: Mutex<Option<Box<dyn Read + Send>>>,
    /// The master writer (serialized under one lock).
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// The shared lifecycle state.
    state: Arc<TerminalState>,
}

impl fmt::Debug for TerminalHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalHandle")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

/// Allocates one PTY and spawns the spec's argv inside it.
///
/// # Errors
///
/// Returns the allocator's or spawner's error text when the PTY pair or the
/// child process cannot be created.
pub fn spawn_terminal(
    spec: &TerminalSpawnSpec,
    inspector: Arc<dyn ProcIntrospection>,
) -> Result<TerminalHandle, String> {
    let Some(argv0) = spec.argv.first() else {
        return Err("terminal-bash: sandbox returned empty argv".to_owned());
    };
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: spec.rows,
            cols: spec.cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| error.to_string())?;
    let mut builder = CommandBuilder::new(argv0);
    builder.args(&spec.argv[1..]);
    if let Some(cwd) = &spec.cwd {
        builder.cwd(cwd);
    }
    builder.env_clear();
    for (key, value) in &spec.env {
        builder.env(key, value);
    }
    let child = pty
        .slave
        .spawn_command(builder)
        .map_err(|error| error.to_string())?;
    let pid = child
        .process_id()
        .ok_or_else(|| "terminal spawn did not report a pid".to_owned())?;
    let reader = pty
        .master
        .try_clone_reader()
        .map_err(|error| error.to_string())?;
    let writer = pty
        .master
        .take_writer()
        .map_err(|error| error.to_string())?;
    let killer = child.clone_killer();
    let root = root_identity(inspector.as_ref(), pid);
    let state = Arc::new(TerminalState {
        pid,
        exited: AtomicBool::new(false),
        outcome: Mutex::new(None),
        outcome_ready: Condvar::new(),
        termination_started: AtomicBool::new(false),
        termination: Mutex::new(None),
        termination_ready: Condvar::new(),
        root_identity: root,
        tracked: Mutex::new(Vec::new()),
        grace_ms: spec.grace_ms,
        killer: Mutex::new(killer),
        master: Mutex::new(pty.master),
        inspector,
    });
    publish_on_wait(Arc::clone(&state), child);
    Ok(TerminalHandle {
        pid,
        reader: Mutex::new(Some(reader)),
        writer: Arc::new(Mutex::new(writer)),
        state,
    })
}

/// The exit-facts waiter thread (one per terminal; the dsh onExit). The
/// child is reaped here with the raw kernel status so the POSIX signal
/// number maps to the dsh SIG* name without a locale-dependent strsignal
/// round-trip; the portable child wrapper is never waited on.
fn publish_on_wait(state: Arc<TerminalState>, child: Box<dyn Child + Send + Sync>) {
    std::thread::Builder::new()
        .name("fr-shell-pty-wait".to_owned())
        .spawn(move || {
            drop(child);
            let pid = state.pid;
            let pid_cast: libc::pid_t = pid.try_into().unwrap_or(-1);
            let mut status = 0_i32;
            let outcome = loop {
                // SAFETY: waitpid on the terminal's own child pid; only
                // this thread reaps it, and an EINTR/ECHILD retry keeps the
                // loop bounded by the exit event itself.
                let reaped = unsafe {
                    let status_ptr = std::ptr::addr_of_mut!(status);
                    libc::waitpid(pid.try_into().unwrap_or(-1), status_ptr, 0)
                };
                if reaped == pid_cast {
                    break map_wait_status(status);
                }
                if reaped < 0 {
                    // EINTR retries; ECHILD (already reaped elsewhere)
                    // degrades to the transport-failure shape.
                    let errno = std::io::Error::last_os_error();
                    if errno.raw_os_error() == Some(libc::EINTR) {
                        continue;
                    }
                    break TerminalOutcome {
                        exit_code: None,
                        signal: None,
                    };
                }
            };
            let first_publication = {
                let mut slot = state.outcome.lock().expect("terminal outcome lock");
                let first = slot.is_none();
                if first {
                    *slot = Some(outcome);
                }
                first
            };
            if first_publication {
                state.exited.store(true, Ordering::Release);
                state.outcome_ready.notify_all();
            }
        })
        .expect("spawning the terminal waiter never fails under the plugin contract");
}

/// Maps one raw kernel wait status into the dsh outcome shape.
#[must_use]
fn map_wait_status(status: i32) -> TerminalOutcome {
    if libc::WIFSIGNALED(status) {
        return TerminalOutcome {
            exit_code: None,
            signal: Some(signal_name(libc::WTERMSIG(status))),
        };
    }
    TerminalOutcome {
        exit_code: Some(i64::from(libc::WEXITSTATUS(status))),
        signal: None,
    }
}

impl TerminalHandle {
    /// Takes the master reader for the session pump (one taker).
    ///
    /// # Panics
    ///
    /// Never in practice: the reader lock's poisoning arm is unreachable
    /// under the plugin recovery contract.
    #[must_use]
    pub fn take_reader(&self) -> Option<Box<dyn Read + Send>> {
        self.reader.lock().expect("terminal reader lock").take()
    }

    /// Writes one text chunk to the terminal master.
    ///
    /// # Errors
    ///
    /// Returns `terminal process has exited` after the exit event, and the
    /// writer's io error text when the write fails.
    ///
    /// # Panics
    ///
    /// Never in practice: the writer lock's poisoning arm is unreachable
    /// under the plugin recovery contract.
    pub fn write(&self, data: &str) -> Result<(), String> {
        if self.state.exited.load(Ordering::Acquire) {
            return Err("terminal process has exited".to_owned());
        }
        let mut writer = self.writer.lock().expect("terminal writer lock");
        writer
            .write_all(data.as_bytes())
            .and_then(|()| writer.flush())
            .map_err(|error| error.to_string())
    }

    /// Resizes the terminal window.
    ///
    /// # Errors
    ///
    /// Returns `terminal process has exited` after the exit event, and the
    /// resize error text on failure.
    ///
    /// # Panics
    ///
    /// Never in practice: the master lock's poisoning arm is unreachable
    /// under the plugin recovery contract.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        if self.state.exited.load(Ordering::Acquire) {
            return Err("terminal process has exited".to_owned());
        }
        self.state
            .master
            .lock()
            .expect("terminal master lock")
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| error.to_string())
    }

    /// Observes the foreground process group and refreshes the tracked
    /// descendants (the dsh `inspectForeground`: adoption rides every
    /// observation).
    ///
    /// # Errors
    ///
    /// Returns the snapshot error text when `/proc` is unreadable.
    ///
    /// # Panics
    ///
    /// Never in practice: the tracked lock's poisoning arm is unreachable
    /// under the plugin recovery contract.
    pub fn inspect_foreground(&self) -> Result<Option<TerminalForeground>, String> {
        self.state.refresh_descendants()?;
        let shell_pid = handle_pid_i64(self.pid);
        let Some(group_id) = foreground_pgid(self.state.inspector.as_ref(), shell_pid) else {
            return Ok(None);
        };
        let input_waiting = is_stdin_waiting(self.state.inspector.as_ref(), group_id, shell_pid);
        Ok(Some(TerminalForeground {
            process_group_id: group_id,
            input_waiting,
        }))
    }

    /// Delivers one allowed signal to the verified foreground process group.
    ///
    /// # Errors
    ///
    /// Returns the snapshot error text, the no-foreground text, and the
    /// shell SIGKILL fence text (the dsh `signalForeground` contract).
    ///
    /// # Panics
    ///
    /// Never in practice: the inspection locks' poisoning arm is
    /// unreachable under the plugin recovery contract.
    pub fn signal_foreground(&self, signal: &str) -> Result<i64, String> {
        let Some(foreground) = self.inspect_foreground()? else {
            return Err(format!(
                "cannot resolve foreground process group for terminal {}",
                self.pid
            ));
        };
        let pgid = foreground.process_group_id;
        if signal == "SIGKILL" && pgid == i64::from(self.pid) {
            return Err(
                "refusing to SIGKILL the terminal shell; terminate the terminal session instead"
                    .to_owned(),
            );
        }
        let Some(number) = signal_number(signal) else {
            return Err(format!("signal {signal} is unsupported"));
        };
        self.state.inspector.signal_group(pgid, number);
        Ok(pgid)
    }

    /// Whether the exit event has not yet published.
    #[must_use]
    pub fn running(&self) -> bool {
        !self.state.exited.load(Ordering::Acquire)
    }

    /// The exit facts once published, `None` while running.
    ///
    /// # Panics
    ///
    /// Never in practice: the outcome lock's poisoning arm is unreachable
    /// under the plugin recovery contract.
    #[must_use]
    pub fn settled_outcome(&self) -> Option<TerminalOutcome> {
        self.state
            .outcome
            .lock()
            .expect("terminal outcome lock")
            .clone()
    }

    /// Blocks until the exit facts publish.
    ///
    /// # Panics
    ///
    /// Never in practice: the outcome lock's poisoning arm is unreachable
    /// under the plugin recovery contract.
    #[must_use]
    pub fn done(&self) -> TerminalOutcome {
        let mut slot = self.state.outcome.lock().expect("terminal outcome lock");
        loop {
            if let Some(outcome) = slot.as_ref() {
                return outcome.clone();
            }
            let (next, _timeout) = self
                .state
                .outcome_ready
                .wait_timeout(slot, Duration::from_millis(200))
                .expect("terminal outcome lock");
            slot = next;
        }
    }

    /// Starts the teardown ladder once (the dsh `terminate()`); the awaited
    /// result surfaces through [`Self::termination`] or
    /// [`Self::wait_termination`].
    ///
    /// # Panics
    ///
    /// Never in practice: the ladder thread's builder only fails on OS
    /// resource exhaustion, which the plugin contract treats as
    /// unreachable (the dsh thread-spawn expectation), and the termination
    /// lock's poisoning arm follows the same contract.
    pub fn terminate(&self) {
        if self.state.termination_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let state = Arc::clone(&self.state);
        std::thread::Builder::new()
            .name("fr-shell-pty-close".to_owned())
            .spawn(move || {
                let result = close_once(&state);
                let first_publication = {
                    let mut slot = state.termination.lock().expect("terminal termination lock");
                    let first = slot.is_none();
                    if first {
                        *slot = Some(result);
                    }
                    first
                };
                if first_publication {
                    state.termination_ready.notify_all();
                }
            })
            .expect("spawning the termination ladder never fails under the plugin contract");
    }

    /// The termination ladder's result once settled.
    ///
    /// # Panics
    ///
    /// Never in practice: the termination lock's poisoning arm is
    /// unreachable under the plugin recovery contract.
    #[must_use]
    pub fn termination(&self) -> Option<Result<(), String>> {
        self.state
            .termination
            .lock()
            .expect("terminal termination lock")
            .clone()
    }

    /// Blocks until the termination ladder settles.
    ///
    /// # Errors
    ///
    /// Returns the surviving-pid text when the ladder could not clear the
    /// tree (the dsh `closeOnce` rejection).
    ///
    /// # Panics
    ///
    /// Never in practice: the termination lock's poisoning arm is
    /// unreachable under the plugin recovery contract.
    pub fn wait_termination(&self) -> Result<(), String> {
        let mut slot = self
            .state
            .termination
            .lock()
            .expect("terminal termination lock");
        loop {
            if let Some(result) = slot.as_ref() {
                return result.clone();
            }
            let (next, _timeout) = self
                .state
                .termination_ready
                .wait_timeout(slot, Duration::from_millis(200))
                .expect("terminal termination lock");
            slot = next;
        }
    }

    /// Synchronous force-stop of the tree without timers (the host-exit
    /// path). Does not claim quiescence and does not replace `terminate`.
    pub fn terminate_for_host_exit(&self) {
        let Some(snapshot) = scan_snapshot(self.state.inspector.as_ref()) else {
            self.state.force_stop_shell();
            return;
        };
        for member in self.state.descendants_from(&snapshot) {
            self.state.signal_identity_killer(&member);
        }
        self.state.force_stop_shell();
        for member in self.state.descendants_from(&snapshot) {
            self.state.signal_identity_killer(&member);
        }
    }
}

/// The handle pid as the i64 `/proc` domain (the OS pid width cast).
fn handle_pid_i64(pid: u32) -> i64 {
    i64::from(pid)
}

impl TerminalState {
    /// Refreshes the tracked descendants (the dsh `descendants()`: adopt
    /// tree plus session members only while the numeric root pid provably
    /// still carries the spawn-time start identity).
    fn refresh_descendants(&self) -> Result<(), String> {
        let snapshot = scan_snapshot(self.inspector.as_ref())
            .ok_or_else(|| "Cannot inspect processes: /proc directory is unreadable".to_owned())?;
        let next = self.descendants_from(&snapshot);
        {
            let mut tracked = self.tracked.lock().expect("terminal tracked lock");
            *tracked = next;
        }
        Ok(())
    }

    /// The adoption walk against one snapshot (tree plus session members
    /// under the root identity fence; survivors only, self excluded).
    fn descendants_from(&self, snapshot: &crate::inspect::ProcessSnapshot) -> Vec<ProcessIdentity> {
        let pid = handle_pid_i64(self.pid);
        let tree = snapshot.tree(pid);
        let root_row = tree.iter().find(|member| member.pid == pid);
        let root_verified = self.root_identity.as_ref().is_some_and(|spawn_identity| {
            root_row.is_some_and(|row| row.started == spawn_identity.started)
        });
        let mut candidates = self.tracked.lock().expect("terminal tracked lock").clone();
        if root_verified {
            let session = snapshot.session(pid);
            for member in tree.iter().chain(session.iter()) {
                candidates.push(member.clone());
            }
        }
        candidates.retain(|member| member.pid != pid);
        candidates.retain(|member| snapshot.alive(member));
        candidates.sort_by_key(|member| member.pid);
        candidates.dedup_by(|a, b| a.pid == b.pid && a.started == b.started);
        candidates
    }

    /// Signals one exact identity after rechecking its start fence.
    fn signal_identity(&self, identity: &ProcessIdentity, sig: i32) {
        let Some(stat) = self.inspector.stat(identity.pid) else {
            return;
        };
        if stat.started != identity.started || crate::inspect::quiescent(Some(&stat.state)) {
            return;
        }
        self.inspector.signal_process(identity.pid, sig);
    }

    /// The KILL arm against one identity without the fence re-check (the
    /// host-exit force-stop path).
    fn signal_identity_killer(&self, identity: &ProcessIdentity) {
        self.inspector.signal_process(identity.pid, libc::SIGKILL);
    }

    /// Polls survivors every 25ms until the grace elapses.
    fn wait_for_members(
        &self,
        members: &[ProcessIdentity],
    ) -> Result<Vec<ProcessIdentity>, String> {
        if members.is_empty() {
            return Ok(Vec::new());
        }
        let until = Instant::now() + self.grace_duration();
        let mut survivors = self.survivors_of(members)?;
        while !survivors.is_empty() && Instant::now() < until {
            let remaining = until.saturating_duration_since(Instant::now());
            let step = remaining
                .min(Duration::from_millis(25))
                .max(Duration::from_millis(1));
            std::thread::sleep(step);
            survivors = self.survivors_of(members)?;
        }
        Ok(survivors)
    }

    #[must_use]
    const fn grace_duration(&self) -> Duration {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the grace is a non-negative millisecond bound; the u64 floor keeps the OS wait range"
        )]
        Duration::from_millis(self.grace_ms as u64)
    }

    fn survivors_of(&self, members: &[ProcessIdentity]) -> Result<Vec<ProcessIdentity>, String> {
        let snapshot = scan_snapshot(self.inspector.as_ref())
            .ok_or_else(|| "Cannot inspect processes: /proc directory is unreadable".to_owned())?;
        Ok(members
            .iter()
            .filter(|member| snapshot.alive(member))
            .cloned()
            .collect())
    }

    /// The dsh stopDescendants ladder: TERM the captured set, wait the
    /// grace, KILL the union with any fresh adoptees, wait, and report the
    /// remaining survivors.
    fn stop_descendants(&self) -> Result<Vec<ProcessIdentity>, String> {
        let captured = std::mem::take(&mut *self.tracked.lock().expect("terminal tracked lock"));
        let captured = self.union_with_fresh(captured)?;
        self.signal_members(&captured, libc::SIGTERM);
        let captured_survivors = self.wait_for_members(&captured)?;
        let members = self.union_with_fresh(captured_survivors)?;
        self.signal_members(&members, libc::SIGKILL);
        let survivors = self.wait_for_members(&members)?;
        self.union_with_fresh(survivors)
    }

    fn signal_members(&self, members: &[ProcessIdentity], sig: i32) {
        for member in members {
            self.signal_identity(member, sig);
        }
    }

    /// The dsh unionMembers against a fresh adoption round.
    fn union_with_fresh(
        &self,
        members: Vec<ProcessIdentity>,
    ) -> Result<Vec<ProcessIdentity>, String> {
        let snapshot = scan_snapshot(self.inspector.as_ref())
            .ok_or_else(|| "Cannot inspect processes: /proc directory is unreadable".to_owned())?;
        let fresh = self.descendants_from(&snapshot);
        let mut union: Vec<ProcessIdentity> = members;
        for member in fresh {
            union.push(member);
        }
        union.retain(|member| snapshot.alive(member));
        union.sort_by_key(|member| member.pid);
        union.dedup_by(|a, b| a.pid == b.pid && a.started == b.started);
        Ok(union)
    }

    /// The dsh stopShell ladder: TERM, wait done-or-grace, KILL, wait
    /// done-or-grace, then the surviving-pid rejection.
    fn stop_shell(&self) -> Result<(), String> {
        if !self.exited.load(Ordering::Acquire) {
            // SAFETY: kill on the terminal's own shell pid (the TERM arm of
            // the stop ladder); a raced exit makes this a harmless no-op.
            let _ = unsafe {
                libc::kill(
                    self.pid.try_into().unwrap_or(libc::pid_t::MAX),
                    libc::SIGTERM,
                )
            };
            self.wait_exit_or_grace();
        }
        if !self.exited.load(Ordering::Acquire) {
            {
                let mut killer = self.killer.lock().expect("terminal killer lock");
                let _ = killer.kill();
            }
            self.wait_exit_or_grace();
        }
        if !self.exited.load(Ordering::Acquire) {
            return Err(format!(
                "terminal cleanup failed; surviving pid: {}",
                self.pid
            ));
        }
        Ok(())
    }

    fn wait_exit_or_grace(&self) {
        let total = self.grace_duration();
        let slot = self.outcome.lock().expect("terminal outcome lock");
        let (_lock_guard, _wait_timeout) = self
            .outcome_ready
            .wait_timeout_while(slot, total, |current: &mut Option<TerminalOutcome>| {
                current.is_none()
            })
            .expect("terminal outcome lock");
    }

    /// Force-stops the shell identity without timers (the host-exit path).
    fn force_stop_shell(&self) {
        if self.exited.load(Ordering::Acquire) {
            return;
        }
        if let Some(root) = &self.root_identity {
            // SAFETY: kill on the fenced root identity's pid (the host-exit
            // force-stop arm); a raced exit makes this a harmless no-op.
            let _ = unsafe {
                libc::kill(
                    root.pid.try_into().unwrap_or(libc::pid_t::MAX),
                    libc::SIGKILL,
                )
            };
            return;
        }
        let mut killer = self.killer.lock().expect("terminal killer lock");
        let _ = killer.kill();
    }
}

/// The dsh closeOnce: clear descendants, clear the shell, re-check
/// descendants, and reject on any survivors (the Windows settleExitIfGone
/// arm stays archived with the Windows lane).
fn close_once(state: &TerminalState) -> Result<(), String> {
    let survivors = state.stop_descendants()?;
    if !survivors.is_empty() {
        return Err(format!(
            "terminal cleanup failed; surviving pids: {}",
            survivors
                .iter()
                .map(|member| member.pid.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    state.stop_shell()?;
    let survivors = state.stop_descendants()?;
    if !survivors.is_empty() {
        return Err(format!(
            "terminal cleanup failed; surviving pids: {}",
            survivors
                .iter()
                .map(|member| member.pid.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{TerminalSpawnSpec, spawn_terminal};
    use crate::inspect::ProcFs;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn spawn_spec(argv: &[&str]) -> TerminalSpawnSpec {
        TerminalSpawnSpec {
            argv: argv.iter().map(|word| (*word).to_owned()).collect(),
            cwd: None,
            env: vec![
                ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
                ("TERM".to_owned(), "dumb".to_owned()),
            ],
            rows: 40,
            cols: 160,
            grace_ms: 1000.0,
        }
    }

    /// Reads the master reader on a park-able collector thread until
    /// `needle` appears or the deadline passes (the thread stays parked on
    /// the blocking master fd after the deadline; the teardown's EOF ends
    /// it).
    fn read_until(handle: &super::TerminalHandle, needle: &str, budget: Duration) -> String {
        use std::sync::atomic::{AtomicBool, Ordering};
        let Some(mut reader) = handle.take_reader() else {
            return String::new();
        };
        let shared = Arc::new(std::sync::Mutex::new(String::new()));
        let collector = Arc::clone(&shared);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_reader = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut buffer = [0_u8; 4096];
            loop {
                if stop_reader.load(Ordering::Acquire) {
                    return;
                }
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => return,
                    Ok(count) => {
                        let mut text = collector.lock().expect("collector lock");
                        text.push_str(&String::from_utf8_lossy(&buffer[..count]));
                    }
                }
            }
        });
        let deadline = Instant::now() + budget;
        loop {
            let snapshot = shared.lock().expect("collector lock").clone();
            if snapshot.contains(needle) || Instant::now() >= deadline {
                stop.store(true, Ordering::Release);
                return snapshot;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn pty_echo_roundtrip_and_teardown_settles_exit_code() {
        let handle = spawn_terminal(&spawn_spec(&["/bin/cat"]), Arc::new(ProcFs)).expect("spawned");
        handle.write("fr-pty-roundtrip\n").expect("written");
        let _text = read_until(&handle, "fr-pty-roundtrip", Duration::from_secs(3));
        handle.terminate();
        handle.wait_termination().expect("clean teardown");
        let outcome = handle.done();
        // The stop ladder TERMs the shell itself, so the direct cat dies by
        // signal (the dsh outcome mapping: exitCode null under a signal).
        assert_eq!(outcome.exit_code, None);
        assert_eq!(outcome.signal, Some("SIGTERM".to_owned()));
    }

    #[test]
    fn terminate_ladder_clears_the_descendant_tree() {
        let handle = spawn_terminal(
            &spawn_spec(&["/bin/bash", "-c", "sleep 30 & wait"]),
            Arc::new(ProcFs),
        )
        .expect("spawned");
        handle.terminate();
        handle.wait_termination().expect("clean teardown");
        let outcome = handle.done();
        // The sleep dies under TERM, the waiting bash exits cleanly.
        assert_eq!(outcome.exit_code, Some(0));
    }

    #[test]
    fn signal_foreground_fences_shell_sigkill_and_delivers_int() {
        let handle = spawn_terminal(
            &spawn_spec(&["/bin/bash", "--noprofile", "--norc", "-i"]),
            Arc::new(ProcFs),
        )
        .expect("spawned");
        let foreground = handle
            .inspect_foreground()
            .expect("inspected")
            .expect("interactive bash owns the tty");
        assert_eq!(foreground.process_group_id, i64::from(handle.pid));
        let refused = handle
            .signal_foreground("SIGKILL")
            .expect_err("shell SIGKILL refused");
        assert_eq!(
            refused,
            "refusing to SIGKILL the terminal shell; terminate the terminal session instead"
        );
        let delivered = handle.signal_foreground("SIGINT").expect("delivered");
        assert_eq!(delivered, i64::from(handle.pid));
        handle.terminate();
        handle.wait_termination().expect("clean teardown");
    }

    #[test]
    fn write_after_exit_reports_the_dsh_error_text() {
        let handle = spawn_terminal(&spawn_spec(&["/bin/cat"]), Arc::new(ProcFs)).expect("spawned");
        handle.terminate();
        let _ = handle.wait_termination();
        let _ = handle.done();
        let error = handle.write("late").expect_err("exited");
        assert_eq!(error, "terminal process has exited");
    }
}
