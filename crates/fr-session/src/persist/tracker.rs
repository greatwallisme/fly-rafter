//! The backend's in-process bookkeeping (dsh storage.ts
//! `JsonlBackendTracker`): the single active writer per session id, the
//! open-handle set the teardown sweep closes, and the
//! created-but-unmaterialized sessions this process can already observe.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use fr_wire::event::SessionHeader;
use fr_wire::ids::SessionId;

use super::PersistError;
use super::handle::{HandleInner, JsonlSessionHandle, SessionAccess};

/// One created-but-unmaterialized session tracked in this process only
/// (dsh `PendingSession`).
#[derive(Debug, Clone)]
pub struct PendingSession {
    /// The validated detached header.
    pub(crate) header: SessionHeader,
    /// The in-memory revision token (`memory:<name>:<n>`).
    pub(crate) revision: String,
    /// Exact fork-inherited prefix length supplied at create.
    pub(crate) inherited_event_count: u64,
}

struct TrackerState {
    /// `None` marks a claim whose handle is still being constructed.
    writers: HashMap<SessionId, Option<Arc<HandleInner>>>,
    open_handles: Vec<(u64, Arc<HandleInner>)>,
    pending: HashMap<SessionId, PendingSession>,
}

/// The JSONL backend tracker; all methods take the state lock briefly and
/// never perform I/O under it.
pub struct BackendTracker {
    name: &'static str,
    counter: Mutex<u64>,
    state: Mutex<TrackerState>,
}

impl BackendTracker {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            counter: Mutex::new(0),
            state: Mutex::new(TrackerState {
                writers: HashMap::new(),
                open_handles: Vec::new(),
                pending: HashMap::new(),
            }),
        }
    }

    /// Claims write ownership and records the created session as pending,
    /// making it observable to this process before it materializes (dsh
    /// `registerCreated`): session ids do not collide across processes and
    /// no durable artifact exists before the first materializing write.
    ///
    /// # Errors
    ///
    /// Returns `AlreadyExists` when a concurrent create or an open write
    /// handle holds the id.
    pub fn register_created(
        &self,
        header: &SessionHeader,
        inherited_event_count: u64,
    ) -> Result<(), PersistError> {
        {
            let mut state = lock(&self.state);
            if state.writers.contains_key(&header.id) {
                return Err(PersistError::AlreadyExists(format!(
                    "session \"{}\" already exists",
                    header.id.as_str()
                )));
            }
            state.writers.insert(header.id.clone(), None);
            let seq = {
                let mut counter = lock(&self.counter);
                *counter += 1;
                *counter
            };
            let revision = format!("memory:{}:{seq}", self.name);
            state.pending.insert(
                header.id.clone(),
                PendingSession {
                    header: header.clone(),
                    revision,
                    inherited_event_count,
                },
            );
        }
        Ok(())
    }

    /// Claims write ownership for an existing session (dsh `claimWrite`).
    ///
    /// # Errors
    ///
    /// Returns `AlreadyOwned` when an active write handle (or a creator
    /// whose close has not yet erased its pending entry) holds the id.
    pub fn claim_write(&self, id: &SessionId) -> Result<(), PersistError> {
        {
            let mut state = lock(&self.state);
            if state.writers.contains_key(id) {
                return Err(PersistError::AlreadyOwned(format!(
                    "session \"{}\" is already owned by an active write handle",
                    id.as_str()
                )));
            }
            state.writers.insert(id.clone(), None);
        }
        Ok(())
    }

    /// Rolls a failed write open back (dsh `releaseClaim`).
    pub fn release_claim(&self, id: &SessionId) {
        lock(&self.state).writers.remove(id);
    }

    /// The pending entry for a created-but-unmaterialized session, if any.
    pub fn pending_of(&self, id: &SessionId) -> Option<PendingSession> {
        lock(&self.state).pending.get(id).cloned()
    }

    /// The active write handle for one session, if any (dsh `writers.get`).
    pub(crate) fn writer(&self, id: &SessionId) -> Option<Arc<HandleInner>> {
        lock(&self.state)
            .writers
            .get(id)
            .and_then(|slot| slot.as_ref().map(Arc::clone))
    }

    /// Whether this process still tracks a created-but-unmaterialized
    /// session.
    pub fn has_pending(&self, id: &SessionId) -> bool {
        lock(&self.state).pending.contains_key(id)
    }

    /// The pending entries snapshotted for listing, keyed by session id.
    pub fn pending_entries(&self) -> Vec<(SessionId, PendingSession)> {
        lock(&self.state)
            .pending
            .iter()
            .map(|(id, entry)| (id.clone(), entry.clone()))
            .collect()
    }

    /// Drops a pending entry once the session materialized durably.
    pub fn materialized(&self, id: &SessionId) {
        lock(&self.state).pending.remove(id);
    }

    /// Tracks one open handle for teardown and, for a write handle, binds
    /// it as the session's live event route (dsh `adopt`).
    pub fn adopt(&self, handle: &JsonlSessionHandle) {
        let inner = handle.inner_arc();
        let mut state = lock(&self.state);
        state.open_handles.push((inner.uid, Arc::clone(&inner)));
        if inner.access == SessionAccess::Write {
            state.writers.insert(inner.id.clone(), Some(inner));
        }
    }

    /// Releases one handle's bookkeeping on close (dsh `release`): a write
    /// handle drops its ownership claim; a creator that never materialized
    /// leaves nothing behind — the session never existed.
    pub fn release(&self, uid: u64, id: &SessionId, access: SessionAccess, materialized: bool) {
        let mut state = lock(&self.state);
        state.open_handles.retain(|(open_uid, _)| *open_uid != uid);
        if access != SessionAccess::Write {
            return;
        }
        state.writers.remove(id);
        if !materialized {
            state.pending.remove(id);
        }
    }

    /// Drains and flushes every active write handle — the service-wide
    /// durability barrier behind the backend `flush` (dsh `flushAll`).
    ///
    /// # Errors
    ///
    /// Returns one aggregate naming each session whose flush failed; the
    /// remaining handles still flush.
    pub fn flush_all(&self) -> Result<(), PersistError> {
        let writers: Vec<Arc<HandleInner>> = {
            let state = lock(&self.state);
            state.writers.values().flatten().map(Arc::clone).collect()
        };
        let mut errors: Vec<PersistError> = Vec::new();
        for inner in writers {
            let handle = JsonlSessionHandle::from_inner(Arc::clone(&inner));
            // A handle closed during the sweep counts as flushed: its close
            // drained the routed buffer durably before refusing this flush.
            let outcome = handle.drain_live().and_then(|()| handle.flush());
            if let Err(error) = outcome
                && !matches!(error, PersistError::HandleClosed(_))
            {
                errors.push(error);
            }
        }
        match errors.len() {
            0 => Ok(()),
            _ => Err(PersistError::Aggregate {
                message: format!("{} flush failed", self.name),
                errors,
            }),
        }
    }

    /// Closes every open handle — the teardown sweep behind the plugin
    /// effect (dsh `install` teardown arm; the listener wiring lands with
    /// the dougong ctx step).
    ///
    /// # Errors
    ///
    /// Returns one aggregate naming each session whose close failed.
    pub fn close_all_for_dispose(&self) -> Result<(), PersistError> {
        let handles: Vec<Arc<HandleInner>> = {
            let state = lock(&self.state);
            state
                .open_handles
                .iter()
                .map(|(_, inner)| Arc::clone(inner))
                .collect()
        };
        let mut errors: Vec<PersistError> = Vec::new();
        for inner in handles {
            if let Err(error) = JsonlSessionHandle::from_inner(Arc::clone(&inner)).close() {
                errors.push(error);
            }
        }
        match errors.len() {
            0 => Ok(()),
            _ => Err(PersistError::Aggregate {
                message: format!("{} dispose failed", self.name),
                errors,
            }),
        }
    }
}

pub fn lock<T>(cell: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    cell.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

// Rust guideline compliant 2026-10-23
