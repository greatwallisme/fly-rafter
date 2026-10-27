// Rust guideline compliant 2026-10-27
//! The loop's phase machine (dsh `agent.ts` `Phase`/`setPhase` port).
//!
//! Three phases cover the loop: `idle` (parked), `maintenance` (a
//! structured job owns the loop; new steps refuse), and `running` (a
//! driver owns the turn/step cycle). The externally visible status
//! collapses maintenance onto idle (dsh `status` getter), and a phase
//! commit publishes `agent/status` exactly when that derived value flips
//! (dsh `setPhase` single point).
//!
//! Each non-idle phase owns an [`AbortLatch`]: the dsh `AbortController`
//! and `signal` pair collapsed into the one slot the driver thread reads.
//! The latch is driver-owned (Q79-A): a queued `Cancel` command is applied
//! by the loop thread itself, and the dsh `throwIfAborted` sites become
//! mailbox polls at the same control-flow points. The first cause wins —
//! a later `abort` on an already-aborted latch is a no-op, matching both
//! the WHATWG controller and dsh's first-cause-wins cancel contract.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use fr_wire::event::CancelCause;

use crate::agent_events::AgentEvents;

/// The externally visible loop status (dsh `AgentStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    /// No driver is running the turn cycle.
    #[default]
    Idle,
    /// A driver is running the turn cycle.
    Running,
}

/// The shared status snapshot the agent service's query face reads.
///
/// Decision 46: `whenIdle` over the ABI is the `agent/status` event plus
/// a non-blocking query — a require that blocks waiting for idle would
/// violate the fast-path invariant.
#[derive(Debug, Default)]
pub struct StatusCell(AtomicU8);

impl StatusCell {
    /// The idle snapshot.
    #[must_use]
    pub const fn new() -> Self {
        Self(AtomicU8::new(0))
    }

    fn store(&self, status: AgentStatus) {
        let encoded = match status {
            AgentStatus::Idle => 0,
            AgentStatus::Running => 1,
        };
        self.0.store(encoded, Ordering::Release);
    }

    /// Reads the latest committed status.
    #[must_use]
    pub fn load(&self) -> AgentStatus {
        match self.0.load(Ordering::Acquire) {
            1 => AgentStatus::Running,
            _ => AgentStatus::Idle,
        }
    }
}

/// One loop-owned abort latch; see the module docs for the threading
/// contract (single driver thread reads and writes it).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AbortLatch {
    cause: Option<CancelCause>,
}

impl AbortLatch {
    /// The live (unaborted) latch.
    #[must_use]
    pub const fn new() -> Self {
        Self { cause: None }
    }

    /// Latches the cancellation cause; the first cause wins.
    pub fn abort(&mut self, cause: CancelCause) {
        if self.cause.is_none() {
            self.cause = Some(cause);
        }
    }

    /// The latched cause while aborted (`dsh abortedCancelCause` returns
    /// undefined on a live signal).
    #[must_use]
    pub const fn aborted_cause(&self) -> Option<&CancelCause> {
        self.cause.as_ref()
    }
}

/// The loop's current phase (dsh `Phase`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Parked; the last completed turn number is retained for the next
    /// driver's first turn (`lastTurn + 1`).
    Idle {
        /// The last turn that ran.
        last_turn: u64,
    },
    /// A maintenance job owns the loop; new steps refuse.
    Maintenance {
        /// The job's abort latch.
        abort: AbortLatch,
        /// The last completed turn number.
        last_turn: u64,
        /// A wake the job must replay once it converges.
        wake_requested: bool,
    },
    /// A driver owns the turn/step cycle.
    Running {
        /// The driver's abort latch.
        abort: AbortLatch,
        /// The running turn's number.
        turn: u64,
        /// The running step's number within the turn.
        step: u64,
        /// A wake to replay when the driver converges.
        wake_requested: bool,
    },
}

impl Default for Phase {
    fn default() -> Self {
        Self::Idle { last_turn: 0 }
    }
}

impl Phase {
    /// The externally visible status (maintenance collapses onto idle).
    #[must_use]
    pub const fn status(&self) -> AgentStatus {
        match self {
            Self::Idle { .. } | Self::Maintenance { .. } => AgentStatus::Idle,
            Self::Running { .. } => AgentStatus::Running,
        }
    }
}

/// The driver-owned phase bookkeeping: the current phase, the dispatch
/// face that publishes status flips, and the shared snapshot the agent
/// service's query face reads.
#[derive(Debug)]
pub struct PhaseMachine {
    phase: Phase,
    events: Arc<AgentEvents>,
    cell: Arc<StatusCell>,
}

impl PhaseMachine {
    /// Builds the machine over an initial phase.
    #[must_use]
    pub fn new(phase: Phase, events: Arc<AgentEvents>, cell: Arc<StatusCell>) -> Self {
        cell.store(phase.status());
        Self {
            phase,
            events,
            cell,
        }
    }

    /// The current phase.
    #[must_use]
    pub const fn phase(&self) -> &Phase {
        &self.phase
    }

    /// Mutable access for the in-place field updates the dsh driver
    /// performs between commits (`phase.turn`, `phase.step`,
    /// `wakeRequested`).
    #[must_use]
    pub const fn phase_mut(&mut self) -> &mut Phase {
        &mut self.phase
    }

    /// The current derived status.
    #[must_use]
    pub const fn status(&self) -> AgentStatus {
        self.phase.status()
    }

    /// Commits the next phase, publishing `agent/status` exactly when the
    /// derived status flips (dsh `setPhase`).
    pub fn commit(&mut self, next: Phase) {
        let previous = self.phase.status();
        self.phase = next;
        let status = self.phase.status();
        self.cell.store(status);
        if status != previous {
            self.events.emit_status(status);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AbortLatch, AgentStatus, Phase, PhaseMachine, StatusCell};
    use crate::agent_events::AgentEvents;
    use crate::outbound::Outbound;
    use dougong_abi::{HostExports, STATUS_OK};
    use fr_wire::event::CancelCause;
    use std::ffi::c_void;
    use std::sync::{Arc, Mutex};

    fn quiet_events() -> AgentEvents {
        unsafe extern "C" fn quiet_require(
            _host: *mut c_void,
            _service: *const u8,
            _service_len: usize,
            _method: u32,
            _in_data: *const u8,
            _in_len: usize,
            _out_buf: *mut u8,
            _cap: usize,
            _out_len: *mut usize,
        ) -> i32 {
            STATUS_OK
        }
        unsafe extern "C" fn quiet_emit(
            _host: *mut c_void,
            _event: *const u8,
            _event_len: usize,
            _payload: *const u8,
            _payload_len: usize,
        ) -> i32 {
            STATUS_OK
        }
        AgentEvents::new(
            Outbound::new(HostExports {
                host_handle: std::ptr::null_mut(),
                require: quiet_require,
                emit: quiet_emit,
            }),
            Arc::new(|_warning: &str| {}),
        )
    }

    /// Wraps a face with one status recorder registered.
    fn recorded_events(seen: &Arc<Mutex<Vec<AgentStatus>>>) -> Arc<AgentEvents> {
        let mut events = quiet_events();
        let seen = Arc::clone(seen);
        events.on_status(Box::new(move |status| {
            seen.lock().expect("test lock").push(status);
            Ok(())
        }));
        Arc::new(events)
    }

    #[test]
    fn status_collapses_maintenance_onto_idle() {
        assert_eq!(Phase::Idle { last_turn: 3 }.status(), AgentStatus::Idle);
        assert_eq!(
            Phase::Maintenance {
                abort: AbortLatch::new(),
                last_turn: 3,
                wake_requested: false,
            }
            .status(),
            AgentStatus::Idle
        );
        assert_eq!(
            Phase::Running {
                abort: AbortLatch::new(),
                turn: 4,
                step: 0,
                wake_requested: false,
            }
            .status(),
            AgentStatus::Running
        );
    }

    #[test]
    fn commit_publishes_only_cross_value_flips() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let events = recorded_events(&seen);
        let cell = Arc::new(StatusCell::new());
        let mut machine = PhaseMachine::new(
            Phase::Idle { last_turn: 2 },
            Arc::clone(&events),
            Arc::clone(&cell),
        );
        assert_eq!(cell.load(), AgentStatus::Idle);

        machine.commit(Phase::Maintenance {
            abort: AbortLatch::new(),
            last_turn: 2,
            wake_requested: false,
        });
        machine.commit(Phase::Idle { last_turn: 2 });
        assert_eq!(*seen.lock().expect("test lock"), []);

        machine.commit(Phase::Running {
            abort: AbortLatch::new(),
            turn: 3,
            step: 0,
            wake_requested: false,
        });
        assert_eq!(*seen.lock().expect("test lock"), [AgentStatus::Running]);
        assert_eq!(cell.load(), AgentStatus::Running);

        machine.commit(Phase::Idle { last_turn: 3 });
        assert_eq!(
            *seen.lock().expect("test lock"),
            [AgentStatus::Running, AgentStatus::Idle]
        );
        assert_eq!(cell.load(), AgentStatus::Idle);
    }

    #[test]
    fn abort_latch_keeps_the_first_cause() {
        let mut latch = AbortLatch::new();
        assert!(latch.aborted_cause().is_none());
        latch.abort(CancelCause::User);
        latch.abort(CancelCause::Hook {
            reason: "late veto".to_owned(),
        });
        assert_eq!(latch.aborted_cause(), Some(&CancelCause::User));
    }

    #[test]
    fn phase_mut_updates_fields_in_place() {
        let events = quiet_events().into();
        let mut machine = PhaseMachine::new(
            Phase::Running {
                abort: AbortLatch::new(),
                turn: 1,
                step: 0,
                wake_requested: false,
            },
            events,
            Arc::new(StatusCell::new()),
        );
        let Phase::Running { turn, step, .. } = machine.phase_mut() else {
            panic!("fixture starts a running phase");
        };
        *turn = 2;
        *step = 5;
        assert_eq!(
            machine.phase(),
            &Phase::Running {
                abort: AbortLatch::new(),
                turn: 2,
                step: 5,
                wake_requested: false,
            }
        );
    }
}
