// Rust guideline compliant 2026-10-28
//! Tool-call cancellation signals (dsh `fuseToolSignals` and the
//! `AbortSignal` surfaces of `core/tools/src/index.ts`).
//!
//! dsh fuses caller and wrapper cancellation through a dispatch-scoped relay
//! signal with listener cleanup; this port composes the same first-abort-wins
//! observable by polling sources recursively — an aborted leaf (caller
//! aborted by the loop, or a wrapper deadline activated by its timer thread)
//! is visible through the fused view with no relay thread. The dsh `dispose`
//! listener cleanup is therefore structurally absent.

use std::sync::Arc;

use fr_timeout::{AbortKind, DeadlineSignal};

/// One cancellation signal visible to tool bodies and policy listeners.
#[derive(Debug, Clone)]
pub enum ToolSignal {
    /// A leaf signal (a caller-owned or deadline-derived signal).
    Plain(DeadlineSignal),
    /// The disjunction of two signals (dsh `fuseToolSignals`).
    Fused {
        /// The caller-side source.
        caller: Arc<Self>,
        /// The wrapper-side source (possibly replaced by a listener).
        wrapper: Arc<Self>,
    },
}

impl ToolSignal {
    /// Wraps one leaf signal.
    #[must_use]
    pub fn plain(signal: DeadlineSignal) -> Arc<Self> {
        Arc::new(Self::Plain(signal))
    }

    /// Fuses the caller signal with the current wrapper-visible signal (dsh
    /// `fuseToolSignals`; same-object inputs collapse like dsh's identity
    /// shortcut).
    #[must_use]
    pub fn fuse(caller: Arc<Self>, wrapper: Arc<Self>) -> Arc<Self> {
        if Arc::ptr_eq(&caller, &wrapper) {
            return caller;
        }
        Arc::new(Self::Fused { caller, wrapper })
    }

    /// Whether the signal has aborted (any source, recursively).
    #[must_use]
    pub fn is_aborted(&self) -> bool {
        match self {
            Self::Plain(signal) => signal.is_aborted(),
            Self::Fused { caller, wrapper } => caller.is_aborted() || wrapper.is_aborted(),
        }
    }

    /// The abort reason of the first aborted source found (leaf order:
    /// caller before wrapper), when aborted.
    #[must_use]
    pub fn abort_kind(&self) -> Option<AbortKind> {
        match self {
            Self::Plain(signal) => signal.reason(),
            Self::Fused { caller, wrapper } => caller.abort_kind().or_else(|| wrapper.abort_kind()),
        }
    }

    /// Aborts the caller-owned leaf under this signal (the service-face
    /// cancel path: the wire carries no signal object, so the registry
    /// aborts the stored caller signal by token). Wrapper legs belong to
    /// listeners and deadlines and are left untouched.
    pub fn abort_caller(&self) {
        match self {
            Self::Plain(signal) => signal.abort_upstream(),
            Self::Fused { caller, .. } => caller.abort_caller(),
        }
    }
}
