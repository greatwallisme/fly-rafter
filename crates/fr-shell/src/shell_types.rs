// Rust guideline compliant 2026-10-29
//! Execution types for the bash executor seam (dsh
//! `shell/src/types.ts`).
//!
//! Requests, resolved specs, foreground results with first-cause
//! classification, and background process handles. Job ids, ownership,
//! polling, and notices belong to the [`crate::jobs_local`] registry; the
//! managed-environment vocabulary rides the FR_ prefix (PRD 19.2 条 25).

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

/// The managed child environment prefix (PRD 19.2 条 25: the dsh `DSH_`
/// namespace under the Q16a-1A ruling).
pub const FR_ENV_PREFIX: &str = "FR_";

/// An abort signal over threads: the first abort wins and the reason
/// defaults to `aborted`.
#[derive(Debug, Default)]
pub struct AbortSignal {
    aborted: AtomicBool,
    reason: Mutex<Option<String>>,
}

impl AbortSignal {
    /// Creates a clear signal.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Aborts with a reason; the first abort wins (the dsh once-arm).
    pub fn abort(&self, reason: impl Into<String>) {
        if self.aborted.swap(true, Ordering::AcqRel) {
            return;
        }
        *recover(&self.reason) = Some(reason.into());
    }

    /// Whether the signal fired.
    #[must_use]
    pub fn is_aborted(&self) -> bool {
        self.aborted.load(Ordering::Acquire)
    }

    /// The first abort reason, or the stable `aborted` default.
    #[must_use]
    pub fn reason_text(&self) -> String {
        recover(&self.reason)
            .clone()
            .unwrap_or_else(|| "aborted".to_owned())
    }
}

/// Locks one mutex, recovering from poisoning (the fr-tools `recover`
/// judgment).
fn recover<T>(lock: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    lock.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
