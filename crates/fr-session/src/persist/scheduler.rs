//! The live-write batch scheduler: one parked thread per backend that
//! fires each armed handle after the dsh batch window.
//!
//! The JS per-handle `setTimeout` (storage.ts:36,277-281, window
//! `LIVE_WRITE_BATCH_MAX_DELAY_MS`) collapses to a shared deadline heap
//! here.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::PersistError;

/// Maximum intentional wait before a routed live session batch starts
/// writing (dsh `LIVE_WRITE_BATCH_MAX_DELAY_MS`).
pub const LIVE_WRITE_BATCH_MAX_DELAY_MS: u64 = 200;

/// One armable timer target. The armed flag is the single source of truth:
/// arming is a `false -> true` swap and every fire re-checks it, so a stale
/// heap entry (drained before its deadline) is skipped instead of
/// double-draining — the lazy analogue of dsh `clearTimeout` at drain start
/// (storage.ts:294-299).
pub(crate) struct TimerTarget {
    armed: AtomicBool,
    /// The fire body; infallible by contract (failures are reported inside).
    fire: Box<dyn Fn() + Send + Sync>,
}

impl TimerTarget {
    pub(crate) fn new(fire: Box<dyn Fn() + Send + Sync>) -> Self {
        Self {
            armed: AtomicBool::new(false),
            fire,
        }
    }

    /// Attempts to arm the target for one window; returns `false` when a
    /// window is already pending (dsh `if (this.batchTimer !== undefined)
    /// return`) or when the scheduler thread is already shut down.
    pub(crate) fn arm(self: &Arc<Self>, scheduler: &BatchScheduler) -> bool {
        if self
            .armed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        if scheduler.schedule(Arc::clone(self)).is_err() {
            // The backend (and with it the scheduler thread) is gone; undo
            // the arming so the flag never wedges shut.
            self.armed.store(false, Ordering::Release);
            return false;
        }
        true
    }

    /// Disarms a pending window (dsh `clearTimeout` at drain start,
    /// storage.ts:295-298). A later fire observes the cleared flag and
    /// drops the entry.
    pub(crate) fn disarm(&self) {
        self.armed.store(false, Ordering::Release);
    }
}

struct SchedulerState {
    inner: Mutex<HeapState>,
    signal: Condvar,
    /// The window applied to every arming, fixed per backend so heap
    /// entries stay ordered exactly as they were accepted.
    max_delay: Duration,
}

struct HeapState {
    heap: BinaryHeap<Reverse<HeapEntry>>,
    seq: u64,
    closed: bool,
}

/// One queued fire: ordered by deadline, then by acceptance order.
struct HeapEntry {
    deadline: Instant,
    seq: u64,
    target: Arc<TimerTarget>,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.seq == other.seq
    }
}
impl Eq for HeapEntry {}
impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.deadline, self.seq).cmp(&(other.deadline, other.seq))
    }
}

/// The backend-owned scheduler: one parked thread, one deadline heap.
/// Cloning shares the thread; dropping the last clone closes the heap and
/// the thread exits on its next wake-up.
#[derive(Clone)]
pub(crate) struct BatchScheduler {
    state: Arc<SchedulerState>,
}

impl BatchScheduler {
    /// Spawns the scheduler thread; `max_delay` is the window applied to
    /// every arming (production passes [`LIVE_WRITE_BATCH_MAX_DELAY_MS`],
    /// tests shorten it).
    ///
    /// # Errors
    ///
    /// Returns `Io` when the worker thread cannot be spawned.
    pub fn spawn(max_delay: Duration) -> Result<Self, PersistError> {
        let state = Arc::new(SchedulerState {
            inner: Mutex::new(HeapState {
                heap: BinaryHeap::new(),
                seq: 0,
                closed: false,
            }),
            signal: Condvar::new(),
            max_delay,
        });
        let worker_state = Arc::clone(&state);
        std::thread::Builder::new()
            .name("fr-session-batch-scheduler".to_owned())
            .spawn(move || scheduler_loop(&worker_state))
            .map_err(PersistError::Io)?;
        Ok(Self { state })
    }

    /// Queues one fire of `target` after the configured window.
    fn schedule(&self, target: Arc<TimerTarget>) -> Result<(), PersistError> {
        let mut heap = lock(&self.state.inner);
        if heap.closed {
            return Err(PersistError::Corruption(
                "session batch scheduler is shut down".to_owned(),
            ));
        }
        heap.seq += 1;
        let seq = heap.seq;
        heap.heap.push(Reverse(HeapEntry {
            deadline: Instant::now() + self.state.max_delay,
            seq,
            target,
        }));
        drop(heap);
        self.state.signal.notify_one();
        Ok(())
    }

    /// Closes the scheduler: pending windows are dropped and the thread
    /// exits on its next wake-up. Later `arm` calls report `false`.
    pub(crate) fn close(&self) {
        let mut heap = lock(&self.state.inner);
        heap.closed = true;
        drop(heap);
        self.state.signal.notify_all();
    }
}

/// What the scheduler loop should do with the current heap head.
enum NextAction {
    /// The head entry reached its deadline; fire it.
    Fire,
    /// The head entry is future; sleep until it.
    Wait(Duration),
    /// The heap is empty; sleep until a wake-up signal.
    Sleep,
    /// The scheduler was closed; drain and exit.
    Finish,
}

fn scheduler_loop(state: &SchedulerState) {
    loop {
        let mut heap = lock(&state.inner);
        let action = if heap.closed {
            NextAction::Finish
        } else {
            match heap.heap.peek() {
                Some(Reverse(HeapEntry { deadline, .. })) if *deadline <= Instant::now() => {
                    NextAction::Fire
                }
                Some(Reverse(HeapEntry { deadline, .. })) => {
                    let wait = deadline.saturating_duration_since(Instant::now());
                    NextAction::Wait(wait)
                }
                None => NextAction::Sleep,
            }
        };
        match action {
            NextAction::Finish => {
                heap.heap.clear();
                return;
            }
            NextAction::Fire => {
                // Pop under the lock, fire outside it: a fire body may
                // re-enter the scheduler to arm the next window.
                if let Some(Reverse(HeapEntry { target, .. })) = heap.heap.pop() {
                    drop(heap);
                    // A panicking fire body must not wedge the scheduler
                    // for every handle: catch and continue (dsh routes the
                    // timeout-callback rejection into the failure report).
                    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
                        if target.armed.swap(false, Ordering::AcqRel) {
                            (target.fire)();
                        }
                    }));
                    heap = lock(&state.inner);
                }
            }
            NextAction::Wait(wait) => {
                let (guard, _timed_out) = wait_timeout(&state.signal, heap, wait);
                heap = guard;
            }
            NextAction::Sleep => {
                heap = wait(&state.signal, heap);
            }
        }
    }
}

/// Locks a scheduler mutex, recovering from poison: the guarded invariants
/// (heap ordering, seq monotonicity) survive a panic mid-section because
/// every mutation is a single-step push or pop.
fn lock(inner: &Mutex<HeapState>) -> MutexGuard<'_, HeapState> {
    inner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn wait<'a>(signal: &'a Condvar, guard: MutexGuard<'a, HeapState>) -> MutexGuard<'a, HeapState> {
    signal
        .wait(guard)
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn wait_timeout<'a>(
    signal: &'a Condvar,
    guard: MutexGuard<'a, HeapState>,
    timeout: Duration,
) -> (MutexGuard<'a, HeapState>, bool) {
    let (guard, result) = signal
        .wait_timeout(guard, timeout)
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    (guard, result.timed_out())
}

// Rust guideline compliant 2026-10-23

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn counter_target(counter: Arc<AtomicUsize>) -> Arc<TimerTarget> {
        Arc::new(TimerTarget::new(Box::new(move || {
            counter.fetch_add(1, Ordering::AcqRel);
        })))
    }

    #[test]
    fn fires_once_after_the_window_and_stale_rearm_is_ignored() {
        let counter = Arc::new(AtomicUsize::new(0));
        let target = counter_target(Arc::clone(&counter));
        let scheduler = BatchScheduler::spawn(Duration::from_millis(30)).expect("scheduler spawns");
        assert!(target.arm(&scheduler), "first arm wins");
        // A second arm inside the same window is the armed-flag no-op.
        assert!(!target.arm(&scheduler), "double arm refused");
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(counter.load(Ordering::Acquire), 1, "fired exactly once");
        // Disarm before the deadline: the queued fire is skipped.
        assert!(target.arm(&scheduler), "re-arm after fire");
        target.disarm();
        std::thread::sleep(Duration::from_millis(90));
        assert_eq!(counter.load(Ordering::Acquire), 1, "disarmed fire skipped");
        scheduler.close();
    }

    #[test]
    fn close_rejects_future_arms() {
        let counter = Arc::new(AtomicUsize::new(0));
        let target = counter_target(Arc::clone(&counter));
        let scheduler = BatchScheduler::spawn(Duration::from_millis(30)).expect("scheduler");
        scheduler.close();
        assert!(!target.arm(&scheduler), "arm after close refused");
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(counter.load(Ordering::Acquire), 0, "nothing fired");
    }
}
